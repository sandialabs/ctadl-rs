# Logging

`RUST_LOG` is the only verbosity knob, in both directions. There is no `-q` flag and no
`-v` flag.

| Want                                   | Run                          |
| -------------------------------------- | ---------------------------- |
| More: internals, per-item detail       | `RUST_LOG=warn,ctadl=debug`  |
| Even more                              | `RUST_LOG=warn,ctadl=trace`  |
| Default: phase status plus warnings    | *(unset)*                    |
| Less: warnings only, no status         | `RUST_LOG=warn`              |
| One module only                        | `RUST_LOG=warn,ctadl_ascent::languages::jni=debug` |

Keep the leading `warn,` in each of these. `RUST_LOG=debug` on its own turns *every*
dependency up to `debug` too, which buries CTADL's own output under datafusion's and
Ghidra's; `warn,ctadl=debug` raises this project alone and leaves everything else at
`warn`. The `ctadl` prefix matches every crate here whose name starts with it
(`ctadl-ascent`, `ctadl-ir`, `ctadl-flowy`), which is what the default filter
(`warn,ctadl=info`) selects as well.

What goes where:

- **stdout** is command *output* -- the `inspect` listing, the `--dump-ir` /
  bitcode dumps, the parquet record dump. `RUST_LOG` never affects it, so
  `ctadl inspect | jq` keeps working at any verbosity.
- **stderr** is progress, status and warnings, all through the `log` crate, plus the
  final error if the program fails.
- At `info` and `warn` the format is bare -- no timestamp, no module path -- because it
  is meant to be read while a run goes by. A warning is printed under a `Warning:`
  heading. At `debug` and `trace` the timestamp and module path come back, because
  those are read while debugging.
- `error` has no producers by design: a failure propagates up through `Result` and is
  printed once, by `anyhow`, at the top. An `ERROR` line in the output is a bug -- it
  should have been a `log::warn!` or a propagated error.

Roughly, a default run says one thing per phase and a few things per artifact and
sub-import; it does not scale with the size of the program being analyzed. Anything
that scales with functions, call sites or relation rows is at `debug` or below.

# Inspecting an index with duckdb

```
cd ~/.local/state/ctadl/projects/backflash/index && duckdb
```

See summaries from the index:

```sql
select * from summary.parquet join function_id.parquet on summary.function_id = function_id.function_id limit 10;
```

Get functions endpoints are in:

```sql
SELECT DISTINCT f.name AS endpoint_function, t.endpoint_label
FROM read_parquet('taint.parquet')      AS t
JOIN read_parquet('function_id.parquet') AS f
ON t.endpoint_infunc = f.id;
```

# `index` is not deterministic

Running `index` twice on the same unchanged artifacts does not give you the same
index, and re-querying it does not give you the same SARIF. This is expected, not
a bug to chase. Read this before comparing two runs of anything.

What is stable and what is not:

- **Deterministic:** everything written before the fixpoint --- `formal_param`,
  `actual_param`, `call`, `call_target_assign`, `callee_info`,
  `callee_resolvents`, `external_function`, and the `function_id` IdMap. Function
  ids, instruction ids, and therefore the vertex numbers that appear in SARIF are
  byte-stable across indexes.
- **Nondeterministic:** the row *order* of `assign`, `summary` and `paths`
  (written post-fixpoint by `IndexResult::try_save`) and of
  `index_source_map`. The *contents* are identical --- these tables are stable as
  sets and unstable as sequences.

Three causes, in ascending depth. The first two are cheap to fix if the seed
order ever matters; the third is not.

1. `IndexSourceInfo::source_map` (`index_engine/source_info.rs`) is a
   `hashbrown::HashMap` with the default hasher --- randomly seeded per process
   --- and `try_save` serializes it with `into_iter()`.
2. `program_paths` and `summary_paths` (`index_engine/mod.rs`) are collected into
   default-hasher `HashSet`s and then `.into_iter().collect()`ed straight into
   the seed relations. Ascent relations are `Vec`s, so a randomized seed order
   propagates into derivation order. Sorting these two makes `paths.parquet`
   byte-stable, and nothing else.
