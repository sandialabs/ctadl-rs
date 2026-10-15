# Improving `dex-reader` and `jvm-reader` dataflow - DO-NOT-MERGE

Status: §1 (`dex-reader`) is fixed on branch `dex-jvm-reader-fixes`, except for
the open items in §1.7. §2 (`jvm-reader`) is open and not scheduled. These
problems were found while designing the CIL reader's dataflow layer
([`cil-flow-api.md`](cil-flow-api.md)), which is built to avoid them.

---

## 1. `dex-reader`

`Instruction::data_flow()` (`readers/dex-reader/src/instructions.rs:2596`)
returns the registers an instruction reads (`source`) and writes (`dest`). Its
only consumer is `dataflow_to_assign` in `frontends/ctadl-dex/src/lib.rs`
(`:686`).

| Problem | Status | Commit |
| --- | --- | --- |
| §1.2 `iput-wide` reports its object register as a destination | fixed | `145aa942` |
| §1.3 aliased `iput`/`aput` registers drop the stored value | fixed | `11d421d4` |
| §1.1 boxed, uncomparable `DataFlow`, dead `ret` | fixed | `71b8f70e` |
| §1.1 no tests of `data_flow()` | fixed | `124351d0` |
| §1.5 long shifts read their shift amount as a pair | fixed | `86f8634e` |
| §1.6 opcodes `0xfa`-`0xff` named and decoded as unused | fixed | `36978bb0` |
| §1.6 `0xfa`-`0xff` report no registers; 45cc decoded wrong | fixed | `809f5321` |
| §1.6 `0xfa`-`0xff` treated as unable to throw | fixed | `013232a8` |
| §1.6 invoke-polymorphic/custom produce no call | fixed | `50066e95` |
| §1.6 const-method-handle/type leave `vAA` undefined | fixed | `92fa2da6` |
| §1.1 roles erased (bags, not roles) | open | §1.7 |
| §1.7 invoke-custom and method handle targets unresolved | open | §1.7 |

None of these fixes changed `cargo xtask regression`. It reports 165 passed and
3 xfail before (`6839064f`), after the §1.2-§1.5 fixes (`86f8634e`), and after
the §1.6 fixes (`92fa2da6`), with the same outcome for every case.

### 1.1 What the original pattern got wrong

`DataFlow` used to be three boxed streaming iterators:

```rust
pub struct DataFlow {
    pub source: DynIter,   // Box<dyn DoubleEndedStreamingIterator<Item = Reg>>
    pub dest: DynIter,
    pub ret: DynIter,
}
```

1. **Bags, not roles.** *Open.* `(source, dest)` records *whether* a register
   takes part, not *which role* it plays: object base, index, or stored value.
   So the consumer still decodes the instruction again to find the object
   (`f.b`) and the array index (`f.c`). Before `11d421d4` it also recovered the
   stored value by filtering registers out of `source`, which caused §1.3. The
   value is now taken by position (`stored_value_regs`, `lib.rs:952`), but
   `source` still carries no roles.
2. **Silently inconsistent.** *Fixed* (`145aa942`). `IPutWide` listed the object
   register as a *dest* while every other store listed it as a source (§1.2).
3. **No operand data.** *Open, minor.* Field, string and type indices are not in
   `DataFlow`, so the consumer reads them from the instruction directly
   (`parser.get_field(f.idx)`).
4. **`ret` was dead.** *Fixed* (`71b8f70e`). All 67 table entries passed `[]`
   for it, and the consumer bound it as `ret: _`. The field is gone.
5. **Three heap allocations per instruction.** *Fixed* (`71b8f70e`). There was
   also a `Vec` copy of every invoke's arguments, and every consumer had to
   depend on `streaming-iterator`. The crate that imported `StreamingIterator`
   only to call `dest.owned()` was `ctadl-dex`; an earlier draft of this note
   said `ctadl-ascent`, which was wrong. `source` and `dest` are now
   `SmallVec<[Reg; 5]>` (the `Regs` alias). That fits every fixed-arity
   instruction, including 35c invokes, without allocating. The
   `streaming-iterator` dependency is removed from both crates.
