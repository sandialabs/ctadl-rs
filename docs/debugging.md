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
select * from summary.parquet join function_id.parquet on summary.func_id = function_id.id limit 10;
```

`ctadl inspect <index>/function_id.parquet` prints the same function table without duckdb,
one `<id> function(<name>)` per line.

A summary row relates two *roots*, each a formal or a global, written as two columns: `dst_index`
and `dst_global` (one of them null), and likewise `src_index` and `src_global`. A formal index is
`-1` for the return. The rows rooted at a global:

```sql
select f.name, s.* from summary.parquet s join function_id.parquet f on s.func_id = f.id
where s.dst_global is not null or s.src_global is not null;
```

No row has two global roots: a flow between two globals is no function's (G6 of
[ir.md](ir.md#globals)) but an edge of the *global frame*, the reserved function `<globals>`
(the largest function id, `GLOBAL_FRAME`), which `function_id.parquet` does not list.
`global_flow.parquet` holds those edges, `(dst, dst_path, src, src_path)` over global names, and
`assign.parquet` holds them too, under the frame's id. A vertex variable may be a global, tag 4 in
the `*_var_tag` columns, with its name in `*_var_ref_name`. `globals.parquet` lists each global and
an import that declares it, with `source_span_id`, the declaration's span in that import's
source-info tables when the front end recorded one: a C declarator, a flowy `var`, a Lua chunk
(`_ENV`), a class header (JVM), `class_def_item` (dex) or `TypeDef` row (CIL), or the image
itself, offset 0 (pcode).

```sql
select g.name, i.name as import, g.source_span_id
from globals.parquet g join import_id.parquet i on g.import_id = i.id;
```

`CTADL_DUMP_RELATIONS` (below) prints the frame's rows under `<globals>`, among them
`call_target_assign_like` and `const_reaches`, the labels pooled at global paths.

Get functions endpoints are in:

```sql
SELECT DISTINCT f.name AS endpoint_function, t.endpoint_label
FROM read_parquet('taint.parquet')      AS t
JOIN read_parquet('function_id.parquet') AS f
ON t.endpoint_infunc = f.id;
```

# Dumping the index graph

`assign` holds one row per flow between two access paths: a copy when the destination path is
empty, a store otherwise (see [ir.md](ir.md)); there is no separate relation for a load or a
store. The index graph --- the `assign` relation, drawn --- comes out of a *finished* index:

```
ctadl index app
ctadl inspect app --dump-index-graph app.dot
```

`NAME` here is an analysis project, not an import: the flag reads `assign.parquet` and
`function_id.parquet` out of the project's `index/`, so nothing is re-indexed and nothing is
written back to the store. The project must have been indexed; if it was not, or if the index was
written by an incompatible build, this fails the same way `ctadl query` would.

An edge `A -> B` is the assignment `B = A`. A node is the triple (function, variable, access
path), so `local(t)` and `local(t).x` are distinct; the file opens with a legend saying so.

Two dumps of the same program are byte-identical, and `diff` between them is meaningful. This is
the one thing in the post-fixpoint output that is --- see the next section for why the tables
themselves are not. `inspect` gets it by sorting `assign` by content before rendering
(`graphviz::index_edge_cmp`), which is also why node ids and variable names are compared as text
rather than by their string-table ids.

Two things to know before pointing this at a real target. The whole `assign` table is loaded into
memory, and on a large program it is the biggest thing in the index. And the resulting graph has
millions of nodes, which Graphviz will not lay out usefully --- there is no filtering flag yet, so
this is a tool for small inputs and for reduced test cases.

# `index` is not deterministic

Running `index` twice on the same unchanged artifacts does not give you the same
index, and re-querying it does not give you the same SARIF. This is expected, not
a bug to chase. Read this before comparing two runs of anything.

What is stable and what is not:

- **Deterministic:** everything written before the fixpoint --- `formal_param`,
  `actual_param`, `call`, `call_target_assign`, `callee_info`,
  `callee_resolvents`, `external_function`, and the `function_id` IdMap --- as
  facts about functions and call sites. Instruction ids are not part of that
  promise: a front end may number instructions in any order (in parallel, say),
  even across two imports of one artifact, so do not compare instruction ids, or
  the vertex numbers in SARIF that carry them, across indexes. `ctadl diff-index`
  (next section) names instructions by content instead.
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
3. Interned keys hash by address or id: `Symbol = ArcIntern<str>`
   (`ctadl-ir/src/cir/mod.rs`) hashes `get_pointer()`, and `Path` hashes its
   interner id, which depends on the order paths were first interned. Every hash
   container keyed by a path or a field symbol therefore iterates in an
   order that moves per process. This is hasher-independent: ascent's relation
   indices and the BYODS tries both use `FxHasher` and are still id-ordered.
   Hashing `Path` by contents does not help, because that recurses into
   `PathSegment::Symbol(ArcIntern<str>)`, which hashes the pointer again.

How this reaches the SARIF: the search is breadth-first-shortest, and among
equal-length paths the tie-break is adjacency order. `TaintSearchGraph::new`
(`query_engine/search.rs`) sorts each adjacency list by content, so that order is
not the index's row order, and the query's output is sorted by content too.
`tests/integration/determinism.rs` checks that the graphviz, the relation
dumps and the SARIF come out byte-identical, timestamps aside, whatever order
values were interned in. Without that sort, a different index would pick a different
*witness* path for the same source/sink pair, so the rendered code flow would change
while the finding did not.

Consequences for measurement:

- Endpoint-level and flow-rendering counters are both reproducible: two indexes
  of identical input give the same SARIF, timestamps aside.
- Regression cases must still assert set-level properties --- reached lines,
  whether a code flow connects a source and a sink --- never the shape of a
  rendered flow.
  The `nightly` harness does this today; keep it that way. To compare two query outputs
  this way, use `ctadl diff-query --endpoints-only` (see *Comparing two query outputs*).

# Comparing two index runs

To check whether two `ctadl index` runs wrote the same index (serial and parallel, before and
after a change, two builds), use `ctadl diff-index`. Do not write a comparison script: the
command already avoids the ways a fresh one goes wrong (comparing rows in file order, comparing
ids a run numbers for itself, losing NULLs in tagged columns, leaving tables out, comparing
indexes of different formats, cutting output short without saying so).

```
ctadl diff-index app app-par                    # two projects in the store
ctadl diff-index before/index after/index       # two index directories
ctadl diff-index A B --table call --function onCreate --limit 50
ctadl diff-index A B --json
```

Each side is a project name or an index directory. Both must be in the index format this build
reads; otherwise the command exits 2 without comparing anything. Every table either index holds
is compared as a multiset of rows. Row order, row groups, function numbering, instruction
numbering and the order values were interned in do not matter, span ids are not compared, and
import ids are compared by the import's name. A `.parquet` file that holds no known table is
listed as not compared. `profile.json`, the index configuration and relation dumps are not
compared.

The first line is `IDENTICAL`, or `DIFFERENT: N of M tables differ:` and their names. Then each
table gets a line: its row counts, and how many distinct rows are only in A, only in B, or on
both sides a different number of times. Each differing table prints at most `--limit` rows per
side (20 by default), first by content, each as `[A n, B m]` followed by the row, and says how
many it did not show. `--json` prints the same report as one JSON document. The exit code is 0
for the same, 1 for different, 2 for an error.

An instruction is named by its facts: its function, and its rows of `call`, `actual_param`,
`call_target_assign` and `callee_info`, printed as `caller@callee(arguments)`. An instruction
whose input facts change, an added argument for example, gets a new name, so every row about it
shows up as removed and added. Instructions of one function with the same facts share a name,
and the diff counts how many rows land on it. In particular, every instruction with no input
facts in a function shares one name (`caller@<no facts>`). Android intent sends are the common
case: each send gets a bridge instruction whose facts are all derived, so if two sends in one
function swap their receivers, `intent_pair` holds the same rows and the diff does not see it.

For ad-hoc questions the command does not answer, query the tables with DuckDB. This recipe
compares raw ids as sets, so it only means something between indexes whose function and
instruction ids agree, and it does not count repeated rows (0/0 means an order-only
difference):

```sql
SELECT
  (SELECT count(*) FROM (SELECT * FROM read_parquet('A/assign.parquet')
                         EXCEPT SELECT * FROM read_parquet('B/assign.parquet'))) AS only_in_a,
  (SELECT count(*) FROM (SELECT * FROM read_parquet('B/assign.parquet')
                         EXCEPT SELECT * FROM read_parquet('A/assign.parquet'))) AS only_in_b;
