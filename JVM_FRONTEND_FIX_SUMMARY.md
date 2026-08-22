# CTADL JVM frontend: what was fixed

Implements `JVM_FRONTEND_FIX_PLAN.md` end to end — all seven defects it lists,
plus two more the new fixtures exposed. Every fixture in the plan's regression
suite is in the tree and passing, and the full regression run is green.

Nothing was committed; the changes sit in the working tree on `misc-bugfixes`.

## Before

Eight purpose-written Java classes, `javac`-compiled with no post-processing.
All eight failed to import, and the JAR of all eight failed without naming the
class it choked on:

```
DenseSwitch          inconsistent operand stack height at basic-block join: block 0 (pc 0) <- block 7 (pc 113), existing_len=0, new_len=1
SparseSwitch         inconsistent operand stack height at basic-block join: block 0 (pc 0) <- block 6 (pc 108), existing_len=0, new_len=1
StringSwitch         inconsistent operand stack height at basic-block join: block 13 (pc 143) <- block 11 (pc 139), existing_len=0, new_len=2
GuardedStringSwitch  inconsistent operand stack height at basic-block join: block 14 (pc 148) <- block 12 (pc 144), existing_len=0, new_len=2
IushrLength          stack underflow while rewriting StackInput: ... pc=3 opcode=0x7c mnem=iushr depth=2 stack_len=2
PairedOnly           InvalidUtf8
SurrogateConstants   InvalidUtf8
WideParams           IR verify error: reference to nonexistent parameter: '3'
repro.jar            InvalidUtf8            <- no indication which entry
```

## After

All eight import, and so does the JAR:

```
$ ./repro/run.sh
'repro-DenseSwitch': imported 5 function(s)
'repro-GuardedStringSwitch': imported 8 function(s)
'repro-IushrLength': imported 5 function(s)
'repro-PairedOnly': imported 4 function(s)
'repro-SparseSwitch': imported 5 function(s)
'repro-StringSwitch': imported 10 function(s)
'repro-SurrogateConstants': imported 4 function(s)
'repro-WideParams': imported 14 function(s)
'repro-jar': imported 41 function(s)
```

## The fixes

### 1. Switch instructions had no stack effect — `flow.rs`

`tableswitch` (0xaa) and `lookupswitch` (0xab) each pop one int selector, and
neither was modelled. The selector survived as a phantom slot and surfaced at
the first join the switch dominated — which for a `while (…) switch (…)` is the
loop header itself, and for a Java 8 string switch (which lowers to *both*
instructions) is two slots deep.

```rust
// Switches consume their int selector. Both decode to no dataflow and
// to InstructionKind::Other, so this is where their stack effect lives.
0xaa | 0xab => (1, 0), // tableswitch, lookupswitch
```

### 2. Shift stack effects were assigned by opcode range — `flow.rs`

The int and long shifts alternate; they are not contiguous. The ranges gave
`lshl` (0x79) the int effect and `iushr` (0x7c) the long one, so `iushr`
claimed to pop three slots where two were present.

```rust
0x78 | 0x7a | 0x7c => (2, 1), // ishl, ishr, iushr
0x79 | 0x7b | 0x7d => (3, 2), // lshl, lshr, lushr
```

### 3. `iushr` was given two inline operand bytes — `flow.rs`

`operand_byte_count` had `0x7c => 2`. `iushr` is a one-byte instruction, and
`instruction_length` drives the only instruction iterator, so a wrong length
desynchronized the whole rest of the method — genuinely independent of defect 2,
which is why the fixture fails differently when only one of the two is fixed.
The arm is deleted; `_ => 0` was already right.

### 4. Modified UTF-8 — `parse_utils.rs`, `types.rs`

Both stages of the plan, not just Stage A.

Modified UTF-8 is a **UTF-16** transport: each one-, two- or three-byte sequence
is one UTF-16 code unit, so a supplementary character arrives as a CESU-8
*pair*. The decoder treated each sequence as an independent scalar value, so
**any** class with an emoji or a CJK extension character in a literal failed —
far more than the packed-table case the report described.

`decode_modified_utf8_code_units` now returns the code units verbatim, and a new
`JvmString` decides what to do with them:

```rust
pub enum JvmString {
    /// Every code unit is a Unicode scalar value (surrogates, if any, paired).
    Utf8(String),
    /// Holds unpaired surrogates; kept as raw UTF-16 code units.
    Utf16(Box<[u16]>),
}
```

with `as_str() -> Option<&str>`, `to_string_lossy()`, `to_string_replacing(char)`
and `code_units()`. Pairs recombine for free via `String::from_utf16`, and the
common case stays a plain `String` — only an entry that actually needs code
units pays for them.

`ClassFile::get_utf8` still returns `&str` and now errors on unpaired
surrogates, which is correct for every one of its callers: names and descriptors
can never legally contain one. String *constants* go through `get_utf8_lossy` /
`get_jvm_string`, so a packed UTF-16 table no longer stops the class parsing.

