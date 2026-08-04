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

An app's Java and native halves analyze as a whole using [the JNI bridge](docs/jni.md). When the
halves are separate files, import each and name both:

```bash
ctadl import app.dex            --name app_dex
ctadl import -l pcode libapp.so --name app_native
ctadl index  app app_dex app_native
```

## Documentation

- [Model generators](docs/model-generators.md) — the declarative language for
  sources, sinks, and propagation through code CTADL cannot see.
- [The JNI bridge](docs/jni.md) — how Java `native` methods are linked to their
  native implementations when both are indexed together.
- [Debugging](docs/debugging.md).

# Testing

Unit and integration tests run with Cargo:

```bash
cargo test
```

The regression tests, however, are only reliable when run through Nix, which
pins the full toolchain (compilers, Ghidra, etc.) the fixtures are built and
checked against. To run the whole suite as a sealed check:

```bash
nix build .#checks.${system}.regression
```

where `${system}` is your platform (e.g. `aarch64-darwin`, `x86_64-linux`).
Running the regression suite outside Nix is not reliable because results depend
on the exact compiler/disassembler versions Nix provides.

For iterating on tests, the `regression` dev shell provides that same pinned
toolchain while letting you run the harness against your local working tree:

```bash
nix develop .#regression -c cargo xtask regression
```

# History

CTADL is based on a prior [Souffle implementation](https://github.com/sandialabs/ctadl).

# Copyright

Copyright 2026 National Technology & Engineering Solutions of Sandia, LLC
(NTESS). Under the terms of Contract DE-NA0003525 with NTESS, the U.S.
Government retains certain rights in this software.