3. Interned keys hash by heap address: `Symbol = ArcIntern<str>`
   (`ctadl-ir/src/mir/mod.rs`) hashes `get_pointer()`, and `tailshare::Seq`
   --- what `Path` wraps --- uses `std::ptr::hash`. Every hash container keyed by
   an access path or a field symbol therefore iterates in heap-address order, and
   addresses move per process. This is hasher-independent: ascent's relation
   indices and the BYODS tries both use `FxHasher` and are still address-ordered.
   Hashing `Seq` by contents does not help, because that recurses into
   `PathSegment::Symbol(ArcIntern<str>)`, which hashes the pointer again.

How this reaches the SARIF: `query` is deterministic *given an index*, but
`TaintSearchGraph::new` (`query_engine/search.rs`) builds its adjacency lists by
pushing `assign` rows in stored order, and the search is breadth-first-shortest.
Among equal-length paths the tie-break is adjacency order, i.e. index row order.
A different index picks a different *witness* path for the same source/sink pair,
so the rendered code flow changes while the finding does not.

Consequences for measurement:

- Endpoint-level counters are reproducible: sources and sinks matched, result
  counts, per-file/per-plugin attribution, which sinks a given source reaches.
- Flow-rendering counters are not: distinct source->sink pairs, how many results
  anchor on the last code-flow step, per-result step and alternative-flow totals.
  Expect a few counts of drift between indexes of identical input.
- To attribute a moved number to a code change, compare the stable counters, or
  hold the index fixed and vary only the model. Never diff two SARIFs byte-wise.
- Regression cases must assert set-level properties --- reached lines, whether a
  code flow connects a source and a sink --- never the shape of a rendered flow.
  The `nightly` harness does this today; keep it that way.

Check that two indexes agree as sets (0/0 means order-only difference):

```sql
SELECT
  (SELECT count(*) FROM (SELECT * FROM read_parquet('A/assign.parquet')
                         EXCEPT SELECT * FROM read_parquet('B/assign.parquet'))) AS only_in_a,
  (SELECT count(*) FROM (SELECT * FROM read_parquet('B/assign.parquet')
                         EXCEPT SELECT * FROM read_parquet('A/assign.parquet'))) AS only_in_b;
```

# Pcode

## Ghidra output

A headless Ghidra run prints thousands of lines of analyzer progress. None of it
goes to your terminal by default; it is captured verbatim, both streams
interleaved, in

```
~/.local/state/ctadl/imports/<name>/ghidra.log
```

The child writes it there directly, so `tail -f` on it follows a running import.
An APK's native libraries are each their own sub-import
(`<parent>__<abi>__<stem>`), so each gets its own `ghidra.log`.

When Ghidra fails, or succeeds but exports no facts, the error names that path and
quotes the last 20 lines, so a failed import points you at the log without your
having to know it exists.

## Duckdb

Print high PCode in Duckdb:

```sql
SELECT
    bbf."column1",
    printf('%x', target."column1") AS addr,
    o."column1" AS output,
    mnem."column1",
    i0."column2" AS in0,
    i1."column2" AS in1,
    i2."column2" AS in2
FROM read_csv("PCODE_INDEX.facts", header=false) idx
JOIN read_csv("PCODE_MNEMONIC.facts", header=false) mnem USING ("column0") --id
JOIN read_csv("PCODE_TARGET.facts", header=false) target USING ("column0") --id
JOIN read_csv("PCODE_PARENT.facts", header=false) par USING ("column0") --id
JOIN read_csv("BB_HFUNC.facts", header=false) bbf ON (par."column1" = bbf."column0") --bbid
LEFT JOIN read_csv("PCODE_OUTPUT.facts", header=false) o USING ("column0")
JOIN read_csv("PCODE_INPUT.facts", header=false) i0 ON (i0."column0"=idx."column0" AND i0."column1"=0)
LEFT JOIN read_csv("PCODE_INPUT.facts", header=false) i1 ON (i1."column0"=idx."column0" AND i1."column1"=1)
LEFT JOIN read_csv("PCODE_INPUT.facts", header=false) i2 ON (i2."column0"=idx."column0" AND i2."column1"=2)
ORDER BY target."column1", idx."column1";
-- WHERE
-- Function to fetch
-- bbf.hfunc = 'main@1400014d2'
-- ORDER BY target.target_address, idx."index";
```

## Sqlite

```
cd ~/.local/state/ctadl/imports/ls/facts
cat pcode_schema.sql | sqlite3 facts.db
```

