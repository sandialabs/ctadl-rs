# Query engine search plan: witness completeness for source→sink pairs -- DO-NOT-MERGE

Scope: `ctadl-ascent/src/query_engine/search.rs`, the generic search it drives in
`ctadl-ir/src/graph/mod.rs`, and `ctadl-ascent/src/query_engine/formatter.rs`.

This document had one subject — the state-space blowup from context obligations
(`cf558108`) and its subsumption fix. That fix **landed** (`c666df16`); the appendix keeps its
measurements because they are the memory budget every change below has to live inside.
The live subject is now a **correctness property the pipeline does not have**, stated in §0
and violated four independent ways (§2). §3 is the change, and it is ordered: **§3.1 lands
first and alone** — it is ~15 lines in `facts.rs`, it is the only part that changes what the
index records, and it is the one with a prototype and a measurement behind it (§2.4, §6.1).
§3.2–§3.6 are the query-side reporting work; §5 is the formatter side.

**What changed in this draft.** The search already derives every path it reports — it keeps the
whole first-reach forest, so `search.path_to` hands back the ordered walk with the calling
context that made each step legal. Four lines later `taint_edge.insert` shreds that path into a
global edge set, and the formatter re-derives *a* path by searching the union of every reported
path's edges. This draft stops doing that: §3.3 carries the witness, §5.1 has the formatter
report it, and Cause 4 is deleted in both halves rather than half-patched and half-deferred. It
also *removes* work the previous draft proposed — the per-node forward tagging in §3.3 existed
only so the re-derivation could find pairs, and with the pairs carried it is dead weight.

## 0. The property

> Given the query's set of source endpoints and sink endpoints: for **every** pair
> `(S, K)` such that a data-flow path from `S` to `K` exists, the SARIF output contains a
> result whose `codeFlow` is a path from `S` to `K`.

Two clauses, and they fail separately:

- **Pair completeness** — every connected pair is *reported*. This is what breaks today.
- **Witness** — the reported `codeFlow` is *this pair's* path: the walk the search actually
  took, edge for edge. Today the formatter re-derives a path instead (§2.5), so a reported pair
  comes with *some* path — possibly spliced out of other pairs' edges, possibly one the search's
  own context obligations rejected. §3.3 and §5.1 make the search hand over the path it found
  and retire the re-derivation to the datalog fallback.

"A data-flow path" means the edges `TaintSearchGraph::labeled_successors` generates, walked
under the **one-bit call/return discipline** — `TaintState::Free`/`Restricted`, the same
matching rule the closure engine enforced. Context obligations are deliberately *not* part of
the definition: they are a precision filter layered over it, and §2.4 is the claim that the
filter as built removes paths the definition admits. (Defining the relation as "whatever
`PathState::expand` accepts" would make that claim unfalsifiable, which is what an earlier
draft of this section did.) It is also deliberately **not** "any path the datalog oracle would
find" — §7 lists the four places those two regimes differ on purpose, so a differential
against `CTADL_QUERY_DATALOG=1` must not read those as violations.

Note the asymmetry the two clauses then have, once §3.3 lands: the one-bit definition governs
which *pairs* must be reported, while the reported *path* is the search's own and therefore
always context-consistent as well. Carrying the witness can only strengthen clause two — it
cannot report a walk the obligations rejected, which today's re-derivation can.

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

This makes Cause 1 unfixable on its own, and it is worth walking through why, because the
dead end is what points at §3.3. Tag the sink node with *both* sources and the pairing loop
asks the formatter for a walk from `call-arg(2,-1)` — whose vertex appears in no `taint_edge`
row at all. `find_annotated_path_to_set` (`formatter.rs:2702`) returns `None` and the pair is
dropped with no diagnostic. Emitting more tags does not help; emitting more *edges* means
emitting the whole traversed graph, which is the blowup the demand-driven regime exists to
avoid. **Neither cause is fixable by tagging, and both stop mattering once the pair and its
path are carried instead of reconstructed** (Cause 4, §2.5).

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