```

# Comparing two query outputs

To check whether two `ctadl query` runs found the same things (before and after a model change
or an engine change, two builds), use `ctadl diff-query`. Do not compare the files with `diff`
or `jq`: they compare arrays in the order they were written, and logical location indexes that
each run numbers for itself. Microsoft's `sarif diff` is no better: it counts results per rule,
so a result that moves is no change.

```
ctadl diff-query before.sarif after.sarif
ctadl diff-query A.sarif B.sarif --limit 50
ctadl diff-query A.sarif B.sarif --endpoints-only
ctadl diff-query A.sarif B.sarif --json
```

The results of every run in a file are compared as one multiset, each by its content: its rule,
kind and level (absent compares equal to SARIF's default), its locations, its code flows, and
the properties ctadl writes (`taintLabels`, `taintVertex`, `taintVertices`, `sourceFunctions`,
`sinkFunctions`, `taintedFormals`, `import`). A logical location is compared by the name it
resolves to, not by its index; a physical location by its `uriBaseId` name, `uri`, region and
address. The order of results, of a result's locations and code flows, of a code flow's thread
flows, of a location's logical locations and of the set- and map-valued properties does not
matter. The steps of a thread flow are compared in order, each by its `kinds`, its location,
and the `taintVertex`, `callee` and `summarized` in its `properties` (absent compares equal
only to absent).

`ctadl query` writes a step at each call the path passes through, and says in its
`properties` what the step is:

- Every step has `taintVertex`, the tainted vertex its message names, as
  `<variable><path>`.
- A call step, where the path enters a callee, has `callee`: the function entered, which its
  message names too.
- A return step has `callee`: the function returned from. Its message names the function
  returned into, the caller.
- A summarized step, where the path crosses a call by a summary or a model without entering a
  callee, has `summarized: true` and no `callee`: nothing records which callee's summary
  carried the taint.
- A source or sink step has `taintVertex` only. Every callee of its call site is in the
  result's `sourceFunctions` or `sinkFunctions`, sorted by name.

A flow has one source and one sink, but its first or last step is not always the source or
sink step. The sink step is not written when the step before it is at the same instruction (a
return into the sink's own vertex, or a source and sink at one call with nothing between), and
a source or sink with no call site (a global, or a function with no callers) gets no step.
`--endpoints-only` then compares the step that is there. Files written by an older
`ctadl query` have no step properties, and their steps are compared by `kinds` and location.

The execution notifications (`toolExecutionNotifications`) of every invocation of every run in
a file are compared as one multiset, each by everything it holds but `message.text` and
`message.arguments`: its `descriptor`, `level`, `message.id` and `properties`. The invocations
of every run are compared as one multiset too, each by its `executionSuccessful` and `exitCode`
(absent compares equal only to absent).

Not compared: run metadata, the tool and its rules, message text anywhere, a notification's
`message.arguments`, `toolConfigurationNotifications`, the rest of an invocation (`commandLine`,
`arguments`, times, `exitCodeDescription`, `ruleConfigurationOverrides`), `executionOrder`
(checked to increase, then dropped), the `sourceCallee` and `sinkCallee` that files written by
an older `ctadl query` hold, the debug profile's `fileSpanId`, `funcId` and `insnId`, the value
a `uriBaseId` stands for, and the instruction id in a `call-arg(<id>, <formal>)` vertex, in a
result or a step. Equal results, notifications or invocations count separately: a result written
twice in A and once in B is a difference. The canonical form of a result and the merge that
compares two multisets are proved with Verus (`sarif-canon`, `multiset-diff`); the parser that
picks the fields out of a file is tested on real `ctadl query` output in every profile.

`--endpoints-only` compares a `C0001.tainted-path` result by its rule, kind, level,
`taintLabels`, `sourceFunctions`, `sinkFunctions`, and the first and last step of each thread
flow, with their `taintVertex`, `callee` and `summarized`. Its primary location, its interior
steps, and its own `taintVertex`, `taintVertices`, `taintedFormals` and `import` are left out,
so two runs that pick different witness paths for one source/sink pair (see above) compare
equal. Results with equal endpoints still count separately: two such flows in A and one in B is
a difference. Other rules are compared in full, and notifications and invocations as without the
flag. The flag changes what is compared, never whether a file is accepted.

The first line is `IDENTICAL` or `DIFFERENT`. Then the two files, `flag: --endpoints-only` when
it is given, and how many results each file holds. Then, for each side that holds some results
more often than the other, `more in A: N results, K not shown` (N distinct results) and at most
`--limit` of them (20 by default), first by their printed form, each as `[A n, B m]` followed by
the result as one line of JSON: SARIF's key names, logical locations as their names, absent
fields and empty collections left out, a step's `taintVertex`, `callee` and `summarized` under
`properties` with the instruction id of a `call-arg` vertex printed as `_`, and a flow cut to
its endpoints as `{"endpoints":[first,last]}`. Then `notifications: A n, B m` and
`invocations: A n, B m`, each with its own `more in A` and `more in B` rows in the same form and the same
`--limit`: a notification as its JSON without `message.text` and `message.arguments`, keys
sorted, and an invocation as `{"executionSuccessful":…,"exitCode":…}`. For example, with
`xfer.c` from `ctadl-ascent/tests/c` as A, and as B the same file with a second sink call,
`sink(s);`, added at line 14:

```
DIFFERENT
A: a.sarif
B: b.sarif
results: A 3, B 5
more in B: 2 results, 0 not shown
  [A 0, B 1] {"ruleId":"C0001.tainted-path","kind":"fail","level":"warning","locations":[{"physicalLocation":{"artifactLocation":{"uriBaseId":"IMPORT_xfer.c","uri":"xfer.c"},"region":{"startLine":14,"startColumn":3,"endLine":14,"endColumn":10}},"logicalLocations":["main"]}],"codeFlows":[{"threadFlows":[{"locations":[{"kinds":["taint"],"location":{"physicalLocation":{"artifactLocation":{"uriBaseId":"IMPORT_xfer.c","uri":"xfer.c"},"region":{"startLine":11,"startColumn":3,"endLine":11,"endColumn":15}},"logicalLocations":["main"]},"properties":{"taintVertex":"call-arg(_, -1)"}},{"kinds":["taint"],"location":{"physicalLocation":{"artifactLocation":{"uriBaseId":"IMPORT_xfer.c","uri":"xfer.c"},"region":{"startLine":14,"startColumn":3,"endLine":14,"endColumn":10}},"logicalLocations":["main"]},"properties":{"taintVertex":"call-arg(_, 0)"}}]}]}],"properties":{"taintLabels":["xfer"],"taintVertices":[["xfer",["call-arg(_, 0)"]]],"sourceFunctions":["source"],"sinkFunctions":["sink"]}}
  [A 0, B 1] {"ruleId":"C0004.taint-sink","kind":"informational","level":"none","locations":[{"physicalLocation":{"artifactLocation":{"uriBaseId":"IMPORT_xfer.c","uri":"xfer.c"},"region":{"startLine":14,"startColumn":3,"endLine":14,"endColumn":10}},"logicalLocations":["sink"]}],"properties":{"taintLabels":["xfer"],"taintVertex":"call-arg(_, 0)"}}
