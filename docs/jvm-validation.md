# Validating `jvm-reader`'s instruction semantics - DO-NOT-MERGE

This is the JVM counterpart of [`dex-validation.md`](dex-validation.md).
`jvm-reader` hand-writes what each opcode does to the operand stack, the locals
and the heap (`decode_dataflow`, `decode_call`, `misc_stack_effect` in
`readers/jvm-reader/src/flow.rs`). [`dex-jvm-reader-improvements.md`](dex-jvm-reader-improvements.md)
§3 records bugs in it that were found by reading code. The checks here compare
it with independent sources instead.

| | ASM cross-check | Per-opcode runtime harness |
| --- | --- | --- |
| Oracle | ASM's analyzer (`Frame.execute`) | HotSpot running each opcode |
| Checks | stack effect, operand positions, `dup`/`swap` permutations, locals, field and array places, call operands, constants | which inputs each output actually depends on |
| Needs | a JDK and ASM | a JDK and ASM |
| Runs in `cargo xtask regression` | yes, as `jvm:asm-flow` | yes, as `jvm:opcodes` |
| Time | a few seconds | about a second for 214 cases |

Unlike Dex, the JVM needs no emulator: the JDK in the dev shell runs the
bytecode itself. ASM comes from Maven Central through the flake
(`asmClasspath` in `flake.nix`), which puts an `asm-classpath` script on `PATH`.
`ASM_CLASSPATH` overrides it.

---

## 1. ASM cross-check (`jvm:asm-flow`)

`xtask/src/asm_flow/`. For every instruction, `AsmFlow.java` takes the frame
ASM's analyzer computes before it, refills it with one fresh atom per input
(`in<p>` for stack slot `p`, `local<n>` for local `n`), and runs the
instruction over it with ASM's `Frame.execute`. That gives, per instruction:

- the stack height before and after;
- what each stack slot it writes holds afterwards, so `dup_x1` on `[in0, in1]`
  is `out0=in1 out1=in0 out2=in1`;
- each local it writes;
- each field or array element it stores into, and from which slots the
  object, array and index come;
- the receiver and each argument slot of a call.

The Rust side prints the same lines from jvm-reader's decoded dataflow, reading
it the way `ctadl-jvm` does (see the module comment), and the two dumps must
agree line for line. A category-2 value is two slots on both sides, and ASM
keeps the halves apart while a value is only moved, so `dup2` of a long is
checked at the slot level as well.

The inputs are:

- `AllOpcodes.java`, which builds a class holding every opcode with ASM's class
  writer: every form of `dup_x2`, `dup2_x1` and `dup2_x2`, `swap`, `jsr`/`ret`
  (plain and wide), `goto_w`, `jsr_w`, `ldc_w`, wide loads, stores and `iinc`,
  and `ldc` of every constant kind. javac emits few of these. The check fails if
  the class stops covering an opcode. It is analyzed, never loaded.
- the jvm-reader samples (`readers/jvm-reader/tests/sample`), as the other
  `jvm:*` checks compile them;
- the nightly Java cases (`nightly/tests/java`), compiled with `--release 8` as
  the JVM taint cases compile them.

Every dump and every disagreement is written under the run's scratch
directory; a failure names the file.

### What it found

Everything agrees except the following. They are listed in `asm_flow::KNOWN`,
so the check passes while they stand, and fails when one stops happening (the
bug was fixed and its entry must go).

| Opcode | jvm-reader | Correct | |
| --- | --- | --- | --- |
| `fconst_1`, `fconst_2`, `dconst_1` | `Float(1)`, `Float(2)`, `Double(1)`: the raw bits 1 and 2, tiny denormals | `0x3f800000`, `0x40000000`, `0x3ff0000000000000` | new; `const_for_const_op` casts the integer instead of converting it |

The samples and nightly cases trip only `dconst_1`.

It also found that `dup_x1`, `dup_x2`, `dup2`, `dup2_x1`, `dup2_x2` and `swap`
sent every input to every output, rather than following the JVMS §6.5
permutation (improvements §3.2 and §3.4). That was fixed in jvm-reader, and its
`KNOWN` entries were deleted. Likewise, `ldc` of a MethodType, MethodHandle or
dynamic constant made the whole method fail to decode (improvements §3.4). It
now pushes the constant, and the check fails if such a method stops decoding.
And `anewarray` of an array type allocated `[L[I;` rather than `[[I`, because
the decoder wrapped the operand in `L...;` even when it was already an array
descriptor. That was fixed too.

Everything else agrees: every load, store and `iinc` (short, plain and wide
forms, and stores into parameter slots), every arithmetic, conversion and
comparison instruction, every array load and store including `lastore` and
`dastore`, every field access including wide `putfield`, every call kind
including `invokedynamic` and `invokeinterface`, `jsr`/`ret`, branches and
switches.