### 5. `long`/`double` parameters were indexed in the wrong space — `flow.rs`

`Location::Parameter` is a parameter **ordinal** — it indexes the list
`jvm_descriptor_to_params` builds — but it was constructed from the **local
slot**. Wide parameters take two slots and one ordinal, so the spaces diverge at
the first `long` or `double` and the IR verifier rejects the method.

New `ParameterSlotMap` translates, built once per method from the descriptor and
access flags; the second half of a wide parameter maps to that parameter's
ordinal, not the next one. `local_slot_to_location` takes the map instead of a
slot count. `is_parameter_slot(slot, param_slot_count)` is gone: its only
plausible use was deciding whether to build `Location::Parameter(slot)`, which
is exactly the bug, and nothing called it any more.

This is what `IushrLength` hits once defects 1–3 are patched, so a decoder-only
fix would have traded one failure for another.

### 6. Errors carried no context — `error.rs`, `jar.rs`, `flow.rs`

`InvalidUtf8` was a unit variant; it is replaced by two variants that say what
and where, and the constant-pool parser attaches the index:

- `MalformedUtf8 { cp_index, offset, byte }`
- `UnpairedSurrogate { cp_index, index, code_unit }`

`jar.rs` wraps per-entry failures in `InEntry { entry, source }`, so a whole-JAR
error names the class instead of just the error kind. The join error became the
structured `StackHeightMismatch { … }`, which now also carries class, method
name and descriptor alongside the block/pc edge it already had.

### 7. `xtask` skipped a variant that is no longer produced — `xtask/src/jvm.rs`

The counted skip matched `InvalidClassFile("inconsistent operand stack height at
basic-block join")`, a literal `&'static str` the code stopped producing when
the message gained detail — so the intended skip was a `bail!`. It now matches
the structured `ClassFileError::StackHeightMismatch { .. }`, as the plan's
preferred option suggests, so neither the message text nor a prefix match is
load-bearing.

## Two further defects the new fixtures exposed

Neither is in the plan; both are real, and both blocked the regression suite.

### 8. The disassembler never printed the exception table — `instructions.rs`

No existing sample had a `try`/`catch`, so `jvm:javap` had never compared one.
`javap -c` prints an `Exception table:` section after the code; jvm-reader
printed nothing, and `GuardedStringSwitch` failed the comparison immediately.
Added `format_exception_table`, including the `any` rendering `javap` uses for a
`catch_type` of 0.

A second, smaller mismatch: `javap` writes an unpaired surrogate as `?`, because
no charset can encode one and its output stream substitutes. The disassembler
now renders string constants with `to_string_replacing('?')` to match, while the
IR path keeps `U+FFFD`.

### 9. `xtask` asserted that the entry block has no predecessors

False for any method that opens with a loop header: `javac` emits a back edge to
pc 0, which is exactly what `SparseSwitch.parseTable` and `DenseSwitch.parseTable`
do. `normalize_stack_slots_for_method` handles this correctly (it seeds the entry
with an empty stack and the back edge must agree). The check is narrowed to what
is actually invariant — the entry may have back edges, but not a *forward*
predecessor, which would mean a second way into the method.

## Tests

### Fixtures

The `repro/src/*.java` sources moved into the test tree, as the plan suggested:

| Fixture | Covers |
| --- | --- |
| `tests/sample/SparseSwitch.java` | `lookupswitch`, back-edge join |
| `tests/sample/DenseSwitch.java` | `tableswitch`, back-edge join |
| `tests/sample/StringSwitch.java` | Java 8 string switch: both instructions |
| `tests/sample/GuardedStringSwitch.java` | same, join is an exception-handler edge |
| `tests/sample/IushrLength.java` | `iushr` length + the full shift family |
| `tests/sample/WideParams.java` | wide params leading/middle/trailing, static and instance |
| `tests/sample-jvm-only/PairedOnly.java` | a well-formed surrogate pair, nothing else |
| `tests/sample-jvm-only/SurrogateConstants.java` | paired, unpaired high, unpaired low, packed table |

`WideParams.java` was extended from the three static methods it had to eleven,
so leading, middle and trailing wide parameters are covered on both static and
instance methods, as the plan's suite asks.

**Why `sample-jvm-only/` exists.** `tests/sample/` is shared with the dex-reader
checks, which compile every source there down to `.dex`. dex-reader's
`decode_mutf8` has the identical defect this change fixed in jvm-reader — it maps
each three-byte sequence through `char::from_u32` independently — so both
surrogate fixtures would fail `dex:samples` for a reason that has nothing to do
with the sample. They are held in a sibling directory that only the jvm-reader
checks and `jvmTestFixtures` compile. **Fixing `dex-reader/src/parse_utils.rs`
the same way is left as a separate change**; move the two fixtures into
`tests/sample/` when it lands.

