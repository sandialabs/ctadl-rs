# The hybrid-inlining plateau

**Resolved.** `ctadl index remote_control_smack` reaches a fixpoint in **26.5 s** (33 s wall
clock for the whole command), against a run that had not converged after 1200 s and was
projected to take hours. The fix is item 1 of *What is left to try* — modeling the ARM C++
exception unwinder — and it needed one new engine feature to work at all. See
*The fix: `modes: ["skip-analysis"]`* at the end. The app is back in the TaintBench suite.

Everything between here and that section describes the run **before** the fix, and is kept
because it is the measurement that identified the cause. Every relation size and rule time
below is a pre-fix number.

`ctadl index remote_control_smack` did not finish. It was excluded from the TaintBench
suite for that reason (`taintbench/apps/remote_control_smack/app.json`). This note records
what the run was actually doing, which explanations turned out to be wrong, and what is
left to try.

Everything below comes from seven capped runs at 1200 s on the same machine and the same
imports -- four timed, one sampled under a profiling build, two instrumented -- plus short
runs to check that added instrumentation left Ascent's join plans alone. None reached a
fixpoint; all stopped at the cap after 29 iterations, and all agree on every relation size
and counter.

## Reproducing the measurement

```
RUST_LOG=warn,ctadl=debug,ctadl_ascent=debug \
CTADL_INDEX_TIMEOUT_SECS=1200 \
  ./target/release/ctadl index remote_control_smack
```

`CTADL_INDEX_TIMEOUT_SECS` stops the semi-naive loop at the first iteration boundary past
the deadline and returns as if it had converged, so the per-SCC and per-rule times get
logged for a run that would otherwise never print them. The index block carries
`#![measure_rule_times]`, so `scc_times_summary()` reports every rule.

Run this command today and it converges in 33 s, because the unwinder is modelled. To
reproduce the plateau, add `--no-default-models` (which drops every native default, not only
the unwinder) or delete the `skip-analysis` generator from
`ctadl-ascent/src/models/defaults/native-index.jsonl`.

Adding `CTADL_DUMP_JOIN_DIR=<dir>` writes the join-density files described under *The join,
measured*. It costs 544 s of extra rule time, and **the relation sizes come out unchanged
anyway**: the cap stops at an iteration boundary, iteration cost grows steeply enough that
cumulative time crosses 1200 s during iteration 29 either way, so both runs do 29 iterations
and produce identical relations. Every counter below is therefore comparable across
instrumented and uninstrumented runs. Rule *times* are not -- take those from a run with the
dump off.

Measure memory with **physical footprint**, not RSS -- macOS compresses cold pages and
`ps -o rss=` undercounts badly. See the `measure-process-memory` skill.

## What a capped run destroys

Ascent's `run_timeout` returns from *inside* the SCC loop
(`__check_return_conditions!()` expands to `if ... {return false;}`,
`ascent_codegen.rs:203`). The generated code writes each relation's index store back to the
program struct only *after* that loop ends (`move_total_to_field`, `ascent_codegen.rs:517`),
so the early return skips it. Ascent `mem::take`s those stores out of the struct at the top
of the SCC, so on any capped run they are dropped.

Plain and lattice relations are unaffected -- their tuples live on the struct itself, which
is why `summary`, `context_assign`, `context_locals` and the rest survive. What is lost is
anything held in a BYODS store or an index:

| symptom | why |
| --- | --- |
| `assign.parquet` written with **0 rows** | `assign_like` lives in `assign_like_trie` |
| `reached_variables` = 0 (`0.0% of variables reached`) | read off `__locals_ind_common` |
| `locals store estimate` / `assign_like store estimate` all zeroes | same |
| `scc 10: iterations: 29, time: 0ns` | the per-SCC total is also accumulated after the loop |

None of this affects the numbers in this note -- `locals`'s row count comes from the
`CountingVec` stub, which counts inserts and lives on the struct -- but it does mean **the
saved index of a timed-out run has no `assign_like`**, and any future measurement that reads
a store must collect what it needs by a rule instead. See *Instrumentation*.

The fix in Ascent is to set a flag and `break` rather than `return`, then run
`move_total_to_field` and return the flag. Not attempted here; ascent is a crates.io
dependency.

## What the run looks like

Peak physical footprint is **1.6 GB**, and it is nearly flat after the first few minutes.
This is a plateau, not runaway growth -- the engine is busy, not leaking.

Facts going in:

| relation | rows |
| --- | ---: |
| `assign` | 2,589,090 |
| `actual_param` | 302,257 |
| `formal_param` | 83,263 |
| `callee_resolvents` | 67,650 |
| `callee_info` | 5,038 (2,511 distinct callers, max 46 per caller) |
| `call_target_assign` | 6,573 |

Relations at the cap, identical in every run:

| relation | rows |
| --- | ---: |
| `locals` | 14,439,746 |
| `summary` | 358,695 (over 22,064 functions) |
| `context_assign` | 396,961 |
| `context_locals` | 2,537,958 |
| `context_summary` | 75,642 |
| `call_target_assign_like` | 79,091 |
| `critical_summary` | 2,933 |
| `resolvent` | **220** |

Every SCC except one is trivial. SCC 10 holds all the work, 1321.9 s of it:

