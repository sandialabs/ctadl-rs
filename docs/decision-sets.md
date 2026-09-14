# Contexts as decision sets: the call-string lattice removed - DO-NOT-MERGE

Branch `rework-hybrid-inlining`, following `hybrid-inlining-rework.md`. That note left the
engine with three context modes and a defect in the default: the `SmallestCallString` lattice
kept one route per contextual row, so the second caller of a decision silently lost the
callee's conditional flows, and which caller lost depended on iteration order (its §4). The
complete alternative, `--hybrid-context decision`, keyed every contextual row by the decision
and did not fit in memory on an interpreter-shaped program (`xbot_android_samp`: 44 M
`context_locals` rows in 30 s, killed at 125 GB). This note records what replaced both, why,
and what it measures. There is no call string anywhere in the engine any more, and nothing is
bounded by a `k`.

## The idea

The scratchpad's key (`../hybrid-inlining-scratchpad/hi-alternate-design.md` §2) is right:
a contextual row of `f` should be keyed by *what was decided* -- `Decision { formal, path,
target }`, "formal `n.p` holds target `t`" -- and never by the route that established it. Two
routes that establish the same decision share one closure and both get the result. What made
it unbounded here was keying the *rows* by the decision: a function's contextual closure was
repeated once per decision reaching it, and `Interpreter.interpretLoop` is reached by 1,276.

The rows of those 1,276 closures are almost all the same rows. So the key moved from the row
to a lattice column: `context_locals(f, v, p, a, p4) -> DecisionSet`, one row per
`(f, v, p, a, p4)`, whose value is the set of decisions the row holds under. A seed edge (rule
3.1) holds under the one decision that resolved its site; a row derived from it inherits the
set; where flows of two decisions meet, the sets union. `context_assign` and `context_summary`
carry the same column. The row count is bounded exactly as the call-string lattice bounded
it -- by the context-free `locals` of a run that instantiated every decision unconditionally
-- and no decision is dropped: the result is the same fixpoint `decision` computed, checked
row for row on `remote_control_smack` and `smbd` (§Measurements).

Sets are interned ([`decision.rs`](../ctadl-ascent/src/index_engine/decision.rs)): a
`DecisionSet` is a `Copy` pointer to a sorted slice of dense decision ids, equal sets are
pointer-equal, and rows that hold the same set share one allocation. On xbot the final
`context_locals` holds 9.1 M rows over **292 distinct sets** standing for 6.0 *billion*
row-decision memberships; hash-consing is what makes the column cost nothing. A per-thread
memo of unions by address pair keeps the merge walk to once per distinct pair.

Two more things changed with it:

