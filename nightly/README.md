# Nightly regression suite

Source-sink taint regression tests. Each case is a small program plus a JSON
file holding its **known answer** (the source lines a flow is expected to reach)
and the ctadl query model. The orchestration lives in the `xtask` crate at the
repo root (`xtask/src/`), not in shell scripts.

## Running

Run the suite through the flake check. It is one hermetic derivation that does
**everything** — builds `ctadl` and `dex-reader` and supplies every other tool
(javac, dx, Ghidra, gcc/addr2line, the Android SDK) from Nix, then runs all the
cases. No dev shell, no PATH setup, no prebuilt binaries:

```sh
nix build .#checks.aarch64-darwin.regression    # macOS
nix build .#checks.x86_64-linux.regression       # Linux / CI
```

This is the supported way to run the suite, and it is what the nightly GitHub
workflow runs on a schedule. Add expensive tests here, not in YAML.
If you are iterating on individual cases, the flake also provides a local dev
shell with the full regression toolchain on `PATH`:

```sh
nix develop .#regression
cargo xtask regression --frontend pcode        # only the pcode/C cases
cargo xtask regression --filter ArrayFlow      # only cases whose name contains this
```

`--frontend` takes `pcode`, `jvm`, `dex`, `c`, `lua`, or `jni` (comma-separated, or
repeated) and defaults to all of them. It selects *before* anything runs, so
`--frontend pcode` never invokes the Java toolchain, `--frontend jvm,dex` never
starts Ghidra, and `--frontend lua` needs no external toolchain at all. `jni` is
the odd one out: its cases build both halves of a JNI boundary, so they need the
Java toolchain *and* Ghidra. Use it with `--filter` to narrow further:
`--frontend pcode --filter funcptr`.

> **Note:** the `lua` frontend lowers its `tests/lua/` cases end to end, including
> table field-sensitivity, varargs, `ipairs`/`pairs` and `table.insert`, and
> metatable-based OOP (method calls resolve through the recovered `__index` hierarchy;
> instance fields flow across calls). All of them are expected to pass — there are no Lua
> XFAILs. `closure-flow` (a closure returned out of one function and called in another)
> was one until the engine gained return-direction propagation of call-target objects;
> `tests/c/funcptrfactory.c` is the language-neutral regression guard on that rule.

Under the hood both paths invoke the `xtask` task runner (`xtask/src/`) over the
cases. The flake check above remains the canonical path and the one used in CI.

## How discovery works
A test case = a source file paired with its config. Drop the two files in and
they are picked up automatically; no list to edit.

- **Java / DEX** — `tests/java/Foo.java` pairs with `tests/java/foo.json`, where
  the config name is the kebab-case of the class (`ArrayListIteratorFlow` →
  `array-list-iterator-flow.json`). A `.java` with no matching config is ignored.
- **Pcode / C** — `tests/c/foo.c` pairs with `tests/c/foo-query.json`, or falls
  back to a shared `tests/c/query.json`.The pair registers under *both* C
  frontends: a `pcode` case named `foo` and a tree-sitter `c` case named `C:foo`.

- **Lua** — `tests/lua/foo.lua` pairs with `tests/lua/foo-query.json`. A `.lua`
  with no matching config is ignored. Cases report as `Lua:foo`. Because Lua is a
  source-level frontend, SARIF regions carry source lines directly, so
  `expected_lines` are checked against the code-flow `startLine`s with no
  compilation, linemap, or disassembler in the loop.