| rule family | join | `mod.rs` | time | share |
| --- | --- | ---: | ---: | ---: |
| `locals` forward-field propagation | `locals` ⋈ `assign_like` | 1148, 1154 | 495.9 s | 37.5% |
| `context_locals` forward-field propagation (3.3a) | `context_locals` ⋈ `assign_like` | 1294, 1300 | 464.9 s | 35.2% |
| `context_locals` seeded from `context_assign` (3.3b) | `context_assign` ⋈ `locals` | 1308, 1314 | 343.5 s | 26.0% |
| `assign_like` local-dispatch bypass (`summary` delta variant) | | 1331 | 15.9 s | 1.2% |
| everything else (30 rules) | | | 1.4 s | 0.1% |

Each of the first three families is two rules: one substituting into the variable-side
path, one into the formal-side path.

3.3a and 3.3b are distinct rules, not two descriptions of one thing. **3.3b is the entry
point**: it imports a context-free `locals` flow into the contextual world across a
`context_assign` edge, stamping it with the call string. **3.3a is the closure** over the
result, driving on the `context_locals` delta across ordinary `assign_like` edges, exactly
as the `locals` pair does for the context-free half.

**The context-sensitive half of the engine is 61% of the run.** It is a near-exact
duplicate of the `locals` closure carrying a call string.

## Rule numbering

| rule | head | body driver | `mod.rs` |
| --- | --- | --- | ---: |
| 3.1 Contextual Assignment (seed) | `context_assign` | `resolvent` x `summary` | 1258 |
| 3.2 Contextual assignment (chain) | `context_assign` | `context_summary`, pop | 1269 |
| — (bare, uncontextual twin of 3.2) | `assign_like` | `context_summary`, pop to empty | 1280 |
| 3.3a Contextual forward-field propagation | `context_locals` | `context_locals` x `assign_like` | 1294, 1300 |
| 3.3b Contextual local reachability | `context_locals` | `context_assign` x `locals` | 1308, 1314 |
| 3.4 Conditional summary | `context_summary` | `context_locals` x `formal_param` | 1323 |

Phases 1 and 2: 1.1 / 1.2 build `critical_summary`, 2.1 / 2.2 build `resolvent`.

## The funnel: what the hot rules actually do

All six hot rules share one body shape -- join two relations on `(func, var)`, substitute one
path prefix into the other, check the result is a real program path, derive -- so three
counters describe every one of them (`facts::counters::Funnel`):

* **joined** -- body pairs the join produced, before any guard.
* **built** -- `substitute_prefix` returned `Some`: the prefix matched *and* a derived path
  was allocated and interned. This is the expensive stage.
* **kept** -- survived `paths()`, i.e. actually derived a tuple. The old derivation counters.

| rule | joined | built | kept | fanout / driver row |
| --- | ---: | ---: | ---: | ---: |
| `locals` fwd dst | 13,313,699,165 | 1,372,068,115 (10.3%) | 90,182,617 (6.6%) | 922.0 |
| `locals` fwd fml | 13,313,699,165 | 83,057,553 (0.6%) | 32,823,638 (39.5%) | 922.0 |
| 3.3a ctx fwd dst | 11,793,671,992 | 1,203,123,153 (10.2%) | 43,825,593 (3.6%) | 4,646.9 |
| 3.3a ctx fwd fml | 11,793,671,992 | 69,113,651 (0.6%) | 26,712,701 (38.7%) | 4,646.9 |
| 3.3b ctx seed dst | 11,697,294,479 | 1,191,471,980 (10.2%) | 40,508,353 (3.4%) | 29,467.1 |
| 3.3b ctx seed fml | 11,697,294,479 | 67,407,720 (0.6%) | 26,095,619 (38.7%) | 29,467.1 |

Percentages are survival from the previous stage. Fanout divides `joined` by the population
the rule drives on: `locals` for the `locals` pair, `context_locals` for 3.3a,
`context_assign` for 3.3b.

Across all six: **73.6 G joined -> 4.0 G built -> 260 M kept -> 17.0 M final rows.**

### Four things this settles

**Almost all the derived paths are thrown away.** `substitute_prefix` allocates a
`Vec<PathSegment>` and interns one cons cell per component *before* `paths()` is consulted.
The engine builds 4.0 billion access paths and keeps 260 million -- 6.5%. That is the
mechanism behind the profile's "about two thirds of the run is one call": the call is being
made mostly for tuples that are about to be discarded. Rule 3.3b's dst half is the extreme:
1.19 billion built, 3.4% kept.

**Build-then-discard is not an unwinder artifact.** The prefix-survival rate is the same in
the context-free and context-sensitive halves -- 10.2-10.3% for the dst rules, 0.6% for the
fml rules. The `paths()` rate differs by about 2x (6.6% against 3.4%), but the context-free
`locals` pair still discards 93% of what it builds. This is what the engine does everywhere.
It is also why item 3 below helps `locals` and `context_locals` alike.

**Time follows work attempted, not work retained.** Normalising the three families' rule
times over the three (495.9 / 464.9 / 343.5 s from the table above) gives
**38.0% / 35.6% / 26.3%**; a dump-on run gives 38.1 / 35.6 / 26.3, so the dump does not
distort the split. Their shares of `built` are 36.5% / 31.9% / 31.6% and of `joined`
36.2% / 32.0% / 31.8%; their shares of `kept` are 47.3% / 27.1% / 25.6%. The attempted-work
shares are much the closer fit. The residual is 3.3b, about 20% cheaper per built path than
the other two, which is consistent with it probing the `locals` CSR trie rather than a hash
index.

