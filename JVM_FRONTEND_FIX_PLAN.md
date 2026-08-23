# CTADL JVM frontend: second-round defect confirmation and fix plan

This plan covers the CVE reproducibility report on CTADL 0.1.2 (APISIX, Emissary,
Spring AI, OpenMRS, Yamcs, Junrar, GeoTools). It replaces the first-round plan,
whose defects — switch selectors, shift opcode ranges, `iushr` length,
modified UTF-8, wide parameter ordinals, error context, the `xtask` skip — are
all fixed and landed; see `JVM_FRONTEND_FIX_SUMMARY.md` for that round.

Of the report's seven cases, **one was already fixed** (GeoTools), **two were
real and are now fixed** (Emissary, Spring AI's `SearchRequest`), **one is
untouched and needs Lua work** (APISIX), and **three could not be reproduced
directly** because the artifacts are not on this machine (OpenMRS, Yamcs,
Junrar) — but every failure mode they describe is now absent from a 333k-method
corpus. Three defects the report did not name were found along the way.

## Verdict against the report

| # | Report case | Report's primary defect | Verdict |
| --- | --- | --- | --- |
| 1 | APISIX CVE-2022-29266 | Lua table-selected callback resolution | **Open.** Not addressed; needs Lua frontend work (below) |
| 2 | Emissary CVE-2026-35582 | JVM category-2 field width | **Was real, now fixed** (defect A) |
| 3 | Spring AI CVE-2026-40967 | Category-2 fields, stack joins | **Field half was real, now fixed** (defect A). Join half not reproduced (below) |
| 4 | OpenMRS CVE-2026-40075 | Operand-stack join handling | **Not reproduced.** Every join failure in the corpus was a symptom of A–D |
| 5 | Yamcs CVE-2026-44632 | Unclassified stack underflow | **Not reproduced.** That message fired 19× pre-fix, 0× post-fix |
| 6 | Junrar CVE-2026-41245 | Operand-stack join handling | Same as #4. Licensing exclusion is unaffected either way |
| 7 | GeoTools CVE-2026-76904 | Shift-opcode table | **Already fixed** in the first round; re-verified |

Two of the report's prescriptions turned out **not** to be indicated:

- *"Parse the invokedynamic descriptor and preserve category-2 argument widths."*
  `decode_call` already does this — `descriptor_param_slot_count` sums
  `slot_width`, and `descriptor_return_slot_count` returns 2 for `J`/`D`.
  `SearchRequest.toString` failed because the **`double` field** it loaded
  pushed one word, not because the `invokedynamic` consumed the wrong count.
  The reported `consumed=5` was already correct.
- *"Replace exact slot-identity comparison at joins with typed frame merging /
  phi-like join slots."* Not needed, and it would mask defects rather than fix
  them. See "The join checks" below.

## Defects

Four decoder defects, all in `jvm-reader/src/flow.rs`. Each is a table or
descriptor omission; none needs a new analysis.

### A. Field opcodes ignore the descriptor's category-2 width

`long` and `double` occupy two operand-stack words. All four field opcodes moved
exactly one, whatever the descriptor said:

```rust
0xb2 => { …; destinations.push(Location::StackOutput); }              // getstatic
0xb3 => { sources.push(Location::StackInput(0)); … }                  // putstatic
0xb4 => { …; destinations.push(Location::StackOutput); }              // getfield
0xb5 => { sources.push(StackInput(0)); sources.push(StackInput(1)); } // putfield
```

The shortfall does not fail at the field access; it fails at whatever consumes
the value, which is why the report saw it at `lcmp`. Reproduced from a
purpose-written class, matching Emissary's `Roller.incrementProgress` and
`Executrix.execute` exactly:

```
$ ctadl import -l jvm -n t LongField.class
stack underflow while rewriting StackInput: class=LongField
method=overBudget(J)Z pc=5 opcode=0x94 mnem=lcmp depth=3 stack_len=3
```

and Spring AI's `SearchRequest.toString`, where the same missing word lands on a
string-concat `invokedynamic` instead:

```
$ ctadl import -l jvm -n t Concat.class
stack underflow while rewriting call argument StackInput: class=Concat
method=toString()Ljava/lang/String; pc=16 opcode=0xba mnem=invokedynamic
depth=4 stack_len=4 consumed=5
```

**Fix.** A `field_slot_width(descriptor)` helper — 2 for `J`/`D`, 1 otherwise —
applied to all four opcodes: `getstatic` pushes it, `putstatic` pops it,
`getfield` pops the receiver and pushes it, `putfield` pops it plus the receiver
underneath.

### B. `sastore` is misnamed and unmodelled

`0x56` is `sastore`, the last of the array stores. The mnemonic table called it
`dup2_x2` (which is `0x5e`), and the array-store arms — in both `decode_dataflow`
and `opcode_kind` — stopped at `0x55` (`castore`). So every `short[]` store was
modelled as a no-op and left its arrayref, index and value on the simulated
stack. The three phantom slots surface at the first join the store dominates:

```
java/util/Arrays.fill([SS)V: inconsistent operand stack height at basic-block
join: block 1 (pc 5) <- block 2 (pc 10), existing_len=0, new_len=3
```

This is the single largest source of join-height failures in the corpus, and it
presents exactly as the "operand-stack join handling" defect the report
attributes to OpenMRS and Junrar — as a join error whose real cause is several
instructions upstream.

**Fix.** `0x56 => "sastore"`, and extend both arms to `0x4f..=0x56`.

### C. `sipush` reads four operand bytes

```rust
0x11 => { let s = read_i32_be(code, pc + 1)? as i16 as i32; … }
```

`sipush`'s operand is a two-byte signed short. Reading four bytes and narrowing
takes the *following* two bytes as the constant — so the decoded value is wrong
whenever the read succeeds — and runs off the end of the code array whenever
`sipush` sits within three bytes of it. `static int limit() { return 1024; }`
compiles to `sipush 1024; ireturn`, four bytes total, and fails the class:

```
com/sun/corba/se/impl/orb/ORBSingleton.getGIOPFragmentSize:
OutOfBounds { offset: 1, size: 4, len: 4 }
```

`instruction_length` was already right (`0x11 => 2`), so this never
desynchronized the decoder — it only corrupted the constant or aborted the
method.

**Fix.** `read_u16_be(code, pc + 1)? as i16 as i32`.

### D. `multianewarray` has no stack effect

`0xc5` pops one int count per dimension and pushes the array reference. It was
neither in `decode_dataflow` nor in `misc_stack_effect`, so every `new T[a][b]`
left its counts behind:

```
com/sun/media/sound/SoftAbstractResampler$ModelAbstractResamplerStream.open:
inconsistent operand stack height at basic-block join:
block 2 (pc 48) <- block 1 (pc 27), existing_len=0, new_len=1
```

**Fix.** Decode it like the other allocations: one `StackInput` per dimension
plus a `Location::Allocation` source, one `StackOutput` destination, and
`InstructionKind::Dataflow`.

## Evidence

A sweep runs `basic_blocks_with_stack_slots` over every method with code in a
JAR and tallies the failures by kind and opcode. Corpus: JDK 8 `rt.jar`, JDK 21
(`java.base`, `java.desktop`, `java.xml`, `java.sql`, `java.logging`,
`jdk.compiler`, `java.management`, extracted with `jimage`), apktool-lib 2.9.3,
baksmali 3.0.9-fat (which vendors Guava), commons-io 2.15.1.

| Stage | Corpus | Failing methods |
| --- | --- | --- |
| Before any fix | `rt.jar` | 2,288 of 161,225 |
| After A | 4 JARs | 224 — 128 join-height, 96 `OutOfBounds` |
| After A, B, C | 5 corpora | 23 — all `multianewarray` |
| After A, B, C, D | 5 corpora | **0 of 333,125** |

The pre-fix `rt.jar` breakdown is worth keeping, because it shows how far a
category-2 shortfall travels from its cause: 1,026 `StackInput` underflows (468
at `lcmp`, the rest at `ladd`/`lsub`/`land`/`dcmp*`/`dmul`/`lstore_*`/`dup2`/
`putfield`), 677 receiver underflows, 238 call-argument underflows, 235
join-height mismatches, 93 `OutOfBounds`, and 19 bare "stack underflow in
stack-slot simulation" — the exact message the report could not classify for
Yamcs. All six kinds are now zero.

## What is still open

### 1. APISIX: Lua function values in tables — untouched

Nothing on this branch changes the Lua frontend. Today
`LuaLower::indirect_call_target` lowers a call to `CallStyle::FuncPtrCall` only
when the callee is a **bare name** bound to a local or parameter. APISIX's

```lua
phase_func(plugins[i + 1], api_ctx)
```

does get a `FuncPtrCall`, because `phase_func` is a local — but nothing
connects that local back to `plugins.jwt-auth.rewrite`, because function values
are not tracked through table fields, array reads, or the plugin registration
structures they are stored in. The report's required fix stands as written:

- track function values stored in tables, arrays and registration structures;
- propagate them through indexed reads such as `plugins[i + 1]`;
- resolve the `phase_func` call to compatible plugin-phase implementations,
  discriminated by table identity, field name, registration site or receiver
  context, so the edge is not "every indirect call to every plugin";
- preserve the relationship through indexing.

Acceptance criterion unchanged: the existing models produce a path through
`jwt-auth.rewrite` to `apisix.core.response.exit` with no summary specifically
connecting `<indirect-call>` to that handler.

This is the one item in the report that is a missing analysis rather than a
decoder table, and it is the only one that needs its own design.

### 2. Two error sites still carry no context

The report is right that the diagnostics blocked its own diagnosis, and two
sites are still bare `InvalidClassFile(&'static str)`:

- `flow.rs` — `"inconsistent operand stack layout at basic-block join"`, the
  message the report quotes for Spring AI's `AbstractFilterExpressionConverter`.
  No class, method, pc, or edge.
- `flow.rs` — `"stack underflow in stack-slot simulation"`, the message the
  report quotes for Yamcs. No class, method, pc, or opcode.

Everything else already reports class, method, descriptor, pc, opcode, mnemonic
and depths; `StackHeightMismatch` additionally reports the block edge. Give
these two the same treatment — a structured variant apiece, carrying the frame
and the predecessor edge alongside what the others already carry. Cheap, and it
is what would have let the report name the Yamcs and OpenMRS instructions.

### 3. The join checks — do not replace them with phi merging

The report asks for typed frames and phi/canonical join slots. That is the wrong
change here, and it would have hidden defects A–D rather than fixing them: every
join failure in the corpus was a real decoder bug several instructions upstream,
and a merge that accepts mismatched frames would have swallowed all of them.

The height check is doing exactly its job and should stay.

The **layout** check is a different matter, but the fix is smaller than phi
merging. Stack-slot ids in `simulate_block` are positional — a slot's id *is* its
depth (`remaining_len + i`) — so two predecessors at equal height always produce
identical slot vectors, and the layout check is vacuous by construction. The one
exception is `handler_entry_slots`, which draws the exception object's id from a
separate global counter (`next_slot_id`). A handler entry slot therefore gets a
non-positional id, and the layout check can only ever fire on a block reachable
from both a handler and a normal edge.

That shape does not occur in this corpus: instrumenting the propagation loop
found **zero** blocks in 333,125 methods with both an exception and a
non-exception predecessor. So the Spring AI failure could not be reproduced, and
its more likely explanation is defect A, which independently and provably breaks
`AbstractFilterExpressionConverter`'s class (it is a `double`-carrying converter
in the same JAR as `SearchRequest`).

