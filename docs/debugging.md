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
dependency up to `debug` too, which buries CTADL's own output under its dependencies' and
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
`const_reaches`, the string labels pooled at global paths.

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
written by an incompatible build, this fails.

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
index. This is expected, not a bug to chase. Read this before comparing two runs of anything.

What is stable and what is not:

- **Deterministic:** everything written before the fixpoint --- `formal_param`,
  `actual_param`, `call`, `callee_info`, `external_function`, and the
  `function_id` IdMap. Function
  ids and instruction ids are byte-stable across indexes.
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

`tests/integration/determinism.rs` checks that the graphviz and the relation dumps come out
byte-identical whatever order values were interned in.

Check that two indexes agree as sets (0/0 means order-only difference):

```sql
SELECT
  (SELECT count(*) FROM (SELECT * FROM read_parquet('A/assign.parquet')
                         EXCEPT SELECT * FROM read_parquet('B/assign.parquet'))) AS only_in_a,
  (SELECT count(*) FROM (SELECT * FROM read_parquet('B/assign.parquet')
                         EXCEPT SELECT * FROM read_parquet('A/assign.parquet'))) AS only_in_b;
```

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
scripts/fact-dump-diff.sh record ref-serial -- ctadl index app --monolithic
scripts/fact-dump-diff.sh record ref-par    -- ctadl index app --monolithic -j 8
scripts/fact-dump-diff.sh record ref-lsm    -- ctadl index app --monolithic --lsm

# Later, compare a candidate build against the reference
scripts/fact-dump-diff.sh compare ref-serial -- target/release/ctadl index app --monolithic
scripts/fact-dump-diff.sh compare -k cand-par ref-par -- target/release/ctadl index app --monolithic -j 8

# Or compare two saved dumps, for example two engines
scripts/fact-dump-diff.sh diff ref-serial ref-par
```

It exits 0 if every relation is the same set. Otherwise it exits 1 and prints, for each
differing relation, the row count on each side, how many rows only one side has, and the first
few of them (`-n N`, default 5). The `config` relation leaves out the engine and storage, so
dumps of different engines compare.

`ctadl index` also writes the seven tables of the index it saved (`summary`, `assign`, `paths`,
`external_function`, `global_flow`, `resolved_call`, `intent_pair`) to `<dir>/outputs/`, printed
the same way. The relations are the fixpoint's working state, so they compare only between runs
that compute the index the same way; the output tables compare between any two runs. With
`--outputs`, every mode works on `outputs/` alone:

```
scripts/fact-dump-diff.sh record ref -- ctadl index app --monolithic
scripts/fact-dump-diff.sh compare --outputs ref -- target/release/ctadl index app --staged
scripts/fact-dump-diff.sh compare --outputs ref -- target/release/ctadl index app --index-budget 2G -j 4
```

This is how a staged run is checked. It holds no relation of the whole program, only one batch's
at a time, so it writes `outputs/` and no relations, and only `--outputs` compares it. Record
the reference with `--monolithic`, which is the run the staged one must match. To see a batch's
own relations, set `CTADL_DUMP_STAGES=1` as well: each batch then dumps them to
`<dir>/stages/<batch>/`, `batch-3` or `batch-3-round-2` for a part of a split recursive SCC. A
batch in a worker process has no names for functions, so they print as numbers there.

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
