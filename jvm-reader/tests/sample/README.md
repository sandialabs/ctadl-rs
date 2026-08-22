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

- **jvm-reader's own `flow.rs` unit tests** load compiled classes at runtime
  from the directory named by `JVM_READER_TEST_FIXTURES`. The `jvm-reader-tests`
  check (flake.nix) compiles them from these sources and points the env var at
  them. Those tests are `#[ignore]`d, so only a run with both the fixtures and
  `--include-ignored` exercises them; the pure opcode-table and
  parameter-slot-map tests alongside them need no fixture and run under a plain
  `cargo test`.

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

### Decoder regression fixtures

One per root cause from the JVM-frontend defect report. Each reproduces a
decoder bug on ordinary, verifiable major-version-52 bytecode straight out of
`javac`, so a failure is never a question of malformed input.

- **SparseSwitch.java** – sparse integer switch → `lookupswitch`, with the
  default arm as a back edge to the loop header. The selector must be consumed;
  otherwise it survives as a phantom slot and the join sees height 1 where the
  StackMapTable says 0.
- **DenseSwitch.java** – the same shape with dense case values, so `javac`
  emits `tableswitch`.
- **StringSwitch.java** – a Java 8 string switch, which lowers to *both*
  instructions (`lookupswitch` on `hashCode`, then `tableswitch`), joining at
  the default arm with two phantom slots.
- **GuardedStringSwitch.java** – the same, wrapped in `try`/`catch`, so the join
  is also an exception-handler edge: the handler pops its exception object and
  arrives with height 0 against the normal path's 2.
- **IushrLength.java** – `iushr` followed by meaningful one-byte instructions,
  plus the full `ishl`/`lshl`/`ishr`/`lshr`/`iushr`/`lushr` set. Covers both a
  wrong instruction length (which desynchronizes the rest of the method) and
  the shift stack effects, which alternate int/long and so cannot be assigned
  by opcode range.
- **WideParams.java** – `long` and `double` parameters in leading, middle and
  trailing positions, on static and instance methods. Wide parameters take two
  local slots but one ordinal, so a decoder that reports the slot as
  `Location::Parameter` names parameters that do not exist.

The modified-UTF-8 fixtures live in `../sample-jvm-only/` instead; see the
README there for why.

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
javac -encoding UTF-8 -d /tmp/fixtures \
  jvm-reader/tests/sample/{HelloWorld,ArrayFlow,LoopFlow}.java \
  jvm-reader/tests/sample/{SparseSwitch,DenseSwitch,StringSwitch,GuardedStringSwitch}.java \
  jvm-reader/tests/sample/{IushrLength,WideParams}.java \
  jvm-reader/tests/sample-jvm-only/{PairedOnly,SurrogateConstants}.java
JVM_READER_TEST_FIXTURES=/tmp/fixtures \
  cargo test --manifest-path jvm-reader/Cargo.toml -- --include-ignored
```

The list is spelled out in `jvmTestFixtures` in `flake.nix`; keep the two in
step when adding a fixture a `flow.rs` test loads.
