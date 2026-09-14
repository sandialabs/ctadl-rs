# Reworking the index engine's joins: the plateau, measured and removed - DO-NOT-MERGE

Branch `rework-hybrid-inlining`. This note records what changed in the index engine, why, and
what it measured, so the next change can be compared against it. The plateau it is about is
the one `../ct-taintbench/hybrid-inlining-plateau.md` describes: `ctadl index` on
`remote_control_smack` with its native libraries, *without* the C++ unwinder model, ran for
20+ minutes at a flat 1.6 GB and never reached a fixpoint. With the unwinder modelled it took
27 s; the model is a band-aid over an engine that cannot stand dense functions.

## Where the ideas came from, and a caveat

`../hybrid-inlining-scratchpad` is an independent Datalog implementation of Hybrid Inlining.
Its convergence story is real but **it has never analyzed the program that plateaus here**: its
`-l apk` front end reads `classes*.dex` only, so on `remote_control_smack` it never saw the
ARM unwinder at all (`taintbench-profile.md:110-114` there says so). What transferred was its
*shape*, not a measurement: it never tests a derived access path after building it — `paths`
is an input, `cat(α, ρ, α·ρ)` makes an extension a lookup, and its congruence rule joins
`used_ext` on a whole path so "every tuple it retrieves is a match". That is exactly the waste
the plateau note had quantified in this engine: 73.6 G join pairs, 4.0 G paths built, 260 M
kept, 17 M rows.

Two more of its ideas are taken. Its two-phase split (`resolved::ResolvedAnalysis`): let the
context-sensitive fixpoint decide *which* callees a critical site has, then compute the data
flow over that call graph context-insensitively; here that is `--hybrid-context none`. And
its headline design, contexts *keyed by decision rather than by route* (`hi-alternate-design.md`):
that turned out to fix a real incompleteness in the call-string engine, found while validating
the first change on `smbd` (see §4 below).

## What changed

### 1. Exact-key congruence joins (semantics unchanged)

The forward-field propagation rules for `locals` — and the contextual twins 3.3a/3.3b for
`context_locals` — used to join `locals(f, v2, p23, ..)` with `assign_like(f, v1, p1, v2, p2)`
on `(f, v2)` alone, then `substitute_prefix` and test `paths`. Every pair on a dense
`(f, v2)` key was visited, a path was allocated and interned for a tenth of them, and a tenth
of those survived.

Now every path is split once, at every point it could match a prefix, and the join is keyed
on the split:

| relation | rows | holds |
| --- | --- | --- |
| `reach_vp(f, v, p)` | distinct paths of `locals` ∪ `context_locals` | so splits run once per path, not per row (rows outnumber paths 30:1 on rcs) |
| `locals_key(f, v, key, rest, p)` | one per split of a reached path | `p = key · rest` |
| `locals_key_wild`, `locals_wild` | splits before / paths ending in an offset | `match_prefix`'s offset arithmetic (`.x.[m]` matches `.x.[n]·rest` for any `m`) |
| `edge_split(f, v2, key, rest, (v1, p1))`, `edge_split_wild`, `assign_wild` | one per split of an edge's source path | the same, for edges |
| `ext_dst(f, v1, p13, v2, p23)` | one per (edge, reached path) that extends | the congruence-expanded edge: `v1.p13` gets whatever `v2.p23` has |
| `ext_fml` | the wild half of the formal-side direction | |

and the reachability step is `locals(f, v1, p13, a, p4) <-- ext_dst(f, v1, p13, v2, p23), locals(f, v2, p23, a, p4)`,
an exact join on `(f, v2, p23)`. The contextual closure (3.3a) walks the *same* `ext_dst` /
`edge_split` / `ext_fml`; 3.3b keys `context_assign` edges the same way (`ctx_edge_split`,
`ctx_ext_dst`, ...). Two Ascent details are load-bearing:

- **A literal in a clause makes Ascent index that column alone.** A first draft carried a
  `wild: bool` column and wrote `locals_key(f, v2, key, false, rest, p23)`; the planner then
  drove whole-relation scans off a two-key index. Hence two relations per split kind.
- **Nothing may sit between the first two clauses** of a rule that needs both semi-naive
  variants, so the `path_set(ps), if let Some(p) = ps.concat(..)` gate comes third.

The path test is [`PathSet::concat`](../ctadl-ascent/src/index_engine/path_set.rs): it hashes
the *virtual* concatenation (prefix, optional offset adjustment, rest, with the same
offset-merging normalization `Path::from_accesses` applies), probes, and confirms by comparing
components. A hit returns the set's own interned `Path`; a miss allocates nothing. `paths` is
now closed *before* the run (`compute_paths`, the same six rules in their own fixpoint) and
held twice: as the `paths` relation and as the `PathSet`, which also carries every admissible
path's splits precomputed.