**The `dst` and `fml` rule of each pair perform the same join twice.** Their body clauses are
identical -- only the substitution direction differs -- and their `joined` counts agree to
the digit. The pair is two passes over one join, and the second pass keeps 39% of what it
builds against the first pass's 3-7%, so nearly all the waste sits in the dst rule.

## The productivity gap, decomposed

`context_locals` produces rows an order of magnitude slower than `locals`. Temporary
counters (see *Instrumentation* below) turned that into an exact decomposition:

| | time | derivations | final rows | µs/deriv | derivs/row |
| --- | ---: | ---: | ---: | ---: | ---: |
| `locals` fwd | 495.9 s | 123,006,255 | 14,439,746 | 4.03 | **8.5** |
| `context_locals`, all rules | 808.4 s | 137,142,266 | 2,537,958 | 5.89 | **54.0** |
| &nbsp;&nbsp;↳ fwd propagation (3.3a) | 464.9 s | 70,538,294 | | 6.59 | |
| &nbsp;&nbsp;↳ seed from `context_assign` (3.3b) | 343.5 s | 66,603,972 | | 5.16 | |

`context_locals` performs **more total derivations than `locals`** -- 137.1 M against
123.0 M -- while producing 5.7x fewer rows. Three ratios describe it. They are *not*
independent, so do not multiply all three: redundancy is already the first ratio times the
row ratio (1.11 x 5.69 = 6.34), and multiplying all three double-counts the 1.11 and gives
10.3x against the measured 9.3x.

- **1.11x** more derivations
- **1.46x** more cost per derivation. Storage accounts for about 1.08x of that; the rest is
  the higher fanout of the driver population, measured under *The top rule, head to head*.
- **6.34x** more redundancy: 54 derivations per surviving row against 8.5

Two factorings of the one identity, either of which is correct on its own:

- by time: 1.11 x 1.46 = **1.63x the time** for 5.7x fewer rows, so 9.3x worse rows per
  second.
- by derivation: 1.46x per derivation x 6.34x redundancy = the same 9.3x per row.

The second is the more useful split, because a profile measures the first term and only the
first term. Redundancy dominates it either way.

Rule 3.3b fires 66.6 M times against 396,961 `context_assign` edges -- 167.8 *derivations*
per edge, three filters downstream of the 29,467 joined pairs per edge the funnel counts.
Almost none of it lands on a new tuple. Ascent calls `join_mut` only when a key already
exists, and it was called 136,228,717 times against 137.1 M derivations, so **98.2% of
derivations hit a key that was already there**.

### The top rule, head to head

Because churn is ~0 (below), every `context_locals` row enters the delta exactly once, so
the delta-driven instance's driver volume over the whole run *is* the final row count:

| instance 1 (driver = own delta) | time | driver tuples | µs per driver tuple |
| --- | ---: | ---: | ---: |
| `locals` (`mod.rs:1148`) | 297.6 s | 14,439,746 | **20.6** |
| `context_locals` 3.3a (`mod.rs:1294`) | 277.5 s | 2,537,958 | **109.3** |

Same rule shape, same `assign_like_indices_0_3` probe, same `(func, var)` key type, 5.3x
the cost per driver tuple.

The fanout half is the real answer, and the funnel now measures it instead of estimating it.
Cost per driver tuple is proportional to how many `assign_like` rows share that
`(func, v2)` key, which is `joined` over the driving relation's rows:

| | joined | driver rows | fanout |
| --- | ---: | ---: | ---: |
| `locals` fwd | 13,313,699,165 | 14,439,746 | **922.0** |
| 3.3a `context_locals` fwd | 11,793,671,992 | 2,537,958 | **4,646.9** |

**5.04x.** Both rules probe the same relation with the same key shape, so the ratio is
exactly "the `context_locals` population sits on 5x denser `assign_like` keys".

### Which functions carry it

Four functions hold **99.67%** of `context_locals`. Rank them by input `assign` count and you
will not find them:

| function | `context_locals` | input `assign` | `assign_like` on its probe keys | blowup | share of 3.3a's join |
| --- | ---: | ---: | ---: | ---: | ---: |
| `__gnu_Unwind_Resume` | 1,168,617 | **86** | 65,121 | 757x | 50.5% |
| `unwind_phase2` | 547,462 | **926** | 22,003 | 24x | 16.4% |
| `FUN_000bbaa4` | 406,774 | **105** | 20,063 | 191x | 16.4% |
| `FUN_00026cac` | 406,774 | **105** | 20,063 | 191x | 16.4% |

These are tiny functions by source. They are dense in `assign_like`, the derived closure --
summary instantiation is what puts the rows there, which is the same 50 k-row summaries that
drive everything else in this note. **Ranking functions by input `assign` count points at
entirely the wrong ones**, and any future triage should rank by `assign_like`.

The same six `call-arg(insn, 2)` vertices carry both rules: 3.3b seeds `context_locals` at
those call-argument vertices, and 3.3a then closes over them. One set of keys, two rules.

## Root cause: unwinder personality routines

Summary sizes are savagely skewed -- median **1** row per function, p99 **42**, max
**50,663**:

| function | summary rows | resolvable target? |
| --- | ---: | --- |
| `__gnu_unwind_frame` | 50,663 | no |
| `__aeabi_unwind_cpp_pr0` | 49,976 | **yes** |
| `__gnu_unwind_pr_common` | 49,976 | no |
| `__aeabi_unwind_cpp_pr2` | 49,976 | **yes** |
| `__aeabi_unwind_cpp_pr1` | 49,976 | **yes** |
| `__gnu_Unwind_Resume` | 19,040 | no |
| `unwind_phase2` | 18,844 | no |
| `__gnu_unwind_execute` | 7,041 | no |
| `_Unwind_VRS_Pop` | 313 | no |

Read that out of the saved index with

```sql
select f.name, count(*) n
from summary.parquet s join function_id.parquet f on s.func_id = f.id
where f.name ilike '%unwind%' group by 1 order by n desc;
```

-- `ilike` and `%` both matter: `_Unwind_*` and `__gnu_Unwind_Resume` carry a capital U, and
`like 'unwind'` without wildcards matches nothing at all.

These are the ARM C++ exception unwinder personality routines from the native libraries.
The unwinder shuffles registers and stack memory wholesale, so nearly everything is
reachable from nearly everything and each one summarises to ~50,000 rows.

Three of them are resolvable indirect-call targets, which is correct -- a personality
routine *is* reached through a function pointer in the exception table. Of the 161,467
summary rows held by resolvable targets, **149,928 (93%) belong to those three**.

The arithmetic closes. Rule 3.2 emits at most one row per `context_summary` tuple, so
≤ 75,642; rule 3.1 therefore accounts for ≥ 321,278 of the distinct `context_assign` keys,
and 321,278 / 49,976 ≈ **6.4**. Six or seven (call site, resolved personality routine)
pairs produce essentially the entire relation.

`context_assign` is not big because many things resolve. **220** resolvents resolve, and a
handful of them land on functions with 50,000-row summaries.

### The chain, end to end

```
3 unwinder personality routines, ~50 k summary rows each
  -> ~321 k context_assign edges from rule 3.1
     -> each scanning ~29 k locals rows in rule 3.3b   (48.6 k once the join completes)
        -> 11.7 G joined pairs, of which 1.26 G survive the prefix test
           -> 1.26 G access paths built, of which paths() keeps 66.6 M
              -> feeding 70.5 M forward derivations in 3.3a
                 -> collapsing to 2.54 M distinct rows
```

### The join, measured

`join-density/` holds four parquet files describing the two context-sensitive joins, written
by `CTADL_DUMP_JOIN_DIR`. What they show:

Rule 3.3b probes `locals` on `(func, v2)`, `context_assign`'s source variable. Over 396,961
edges and 6,732 distinct probes there are 287 distinct keys, of which only 180 have any
`locals` rows at all. Group sizes: median 885, **max 97,219**. Weighted by edges, the
completed join scans **48,618 `locals` rows per edge**.

Concentration is total. Of 396,961 edges, **396,528 (99.9%)** sit at call sites resolving to
unwinder personality routines, and those hold **100.0%** of the scan volume:

| caller | edges | max group | share of scan |
| --- | ---: | ---: | ---: |
| `__gnu_Unwind_Resume` | 99,132 | 97,219 | 49.9% |
| `unwind_phase2` | 33,044 | 96,588 | 16.5% |
| `FUN_00026cac` | 33,044 | 96,588 | 16.5% |
| `FUN_000bbaa4` | 33,044 | 96,588 | 16.5% |
| `__gnu_Unwind_RaiseException` | 99,132 | 885 | 0.5% |

`__gnu_Unwind_RaiseException` is the control: the same edge count as `__gnu_Unwind_Resume`, a
110x smaller group, and 100x less work. **Fanout, not edge count, is what costs.**

**Six `(function, call-arg)` keys hold 99.47%** of the 584,529 `locals` rows any edge probes
-- about 97,000 rows each, factoring as ~228 distinct `locals` paths x ~213 formal paths, with
deref chains five deep. The same six keys carry 3.3a.

Rule 3.2 is cheap in itself (324 µs) but closes the loop:
3.1 -> `context_assign` -> 3.3b -> `context_locals` -> 3.4 -> `context_summary` -> 3.2 ->
`context_assign`. Each turn pops one frame off the call string and re-instantiates in the
caller. `CallString::push` (`facts.rs:331`) rejects only a site whose *function* is
already in the string -- cycle detection, not a depth limit -- so with 22 k functions
nothing bounds the number of generations.

## Where the time actually goes

The per-rule timers say *which rule* is slow. The funnel says how much work each rule
attempts and discards. Neither says which machine instructions the time goes into. A sampled
profile answers that, and it is what ruled storage out as the explanation.

### Method

Build with the `profiling` profile -- `release` plus `debug = true` and `strip = "none"`,
so `sample` resolves frames to `file:line` with no separate dSYM step:

```
cargo build --profile profiling --bin ctadl
RUST_LOG=warn,ctadl=debug,ctadl_ascent=debug CTADL_INDEX_TIMEOUT_SECS=1200 \
  ./target/profiling/ctadl index remote_control_smack
sample <pid> 30 5 -f snapshot.txt      # 30 s window, 5 ms interval
```

