# CTADL JVM frontend: defect confirmation and fix plan

> **Implemented.** Every defect below is fixed and covered by tests; see
> `JVM_FRONTEND_FIX_SUMMARY.md` for what landed, including the two further
> defects the new fixtures exposed in the disassembler and the `xtask` CFG
> check. The `repro/` fixtures moved into `jvm-reader/tests/`. The text below is
> kept as the original analysis.

Status: all defects in `bug1.md` reproduced locally from first principles, plus
two additional defects the report did not cover. Nothing in the tree has been
modified — `git status` is clean apart from the new `repro/` directory and this
file.

## How this was reproduced

The report reproduces against a retained Apktool checkout that is not present on
this machine. Instead, every defect is reproduced from purpose-written Java
compiled by `javac` with no post-processing, so there is no question of
malformed input: the fixtures are ordinary, verifiable major-version-52 (Java 8)
class files.

```
cargo build --bin ctadl
./repro/run.sh                    # compiles repro/src/*.java, imports each class
./repro/run.sh /path/to/ctadl     # or point at another binary
```

`run.sh` picks up a JDK 8 from the Nix store automatically
(`/nix/store/*zulu-ca-jdk-8*`); set `JAVA_HOME` to override. JDK 8 is used only
to keep the fixtures on major version 52, matching the reported case.

Fixtures, one per root cause:

| Fixture | Targets |
| --- | --- |
| `SparseSwitch.java` | `lookupswitch` selector not consumed |
| `DenseSwitch.java` | `tableswitch` selector not consumed |
| `StringSwitch.java` | Java 8 string switch: both selectors at once |
| `GuardedStringSwitch.java` | same, where the join is also an exception-handler edge |
| `IushrLength.java` | `iushr` stack effect and instruction length |
| `SurrogateConstants.java` | modified-UTF-8 constants with unpaired surrogates |
| `PairedOnly.java` | modified-UTF-8 constant with only a *well-formed* pair |
| `WideParams.java` | `long`/`double` parameter slot indexing |

## Confirmed defects

### 1. Switch instructions have no stack effect

`jvm-reader/src/flow.rs:1764` — `misc_stack_effect` models conditional branches
but omits `tableswitch` (0xaa) and `lookupswitch` (0xab). Both pop one int
selector. Switches decode to no dataflow and to `InstructionKind::Other`
(`flow.rs:1194`), so `misc_stack_effect` is the correct place for them.

The selector survives as a phantom slot, and the discrepancy surfaces at the
first join that the switch dominates:

```
SparseSwitch:  block 0 (pc 0)   <- block 6 (pc 108),  existing_len=0, new_len=1
DenseSwitch:   block 0 (pc 0)   <- block 7 (pc 113),  existing_len=0, new_len=1
StringSwitch:  block 13 (pc 143) <- block 11 (pc 139), existing_len=0, new_len=2
```

`SparseSwitch.parseTable` is a deliberate structural copy of
`BinaryResourceParser.parseTable`: a `while` loop whose `default` arm branches
back to the loop header, giving exactly the reported 0 → 1 discrepancy.
`StringSwitch.decode` reproduces the two-selector case — `javac` lowers a string
switch to `lookupswitch` on `hashCode()` at pc 8 followed by `tableswitch` at
pc 84, joining at `new StringBuilder` (pc 139), which matches
`ResFileDecoder.decode` instruction for instruction.

`GuardedStringSwitch` reproduces §3 of the report: the same two phantom slots
meeting an exception handler that pops its exception object. Locally the edge is
reported as `existing_len=0, new_len=2` rather than the report's `2 → 0`; the
orientation just depends on which predecessor the worklist visits first, so this
is the same defect, not a distinct one.

**Fix.** In `misc_stack_effect`, beside the conditional-branch arms:

```rust
// Switches consume their int selector.
0xaa | 0xab => (1, 0), // tableswitch, lookupswitch
```

**Verified.** With only this change, `SparseSwitch`, `DenseSwitch`,
`StringSwitch` and `GuardedStringSwitch` all import cleanly.

### 2. Shift stack effects are assigned by range and are off by one

`jvm-reader/src/flow.rs:1749-1750`:

```rust
0x78..=0x7a => (2, 1), // ishl, ishr, iushr     <- actually ishl, lshl, ishr
0x7b..=0x7d => (3, 2), // lshl, lshr, lushr     <- actually lshr, iushr, lushr
```

The int and long shifts alternate; they are not contiguous. The ranges therefore
give `lshl` (0x79) the int effect and `iushr` (0x7c) the long effect. The
comments describe the intended grouping, so this reads as a transcription slip
rather than a misunderstanding.

`iushr` claiming to pop 3 slots when 2 are present aborts before any join check:

```
stack underflow while rewriting StackInput: class=IushrLength
method=unpackLanguageOrRegion(ICZ)I pc=3 opcode=0x7c mnem=iushr depth=2 stack_len=2
```