6. **Untestable.** *Fixed* (`71b8f70e`, `124351d0`). `DataFlow` now derives
   `Debug, Clone, PartialEq, Eq, Default`. `readers/dex-reader` has per-family
   unit tests of `data_flow()` and a property test over every store opcode.
7. **Nothing to check against.** *Still true.* Dex has no pop/push table, so
   unlike CIL (whose `OpCode` carries `StackBehaviour` generated from Cecil)
   there is no independent source of truth. The unit tests pin the table's
   current answers, checked by hand against the Dalvik bytecode spec. They do
   not derive those answers from a spec.

### 1.2 Fixed: `iput-wide` reported the object register as a destination

**Severity:** latent; it gave no wrong output. Present from the initial commit
(`a3c69002`) until `145aa942`.

Dex format 22c is `iput vA, vB, field@CCCC`. `vA` (plus `vA+1` when wide) is
the **value stored**, and `vB` is the **object written into**. `vB` is read,
never written. The table had:

```rust
IPut(f) | ... | IPutObject(f) => dataflow_source_sink_ret!([f.a, f.b], [], [])  // vB a source
IPutWide(f) => dataflow_source_sink_ret!([f.a, f.a.next_reg()], [f.b], [])      // vB a DEST
```

The ctadl-dex consumer never read `dest` on the iput path, so the output was
correct. But any consumer that treats `dest` as "the registers this instruction
defines" (liveness, def-use chains, an SSA renamer) would decide that
`iput-wide` kills `vB`, and the object's incoming flow would be cut there.

**Fix.** `IPutWide(f) => dataflow!([f.a, f.a.next_reg(), f.b], [])`.
**Regression protection.** `iput_wide_reads_object_register`, plus
`stores_read_every_operand_and_write_nothing`, which checks every `iput*` and
`aput*` opcode.

### 1.3 Fixed: aliased `iput`/`aput` registers dropped the stored value

**Severity:** live; it dropped real dataflow edges. Fixed in `11d421d4`.

The consumer recovered the value operands by removing the object register (and,
for `aput`, the index register) from `source` **by value**:

```rust
source.cloned().filter(|r| *r != f.b) ...               // IPut* arm
source.cloned().filter(|r| *r != f.b && *r != f.c) ...  // APut* arm
```

When the value shared a register with either one, the filter also removed the
value. The output was `assign(temp, [])` followed by `store(object.field,
temp)`, so the edge from the value into the field or element was lost. Affected
shapes:

| Instruction | Source idiom |
| --- | --- |
| `iput-object p0, p0, LFoo;->cb:LBar;` | `this.cb = this` (self-registering listeners, self-referential nodes) |
| `aput-object v1, v1, v2` | `a[i] = a` |
| `aput v0, v1, v0` | `a[i] = i`. An earlier draft of this note missed this case; it is probably the most common of the three. |

**Fix.** Take the value registers by position: `vA`, plus `vA+1` for
`iput-wide` and `aput-wide` (`stored_value_regs`).
**Regression protection.** In `frontends/ctadl-dex/src/tests.rs`:
`iput_object_self_alias_stores_value`, `aput_object_self_alias_stores_value`,
`aput_value_is_index_stores_value` (all three fail before the fix), plus
non-aliased controls for `iput-object`, `iput-wide` and `aput-wide`.

### 1.4 Regression protection

This section listed three cheap protections. All three are in place:

1. A non-boxed, `PartialEq` `DataFlow` with per-opcode-family unit tests
   (`readers/dex-reader/src/instructions.rs`, `mod tests`).
2. A table-level property test: every store reads its value, object or array,
   and index, and writes no register
   (`stores_read_every_operand_and_write_nothing`).
3. Frontend tests over `iput-wide` and aliased stores (§1.3).