**Use an explicit 5 ms interval.** At `sample`'s 1 ms default this process yields two
stacks in three seconds; at 5 ms it yields ~4,700 in thirty, at negligible cost. Eighteen
windows were taken between t=900 s and t=1376 s, all inside the fixpoint loop (it ran to
t=1400): four of 30 s at t=900/1020/1140/1260, and fourteen of 4 s every ~29 s. Pooled:
**27,553 self samples**.

The run reproduces the others -- every per-rule derivation count identical, `join_mut`
136,228,717 calls and 18 changes -- so the profile describes the same computation. Under the
profiling build scc 10 summed to 1383.2 s, of which `context_locals` took 856.3 s (**61.9%**)
and the `locals` forward pair 509.2 s (36.8%).

Each stack's nearest `IndexProg::run_timeout` frame carries the generated instance (its
offset into the function) and the clause being evaluated (its `mod.rs` line), so samples
attribute to a single rule instance. Attribution was total: 0% unattributed.

### The pooled answer

| work | share of sampled time |
| --- | ---: |
| `tailshare::Seq::intern` -- path hash-consing | 30.8% |
| `match_prefix` / `prepend_onto` -- path prefix algebra | 21.7% |
| iterator glue (closure dispatch inside the driver fold) | 16.9% |
| allocator / `Vec` | 16.1% |
| **generic hash index (table scan / probe)** | **10.9%** |
| TLS lookup for the interner registry | 3.0% |
| `locals` CSR trie | 0.004% |

By inclusive time -- the share of the run spent anywhere inside the function:

| function | inclusive |
| --- | ---: |
| `facts::prepend_onto` | **44.0%** |
| `tailshare::Seq::intern` | 34.2% |
| `facts::match_prefix` | 23.4% |

`match_prefix` then `prepend_onto` *are* `Path::substitute_prefix`, and they do not
overlap, so **about two thirds of the run is one call: building the derived access path.**
The callers confirm the shape -- 93.3% of allocator time and 78.6% of interning time is
reached through `prepend_onto`; a further 10.8% of interning comes from `match_prefix`
(its `map_head` offset-adjust re-interns a cons cell).

`prepend_onto` allocates a `Vec<PathSegment>` per call and interns one cons cell per
prefix component, each intern being a hash, a sharded `parking_lot::RwLock` acquire, and a
`hashbrown` lookup. The engine is serial (`ascent!`, not `ascent_par!`), so the sharding
and the locks buy nothing here.

### Storage is not the bottleneck

The decisive comparison is that the context-free `locals` forward rule -- which reads the
CSR trie and carries no lattice column, the cheap side of the 1.46x -- has **the same
profile**:

| | path prefix algebra | intern | iterator glue | allocator | index |
| --- | ---: | ---: | ---: | ---: | ---: |
| `locals` fwd, delta-driven (`mod.rs:1148`) | 36.2% | 35.2% | 16.0% | 9.5% | 0.6% trie |
| 3.3a `context_locals` fwd, delta-driven (`mod.rs:1294`) | 36.3% | 32.5% | 17.9% | 10.4% | 0.02% hash |

Same work, same proportions. Whatever makes `context_locals` cost more per derivation, it
is not that its rows live in a hash index instead of a trie.

Hash-index time is real, but it is confined to one instance of each pair. Ascent compiles
every two-clause rule into a `(Delta, Total+Delta)` and a `(Total, Delta)` variant. The
variant that iterates its own delta pays nothing for indexing; the variant that probes a
`Total` index pays 20-64%.

Seven of `context_locals`' eight instances were sampled. Their offsets into the generated
function rise in exactly the order the rule timers print, and the one gap falls exactly
where the unsampled instance belongs, which fixes the mapping:

| instance | rule | probes | rule time | hash index |
| --- | --- | --- | ---: | ---: |
| `+78680` | 3.3a dst | `assign_like` total, own delta drives | 307.3 s | 0.02% |
| `+79336` | 3.3a dst | `context_locals` total | 69.0 s | **32.2%** |
| `+80268` | 3.3a fml | `assign_like` total, own delta drives | 99.1 s | 0% |
| *(not sampled)* | 3.3a fml | `context_locals` total | 28.2 s | — |
| `+81872` | 3.3b dst | `locals` trie, `context_assign` delta drives | 165.1 s | 0% |
| `+82964` | 3.3b dst | `context_assign` total | 92.6 s | **20.4%** |
| `+83468` | 3.3b fml | `locals` trie, `context_assign` delta drives | 66.6 s | 0% |
| `+84560` | 3.3b fml | `context_assign` total | 28.6 s | **63.8%** |