The change to make anyway, when the Spring AI JAR is available to confirm
against: **allocate the handler's entry slot positionally** — the exception
reference is at depth 0, so its id is `0` — and delete `handler_entry_slots` and
`next_slot_id`. That makes the layout check vacuous in every case rather than
almost every case, and it removes a latent second problem: `next_slot_id` is
per-method-global, so a method with 64 or more handlers would emit slot ids past
the `id >= 64` bound that `xtask`'s `assert_normalized` treats as corruption.

### 4. Smaller category-2 and legacy gaps, none currently reachable

Found while auditing; all inert today, all worth closing when that code is
touched:

- `misc_stack_effect` gives `0xac..=0xb0` (all five value returns) `(1, 0)`.
  `lreturn` and `dreturn` pop two. Harmless because a return block has no
  successors, so the phantom slot never reaches a join.
- `jsr` (0xa8) does not push its return address, `ret` (0xa9) is missing from
  `operand_byte_count` (it takes a one-byte local index), and neither has a
  stack effect. `jsr`/`ret` are illegal from class version 51 on, so nothing in
  the corpus exercises them; a pre-Java-7 artifact would desynchronize.
- `mnemonic` lumps `0x85..=0x93` into a single `"conv_or_cmp"`. That is a real
  disassembly gap — `javap` prints `i2l`, `l2i`, `f2d` and the rest — and it is
  only invisible to `jvm:javap` because no sample fixture contains a numeric
  conversion.

