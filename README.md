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
jni bridge: 14 native method(s): 12 linked (9 registered), 1 unresolved, 1 ambiguous
```

`registered` counts the subset of `linked` that came from a `RegisterNatives` table. For the
per-method pairings, run with `RUST_LOG=warn,ctadl_ascent::languages::jni=debug`.

Two flags switch it off, for an A/B of what it contributes: `--no-jni-registry` links by symbol
name alone, and `--no-jni-bridge` disables the pass entirely (and implies the first). Use
`--no-jni-bridge` also when joining a pair by hand with a
[`bridge` model](docs/model-generators.md#bridge), so the pair is not bridged twice. Note that the
`RegisterNatives` tables are recovered at *import* time, so a library imported before this feature
existed has none, and `ctadl import --skip-existing` will not create one — re-import without it.

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
