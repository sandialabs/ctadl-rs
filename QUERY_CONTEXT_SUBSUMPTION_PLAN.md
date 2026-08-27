# Query engine search plan: witness completeness for source→sink pairs -- DO-NOT-MERGE

Scope: `ctadl-ascent/src/query_engine/search.rs` and the generic search it drives in
`ctadl-ir/src/graph/mod.rs`.

This document had one subject — the state-space blowup from context obligations
(`cf558108`) and its subsumption fix. That fix **landed** (`c666df16`); the appendix keeps its
measurements because they are the memory budget every change below has to live inside.
The live subject is now a **correctness property the search does not have**, stated in §0
and violated three independent ways (§2). §3 is the change, and it is ordered: **§3.1 lands
first and alone** — it is ~15 lines in `facts.rs`, it is the only part that changes what the
index records, and it is the one with a prototype and a measurement behind it (§2.4, §6.1).
§3.2–§3.6 are the query-side reporting work, unchanged in substance from the previous draft.

## 0. The property

> Given the query's set of source endpoints and sink endpoints: for **every** pair
> `(S, K)` such that a data-flow path from `S` to `K` exists, the SARIF output contains a
> result whose `codeFlow` is a path from `S` to `K`.

Two clauses, and they fail separately:

- **Pair completeness** — every connected pair is *reported*. This is what breaks today.
- **Witness** — the reported `codeFlow` is a real path, not an existence claim. The
  formatter already re-walks a graph to produce one, so a reported pair always comes with
  *some* path; §5.2 is about that path being *this pair's* path.

"A data-flow path" means the edges `TaintSearchGraph::labeled_successors` generates, walked
under the **one-bit call/return discipline** — `TaintState::Free`/`Restricted`, the same
matching rule the closure engine enforced. Context obligations are deliberately *not* part of
the definition: they are a precision filter layered over it, and §2.4 is the claim that the
filter as built removes paths the definition admits. (Defining the relation as "whatever
`PathState::expand` accepts" would make that claim unfalsifiable, which is what an earlier
draft of this section did.) It is also deliberately **not** "any path the datalog oracle would
find" — §7 lists the three places those two semantics differ on purpose, so a differential
against `CTADL_QUERY_DATALOG=1` must not read those as violations.

Contrapositive worth stating, because it is the acceptance bar: **no reachable pair may be
silently dropped.** A pair the engine cannot witness must show up in a counter (§5.3), not
in silence.

## 1. Status of the earlier plan (landed, keep)

| item | state |
| --- | --- |
| `LazyAnnotation::generalization` hook + contract (`graph/mod.rs:270`) | landed `c666df16` |
| `subsumed` chain walk at both visited checks (`graph/mod.rs:363`, `:392`, `:424`) | landed |
| Two frontiers, general drained first (`graph/mod.rs:386`, `:414`) | landed |
| `PathState::generalization` / `CallString::drop_outermost` (`search.rs:682`, `facts.rs:333`) | landed |
| Per-label `ctx_bearing` debug line (`search.rs:886`) | landed |
| `CallString` as a `u32` id; `edge: Option<L>` out of `SearchState` (old §6) | **open**, still the other 1.8× |

Measured effect on the bisect target (`fw_pppd`, `argv_input` search): 14,029,871 states /
4.09 GB → 6,471,144 / 2.17 GB. Main is 6,194,425 / 1.48 GB; the residual gap is per-state
size, which is the open item above.

## 2. Why the property does not hold

### 2.1 Reproducer

`two.c` — two sources of one label into one sink vertex, both flows real (`c` is a phi of
`a` and `b`):

```c
int produce(void);
void consume(int x);

void f(int cond) {
  int a = produce();          /* source 1: call-arg(2,-1) */
  int b = produce();          /* source 2: call-arg(3,-1) */
  int c;
  if (cond) { c = a; } else { c = b; }
  consume(c);                 /* sink: call-arg(7,0) */
}
```

```sh
ctadl --store ./store  go -l c two.c -m model.json -o search.sarif   # demand-driven search
CTADL_QUERY_DATALOG=1 ctadl --store ./store2 go -l c two.c -m model.json -o dl.sarif
```

| | search (default) | datalog oracle |
| --- | ---: | ---: |
| `taint-source` results | 2 | 2 |
| `taint-sink` results | 1 | 1 |
| **`tainted-path` results** | **1** | **2** |
| `taint_edge` edges (`--dump-taint-graph`) | **1** | 32 |

Both sources are found, the sink is found, and one of the two pairs is gone. The debug line
says the whole story: `label 'UserInput' with 2 source endpoint(s): 10 states (0
context-bearing), 1 sink vertices reached` — *sink vertices*, not source→sink pairs, is the
unit the search reports in.

This is a regression against the closure engine, and it is not D4's: the datalog `taint`
relation carries the endpoint in the tuple (`mod.rs:455`), so every source that reaches a
node gets its own row, and its `taint_edge` is the whole closure. The demand-driven search
(`de118585`) replaced both with a first-reach projection.

