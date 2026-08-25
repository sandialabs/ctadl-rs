# CTADL engine: a resolved indirect call is unusable — fix plan -- DO-NOT-MERGE

Scope: defects **D4**, **D4b** and **D4c**, extracted from
`LUA_FRONTEND_FIX_PLAN.md`'s Phase 2. They live in the index engine
(`ctadl-ascent/src/index_engine/mod.rs`) and the query engine
(`ctadl-ascent/src/query_engine/search.rs`), not in any frontend, and they
reproduce as cleanly in C as in Lua. Defect numbering is kept from the Lua plan
so cross-references stay valid.

Why it is its own plan: nothing here needs a Lua change to be implemented,
tested or measured, and three of the five steps below are behaviour-preserving
or independently observable. The work also carries the only real *cost* risk in
the Lua programme — query-state multiplication and an index format bump — so it
wants its own A/B baseline and its own numbers, rather than landing inside a
frontend change where a regression could be attributed to either.

Note the converse, from `LUA_FRONTEND_FIX_PLAN.md`: this work is **not
observable on APISIX** until that plan's D2 and D3 land. The artifact yields
`resolvent: 0` and `context_assign: 0/144170` today, so the machinery being
repaired here is idle on it. Development and gating run against the micro-shapes
below; APISIX enters only as a cost benchmark and, later, as the end-to-end
acceptance case.

## Reproduction

`ctadl` built from this tree at `0.1.2`. Five shapes, one indirect call each,
differing only in where taint enters and where it is consumed. In Lua the
callback must be a closure (`local h = function(x) ... end`) or the Lua
frontend's D2 blocks them a step earlier with `resolvent: 0`; the C twins have
no such constraint.

| # | taint crosses the site as | sink | `context_assign` rows | today |
| --- | --- | --- | --- | --- |
| 1 | callee summary (arg → ret) | caller of the frame | 1 | **flow found** |
| 2 | callee summary (arg → ret) | the frame itself | 1 | no flow — **D4** |
| 3 | return out of the callee | the frame itself | **0** | no flow — **D4b** |
| 4 | return out of the callee | caller of the frame | **0** | no flow — **D4b** |
| 5 | argument into the callee | inside the callee | **0** | no flow — **D4b** |

Only shape 1 — the one `nightly/tests/c/funcptr.c` covers — works today.

## Defects

### D4. A resolved indirect call is unusable in the frame that contains it