notifications: A 3, B 3
more in A: 2 notifications, 0 not shown
  [A 1, B 0] {"descriptor":{"id":"CTADL0100.endpoint-match-summary"},"level":"note","message":{"id":"default"},"properties":{"portsDeclared":2,"portsUnmatched":0,"sinksDeclared":1,"sinksMatched":1,"sourcesDeclared":1,"sourcesMatched":1,"unmatchedDeclarations":0}}
  [A 2, B 1] {"descriptor":{"id":"CTADL0111.call-without-body"},"level":"note","message":{"id":"default"},"properties":{"calls":1}}
more in B: 2 notifications, 0 not shown
  [A 0, B 1] {"descriptor":{"id":"CTADL0100.endpoint-match-summary"},"level":"note","message":{"id":"default"},"properties":{"portsDeclared":2,"portsUnmatched":0,"sinksDeclared":1,"sinksMatched":2,"sourcesDeclared":1,"sourcesMatched":1,"unmatchedDeclarations":0}}
  [A 0, B 1] {"descriptor":{"id":"CTADL0111.call-without-body"},"level":"note","message":{"id":"default"},"properties":{"calls":2}}
invocations: A 1, B 1
```

B has one new flow, to line 14, and one more `C0004.taint-sink` result, at line 14. Its
notifications count the new call: the endpoint summary matches two sinks, and `sink`'s
`CTADL0111.call-without-body` counts 2 calls. In A that notification equals `source`'s once the
message is dropped, hence `[A 2, B 1]`.

`--json` prints the same report as one JSON document, each row's printed form as a string:

```
{"result":"DIFFERENT","results_differ":2,"notifications_differ":4,"invocations_differ":0,
 "a":"a.sarif","b":"b.sarif","endpoints_only":false,"limit":20,"results_a":3,"results_b":5,
 "more_in_a":{"rows":[],"not_shown":0},
 "more_in_b":{"rows":[{"row":"{\"ruleId\":\"C0001.tainted-path\",...}","count_a":0,"count_b":1},
 ...],"not_shown":0},
 "notifications_a":3,"notifications_b":3,
 "notifications_more_in_a":{"rows":[{"row":"{\"descriptor\":{\"id\":\"CTADL0100.endpoint-match-summary\"},...}",
 "count_a":1,"count_b":0},...],"not_shown":0},
 "notifications_more_in_b":{"rows":[{"row":"{\"descriptor\":{\"id\":\"CTADL0100.endpoint-match-summary\"},...}",
 "count_a":0,"count_b":1},...],"not_shown":0},
 "invocations_a":1,"invocations_b":1,"invocations_more_in_a":{"rows":[],"not_shown":0},
 "invocations_more_in_b":{"rows":[],"not_shown":0}}
