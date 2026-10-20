# Investigating an index memory blowup

This branch (`index-instrument`) is never merged. It is main plus instrumentation for finding out
why `ctadl index` uses too much memory on some program: which relations grow, which rules
spend the time, which indices cost the bytes, and which program entities the rows come from.

## What this branch adds

- `vendor/ascent_macro`: a patched Ascent macro (wired in through `[patch.crates-io]` in
  `Cargo.toml`; every change is marked `// ctadl patch:`). Its `run_timeout` keeps the stopped
  SCC's stores and the time spent in it, so a run stopped by `CTADL_INDEX_TIMEOUT_SECS` still
  reports per-rule profile information (stock Ascent loses both). It also generates
  `index_sizes_summary()` and `rule_counts_summary()` on every program and prints the
  `CTADL_ASCENT_TRACE` lines.
- `ctadl-ascent/src/index_engine/instrument.rs`: everything ctadl-side, reached from `mod.rs`
  by one `instrument::after_run!` call after the run.

## Instrumentation

All logging is at debug level: run with `RUST_LOG=debug` (or narrower,
`RUST_LOG=ctadl_ascent::index_engine=debug`) and keep stderr.

| Name | What it gives | Where |
|---|---|---|
| `index scc times:` | Per-SCC and per-rule time (`scc_times_summary()`) | main |
| `[relsizes]` | Every relation's tuple count, plus the store-backed ones (`assign_like`, `locals`, `edge_split`, `locals_key`, `ext_dst`) | main |
| `profile.json` | The same relation sizes and SCC/rule times as JSON, written next to the index on every run (no logging needed); `reached_fixpoint` is false after a timeout | main |
| `[indexsizes]` | `index_sizes_summary()`: every physical field of every relation (its rows and each index, e.g. `reach_indices_0_2`) with keys, entries and estimated heap bytes | this branch |
| `index rule counts:` | `rule_counts_summary()`: per rule, head tuples produced and tuples new, their ratio, and the rule's time | this branch |
| `CTADL_ASCENT_TRACE=1` | One `[ascent-iter]` line per SCC iteration on stderr, with each relation's delta and total | this branch |
| `CTADL_INDEX_TIMEOUT_SECS=<n>` | Stops the fixpoint at the first iteration boundary past `n` s, then logs everything above as of that point | main + patched macro |
| `CTADL_INDEX_DUMP=<dir>` | The heavy relations as TSV (below) | this branch |

### Reading the branch's own output

- `[indexsizes]` prices the serial engine's storage the way the allocator sees it (capacity,
  hash-table slack, 16-byte malloc quantum); a `total:` line ends it. It cannot price the
  parallel engine's indices or the trie stores (`__locals_ind_common`,
  `__assign_like_ind_common`) and says `bytes: ?` for those, so size indices with the serial
  engine. It does not count heap owned by column values (a `String` in a tuple).
- `index rule counts:` has the same SCC and rule order and rule text as `index scc times:`.
  `dup_ratio` (produced / new) is redundant work: a rule at 50 spends most of its time
  rediscovering tuples. A rule with several heads counts them together. For a lattice, `new`
  counts every join that changed a value, so it can exceed the lattice's size.
- `[ascent-iter] prog=<program> scc=<i> iter=<n> elapsed=<s> rel:delta=<n>,total=<n> ...`, one
  line per iteration, `done` for a non-recursive SCC. `total` is the size at the start of the
  iteration and `delta` what it added. `prog=` matters: an index runs more than one Ascent
  program, and their SCC numbers overlap. The line for the iteration a timeout stops is printed
  too. Every SCC relation is on the line, so it is long; pull one relation out with
  `grep -o 'locals:delta=[0-9]*,total=[0-9]*'`.

### `CTADL_INDEX_DUMP=<dir>`

Writes one TSV per relation into `<dir>` (created if missing), each with a header row, after
the run (so after a timeout too, which makes it the partial state). It works under all three
engines: serial in-memory, parallel (`-jN`), and LSM storage (`--lsm`).