The delta-driven instances hold **638.0 s of the 856.3 s and spend 0% in the index** -- a
different store cannot improve them at all. Applying each sampled share to its own rule
time (and giving the unsampled instance its twin's 32%) puts the whole index cost at
**~68 s, or 8% of `context_locals`**: a 1.08x factor, not 1.46x.

One caveat on reading the frames: `sample` shows the scan as
`hashbrown::raw::RawIterRange::fold_impl`, which names the table but not the relation whose
index it belongs to. The relation column above is read off the rule timers' index names,
not off the stacks.

## Rejected explanations

Recording these so they are not re-tried.

**Lattice churn.** The `SmallestCallString` lattice joins toward the shortest call string,
so an improvement re-enters the tuple into the delta and re-propagates the closure. That
was the leading hypothesis for three runs. Instrumenting `join_mut` killed it:
**136,228,717 calls, 18 changes.** One update per 7.6 M joins. The lattice essentially
never raises a value, nothing re-enters the delta, and there is no re-derivation.

**Swapping `SmallestCallString` for `Consistent`** (`lattice.rs:35`) to eliminate that
churn. Follows directly from the above -- there is no churn to eliminate. It would also
give up the guarantee the doc comment on `SmallestCallString` exists for: that the
`cs.is_empty()` feedback at `mod.rs:1280` -- the bare `assign_like` head beside rule 3.2 --
is never missed because a longer string was recorded first.

**Dead summary instantiation in rule 3.1, prunable by probing `actual_param`.** The idea
was that 3.1 instantiates the resolved callee's entire summary regardless of the site, and
3.3b discards the dead ones only after they have been materialised and rescanned. Probing
`actual_param(call_site, n2_sum, _)` in 3.1 pruned **41 rows** of 396,961 -- 0.01%.
Virtually every summary source 3.1 instantiates *is* bound at its call site. The probe is
sound (`locals` and `summary` were unchanged) but costs an extra index and makes 3.1
slower than it saves. Not kept.

**Clause reordering in the two forward families** (`locals` fwd and 3.3a). Both are
`X(infunc, v2, ...), assign_like(infunc, v1, p1, v2, p2)`, whose only shared variables are
`(infunc, v2)`. Ascent already generates the right indices and both delta directions;
there is no cheaper order. See *Ascent mechanics* for why the printed clause order is not
the execution order anyway.

**Removing the redundant call-string guards from the seed rules** (`f169102a`, which
stripped them from what are now 3.3b and 3.4; 3.1 lost its pair the same way afterwards).
Correct and clarifying -- the guards were implied by the non-empty invariant, and every
relation count came out bit-identical -- but the timing moved -1.5% on the family that
changed, against ±3% drift on families that did not. Noise.

## What is left to try

1. **Model the unwinder routines.** ***Done — see the next section.*** This is what propagation models are for: give
   `__aeabi_unwind_cpp_pr0/1/2`, `__gnu_unwind_frame`, `__gnu_unwind_pr_common`,
   `__gnu_Unwind_Resume`, `unwind_phase2` and `__gnu_unwind_execute` a hand-written summary
   instead of computing one. No shipped model file mentions `aeabi` or `unwind` today.
   Cutting `context_assign` multiplies straight through the chain above; dropping it to 3.2's
   ceiling of ~75 k is a 5.3x cut that scales the 343.5 s seed family down proportionally and
   takes a large share of the 464.9 s forward family with it. Order of 500-600 s off a 1322 s
   run -- a projection, not a measurement. It also generalises: every stripped ARM native
   library in TaintBench carries these symbols.

   The join-density files sharpen the target: **six `(function, call-arg)` keys** carry
   99.47% of what 3.3b scans and 99.67% of `context_locals`, all of them inside
   `__gnu_Unwind_Resume`, `unwind_phase2`, `FUN_00026cac` and `FUN_000bbaa4`. The last two
   are unnamed by Ghidra but sit in the same cluster.

2. **Cap call-string depth.** A `if cs.len() < K` guard in rules 2.2 and 3.2 -- the two
   places a call string grows or re-instantiates. The standard *k*-CFA knob, and the only
   thing that bounds the 3.3b -> 3.4 -> 3.2 generation count.

3. **Test `paths()` before building the path, not after.** The funnel's finding, and the
   largest single lever that is not about the unwinder: the engine builds **4.0 billion**
   derived access paths and keeps **260 million**. Every one of the 3.7 billion rejects costs
   a `Vec` allocation and one intern per prefix component, and the rejection is by a `paths()`
   membership test that runs afterwards.

   What is needed is a cheap way to ask "would `substitute_prefix(p23, p2, p1)` land in
   `paths`?" without materialising the answer. Two shapes worth trying: index `paths` by
   `(new_prefix, suffix)` so the test is a lookup on components that already exist, or intern
   into the `paths` table itself so a miss is discovered during interning rather than after
   it. Either way the win scales with `built - kept`, which is 93.5% of all path building, and
   it applies to `locals` fwd and the two context rules alike.

4. **Make `Path::substitute_prefix` cheaper.** The profile's own recommendation, and the
   other item that helps `locals` and `context_locals` alike -- it is ~two thirds of
   both. Three cuts, in descending size:
   - `prepend_onto` collects its prefix into a heap `Vec<PathSegment>` per call, and 93% of
     all allocator time is reached through it. The prefix is 0-2 components; a `SmallVec`
     removes the malloc/free pair outright.
   - Interning is 34% inclusive. Each cons cell costs a hash, a sharded `RwLock`, and a
     `hashbrown` lookup. Under serial `ascent!` the sharding and locking are pure
     overhead.
   - `get_interner::<T>()` runs a thread-local lookup per intern, which the profile sees as
     3.1% in `_tlv_get_addr`. A monomorphic `OnceLock` for `Node<PathSegment>` erases it.

   None of this reduces derivations, so it scales the whole run rather than fixing the
   plateau. It is the cheapest thing on this list to try.

5. **Cap summary size per function.** The blunt fallback if modelling proves fiddly. Any
   function summarising to more than a few thousand rows is almost certainly one of these
   degenerate cases.

6. **Evaluate each `dst`/`fml` pair as one join.** The two rules of a pair have identical
   body clauses and differ only in which side the prefix substitution runs on, and their
   `joined` counts confirm the engine performs that join twice. Deriving both heads from one
   pass would halve `joined` -- 73.6 G to 36.8 G -- without changing a single derived tuple.
   Whether Ascent can express that is the open part.

7. **Put `context_locals` on a `locals_trie`-style store.** *Not worth much.* The sampled
   profile puts the whole index cost at ~8% of `context_locals`, so the ceiling is ~68 s, and
   only at full parity. `c_locals_trie.rs` already exists (1,820 lines, unwired -- the
   relation is still declared `lattice context_locals`), so the work is mostly done. The
   delta-driven instances, which hold 638 s of the 856 s, spend **0%** in the index; a trie
   cannot improve them at all.

## The fix: `modes: ["skip-analysis"]`

Item 1 above, carried out. Two things had to change, and the first is the one that was not
obvious.

### A model ADDS to a body; it does not replace it

The premise of item 1 was "give the unwinder a hand-written summary instead of computing one".
The engine had no way to express *instead of*. `summary` is both an input relation -- the rows
`codegen::model_matches` pushes from a `propagation` -- **and** a derived one, computed from
`locals` by the two summary rules. Nothing about matching a function stopped the second, so a
model on a function CTADL can see left the ~50,000-row derived summary exactly where it was and
added its own rows on top. Writing the model without checking this would have made the run
*slower*, and the reason would have been invisible.

Measured before anything else was built, with a two-line flowy fixture: a function whose body
gives `return <- src`, plus a `propagation` saying `Argument(0) -> Return`. Both summaries came
out. The pair `ctadl-ascent/tests/tnt/model_adds_to_summary.tnt` and `skip_analysis.tnt` is that
experiment, kept as a regression test -- one asserts the union, the other asserts the
replacement.

### What was implemented

`modes: ["skip-analysis"]` was in the JSON schema and the docs, marked *not yet implemented*. It
now works, end to end:

| piece | file |
| --- | --- |
| `modes` parsed, validated, matched functions recorded | `models/json.rs` (`visit_modes`), `models/matches.rs` |
| names resolved to ids, `facts.skip_analysis` emitted | `codegen/model_matches.rs` |
| the guard | `index_engine/mod.rs`, the `locals`-from-formals seed rule |
| the unwinder generator | `models/defaults/native-index.jsonl` |

**The guard is one clause, and it is deliberately on the cheapest rule in the block.** Adding
`!skip_analysis(infunc)` to the rule that seeds `locals` from a function's formals leaves a
skipped function with no `locals` at all, and every body-derived relation drives on `locals`:
both summary rules, rule 1.1 for `critical_summary`, rule 3.3b for `context_locals`. Guarding
the summary rules instead would have been both redundant and wrong-headed -- redundant because
they drive on `locals`, and wrong-headed because **suppressing the summary rows is not the win**.
The resolvents the body manufactures are. A skipped function's own call sites are untouched: the
instantiation rule reads `summary(tgt, ..)` without ever touching `locals(tgt, ..)`, so callers
still compose with the hand-written behaviour.

`skip_analysis` is an input relation nothing derives, so its negation sits in an earlier stratum
and the fixpoint pays nothing for it.

The unwinder generator carries **no propagation at all** -- 32 names, `modes` and nothing else.
That is the accurate model rather than a convenient one: an unwinder moves register and stack
state to resume at a landing pad, not application data between a caller's arguments. Taint
arriving in a thrown object and leaving at a `catch` never crossed those frames as a data-flow
edge the IR could name.

### What it bought

Same machine, same imports, same app model. The before column is the 1200 s capped run this note
documents, which had **not** converged -- so its relation sizes are lower bounds, not totals.

| | before (capped at 1200 s, 29 iterations) | after (converged, 1957 iterations) |
| --- | ---: | ---: |
| scc 10 | 1321.9 s, no fixpoint | **26.5 s, fixpoint** |
| `summary` | 358,695 and climbing | **53,005** |
| `context_assign` | 396,961 | **497** |
| `context_locals` | 2,537,958 | **3,575** |
| `context_summary` | 75,642 | **228** |
| `resolvent` | 220 | 208 |
| `critical_summary` | 2,933 | 3,669 |
| `locals` | 14,439,746 | 40,763,914 |
| `assign_like` | (destroyed by the cap) | 3,286,730 |

`locals` is *larger* after, and that is not a regression: the before column stopped at iteration
29 of a fixpoint that never arrived. The right reading of the table is the context-sensitive
half. `context_assign` fell 800x, and the chain under *The chain, end to end* multiplies straight
through it.

Where the time goes now, by head relation, over the 23.1 s of summed rule time:

| head | time | share |
| --- | ---: | ---: |
| `locals` | 15.4 s | 66.5% |
| `context_assign` | 3.2 s | 13.8% |
| `critical_summary` | 2.7 s | 11.7% |
| `summary` | 1.1 s | 4.9% |
| everything else | 0.7 s | 3.1% |

**The context-sensitive half is 14% of the run, down from 61%.** Items 3, 4 and 6 above are
untouched by this fix and still apply -- they are about `substitute_prefix` and the join
structure, which is now `locals`' bill rather than `context_locals`'.

Peak memory for the converged run is 3.9 GB, read off `/usr/bin/time -l`'s maximum resident set
size -- **not** the physical-footprint gauge the rest of this note uses, so it is not directly
comparable to the 1.6 GB plateau. The converged run holds 2.8x the `locals` rows of the capped
one, so more memory is the cost of finishing rather than a regression.

### Correctness

`cargo xtask taintbench --apk remote_control_smack=<apk>` passes: 6 of the 17 ground-truth flows
connect and every finding's source and sink is recognized as an endpoint. (The app carries 17
positive and 0 negative findings, so it exercises no false-positive check.) That baseline is now
recorded in the app's `expected.json` and the `excluded` key is gone from its `app.json`, so the
app is back in the default suite.