### 5. The report's remaining asks, resolved

- *"Re-enable the currently ignored `loop_flow_main_stack_normalizes` test."*
  No such test exists anywhere in the tree, and no `#[ignore]` remains in
  `jvm-reader`. The equivalent coverage is the `Jvm:LoopFlow` taint case and the
  `jvm:stack-slots` check, both passing.
- *"Add the seven real artifacts as regression tests."* Not possible here: none
  of the seven are on this machine, and Junrar is excluded for licensing
  regardless. The four fixtures below reproduce every root cause from first
  principles, and the 333k-method corpus sweep covers breadth that seven
  artifacts would not.

## Tests

### Fixtures and cases

Four taint cases in `nightly/tests/java`, each of which the harness runs as both
a `Dex:` and a `Jvm:` case, and each added to `JVM_E2E_ENFORCED` so a `Jvm:`
failure is a failure rather than an XFAIL. Classes import atomically, so a
method that fails to decode takes its class's whole flow with it — which is what
gives each case its teeth.

| Case | Construct | Defect |
| --- | --- | --- |
| `WideFieldFlow` | `long`/`double` instance and static fields: `putfield`/`getfield`/`putstatic`/`getstatic` under `lcmp` and `dcmp` | A |
| `ShortArrayFlow` | `short[]` store and load with a join after the store | B |
| `SmallConstantFlow` | a method that is exactly `sipush 1024; ireturn` | C |
| `MultiArrayFlow` | two-dimensional `multianewarray` with a store and load | D |

