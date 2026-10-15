# dex-reader real-world APK fixture

`com.noto_54.apk` is a real third-party Android application, kept here as a
robustness fixture for the dex-reader regression checks (`cargo xtask
regression`, the `dex:apk` case). It exercises dex-reader against a large,
multi-`classes*.dex` app that no synthetic sample reproduces.

## Why this is a committed binary

The project convention is that test ground truth lives in source that gets
compiled at test time — no committed `.class`/`.jar`/`.dex`. This APK is the
deliberate exception: it is third-party input we cannot rebuild from any source
we hold, so it can only exist as a binary. The *source-based* DEX coverage comes
from the `dex:samples` / `dex:line-map` cases, which compile
`readers/jvm-reader/tests/sample/*.java` down to `.dex` (`javac --release 8` → `dx`) and
parse the result — see `xtask/src/dex.rs`.

## Ownership

xtask owns this fixture. The regression harness finds it automatically at
`xtask/tests/dex/com.noto_54.apk`, or via `cargo xtask regression --dex-apk
<path>`. The Nix `regression` check passes the path explicitly. Nothing else reads it:
the `ctadl-ascent/tests/cli.rs` import smoke test that once did has moved here as
the `apk:*` checks, which is why that file's own rule -- synthetic input, temp
store, milliseconds -- holds without exception.

# `all-opcodes.smali`

Every Dalvik opcode in dex 039, with operands chosen to catch decoder bugs:
distinct registers in each field, the largest register each field can hold,
extreme literals, backward and forward branches, and every payload kind. It is
source, not a binary: `dex:dexlib2-decode` assembles it with `smali assemble
--api 28` at test time, then requires dex-reader to decode every instruction
exactly as dexlib2 does (see `xtask/src/dexlib2.rs`). The check fails if the
file stops covering an opcode. `dex:baksmali-all-opcodes` also compares
dex-reader's smali disassembly of it with baksmali's. It is never run, so it need not pass the
verifier.

# `opcodes-art.txt`

ART's observations from the last passing `cargo xtask dex-opcodes --save` run:
for each case, which registers the instruction wrote, which ones changed its
result, and what it threw, as seen on an emulator. Like the APK, it cannot be
rebuilt from source here, since producing it needs a device. The
`dex:opcodes-replay` check judges dex-reader against it (see
`xtask/src/dex_opcodes/saved.rs` and `docs/dex-validation.md`). Regenerate it
with `--save` after changing a case, `H.java` or `Driver.java`. A unit test
fails until you do.
