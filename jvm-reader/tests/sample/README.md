# jvm-reader sample sources

These `.java` files are the **only** committed test inputs for jvm-reader — no
`.class` or `.jar` files are checked in. They are compiled from source at test
time:

- **The nightly regression suite** (`cargo xtask regression`, run in CI via
  `nix build .#checks.<system>.regression`) compiles each sample with `javac`,
  exercises jvm-reader on the resulting `.class` files (disassembly compared
  against `javap`, plus line-map / basic-block / stack-slot analyses), then
  bundles them with `jar` and re-checks the `.jar` (parsed classes compared
  against `jar tf`). See `xtask/src/jvm.rs`.

- **jvm-reader's own `flow.rs` unit tests** load two of these compiled classes
  (`HelloWorld.class`, `ArrayFlow.class`) at runtime from the directory named by
  `JVM_READER_TEST_FIXTURES`. The `jvm-reader-tests` check (flake.nix) compiles
  them from these sources and points the env var at them.

## The samples

- **HelloWorld.java** – baseline dataflow plus a few call shapes.
- **ControlFlowMaze.java** – branch joins, loops, nested control flow.
- **InvokeShapes.java** – interface/default/virtual/static calls and long/double
  slot behavior (compiles to `InvokeShapes` + `ShapeOps`).
- **ArrayFlow.java** – array load/store for `ArrayElement` stack-slot tests.
- **SourceSinkExample.java** – simple source→intermediate→sink chain.

To run jvm-reader's `flow.rs` tests locally outside Nix, compile the samples and
point the env var at them:

```bash
javac -d /tmp/fixtures jvm-reader/tests/sample/HelloWorld.java jvm-reader/tests/sample/ArrayFlow.java
JVM_READER_TEST_FIXTURES=/tmp/fixtures \
  cargo test --manifest-path jvm-reader/Cargo.toml -- --include-ignored
```