When the function pointer is available in the **same** frame as the indirect
call, `index_engine/mod.rs:1320` ("Local virtual / indirect call and resolvent,
bypassing the resolvent / summary machinery") emits a plain `assign_like` and a
summary-shaped flow works (D4b is the limit of that "works", and it bites here
too). When the pointer arrives from a **caller**, the hybrid-inlining rules
produce a `context_assign` tagged with a call string (rule 3.1, `mod.rs:1248`).
That tuple has exactly two consumers: rule 3.3b, which builds `context_locals`,
and rule 3.4, which turns a `context_locals` reaching an **out-formal** into a
`context_summary` so rule 3.2 can pop it back to the caller. There is no rule
that makes a `context_assign` usable *where it sits*. A flow consumed inside
that frame is dropped.

Two cases, identical except for where the sink is:

```lua
local function run(f, v) return f(v) end
sink(run(h, source()))            -- flow found
```
```lua
local function run(f, v)
  local r = f(v)
  sink(r)                         -- no flow
end
run(h, source())
```

The same pair in C, so this is not a Lua artifact:

```c
int  run12(transform_fn f, int v) { return f(v); }            /* flow found */
void run13(transform_fn f, int v) { int r = f(v); sink(r); }  /* no flow */
```

`RUST_LOG=debug` shows the resolution succeeding and then dying:

```
c12: resolvent: 1, context_assign: 1, context_summary: 1     <- pops back, becomes assign_like
c13: resolvent: 1, context_assign: 1, context_summary: 0     <- nothing consumes it
```

### D4b. Call resolution produces no call-graph edge, so a flow that starts or ends inside the resolved callee cannot cross the call

The hybrid-inlining rules turn a resolvent into *summary instantiations* and
nothing else. `call` is an input relation with no rule in its head — codegen
writes it, the fixpoint never extends it (`index_engine/mod.rs:1112`; `grep -n
'call(.*) <--'` over the ascent block finds only `critical_call`). The query
engine builds every call and return step from that input relation
(`callers_by_callee` / `callee_by_site`, `query_engine/search.rs:145`). So a
dynamically resolved callee has no call edge and no return edge, and taint can
cross the site only if the callee's **summary** happens to describe it — that
is, only for a formal-to-out-formal flow.

A query source anchored *inside* the callee is not a summary. Shapes 3–5 above
produce no `context_assign` row *at all*: the callback has no summary, because
the value it returns (or the sink it contains) involves a modelled endpoint,
which does not exist at index time. There is nothing there for either candidate
repair — the `assign_like` collapse or a query-side traversal — to consume:

```c
int makes_taint(void) { return source(); }
void run(fn0 g) { int r = g(); sink(r); }      /* no flow; resolvent: 1, context_assign: 0 */
void driver(void) { run(makes_taint); }
```

The in-frame bypass rule (`mod.rs:1320`) has the same blind spot, so this is not
a property of the call-string machinery but of resolving a call into a summary
instantiation at all:

```c
int makes_taint(void) { return source(); }
int main(void) { fn0 g = makes_taint; int r = g(); sink(r); }   /* no flow */
```

That is why `resolved_call` below has a second head for the bypass path, with an
empty call string.

**This is the shape the APISIX CVE chain needs.** `plugins.jwt-auth.rewrite`
calls its source directly and has to leave on its own second return, through the
`funcptr-call` at `plugin.lua:743`. No summary of `rewrite` carries that; the
engine needs a **return edge** at the site, and a summary instantiation,
contextual or not, cannot substitute for one.

### D4c. The search engine keeps one callee per call site

`TaintSearchGraph::new` builds `callee_by_site: HashMap<PackedInsnSiteId,
FunctionId>` with `insert` (`query_engine/search.rs:147`), so a site with
several `call` rows keeps whichever was loaded last. `callers_by_callee` is a
multimap, so the *return* direction is fine; the *entry* direction (actual →
formal, `search.rs:315`) silently follows one target, as does the
`absorbing_functions` projection (`search.rs:551`).

This is live today on the default strategy: a Lua method call emits its whole
CHA target set as `call` rows *and* a `callee_info` row (`codegen/mod.rs:565`
ff.), and `--strategy cha` does the same for every frontend. Two classes with a
`go` method, the sink in one of them:

```lua
function A:go(x) sink(x) end      -- sink in A: no flow (search), flow (datalog)
function B:go(x) print(x) end
local function run(o, v) o:go(v) end
run(A.new(), source())
```

Swapping the sink into `B` — the target that survives the overwrite — makes the
flow appear. The datalog engine (`CTADL_QUERY_DATALOG=1`) joins `call` as a
relation and finds both; the two regimes disagree, which is how this surfaced.

It is an independent bug with its own fix, and it is a prerequisite here: a
resolved indirect site is a multi-target site by construction, so leaving it in
place would silently cap every new call edge at one target.

## Design

**The query-side design is the right shape**, for a stronger reason than the
precision argument that motivated it. Teaching the query engine to traverse
`context_assign` under a call-string context, rather than collapsing it into
`assign_like`:

- leaves the index unchanged in size, which was the collapse's only real risk,
  and keeps the per-context answer the index worked to compute instead of
  unioning it away;
- decisively: the edges shapes 3–5 need **cannot** be pre-instantiated in the
  index at all. Whether taint crosses a resolved call depends on where the
  query's sources and sinks are, which the index does not know. The index can say
  *"this site resolves to that function under this context"*; only the query can
  use it. The collapse can never reach shapes 3–5; a query-side design can, for
  the cost of one more persisted table.

So: adopt it, extend it to call and return edges, and keep the collapse only as
a flagged A/B baseline.

### 1. Persist what the query needs

Neither relation exists on disk today — `IndexResult::try_save` writes `summary`,
`assign_like`, `paths`, `external_function` (`index_engine/mod.rs:385`), and
`context_assign` is internal to the fixpoint. Two new tables:

- `context_assign.parquet` — `(func_id, dst_var, dst_path, src_var, src_path,
  call_string)`, rules 3.1/3.2's head as it stands. A `CallString` column encodes
  like `Path` does, as a delimited string of its `PackedInsnSiteId` frames
  (`facts/parquet.rs:522`).
- `resolved_call.parquet` — `(func_id, insn_id, target_id, call_string)`,
  factored out of the two rules that resolve a call today. Those are the only
  two: rule 3.1 (`mod.rs:1253`) and the in-frame bypass (`mod.rs:1322`). Every
  other `callee_info` premise (`:1129`, `:1207`, `:1394`) resolves nothing.

```rust
// The resolved callee of a dynamically dispatched site, under the context that resolves it;
// an empty call string means unconditional. Both rules below discard this today, joining
// straight through to the callee's summary — which describes only formal-to-out-formal flow.
// A query source or sink *inside* the callee is not a summary, so the site needs a call-graph
// edge of its own (D4b).
relation resolved_call(FunctionId, InsnId, FunctionId, CallString);

// Contextual: a resolvent pushed down from a caller. `resolvent`'s call string is non-empty by
// invariant, so these rows are exactly the conditional ones.
resolved_call(caller, call_insn, resolvent_func, *cs) <--
    callee_info(caller, call_insn, v_rec, p_rec, dispatch_key),
    locals(caller, v_rec, p_rec, n, p),
    resolvent(caller, n, p, resolvent_obj, cs_lat),
    if let SmallestCallString::Value(cs) = cs_lat,
    callee_resolvents(resolvent_obj, dispatch_key, resolvent_func);

// In-frame: the target is stored in this very frame, so the resolution is unconditional.
resolved_call(func_id, insn_id, resolve_tgt, CallString::new()) <--
    callee_info(func_id, insn_id, arg, arg_p, dispatch_key),
    call_target_assign_like(func_id, arg, arg_p, cto),
    callee_resolvents(cto, dispatch_key, resolve_tgt);
```

**Both existing consumers then join it instead of repeating the join.** That is
the point of factoring it out: `resolved_call` becomes the single definition of
"this site resolves to that function under this context," and the two rules that
instantiate a summary from one collapse to the same shape, distinguished only by
whether the context is empty:

```rust
// Rule 3.1, restated. The `!cs.is_empty()` guard keeps `context_assign`'s non-empty-call-string
// invariant; the empty-string rows belong to the bypass rule below.
context_assign(caller, v1, p1_sum.clone(), v2, p2_sum.clone(), SmallestCallString::Value(*cs)) <--
    resolved_call(caller, call_insn, resolvent_func, cs),
    if !cs.is_empty(),
    summary(resolvent_func, n1_sum, p1_sum, n2_sum, p2_sum),
    let v2 = call_arg!(*call_insn, *n2_sum),
    let v1 = call_arg!(*call_insn, *n1_sum);

// The in-frame bypass (`mod.rs:1320`), restated: the same instantiation with no context to carry.
assign_like(func_id, v1.into(), p1, v2.into(), p2) <--
    resolved_call(func_id, insn_id, resolve_tgt, cs),
    if cs.is_empty(),
    summary(resolve_tgt, n1, p1, n2, p2),
    let v2 = FlowVariableKind::CallArg(PackedCallArg::try_from_parts(*insn_id, *n2).unwrap()),
    let v1 = FlowVariableKind::CallArg(PackedCallArg::try_from_parts(*insn_id, *n1).unwrap());
```

Equivalent to what those rules compute today, one join each rather than three,
and the table the query engine needs falls out of the refactor instead of being
bolted on beside it. Everything stays in the same SCC as `locals` / `resolvent`,
so nothing about stratification changes; the only new work in the fixpoint is
materializing a relation that was previously an anonymous intermediate.

**Plain relation, not a lattice** — the head cannot carry `SmallestCallString`.
Keyed on `(func, insn, target)`, a lattice would keep one call string per
resolved edge, and two *different* non-empty contexts resolving the same site to
the same target are independent entry conditions: each is a real stack
configuration, and dropping either loses flows at query time. It is the same
witness-versus-enumeration problem as §3, but here there is no reason to accept
it — the relation is small.

One subsumption is worth taking, at load time rather than in the fixpoint: an
empty-call-string row for `(site, target)` makes every non-empty row for that
pair redundant — unconditional dominates conditional — so drop the latter when
building the query's edge index. That holds the search's per-context state
multiplication down without losing an edge.

`resolved_call` is strictly smaller than `context_assign`: it drops the cross
product with the callee's summary rows. It is also the *realized* fan-out of a
dispatched site, which is what `ctadl inspect` should report per site and what a
frontend's fan-out acceptance criteria are measured from — as opposed to a
codegen-side cap, which bounds what dispatch codegen emits in the first place.
The two are complementary: a cap applied here would gate the query-side
call/return edges but could not retract a summary instantiation the fixpoint has
already derived.

### 2. The annotation

- *Don't widen the persisted `TaintState`.* It is a parquet column of
  `taint.parquet`, encoded as a bool (`facts/schema.rs:120`,
  `facts/parquet.rs:1047`). Make the search's annotation a search-local pair —
  `struct PathState { state: TaintState, ctx: CallString }` — and emit `state`
  alone in the `taint` rows. `CallString` is interned and `Copy`, so `PathState`
  stays `Copy` and satisfies `LazyAnnotation`'s `Eq + Hash` bound.
- *Compatibility is suffix-based, not identity.* A call string is ordered
  outermost-first, innermost-last (`push` appends, `pop` takes the last,
  `facts.rs:298`), so the *current* frame sits at the end. Two obligations are
  jointly satisfiable exactly when one is a suffix of the other, and their
  conjunction is the longer:

```rust
// `[s1,s2]` and `[s2]` agree — both say "this frame was entered at s2"; the first adds that
// its caller was entered at s1 — so the refinement is the longer string. The empty context
// (no obligation yet) is a suffix of everything, which is the "identical or empty" case.
fn refine(ctx: CallString, row: CallString) -> Option<CallString>;
```

The three edge rules then read:

- **Contextual edge** — a `context_assign` row, or a call/return edge derived
  from a `resolved_call` with a non-empty call string: traversable iff
  `refine(ctx, row)` is `Some`; the new context is that refinement.
- **`Return(site)`** — still requires `Free`, and additionally, when `ctx` is
  non-empty, its top frame must be `site`; traversing pops it. A mismatch prunes
  the edge. This is where the precision the design is for actually lives.
- **`Call(site)`** — unchanged. Nothing is pushed.

### 3. Why the asymmetry is right

`resolvent` and `context_assign` are *lattices* keyed on their non-context
columns with `SmallestCallString` as the value (`index_engine/mod.rs:1105`,
`:1110`): a tuple derivable under two contexts records only the smaller one. The
recorded call string is a **witness, not an enumeration**. Pushing on `Call` and
testing entry against that witness would reject flows that enter through the
other, merged-away call site — trading away recall on exactly the axis this work
exists to repair. Popping on `Return` is safe by comparison: what it can prune
are flows that *leave* the frame, and leaving the frame already has a complete
mechanism of its own — 3.3b/3.4 lift a contextual flow reaching an out-formal
into a `context_summary`, and 3.2 pops it into the caller as a `context_assign`,
or as a plain `assign_like` once the string empties.

Not *entirely* redundant, and worth writing down before it is rediscovered as a
bug: a flow that starts inside the frame (a source anchored at a call site in it)
and then returns has no summary counterpart, and the witness may name a different
caller than the one being returned to. Cheap mitigation if it shows up in
practice — on mismatch, clear the context and allow the return rather than
pruning it, behind the same flag as the collapse. Measure before choosing.

### 4. Both edge kinds are needed; neither subsumes the other

A summary-shaped flow (shapes 1–2) cannot be recovered by descending through a
resolved call edge: `Call` enters `Restricted` and the matching `Return` is then
pruned, by design. A source inside the callee (shapes 3–5) cannot be recovered
from a summary instantiation, because no summary describes it. `context_assign`
traversal covers the first, `resolved_call` call/return edges the second.

### 5. Query-engine wiring

- Index `context_assign` by source variable exactly as `assign_by_src`
  (`search.rs:76`) and, for load-shaped rows, into `loads_by_dst` — the alias
  back-flow the collapse would have got for free.
- **Do not** feed context rows into `compute_copy_alias` (`search.rs:158`). A
  context-conditional empty-path copy entering the union-find merges two copy
  classes *unconditionally* and hands back precisely the imprecision this design
  exists to avoid. Most `context_assign` rows are empty-path call-arg copies, so
  this is not a corner case.
- The graph needs a fourth edge case, but `FlowEdge` is persisted in
  `taint_edge.parquet` (`facts/schema.rs:129`). Leave it alone and give the
  search a local label — `enum Step { Flow(FlowEdge), Ctx(CallString, FlowEdge) }`
  — mapped back to `FlowEdge` when path edges are emitted (a contextual assign is
  an `Intra` step at the call site; a contextual call/return keeps its
  `Call`/`Return`). The SARIF formatter re-walks `taint_edge` with the plain
  `TaintState` discipline (`formatter.rs:1584`, `:1628`) and is unaffected.
- `callee_by_site` becomes a multimap and the entry edge fans out over every
  target of the site (D4c) — mandatory here, since a resolved site is
  multi-target by construction.
- The **entry** edge inherits the site's argument convention, which is a
  frontend-dependent hazard the engine cannot paper over: a Lua `FuncPtrCall`
  carries the callee value as actual argument 0 (`languages/lua/mod.rs:2169`),
  which lines up with a closure's leading `%self` but is off by one for a named
  `function _M.f(...)`. Returns sit at negative indices and are unaffected. The
  engine's obligation is to anchor the entry edge at the statement whose actuals
  the resolved callee's formals actually match; a frontend that emits both
  conventions at a site must say which statement carries the dispatch.
- The datalog fallback (`CTADL_QUERY_DATALOG=1`, `query_engine/mod.rs:364`) gets
  no contexts. Give it the collapse — `context_assign` as `assign_like`,
  `resolved_call` as `call` — so the two regimes agree modulo precision, and say
  so in its doc comment.

### 6. Cost

A search state is `(node, annotation)` (`ctadl-ir/src/graph/mod.rs:327`), so a
vertex reached under *k* distinct contexts becomes *k* states. Contexts are
introduced only by contextual edges and only shrink at returns, so *k* is bounded
by the distinct call strings on the rows a search actually touches: 0 on APISIX
today, 1 in each micro-case. Index size is unchanged apart from the two additive
tables (`context_assign` was 1/35–1/37 of `assign_like` in the C micro-cases and
0/144170 on APISIX; `resolved_call` is at most one row per site/target/context).

### 7. Keep the collapse as a flagged fallback

The one-rule version —

```rust
assign_like(f, v1.clone(), p1.clone(), v2.clone(), p2.clone()) <--
    context_assign(f, v1, p1, v2, p2, _);
```

— stays available behind `--index-context-collapse` (default off) as the A/B
baseline for the query-side design and as the escape hatch if state
multiplication surprises us on the large JVM corpora. It fixes shape 2 only.

## Implementation order

Five steps. E1 and E2 are independently landable and independently measurable;
E3 is the format bump; E4 is the payload; E5 is the baseline switch. Do not
bundle E2 with anything.

### E1 — D4c: `callee_by_site` becomes a multimap

Smallest and most independent: a live bug today on the default strategy, no
format change, no interaction with the call-string machinery. `callee_by_site:
HashMap<PackedInsnSiteId, FunctionId>` (`search.rs:147`) becomes a multimap; the
entry edge (`search.rs:315`) and `absorbing_functions` (`search.rs:551`) fan out
over every target.

Gate: the multi-target case reports with the sink in *either* target, under both
query regimes; `CTADL_QUERY_SIZES` on the benchmark corpora moves within the
budget agreed below.

### E2 — factor out `resolved_call` (behaviour-preserving)

Rewrite rule 3.1 and the in-frame bypass on top of a materialized
`resolved_call`, per §1. Nothing is persisted yet and no behaviour changes.

Gate: `context_assign` and `assign_like` are **row-for-row identical** to the
pre-change index on every benchmark corpus, and fixpoint wall-clock and peak RSS
are within noise. This is the step that touches load-bearing rules in the hot
SCC; land it alone.

### E3 — persist `context_assign.parquet` and `resolved_call.parquet`

Schema, writer, reader, and the load-time subsumption of conditional rows by an
unconditional row for the same `(site, target)`. `INDEX_FORMAT_VERSION` goes
3 → 4 (`project.rs:141`, and the assertion at `ctadl-ascent/tests/cli.rs:520`);
old stores refuse with the existing "re-run `ctadl index`" error.

Gate: round-trip tests for both tables; index size grows only by the two tables,
reported per corpus.

### E4 — query-side contexts, call edges and return edges

`PathState`, `refine`, the three edge rules, the wiring of §5, and the datalog
fallback's collapse. This is where shapes 2–5 start reporting.

Gate: all five shapes report, in Lua and in C; the nightly suite is green;
`CTADL_QUERY_SIZES` within budget.

### E5 — `--index-context-collapse` (default off)

The one-rule collapse of §7, as the A/B baseline and the escape hatch.

Gate: with the flag on, shape 2 reports and shapes 3–5 do not — i.e. it is
genuinely the weaker configuration, and the A/B measures what it claims to.

## Benchmarking

The reason this is a separate plan. Every step above is measured on the same
corpora, against the same pre-change baseline, with the numbers recorded in the
PR that lands it.

Baseline capture, before E1:

```
$ RUST_LOG=debug ctadl index --store "$S" <corpus> <corpus> 2>&1 | grep 'hybrid inlining'
$ du -sk "$S"/<corpus>/index/*.parquet
$ CTADL_QUERY_SIZES=1 ctadl query --store "$S" --models <models> -o /dev/null <corpus>
```

Metrics, per corpus, per step:

| metric | source | budget |
| --- | --- | --- |
| fixpoint wall-clock, peak RSS | `RUST_LOG=debug` `[mem cp]` checkpoints | E2 within noise; overall ≤ +10% |
| `resolvent` / `context_assign` / `context_summary` counts | `index_engine/mod.rs:357` | E2 identical; E4 unchanged (index-side) |
| index size, per table | `du` on the parquet files | growth only from the two new tables |
| query state count | `CTADL_QUERY_SIZES=1` | agreed per corpus before E4 defaults on |
| query wall-clock | `time` | ≤ +25%, else fall back to E5 |
| results found / lost | SARIF diff vs. baseline | strictly additive; any lost result blocks |

Corpora: the micro-shapes (correctness), APISIX 2.13.0 (cost on a Lua artifact
where the machinery is idle — index size and query state must barely move),
Emissary and Spring (the JVM corpora, where state multiplication would show up
first).

The A/B for the design choice itself is E4-on versus E5-on: same corpora, same
queries, comparing results found, query state count and index size. If E4's state
count exceeds budget on the JVM corpora, E5 ships as the default and E4 stays
behind the flag — but E5 can only ever reach shape 2, so that is a retreat, not
an equivalent outcome, and should be recorded as one.

## Acceptance

- Shapes 1–5 all report, in Lua and in C.
- The D4c multi-target case reports with the sink in either target, in both query
  regimes.
- The search and datalog regimes agree on which flows exist, modulo precision,
  on every nightly case.
- `context_assign` / `assign_like` row-for-row identical across E2.
- Existing nightly suite green; SARIF output strictly additive.
- The benchmark table above filled in for all four corpora.

Downstream, once `LUA_FRONTEND_FIX_PLAN.md`'s D1/D2/D3 land, this work is what
the APISIX CVE chain travels on: the code flow's step out of
`plugins.jwt-auth.rewrite` must be a *return* at `plugin.lua:743`'s call
instruction — the D4b edge — not a summary step. That end-to-end criterion lives
in the Lua plan; it is stated here only because this plan supplies the edge.

## Tests

Nightly cases. The Lua ones use a closure-valued callback so the resolvent
machinery engages without depending on the Lua frontend's D2; the C twins are the
language-neutral guards. C case files in `nightly/tests/c/` follow that
directory's unhyphenated naming (`funcptr.c`, `funcptrfactory.c`).

| case | defect | shape |
| --- | --- | --- |
| `lua/caller-supplied-callback-flow` | D4 | shape 2: pointer from the caller, sink in the frame holding the indirect call |
| `lua/resolved-callee-source-flow` | D4b | shape 3: source *inside* the callee, sink in the frame holding the indirect call |
| `lua/resolved-callee-source-return-flow` | D4b | shape 4: same, sink one frame further up |
| `lua/resolved-callee-sink-flow` | D4b | shape 5: argument into the callee, sink inside it |
| `lua/multi-target-dispatch-flow` | D4c | two CHA targets at one site; the sink is in the one the map does *not* keep |
| `c/funcptrcalleeframe.c` | D4 | the C twin of shape 2 |
| `c/funcptrcalleesource.c` | D4b | the C twin of shapes 3–4 |
| `c/funcptrcalleesink.c` | D4b | the C twin of shape 5 |

`nightly/tests/c/funcptr.c` stays as the shape-1 guard — it is the one that
already passes, and it must keep passing.

Unit tests:

- `index_engine`: a resolvable `callee_info` site yields a `resolved_call` row
  carrying the resolvent's call string; the in-frame bypass yields one with an
  empty call string. Plus E2's equivalence guard: rewriting rule 3.1 and the
  bypass on top of `resolved_call` leaves `context_assign` and `assign_like`
  row-for-row identical on the existing corpora.
- `facts`: round-trip both new parquet tables, including an empty `CallString`
  and a multi-frame one.
- `query_engine::search`: `refine` — suffix compatible, suffix incompatible,
  empty on either side; a `context_assign` row traversed under a compatible
  context and pruned under an incompatible one; a `Return` whose site is not the
  context's top frame is pruned; a site with two `call` targets produces an entry
  edge into *both* (D4c); the load-time subsumption drops conditional rows
  dominated by an unconditional one.

## Risks

- **Refactoring two live rules (E2).** Putting rule 3.1 and the in-frame bypass
  on top of `resolved_call` is meant to be behaviour-preserving, but they are
  load-bearing rules in the hot SCC. Land that step on its own, guarded by the
  row-for-row A/B and a fixpoint-time measurement, before anything else goes in.
- **Query state multiplication (E4).** The annotation is part of the search
  state, so a vertex reached under *k* contexts costs *k* states. Bounded by the
  distinct call strings actually touched (0 on APISIX today, 1 per micro-case),
  but measure `CTADL_QUERY_SIZES` on the JVM corpora before defaulting it on.
  E5 is the escape hatch and the A/B baseline.
- **Recall of the return-side context check.** `context_assign` records the
  *smallest* call string per tuple, not every one, so pruning a return whose site
  disagrees with the witness can drop a real flow — narrowly, for flows that both
  start inside the frame and leave it. Mitigation is a one-line policy change
  (clear the context instead of pruning); decide it on measurement.
- **Index format bump (E3).** Two new tables, so `INDEX_FORMAT_VERSION` goes
  3 → 4 and old stores refuse with the existing "re-run `ctadl index`" error. The
  Lua plan's Phase 4 needs a facts-schema bump too — land them in one release.
- **D4c widens the entry direction (E1).** Fanning out over every CHA target at a
  site is more edges than the search follows today, and on `--strategy cha` that
  is the whole CHA set. It is a correctness fix, but it is also the one step here
  that adds search work on corpora that never resolve an indirect call, so it
  gets its own measurement rather than riding along with E4's.

## Provenance

Extracted from `LUA_FRONTEND_FIX_PLAN.md` (Phase 2, defects D4/D4b/D4c), which
retains the APISIX CVE-2022-29266 chain analysis and the Lua-frontend defects
D1/D2/D3/D5/D6. Everything asserted here was produced from this tree with
`target/release/ctadl` at `0.1.2`:

- The five shapes were run in both Lua (closure-valued callback) and C, one store
  each: shape 1 reports; shapes 2–5 do not; shapes 3–5 produce zero
  `context_assign` rows.
- D4c was reproduced on the default strategy and under `--strategy cha`, in both
  query regimes: the search engine finds the flow only when the sink is in the
  target that survives `callee_by_site`'s overwrite, while `CTADL_QUERY_DATALOG=1`
  finds both.
- `RUST_LOG=debug ctadl index apisix` reports `resolvent: 0`, `context_assign:
  0.00 (0/144170)`, `context_summary: 0` — the hybrid-inlining machinery is idle
  on that artifact today, which is why this plan is gated on micro-shapes rather
  than on APISIX.

---

# Implementation summary (2026-08-24)

All five steps landed. **Shapes 1–5 all report, in C and in Lua, under both query
regimes**, and the D4c multi-target case reports with the sink in either target.
Index wall-clock and peak RSS *improved*; the one cost is query-state
multiplication, reported in full below.

## What was built

| step | change | where |
| --- | --- | --- |
| E1 | `callee_by_site` becomes a multimap; the call-entry edge and the `absorbing_functions` projection fan out over every target | `query_engine/search.rs` |
| E2 | `resolved_call` factored out of rule 3.1 and the in-frame bypass; both now join it | `index_engine/mod.rs` |
| E3 | `context_assign.parquet` + `resolved_call.parquet`; a `CallString` parquet codec; `INDEX_FORMAT_VERSION` 3 → 4 | `facts/schema.rs`, `facts/parquet.rs`, `index_engine/mod.rs`, `project.rs` |
| E4 | `PathState`, `refine`, `Step`, the contextual assign / load / call / return edges, and the datalog fallback's collapse | `query_engine/search.rs`, `query_engine/mod.rs`, `cli/mod.rs` |
| E5 | `--index-context-collapse` (default off) | `index_engine/mod.rs`, `cli/mod.rs`, `main.rs` |

Two design points that the plan left open and the implementation had to settle:

- **A contextual `Return` is refined, not popped.** The plan's three edge rules
  are disjoint, and a `resolved_call` return edge with a non-empty call string is
  a *contextual* edge, so it goes through `refine` rather than through the pop
  discipline. That is not a detail: its call string describes the frame it
  returns *into*, so taking it is an obligation being **acquired**. Popping it
  instead would prune shape 3 outright — the search arrives at the callee's
  out-formal with an empty context, and the row's own frame is not the dispatch
  site. Shape 4 is what shows the two rules composing: the contextual return
  acquires `[S]` on the way into `run`, and the *ordinary* return out of `run`
  at `S` then discharges it by popping. A contextual `Return` still requires
  `Free`; the one-bit discipline is orthogonal to the context and unchanged.
- **`--index-context-collapse` withholds both context tables.** §7 describes the
  flag as an A/B baseline *and* an escape hatch from query-state multiplication.
  Emitting the collapsed `assign_like` rows while still shipping
  `context_assign.parquet` would be neither: the query engine would traverse both
  regimes at once, paying the per-context state the flag exists to avoid on top
  of edges it can already take unconditionally. So under the flag the fixpoint
  runs the collapse rule and the result withholds both tables (written, but
  empty, so the on-disk format is unchanged). This is what makes the E5 gate
  measurable at all.

## Corpora

The plan names APISIX 2.13.0, Emissary and Spring. **None is present on this
machine**, so two substitutes of the same shape were used, and the numbers below
are theirs:

- **Kong** (`/Users/dbueno/proj/kong/kong`, 605 `.lua` files, 4,306 functions) —
  a Lua API gateway, the same class of artifact as APISIX. It is a *better* cost
  benchmark than APISIX would have been: APISIX reports `resolvent: 0` and
  `context_assign: 0/144170`, i.e. the machinery is idle on it, whereas Kong has
  `resolvent: 259`, `context_assign: 23`, `resolved_call: 87` — so it actually
  exercises the code under test.
- **baksmali 3.0.9 (fat jar)** (26,221 functions) — the JVM corpus, standing in
  for Emissary/Spring, and where state multiplication would show up first.
  `resolvent: 4217`, `context_assign: 1350`, `resolved_call: 16532`.

`--strategy cha` on baksmali does not reach a fixpoint within 10 minutes **on the
pre-change tree as well as after** — it is a pre-existing property of that
strategy on a 26k-function artifact, not a regression. The default (mixed)
strategy is what is measured.

## Micro-shapes: correctness

| # | shape | C before | C after | Lua before | Lua after |
| --- | --- | --- | --- | --- | --- |
| 1 | callee summary, sink in the caller | 1 | 1 | 1 | 1 |
| 2 | callee summary, sink in the frame (D4) | **0** | **1** | **0** | **1** |
| 3 | return out of the callee, sink in the frame (D4b) | **0** | **1** | **0** | **1** |
| 4 | return out of the callee, sink one frame up (D4b) | **0** | **1** | **0** | **1** |
| 5 | argument into the callee, sink inside it (D4b) | **0** | **1** | **0** | **1** |

(code-flow counts). D4c, on the two-target Lua dispatch case:

| sink in | search before | datalog before | search after | datalog after |
| --- | --- | --- | --- | --- |
| `A:go` (the target the map dropped) | **0** | 1 | **1** | 1 |
| `B:go` (the target that survived) | 1 | 1 | 1 | 1 |

The two regimes now agree on **every** nightly case, C and Lua — checked
case-by-case, not just on the new ones.

## Cost

Index timing is best-of-7; query timing best-of-3. Memory is the in-process
`phys_footprint` at the `[mem cp] ascent_run returned` checkpoint.

### Kong (Lua, 4,306 functions)

| metric | baseline | after E1 | after E2 | final (E4) | Δ vs baseline |
| --- | --- | --- | --- | --- | --- |
| index wall-clock | 1.166 s | — | 1.144 s | **1.133 s** | **−2.8 %** |
| fixpoint peak RSS | 237.9 MB | — | 238.6 MB | **224.9 MB** | **−5.5 %** |
| `resolvent` / `context_assign` / `context_summary` | 259 / 23 / 18 | same | **same** | same | 0 |
| index size, total | 8,312 KB | 8,312 | 8,312 | **8,324 KB** | **+0.14 %** |
| — `context_assign.parquet` | — | — | — | 8 KB | new |
| — `resolved_call.parquet` | — | — | — | 4 KB | new |
| query wall-clock | 1.27 s | 1.30 s | 1.45 s | **1.29 s** | **+1.6 %** |
| **query states** | 6,599 | **39,113** | 39,113 | **55,564** | **+742 %** |
| results / code flows | 933 / 2 | 933 / 2 | 933 / 2 | 933 / 2 | 0 lost, 0 gained |

### baksmali (JVM, 26,221 functions)

| metric | baseline | after E2 | final (E4) | Δ vs baseline |
| --- | --- | --- | --- | --- |
| index wall-clock | 3.538 s | 3.320 s | **3.251 s** | **−8.1 %** |
| fixpoint peak RSS | 616.8 MB | 610.2 MB | **601.0 MB** | **−2.6 %** |
| `resolvent` / `context_assign` / `context_summary` | 4217 / 1350 / 579 | **same** | same | 0 |
| index size, total | 19,100 KB | 19,036 | **19,204 KB** | **+0.54 %** |
| — `context_assign.parquet` | — | — | 36 KB | new |
| — `resolved_call.parquet` | — | — | 132 KB | new |
| query wall-clock | 2.32 s | 2.69 s | **2.35 s** | **+1.3 %** |
| **query states** | 261,013 | 261,013 | **310,671** | **+19 %** |
| results / code flows | 313 / 18 | 313 / 18 | **316 / 27** | **0 lost, +3 gained** |

## Time and memory regressions

**There is no index-side time or memory regression. Both improved.** E2's
factoring replaced three joins with one in each of the two rules that resolve a
call, and the fixpoint got measurably faster on both corpora (−2.8 % Kong,
−8.1 % baksmali) with peak RSS flat to slightly down. The plan budgeted "E2
within noise; overall ≤ +10 %"; the outcome is better than budget in the
opposite direction. Query wall-clock is within +2 % on both corpora, far under
the +25 % budget.

**The one real cost is query-state multiplication, and it is almost entirely
E1's, not E4's.** On Kong the state count goes 6,599 → 55,564, a factor of 8.4 —
but the step-by-step attribution says:

- **E1 (D4c) accounts for 6,599 → 39,113, a factor of 5.9.** This is the
  fan-out the plan flagged as "the one step here that adds search work on
  corpora that never resolve an indirect call": a Lua method call emits its whole
  CHA target set, so making the entry edge follow every target is, on a
  method-heavy Lua corpus, several times the entry edges it followed before. It
  is a correctness fix — those edges were always supposed to be there, and the
  datalog regime always took them — but it is not free, and it is the number to
  watch on a CHA-heavy target.
- **E4 accounts for 39,113 → 55,564, a factor of 1.42**, and on baksmali for the
  whole +19 %. That is the per-context state the design predicted, and it is
  within the "0 on an idle artifact, small where the machinery is live" bound
  §6 argued for.

At these absolute sizes (55 K and 311 K states) neither shows up in wall-clock.
On a corpus large enough that it does, `--index-context-collapse` retires E4's
share; E1's share has no flag, because reverting it would reintroduce a
silent-wrong-answer bug.

**Results are strictly additive.** A SARIF diff keyed on flow *endpoints* —
result location plus the first and last code-flow step, so a shorter witness for
the same source/sink pair is not counted as a loss — reports **0 lost** on both
corpora, with +3 results and +9 code flows gained on baksmali. (A diff keyed on
the full step sequence shows a handful of results whose trace got *shorter*; that
is the search finding a better witness, not a lost result.)

## Gates

| step | gate | result |
| --- | --- | --- |
| E1 | multi-target case reports with the sink in either target, both regimes | **pass** |
| E1 | query states within budget | see above — +493 % on Kong, 0 on baksmali; wall-clock flat |
| E2 | `context_assign` and `assign_like` **row-for-row identical** | **pass** — byte-identical dumps of both relations on both corpora (23 / 497,733 rows Kong; 1,350 / 967,971 baksmali) |
| E2 | fixpoint wall-clock and peak RSS within noise | **pass** (both improved) |
| E3 | round-trip tests for both tables | **pass** — including an empty and a three-frame `CallString`, with frame *order* asserted |
| E3 | index size grows only by the two tables | **pass** (+12 KB Kong, +156 KB baksmali) |
| E4 | all five shapes report, in Lua and in C | **pass** |
| E4 | nightly suite green | **pass** (C and Lua; see caveat below) |
| E4 | `CTADL_QUERY_SIZES` within budget | reported above |
| E5 | flag on → shape 2 reports, shapes 3–5 do not | **pass**, in C *and* Lua |

The E5 gate doubles as the A/B the plan asked for, and it settles the design
question: the collapse reaches shape 2 and nothing beyond it, exactly as §7
predicted, so E4-on is strictly stronger rather than differently-shaped. It also
independently confirms that E4's query-side machinery — and not some incidental
effect of E1/E2/E3 — is what makes shapes 3–5 report.

## Tests

Nightly cases added (all passing):

| case | defect | shape |
| --- | --- | --- |
| `c/funcptrcalleeframe.c` | D4 | 2 |
| `c/funcptrcalleesource.c` | D4b | 3 **and** 4 (two sinks, one frame apart) |
| `c/funcptrcalleesink.c` | D4b | 5 |
| `lua/caller-supplied-callback-flow` | D4 | 2 |
| `lua/resolved-callee-source-flow` | D4b | 3 |
| `lua/resolved-callee-source-return-flow` | D4b | 4 |
| `lua/resolved-callee-sink-flow` | D4b | 5 |
| `lua/multi-target-dispatch-flow` | D4c | two CHA targets, sink in the one the map dropped |

`nightly/tests/c/funcptr.c` — the shape-1 guard — still passes.

One thing worth recording for whoever writes the next Lua case: **the callback
must be a closure declared inside a function**. A module-level `local h =
function(x) ... end` produces no call-target fact at all (`resolvent: 0`), so
the case silently tests nothing. Every Lua case above declares its closure inside
`main`.

Unit tests added:

- `index_engine` — a caller-supplied dispatch yields a `resolved_call` row
  carrying the resolvent's call string (and feeds a `context_assign` tagged with
  it); the in-frame bypass yields one with an empty call string.