### 2. Exact-path probes in the stores

Both BYODS stores answered an exact `(f, v, p)` probe by scanning the whole `(f, v)` group —
97 k leaves at the hot vertices — so an exact join would have paid the fanout anyway.
[`PathGroup`](../ctadl-ascent/src/index_engine/path_group.rs) replaces the group container in
all four stores (serial and parallel, `locals` and `assign_like`): a flat `HybridSet` while
the group holds at most 64 leaves, a map from path to leaves above that. The assign store
gained a `0_3_4` view and a `none` view; `locals_key` and `edge_split` live in the `locals`
store and `ext_dst` in the `assign_like` store, since they have the same column shapes. The
`assign_like` store also lost its O(group) linear dedup, which had been quadratic on the
18 k-leaf groups a summary instantiation creates.

### 3. `--hybrid-context {call-string,decision,none}`

> **Superseded.** `call-string` and its lattice are gone; `decision` is bounded now and the
> default, and a `collapse` mode was added. See [`decision-sets.md`](decision-sets.md). The
> rest of this section and §4 describe the state this note measured.

Three ways to instantiate the summary of a callee a resolvent decided, one program, one
`config` gate:

- `call-string` — **the default**, and what the engine has always done: the contextual rows
  carry the call string that brought the target down, one per row (the `SmallestCallString`
  lattice), popped back up a frame at a time. Bounded by the context-free relations. Its
  defect is §4.
- `decision` — §4's fix: rows are keyed by the decision (formal, path, target) and applied at
  every caller that established it. Complete, deterministic, and unbounded in the number of
  decisions reaching a function.
- `none` — the scratchpad's two-phase shape. Rule 3.1 instantiates the decided callee's
  summary as plain `assign_like` edges; rules 3.2-3.4 have nothing to do. Every caller shares
  the decision, which unions them at the site — precisely what `tests/tnt/hybrid_inlining.tnt`
  forbids (`bar1` passes `polyY`, `bar2` passes `polyZ`, through `mid` into `foo`), so it
  cannot be the default; it is the cheapest and it converges on everything measured.

The three share every rule but 2.1, 2.2, 3.1 and 3.2: the contextual relations carry a
`Context` key column (the constant `Route` under `call-string`, the `Decision` under
`decision`) beside the call-string lattice column (bottom under `decision`).

### 4. Contexts keyed by decision, not by call string

Validating change 1 on `smbd` (identical inputs, identical rules on paper) gave 8,398 fewer
summaries than the pristine engine — all flows into two globals, all in 27 functions, all
traceable to one root: `lp_load` passes a callback into `pm_process`, and so does
`FUN_0004f204`. The old engine kept **one** call string per `(function, formal, path, target)`
in the `SmallestCallString` lattice (`resolvent`, `context_*`), so `pm_process`'s conditional
summary could pop back to only one of the two callers. The pristine engine gave `lp_load` its
flows only because the row *transiently* held `lp_load`'s call string before the lattice
improved it; the new join order found `FUN_0004f204`'s first, and `lp_load` silently lost
them. The dumps confirm it: at the fixpoint neither engine holds any resolvent or context row
naming `lp_load`'s site. So the call-string result was order-dependent, and incomplete for
whichever caller lost.

The scratchpad's answer is to key the context by *what* was decided, never by the route:
`Decision { formal, path, target }` — "this function's `formal.path` holds `target`". Under
`--hybrid-context decision`:

- `resolvent(f, n, p, obj, ⊥)`: one row per decision, however many callers establish it.
  Rule 2.2 pushes decisions down without any call string and is finite by construction.
- `context_assign` / `context_locals` / `context_summary` are keyed by the `Decision` of the
  function they belong to; the lattice column stays bottom.