### New tests

`jvm-reader/src/flow.rs` — the pure ones are deliberately **not** `#[ignore]`d,
unlike the rest of that module, so a plain `cargo test` catches a table
regression:

- `arithmetic_opcodes_are_one_byte` — `instruction_length` is 1 for every opcode
  in `0x60..=0x83`
- `shift_stack_effects_are_assigned_by_opcode` — `(2,1)` for 0x78/0x7a/0x7c,
  `(3,2)` for 0x79/0x7b/0x7d
- `switches_consume_their_selector`
- four `parameter_slot_map_*` tests: identity without wide params, both halves of
  a wide param, wide params in any position, and the implicit `this` slot

and, fixture-backed (`#[ignore]`d, needs `JVM_READER_TEST_FIXTURES`):

- `sparse_switch_normalizes`, `dense_switch_normalizes`,
  `string_switch_normalizes`, `guarded_string_switch_normalizes`
- `switch_fixtures_contain_the_switch_they_name` — so a `javac` change cannot
  quietly turn the four above into vacuous tests
- `iushr_does_not_desynchronize_the_decoder` — asserts every pc the decoder stops
  at actually holds the opcode it claims, not merely that decoding finishes
- `every_shift_opcode_appears_and_simulates`
- `wide_parameters_are_reported_as_ordinals_not_slots`,
  `wide_parameter_slots_resolve_to_the_right_ordinal`
- `paired_surrogates_parse`, `unpaired_surrogate_constants_parse`

`jvm-reader/src/parse_utils.rs` — nine hermetic tests over raw byte sequences:
ASCII, `C0 80` NUL, pair recombination (U+10000 and U+1F600), four-byte UTF-8
rejected, unpaired high, unpaired low, a packed table keeping all seven code
units, first-unpaired-surrogate reporting, and cp-index attachment.

`jvm-reader/src/error.rs` — two tests that the JAR-entry and join errors say what
they should.

### Mutation-tested

Each defect was reintroduced one at a time to confirm the tests are not vacuous:

| Reintroduced | Tests that failed |
| --- | --- |
| switches lose their stack effect | 5 |
| shift effects back to ranges | 2 |
| `iushr` regains two operand bytes | 2 |
| parameters reported as local slots | 2 |

## Verification

```
cargo test --workspace                                  754 tests, 0 failed
JVM_READER_TEST_FIXTURES=… cargo test -p jvm-reader -- --include-ignored
                                                        43 passed, 0 failed
nix build .#checks.aarch64-darwin.jvm-reader-tests      ok
nix build .#checks.aarch64-darwin.regression            128 passed, 0 skipped,
                                                         0 failed, 2 xfail of 130
cargo clippy --workspace --all-targets                  no new warnings
```

The two xfails are the pre-existing, documented `C_KNOWN_FAILURES` entries
(`C:ptrarith` and the native-defaults known-answer case); they are unrelated to
this change. The `--frontend jvm` and `--frontend dex` slices were also run on
their own: 31/31 and 26/26 passing, which is what confirms the six shared
fixtures are safe for the dex path.

Fixtures were checked against both JDK 8 (major version 52, matching the reported
case) and the JDK 17 the pinned toolchain uses.

## Files changed

```
flake.nix                                  jvmTestFixtures compiles the new fixtures
jvm-reader/src/error.rs                    new variants, context helpers, Display
jvm-reader/src/flow.rs                     defects 1,2,3,5,6 + tests
jvm-reader/src/instructions.rs             exception table, javap surrogate rendering
jvm-reader/src/jar.rs                      per-entry error context
jvm-reader/src/lib.rs                      exports JvmString, ParameterSlotMap
jvm-reader/src/parse_utils.rs              code-unit decoder + tests
jvm-reader/src/parser.rs                   builds JvmString, attaches cp index
jvm-reader/src/types.rs                    JvmString, get_jvm_string, get_utf8_lossy
jvm-reader/tests/sample/*.java             six fixtures (moved from repro/src)
jvm-reader/tests/sample/README.md          documents them
jvm-reader/tests/sample-jvm-only/          two surrogate fixtures + README
repro/run.sh                               points at the relocated sources
xtask/src/jvm.rs                           defect 7, entry-block check, jvm-only dir
```

## Not done

- **dex-reader's modified-UTF-8 decoder** has the same defect (defect 4) and is
  untouched. It is a separate crate with a separate blast radius — its string
  table feeds type, method and field names as well as constants — and fixing it
  means giving `dex-reader` the `JvmString` treatment throughout. The two
  surrogate fixtures are parked in `sample-jvm-only/` until it lands.
- The plan's suggestion to add the ordinary and R8 builds of Apktool's
  `BinaryResourceParser` and `ResFileDecoder` as end-to-end fixtures — the
  Apktool checkout is not on this machine. The eight fixtures above reproduce
  every root cause the report identified, from first principles.