- **`resolvent` is a plain relation** `(f, n, p, target, id)`, one row per decision. What the
  old lattice column recorded about the route is reduced to the one hop rule 3.2 needs:
  `establishes_direct(f, d, caller, insn)` (the caller holds the target itself, 2.1's shape)
  and `establishes_via(f, d, caller, insn, up)` (the caller passes its own formal, on which it
  holds decision `up`, 2.2's shape). Rule 3.2 joins a conditional summary row with these by
  callee and tests the decision by membership. The first draft unfolded `context_summary` per
  decision instead -- 8.3 M rows on xbot -- and the apply rules then scanned that relation
  whenever a caller-side fact was new: 70 iterations in 900 s.
- **`--hybrid-context call-string` is gone**, with `CallString`, `SmallestCallString` and the
  pop rules. The modes are `decision` (the default), `collapse` (below) and `none`.

## Where the time goes, and the abstraction that removes it

With the row count bounded, xbot converges: 423 iterations, 368 s, 6.2 GB. The call-string
lattice took 51 s on the same input, because its rows never change once derived, and here a
row's set changes every time a decision reaches the row after it was derived -- and every
change re-propagates to every row downstream. The counters say how often: 128 M set changes
over 9.1 M rows, 5.9 G union attempts, about fourteen full re-closures of `interpretLoop`.
(The union attempts are the hot path: a thread-local hash-map memo of them cost more than the
joins around it, and a per-firing interning of the singleton `{up}` in rule 3.2 another 35 %;
the memo is a lock-free direct-mapped cache now and the singletons are a table lookup.)

Two things were tried against that and rejected, both kept out of the tree:

- **Hold the seeds back until the context-free fixpoint**, then close (two calls of
  `run_timeout` with a gate relation between them). Worse, 545 s against 435 s for the same
  binary in one run: Ascent re-fires every rule
  over every relation at the start of a resumed run, and the waves were not from decisions the
  context-free half finds -- they come from decisions that each turn of the feedback loop
  discovers (conditional summary applied at a caller, new target flow, new resolvent), 669
  iterations of it.
- **Key by what was inlined rather than which decision inlined it** (a `(site, callee)` key
  for 3.1's seeds, so a new decision that resolves a site to an already-seen callee changes no
  row). Instrumented before building: `interpretLoop` has **3** decided sites of its own and
  **44** sites at which it inherits callees' conditional summaries, and the inherited edges are
  keyed by `interpretLoop`'s own decisions, which is where the waves are. Keying inherited
  edges by anything route-shaped is the call string again (a key that nests through recursion
  and multiplies through fan-out), so this does not reach the hub.

Exact provenance in an incremental fixpoint therefore costs one re-closure per wave of
decisions, and no keying avoids it. What avoids it is a coarser lattice -- coarse, but not a `k`. `--hybrid-context collapse` uses the flat lattice
`⊥ < {d} < ⊤`: a row holds under one decision, or -- the moment a second decision reaches it
-- under ⊤, "every decision of this function". A ⊤ summary row is applied at every caller that
establishes any decision of the function. A row changes at most once after it is created; it
is still a lattice, so the fixpoint is unique and order-independent; there is no parameter.
It is coarser than `decision` exactly where two decisions' flows meet at a row and a third
decision, which does not share the flow, also reaches the function: the third decision's
caller gets the flow. On the fixture `tests/tnt/hybrid_inlining.tnt` the two decisions never
meet, so it passes. On xbot it turns 368 s into 74 s, at 154 more resolvents, 3,229 more
`locals` rows and 195 more summaries (of 9.7 M and 201 k). `decision` stays the default
because it is the complete answer and costs the same as the old default everywhere but the
hub; `collapse` is one flag away (`CTADL_HYBRID_CONTEXT=collapse` for a harness that does not
pass flags).

## Measurements

Same machine as the rework note (20 cores, 128 GB), same imports. `base` is the committed
branch head `1d9eee34` under its default (`call-string`); `decision` and `collapse` are this
tree. Row counts are exact; wall times in the first table were taken while other jobs ran and
carry ±15 % and are from intermediate builds of this tree; the second table is the final
binary, back-to-back on an otherwise idle machine.

### Convergence and rows

| input | mode | result | scc / wall | peak | `locals` | `summary` | `context_locals` |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| xbot | base | fixpoint, 423 it. | 44 s / 46 s | 5.4 GB | 9,681,463 | 201,074 | 8,172,540 |
| xbot | old `decision` (rework note) | killed at 125 GB | — | > 125 GB | — | — | 44 M at 30 s |
| xbot | **decision** | fixpoint, 423 it. | 363 s / 368 s | 6.2 GB | 9,692,972 | 201,552 | 9,147,332 (292 sets; 6.0 G memberships) |
| xbot | collapse | fixpoint, 423 it. | 72 s / 74 s | 6.7 GB | 9,696,201 | 201,747 | 9,158,434 |
| rcs, default models | base | fixpoint, 4269 it. | 28.3 s / 34.5 s | 4.1 GB | 40,520,413 | 52,957 | 3,575 |
| rcs, default models | **decision** | fixpoint, 4269 it. | 28.4 s / 34.5 s | 4.0 GB | 40,520,692 | 53,019 | 3,578 |
| rcs, unwinder unmodelled | base (rework note) | fixpoint | 1274 s / 1304 s | 50.3 GB | 223,105,472 | 6.3 M | 124 M |
| rcs, unwinder unmodelled | old `decision` (rework note) | no fixpoint, killed at 90 GB | — | > 90 GB | — | — | 51.7 M at 600 s |
| rcs, unwinder unmodelled | **decision** | fixpoint, 4269 it. | 1078 s / 1104 s | 51.5 GB | 223,105,751 | 6,291,106 | 124,019,268 (58 sets; 372 M memberships) |
| smbd | base (rework note) | fixpoint | 202 s / 226 s | 21.3 GB | 277,408,873 | 8,398 short (§4 there) | — |
| smbd | **decision** | fixpoint, 1674 it. | 172 s / 194 s | 21.2 GB | 277,816,678 | 4,397,382 | 75,032 |
| smbd | collapse | fixpoint, 1674 it. | 245 s / 267 s | 21.2 GB | 277,836,838 | 4,397,954 | 75,032 (1,442 ⊤) |

The `decision` rows are the complete answer the rework note's `decision` mode computed where
it converged (`locals` 40,520,692 and 277,816,678 there too), now within the old default's
memory and time everywhere but xbot. Nothing plateaus: the unmodelled rcs run, the case the
old engine ran for 20 minutes at a flat 1.6 GB, converges in the same 4,269 iterations as the
modelled one, and its `context_locals` holds 124 M rows where per-decision keying would have
held 372 M.

### Back-to-back on an idle machine

| input | base scc / wall / peak | `decision` scc / wall / peak | `collapse` scc / wall / peak |
| --- | ---: | ---: | ---: |
| xbot | 48.9 s / 51.0 s / 5.41 GB | 363 s / 368 s / 6.19 GB | 72.2 s / 74.4 s / 6.66 GB |
| rcs, default models | 27.1 s / 33.4 s / 4.07 GB | 27.9 s / 33.9 s / 4.07 GB | 27.7 s / 33.7 s / 4.10 GB |
| smbd | 202 s / 226 s / 21.3 GB (rework note) | 172 s / 194 s / 21.2 GB | 245 s / 267 s / 21.2 GB (loaded machine) |
| `ath_dev` (0 resolvents) | 5.2 s / 8.3 s / 1.08 GB | 5.1 s / 8.1 s / 1.12 GB | — |
| `cfg80211` (0 resolvents) | 10.4 s / 15.6 s / 1.24 GB | 10.2 s / 15.4 s / 1.35 GB | — |
| `pluto`, 600 s cap, 60 GB guard | no fixpoint (rework note: 60 GB at 31 min) | no fixpoint: 46 iterations, 43.8 GB, 253 M `locals` and growing | — |

Where no decision reaches more than a few rows the engine is the old engine: the set column
costs nothing measurable. `collapse` and `decision` agree to the row on rcs and differ on
smbd by 20,160 `locals` rows and 572 summaries (1,442 ⊤ rows, all in `tdb_traverse`).

### Suites

- `cargo test --workspace`: all binaries pass (the fixture `tests/tnt/hybrid_inlining.tnt`
  among them; `decision.rs` has unit tests for both lattices).
- `cargo xtask regression --release` for `c,lua,pcode` (66 passed, 2 xfail), `jvm,dex` (77
  passed), `jni` (14 passed).
- TaintBench, 38 apps (`cargo xtask taintbench` in `../ct-taintbench`, the binary on `PATH`):
  38 passed under `decision` and 38 under `collapse`, and every per-finding verdict of both
  sweeps is identical to `taintbench-run-8baae049.log`, the reference the rework note also
  compared against.

## Reading a run

At `RUST_LOG=warn,ctadl=debug` the index log ends with `context_locals by function`: total
rows, the memberships a per-decision keying would have held as rows, the distinct sets, and
the top functions by rows with their decision count and (under `collapse`) their ⊤ rows;
followed by the interner's counters (`sets ever interned` against distinct sets at the
fixpoint is the number of times sets grew, i.e. the waves). `RUST_LOG=warn,ctadl=trace` dumps
the contextual relations with their sets.

## Not done

- **The waves.** `decision` on a hub is bounded but slow (xbot 435 s against 46 s). The
  exact fix would be an engine that does not re-derive downstream rows when a set grows --
  cycle collapsing over the derivation graph, which Ascent cannot express. `collapse` is the
  abstraction that sidesteps it; a finer one (`⊤` per formal path rather than per function,
  say) would be a lattice with a few more levels and the same wave bound.
- **`none` and `collapse` still union what the fixture forbids** in their respective ways;
  `collapse` only where flows meet. Neither is the default.
- `pluto` (firmware) was run capped only; see the batch log. It is the unwinder story of the
  rework note's §4, not a context question: it has 227 resolvents and a handful of functions
  that summarize to tens of thousands of rows.