### 2.5 Cause 4 — the path is derived twice (outside `search.rs`)

The search knows the path at the moment it proves reachability. `find_annotated_paths_from_set`
keeps the entire first-reach forest — every state carries `parent` and the edge it was reached
by — so `search.path_to(t)` (`search.rs:846`) returns the ordered walk, each state holding its
`PathState`: the `TaintState` **and** the `CallString` obligation that made the step legal. That
is derivation 1, and being able to produce it inline is the whole reason this regime exists
instead of a closure plus a path-finding pass.

Four lines later it is destroyed. `taint_edge.insert` (`search.rs:850`) shreds the path into its
edges and drops them into one global `BTreeSet` shared by every reported path of every label.
Order goes — a set of edges is not a path. Pair identity goes — no column says which pair an
edge served. The context goes, explicitly: `.flow_edge()` is documented as a downgrade ("a
contextual step reports as the plain edge of the same kind"). The taint level goes with it.

The formatter then performs derivation 2. `build_taint_flow_graph` (`formatter.rs:1682`) interns
the union edge set into a `LabeledTaintGraph`, and `formatter.rs:2702` runs a second search per
candidate pair:

| | derivation 1 (`search.rs`) | derivation 2 (`formatter.rs`) |
| --- | --- | --- |
| graph | live `TaintSearchGraph`, edges generated from the index | union of every emitted `taint_edge` row |
| annotation | `PathState` = `TaintState` + `CallString` | bare `TaintState` — **no contexts** |
| order | BFS within annotation class (`VecDeque::pop_front`) | **DFS** (`Vec::pop`, `graph/mod.rs:196`) |
| scope | this search's edges | every pair's edges, every label |

Two bounds on the property follow, and they are one defect seen from two sides:

- **4a. Pairing only happens at a "detail" node.** Having been given no pairs, the formatter has
  to *find* them, by looking for a node carrying both a forward and a backward tag — and it looks
  only at `details_by_span`, built from `tainted_insn`, which keeps only call-arg vertices with
  `formal >= 0`, non-globals, *and* a resolvable source span (`formatter.rs:2667`). A witnessed
  pair whose whole path misses such a node is never tested. Usually harmless (the sink is an
  argument at a call), but function-anchored endpoints — the `anchored_at_callsites` fallback for
  a callerless function, or the globals pseudo-formal (`mod.rs:104`–`:133`) — land outside it.
- **4b. The reported path is re-derived, not carried.** The DFS answers with the first walk it
  finds in the union graph, which need not be the walk whose edges put it there. Today Causes 1
  and 2 mask this by keeping the pool tiny and few pairs tested; fix those two by tagging — the
  previous draft's route — and it goes live. If the search emitted `S1 → m → K1` and
  `S2 → m → K2`, the pool holds four edges, `m` carries every tag, and the pair `(S1, K2)` is
  answered `S1 → m → K2`: spliced out of two other pairs' witnesses. Every edge is real, so pair
  completeness survives and §0's first clause is unharmed; but the reported `codeFlow` is not a
  walk this search ever took, and since the re-walk carries no `CallString` it may be one the
  obligation check pruned at `search.rs:627`. Clause two fails. Carrying the witness is the only
  route that fixes 1 and 2 *without* opening this.

Derivation 2 also *filters*, which is where Cause 2 turns silent: when the pair's own edges are
not in the pool, `find_annotated_path_to_set` returns `None` and the pair is dropped with no
diagnostic. And it is the last word — a pair the search witnessed but the re-walk cannot
reproduce is not reported, however sound the search was.

`find_endpoint_paths` (`formatter.rs:1767`, used by the flowy checker) is the same derivation
without the detail-node gate — it pairs every source with every sink present in `taint` — so it
is a *stricter* client of the same emission, and Cause 2 hits it too.

**None of this is forced by persistence.** `schema::taint` and `schema::taint_edge`
(`facts/schema.rs:167`, `:175`) are declared but written by nothing outside the schema
round-trip test (`:405`); the query→formatter boundary is in-process, `QueryResult` handed
straight to `FormatFactsBuilder` (`cli/mod.rs:663`). The relational hand-off is inherited from
the datalog engine, which had no paths to hand over. Carrying the witness costs a struct field,
not a stored schema — so the previous draft's §5.2, which priced it as a persisted schema
addition and marked it optional, was wrong on both counts.

### 2.6 Summary

| cause | site | fixed by |
| --- | --- | --- |
| 1. one origin per state | `search.rs:777`, `:799`, `:826` | §3.2 + §3.3 |
| 2. one path per sink vertex | `search.rs:837`, `:846`, `:866` | §3.2 + §3.3 |
| 3. context join keeps a witness, not a lub | `facts.rs:445` (read at `search.rs:612`, `:630`) | §3.1 |
| 4a. pairing needs a detail node | `formatter.rs:2667` | §5.1 — there is no pairing step left |
| 4b. path re-derived from a union graph | `formatter.rs:2702` | §3.3 + §5.1 |

Causes 1 and 2 are what make the *set of reported pairs* wrong; Cause 3 is what makes it wrong
in the contextual region; Cause 4 is what makes the reported *path* wrong and Cause 2 silent.
Fixing 4 changes the standing of 1: once the formatter is handed the pairs, first-reach
attribution on the bulk `taint` rows stops deciding which findings exist (§3.3).

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

### 3.3 Emission: carry the witness

`emit_witnesses` replaces the `reported` loop (`search.rs:837`–`:880`). Per (source, sink
vertex) it records the path *as a path* — the ordered vertices and the edge walked between each
consecutive pair — instead of shredding it into the edge pool:

```rust
/// One witnessed source -> sink flow: the walk the search actually took.
pub struct TaintWitness {
    pub source: QueryEndpoint,
    pub sink: QueryEndpoint,
    /// Vertices from source to sink. Level-agnostic, for the same reason the `taint` rows
    /// are: `TaintLevel` is a search-local concern.
    pub nodes: Vec<(FunctionId, FlowVariable, Path)>,
    /// `steps[i]` is the edge walked from `nodes[i]` to `nodes[i+1]`; `nodes.len() - 1` long.
    pub steps: Vec<FlowEdge>,
}

let mut seen: HashSet<TaintVertex> = HashSet::default();
for &t in &search.targets {
    let vertex = vertex(&search, t);
    if !seen.insert(vertex) { continue }                   // first target per vertex: shortest-in-class
    let path = search.path_to(t);
    for w in path.windows(2) { taint_edge.insert(...) }    // unchanged: `--dump-taint-graph` reads this
    let nodes: Vec<_> = path.iter().map(|&i| vertex_of(&search, i)).collect();
    let steps: Vec<_> = path[1..].iter()
        .map(|&i| search.states[i as usize].edge.unwrap().flow_edge())
        .collect();
    for &e in &start.endpoints {                           // several endpoints may name one start vertex
        for sink in &sink_nodes[&vertex] {                 // and several sinks one sink vertex
            witnesses.push(TaintWitness {
                source: endpoints[e].clone(), sink: sink.clone(),
                nodes: nodes.clone(), steps: steps.clone(),
            });
        }
    }
}
```

Carried on `QueryResult` (`query_engine/mod.rs:226`) beside `taint_edge`, and through
`TaintAnalysisResults` (`formatter.rs:1523`) to the formatter. No parquet, no schema change,
no new relation — see §2.5's last paragraph. `steps` reads `SearchState::edge`, which the
appendix's open item 2 (moving `edge` out of `SearchState`) explicitly preserves for ancestors
of target states; the two changes do not conflict.

**What this deletes.** The previous draft emitted a third thing — a forward `taint` tag on every
node of every witness path, so the formatter's meeting-node test would succeed there. That
existed solely to help derivation 2 find pairs. With the pairs carried there *is* no
meeting-node test, so the tags are not emitted. Sink tags (`search.rs:866`) stay as they are:
they colour the backward cone in `--dump-taint-graph`, which is unrelated to pairing.

**What this demotes.** Cause 1 stops costing findings. First-reach attribution still stamps one
arbitrary (though valid) origin on the bulk `taint` rows, and those still feed the graph dump's
colouring and the Debug profile's per-vertex listing — but no longer anything that decides
*which pairs are reported*, because that set is now the witness list. Cause 1 goes from silent
finding loss to reporting-fidelity residue, and §3.5's determinism argument no longer rests on
it.

Emission size is proportional to **findings**, not to the state space: `Σ_pairs (path length ×
sinks at that vertex)` vertices and the same count of edges, once each. That is strictly less
than the tagging it replaces (`path length × (1 + sinks)` interned `taint` rows into a
`BTreeSet`). On `cajino_baidu` (890,002 `taint` rows, ~350 findings) it is noise. If the
per-endpoint clone ever matters, intern the path once per sink vertex and have the pairs index
it. Contrast the alternative of emitting every traversed edge to make the union graph complete:
14 M rows on `fw_pppd`, reinstating exactly the `taint_edge` blowup the demand-driven regime
exists to avoid.

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
2. **Path shape.** Two order-dependent inputs feed it today: which edges reach the union pool
   (search order), and which walk the formatter's DFS finds once they are there (row order,
   §7.2). §3.3 removes the second outright — the reported path *is* the search's own
   BFS-in-class path — leaving the first. That is `codeFlow` churn, not finding churn, and it
   is pre-existing.