To show that it catches errors, two old-style bugs were put back in by hand:
swapping `lastore`'s array and index slots, and making `putfield` of a `long`
store only one half of the value. The check failed on both, in the
all-opcodes class and in the samples.

### Limits

- **It checks positions, not policy.** Where jvm-reader's representation is a
  modeling choice, the oracle follows it: an array load reads the element (not
  the index), and `getfield` reads the object and the field. Whether those are
  the right taint rules is not something ASM can say.
- **It checks jvm-reader, not `ctadl-jvm`.** For example, the frontend took
  the `xreturn` value from the bottom of the stack (improvements §3.4), which
  was not visible here. Frontend lowering is covered by the nightly Java
  cases, which run on both the JVM and the Dex frontends.
- **Returns are not compared.** Only `xreturn`'s stack effect is checked, not
  the `return_value` slot jvm-reader gives it.

---

## 2. Per-opcode runtime harness (`jvm:opcodes`)

`xtask/src/jvm_opcodes/`. This is the JVM version of `cargo xtask dex-opcodes`
(see [`dex-validation.md`](dex-validation.md) §2), with HotSpot in place of ART.
It needs no device, so it runs live in every `cargo xtask regression`, and
there is no saved-observation file to keep in step.

`JvmOpcodes.java` builds one class per case with ASM. Its `run()` loads every
input from a helper class: eight int locals as padding, the case's own locals,
then the operand stack, starting with one padding int. Then it runs the one
instruction under test and hands every stack slot and local back, along with
anything the instruction stored into an input object, an array or a static
field, and what a callee received. ASM's analyzer supplies the types needed to
capture the stack afterwards, so the harness does not depend on jvm-reader's
idea of what the instruction left there.

Each case runs once with every input at its first candidate value, then once
per other candidate of each 32-bit input slot. That gives, for every place the
instruction wrote, the input slots that changed it. The places use the same
names as `jvm:asm-flow`. The Rust side reads jvm-reader's dataflow the same way
(`asm_flow::claims`) and judges:

| Finding | Verdict |
| --- | --- |
| an input changed a place, but jvm-reader does not list it as a source | FAIL |
| a place was written, but jvm-reader does not list it | FAIL |
| a listed source never changed its place, or a listed place was never written | REVIEW |

A run shows dependence only for the values it tries, so a REVIEW needs a
person. The expected ones, with reasons, are in `EXPECTED_REVIEWS`. A stale
expectation fails the check, as does an opcode that no case covers.

Some details of the comparison:

- A load from an array element or a field depends on the slots that hold the
  array and index, or the object. A store is keyed by the slot that holds the
  object or array; picking a different object does not count as changing what
  was stored.
- A long or double result is one value. Its two stack slots are compared
  together, so `ladd`'s low word is not required to depend on the high words.
  jvm-reader's sources for the two slots are merged too, so a `dup2` that
  copies a long half by half passes. `jvm:asm-flow` checks the half-by-half
  order. Wide inputs are still perturbed one half at a time.
- Division, remainder, comparison and the long logic ops start with equal
  operands, so that a change to either operand shows up in the result.

### Coverage

214 cases cover every opcode except branches, switches, returns, `goto`, `jsr`
and `ret`. Those move no data jvm-reader models, and `jvm:asm-flow` checks their
stack effect. The cases include the short, plain and wide forms of every load
and store, wide `iinc`, `ldc_w` (from a class whose constant pool is padded past
256 entries), every form of every `dup`, every array kind including boolean, and
all five call kinds.

### What it found

No FAILs: every input that changes an output is one jvm-reader lists as a
source, for every opcode. The expected REVIEWs are listed below. The `dup_x*`,
`dup2` and `swap` cases used to be on this list, because jvm-reader drew their
outputs from inputs that cannot reach them (improvements §3.2, §3.4). That was
fixed, and they now agree.

- `l2i`, whose result never depends on the high word, though the instruction
  pops it.
- `d2i` and `d2l`, where the candidates' low words hold only fractional bits.

Two hand-made bugs show that it fails when it should. Mapping `istore_<n>` to
local n+1 fails all four `istore_<n>` cases, and dropping `getfield`'s object
source fails the three `getfield` cases, and reversing the `swap` or `dup2`
permutation fails the `swap` and `dup2` cases (and `jvm:asm-flow`).

### Limits

- **Dependence is value-specific.** A source that matters only for values the
  candidates miss shows up as a REVIEW, not a FAIL.
- **Only what jvm-reader models.** Exceptions are not judged: jvm-reader has no
  `can_throw()` to check. Nor are branch conditions, since control dependence is
  not dataflow.
- **One instruction at a time, as in `jvm:asm-flow`.** `ctadl-jvm` is not
  checked.
