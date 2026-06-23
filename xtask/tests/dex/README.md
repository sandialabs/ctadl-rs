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
`jvm-reader/tests/sample/*.java` down to `.dex` (`javac --release 8` → `dx`) and
parse the result — see `xtask/src/dex.rs`.

## Ownership

xtask owns this fixture. The regression harness finds it automatically at
`xtask/tests/dex/com.noto_54.apk`, or via `cargo xtask regression --dex-apk
<path>`. The Nix `regression` check passes the path explicitly. One other
consumer, `ctadl-ascent/tests/cli.rs`, reads it by relative path for its import
smoke test.