**Fix.** Enumerate by opcode, exactly as the report recommends:

```rust
0x78 | 0x7a | 0x7c => (2, 1), // ishl, ishr, iushr
0x79 | 0x7b | 0x7d => (3, 2), // lshl, lshr, lushr
```

### 3. `iushr` is given two inline operand bytes

`jvm-reader/src/flow.rs:993` — `operand_byte_count` has `0x7c => 2`. `iushr` is
a one-byte instruction with no inline operands. `instruction_length` feeds
`decode_flow_instruction` (`flow.rs:1304`), which is the sole instruction
iterator, so a wrong length desynchronizes the entire remaining decode of the
method.

This is genuinely independent of defect 2, not a duplicate. With defect 2 fixed
in isolation, the same fixture fails differently:

```
stack underflow while rewriting StackInput: class=IushrLength
method=unpackLanguageOrRegion(ICZ)I pc=6 opcode=0x7e mnem=iand depth=1 stack_len=1
```

`iushr` sits at pc 3; length 3 advances the decoder to pc 6 (`iand`), skipping
`bipush 31` at pc 4. `iand` then finds one slot where it needs two.

**Fix.** Delete the `0x7c => 2` arm. Nothing else in the table needs to change;
`_ => 0` already gives the correct length for every other arithmetic opcode.

### 4. Modified UTF-8: no surrogate-pair recombination, and no representation for unpaired surrogates

`jvm-reader/src/parse_utils.rs:68-116`. The decoder collects each three-byte
sequence as an independent code unit and then maps `char::from_u32` over the
result at line 112.

The report frames this as an unpaired-surrogate problem. It is broader than
that: the decoder never recombines *well-formed* surrogate pairs either, so
**any** Java class containing a supplementary character in a string constant
fails. `PairedOnly.java` contains one emoji and nothing else:

```
$ ctadl import -l jvm -n paired PairedOnly.class
jvm decoding error
InvalidUtf8
```

That is a much larger blast radius than a hand-written lexer table — it is every
class with an emoji, a CJK extension character, or a supplementary symbol in a
literal. The class file encodes these as CESU-8 pairs (`ED A0 B8 ED B8 80`);
recombination is mandatory, not optional.

Unpaired surrogates are a second, harder problem. They are legal in the class
file (`ED A0 80` is `U+D800`) and are used deliberately as packed-table data.
Rust's `String` cannot hold them at all, so this cannot be fixed inside the
current return type.

**Fix, staged.**

*Stage A (small, unblocks the common case).* Recombine well-formed
high/low surrogate pairs into a single scalar before `char::from_u32`. Handles
every ordinary class. Leave unpaired surrogates erroring for now — but with a
better error than `InvalidUtf8` (see defect 6).

*Stage B (correct, larger).* Give `CpEntry::Utf8` a representation that can hold
arbitrary UTF-16 code units. The recommendation is a `JvmString` newtype owning
the raw modified-UTF-8 bytes, with:

- `as_str(&self) -> Option<&str>` for the overwhelmingly common well-formed case,
- `to_string_lossy(&self) -> Cow<'_, str>` substituting `U+FFFD` for unpaired
  surrogates, for names, display and diagnostics,
- `code_units(&self) -> impl Iterator<Item = u16>` for exact data access.

Class, method, field and descriptor names can never legally contain unpaired
surrogates, so those call sites can keep using `&str` and treat a lossy result as
a hard error; only string *constants* need the lossy or code-unit path. Note
that merely pairing valid surrogates — the Stage A fix alone — will not handle
the deliberately unpaired values in `smaliFlexLexer`, exactly as the report says.

Decide between A-only and A+B before starting: Stage A is a contained change to
one function; Stage B touches `CpEntry`, `parser.rs`, and every consumer of
`get_utf8`.

### 5. NEW — `long`/`double` parameters are indexed in the wrong space

Not in `bug1.md`; found while validating the fixes above. It is fully
independent and reproduces on a pristine tree.

`jvm-reader/src/flow.rs:1213` builds `Location::Parameter(slot)` from the **local
variable slot index**. `ctadl-ascent/src/languages/jvm/mod.rs:35`
(`jvm_descriptor_to_params`) builds the function's parameter list with one entry
per **declared parameter**. `long` and `double` occupy two local slots, so the
two index spaces diverge the moment a method takes a wide parameter:

```
$ ctadl import -l jvm -n wideparams WideParams.class
IR verify error
  > in function: LWideParams;->onlyLong(JIZ)J: reference to nonexistent parameter: '3'
  > in function: LWideParams;->withDouble(DI)D: reference to nonexistent parameter: '2'
```

For `static long onlyLong(long v, int n, boolean flag)`: `v` occupies slots 0-1,
`n` slot 2, `flag` slot 3 — but only ordinals 0, 1, 2 exist. `noWide` in the same
class, with no wide parameter, verifies fine.