This is the second prize in this change: §7.2's ±4 finding spread on `cajino_baidu` was
*caused* by first-reach attribution and one-path-per-vertex reporting, read through a formatter
that had to guess pairs from them. §3.2 and §3.3 remove both mechanisms and §5.1 removes the
reading, so the spread should go to zero rather than merely shrink; §6.5 makes that a gate.

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

The carried witnesses are the one thing that must outlive each pass, so they are worth naming
in this argument: they are `Σ_pairs path length`, findings-proportional and independent of the
state space (§3.3), against a per-pass peak measured in gigabytes. They do not disturb it.

Witness passes are independent, so `E` is recoverable in wall time with rayon at a memory
cost of `concurrency × one pass`. Do it only if §6.7 shows the multiplier hurting.

## 5. What has to change outside `search.rs`

§3.3 makes the witness available; this is what consumes it. Both halves of Cause 4 are deleted
here rather than patched — there is no pairing step to gate, and no re-derivation to splice.

### 5.1 The formatter reports the carried witness

- **Delete the pairing loop** (`formatter.rs:2661`–`:2717`), including its
  `find_annotated_path_to_set` call. `results_by_path` is built by interning each witness's
  `nodes` through the same `node_to_id` / `id_to_node` the graph already builds, so everything
  downstream is untouched: `endpoint_node_ids`, `call_arg_site`, the step messages, the
  `path_sites` pre-load. Keep the grouping — key on `(nodes, steps)` so two pairs sharing a
  path still collapse into one result carrying several details, exactly as today.
