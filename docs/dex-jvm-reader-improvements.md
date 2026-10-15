# Improving `dex-reader` and `jvm-reader` dataflow

Status: notes, nothing scheduled. These problems were found while designing the
CIL reader's dataflow layer ([`cil-flow-api.md`](cil-flow-api.md)), which is
built to avoid them. They are recorded here so the Dex and JVM readers can be
fixed the same way. One Dex instance is written up in detail in
[`bug-dex-reader-dataflow-roles.md`](bug-dex-reader-dataflow-roles.md).

## 1. What `dex-reader`'s pattern gets wrong

`dex_reader::instructions::DataFlow` is three boxed streaming iterators:

```rust
pub struct DataFlow {
    pub source: DynIter,   // Box<dyn DoubleEndedStreamingIterator<Item = Reg>>
    pub dest: DynIter,
    pub ret: DynIter,
}
```

Concrete problems, each visible in `ctadl-dex/src/lib.rs`:

1. **Three heap allocations per instruction**, plus `RegSliceStream` cloning the
   argument slice into a `Vec` for every call site. It also forces the
   `streaming-iterator` crate on every consumer (`ctadl-ascent` imports
   `StreamingIterator` solely to call `dest.owned()`).
2. **Bags, not roles.** `(source, dest, ret)` erases *which* source is the object
   base, which is the index, which is the stored value. So the consumer re-decodes
   the instruction anyway — `dataflow_to_assign`'s `IPut` arm reads
   `source.filter(|r| *r != f.b)` and then uses `f.b` directly for the object, and
   `APut` filters two registers out by hand. The dataflow view is doing half the job
   twice.
3. **No operand data.** Field/string/type indices are not in `DataFlow`, so the
   consumer goes back to `parser.get_field(f.idx)` / `constant_pool().strings`.
4. **Silently inconsistent.** `IPut*` lists the object register as a *source*:
   `dataflow_source_sink_ret!([f.a, f.b], [], [])`. `IPutWide` lists it as a *dest*:
   `dataflow_source_sink_ret!([f.a, f.a.next_reg()], [f.b], [])`. Nothing catches
   this because the one consumer ignores both fields for that opcode. There is no
   property that could be asserted, because there is no independent source of truth
   about pop/push counts in Dex.
5. **Untestable.** `DataFlow` derives nothing — no `Debug`, no `PartialEq`. There are
   no unit tests of `data_flow()` itself; the only coverage is 13 tests of the
   *frontend's* translation in `ctadl-dex/src/tests.rs`.

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