The shipped default model file changed, so the whole suite was re-run rather than argued about:
**38 of 38 apps pass**, every app's recorded `matched_finding_ids` baseline unchanged
(`cargo xtask taintbench` with every APK named). The generator matches unwinder symbols only,
and nothing else in the benchmark moved.

The three flowy fixtures pin the semantics rather than the numbers:

- `model_adds_to_summary.tnt` -- a model without `modes` adds to the body-derived summary.
- `skip_analysis.tnt` -- the same model with `modes` replaces it, and flows follow the model.
- `skip_analysis_hybrid.tnt` -- a skipped body's indirect call no longer resolves, which is the
  half that actually mattered here. Drop the directive and both checks fail.

## Ascent mechanics worth knowing

`context_locals` has **eight** generated rule instances: four source rules -- the two 3.3a
forward-field rules at `mod.rs:1294` and `1300`, and the two 3.3b seed rules at `1308` and
`1314` -- x two semi-naive variants.

**The delta is not always the first clause, and it cannot be.** `versions_base`
(`ascent_mir.rs:339`) emits the standard decomposition for two dynamic clauses,
`(Delta, Total+Delta)` and `(Total, Delta)`. That covers every old/new combination exactly
once; delta-first in both would double-count `Δ ⋈ Δ`, and dropping the second would miss
`old ⋈ new`. So one instance per rule necessarily prints its delta second.