```

`results_differ`, `notifications_differ` and `invocations_differ` count the distinct rows that
differ in each, shown or not. The exit code is 0 when results, notifications and invocations are
all equal as multisets, 1 when any differ, and 2 on an error, which goes to stderr with nothing
on stdout: a missing or unreadable file; a file that is not SARIF 2.1.0 (an unknown `kind` or
`level` included); a result property of a shape ctadl does not write; a step `taintVertex` or
`callee` that is not a string, or a step `summarized` that is not a boolean; a logical location
with neither `fullyQualifiedName` nor `name`; a logical location `index` that is negative or
past the end of the run's `logicalLocations`; a thread flow with no steps; or `executionOrder`
values that do not increase. Neither `--limit`, even 0, nor `--json` changes the exit code.

A result is compared by the locations ctadl writes. Where ctadl writes a source or sink result
(`C0003.taint-source`, `C0004.taint-sink`) with only a logical location, naming the function,
and no physical location at its call site, results at different call sites of one function
compare equal, since the call site's instruction id is not compared either, and are counted by
multiplicity. Adding or removing such a call is reported; moving one is not. A field ctadl
starts writing later is not compared until `diff-query` reads it.

# Comparing every relation: `scripts/fact-dump-diff.sh`

The parquet tables hold only the index's outputs. To check that a change derives the same
fixpoint, including `locals`, the propagation relations and the contextual ones, set
`CTADL_DUMP_RELATIONS=<dir>` on a `ctadl index` run. Once the fixpoint is reached, the engine
writes every relation of `index_rules` to `<dir>/<relation>.tsv`, one row per line with tabs
between columns, and every column printed by content: a function by its name, a path, string
or call target by its text, a decision set by its decisions, sorted. No intern id is printed, so
two builds that intern in different orders print the same rows. The dump panics if a relation
in the program is missing from `dump_relations!` (`index_engine/dump.rs`).

`scripts/fact-dump-diff.sh` runs the command, sorts each relation (`LC_ALL=C sort -u`), and
compares the dumps as sets:

```
# Record a reference with the build on PATH, one per engine
scripts/fact-dump-diff.sh record ref-serial -- ctadl index app
scripts/fact-dump-diff.sh record ref-par    -- ctadl index app -j 8
scripts/fact-dump-diff.sh record ref-lsm    -- ctadl index app --lsm