**Why this matters for the Apktool case.** Defects 1-3 abort those methods before
the IR verifier ever sees them. Fixing the decoder unmasks this one: it is what
`IushrLength.java` hits once defects 1-3 are patched. Any plan that stops after
the decoder fixes will simply trade one Apktool failure for another.

**Fix.** Map slot → parameter ordinal at the point `Location::Parameter` is
constructed. `descriptor_parameter_info` already carries `slot_width`, so build a
`Vec<u16>` slot-to-ordinal table once per method (accounting for the implicit
`this` slot on instance methods) and index it in `local_slot_to_location`. A slot
that lands in the *second* half of a wide parameter should map to that
parameter's ordinal, not to the next one.

`local_slot_to_location` currently takes only `param_slot_count`, so this changes
its signature and those of its callers within `flow.rs`.

### 6. Errors carry no context

Three separate gaps, all cheap:

- `jvm-reader/src/jar.rs:38` and `:59` — `ClassFileParser::parse(&data)?`
  propagates without attaching `entry.name()`. The whole-JAR failure is therefore
  just `InvalidUtf8` with no indication of which entry failed, confirmed against
  `repro.jar`.
- `InvalidUtf8` (`error.rs:11`) is a unit variant. It should carry the
  constant-pool index and the offending byte offset or code unit.
- Join and underflow errors already name class, method, pc and opcode via
  `InvalidClassFileMessage`; extend the same treatment to the parse-side errors
  so a failure identifies JAR entry, class, method descriptor, pc, opcode and
  predecessor edge.

### 7. `xtask` skips a variant that is no longer produced

`xtask/src/jvm.rs:459` matches:

```rust
Err(ClassFileError::InvalidClassFile("inconsistent operand stack height at basic-block join")) => { skipped_join_shape += 1; continue; }
```

`flow.rs:545` now returns `InvalidClassFileMessage(format!("inconsistent operand
stack height at basic-block join: block {} (pc {}) <- ...", ...))`. The literal
`&'static str` match cannot fire, so what was meant to be a counted skip becomes
a `bail!`.

**Fix.** Match on `InvalidClassFileMessage(msg)` with
`msg.starts_with("inconsistent operand stack height at basic-block join")`. Better
still, add a dedicated `ClassFileError::StackHeightMismatch { .. }` variant with
structured fields so neither the message text nor a prefix match is load-bearing;
that also gives defect 6 somewhere to put its context.

## Suggested ordering

The decoder defects mask each other and mask defect 5, so land them in this
order and re-run `repro/run.sh` after each step.

1. **Defect 7** first — it is one line and it restores `xtask`'s ability to
   report on the rest without bailing.
2. **Defects 1, 2, 3** together, as one "JVM opcode tables" change. They are
   three small table edits in `flow.rs` with a shared test surface.
3. **Defect 5** immediately after — step 2 unmasks it, and leaving the pair
   split means the Apktool classes still fail.
4. **Defect 4 Stage A** — self-contained, and it fixes far more classes than the
   Apktool case alone.
5. **Defect 6** — do it alongside step 4 if `InvalidUtf8` is gaining fields
   anyway.
6. **Defect 4 Stage B** — separate change, separate review.

## Regression suite

Add as fixtures under `jvm-reader/tests/sample/` (the existing tests load
compiled classes via `JVM_READER_TEST_FIXTURES`, built by the `jvm-reader-tests`
check in `flake.nix`, with no `.class` files committed — `repro/src/*.java` can
move there directly):

- Sparse integer switch → `lookupswitch`, with a back-edge join.
- Dense integer switch → `tableswitch`, with a back-edge join.
- Java 8 string switch → both instructions, joining at the default arm.
- The same string switch wrapped in `try`/`catch`, so the join is an
  exception-handler edge.
- `iushr` followed by meaningful one-byte instructions, plus the full
  `ishl`/`lshl`/`ishr`/`lshr`/`iushr`/`lushr` set.
- `long` and `double` parameters in leading, middle and trailing positions, on
  both static and instance methods.
- Modified-UTF-8 constants: paired surrogates, unpaired high, unpaired low, and a
  packed table mixing them.

Two unit tests worth adding directly, independent of any fixture, since both
defects are pure table lookups:

- `instruction_length` returns 1 for every opcode in `0x60..=0x83`.
- `stack_effect` returns `(2, 1)` for `0x78`, `0x7a`, `0x7c` and `(3, 2)` for
  `0x79`, `0x7b`, `0x7d`.

If the Apktool checkout is restored, add the ordinary and R8 builds of
`BinaryResourceParser` and `ResFileDecoder` as end-to-end fixtures. Until then,
the fixtures above cover every root cause the report identified.

## What is not addressed here

Nothing has been fixed; this is confirmation and plan only. The `repro/`
directory is self-contained and can be deleted, or moved into
`jvm-reader/tests/sample/` as the starting point for the regression suite.