### 2.2 Cause 1 — source attribution is first-reach-wins

One search per *label* (`search.rs:792`), all of the label's source vertices seeded into one
start set with one shared visited set. Each state therefore has exactly one recorded origin
(`search.rs:799`–`:805`, propagated along the first-reach parent link), and the bulk `taint`
rows carry that single endpoint (`search.rs:826`). `start_origin` (`search.rs:777`) collapses
two endpoints naming the *same* vertex the same way.

The formatter pairs at a node carrying both a forward and a backward tag
(`formatter.rs:2630`, `:2667`–`:2710`). In the reproducer the sink node's forward tag is
source 2, so the pair `(source 1, sink)` is never even *considered*.

### 2.3 Cause 2 — one reported path per sink *vertex*

`reported` is keyed on the level-agnostic vertex (`search.rs:837`), and `taint_edge` receives
edges only from those paths (`search.rs:846`–`:860`). So the graph the formatter re-walks
contains one route per reached sink vertex per label search — one edge, in the reproducer,
against the oracle's 32.

This makes Cause 1 unfixable on its own: tag the sink node with *both* sources and the
pairing loop would ask for a walk from `call-arg(2,-1)`, whose vertex appears in no
`taint_edge` row at all. `find_annotated_path_to_set` (`formatter.rs:2702`) returns `None`
and the pair is dropped with no diagnostic. **Both causes have to be fixed together, and
neither is fixed by tagging alone.**

Same for the sink-tag side: only nodes on that one path get the backward tag
(`search.rs:866`), so a second source's route has no meeting node even when its edges exist.

### 2.4 Cause 3 — the context join keeps a witness, not an upper bound

`twosite.c` — two call sites of the same resolved callee through the same dispatch:

```c
int source();
void sink(int x);
typedef int (*fn0)(void);

int makes_taint(void) { return source(); }

int relay(fn0 g) { return g(); }        /* one dispatch insn, resolved to makes_taint */

int main() {
  int a = relay(makes_taint);           /* call site A -> main:12 */
  sink(a);                              /* line 16 */
  int b = relay(makes_taint);           /* call site B -> main:14 */
  sink(b);                              /* line 18 */
}
```

| | search (default) | datalog oracle |
| --- | ---: | ---: |
| `tainted-path` results | **1** (line 16 only) | **2** (lines 16, 18) |

One source endpoint and two *distinct* sink vertices, both emitted as `taint-sink`, so
Causes 1 and 2 are not in play. `context_assign` and `summary` are both empty in this index,
so the D4b return edge is the whole flow.

`resolved_call` holds exactly one row — `(relay, insn 7, makes_taint, [main:12])`. Both call
sites derive that pair; the join kept the lexicographically smaller string. The trace:

| step | edge | label | obligation after |
| --- | --- | --- | --- |
| W → X | `makes_taint` out-formal → `relay`'s `call-arg(7,-1)` | `Ctx([main:12], Return(relay:7))` | `[main:12]` |
| X → Y | `relay`'s `call-arg(7,-1)` → `relay`'s out-formal | `Flow(Intra)` | `[main:12]` |
| Y → Z | `relay`'s out-formal → `main`'s `call-arg(14,-1)` (`b`) | `Flow(Return(main:14))` | **pruned** |

Y → Z dies at `search.rs:627`: `ctx.top() == main:12 != main:14`. Note where the prune is
*not*: the witness edge W → X is taken, `refine(∅, [main:12])` succeeding as it should. The
loss happens three steps later on an ordinary `Flow(Return)`, against the obligation the
witness stamped onto the path. Patching that one arm to `Some({Free, ∅})` and rebuilding gives
2 findings, lines 16 and 18 — which isolates the cause but treats the symptom (§3.4).

The root cause is upstream: `resolvent`, `context_assign`, `context_locals`, `context_summary`
and (since `ef13d498`) `resolved_call` are `SmallestCallString` lattices keyed on their
non-context columns, and that lattice's join is `max` under a total order — it *picks* one of
two incomparable call strings. A tuple derivable under two contexts records one, and nothing
about the survivor covers the loser, so the obligation check is testing an enumeration against
a witness. §3.1 fixes the join; §7.2's `resolved_call` 1734/1735 content nondeterminism is the
same mechanism seen from outside.

Two things this is *not*. It is not `ef13d498`'s doing: reverse-applying that commit's
`index_engine` hunks still yields one `resolved_call` row and one finding, because `resolvent`
(`index_engine/mod.rs:1142`) was already a lattice keyed on `(func, formal, path, target)` and
the merge happens there regardless. And it is not a policy choice — the earlier reading of this
section, that pruning trades completeness for precision, was wrong. Pruning against a *common
suffix* keeps both; only pruning against a *witness* has to choose.

### 2.5 Cause 4 — the formatter's pairing gate (outside search.rs)

Even with 1–3 fixed, two formatter-side facts bound the property:

- **Pairing only happens at a "detail" node** — `details_by_span` is built from
  `tainted_insn`, which keeps only call-arg vertices with `formal >= 0`, non-globals, *and* a
  resolvable source span (`formatter.rs:2667`). A witnessed pair whose whole path misses such
  a node is never tested. Usually harmless (the sink is an argument at a call), but
  function-anchored endpoints — the `anchored_at_callsites` fallback for a callerless function
  or the globals pseudo-formal (`mod.rs:104`–`:133`) — land outside it.
- **The reported path is re-derived, not carried.** The formatter searches the union of all
  emitted edges (`formatter.rs:2702`), so it can answer a pair with a walk spliced out of two
  other pairs' witnesses. Pair completeness survives that; clause two of §0 does not.

`find_endpoint_paths` (`formatter.rs:1767`, used by the flowy checker) has no detail-node
gate — it pairs every source with every sink present in `taint` — so it is a *stricter* client
of the same emission, and Cause 2 hits it too.

### 2.6 Summary

| cause | site | fixed by |
| --- | --- | --- |
| 1. one origin per state | `search.rs:777`, `:799`, `:826` | §3.2 + §3.3 |
| 2. one path per sink vertex | `search.rs:837`, `:846`, `:866` | §3.2 + §3.3 |
| 3. context join keeps a witness, not a lub | `facts.rs:445` (read at `search.rs:612`, `:630`) | §3.1 |
| 4a. pairing needs a detail node | `formatter.rs:2667` | §5.1 (formatter) |
| 4b. path re-derived from a union graph | `formatter.rs:2702` | §5.2 (optional) |

## 3. The change

Ordered: §3.1 lands first and alone. It is the smallest diff in this document, it is the
only one that changes what the *index* records, and until it lands the obligation check is
testing witnesses, so every measurement of §3.2–§3.6 would be taken against a moving floor.

### 3.1 Land first: make the context join a least upper bound

Cause 3 is a defect in `SmallestCallString::join_mut` (`facts.rs:445`), not in the obligation
check that reads its output. Today the join is `max` under `call_string_height_cmp`
(`facts.rs:382`): shorter wins, ties broken lexicographically. For two *incomparable* strings
under one key that discards one of them outright — a choice of witness, not an upper bound.
Nothing about the survivor covers the loser, which is exactly what makes reading the result as
an enumeration wrong.

Replace it with the **longest common suffix**:

| operands | today | change |
| --- | --- | --- |
| `[s2]`, `[s1,s2]` | `[s2]` | `[s2]` (unchanged — already correct) |
| `[s1,s2]`, `[s3,s2]` | `[s1,s2]` or `[s3,s2]`, by tie-break | `[s2]` |
| `[s1]`, `[s2]` | `[s1]` or `[s2]`, by tie-break | `[]` |

Call strings are outermost-first, so a shared suffix is a shared claim about the innermost
frames, and it is the strongest claim both derivations support. This is a genuine semilattice —
idempotent, commutative, associative — with `[]` as top; the result is a suffix of both
operands, so string length is non-increasing and the fixpoint terminates on the same argument
it uses today. The suffix-pair row of that table is why the change is small in practice: the
common merge is already a suffix pair, and it already behaves.

**Why this makes the obligation check sound.** The recorded string becomes a common suffix of
*every* context the resolution holds under. So `refine` (`search.rs:126`) failing means no
recorded context is compatible, and `Flow(Return(site))`'s pop (`search.rs:617`) tests a frame
every recorded context shares. Where the index genuinely saw two incompatible contexts the
string shortens toward `[]` and no obligation is imposed at all — the query stops pruning
exactly where, and only where, the index has nothing to prune with. Where a resolution really
does hold under one context, today's precision survives untouched. This is the "complete
context sets" fix earlier drafts deferred to upstream, in the one form that costs no rows: an LCS is the
abstraction of the set, and the abstraction is what the check needed.

**Shape of the diff.** `CallString::longest_common_suffix`, beside `drop_outermost`
(`facts.rs:334`), and `SmallestCallString::join_mut`. ~15 lines. No schema change, no new
relation, no query-engine change, and no growth in row count — the relations stay keyed
exactly as they are. `Ord` stays the total order it is (`Bottom` handling, row sorting); only
`join_mut` stops being `max`. `ascent` merges lattice columns through `join_mut` alone, which
is the same latitude `Consistent`'s doc comment already takes (`lattice.rs`).

**Measured on the §2.4 reproducer, prototyped:**

| | `resolved_call` rows | `tainted-path` |
| --- | ---: | ---: |
| tip | 1, `cs=[main:12]` | 1 (line 16 only) |
| LCS join | 1, `cs=[]` | **2 (lines 16, 18)** |

`cargo xtask regression --frontend c`: 26 passed, 0 failed, 2 xfail — all four `funcptr*`
cases pass. `--frontend lua`: 28 passed, 0 failed, including all three `resolved-callee-*`.

