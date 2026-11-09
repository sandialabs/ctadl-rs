# Intent: staged index fixpoint

Run the index fixpoint as a sequence of stages, each of which reads its
inputs from disk, runs `index_rules` on a slice of the program, writes its
results to disk and frees everything, so that peak memory is the largest
stage's, not the program's. This assumes the blowup is spread across many
functions, not concentrated in one. Staging is the default for `ctadl index`
and `ctadl go`; the monolithic run stays behind a flag, as the reference the
tests compare against.

**Why it works.** No output contains `locals` or the relations built from it
(`reach_vp`, `locals_key(_wild)`, `locals_wild`, `assign_wild`,
`edge_split(_wild)`, `ext_dst`, `ext_fml`, `alias_flow`). They are per-function
scratch, and they are where the memory goes. With hybrid inlining gone, only
three things cross functions:

- `summary`, from a callee to its callers, through `call`. `call` is fixed
  before the run: `resolved_call` holds only intent pairings and feeds no
  rule.
- Global-to-global edges, from every function into `GLOBAL_FRAME`'s
  `assign_like`. Nothing outside the frame reads the frame's `locals`.
- `const_reaches` labels at global paths, up to the frame and back down to the
  `intent_frame` functions that read them.

**Stages.**

1. **Functions, bottom-up.** Condense `call` into SCCs and process them in
   reverse topological order, packing whole SCCs into batches under a memory
   budget, by an estimate from its seeds. A batch still running when the
   footprint passes the budget is stopped, split and rerun. A batch is seeded with its functions' facts and with its callees'
   `summary` rows, read from disk. It writes its own `summary` rows, its
   `assign_like` (including instantiated summaries) and its global-to-global
   edges. Every function is computed exactly once. An SCC too large for the
   budget is split and its parts iterated until its summaries stop growing.
   Batches whose callees are all done are independent and may run
   concurrently, within the budget.
2. **Global frame.** The union of all global-to-global edges, deduplicated
   and written as `global_flow`. It runs no fixpoint: `global_flow` is the
   frame's edges, not their closure (the query closes them), and the frame's
   `locals` feeds no output.
3. **Intent.** One stage over the `intent_frame` functions and the frame:
   `const_reaches`, `intent_pair`, `resolved_call`. Small, and cyclic through
   the frame, so it is not split.

**On disk.** Stage files hold rows by content, in the encoding of the index's
parquet tables, so any stage can start in a fresh process and a run can
resume after its last completed stage. Rows are keyed by function so a batch
reads only its callees'. Outputs are streamed per stage, never gathered in
memory; `IndexResult::try_save`'s tables are the union of the stage outputs.

**Unchanged.** The rules, each written once: functions run `index_rules`
under the serial or parallel engine. The intent rules (`const_reaches`, the
label exchange, `intent_pair`, `resolved_call`) are factored into their own
rule source, which `index_rules` includes, so the intent stage runs only them
and never computes `locals`. Fact generation and the query side.

**Tests.** On every integration and engine test, the staged run's outputs
equal the monolithic serial engine's as sets (`CTADL_DUMP_RELATIONS`,
`scripts/fact-dump-diff.sh`), with shuffled facts and any batch size down to a
configured floor, swept down to one function. On a large target, under
`memguard.sh`, the peak footprint tracks the largest batch rather than the
program, and a run killed between stages resumes to the same result.