### Unit tests

Hermetic, in `flow.rs`; the field and `multianewarray` tests build the constant
pool they need in Rust rather than loading a compiled class:

- `field_slot_width_follows_the_descriptor`
- `field_opcodes_move_the_descriptor_width` — all four opcodes × wide and narrow
- `sastore_is_an_array_store` — mnemonic and decode, plus that `0x5e` is still
  `dup2_x2`
- `sipush_reads_two_operand_bytes` — value and sign
- `multianewarray_consumes_one_slot_per_dimension` — one through four

### Mutation-tested

Each defect was reintroduced one at a time; each kills exactly its own case and
nothing else:

| Reintroduced | Case that fails |
| --- | --- |
| field opcodes move one word | `Jvm:WideFieldFlow` |
| `sastore` outside the array-store arm | `Jvm:ShortArrayFlow` |
| `sipush` reads four bytes | `Jvm:SmallConstantFlow` |
| `multianewarray` unmodelled | `Jvm:MultiArrayFlow` |

## Verification

```
cargo test --workspace                      39 suites, 0 failed
xtask regression --frontend jvm,dex         72 passed, 1 skipped, 0 failed, 0 xfail
cargo fmt --all -- --check                  clean
cargo clippy --workspace --all-targets      no new warnings
corpus sweep (5 corpora)                    333,125 methods, 0 failures
ctadl import -l jar (apktool, baksmali, commons-io)   all import
```

The one skip is `dex:baksmali` (`baksmali` not on this machine's PATH); the
clippy `items after a test module` warning in `flow.rs` predates this change.