- Rule 3.2's pop is replaced by two *apply* rules that mirror the two push rules: at every
  call site `g → f` of a conditional summary of `f` under decision `(n, p, obj)`, if `g`
  holds `obj` at that argument (`call_target_assign_like`, 2.1's shape) the summary lands in
  `g` as plain edges; if the argument comes from `g`'s own formal `m.q` and `resolvent(g, m,
  q, obj)` exists (2.2's shape) it lands as a summary of `g` conditioned on `(m, q, obj)`, and
  the walk continues. Every route gets it, whichever was found first, and the result no
  longer depends on iteration order.

On `smbd` this restores every summary the lattice had lost and finds 135 more (0 rows only
in the pristine engine, 135 only here). **It is not the default because it is unbounded:**
the old lattice held `context_locals` to one row per `(f, v, p, a, p4)` whatever the number
of decisions, and `decision` keeps one per decision. On TaintBench's `xbot_android_samp`
(Rhino inside; 8,884 resolvents) `context_locals` passed 44 M rows in the first 30 s and the
run was killed at 125 GB, where the lattice engine converges with 8.2 M. The scratchpad
reports the same app as its one remaining non-convergence, for the same reason. What it
needs is its `demand` / `mixable` gates or a ⊤-collapse past a threshold (catalog ideas 2, 9,
7), none of which is ported.

## Measurements

Same machine (20 cores, 128 GB), same imports (`rcs6`, format 6), other jobs running, so wall
times carry ±10% noise; row counts do not. `base` is `99bbed37` built pristine.

### remote_control_smack with native libraries

| configuration | engine | result | scc / wall | peak footprint | `locals` |
| --- | --- | --- | ---: | ---: | ---: |
| default models (unwinder skipped) | base | fixpoint, 1957 iterations | 25.8 s / 32.5 s | 3.5 GB | 40,520,413 |
| default models | **new, `call-string` (default)** | fixpoint, 4269 iterations | 30.1 s / 36.7 s | 4.1 GB | 40,520,413 |
| default models | new, `decision` | fixpoint, 4269 iterations | 33.2 s / 42.7 s (loaded machine) | 4.0 GB | 40,520,692 |
| default models | new, `none` | fixpoint | 36.4 s / 46 s (loaded machine) | 4.0 GB | 40,522,809 |
| **unwinder unmodelled** | base | **no fixpoint; 29 iterations at the 600 s cap, 1364 s wall** | — | 1.9 GB (flat) | 14,439,761 partial |
| unwinder unmodelled | **new, `call-string` (default)** | **fixpoint**, 4269 iterations | 1274 s / 1304 s | 50.3 GB | 223,105,472 |
| unwinder unmodelled | new, `decision` | no fixpoint: 48 iterations at the 600 s cap (39 GB, `context_locals` 51.7 M); killed at the 90 GB guard after 1895 s | — | > 90 GB | — |
| unwinder unmodelled | new, `none` | **fixpoint**, 4269 iterations | 400 s / 422 s | 19.9 GB | 223,107,868 |

Under the default, every relation count of the modelled run is identical between `base`
and `new` (`summary` 52,957, `critical_summary` 3,669, `resolvent` 196, `context_assign`
497, `context_locals` 3,575, `context_summary` 228, `assign_like` 3,258,241): change 1 is
exact where the lattice's order-dependence (§4) does not bite. With `decision`, the modelled run gains 279 `locals` rows, 62 summaries and 32
`context_locals` rows — the routes the lattice used to drop.

In the unmodelled run the old engine's 1200 s were 61% `context_locals` closure and 37%
`locals` closure, both scan-and-substitute joins. In the new engine's 600 s-capped run the
`locals` closure took ~5 s in total, and time went where the rows are. What is left is the
size of the answer — 223 M `locals` rows, 6.3 M summaries — which is the analysis of an
unwinder, not the engine.

`none` against `call-string` on the unmodelled run: 3.2× faster and 2.5× smaller, because
`context_locals` reaches 124 M rows under call strings. On the modelled run the two differ by
261 `assign_like` rows and 255 summaries.

### TaintBench (38 apps, `cargo xtask taintbench` from `../ct-taintbench`)

- `new`, exact joins with call strings kept: 38 passed; every per-finding verdict
  byte-identical to `taintbench-run-8baae049.log`.
- `new, none`: 38 passed; every verdict identical. (A first sweep showed `roidsec` failing
  only because its `ctadl query` was killed by hand after 10 minutes; that query takes
  3-4 minutes with the base binary on its own index too, and is unrelated to this change.)
- `new, decision`: 37 passed with identical verdicts; `xbot_android_samp` killed by hand at
  125 GB (see §4).
- final binary, `call-string` default: 38 passed; every verdict identical.

`xbot_android_samp` on its own, the contextual heavyweight of the suite (8,884 resolvents,
`context_locals` at 84% of `locals`):

| engine | scc / wall / peak | `locals` | `context_locals` |
| --- | ---: | ---: | ---: |
| base | 72.1 s / 90.6 s / 6.6 GB | 9,681,842 | 8,172,571 |
| new, `call-string` | **54.0 s / 58.6 s / 5.4 GB** | 9,681,463 | 8,172,540 |
| new, `decision` | killed at 125 GB | — | 44 M at 30 s |

### Firmware (`../ct-firmware-eval`, `-l pcode`, imports in `/Volumes/Shampoo/hi-vs-ctadl-firmware/store`)

Guarded runs (`memguard.sh`), 1800 s cap where noted. `resolvent` is 0 on the first three,
so the context mode is moot there and the rows are identical.

| target | base scc / wall / peak | new (`decision`) scc / wall / peak | `locals` |
| --- | ---: | ---: | ---: |
| `ath_dfs` | 0.38 s / 3.6 s / 0.19 GB | 0.65 s / 1.4 s / 0.28 GB | 300,523 |
| `ath_dev` | 4.67 s / 8.4 s / 0.68 GB | 6.1 s / 9.8 s / 1.12 GB | 4,193,466 |
| `cfg80211` | 10.7 s / 17.3 s / 1.26 GB | 13.4 s / 19.3 s / 1.35 GB | 11,165,269 |
| `smbd` (39 resolvents) | 1402 s / 1424 s / 14.7 GB, fixpoint | **202 s / 226 s / 21.3 GB** (`call-string`); 210 s / 237 s / 22.2 GB (`decision`), fixpoint | 277,816,035 → 277,408,873 (`call-string`, §4) / 277,816,678 (`decision`) |
| `pluto` (227 resolvents) | no fixpoint: 26 iterations at the 1800 s cap, 10.4 GB, flat | no fixpoint: killed at the 60 GB guard after 1853 s, still growing (`none`: 80 GB after 2641 s) | — |

`smbd` is the plateau case among the firmware targets: recorded as killed at 3 h in
`BENCHMARKS.md`, it converges on `99bbed37` in 24 minutes and here in under 4. Under
`decision` its summaries are a strict superset of the old engine's (0 rows only in base, 135
only in new — the second callers that the lattice dropped); under the default they are 8,398
short, §4's loss with the order reversed. `pluto` is beyond both engines: the new one is not stuck,
it simply keeps deriving, and the answer does not fit in 80 GB.

(`BENCHMARKS.md` there still records `ath_dev`/`cfg80211` as killed at 90 GB on `main`; that
was an older `main`. On `99bbed37` they already converge in seconds.)

### The constant-factor cost

On inputs that already converged, the exact-key engine is 10-20% slower and holds 15-60% more
memory. The time is the split relations (`edge_split`, `locals_key`, `reach_vp`, `ext_dst`:
about 1.5 s of `ath_dev`'s 4.6 s of rule time) and the memory is their rows (roughly 40 B a
row in the stores, more for the plain ones). The closure rules themselves got slightly
faster. A derivation hop now takes up to four semi-naive iterations instead of one
(`locals` → `reach_vp` → `locals_key` → `ext_dst` → `locals`), which is why the iteration
counts roughly doubled; the per-iteration overhead turned out to be small.

## Reproducing

```sh
# the plateau configuration: default models minus the unwinder generator
grep -v '"aeabi_unwind_cpp_pr0"' ctadl-ascent/src/models/defaults/native-index.jsonl > /tmp/native-nounwind.jsonl
RUST_LOG=warn,ctadl=debug CTADL_INDEX_TIMEOUT_SECS=600 /usr/bin/time -l \
  ./target/release/ctadl index rcs_plateau rcs6 --no-default-models \
    -m ctadl-ascent/src/models/defaults/java-index.jsonl -m /tmp/native-nounwind.jsonl