**Printed order is not execution order.** For a reorderable simple join,
`compile_mir_rule_inner` (`ascent_codegen.rs:876`) compiles *both* orders and emits a
runtime test:

```rust
if rel1.len_estimate() <= rel2.len_estimate() { /* drive 1, probe 2 */ }
else                                          { /* drive 2, probe 1 */ }
```

All eight instances print `[SIMPLE JOIN]` with no `[NOT REORDERABLE]` marker -- that marker
fires whenever `simple_join_start_index.is_some() && !reorderable` (`ascent_mir.rs:80`) --
so every one picks its driver by estimated size, per invocation.

That makes `len_estimate` load-bearing, and it is hand-tuned per view rather than a row
count. `locals_trie`'s `View01::len_estimate` (`locals_trie.rs:796`) returns the number of
`(F,V)` groups, which is the right notion for comparing against a default hash index
(also a key count). The `0_3_4` view (`locals_trie.rs:936`) deliberately returns a large
value so the planner never picks it as a driver. A wrong estimate here would silently flip
a join to the expensive side.

## Instrumentation

Two temporary pieces, both to be deleted when this investigation closes.

### `facts::counters` -- always on

`LAT_JOINS` / `LAT_CHANGES` in `SmallestCallString::join_mut`, and one `Funnel`
(`joined` / `built` / `kept`) per hot rule, bumped by three `let _c1..3 = counters::bump(..)`
clauses placed after the join, after `substitute_prefix`, and after `paths()`. They report
through one `log::debug!` beside `scc_times_summary()`.

None of the `let` clauses precedes the first two body clauses, so
`simple_join_start_index` stays at clause 0 and every instance still prints `[SIMPLE JOIN]`
with no `[NOT REORDERABLE]` -- verified by grepping the log after each change. Cost is not
measurable: 1321.9 s with counters against 1336.4 s without.

### `index_engine::join_dump` -- `CTADL_DUMP_JOIN_DIR`

Writes the four `join-density/` files. Because a capped run destroys every BYODS store (see
*What a capped run destroys*), the rows cannot be read out of `locals` or `assign_like` after
the fact; they are collected **by rules** into plain relations, which live on the program
struct and survive:

| relation | holds |
| --- | --- |
| `dump_join_on(bool)` | one row iff the env var is set; leads every collection rule |
| `dump_never(bool)` | seeded nowhere -- see below |
| `ctx_join_locals` | the `locals` rows 3.3b scans, restricted to keys `context_assign` probes |
| `ctx_join_assign_like` | the `assign_like` rows 3.3a scans, restricted to keys `context_locals` probes |
| `dump_site_callee` | rule 3.1's resolution prefix, which names the callee at an indirect site |

A relation nothing reads is a leaf, and Ascent gives it its own SCC ordered *after* the
hybrid-inlining SCC -- which a capped run never reaches, so it would come back empty. The two
`dump_never` rules at the bottom of the `ascent!` block name these relations in a body whose
head (`locals`, `assign_like`, `critical_call`) is already inside that SCC, which makes them
mutually reachable with it and so evaluated *in* it. `dump_never` is seeded nowhere, so
neither rule can derive a row.

The dump costs 544 s of rule time and leaves every relation size and counter unchanged,
because the cap stops at an iteration boundary and both runs do 29 iterations.
