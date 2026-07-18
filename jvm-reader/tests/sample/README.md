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

- **jvm-reader's own `flow.rs` unit tests** load compiled classes
  (`HelloWorld.class`, `ArrayFlow.class`, `LoopFlow.class`) at runtime from the directory named by
  `JVM_READER_TEST_FIXTURES`. The `jvm-reader-tests` check (flake.nix) compiles
  them from these sources and points the env var at them.

## The samples

- **HelloWorld.java** – baseline dataflow plus a few call shapes.
- **ControlFlowMaze.java** – branch joins, loops, nested control flow.
- **InvokeShapes.java** – interface/default/virtual/static calls and long/double
  slot behavior (compiles to `InvokeShapes` + `ShapeOps`).
- **ArrayFlow.java** – array load/store for `ArrayElement` stack-slot tests.
- **LoopFlow.java** – loop with string concat (`iinc`, `invokedynamic`); stack normalization in `main`.
- **SourceSinkExample.java** – simple source→intermediate→sink chain.
- **Factorial.java** – recursion and a counted loop (`invokestatic` self,
  `imul`/`lmul`, `i2l`, forward/back branches).
- **Numerics.java** – `long`/`float`/`double` arithmetic and numeric
  casts/compares (`ladd`, `dmul`, `lcmp`, `i2l`, `l2f`, `f2d`, `d2i`, ...), the
  opcode families the int-only samples never reach.
- **InnerClasses.java** – a static nested class: two `.class` files, an
  InnerClasses attribute, and object construction (`new`/`dup`/`invokespecial`,
  `putfield`/`getfield`).

> These samples also feed the **dex-reader** checks (`xtask/src/dex.rs`): each is
> compiled down to `.dex` and parsed, line-mapped, and diffed against baksmali.
>
> The last three revive part of the old `classfile-parser` `.class` corpus that
> was dropped when binaries were removed, as source. Members that can't be (or
> aren't worth) expressing as source were left out on purpose: `malformed.class`
> (a deliberately corrupt file — not producible by `javac`), and
> `module-info` / `UnicodeStrings` / annotation-only fixtures (the original
> `javap -c` comparison already skipped these, and `javap -c` doesn't render
> annotations anyway).

To run jvm-reader's `flow.rs` tests locally outside Nix, compile the samples and
point the env var at them:

```bash
javac -d /tmp/fixtures jvm-reader/tests/sample/HelloWorld.java jvm-reader/tests/sample/ArrayFlow.java jvm-reader/tests/sample/LoopFlow.java
JVM_READER_TEST_FIXTURES=/tmp/fixtures \
  cargo test --manifest-path jvm-reader/Cargo.toml -- --include-ignored
```