- **Take each step's label from the witness, not from a map.** `site_by_edge` and
  `edge_by_edge` (`formatter.rs:1728`, `:1736`) are keyed on `(src_id, dst_id)` across the whole
  pool and are last-write-wins. `taint_edge` is keyed on `(edge, src, dst)`, so one vertex pair
  can hold both an `Intra` and a `Call` row — and then whichever was inserted last decides how
  *every* path through that pair is narrated. Indexing `steps[i]` by position removes that; the
  step loop (`formatter.rs:2945`) is otherwise unchanged. This is a latent bug the change fixes
  for free, not a new requirement.
- **Resolve the reporting location per pair.** `span_key` comes from `details_by_span` today
  only because that is what the pairing loop happened to be standing on (Cause 4a). Replace it
  with the sink endpoint's `call_site` span, falling back to the last node on the path with a
  resolvable span. The `dropped_no_location` path (`formatter.rs:3155`) then becomes the sole
  remaining way a witnessed pair fails to be reported, and §5.3 counts it.
- **`find_endpoint_paths` becomes a projection.** With witnesses present it is a `map` into
  `EndpointPath`, no search (`formatter.rs:1767`). The flowy checker and the SARIF results then
  draw on the *same* set by construction — strictly stronger than today's "stricter client of
  the same emission", and it makes §6.4 a real end-to-end property harness rather than a
  correlated one.

