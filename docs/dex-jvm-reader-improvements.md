# Improving `dex-reader` and `jvm-reader` dataflow - DO-NOT-MERGE

Status: open. Nothing is scheduled. These problems were found while designing
the CIL reader's dataflow layer ([`cil-flow-api.md`](cil-flow-api.md)), which is
built to avoid them. §1 covers `dex-reader`, including two concrete bugs, one of
which gives wrong output today (§1.3). §2 covers `jvm-reader`.

---

## 1. `dex-reader`

### 1.1 What its pattern gets wrong

`dex_reader::instructions::DataFlow` (`readers/dex-reader/src/instructions.rs:1274`)
is three boxed streaming iterators:

```rust
pub struct DataFlow {
    pub source: DynIter,   // Box<dyn DoubleEndedStreamingIterator<Item = Reg>>
    pub dest: DynIter,
    pub ret: DynIter,
}
```

Its only consumer is `dataflow_to_assign` in `frontends/ctadl-dex/src/lib.rs`
(`:655`). Problems, each visible there:

1. **Bags, not roles.** `(source, dest, ret)` records *whether* a register takes
   part, not *which role* it plays: object base, index, or stored value. So the
   consumer re-decodes the instruction anyway. The `IPut` arm filters `f.b` out of
   `source` and then uses `f.b` directly as the object, and `APut` filters two
   registers out by hand. The dataflow view does half the job twice, and the two
   decodings can drift apart (§1.2) or be ambiguous (§1.3).
2. **Silently inconsistent.** `IPutWide` lists the object register as a *dest*
   while every other store lists it as a source (§1.2).
3. **No operand data.** Field, string and type indices are not in `DataFlow`, so
   the consumer goes back to `parser.get_field(f.idx)` and
   `constant_pool().strings`.
4. **`ret` is dead.** All 67 `dataflow_source_sink_ret!` invocations pass `[]`
   for it, `dataflow_from_args` sets it empty, and the consumer binds it as
   `ret: _` (`frontends/ctadl-dex/src/lib.rs:665`). It costs a boxed empty
   iterator per instruction and carries nothing.
5. **Three heap allocations per instruction**, plus `RegSliceStream` copying the
   argument slice into a `Vec` at every call site. It also forces the
   `streaming-iterator` crate on every consumer (`ctadl-ascent` imports
   `StreamingIterator` solely to call `dest.owned()`).
6. **Untestable.** `DataFlow` derives nothing: no `Debug`, no `PartialEq`, and its
   fields are trait objects, so a test cannot compare two of them. There are no
   tests of `data_flow()` in `dex-reader`. The only coverage is the 11 tests of
   the *frontend's* translation in `frontends/ctadl-dex/src/tests.rs`, and none
   exercises `iput-wide` or a self-aliased store.
7. **Nothing to check against.** Dex has no pop/push table, so unlike CIL (whose
   `OpCode` carries `StackBehaviour` generated from Cecil) there is no
   independent source of truth a property test could cross-check `data_flow()`
   against.

### 1.2 Bug: `iput-wide` reports the object register as a destination

**Severity:** latent. No wrong output today, but it blocks any second consumer.
Present since the initial commit (`a3c69002`).

Dex format 22c is `iput vA, vB, field@CCCC`: `vA` (plus `vA+1` when wide) is the
**value stored**, and `vB` is the **object written into**. `vB` is read, never
written. The table disagrees with itself about `vB`:

```rust
// readers/dex-reader/src/instructions.rs:2711
IPut(f) | IPutBoolean(f) | IPutByte(f) | IPutChar(f) | IPutShort(f) | IPutObject(f) => {
    dataflow_source_sink_ret!([f.a, f.b], [], [])          // vB is a SOURCE
}
// readers/dex-reader/src/instructions.rs:2714
IPutWide(f) => dataflow_source_sink_ret!([f.a, f.a.next_reg()], [f.b], [])  // vB is a DEST
```

`IPutWide` is the only outlier among the stores:

| Opcode | source | dest | object/array register |
| --- | --- | --- | --- |
| `IPut*` (`:2711`) | `[a, b]` | `[]` | source ✓ |
| `IPutWide` (`:2714`) | `[a, a+1]` | `[b]` | **dest ✗** |
| `SPutWide` (`:2704`) | `[a, a+1]` | `[]` | n/a (static) ✓ |
| `APut*` (`:2801`) | `[a, b, c]` | `[]` | source ✓ |
| `APutWide` (`:2804`) | `[a, a+1, b, c]` | `[]` | source ✓ |

**Why nothing catches it.** The consumer handles all seven `IPut*` variants in
one arm (`frontends/ctadl-dex/src/lib.rs:797-822`). It filters `f.b` back out of
`source` (`:810`), takes the object from `f.b` directly (`:813`), and never reads
`dest` on this path. So the wrong `dest` is discarded and the output is correct.
Problems 6 and 7 above mean no test could notice.

**What breaks when it stops being invisible.** Any consumer that reads `dest` as
"the registers this instruction defines" (a liveness pass, def-use chains, an SSA
renamer, a register-level slicer) will conclude that `iput-wide` kills `vB`. The
object's incoming flow is cut there, and every later use of `vB` is attributed
to a definition that does not exist.

**Fix.** One line, and it doesn't change what the current consumer does,
because it filters `f.b` out of `source` anyway:

```rust
IPutWide(f) => dataflow_source_sink_ret!([f.a, f.a.next_reg(), f.b], [], []),
```

### 1.3 Bug: a self-aliased `iput`/`aput` silently drops the stored value

**Severity:** live. It drops a real dataflow edge today.

The consumer recovers the value operands by removing the object register **by
value**:

```rust
// frontends/ctadl-dex/src/lib.rs:810  (IPut* arm)
source.cloned().filter(|r| *r != f.b) ...
// frontends/ctadl-dex/src/lib.rs:877  (APut* arm)
source.cloned().filter(|r| *r != f.b && *r != f.c) ...
```

When the value register and the object register are the *same* register, the
filter removes the value too. The generated statements are `assign(temp, [])`, a
temp with no sources, then `store(object.field, temp)`. The edge
`vX -> vX.field` is lost.

`iput-object p0, p0, LFoo;->cb:LBar;` is exactly this shape. It is what
`this.mCallback = this` compiles to, a common Android idiom (self-registering
listeners, self-referential nodes). `aput-object v1, v1, v2` (`a[i] = a`) is the
array analogue.

**Fix.** Don't recover operands by comparing values. Take the value operands
positionally from the instruction fields (`f.a`, plus `f.a.next_reg()` for the
wide variants) instead of filtering `source`.

### 1.4 The structural fix and regression protection

Both bugs come from problem 1: `DataFlow` erases roles, so the consumer
reconstructs them. The structural fix is a role-carrying, `PartialEq`-derivable
per-instruction view, the shape [`cil-flow-api.md`](cil-flow-api.md) specifies
for CIL (§3, and §6.2 for why roles make §1.2 impossible). If `dex-reader` is
reworked, that is the shape to converge on.

Short of that, any of these would have caught §1.2, and the third catches §1.3:

1. `#[derive(Debug, Clone, PartialEq)]` on a non-boxed `DataFlow` (for example
   `Vec<Reg>` fields), plus per-opcode-family unit tests asserting the exact
   register lists.
2. A table-level property test: for every store opcode, the object or array
   register appears in `source` and not in `dest`.
3. Frontend tests in `frontends/ctadl-dex/src/tests.rs` over `iput-wide` and
   `iput-object p0, p0, F`.

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
