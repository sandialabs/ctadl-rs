# CTADL

CTADL (Compositional Taint Analysis in Datalog) is a static taint analyzer. CTADL is implemented with the Ascent (https://s-arash.github.io/ascent/) Datalog engine embedded in Rust.

> **⚠️ Under active development.** CTADL is in flux: commands, flags, and file
> formats may change without notice or backward compatibility.

## Usage

The typical pipeline is **import → index → query**. Use `ctadl <command> --help` for the full,
current set of flags.

| Command | What it does |
| --- | --- |
| `import` | Import a single artifact (`.dex`, `.jar`, `.class`, APK, directory of `.c` files, Ghidra pcode, Flowy) into the store. |
| `index` | Index one or more imported programs into an analysis project, resolving calls and building SSA. Can load prior summaries and propagation models. |
| `query` | Run a taint-analysis query over an indexed project and write results as SARIF. With `--models` and no index yet, reports what those model files match in the imported programs instead. |
| `go` | One-shot convenience: import, index, and query in a single invocation. |
| `init-model` | Emit a template JSON5 model file for defining sources, sinks, and external function propagation models. |
| `inspect` | Inspect the contents of the CTADL store (artifacts, projects). |
| `report` | Measure an imported program's call graph. Needs only an import; reads no index. |
| `legacy-pcode-cli` | Legacy `index`/`query` commands kept for Ghidra pcode integration. |

One-shot APK analysis:

```bash
ctadl go my-app /path/to/my/app.apk query.json
```

Or run the stages separately:

```bash
ctadl import /path/to/app.apk --name my-app
# Optional, and needs only the import: with no index yet, `query` reports which generators
# select a function and which select nothing. Run it while you write the model file rather
# than after indexing.
ctadl query my-app --models sources-and-sinks.json5 --output check.sarif
ctadl index my-app
ctadl query my-app --models sources-and-sinks.json5 --output results.sarif
```

### Report

`ctadl report <name>` measures program-analysis-relevant statistics about a program, heavily
focused on the call graph. It walks the imported IR and runs one class-hierarchy analysis, and
prints the call census, how many targets each virtual call site has (with the distribution, not
just the mean), which method signatures own most of the imprecision, what restricting to
allocated types would buy, fan-in, and recursion.

```bash
ctadl import /path/to/app.apk --name my-app
ctadl report my-app                 # for reading
ctadl report my-app --format json   # for tracking the numbers across runs
```

It needs no index and writes nothing to the store. Naming a project reports on every program in
it separately, since the class hierarchy is per program — for an `.xapk` that is one report per
split APK. `--no-recursion` skips the one section that has to build the whole call graph, which
on a very large app is most of the running time.

### Import

An APK also imports the native libraries packaged in it (`--no-native-libs`, `--native-abi` to
control). For pcode (`-l pcode`), the artifact may be a binary, an existing Ghidra project
(`<name>.gpr`), or a Ghidra Server URL (`ghidra://…`). 

### The JNI bridge

A Java `native` method has no body, and nothing names the function implementing it. Whenever a
Java or Dex artifact is indexed alongside native code, CTADL joins the two and maps the arguments
across the JNI ABI, so taint flows both ways. It runs automatically; there is nothing to write.
Both bindings are covered: the `Java_…` symbol convention, and the `JNINativeMethod[]` tables a
`RegisterNatives` call reads, which CTADL recovers from the library's data sections at import time
— for most real Android apps, that is where the majority of the links come from.

An APK contains both halves, so importing one imports both. Its libraries are recorded as
sub-imports, and naming the APK in `ctadl index` co-indexes them:

```bash
ctadl import app.apk
ctadl index  app app       # <- the bridge fires here
```

Only one ABI is imported per APK (`--native-abi` to choose), and an `.xapk` app bundle imports
directly, splits and all. Disassembly needs Ghidra; without it CTADL warns and imports the Dex half
anyway, leaving the native methods unlinked. When the halves are separate files, import each and
name both:

```bash
ctadl import app.dex            --name app_dex
ctadl import -l pcode libapp.so --name app_native
ctadl index  app app_dex app_native
```

`index` reports what it linked at `info` level. Read those lines: a method that fails to link
produces no flow *and no error*, so the analysis just comes out quieter than it should.

```
jni registry: 3 table(s), 28 entr(ies) in app__arm64-v8a__libcrypto: 28 attributed to 3 class(es), 0 unattributed
jni bridge: 14 native method(s): 12 linked (9 registered, 0 from summary, 1 prototype mismatch), 1 unresolved, 1 ambiguous
```

`registered`, `from summary` and `prototype mismatch` each count a subset of `linked`: the links
that came from a `RegisterNatives` table, the ones whose implementation came from a `--summary`
project (below), and the ones whose native prototype, as the disassembler recovered it, does not
fit the Java descriptor. Each mismatch gets its own warning; the usual fix is to build the library
with `-g` and re-import. On a 32-bit ABI (`armeabi-v7a`, `x86`) a `long` or `double` argument
recovered as two parameters is not a mismatch: the bridge maps it to both. For the per-method
pairings, run with `RUST_LOG=warn,ctadl_ascent::languages::jni=debug`.

Two flags switch it off, for an A/B of what it contributes: `--no-jni-registry` links by symbol
name alone, and `--no-jni-bridge` disables the pass entirely (and implies the first). Use
`--no-jni-bridge` also when joining a pair by hand with a
[`bridge` model](docs/model-generators.md#bridge), so the pair is not bridged twice. Note that the
`RegisterNatives` tables are recovered at *import* time, so a library imported before this feature
existed has none, and a re-import reuses the unchanged library without creating one — re-import
with `ctadl import --force`.

#### Linking against a library indexed on its own

A large app's Java half and each of its libraries can be indexed separately. Index the library
into a project of its own, then index the app without its libraries and pass that project as a
`--summary`:

```bash
ctadl import app.apk                          # app, app__arm64-v8a__libX, app__arm64-v8a__libY
ctadl index  xproj app__arm64-v8a__libX       # the library, on its own
ctadl index  appproj app --no-native-libs --summary xproj
ctadl query  appproj -m models.json           # Java -> libX -> Java flows are found
```

`index --no-native-libs` drops the native libraries naming an APK or XAPK would otherwise pull in
(an `.xapk`'s split APKs stay), so `libY` is never loaded. An import named on the command line is
always kept. `query`, `report` and `inspect` read the project's saved import list, so they see the
same filtered set with no flag of their own.

`--summary P` maps `P`'s saved function summaries into the project, keeping those for functions
the project has. For the JNI bridge it also reads each of `P`'s libraries' symbol table and
`RegisterNatives` tables (never their code), so a Java `native` implemented there is linked, and
the summaries of the function it links to come in with it. The `jni bridge` line counts these
links as `from summary`, and a warning names any linked function `P` has no summaries for. Another
warning says when `P` may not describe this app: a library that is not one of this app's
sub-imports (another ABI, app or version), or one that changed since `P` was indexed.

The native half of such a flow is carried by the summaries alone, so its SARIF locates the Java
steps only; co-index the library to see where the taint goes inside it. Only context-free summaries
cross this way, so a flow in the library that depends on resolving an indirect call is lost.

## Documentation

- [Model generators](docs/model-generators.md) — the declarative language for
  sources, sinks, and propagation through code CTADL cannot see.
- [Debugging](docs/debugging.md).

# Testing

We provide unit and integration tests, as well as regression tests:

```bash
cargo test
cargo xtask regression
```

The regression tests require some complex toolchains; the Nix dev shell provides those dependencies.

# History

CTADL is based on a prior [Souffle implementation](https://github.com/sandialabs/ctadl).

# Copyright

Copyright 2026 National Technology & Engineering Solutions of Sandia, LLC
(NTESS). Under the terms of Contract DE-NA0003525 with NTESS, the U.S.
Government retains certain rights in this software.