`taint_edge` keeps being emitted unchanged: `--dump-taint-graph` (`cli/mod.rs:845`) reads it,
and `codegen/tests.rs:206` asserts a forward walk over it.

### 5.2 The re-walk stays, for the datalog regime only

`taint_analysis_datalog` (`query_engine/mod.rs:436`) computes a closure and has no paths to hand
over — there, `taint_edge` *is* the whole closure. So `build_taint_flow_graph` and
`find_annotated_path_to_set` stay in the tree, reached when the witness list is empty. That is
what keeps `CTADL_QUERY_DATALOG=1` working, which §6.8's differential depends on. "Do away with
the second derivation" means it stops being how the default regime reports — not that the code
is deleted.

Keeping it also keeps the fallback honest: the two regimes then differ in *how they report* the
same way they already differ in how they search, and §7 stays the whole list of deliberate
divergences.

### 5.3 Counters, so a violation cannot be silent

`PathStats` (`formatter.rs:329`) counts `reported` and `dropped_no_location`. The previous
draft's `pairs_unwitnessed` — a pair whose tags met but whose re-walk found nothing — is now
*structurally impossible*: there is no re-derivation left to fail. What remains:

- `pairs_witnessed` — witnesses handed over, i.e. §0's pair count as the search sees it.
- `pairs_no_location` — witnessed but unreportable (the §5.1 residue).

`pairs_witnessed - pairs_no_location` must equal the number of `(source, sink)` details across
the emitted results — *not* `reported`, which counts results, and results group every pair that
shares a path (§5.1). That identity is the machine-checkable form of §0, and it is an equality
rather than the previous draft's "must be 0" because the location residue is real and is
allowed to be non-zero as long as it is counted rather than silent.
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
   `QueryFacts` by hand): (a) two sources → one sink vertex yields two `TaintWitness`es whose
   `nodes` begin at *different* start vertices, and `taint_edge` an edge out of both; (b) a
   source whose only route to the sink is longer than another source's still gets its own
   witness; (c) a `Flow(Return)` frame mismatch on a resolution recorded under *two* contexts
   no longer drops the flow — the join records their common suffix (§3.1) — and still does drop
   it when the resolution has a single recorded context; (d) **witness integrity**, for every
   carried witness: `nodes[0]` is the source endpoint's vertex, `nodes.last()` is the sink's,
   `steps.len() == nodes.len() - 1`, each `steps[i]` is an edge the graph actually offers
   between `nodes[i]` and `nodes[i+1]`, and the `TaintState` discipline balances end to end.
   This is not bookkeeping: the re-walk being deleted was *also*, incidentally, a validity
   filter — it could only ever report a path it could itself walk. A `debug_assert` in
   `emit_witnesses` plus this test are what replace it deliberately.
3. **The reproducer as a regression case.** `nightly/tests/c/twosource.c` + query json;
   both the `pcode` and tree-sitter `c` frontends pick it up automatically. The harness
   asserts lines, not pair counts, so add an optional `expected_path_count` key
   (`xtask/src/assertions.rs`, alongside `read_expected_lines`) and assert `2`. Without a
   count assertion this case passes today.
4. **Flowy `requires`.** `check_human_profile_paths` (`codegen/flowy.rs:229`) already asserts
   per-endpoint path existence through `find_endpoint_paths`, which §5.1 turns into a
   projection of the witness list — so the checker and the SARIF results assert against one
   set instead of two correlated ones. A multi-source flowy case is then the cheapest
   end-to-end property harness in the tree. Add one.