# Later, compare a candidate build against the reference
scripts/fact-dump-diff.sh compare ref-serial -- target/release/ctadl index app
scripts/fact-dump-diff.sh compare -k cand-par ref-par -- target/release/ctadl index app -j 8

# Or compare two saved dumps, for example two engines
scripts/fact-dump-diff.sh diff ref-serial ref-par
```

It exits 0 if every relation is the same set. Otherwise it exits 1 and prints, for each
differing relation, the row count on each side, how many rows only one side has, and the first
few of them (`-n N`, default 5). The `config` relation leaves out the engine and storage, so
dumps of different engines compare.

The dump and the sort scale with the index: `locals` alone is tens of millions of rows on a large
target. Raw dumps and sort's temporary files go under `$TMPDIR`, so point it at a disk with room
for a few times the dump. A reference is only as good as the build that wrote it: record it with
a build that has `CTADL_DUMP_RELATIONS`, and pass the same index flags on both sides.

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

Print high PCode in Duckdb. Ids are numbers; `HFUNC_FUNC` names each function and
`VNODE_NAME` each varnode:

```sql
SELECT
    fn."column1" AS function,
    printf('%x', target."column1") AS addr,
    o."column1" AS output,
    mnem."column1",
    i0."column2" AS in0,
    i1."column2" AS in1,
    i2."column2" AS in2