**Cost, and it is not one-directional.** Context-bearing search states can only fall: the join
shortens strings, so a key carries no more distinct contexts than it does today. But a pair
that merges to `[]` stops seeding `context_assign` and instead instantiates the callee's
summary into plain `assign_like` (`index_engine/mod.rs:1403`, and `:1345` for the pop-up
chain), so the *context-free* graph grows and precision drops there. Net direction is
unmeasured. Gate it on `fw_pppd` (peak, per §4) and `fakedaum` (findings, per §6.8) before
§3.2 starts moving numbers of its own.

**Residue this does not fix.** `CallString::push` (`facts.rs:340`) returns `None` when the call
site's function is already in the string, and rules 2.1 (`index_engine/mod.rs:1276`) and 2.2
(`:1296`) gate on it, so a derivation needing a recursive context is dropped rather than
widened. A resolution derivable both acyclically and through recursion therefore still records
the acyclic context alone, and the recursive flow is still pruned at the return. The fix is the
same shape — join `SmallestCallString::top()` on a failed push instead of dropping the
derivation — but it *adds* resolutions rather than relabelling them, so it is a separate diff
with its own corpus differential. **Unverified.** Until it lands, §3.4 stays on the shelf
rather than being deleted.

**Rejected alternative: bare relations.** Storing the contexts as a plain relation instead of a
lattice enumerates them completely and needs no query change, and it is what the pre-`ef13d498`
comment on `resolved_call` argued for. Against it:

- There is no `k`. `push` bounds only cycles, so a bare `resolvent` holds one row per distinct
  acyclic call path to the formal — path count in a DAG, not node count.
- The multiplier compounds. Rule 3.1 (`:1325`) joins `resolved_call × summary(callee)`; rules
  3.3a/3.3b (`:1360`–`:1382`) take the `context_locals` transitive closure *per context* — a
  per-context copy of `locals`; rule 3.4 lifts to `context_summary` and 3.2 (`:1335`) pops each
  one into a caller, spawning a per-context chain up the stack.
- The query pays it worse. §7.1: 8,227 `context_assign` rows over **2 functions with 2 call
  strings** took the `argv_input` search from 6.19 M states / 1.48 GB to 14.03 M / 4.10 GB. And
  subsumption cannot reclaim it — `generalization` collapses a contextual state against a
  *context-free* one at the same node, never two sibling contexts against each other, so
  `[main:12]` and `[main:14]` stay two states.
- It does not fix the bug. The cycle-drop residue above is unaffected by enumeration: a bare
  relation enumerates more contexts but still only *acyclic* ones.
- It loses the `is_empty()` feedback (`:1316`, `:1345`, `:1403`), so a pair also derivable
  unconditionally keeps the unconditional row *and* N conditional copies of the same edge.

The one point in its favour is unmeasured: §7.2 saw `resolved_call` at 1734/1735 rows across
imports while it was already bare, so bare did not buy determinism there either.

### 3.2 Witness passes: one search per source start vertex

Replace "one search per label" with "one search per label **plus** one search per source
start vertex". The per-source search is what makes a per-pair witness *exist*: a single-start
search's forest has exactly one root, so every target state's `path_to` is a path from *that*
source, and `targets` gives one per reached sink vertex in BFS-within-class order.

```rust
// Per label, canonically ordered (§3.5) and deduped by start node: several endpoints can
// name one vertex, and they share its search.
struct Start { node: TaintNode, endpoints: Vec<usize> }   // indices into this label's endpoints

// Pass 1 (gate) — today's shared search, unchanged. Two jobs: the bulk `taint` rows exactly
// as they are emitted now, and the set of sink vertices reachable from this label at all.
let gate = find_annotated_paths_from_set(&graph, starts.iter().map(|s| s.node), is_sink);
emit_bulk_rows(&gate, &origin, ...);                      // search.rs:799-:826, unchanged
let reachable: HashSet<TaintVertex> = gate.targets.iter().map(|&t| vertex(&gate, t)).collect();
if starts.len() == 1 {
    emit_witnesses(&gate, &starts[0], &reachable);        // one source: the gate *is* the witness pass
}
drop(gate);                                               // free 2.2 GB before any witness pass

// Pass 2 (witness) — only when there is something to witness and more than one source.
if starts.len() > 1 && !reachable.is_empty() {
    for s in &starts {
        let w = find_annotated_paths_from_set(&graph, [s.node], is_sink);
        emit_witnesses(&w, s, &reachable);
    }
}
```

The `starts.len() == 1` and `reachable.is_empty()` guards are what keep this affordable
(§4): a label with one source pays exactly today's cost, and a label that reaches no sink
pays exactly today's cost — which is the `fw_pppd` case, where zero targets are reached.

### 3.3 Emission: one witness per (source, sink vertex)

`emit_witnesses` replaces the `reported` loop (`search.rs:837`–`:880`). Per witness it emits
three things instead of two — the forward tag is the new one:

```rust
let mut seen: HashSet<TaintVertex> = HashSet::default();
for &t in &search.targets {
    let vertex = vertex(&search, t);
    if !seen.insert(vertex) { continue }                   // first target per vertex: shortest-in-class
    let path = search.path_to(t);
    for w in path.windows(2) { taint_edge.insert(...) }    // as today: this pair's route is in the graph
    for &i in &path {
        let st = &search.states[i as usize];
        for &e in &start.endpoints {                       // NEW: forward tag naming *this* source
            witness_rows.insert((st.node.0, st.annot.state, st.node.1, st.node.2, endpoints[e].clone()));
        }
        for sink in &sink_nodes[&vertex] {                 // as today: backward tag
            witness_rows.insert((st.node.0, st.annot.state, st.node.1, st.node.2, sink.clone()));
        }
    }
}
```

`witness_rows` is a `BTreeSet` (paths share prefixes, and now across sources too), merged
into `taint` where `sink_tags` is merged today (`search.rs:894`).

Why this is sufficient for pairing: every node on the witness path now carries both the
source tag and the sink tag, so the formatter's meeting-node test succeeds at *any* node of
the path — and the path's own edges are in `taint_edge`, so the re-walk has a route to find.
Cause 1 and Cause 2 are fixed by the same two lines.

Emission size is proportional to **findings**, not to the state space: `Σ_pairs (path
length × (1 + sinks at that vertex))`. On `cajino_baidu` (890,002 `taint` rows, ~350
findings) the witness rows are noise. Contrast the alternative of emitting every traversed
edge to make the union graph complete: that is 14 M rows on `fw_pppd` and reinstates exactly
the `taint_edge` blowup the demand-driven regime exists to avoid.

### 3.4 Contingency (not part of the plan): obligations stop pruning

Held in reserve, not scheduled. With §3.1 landed the obligation check tests a suffix every
recorded context shares, so the two mismatch arms prune only flows no recorded context admits,
and clearing them would buy nothing but false positives. This section exists because §3.1 has
one acknowledged hole — the cycle-drop residue it names — and because the corpus may find
another.

Both mismatch arms of `PathState::expand` traverse with the obligation **cleared** instead of
returning `None`:

| arm | today | change |
| --- | --- | --- |
| `Flow(Return(site))`, `top(ctx) != site` | `None` | `Some({Free, ∅})` |
| `Step::Ctx(row, e)`, `refine(ctx, row) == None` | `None` | `Some({state per e, ∅})` |

`∅` is the bottom of the generalization order, so a cleared state is immediately a candidate
for subsumption against the context-free region; this makes the search **cheaper**, not more
expensive. Applying the first arm alone is what isolated Cause 3 in §2.4, so it is also the
diagnostic to reach for if a flow goes missing after §3.1: if clearing recovers it, the
obligation data is still incomplete somewhere.

If it is ever needed as more than a diagnostic, gate it the permissive way round —
`CTADL_QUERY_CONTEXT_LOOSE=1` selects it — since §3.1 makes the pruning default the sound one.
Precision cost is one-directional: flows the contexts were pruning come back as findings.

### 3.5 Determinism of what gets reported

Two orderings currently inherit the index's row order (§7.2), and both become observable
choices once pairs are reported per source:

1. **Endpoint order.** Sort each label's endpoints by `(infunc, vertex, call_site, saturating)`
   before building `starts`, and sort `sink_nodes`' per-vertex endpoint vectors the same way.
   The *set* of witnessed pairs is then order-invariant by construction (it no longer depends
   on which source reached a node first).
2. **Path shape** still follows successor order, which follows row order. That is `codeFlow`
   churn, not finding churn, and it is pre-existing (§7.2).

This is the second prize in this change: §7.2's ±4 finding spread on `cajino_baidu` was
*caused* by first-reach attribution and one-path-per-vertex reporting. Removing both should
remove the spread; §6.5 makes that a gate.

### 3.6 Optional: early exit for the witness passes

A witness pass has nothing left to do once it has a path to every vertex in `reachable`.
`find_annotated_paths_from_set` always explores everything, so this needs a knob in
`ctadl-ir/src/graph/mod.rs` — cheapest form is letting `is_target` return a
`std::ops::ControlFlow` (or a separate `stop: impl Fn(&AnnotatedSetSearch) -> bool` checked
when a target is recorded). Land §3.2–§3.5 first and measure; this is a constant-factor
optimization, not part of the property.

## 4. Cost

| case | searches per label | wall | peak |
| --- | --- | --- | --- |
| one source vertex | 1 (gate doubles as witness) | unchanged | unchanged |
| many sources, no sink reached (`fw_pppd` `argv_input`) | 1 | unchanged | unchanged |
| many sources, sinks reached | 1 + `E` | up to `(1+E)×` this label | `max(gate, one witness pass)` |