5. **Corpus differential**, 17 benchmarks (12 TaintBench APKs, 4 Operation Mango cmdi
   binaries, 3 `large_dataset` firmware), **one store per benchmark, queried by both
   binaries** (§7.2 — per-side imports are not a valid differential; the same unmodified
   binary swung 353 vs 357 across two imports of `cajino_baidu`). Expectations, and they are
   *not* "identical counts" this time:
   - findings must be a **superset** of `c666df16`'s — every new one is a pair that was
     connected and unreported, either because it was never paired (Causes 1, 2, 4a) or because
     the re-walk could not reproduce it (Cause 4b's filter, now gone);
   - `codeFlow` diffs are expected even where the finding set is unchanged: a reported path is
     now the search's own walk rather than a DFS over the union pool, so steps may differ on
     any multi-source or contextual case. Diff finding *sets* first, paths second;
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

## 7. Deliberate divergences — where the search is narrower, and where it reports differently

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
4. **The two regimes now build `codeFlow` differently.** The default reports the search's own
   walk (§3.3); the oracle still re-derives one by DFS over its closure (§5.2). Step counts and
   step *order* are therefore incomparable between them by construction — a differential must
   compare finding sets, never flow shapes. This is new with this change and is the reason
   §6.8 says so explicitly.

## 8. Risks

| risk | signal | mitigation |
| --- | --- | --- |
| `E×` wall time on a label with many source call sites | §6.7/§6.5 timings; the per-label debug line's endpoint count | the two guards in §3.2 (single source, no reachable sink) cover the common cases; then §3.6 early exit, then rayon across witness passes |
| §3.1 merges a pair to `[]` that should have kept a context | new findings in `funcptrcallee*` / `resolved-callee-*`, or a jump on `fakedaum` | precision-only, never completeness; both suites pass on the prototype, and §6.6 is the gate |
| §3.1 grows the context-free graph (merged pairs feed `assign_like`, not `context_assign`) | `fw_pppd` peak and state count, `assign_like` row counts | §6.7 before §3.2 lands, so the two changes stay separable; if it bites, keep merged pairs in `context_assign` under `[]` rather than routing them to the unconditional head |
| §3.1's cycle-drop residue leaves a recursive context unrecorded | a flow missing on a recursive dispatch that returns under §3.4's first arm | widen `push` failures to `top()` (its own diff, §3.1); §3.4 stays on the shelf until that lands |
| Carried witnesses grow `QueryResult` | witness count × path length per benchmark | findings-proportional, and strictly less than the `taint` tagging it replaces (§3.3); if it bites, intern the path once per sink vertex and have the pairs index it |
| A carried path is not a real walk, and nothing catches it | witness-integrity assertions (§6.2d) | the deleted re-walk was incidentally a validity filter — replace it deliberately with a `debug_assert` in `emit_witnesses` and the unit test, not with trust |
| A witnessed pair still unreported (no resolvable location) | `pairs_no_location` > 0 (§5.3) | §5.1's per-pair resolution: sink `call_site`, then the last locatable node on the path |
| Deleting the pairing loop breaks the datalog regime | `CTADL_QUERY_DATALOG=1` on `fakedaum` reports nothing | §5.2 keeps the re-walk as the empty-witness fallback; §6.8 exercises it every run |
| `codeFlow` steps churn on cases whose finding set is unchanged | step counts in the corpus differential | expected and intended (§6.5): the path is now the search's, not a DFS over the pool. Compare finding sets first |
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
to one order, 19 to the other. **This is why §3.5 predicts the spread disappears** — §3.2 and
§3.3 remove both mechanisms, and §5.1 removes the formatter's dependence on either: handed the
pairs and their paths, it no longer consults `origin[]` or the edge pool to decide what is
reported.

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
