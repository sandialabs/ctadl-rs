# jvm-reader sample sources

These `.java` files are the **only** committed test inputs for jvm-reader — no
`.class` or `.jar` files are checked in. **The nightly regression suite**
(`cargo xtask regression`, run in CI via
`nix build .#checks.<system>.regression`) compiles each sample with `javac`,
exercises jvm-reader on the resulting `.class` files (disassembly compared
against `javap`, plus line-map / basic-block / stack-slot analyses, and the
`jvm:switch-shapes` / `jvm:utf8-constants` checks), then bundles them with `jar`
and re-checks the `.jar` (parsed classes compared against `jar tf`). See
`xtask/src/jvm.rs`. `xtask/src/dex.rs` compiles the same sources down to `.dex`.

Nothing else compiles them. `jvm-reader`'s own unit tests are hermetic — they
build the two-entry constant pool they need in Rust — so a plain `cargo test`
covers them and needs no JDK.

**These are reader-level fixtures, not taint cases.** They exist to give the
parser, disassembler and CFG builder bytecode shapes to chew on. Whether taint
*flows* correctly through those shapes is asserted end to end by the regression
cases in `nightly/tests/java` — `SwitchFlow`, `StringSwitchFlow`,
`WideParamFlow` and `ShiftFlow` cover the same constructs in both the JVM and
DEX frontends. Add a taint case there when the question is "does the analysis
get the right answer"; add a sample here when the question is "does the reader
survive this bytecode and describe it faithfully".

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

Both `xtask/src/jvm.rs` and `xtask/src/dex.rs` glob these directories rather
than naming files, so a new `.java` dropped in either one is picked up with
nothing to keep in step. Adding a fixture only needs a line in the list above.
