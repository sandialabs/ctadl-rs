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
| Checks | stack effect, operand positions, `dup`/`swap` permutations, locals, field and array places, call operands, constants | value dependence of each output on each input, exceptions |
| Needs | a JDK and ASM | a JDK and ASM |
| Runs in `cargo xtask regression` | yes, as `jvm:asm-flow` | yes |

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
| `dup_x1`, `dup_x2`, `dup2`, `dup2_x1`, `dup2_x2` | every input reaches every output; `dup2` also rewrites the two slots it copies | the JVMS §6.5 permutation | improvements §3.2, now confirmed for every form |
| `swap` | both inputs reach both outputs | `out0=in1 out1=in0` | improvements §3.4, confirmed |
| `fconst_1`, `fconst_2`, `dconst_1` | `Float(1)`, `Float(2)`, `Double(1)`: the raw bits 1 and 2, tiny denormals | `0x3f800000`, `0x40000000`, `0x3ff0000000000000` | new; `const_for_const_op` casts the integer instead of converting it |
| `anewarray` of an array type | allocates `[L[I;` | `[[I` | new; `decode_dataflow` always wraps the class in `L...;` |
| `ldc` of a MethodType, MethodHandle or dynamic constant | the whole method fails to decode | a constant | improvements §3.4, confirmed |

The samples and nightly cases trip only `dconst_1`. javac does not emit the
stack shuffles for them.

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
- **It checks jvm-reader, not `ctadl-jvm`.** For example, `stack_dup_statements`
  in the frontend links every `dup` output to every other output on its own
  (improvements §3.2), and the `xreturn` value is taken from the bottom of the
  stack (§3.4). Neither is visible here.
- **Returns are not compared.** jvm-reader does not model what `xreturn`
  returns; only its stack effect is checked.