- **JNI** — `tests/jni/Foo.java` pairs with its sibling `tests/jni/Foo.c` and the
  kebab-cased `tests/jni/foo.json`. All three are required. Cases report as
  `Jni:Foo`. This is the only two-import case kind: the `.java` is imported as a
  DEX, the `.c` as a pcode shared library, and the two are co-indexed as one
  project so the JNI bridge has both halves to join.

  Each pair also yields two packaging variants, which make exactly the same
  claims — so the set is a direct A/B on the importer alone:

  - `Jni:Foo+apk` packages both halves into one APK and imports it once, the way
    an ordinary Android app ships. It passes only if the APK importer finds,
    extracts, and disassembles `lib/<abi>` for itself.
  - `Jni:Foo+split-apks` packages each half into an APK of its own and imports
    both, the way an app bundle (or an XAPK download) ships. The native APK has
    no `classes*.dex` in it at all.

  A sibling `foo.bridge.jsonl` adds a third variant, `Jni:Foo+bridge`, which
  joins the boundary with a declarative model under `--no-jni-bridge` instead of
  the built-in pass.

## Checks that are not taint cases

Not everything the suite reports comes from `tests/`. Four families are written in Rust in the
`xtask` crate and folded into the same report, because they are expensive, or need a toolchain, or
both — the same reasons the taint cases are nightly:

- **`dex:*`** — `dex-reader` over the compiled samples (parse, line map, UTF-8 constants) and
  against `baksmali` as ground truth, plus `dex:apk`, which parses every `classes*.dex` in the
  real-world APK. Needs `javac`/`dx` for the sample-derived checks; `dex:apk` does not.
- **`jvm:*`** — `jvm-reader` over the same samples compiled to `.class`/`.jar`. Needs `javac`.
- **`apk:*`** — `ctadl` itself, end to end, over that same real-world APK
  (`xtask/tests/dex/com.noto_54.apk`). Needs no toolchain at all: the APK is prebuilt and checked
  in, so all these need is the `ctadl` binary.
- **`models:*`** — the model files ctadl ships, validated against the model generator schema it
  publishes. Reads three small files and runs no tool.