| File | Columns |
|---|---|
| `functions.tsv` | `f name`: function id to name. Join the `f`/`tgt` columns of the other files on this. |
| `locals.tsv` | `f v p formal fp` |
| `assign_like.tsv` | `f vd pd vs ps` (destination var/path, source var/path) |
| `call_target_assign_like.tsv` | `f v p target` |
| `call_target_assign.tsv` | `f v p target` |
| `summary.tsv` | `f n1 p1 n2 p2` |
| `call.tsv` | `f insn tgt` |
| `callee_info.tsv` | `f insn v p key` |
| `resolved_call.tsv` | `f insn tgt` |
| `context_locals.tsv` | `f v p formal fp nset` (`nset`: decisions in the row's context, -1 for top) |

Variables and paths are written in their display form; an empty path column is the empty
path (the variable itself). To attribute rows, count by function:

```sh
cut -f1 dump/locals.tsv | tail -n +2 | sort | uniq -c | sort -rn | head -20 \
  | while read n f; do printf '%s\t%s\n' "$n" "$(awk -F'\t' -v f="$f" '$1==f{print $2}' dump/functions.tsv)"; done
```

The dump holds every row in text: a 50k-function APK (`com.noto_54.apk`) writes 8M rows,
581 MB. Use it on runs stopped early, and write it to `/Volumes/Shampoo`.

## Recipe

Build release with the target dir on the big volume:

```sh
export CARGO_TARGET_DIR=/Volumes/Shampoo/cargo-target/ct-index-instrument
export TMPDIR=/Volumes/Shampoo/tmp
nix develop -c cargo build --release -p ctadl-ascent --bin ctadl
CTADL=$CARGO_TARGET_DIR/release/ctadl
```

1. **Find the memory guard time.** Run the index under a memory cap: `bench/memguard.sh` on the
   `bench` branch (`memguard.sh <GiB> <command...>`, exits 137 if it killed the command and prints
   peak footprint and wall time on stderr), or the `memory-guard` skill. Note how long the run
   takes to hit the cap. Every later run must stop well before that, because a run killed by the
   guard prints nothing useful; a run Ascent stops prints its profile.

   ```sh
   memguard.sh 16 $CTADL --store $STORE index bin -m model.json5
   ```

2. **Stop the run with `CTADL_INDEX_TIMEOUT_SECS`.** With the patched macro, a stopped run still
   reports per-rule times and the relation sizes as of the stop:

   ```sh
   RUST_LOG=debug CTADL_INDEX_TIMEOUT_SECS=20 memguard.sh 16 \
     $CTADL --store $STORE index bin -m model.json5 2> t20.log
   ```

3. **Step the timeout up: 10 s, 20 s, 30 s, ...**, staying below the guard time. Ascent finishes
   the iteration it is in when the timeout hits, so the actual stop can be well past `n`; small
   steps show how far each step overshoots and where growth takes off. Keep each run's log.

   ```sh
   for t in 10 20 30 40 60; do
     RUST_LOG=debug CTADL_INDEX_TIMEOUT_SECS=$t memguard.sh 16 \
       $CTADL --store $STORE index bin -m model.json5 2> t$t.log || break
   done
   ```

4. **Rank rules by time and by time per tuple.** `index rule counts:` gives each rule's time
   with the tuples it produced and the new ones, so time per tuple is per rule, not per relation;
   `[relsizes]` gives each relation's total. Sort by total time, by time per new tuple, and by
   `dup_ratio`. The few expensive relations fall out, and comparing the steps shows which ones
   keep growing. Add `CTADL_ASCENT_TRACE=1` to one run to see it per iteration: whether a
   relation's delta shrinks toward a fixpoint or keeps up, and at which iteration it takes off.

5. **Characterize the expensive relations by their indices.** For each, list its indices and
   what each costs (`[indexsizes]`, serial engine): how many there are and the bytes of each. Name an
   index by its key columns, the way Ascent names the fields: `reach_indices_0_2` is `reach`
   keyed on columns 0 and 2; `_none` is the full scan and the all-columns index is the
   existence check. An index whose key is a prefix of another's, or one used by a single cold
   rule, is a candidate to drop or to replace with a custom store.

6. **Attribute the rows.** Rerun the last step under the guard with `CTADL_INDEX_DUMP=<dir>` and
   count rows of the expensive relations by function, variable and call site (see above).

## Headless machine

The bench machine has no one at the console. Do not use tools that raise a macOS permission
dialog and then block: `footprint -p`, `sample`, `vmmap`, `lldb` attach. `memguard.sh` already
falls back to `ps` rss when `footprint` does not answer. Wrap anything that might hang in
`timeout`.

## Where candidate benchmarks come from

The continuous benchmark database is `/Volumes/Shampoo/ctadl-bench/bench.db` (SQLite; tables
`units`, `runs`, `metrics`). The metric `mem_budget_ratio` is the index's peak footprint
divided by 100 x the IR bytes; above 1 is over budget.

```sh
sqlite3 /Volumes/Shampoo/ctadl-bench/bench.db "
  select u.suite, u.bench, round(m.value,1) ratio, r.outcome, u.spec
  from metrics m join runs r on r.id = m.run_id join units u on u.id = r.unit_id
  where m.name = 'mem_budget_ratio' order by m.value desc limit 20;"
```

- Beware tiny IRs: a fixed overhead dominates them, so a large ratio there means little.
  Check `peak_footprint_b` too.
- A run killed at the cap (`runs.outcome` `oom` or `killed`) has only a lower bound for its ratio.
- `firmware-sweep` benchmark names are sha256 prefixes. Resolve one to its binary with
  `bench/suites/firmware-sweep.json` on the `bench` branch (`sha256` and `path` per entry).
- The runner deletes the stores, so re-import before indexing. Each unit's exact argv and
  environment is in `units.spec` (JSON): run the `import` unit's argv, then the `index` unit's,
  replacing `{ctadl}` and `{store}`.

  ```sh
  sqlite3 /Volumes/Shampoo/ctadl-bench/bench.db \
    "select phase, spec from units where bench = '166cf67fa15e58b6' and phase in ('import','index');"
  ```

## Rebasing onto a new main

The branch is meant to stay a single commit on top of main. Almost all of it is
`vendor/ascent_macro`, which main never touches; the rest is the `[patch.crates-io]` lines in
`Cargo.toml`, `vendor/ascent_macro` in the readers' source set in `nix/android-jvm.nix` (naersk
fails without it), the new `instrument.rs`, and two lines in `index_engine/mod.rs`
(`mod instrument;` and the `instrument::after_run!(prog, id_map);` call just before
`prog.take_assign_like()`).

```sh
git fetch origin
git rebase origin/main          # conflicts, if any, are in Cargo.toml / Cargo.lock / mod.rs
git reset --soft origin/main && git commit -C ORIG_HEAD   # squash back to one commit if it grew
```

If `mod.rs` conflicts, re-add the two lines. If main renamed a dumped relation or changed its
columns, `instrument.rs` stops compiling: fix the field in `after_run!` and the type in
`Relations`. If the vendored macro no longer matches the `ascent` version main uses, re-vendor
it from the new release and reapply the patch.