FROM read_csv("BB_PCODE_INDEX.facts", header=false) idx -- block, index, op
JOIN read_csv("PCODE_MNEMONIC.facts", header=false) mnem ON (mnem."column0" = idx."column2")
JOIN read_csv("PCODE_TARGET.facts", header=false) target ON (target."column0" = idx."column2")
JOIN read_csv("BB_HFUNC.facts", header=false) bbf ON (bbf."column0" = idx."column0")
JOIN read_csv("HFUNC_FUNC.facts", header=false) fn ON (fn."column0" = bbf."column1")
LEFT JOIN read_csv("PCODE_OUTPUT.facts", header=false) o ON (o."column0" = idx."column2")
LEFT JOIN read_csv("PCODE_INPUT.facts", header=false) i0 ON (i0."column0" = idx."column2" AND i0."column1" = 0)
LEFT JOIN read_csv("PCODE_INPUT.facts", header=false) i1 ON (i1."column0" = idx."column2" AND i1."column1" = 1)
LEFT JOIN read_csv("PCODE_INPUT.facts", header=false) i2 ON (i2."column0" = idx."column2" AND i2."column1" = 2)
-- Function to fetch:
-- WHERE fn."column1" = 'main@1400014d2'
ORDER BY target."column1", idx."column1";
```

## Sqlite

```
cd ~/.local/state/ctadl/imports/ls/facts
cat pcode_schema.sql | sqlite3 facts.db
```

# Debugging Android apps

The dev shell carries an emulator, adb, frida, jdb and a native lldb. The flake
comments next to `androidDebugSdk` have the command sequences; these are the
things that do not work the obvious way.

- **The SDK is read-only.** It lives in the Nix store, so `sdkmanager --install`
  fails. Add components to `androidDebugSdk` in `flake.nix` instead.
- **jdb needs a debuggable app, even on the userdebug emulator image.** A release
  APK gets `Not starting debugger since process cannot load the jdwp agent` in
  logcat, `adb jdwp` lists nothing, and `jdb -attach` fails its handshake. Setting
  `persist.debug.dalvik.vm.jdwp.enabled` does not help. Repack the APK instead:

  ```
  apktool d -o app app.apk
  # add android:debuggable="true" to <application> in app/AndroidManifest.xml
  apktool b -o app-unsigned.apk app
  zipalign -p -f 4 app-unsigned.apk app-aligned.apk
  keytool -genkeypair -keystore dbg.keystore -storepass android -keypass android \
    -alias dbg -keyalg RSA -validity 10000 -dname CN=dbg
  apksigner sign --ks dbg.keystore --ks-pass pass:android --out app-debug.apk app-aligned.apk
  adb uninstall <pkg>; adb install app-debug.apk
  ```

  The `-p` matters for apps with native code: without it the install fails with
  `Failed to extract native libraries`. The new signature means the original app
  has to be uninstalled first. Frida needs none of this: after `adb root` it
  works on any app.
- **`am start -D` waits only until a debugger attaches.** Once jdb connects the
  app runs on, so a breakpoint typed a few seconds later can miss startup code
  like `onCreate`/`onResume`. Set it as jdb's first command.
- **jdb only invokes methods from a thread stopped at an event.** After a plain
  `suspend`, `print Foo.bar()` fails with `IncompatibleThreadStateException` or
  `Name unknown`. Stop at a breakpoint first. A dotted nested class does not resolve:
  `android.os.Build.VERSION.SDK_INT` gives `No static field or method with the
  name VERSION`.
- **jdb's thread and object ids change between sessions.** An id from one
  `jdb -attach` is meaningless in the next, so script a single session rather than
  re-attaching for each command.
- **Use `android-lldb`, not the NDK's lldb or `lldb`.** The NDK's host lldb needs a
  `libpython3.11.dylib` the Nix package leaves out, so it dies in dyld.
  `/usr/bin/lldb` is an xcrun shim that cannot find Xcode inside the shell.
  `android-lldb` is nixpkgs' lldb 21, which segfaults attaching to an Android
  process while it loads modules in parallel; the wrapper runs
  `settings set target.parallel-module-load false` for you. The device side is
  `$ANDROID_LLDB_SERVER`.
- **frida-server must match the host frida exactly.** `$FRIDA_SERVER` is the
  matching build; do not download one by hand.
