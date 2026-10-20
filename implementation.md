# Implementation plan - DO-NOT-MERGE

The next steps for the memory and performance problems in `things-to-improve.md`, in the order
proposed, with the options for each. Problem (P), solution (S) and measurement (M) numbers refer to
that file, which has the evidence behind every number quoted here.

## Done so far

| Change | Problems | Effect (M13) | State |
|---|---|---|---|
| S36: test the mode first in the context-free call rule | P31 | −28.7 s rule time on Element, −18.7 s on OsmAnd~ | Uncommitted |
| S27, first half: skip the wildcard rules without offset paths | P6 (Java) | −15.0 s on Element, −11.5 s on OsmAnd~ | Uncommitted |
| S12: unkeyed call-target join by default | P17 | −1.3 GB peak but +44 s on Element | Reverted; keyed stays the default |

Together: Element's scc 4 went from 301 to 246 s (−18%), with identical indexes on all three
apps. Peak memory did not change; none of these touch memory.

## How to verify a change

M13 settled on this protocol; every step below should follow it.

1. **A/B, back to back.** Index each benchmark app with the old and the new binary one after the
   other, alternating which goes first, each into its own store (a copy of the import), under the
   same cap (`.scratch/memguard.sh 32 LOG ...`, `MEMGUARD_SAMPLE_MS=2000`,
   `RUST_LOG='warn,ctadl=debug,ctadl_ir::ssa=info'`). `tmp/ab/ab.sh` does this. Copy the new binary
   aside first, so a rebuild can't change it mid-run.
2. **Read the saving off the rules that changed.** This machine's speed drifts by up to 30% between
   days and 10% between the two runs of a pair, so whole-run times are only indicative.
   `tmp/accuracy/ruled.py BEFORE AFTER` gives per-rule time and row deltas; rules that derive
   nothing and disappear are an exact saving.
3. **Check accuracy on the index, not on query results.** `tmp/accuracy/idxcmp.py A B` compares two
   index directories as multisets of rows per parquet file; a change that should preserve results
   must show "same" for every file. A results diff is not usable until P33 is fixed: two runs of
   the same binary already differ in 0.3-0.5% of results.
4. **Run the tests.** `cargo test --release -p ctadl-ascent` (about 45 minutes here), and the C and
   pcode regression families for anything that touches offset paths.

Benchmarks on this machine: WhatsApp (small, 1 min), OsmAnd~ (6-7 min) and Element (9-10 min),
imported Dex only under `tmp/blowup-*/state`. F-Droid is reachable, so the Java apps of M11
(Conversations, LibreTube, DAVx⁵) can be fetched. Ghidra 12.1.4 is installed (`GHIDRA_HOME`,
JDK 21), so an APK import now disassembles its native libraries too: pass `--no-native-libs` to
`ctadl import` for a Dex-only import comparable with M10-M13. The native corpus and
`cash.p.terminal` are on other machines, but Element's and OsmAnd~'s 65 MB of native code each
can stand in.

## Step 0. Commit the quick wins

Done in `8d02f0e7`.

## Step 1. Tooling that every later step needs

### S37: read the footprint on Linux (P32)

`phys_footprint_mb` (`ctadl-ascent/src/index_engine/mod.rs`) has macOS and Windows branches and
returns −1 elsewhere. Add a Linux branch.

- **Option A:** `VmRSS` from `/proc/self/status`. Matches what the cgroup cap sees, minus page cache.
- **Option B:** resident pages from `/proc/self/statm` times the page size. Cheaper to parse.

Either is a few lines. Without it the memory steps below can't be measured phase by phase, since
the cap's 2 s samples can't separate the end-of-loop transient (P30) from what scc 4 returns.

### S38: make the query independent of row order (P33)

The index writes the same rows in a different order each run (`assign`, `summary`, `paths`,
`resolved_call`, `index_source_map`), and the query's tainted-instruction results depend on that
order.

