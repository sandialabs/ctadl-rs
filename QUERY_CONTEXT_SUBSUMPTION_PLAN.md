# Query engine: context obligations multiply the search state space — pruning plan -- DO-NOT-MERGE

Scope: the time/memory regression introduced by `56728caf` ("Query finds sinks under
contexts") in `ctadl-ascent/src/query_engine/search.rs` and the generic search it drives
in `ctadl-ir/src/graph/mod.rs`. The fix is a **subsumption rule** on the search
annotation, plus two independent size reductions. Nothing here changes what flows are
found; the acceptance bar is byte-identical finding counts with a smaller state space.

## 1. The regression, as measured

Bisected on one target (`fw_pppd`, a 238 KB Linksys firmware binary imported through the
pcode frontend, queried with `firmware-eval/models/cmdi-firmware.json5`). Query phase
only; each side got its own import + index — see §7.2/§7.2.1, that turns out not to be sound
for *finding counts*, though the state and footprint figures here reproduce on a shared store
(6,471,144 states / 2.17 GB after pruning, against 14,029,871 / 4.09 GB before). Peak is
**`phys_footprint`** — polled with
`footprint -p <pid> -f bytes` and cross-checked against `/usr/bin/time -l`'s "peak memory
footprint" — not RSS, which undercounts on macOS.

| commit | wall | peak footprint |
| --- | ---: | ---: |
| `6ecbfb45` (main) | 5.90 s | 1.48 GB |
| `862ad660` (= `56728caf^`) | 6.00 / 6.02 s | 1.48 GB |
| **`56728caf`** "Query finds sinks under contexts" | **10.59 s** | **4.10 GB** |
| `f3895bca` (branch tip) | 10.45 s | 4.08 GB |

The parent is indistinguishable from main and the tip is indistinguishable from
`56728caf`: `f3895bca` (`ascent_par!`) touches only the index engine, and both builds
report identical search sizes (14,029,871 states). One commit owns the whole delta.

Two other regressions in the same corpus have the same origin but a different profile:
`phospy` (TaintBench) 2.42 → 3.30 s with **identical** state counts, and `fakedaum` 0.28
→ 0.33 s with peak 0.07 → 0.11 GB. Those are per-state constant factors (§2.2), not state
explosion. `cajino_baidu` is the only benchmark whose findings changed: 350 → 353, which
is what D4 bought and what §5 must protect.

### 1.1 Where the memory goes

`taint_search` runs one search per source label. On `fw_pppd` the `file_input` search is
untouched (3,619,877 → 3,620,063 states, 0 context-bearing). `argv_input` is the whole
regression:

| | main | `56728caf` |
| --- | ---: | ---: |
| states in the `argv_input` search | 6,194,425 | **14,029,871** |
| — context-free states | 6,194,425 | 6,194,551 |
| — states carrying a call-string obligation | 0 | **7,835,320** |
| distinct graph vertices reached | ≤ 6.19 M | 6,333,483 |
| vertices explored under >1 annotation | — | 3,929,497 (max 3) |
| `size_of::<SearchState>()` | 48 B | **88 B** |
| — annotation (`TaintState` → `PathState`) | 1 B | 24 B |
| — edge label (`FlowEdge` → `Step`) | 9 B | 32 B |
| `states` Vec at final capacity | 8.39 M × 48 = 402 MB | 16.78 M × 88 = **1.48 GB** |
| `visited` entry / table | 40 B → ~301 MB | 56 B → ~837 MB |
| measured peak footprint | 1.48 GB | 4.10 GB |

The context-free part of the branch search (6,194,551) is main's *entire* search
(6,194,425) to within 126 states — the D4b/D4c edges add almost no new reachable
vertices. Every one of the extra 7.8 M states is a vertex already reached being
re-explored under an obligation.

The trigger is small: `context_assign.parquet` holds **8,227 rows, in 2 functions, with 2
distinct call strings**. That is enough to fork the downstream reachable region into up to
three copies, because `visited` in `find_annotated_paths_from_set` is keyed on
`(node, annotation)` (`ctadl-ir/src/graph/mod.rs:327`) and the annotation now carries a
`CallString`.

`malloc_history -callTree` on the live process confirms the shape: `TaintSearchGraph::new`
is flat across the two sides (97.8 MB vs 98.4 MB — the new `ctx_assign_by_src` /
`resolved_by_*` indices are *not* the cost), and the largest single live block on both
sides is the `states` Vec realloc. Its doubling is also why peak footprint outruns live
bytes: growing that Vec to 1.48 GB briefly holds 2.2 GB.

## 2. The two multipliers

### 2.1 State splitting (×2.26) — the subject of this plan

An obligation only ever **restricts**; it never enables. No arm of
`PathState::expand` (`search.rs:591`) is reachable only under a non-empty context:
`refine(∅, row)` (`search.rs:126`) always succeeds, since the empty string is a suffix of
everything, and `Flow(Return)` is *more* permissive at `∅` (no top-frame check to fail).
So a context obligation buys nothing on a node that is also reached context-free.

### 2.2 Per-state size (×1.83) — independent, §6

`CallString` is `immortal!`-interned as `&'static [PackedInsnSiteId]`
(`ctadl-ascent/src/facts.rs:283`) — `repr(transparent)` over a **slice reference**, so 16
bytes, not a 4-byte id. That makes `PathState` 24 B and `Step::Ctx(CallString, FlowEdge)`
32 B, pushing `SearchState` from 48 to 88 B and the `visited` entry from 40 to 56 B. This
is what `phospy` pays even with an unchanged state count.

## 3. The rule

**`{s, a}` subsumes `{s, b}` whenever `a` is a suffix of `b`.** The empty context is the
bottom of that order, not a special case.

Proof obligation is a simulation: every edge enabled at `b` must be enabled at `a`, with
successors again related. Checking the four arms of `expand`:

| arm | at `b` | at `a` (suffix of `b`) | successors related? |
| --- | --- | --- | --- |
| `Flow(Intra)` | ctx unchanged | ctx unchanged | yes, trivially |
| `Flow(Call)` | → `Restricted`, ctx unchanged | same | yes |
| `Flow(Return(site))` | needs `Free`; if ctx non-empty, `top(b) == site`, then pops | `a` empty → always passes, → `∅`; `a` non-empty → shares the top frame with `b`, so passes whenever `b` does, → `pop(a)` | `∅` and `pop(a)` are suffixes of `pop(b)` |
| `Ctx(row, _)` | needs `refine(b, row)` | two suffixes of a common string are always suffix-comparable, so `refine(a, row)` succeeds whenever `refine(b, row)` does | `refine(a,row)` is a suffix of `refine(b,row)` (case split on which of `row`/ctx is longer) |

Enabledness cannot go the other way, which is exactly why the pruning never enables a
traversal the current engine would reject.

The chain is walked one link at a time — `[s1,s2] → [s2] → ∅` — so the general rule costs
the same as the `∅`-only special case: at most `|ctx|` hash probes, and `max_k = 3` on
`fw_pppd` means ≤ 2 extra probes.

## 4. Four things the rule does *not* give you

**4.1 The annotation is a pair, and comparing only the context is unsound.** `PathState`
is `{state: TaintState, ctx}` (`search.rs:110`). `{Restricted, ∅}` does **not** subsume
`{Free, [s]}`: `Free` traverses returns that `Restricted` prunes. The rule must match
`state` exactly.

There *is* a further valid collapse — `Free` simulates `Restricted` (`Intra` preserves,
`Call` sends both to `Restricted`, `Return` only `Free` survives) — but emission writes
`st.annot.state` into the `taint` table, so folding `Restricted` into `Free` changes
persisted rows and what the formatter re-walks. **Out of scope for v1**; it is a separate
change needing its own diff.

**4.2 The win is order-dependent.** The check only fires if the more general state is
*already* in `visited` when the candidate is examined (`graph/mod.rs:335`, `:362`). BFS
usually gets there first — a ctx-bearing state is ≥ 1 hop past a contextual edge — but
nothing guarantees it. If a node is first reached under `[s]`, that subtree expands in
full and the later `∅` state re-expands the same region: both are paid, marginally worse
than today. §5.3 makes the ordering deterministic; without it the win is luck, not
structure.

**4.3 It changes observable output even where it is sound.** Neither of these drops a
finding, but both move SARIF bytes:

- **Endpoint attribution.** `origin[i]` propagates the source endpoint each state descends
  from; pruning the ctx state can leave a node attributed to a different endpoint. Same
  class of approximation as today's first-reach-wins dedup, but wider.
- **Reported path shape.** `taint_edge` takes the first (breadth-first shortest) path per
  sink vertex; killing ctx states re-routes some paths through the `∅` region, changing
  codeFlow steps.

**4.4 It fixes one multiplier of two, and the win is data-dependent.** Perfect pruning
leaves ~6.2 M states at 88 B: `states` ~738 MB at capacity, `visited` ~418 MB → peak
around **2.2–2.5 GB against main's 1.48 GB**. Parity needs §6 as well. And the win is
proportional to the `∅`/ctx node overlap — 98% on `fw_pppd`, but on a target whose
contextual region is genuinely disjoint from the `∅`-reachable region it buys nothing, and
that is precisely the target where the contexts earn their keep.

## 5. Implementation

### 5.1 A generalization hook on `LazyAnnotation`

`ctadl-ir/src/graph/mod.rs:246`. Allocation-free, because it runs on every candidate edge:

```rust
/// The next-more-general annotation, or `None` at the top of the chain.
///
/// Contract: every edge enabled at `self` must be enabled at the returned annotation,
/// with successors again so related — a simulation. The search may therefore skip a
/// state whose generalization is already visited for the same node.
fn generalization(&self) -> Option<Self> {
    None
}
```

The default `None` keeps every other impl behaviour-identical.
`PathState::generalization` drops the **outermost** frame (`[s1,s2] → [s2] → ∅`) and
leaves `state` untouched. Two `CallString::intern` calls per probe, both hitting the
intern cache. Note `CallString::pop` (`facts.rs:319`) removes the *innermost* frame — the
generalization chain needs the other end, so it takes `&self.0[1..]`.

### 5.2 Consult it at the visited checks

`graph/mod.rs:335` (start states) and `:362` (expanded states): walk the chain and skip if
any generalization is already keyed for that node. Iterative, no `Vec`, at most `|ctx|`
probes, and only entered when the candidate's ctx is non-empty.

### 5.3 Make the frontier order deterministic

Split the frontier into a `∅` queue and a ctx queue, draining `∅` first (a second
`VecDeque`, ~10 lines). Every context-free state then exists before any ctx state is
dequeued, so the pruning is maximal rather than lucky. This deliberately prefers
context-free routes for reported paths — update the "first entry for a node is on a
shortest path" comment (`graph/mod.rs:276`), since shortest-path now holds *within* each
class.

If `CTXSTATS` (§5.4) still shows meaningful residual duplication, escalate to a true
two-phase search: run `∅` to fixpoint, then seed the contextual pass from those `∅` states
that have `Ctx` successors, using the phase-1 visited set as the subsumption oracle.

### 5.4 Keep the instrumentation

The measurements above came from a throwaway patch that printed, per label search:
`states`, `distinct_nodes`, `ctx_bearing_states`, `nodes_with_k>1`, `max_k`, and
`size_of` for state/node/annotation/label. Land a `log::debug!` version of the cheap half
(`states`, `ctx_bearing_states`, `max_k`) next to the existing per-label debug line
(`search.rs:839`); the distinct-node histogram stays a scratch patch, since it costs a
second pass over every state.

## 6. The other multiplier (independent, do after §5 lands and is measured)

1. **Intern `CallString` as a `u32` id** instead of `&'static [PackedInsnSiteId]`
   (`facts.rs:283`). `PathState` 24 → 8, `Step` 32 → 16, `SearchState` 88 → ~56,
   `visited` entry 56 → 40. ~35–40% off the peak, zero semantic change.
2. **Move `edge: Option<L>` out of `SearchState`.** Labels are only needed for ancestors
   of target states; on `fw_pppd` zero targets are reached, so all 14 M labels are dead
   weight. A side table keyed by state index, or recomputation during path
   reconstruction, removes 32 B/state.

## 7. Validation

In this order — each step is a gate, not a suggestion:

1. **`fw_pppd`, the bisect target.** Expect `argv_input` states 14,029,871 → ~6.2 M, peak
   4.10 → ~2.3 GB, wall 10.4 → ~6.5 s. Read `CTXSTATS` to confirm the residue is small
   rather than assuming it.
2. **Differential SARIF over the 17-benchmark corpus** used for the regression sweep (12
   TaintBench APKs, 4 Operation Mango cmdi binaries, 3 `large_dataset` firmware binaries),
   against the branch tip. Finding counts must be identical everywhere, and
   `cajino_baidu` must stay at **353** — those 3 findings over main are what the contexts
   bought, and they are the direct test that pruning does not undo D4. Diff the codeFlows
   on one contextual case to see how path shapes moved (§4.3).

   **Run both sides against one store per benchmark.** Import + index once, then query it
   with each binary. Giving each side its own import — what the original regression sweep
   did — is *not* a valid differential: `cajino_baidu.apk` imported twice and queried both
   times by the **same unmodified** branch-tip binary yielded **357** findings on one import
   and **353** on the other. That spread is four times the D4 delta this gate is supposed to
   detect, so a per-side import can manufacture or mask the exact signal being measured. A
   finding count is a property of *(binary, store)*, never of the binary alone; the gate that
   means something is *tip and pruned agree on the same store*, with `353` reproduced on the
   import that produces it.

   That gate is sound, because the query itself **is** deterministic: three queries against
   one store gave byte-identical SARIF apart from the `invocations` block. §7.2.1 is where
   the variance actually lives. It is pre-existing and independent of the subsumption change.

3. **The contextual-dispatch tests this branch added**: `nightly/tests/c/funcptrcallee{source,sink,frame}`
   and `nightly/tests/lua/resolved-callee-*`. If the subsumption were wrong these break
   first.
4. **Independent oracle.** Cross-check one target against the closure engine through the
   `CTADL_QUERY_DATALOG` escape hatch (`query_engine/mod.rs:411`). Pick a target with a
   substantial contextual region — `fakedaum` carries a call string on 43% of its states
   and runs in seconds, which is a better oracle than `fw_pppd` (whose closure run does not
   finish in 10 minutes).

   Compare **finding sets only**. The closure engine's reported paths are themselves
   nondeterministic: the same binary over the same store, run twice, gave identical 104
   findings both times but codeFlow step totals of 34 and 20. So a codeFlow diff against
   this oracle carries no signal, and the two engines' finding sets differ slightly anyway
   (contexts are collapsed on the datalog side). The question the oracle answers is
   comparative — *does the pruned search agree with the closure engine exactly as well as
   the unpruned search does?* — so run the tip binary through the same comparison and check
   that the disagreement is unchanged, rather than expecting it to be empty.

   Also pre-existing and independent of this change — and probably the same root cause:
   §7.2.1's pointer-hashed interned keys randomize hash-container iteration per process, which
   is enough to re-route a closure-engine path without changing the finding set.

### 7.2.1 Why per-import counts move: index row order, read through a lossy emission

Measured on `cajino_baidu` with the branch tip plus this plan's pruning, querying the shipped
`apps/cajino_baidu/model.json` with its non-schema `description` key stripped and counting
`runs[0].results`: 11 import+index runs gave 345 findings nine times and 349 twice. The
absolute base differs from the sweep's 353/357 (a different harness); the ±4 spread does not.

**The index is order-nondeterministic, not content-nondeterministic.** Take a 345-store and a
349-store: every relation is **multiset-identical** — the same facts, exactly. Only the row
*order* differs, and only in the derived relations (`assign`, `context_assign`, `paths`,
`resolved_call`, `summary`, `index_source_map`); `call` / `actual_param` / `formal_param` /
`callee_*` are stable. Copying one relation file at a time from the 349-store into the
345-store isolates it to one relation:

| relation whose row order was swapped in | findings |
| --- | ---: |
| `assign` | **349** |
| `context_assign`, `paths`, `resolved_call`, `summary`, `index_source_map` | 345 |

**The search loses nothing; the emission does.** Under both orders the searches are identical
— `FileData` 1,059,867 states / 46 sink vertices, `DatabaseData` 534,302 / 35, `DeviceId`
379,937 / 11, same context-bearing counts — and so is the winning (sink vertex → source
endpoint) map over target states. Two *reporting* structures differ:

- **`origin[]` (`search.rs:799`) is first-reach-wins over a multi-source start set.** 44,728
  of 890,002 forward `taint` rows differ **only in the endpoint column**: the same
  `(func, state, var, path)` node attributed to the source endpoint in func 13875 under one
  order and 13306 under the other.
- **`reported` (`search.rs:837`) emits one BFS-shortest path per sink vertex per label
  search.** `taint_edge` came out 322 edges under one order and 303 under the other.

Both are what the formatter pairs sources to sinks with (`formatter.rs:2657`–`2710`): it reads
`node_to_endpoint` off those `taint` rows, then re-walks the `taint_edge` graph looking for a
realizable path. A pair whose route was not the one emitted, or whose node was attributed to a
sibling source, is never tested. Net effect between the two orders: 15 findings unique to one,
19 unique to the other, at different sink instructions. This mechanism predates the contexts —
it arrived with the demand-driven search in `de118585`; D4 changes the numbers, not the shape.

**Where the row order comes from.** Not `ascent_par!`: reverting `f3895bca` to serial
`ascent!` is still nondeterministic, and so is `RAYON_NUM_THREADS=1`. It is upstream of the
fixpoint — the *seed* relations already differ in order at index entry. Two causes:

- **`ctadl-ir/src/ssa/mod.rs:297`.** Phi placement iterates a
  `std::collections::HashSet<ArcIntern<Variable>>` (`RandomState`, seeded per process) and
  `push_front`s each phi, so phi order within a block — and therefore SSA numbering and fact
  emission order — is random per process. Sorting that iteration made **every** seed relation
  identical except one.
- **Interned keys hash by pointer**: `internment::ArcIntern` (`Symbol`), `immortal!`
  (`facts.rs:279`), `tailshare::Seq` (`tailshare/src/lib.rs:307`), `trie::Trie`
  (`trie/src/lib.rs:224`). Addresses vary per process, so any hash container keyed on a
  `Path`/`Symbol` iterates differently every run. That is the residual `program_paths`
  divergence, and it survives switching those containers to `FxBuildHasher`.

One wrinkle that is *not* order: `resolved_call` occasionally differs in content (1734 vs 1735
rows — one extra contextual resolution). It does not correlate with the finding delta here (a
1734-row store gave 349, a 1735-row store gave 345), but it is a real nondeterminism in the
lattice witness and needs its own look.

**Two candidate fixes, and they are a choice.** Make the *index* deterministic — sort the phi
placement, and hash interned values by a monotonic id rather than an address (the phi sort is
verified to collapse all but one seed; the interning change is not yet implemented). Or make
the *query's emission* order-invariant — carry every origin per state and every path per
source→sink pair instead of first-reach-wins. The first is cheap and local; the second changes
what SARIF contains. Both are out of scope for this plan.

## 8. Risks

| risk | signal | mitigation |
| --- | --- | --- |
| Simulation argument wrong for some `Step` shape added later | a `funcptrcallee*` / `resolved-callee-*` test flips to no-flow | the contract is stated on `generalization`; any new `Step` arm must be checked against it |
| SARIF churn from re-routed paths or endpoint attribution | codeFlow diffs with equal finding counts | accept, but record the diff in the PR; §4.3 says which two mechanisms produce it |
| Win does not materialize on other corpora | `CTXSTATS` shows `ctx_bearing_states` still large after §5.3 | escalate to the two-phase search; if a target's contextual region really is disjoint, the states are load-bearing and §6 is the only lever |
| Pruning masks a genuine precision bug in `refine` | none — pruning cannot enable a traversal | §7.4's datalog oracle |
| Index row order read as a pruning regression (or hiding one) | a finding-count diff that does not reproduce when both sides query one store | §7.2: one import per benchmark, queried by both binaries. Seen at ±4 findings on `cajino_baidu`, against a D4 delta of 3. Mechanism in §7.2.1: the facts are identical, the row order is not, and the query's first-reach source attribution reads it |
| Closure-engine path nondeterminism read as §4.3 codeFlow churn | codeFlow diffs against the datalog oracle | §7.4: compare finding sets only; the oracle's own codeFlows vary run to run |
