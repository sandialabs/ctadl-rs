# Nightly regression suite

Checks too expensive for `cargo test`, or that need a toolchain it does not have. The orchestration
lives in the `xtask` crate at the repo root (`xtask/src/`), not in shell scripts.

## Running

Run the suite through the flake check. It is one hermetic derivation that does
**everything** — builds `ctadl` and supplies every other tool (javac, dx, Ghidra,
gcc, the Android SDK) from Nix, then runs every check. No dev shell, no PATH
setup, no prebuilt binaries:

```sh
nix build .#checks.aarch64-darwin.regression    # macOS
nix build .#checks.x86_64-linux.regression       # Linux / CI
```

This is the supported way to run the suite, and it is what the nightly GitHub
workflow runs on a schedule. Add expensive tests here, not in YAML.
For iterating on individual checks, run the harness directly — the dev shell
already carries the full regression toolchain on `PATH`:

```sh
cargo xtask regression --frontend pcode        # only the pcode checks
cargo xtask regression --filter apk:           # only checks whose name contains this
```

`--frontend` takes `pcode`, `jvm`, `dex`, or `cil` (comma-separated, or
repeated) and defaults to all of them. It selects *before* anything runs, so
`--frontend pcode` never invokes the Java toolchain and `--frontend jvm,dex` never
starts Ghidra. Use it with `--filter` to narrow further.

Under the hood both paths invoke the `xtask` task runner (`xtask/src/`). The
flake check above remains the canonical path and the one used in CI.

## Cases under `tests/`

- **Pcode facts** — each `tests/pcode-facts/foo.c` is compiled, exported by
  Ghidra and imported, and reports as `Facts:foo`. It is checked on the facts the
  export writes: each of a function's high variables keeps an id of its own. See
  `check_pcode_facts` in `xtask/src/regression.rs`.

`tests/java/` holds Java programs that `jvm:asm-flow` compiles (below); they are
not cases of their own.

## Checks that are not cases

These families are written in Rust in the `xtask` crate and folded into the same report:

- **`dex:*`** — `dex-reader` over the compiled samples (parse, line map, UTF-8 constants) and
  against `baksmali` as ground truth, plus `dex:apk`, which parses every `classes*.dex` in the
  real-world APK. Needs `javac`/`dx` for the sample-derived checks; `dex:apk` does not.
  `dex:opcodes-replay` judges dex-reader's `data_flow()` and `can_throw()` against ART's
  saved observations of every opcode (`xtask/tests/dex/opcodes-art.txt`); it needs `smali`.
- **`jvm:*`** — `jvm-reader` over the same samples compiled to `.class`/`.jar`. Needs `javac`.
  `jvm:asm-flow` compares jvm-reader's dataflow for every instruction with ASM's analyzer, over
  the samples, the Java programs in `tests/java` and a generated class that holds every opcode
  (`xtask/src/asm_flow/`). `jvm:opcodes` runs each opcode on the JVM, changing one input at
  a time, and checks that every input that changes an output is one jvm-reader lists as a source.
  Both need ASM (`asm-classpath`, from the flake).
- **`apk:*`** — `ctadl` itself, end to end, over that same real-world APK
  (`xtask/tests/dex/com.noto_54.apk`). Needs no toolchain at all: the APK is prebuilt and checked
  in, so all these need is the `ctadl` binary.
- **`cil:*`** — `cil-reader` held to Mono.Cecil (byte-identical dumps over Cecil's own test corpus
  and the compiled fixtures, against committed goldens and a live CecilDump), plus the flow layer,
  the `ctadl-cil` frontend, the default .NET models checked against .NET itself, and `cil:taint`,
  which checks the index's summaries against the flows a run of the exec fixtures shows. Needs the
  Cecil checkout and the compiled fixtures (the dev shell and the flake check supply both); without
  them the checks skip. `xtask/src/cil.rs` lists each check.
- **`Pcode:GhidraProject`** — the pcode importer driven against an existing Ghidra project rather
  than a fresh binary. Self-skips without Ghidra.

The `apk:*` checks are where the analyzer meets a real app rather than a fixture: 6.4 MB, two
`classes*.dex`, some 50,000 functions. Importing it costs about 13 seconds, which is why they live
here rather than in `cargo test`. The import is done **once** and the checks below read the store
it wrote:

| Check | What it claims |
| --- | --- |
| `apk:import` | The app imports, the config records the language, format version, artifact path and content hash, and `ctadl inspect` decodes the stored program and reports functions in it. |
| `apk:no-native-libs` | This APK carries no `lib/<abi>` entries, so the native-library pass records no sub-imports and stages nothing — the path that must not need Ghidra. |
| `apk:report` | `ctadl report` runs on the import, its first line names the static tier, the JSON carries every section a Java program has, four aggregate counts plus the site count of each dispatch kind are pinned to this APK, and nothing is written into the store. |
| `apk:report-invariants` | The report's numbers add up — the census sums to its total, the target split sums to the virtual sites, the dispatch kinds partition those sites and their per-kind edge, excess and RTA totals sum back to the pooled ones, every distribution is monotone, RTA never exceeds CHA, the edge shares are ordered, and removing the interface edges cannot grow the call graph — and two runs over one import are byte-identical. |
| `apk:reimport` | A re-import skips an unchanged artifact, and only an unchanged one: falsify the recorded hash and the same command re-imports; `--force` re-imports an unchanged one. |

They select with `--frontend dex` (the app is a Dex artifact) and, like `dex:apk`, self-skip when
no APK resolves. `cargo xtask regression --frontend dex --filter apk:` runs just these, and needs
nothing on `PATH` beyond a Rust toolchain to build `ctadl`.

`apk:report` is the only check in the suite that pins concrete analysis numbers rather than
properties. That is deliberate: it is what catches a silent change in what call resolution
resolves *to*, which no invariant can see. The per-dispatch counts are pinned for the sharper
version of the same reason: a frontend that stopped reading the invoke opcode would report
every virtual call under one kind, and every other assertion here would still pass. The constants live beside the check in
`xtask/src/apk.rs` with instructions for re-pinning them, and a commit that moves them should
say which frontend change did.

Because these drive the shipped binary rather than the library, they cover what a library-level
test cannot: argument wiring in `main.rs`, exit statuses, and the on-disk shape of the store. The
store paths they assert on are spelled out in `xtask/src/apk.rs` rather than imported from
`ctadl-ascent`, deliberately — a layout change should fail them loudly rather than follow along.

## Notes

- Each check runs in its own scratch directory under `$TMPDIR`; nothing is
  written back into `tests/`.
- Checks are independent — one failure does not abort the rest; every check is run
  and the final line reports `N passed, M skipped, K failed`.
- The runner expects its tools (`ctadl`, `javac`, `dx`, `gcc`, Ghidra) on `PATH`.
  The flake check in [Running](#running) builds and supplies all of them; the dev
  shell carries the same toolchain for local iteration.
- `scripts/*.py` are unused by the current suite and kept only for reference.