- `facts::schema` — parquet round-trips for both tables, including an empty and a
  multi-frame `CallString` with frame order asserted explicitly.
- `query_engine::search` — `refine` over suffix-compatible, suffix-incompatible,
  prefix-not-suffix, and empty-on-either-side inputs; a contextual edge traversed
  under a compatible context and pruned under an incompatible one; an ordinary
  `Return` popping its site and pruning a mismatch (and still requiring `Free`);
  a `resolved_call` producing *both* an entry and a return edge; a multi-target
  site producing an entry edge into both targets (D4c); and the load-time
  subsumption dropping conditional rows dominated by an unconditional one.

`cargo test --release --workspace`: **792 passed, 0 failed** across 39 suites.

Nightly suite, run two ways:

- Locally (`cargo xtask regression`), which is what this worktree can drive:
  `--frontend lua` **28/28 pass**; `--frontend c` **26 pass, 2 xfail**
  (`C:ptrarith` and `C:defaultmodels`, both pre-existing and unrelated).
- The canonical flake check, `nix build .#checks.aarch64-darwin.regression`,
  which builds `jvm-reader` / `dex-reader` / Ghidra / the Java toolchain itself
  and runs **every** frontend: **158 passed, 0 skipped, 0 failed, 2 xfail of 160**
  — the two xfails being the same pre-existing `C:` pair (`C:ptrarith`,
  `C:defaultmodels`). That run covers the JVM, DEX, JNI and pcode cases this
  worktree cannot drive on its own, so "the existing nightly suite is green" is
  established across all of them, not just C and Lua.

  It was run twice. The first pass, before the new case files were staged, is the
  clean "did the engine change break anything" measurement: Nix reads only tracked
  paths, so it ran the **pre-existing** suite alone against the changed engine —
  147 passed, 0 failed, 2 xfail of 149. The second pass, with the eight new case
  files staged, adds their eleven registrations (each C case registers under both
  the `pcode` and the tree-sitter `c` frontend) and all eleven pass — **including
  the three pcode variants**, so the new C cases' `expected_lines` hold under
  Ghidra + `addr2line` as well as under source spans.

## Not verified here

- **APISIX, Emissary and Spring** — absent from this machine; Kong and baksmali
  stand in, as described above. The APISIX end-to-end acceptance criterion still
  belongs to `LUA_FRONTEND_FIX_PLAN.md` and is still gated on its D1/D2/D3.
- **The recall risk of the return-side context check** (§3, "Risks") was not
  observed on any corpus or case, so the mitigation — clearing the context on
  mismatch instead of pruning — was **not** implemented. The pruning behaviour is
  what ships.
