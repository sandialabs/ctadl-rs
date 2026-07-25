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

`--frontend` takes `pcode`, `jvm`, or `dex` (comma-separated, or repeated) and
defaults to all three. It selects *before* anything runs, so `--frontend pcode`
never invokes the Java toolchain, and `--frontend jvm,dex` never starts Ghidra.
Use it with `--filter` to narrow further: `--frontend pcode --filter funcptr`.

Under the hood both paths invoke the `xtask` task runner (`xtask/src/`) over the
cases. The flake check above remains the canonical path and the one used in CI.

## How discovery works
A test case = a source file paired with its config. Drop the two files in and
they are picked up automatically; no list to edit.

- **Java / DEX** — `tests/java/Foo.java` pairs with `tests/java/foo.json`, where
  the config name is the kebab-case of the class (`ArrayListIteratorFlow` →
  `array-list-iterator-flow.json`). A `.java` with no matching config is ignored.
- **Pcode / C** — `tests/c/foo.c` pairs with `tests/c/foo-query.json`, or falls
  back to a shared `tests/c/query.json`.

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

**Pass criterion (pcode):** addresses are mapped to lines with `addr2line`
(after subtracting Ghidra's `0x100000` base). The test **passes only if every**
`expected_lines` entry is found. On macOS, if Ghidra reports no tainted
instructions the case is **skipped** (cross-platform decompiler differences);
the strict check runs on Linux/CI.

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