`E` = distinct source *start vertices* for the label, which is the number of call sites the
model's source functions have (`anchored_at_callsites`, `mod.rs:104`): 2 in the reproducer,
one per `getenv`/`getDeviceId` call site on a real target. The per-label debug line already
prints it — **measure `E` per label on the corpus before landing**, because it is the whole
cost story.

Peak memory does not grow: witness passes run one at a time and the gate's `states` +
`visited` are dropped first (`drop(gate)` in §3.2 is load-bearing — that is 2.2 GB on
`fw_pppd`). A single-start pass is *usually* smaller than the shared one, though not strictly
a subset: the shared pass can subsume a contextual state via a *different* source's more
general state, which a single-start pass will not have.

Witness passes are independent, so `E` is recoverable in wall time with rayon at a memory
cost of `concurrency × one pass`. Do it only if §6.7 shows the multiplier hurting.

## 5. What has to change outside `search.rs`

The search change is necessary and not sufficient; these are small and named so the PR can
say which it took.

### 5.1 Pair outside the detail-node gate (needed for the general case)

`formatter.rs:2667` pairs only at detail nodes. Add the fallback `find_endpoint_paths`
already uses (`formatter.rs:1767`): for any (source, sink) pair whose tags meet on *no*
detail node, pair directly at the sink's endpoint vertex and resolve the reporting location
from the sink's own `call_site`/`infunc`. Without this, a witnessed pair on a
function-anchored endpoint stays unreported (and, with §5.3, at least becomes visible).

### 5.2 Optional: carry the witness instead of re-deriving it (clause two of §0)

Emit the witness path itself — a `(pair id, step index, vertex)` relation alongside
`taint_edge`, or a `taint_edge` variant keyed by pair — and have the formatter report that
path when it has one, falling back to the re-walk otherwise. This is the only way to
guarantee the reported `codeFlow` is *this pair's* path rather than a splice, and it also
removes the last order-dependent input to the reported path shape. It costs a persisted
schema addition, so it is a separate diff from §3.

### 5.3 Counters, so a violation cannot be silent

`PathStats` (`formatter.rs:329`) counts `reported` and `dropped_no_location`. Add:

- `pairs_considered` / `pairs_unwitnessed` — a pair whose tags met but whose re-walk found no
  path. **After §3 this must be 0**; it is the direct machine-checkable form of §0.
- `pairs_no_location` — witnessed but unreportable (the §5.1 residue).

Emit both as `invocations[0]` notifications the way `dropped_no_location` already is
(`formatter.rs:831`).

## 6. Validation

1. **Longest-common-suffix join (§3.1), on its own, before anything else.** Unit tests on
   `CallString::longest_common_suffix` (suffix pair keeps the shorter; incomparable pair goes
   to `[]`; empty is absorbing) and on `SmallestCallString::join_mut` (idempotent, commutative,
   and length non-increasing, which is the termination argument). Then `twosite.c` as a
   regression case — `nightly/tests/c/funcptrcalleetwosite.c`, `expected_lines: [16, 18]`,
   which fails today. **Run and recorded on the prototype:** `cargo xtask regression
   --frontend c` → 26 passed / 0 failed / 2 xfail, all four `funcptr*` cases; `--frontend lua`
   → 28 passed / 0 failed, all three `resolved-callee-*`. Still owed: §6.7's peak on `fw_pppd`
   and §6.8's oracle differential on `fakedaum`, both taken *before* §3.2 lands so the two
   changes' effects on finding counts stay separable.
2. **Unit tests in `search.rs`'s `mod tests`** (no frontend needed, the existing tests build
   `QueryFacts` by hand): (a) two sources → one sink vertex yields two witnessed pairs, with
   `taint_edge` containing an edge out of *both* start vertices; (b) a source whose only route
   to the sink is longer than another source's still gets its own witness; (c) a
   `Flow(Return)` frame mismatch on a resolution recorded under *two* contexts no longer drops
   the flow — the join records their common suffix (§3.1) — and still does drop it when the
   resolution has a single recorded context.
3. **The reproducer as a regression case.** `nightly/tests/c/twosource.c` + query json;
   both the `pcode` and tree-sitter `c` frontends pick it up automatically. The harness
   asserts lines, not pair counts, so add an optional `expected_path_count` key
   (`xtask/src/assertions.rs`, alongside `read_expected_lines`) and assert `2`. Without a
   count assertion this case passes today.
4. **Flowy `requires`.** `check_human_profile_paths` (`codegen/flowy.rs:229`) already asserts
   per-endpoint path existence through `find_endpoint_paths`, which has no detail-node gate —
   a multi-source flowy case is the cheapest end-to-end property harness in the tree. Add one.
5. **Corpus differential**, 17 benchmarks (12 TaintBench APKs, 4 Operation Mango cmdi
   binaries, 3 `large_dataset` firmware), **one store per benchmark, queried by both
   binaries** (§7.2 — per-side imports are not a valid differential; the same unmodified
   binary swung 353 vs 357 across two imports of `cajino_baidu`). Expectations, and they are
   *not* "identical counts" this time:
   - findings must be a **superset** of `c666df16`'s — every new one is a pair that was
     connected and unreported;
   - `cajino_baidu` must still include the 3 findings D4 bought (≥ 353 on the import that
     produces 353);
   - repeat the 11-import spread from §7.2 and check the count is now **stable across
     imports**. That is §3.5's claim, and the sharpest available signal that the fix is
     structural rather than incidental.