# the same, context-insensitively
... --hybrid-context none
```

The debug log ends with `propagation relations: ...` (the sizes of the split relations) and
`index scc times` (per rule variant). Read memory off `peak memory footprint`, never RSS.

## Not done, and what to try next

> The first item below is done in [`decision-sets.md`](decision-sets.md); the rest stand.

- **Which mode should ship.** `call-string` is bounded and passes everything, and is what the
  engine always computed, so it stays the default; but §4 shows it is order-dependent and
  incomplete for the second caller of a decision, and the exact-key joins changed the order,
  so its results moved (`smbd` −8,398 summaries against the pristine engine, all `lp_load`'s
  and its callers'). `decision` is the correct semantics and needs a bound before it can be
  the default: the scratchpad's `demand`/`mixable` gates (catalog ideas 2 and 9) or a
  ⊤-collapse past a per-function decision threshold (idea 7). `none` is the fastest and
  converges everywhere, at the cost of the fixture's precision.
- **`stuck`/`live` rounds, root columns on `summary`** (catalog ideas 6, 4) are untouched.
  Neither addresses a plateau that no longer exists.
- **The split relations' memory.** `reach_vp` and the two wild relations are plain Ascent
  relations; the rest live in the BYODS stores. Deriving `locals_key` straight from `locals`
  would drop `reach_vp` and one iteration of latency per hop at the cost of three dedup
  probes per `locals` row.
- **`ctadl query` on `roidsec`** takes 3-4 minutes on an 11 k-edge index with either engine.
  That is the query engine's path search, and worth its own note.