- **Option A: sort at save.** Sort each relation before `try_save`. It also makes index files
  comparable byte for byte, which would replace `idxcmp.py`. Costs a sort of up to 20 M rows
  (Element's `assign`) per index; `paths` and the interned names need a stable key, not their
  intern ids.
- **Option B: sort at load,** in the query engine (`ctadl-ascent/src/query_engine/`). Fixes the
  results but not the files.
- **Option C: find the order-dependent step** in the query and fix it. The real fix: the results
  should not depend on order at all, and options A and B only hide it. Start by diffing one
  location that differs between two baseline runs (WhatsApp, M13 lists several) and tracing which
  rows the query visits for it.

Recommendation: C first, time-boxed; A if C takes too long, since A also helps every comparison.

## Step 2. Memory constants (every app)

These lower the floor for every app, are mechanical, and come to about 4 GB on
`cash.p.terminal` (estimated). Measure each with S37 in place.

### S25: free `actual_param` after scc 1 (P19)

Only the call-arg rule ("Compute assignments from call sites" in `index_engine/mod.rs`) reads
`actual_param`, but Ascent holds it, with its `_indices_none` copy and `_0_1_2` index, through scc
4: 779 MB on Element and OsmAnd~, 1.56 GB on `cash.p.terminal`.

- **Option A: derive the call-arg edges in Rust** before `ascent_run`, straight into the
  `assign_like` seed, and drop `actual_param` from the program. Simplest and saves all of it.
- **Option B: drain the relation after scc 1** with a hook in the vendored Ascent macro. More
  general (other input relations could use it) but touches the macro.

Recommendation: A. Check that nothing else reads `actual_param` (the index saves it as
`actual_param.parquet`, so the save must take it from the facts, not from the program).

### S24: fold `reach_vp` into its consumers (P18)

`reach_vp` is a projection of `locals` and `context_locals` that drives `locals_key` (and the
wildcard keys) through its delta, and it is stored with a full `_indices_none` copy: 1.17 GB on
Element, 2.34 GB on `cash.p.terminal`.

- **Option A:** derive `locals_key` from the `locals` delta directly, deduplicating `(f, v, p)` in
  the rule. Loses `reach_vp`'s role of running the splits once per path rather than once per row
  (rows outnumber paths thirty to one on dense binaries), so measure on a native app too.
- **Option B:** keep `reach_vp` but hold it in a BYODS set (like `locals_key`), removing the
  `_indices_none` copy. Saves about a third to a half of it and keeps the per-path splitting.

Recommendation: B first; it is the safer half of the saving.

### S32: size input relations exactly (P26)

Input relations are `Vec`s grown by doubling, so they hold up to twice their rows: `actual_param`'s
row store and copy are exactly 256 MiB each on Element. Seed them with their final length (it is
known before `ascent_run`) or `shrink_to_fit` after seeding. With S25 done, the saving is in
`formal_param`, `call`, `alias_of_formal` and the other inputs.

### S33: reserve the BYODS outer maps (P30)

The peak sits 0.55-1 GB above what scc 4 returns, from hash tables doubling late in scc 4 (the old
and new table are live together). Reserve the outer `(f, v)` maps of the `edge_split` and `locals`
stores (`index_engine/locals_trie.rs`, `assign_like_trie.rs`) before the loop: their key counts are
bounded by the distinct `(f, v2)` of `assign_like` and by the variables. Leaves the last,
unidentified 100 MiB growth; S37 plus `CTADL_BIG_ALLOC_MB` can name it.

## Step 3. The scanning joins (P21, the rest of S27)

The largest remaining time on large Java apps: on Element, `context_assign` 3.1 (32 s),
`critical_summary` 1.2 (21 s) and `establishes_direct` (11 s), at 100-600 µs per row against 1 µs
for ordinary rules. Each is driven from a whole relation (`call_indices_2_total`,
`callee_info_indices_0_2_3_total`) and probes the small delta last, so every iteration rescans it.
The same join costs 0.03-0.06 s when run once (`resolved_call`).

- **Option A: reorder each body** so the relation whose delta is small comes first. Ascent's
  planner only swaps the first two clauses, so the order written is the order run. Each rule has
  several semi-naive variants; the one driven by the delta should lead with it.
- **Option B: add keyed helper relations,** like `cta_key` did for P16, so the join retrieves only
  matching rows. More memory; use only where reordering is not enough.
- **Option C: write the delta-driven variant explicitly** where Ascent's generated order can't be
  fixed by reordering alone.

Expect up to about 60 s of rule time on Element. Verify with the row-set comparison; these rules
feed decisions, so check `context_assign` and `resolved_call` row counts too.

## Step 4. Call-target tags (P17)

20-31% of relation memory on Java apps (2.34 GB on Element). S12 showed the unkeyed join is too
slow on large apps, so:

- **Option A (S13): hold `call_target_assign_like` and `cta_key` in a BYODS trie**, as `locals`
  and `assign_like` are. Those take 37-67 B per row; these take 192-512 B. The largest saving, and
  it keeps the keyed join's speed.
- **Option B (S12, refined): key only the high-fan-out vertices.** Needs a fan-out measure at seed
  time; more complex, smaller saving.
- **Option C (S18, first step): drop tags at primitive slots** (`ByVal` formals, primitive fields
  and array elements), with facts that already exist. Shrinks tags and decisions; also a precision
  fix.

Recommendation: A, then C.

## Step 5. The globals slot (P28)

The one Java blowup found so far (Conversations: a late 5.2 M-row cascade). Two options, not yet
told apart:

- **Option A:** pass the globals slot only to callees whose summary or model reads or writes
  globals.
- **Option B:** version the global heap per static-field write, so a call sees only the fields
  written before it (S3's idea applied to globals).

Start by checking the two open questions in P28: why every call-arg sees all 699 fields, and
whether the callees read globals at all. Benchmark: Conversations (fetch from F-Droid), and
OpenKeychain.

## Step 6. Investigations to run alongside

- **S26 (P20): memory held around the fixpoint.** 7-8 GB on `cash.p.terminal`; on Element 3.9 GB
  before the loop and 108 s after it (51 s before saving starts). Needs S37.
- **S14 (P29): bound decision sets on LibreTube.** `--hybrid-context bounded:k` and `spill:k`
  already exist; try `k` below 47 and check the regression suite. No code needed.
- **S9 (P5): find which native indirect call sites mint decisions** (`AnarchRE`,
  `heartratemonitor`), before any fix.

## Step 7. Native-code blowups

The largest wins and the most research. Ghidra is installed here; the native corpus of M7 is not,
but whole-APK imports of Element and OsmAnd~ (with their native libraries) give native benchmarks.

- **P1, the stack mixing pool: S4,** promote escaped stack slots and spill them around their
  escapes. The `libgdx` apps peak at 20-23 GB; `androidudpbus` is still 5x over budget.
- **P4, cross-product summaries: measure S8 first** (do the wasted pairs fail `concat`?), then S8
  or S7 (collapsed summary rows). A model-based skip like S6 for BoringSSL is the stopgap.
- **P6 on native code:** the wildcard relations themselves (11.4 of 13.3 GB on
  `conscryptprovider`). No design proposed yet.
- **P3:** interpreter-loop phis (S5); investigate first.

## Decisions needed

- S38: fix the order dependence at its source (option C) or sort at save (option A).
- Whether step 7 needs the original native corpus, or whether whole-APK imports of the apps here
  are enough to start.