### 1.5 Fixed: long shifts read the shift amount as a register pair

**Severity:** live; it created spurious dataflow edges. Found while writing the
tests in §1.4, and fixed in `86f8634e`.

`shl-long`, `shr-long` and `ushr-long` were grouped with the other long binops,
so the table read the second operand as a pair: `vC, vC+1` (or `vB, vB+1` for
the `/2addr` forms). The shift amount is an `int` (`long << int` in Java, and
ART's verifier checks that operand as `Integer`), so it is one register.
`shl-long v0, v2, v4` reported a read of `v5`. ctadl-dex's generic arm flows
every source into each destination, so it created a false edge from whatever
`v5` held into `v0:v1`, which can produce false taint paths.

**Fix.** The three shifts have their own arms, reading `vC` (or `vB`) as one
register.
**Regression protection.** `long_shift_amount_is_one_register` checks all six
opcodes. The frontend test `shl_long_reads_one_shift_register` fails before the
fix.

### 1.6 Fixed: opcodes `0xfa`-`0xff` were treated as unused

**Severity:** live, but rare in app code. These opcodes exist from dex 038
(Android 8.0) onward. Fixed in `36978bb0`, `809f5321`, `013232a8`, `50066e95` and
`92fa2da6`.

dex-reader decoded opcodes `0xfa`-`0xff` with the right formats, but named them
`UnusedFA` to `UnusedFF`. In dex 038 they are `invoke-polymorphic`,
`invoke-polymorphic/range`, `invoke-custom`, `invoke-custom/range`,
`const-method-handle` and `const-method-type`. `MethodHandle.invoke` and
`invokeExact` compile to `invoke-polymorphic`. A non-desugared lambda or
`invokedynamic` compiles to `invoke-custom`. The `Unused` names led several
places to treat them as no-ops:

| Where | What was wrong | Fixed in |
| --- | --- | --- |
| names and disassembly | `unused-fa` ... `unused-ff`; const-method-handle's operand typed as a method index, though it indexes the method_handles table | `36978bb0` |
| `data_flow()` | invoke-polymorphic(/range) read no registers; const-method-handle/type wrote no register | `809f5321` |
| `Format45cc::new` | the `A\|G` nibbles of invoke-polymorphic's first unit were swapped, so the argument count and the fifth register were read from each other's bits | `809f5321` |
| `call_args()`, `is_invoke()` | invoke-polymorphic missing from both; invoke-custom missing from `is_invoke()` | `809f5321` |
| `can_throw()` | all six listed as unable to throw, so inside a `try` block they got no edge to the handler | `013232a8` |
| ctadl-dex `decode_call` | invoke-polymorphic and invoke-custom produced no statement. The following move-result read the shared `retval` temporary, whose reaching definition was the *previous* call's result, so that unrelated value flowed into the register | `50066e95` |
| ctadl-dex `dataflow_to_assign` | const-method-handle/type emitted nothing, so `vAA` kept its earlier value | `92fa2da6` |

**Fix.** In dex-reader: real names and mnemonics, a `MethodHandleIdx` index
type, and 45cc decoded into an `args` list in the same order as 35c. The
invokes read their arguments, the const opcodes write `vAA`, and all six can
throw. In ctadl-dex:

- `invoke-polymorphic` becomes a `JavaCall` on the method it names, with the
  handle as receiver, the same as `invoke-virtual`. It gets an external stub,
  and models can match it.
- `invoke-custom` names a call site, not a method, so it becomes a
  `CallStyle::Unknown` call. Flow through it is still cut (see §1.7), but its
  result is now defined at the site, and `ctadl report` counts it as `unknown`.
- `const-method-handle` and `const-method-type` define `vAA` as an object
  reference of class `Ljava/lang/invoke/MethodHandle;` or
  `Ljava/lang/invoke/MethodType;`.

**Regression protection.** In dex-reader:
`invoke_polymorphic_decodes_and_reads_its_arguments`, which decodes raw code
units and so pins the nibble order;
`invoke_polymorphic_range_and_invoke_custom_read_their_arguments`;
`const_method_handle_and_type_write_their_register`; and
`opcodes_fa_to_ff_can_throw`. In ctadl-dex:
`invoke_polymorphic_is_a_virtual_call`,
`invoke_custom_is_a_call_with_unknown_target` and
`const_method_handle_and_type_assign_an_object`.

The regression samples are compiled with `javac --release 8` and `dx`, which
never emit these opcodes. So `cargo xtask regression` does not exercise them,
and the unit tests are their only coverage.

### 1.7 Still open

1. **Roles.** The structural fix for §1.1 problem 1 is a role-carrying
   per-instruction view, the shape [`cil-flow-api.md`](cil-flow-api.md) specifies
   for CIL (§3, and §6.2 for why roles make §1.2 impossible). If `dex-reader` is
   reworked, that is the shape to converge on. Until then, consumers must decode
   operand roles from the instruction fields by position, never by comparing
   register values.
2. **invoke-custom and method handle targets.** An `invoke-custom` call has no
   target, so no flow passes through it. Resolving one means reading its
   `call_site_item` (bootstrap method handle, name and method type; `DexParser`
   already parses `call_site_ids` and `method_handles`). It also means modeling
   the common bootstrap, `LambdaMetafactory.metafactory`, which returns an object
   capturing the arguments. Likewise, `invoke-polymorphic` resolves only to
   `MethodHandle.invoke` itself. Which method a handle points to (from
   `const-method-handle` or `MethodHandles.Lookup`) is not tracked, so flow
   through the handle depends on a model for `MethodHandle.invoke`, and the
   default models have none.

---

## 2. What `jvm-reader`'s pattern gets wrong

`jvm_reader::flow` is closer — it has a `Location` enum carrying `Constant`,
`FieldRef`, `ArrayElement`, `Allocation`, which is the right instinct. But:

1. **Two representations for one thing.** Decode emits `Location::StackInput(depth)`
   / `StackOutput`; a later `normalize_stack_slots_for_method` pass *mutates* them
   into `Location::StackSlot(id)`. Every consumer must handle both, and
   `ctadl-jvm`'s `convert_location_to_var_ref` has a `"Stack Local?"` bucket for
   the case that should be impossible.
2. ~~**Slot ids are stack depths, not definitions.**~~ *Retracted.* This said that
   naming a stack slot by its depth merges unrelated values. It does not, because
   the frontend's output goes through `ctadl_ir::ssa::run_pipeline` before any facts
   are generated. Pruned SSA gives every definition of `s2` its own version and puts
   a phi only where two definitions really meet. Depth naming is in fact what the
   CIL design uses (see [`cil-flow-api.md`](cil-flow-api.md) §4).
3. **The frontend re-implements value numbering.** To answer "which
   object does this `putfield` write to", `ctadl-jvm/src/lib.rs` carries
   `stack_aliases`, `dup_slot_pairs`, `last_aload_reg`, `note_assign_aliases`,
   `stack_slot_alias`, `aload_for_stack_slot`, `aload_register_before`,
   `resolve_object_var`, `field_object_base`, and `init_survivor_link` — roughly 200
   lines of backwards-scanning heuristics, with comments like "Prefer a stable
   register/parameter for this stack slot when it differs from the most recent
   aload". Depth naming is not why this code exists (see 2). After SSA,
   `propagate_copies` already rewrites a store's base from a stack slot to the
   variable that was copied into it. Whether this code is needed at all has not
   been checked against `ctadl-jvm`.
4. **Hand-written stack effects.** `stack_effect` and `misc_stack_effect` are
   opcode tables maintained by hand.
5. **Fails whole methods.** An inconsistent stack height at a join returns
   `Err(InvalidClassFileMessage(...))`, losing the method — and with it every call
   edge through it.