The `apk:*` checks are where the analyzer meets a real app rather than a fixture: 6.4 MB, two
`classes*.dex`, some 50,000 functions. Importing it costs about 13 seconds, which is why they live
here — they used to be `#[test]`s in `ctadl-ascent/tests/cli.rs`, where they accounted for ~60 s of
the ~77 s the workspace's tests spent executing, and moving them out halved `cargo test
--workspace`. The import is done **once** and the four checks read the store it wrote:

| Check | What it claims |
| --- | --- |
| `apk:import` | The app imports, the config records the language, format version, artifact path and content hash, and `ctadl inspect` decodes the stored program and reports functions in it. |
| `apk:no-native-libs` | This APK carries no `lib/<abi>` entries, so the native-library pass records no sub-imports and stages nothing — the path that must not need Ghidra. |
| `apk:model-check` | `ctadl query` against an import that was never indexed exits non-zero, reports which imports it checked and what the generator selected, and writes **nothing** into the store. |
| `apk:skip-existing` | `--skip-existing` skips a re-import of an unchanged artifact, and only of an unchanged one: falsify the recorded hash and the same command re-imports. |

They select with `--frontend dex` (the app is a Dex artifact) and, like `dex:apk`, self-skip when
no APK resolves. `cargo xtask regression --frontend dex --filter apk:` runs just these, and needs
nothing on `PATH` beyond a Rust toolchain to build `ctadl`.

Because these drive the shipped binary rather than the library, they cover what a library-level
test cannot: argument wiring in `main.rs`, exit statuses, and the on-disk shape of the store. The
store paths they assert on are spelled out in `xtask/src/apk.rs` rather than imported from
`ctadl-ascent`, deliberately — a layout change should fail them loudly rather than follow along.

## Adding a Java/DEX test

1. Write `tests/java/Foo.java` with a `source()` and a `sink(...)` (see the
   existing cases). Keep it self-contained; co-located/inner classes are fine.
2. Write `tests/java/foo.json`:

   ```json
   {
     "expected_lines": [20, 21, 22],
     "model_generators": [
       { "find": "methods",
         "where": [{ "constraint": "signature_match", "name": "source", "parent": "LFoo;" }],
         "model": { "sources": [{ "kind": "UserInput", "port": "Return" }] } },
       { "find": "methods",
         "where": [{ "constraint": "signature_match", "name": "sink", "parent": "LFoo;" }],
         "model": { "sinks": [{ "kind": "TaintedData", "port": "Argument(0)" }] } }
     ]
   }
   ```

   `model_generators` is the query model passed to `ctadl query -m`.
   `expected_lines` is the known answer.

**Pass criterion (DEX):** the runner compiles to DEX, analyzes, and maps each
reported byte offset back to a source line via the dex linemap. A positive test
**passes if at least one** `expected_lines` entry is among the mapped lines. An
**empty** `expected_lines` makes it a *negative* test: it passes only if **no**
flow is reported (see `Reassignment.java` / `reassignment.json`).

## Adding a pcode/C test

1. Write `tests/c/foo.c` with functions matching the query's `source`/`sink`
   signature patterns.
2. Add `expected_lines` + `model_generators` to `tests/c/query.json` (or a
   dedicated `tests/c/foo-query.json`).

The same two files also drive the tree-sitter `c` case; nothing extra is needed
to add one.

**Pass criterion (pcode):** addresses are mapped to lines with `addr2line`
(after subtracting Ghidra's `0x100000` base). The test **passes only if every**
`expected_lines` entry is found. On macOS, if Ghidra reports no tainted
instructions the case is **skipped** (cross-platform decompiler differences);
the strict check runs on Linux/CI.

**Pass criterion (c):** the tree-sitter frontend parses the source directly and
emits real source spans, so the reached lines are read straight off the SARIF
code-flow regions — no compiler, no Ghidra, no `addr2line`, and no macOS skip
(it runs identically everywhere). Like pcode, the case **passes only if every**
`expected_lines` entry is among the reached lines, a code flow connects a source
to a sink, and no `unexpected_lines` entry is reached. The `c` frontend is still
maturing, so a short quarantine list in `xtask/src/regression.rs`
(`C_KNOWN_FAILURES`) reports its current gaps as **XFAIL** instead of failing the
suite; every other `C:` case is enforced. Remove an entry once its case passes so
a later regression is caught. Two cases are quarantined today:

- `C:ptrarith` — a *subscript* now lowers to an offset address, so `arr[2]` and
  `&arr[2]` compose, but the binary `+` in `*(p + 2)` does not yet, so taint
  stored that way is not resolved back to the aliased array slot.
- `C:defaultmodels` — a known-answer test for the *shipped* propagation defaults,
  which only a Native (pcode) import loads. Its chain lives in
  `native-index.jsonl`, and a `-l c` import has no method table, so it gets no
  default model file. The Pcode twin (`defaultmodels`) is what enforces those
  defaults, and it is enforced.

## Adding a JNI test

1. Write `tests/jni/Foo.java` with a `source()`, a `sink(...)`, and one or more
   `native` methods, plus `tests/jni/Foo.c` implementing them under their mangled
   `Java_…` names. Declare the JNI types locally (`typedef void *jstring;` and
   friends) rather than including `<jni.h>`: the flake ships no NDK, and only the
   arity and the dataflow shape matter.
2. Write `tests/jni/foo.json` exactly as a Java/DEX config. `expected_lines` and
   `unexpected_lines` there refer to the **Java** source; the optional
   `expected_native_lines` refers to the **C** source (see below).

**Pass criterion (JNI):** the DEX criterion above — a code flow connecting a
source to a sink, plus `expected_lines` and `unexpected_lines` through the dex
linemap — with one difference: `unexpected_lines` is checked against the lines the
code flows actually visited, not the wider set that includes the machine profile.
For these cases the connected flow *is* the assertion that the bridge fired, since
the Java half alone cannot carry the taint from the source to the sink. There is
no macOS self-skip: every criterion, native lines included, is satisfied on Darwin
today.

`expected_native_lines` is the same known-answer claim on the far side of the
boundary: the addresses reported in the shared library are mapped back with
`addr2line` (exactly as a pcode case's are) and must cover every line listed. It
says the taint is where it should be *in the artifact it should be in*, which the
Java-side claims cannot: those hold as soon as a flow exists at all.

Note what is nameable there. CTADL reports a tainted **instruction** at a call
whose argument is tainted, so only a native *call site* can appear. A body that
writes the tainted value straight into a global carries the flow just as well but
contributes no located result, which is why both cases' C halves pass the value
through a small `keep()` helper: the call is the line the case asserts on.

Shape the case so no per-function propagation model could fake it. `JniFlow` is
the worked example: the taint enters one native function, survives in a native
global, and leaves through a *different* one, so nothing short of a real link
across the boundary produces the answer. `JniArgShift` pins the argument shift
itself — the same instance native is called twice, once with the taint in the
argument the implementation returns and once with it in the argument the
implementation drops, so an off-by-one in the port map flips both assertions.

## Asserting that a line stays clean

Any case, in any frontend, may add an optional `unexpected_lines` array naming
source lines that must carry **no** flow:

```json
{
  "expected_lines": [26, 31],
  "unexpected_lines": [30],
  "model_generators": [ ... ]
}
```

The case fails if a flow reaches any line listed there, and the key may be
omitted entirely by cases that make no such claim.

Use it whenever the point of a case is precision rather than reachability --
that taint stopped where it should have. `expected_lines` alone cannot express
this: neither pass criterion above objects to *extra* lines being tainted, so a
case that merely leaves the clean line unlisted keeps passing if that line later
becomes tainted. `globalstruct.c` is the worked example: it writes `g_pair.a`
and sinks both fields, so the sink on the untouched `g_pair.b` is the whole
point of the test and is named in `unexpected_lines`.

An **empty** `expected_lines` (the DEX/JVM negative test) already asserts that
nothing flows at all, which subsumes any `unexpected_lines`.

## SARIF validation

Every case also validates the SARIF it emitted with `checksarif`
(`nix/sarif-multitool/checksarif.nix`), which checks a file against the SARIF
2.1.0 schema and the SARIF Multitool's rule set. A file that draws any `error`
or `warning` diagnostic fails its case whatever the taint answer was: a log a
consumer cannot read is a defect on its own. The report lists every diagnostic
and the path to the validation log, which is left in the case's scratch
directory as `checksarif-log.sarif`.

This is the one check the JVM allowlist cannot demote to XFAIL. The allowlist
exists for the maturity of that frontend's *taint results*, which the shape of
the log says nothing about.

Rules are configured in `nix/sarif-multitool/sarif-validation.xml`. A rule that
cannot apply to a binary/bytecode analyzer belongs there, turned off with a
comment saying why -- `SARIF2017` is the worked example: it asks every result
location for a `region.startLine`, and results here point into a `.jar`/`.dex`
by byte offset or into an executable by address. Keeping the exceptions there
rather than in `xtask` keeps "checksarif says nothing" as the pass criterion,
whether it is the suite running it or a person.

`checksarif` is a .NET tool that only the Nix environments supply; both paths
under [Running](#running) have it. A suite run from a shell without it prints a
warning and skips the validation rather than failing every case.

## Notes

- Each case runs in its own scratch directory under `$TMPDIR`; nothing is
  written back into `tests/`.
- Cases are independent — one failure does not abort the rest; every case is run
  and the final line reports `N passed, M skipped, K failed`.
- The runner expects its tools (`ctadl`, `dex-reader`, `javac`, `dx`, `gcc`,
  `addr2line`, Ghidra, `checksarif`) on `PATH`. The flake check in [Running](#running) builds
  and supplies all of them and `nix develop .#regression` provides an
  interactive shell with the same toolchain for local iteration.
- `scripts/*.py` are unused by the current suite and kept only for reference.