6. **Contextual-dispatch tests**: `nightly/tests/c/funcptrcallee{source,sink,frame}`,
   `nightly/tests/lua/resolved-callee-*`. §3.1 relabels contexts rather than loosening the
   check, so these are the precision gate: they should still pass, and a *new* flow appearing
   in one means a pair merged to `[]` that should have kept a context. Both suites pass on the
   prototype.
7. **Cost gate on `fw_pppd`**: wall and peak must be unchanged (zero sinks reached ⇒ one
   search, per §4). If they move, the `reachable.is_empty()` guard is not doing its job.
8. **Datalog oracle** (`CTADL_QUERY_DATALOG=1`) on a target with a real contextual region —
   `fakedaum` (43% of states carry a call string, runs in seconds), not `fw_pppd` (whose
   closure run does not finish in 10 minutes). Compare **finding sets only**: the oracle's own
   `codeFlow` step counts vary run to run (104 findings both runs, 34 vs 20 steps). The
   question is comparative — the pruned-and-witnessing search should disagree with the oracle
   *less* than the tip does, and every remaining disagreement should land in §7.

## 7. Semantic boundaries — where "a data-flow path" is deliberately narrower

Do not chase these as property violations; do expect them in the oracle differential.

1. **Sink matching is exact on the vertex.** `is_target` (`search.rs:792`) matches
   `(func, var, path)` exactly. Taint arriving at `x` when the sink names `x.f` is a flow only
   for a `saturating` source (`sink_ext_by_var`, `search.rs:464`). The datalog oracle pairs
   more liberally because its *backward* rules walk the sink's aliases down to bases
   (`mod.rs:576`–`:600`), so it will report pairs the search will not.
2. **The materialized-paths gate.** Both engines drop a step whose result path is not in
   `paths`; shared semantics, no differential.
3. **Context obligations at all.** Even after §3.1, `refine` still restricts where it
   *succeeds* consistently. The oracle collapses contexts entirely (`mod.rs:429`) and so finds
   strictly more. That direction is intentional and documented there.

## 8. Risks

| risk | signal | mitigation |
| --- | --- | --- |
| `E×` wall time on a label with many source call sites | §6.7/§6.5 timings; the per-label debug line's endpoint count | the two guards in §3.2 (single source, no reachable sink) cover the common cases; then §3.6 early exit, then rayon across witness passes |
| §3.1 merges a pair to `[]` that should have kept a context | new findings in `funcptrcallee*` / `resolved-callee-*`, or a jump on `fakedaum` | precision-only, never completeness; both suites pass on the prototype, and §6.6 is the gate |
| §3.1 grows the context-free graph (merged pairs feed `assign_like`, not `context_assign`) | `fw_pppd` peak and state count, `assign_like` row counts | §6.7 before §3.2 lands, so the two changes stay separable; if it bites, keep merged pairs in `context_assign` under `[]` rather than routing them to the unconditional head |
| §3.1's cycle-drop residue leaves a recursive context unrecorded | a flow missing on a recursive dispatch that returns under §3.4's first arm | widen `push` failures to `top()` (its own diff, §3.1); §3.4 stays on the shelf until that lands |
| Witness rows inflate `taint` | row counts per benchmark | growth is `pairs × path length`, findings-proportional; if it bites, tag only call-arg and sink nodes on the path instead of every node |
| Union-graph splicing reports a path that is not this pair's | `codeFlow` diffs on a multi-source case | pair completeness is unaffected; §5.2 is the fix, as its own diff |
| A witnessed pair still unreported (detail-node gate) | `pairs_no_location` > 0 (§5.3) | §5.1 |
| Read as "no change, counts moved" in review | count *increases* are the point this time | state the expected direction up front: findings ⊇ tip, and the per-import spread → 0 |
| Index row order read as a regression | a count diff that does not reproduce when both sides query one store | §6.5: one store per benchmark, both binaries; mechanism in §7.2 |

---

## Appendix: the memory story (landed subsumption, retained for the budget)

### 7.1 The regression and the fix, as measured

Bisected on `fw_pppd` (238 KB Linksys firmware through the pcode frontend,
`firmware-eval/models/cmdi-firmware.json5`). Query phase only. Peak is `phys_footprint`
(`footprint -p <pid> -f bytes`, cross-checked against `/usr/bin/time -l`), not RSS, which
undercounts on macOS.

