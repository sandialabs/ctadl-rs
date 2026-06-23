# Nightly regression suite

Source-sink taint regression tests. Each case is a small program plus a JSON
file holding its **known answer** (the source lines a flow is expected to reach)
and the ctadl query model. The orchestration lives in the `xtask` crate at the
repo root (`xtask/src/`), not in shell scripts.

## Running

```sh
# from the repo root, inside the Nix dev environment
cargo xtask regression
cargo xtask regression --filter ArrayFlow      # only cases whose name contains this
```

In CI this is driven by the flake check (which provides ctadl, dex-reader,
javac, dx, Ghidra, gcc/addr2line):

```sh
nix build .#checks.x86_64-linux.regression
```

The nightly GitHub workflow runs that check on a schedule. Add expensive tests
here, not in YAML.

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

## Notes

- Each case runs in its own scratch directory under `$TMPDIR`; nothing is
  written back into `tests/`.
- Cases are independent — one failure does not abort the rest; every case is run
  and the final line reports `N passed, M skipped, K failed`.
- The runner expects its tools (`ctadl`, `dex-reader`, `javac`, `dx`, `gcc`,
  `addr2line`, Ghidra) on `PATH`; the Nix check supplies them. Outside Nix, run
  from within the project's dev environment.
- `scripts/*.py` are unused by the current suite and kept only for reference.