| commit | wall | peak footprint |
| --- | ---: | ---: |
| `6ecbfb45` (main) | 5.90 s | 1.48 GB |
| `6a58056a` (= `cf558108^`) | 6.00 s | 1.48 GB |
| **`cf558108`** "Query finds sinks under contexts" | **10.59 s** | **4.10 GB** |
| `9c18bad5` (`ascent_par!`) | 10.45 s | 4.08 GB |
| `c666df16` (subsumption landed) | — | 2.17 GB / 6,471,144 states |

One commit owned the whole delta. `taint_search` runs one search per source label; the
`file_input` search was untouched (3.62 M states, 0 context-bearing) and `argv_input` was the
regression:

| | main | `cf558108` |
| --- | ---: | ---: |
| states in the `argv_input` search | 6,194,425 | **14,029,871** |
| — context-free | 6,194,425 | 6,194,551 |
| — carrying a call-string obligation | 0 | **7,835,320** |
| vertices explored under >1 annotation | — | 3,929,497 (max 3) |
| `size_of::<SearchState>()` | 48 B | **88 B** |
| `states` Vec at final capacity | 402 MB | **1.48 GB** |
| `visited` entry / table | 40 B → ~301 MB | 56 B → ~837 MB |

The trigger was small — `context_assign.parquet` holds 8,227 rows in 2 functions with 2
distinct call strings — because `visited` is keyed on `(node, annotation)` and the annotation
now carries a `CallString`. `malloc_history -callTree` confirmed `TaintSearchGraph::new` is
flat across both sides (97.8 vs 98.4 MB): the new indices are not the cost, the `states` Vec
realloc is (and its doubling is why peak outruns live bytes).

Two multipliers, one fixed: **state splitting** (×2.26, fixed by subsumption — an obligation
only ever restricts, so a context-bearing state buys nothing on a node also reached
context-free) and **per-state size** (×1.83, open): `CallString` is `immortal!`-interned as
`&'static [PackedInsnSiteId]`, i.e. 16 bytes rather than a 4-byte id, which is what `phospy`
paid with an unchanged state count (2.42 → 3.30 s).

Open items, in order, after §3 lands and is measured:

1. Intern `CallString` as a `u32` id (`facts.rs:283`): `PathState` 24 → 8, `Step` 32 → 16,
   `SearchState` 88 → ~56, `visited` entry 56 → 40. ~35–40% off peak, zero semantic change.
2. Move `edge: Option<L>` out of `SearchState`. Labels are needed only for ancestors of
   target states; on `fw_pppd` zero targets are reached, so all 14 M labels are dead weight.

### 7.2 Why per-import finding counts move: index row order, read through a lossy emission

Measured on `cajino_baidu` at `9c18bad5`: 11 import+index runs gave 345 findings nine
times and 349 twice (a different harness from the sweep's 353/357; the ±4 spread is the
point). **The index is order-nondeterministic, not content-nondeterministic** — take a
345-store and a 349-store and every relation is multiset-identical; only row *order* differs,
and only in derived relations. Copying one relation at a time isolates it to `assign`.

The searches are identical under both orders (same state counts, same context-bearing counts,
same winning sink-vertex → source-endpoint map). Two *reporting* structures differ, and they
are precisely Causes 1 and 2 of §2:

- `origin[]` (`search.rs:799`) is first-reach-wins: 44,728 of 890,002 forward `taint` rows
  differ **only in the endpoint column**.
- `reported` (`search.rs:837`) emits one path per sink vertex: `taint_edge` came out 322 vs
  303 edges.

The formatter reads both (`formatter.rs:2630`–`:2710`), so a pair whose route was not the one
emitted, or whose node was attributed to a sibling source, is never tested: 15 findings unique
to one order, 19 to the other. **This is why §3.5 predicts the spread disappears** — the fix
removes both mechanisms.

Where the row order comes from (upstream of the fixpoint, not `ascent_par!` — reverting it and
`RAYON_NUM_THREADS=1` are both still nondeterministic):

- `ctadl-ir/src/ssa/mod.rs:297` iterates a `std::collections::HashSet<ArcIntern<Variable>>`
  for phi placement and `push_front`s each phi, so phi order — hence SSA numbering and fact
  emission order — is random per process. Sorting it made every seed relation identical but
  one.
- Interned keys hash by **pointer**: `internment::ArcIntern`, `immortal!` (`facts.rs:279`),
  `tailshare::Seq`, `trie::Trie`. Addresses vary per process, so any hash container keyed on a
  `Path`/`Symbol` iterates differently every run; switching to `FxBuildHasher` does not help.

One wrinkle that is *not* order: `resolved_call` occasionally differs in **content** (1734 vs
1735 rows — one extra contextual resolution). It does not correlate with the finding delta,
but it is a real nondeterminism in the lattice witness, and it is the evidence in §2.4 that a
witness-based obligation check cannot be complete.

Two candidate upstream fixes, out of scope here: make the index deterministic (sort phi
placement — verified to collapse all but one seed; hash interned values by a monotonic id
rather than an address — not implemented), or make the query's emission order-invariant. §3
does the second for the *finding set*; path shape still follows row order.
