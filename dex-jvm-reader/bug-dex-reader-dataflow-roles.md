# Bug: `dex_reader`'s `DataFlow` gives the object register two different roles

**Component:** `dex-reader` (`readers/dex-reader/src/instructions.rs`), consumed by `frontends/ctadl-dex/src/lib.rs`
**Status:** open — present since the initial commit (`a3c69002`)
**Severity:** finding 1 is latent (no wrong output today, blocks any second consumer);
finding 2 is live (silently drops a real dataflow edge)
**Found by:** design review for the CIL frontend (`docs/dex-jvm-reader-improvements.md` §1, item 4)

---

## Finding 1 — `iput-wide` reports the object register as a *destination*

`Instruction::data_flow()` returns three register bags — `source`, `dest`, `ret`
(`readers/dex-reader/src/instructions.rs:1274`). For the `iput` family, Dex format 22c is
`iput vA, vB, field@CCCC`: `vA` (plus `vA+1` when wide) is the **value stored**, `vB` is
the **object written into**. `vB` is read, never written.

The table disagrees with itself about `vB`:

```rust
// readers/dex-reader/src/instructions.rs:2711
IPut(f) | IPutBoolean(f) | IPutByte(f) | IPutChar(f) | IPutShort(f) | IPutObject(f) => {
    dataflow_source_sink_ret!([f.a, f.b], [], [])          // vB is a SOURCE
}
// readers/dex-reader/src/instructions.rs:2714
IPutWide(f) => dataflow_source_sink_ret!([f.a, f.a.next_reg()], [f.b], [])  // vB is a DEST
```

`[f.b]` in the dest position claims `iput-wide` *defines* `vB`. It does not. Every
sibling store opcode takes the other choice, so `IPutWide` is the sole outlier in the
table:

| Opcode | source | dest | object/array register role |
| --- | --- | --- | --- |
| `IPut*` (`:2692`) | `[a, b]` | `[]` | source ✓ |
| `IPutWide` (`:2695`) | `[a, a+1]` | `[b]` | **dest ✗** |
| `SPutWide` (`:2685`) | `[a, a+1]` | `[]` | n/a (static) ✓ |
| `APut*` (`:2783`) | `[a, b, c]` | `[]` | source ✓ |
| `APutWide` (`:2785`) | `[a, a+1, b, c]` | `[]` | source ✓ |

### Why nothing catches it

1. **The one consumer ignores both fields for this opcode.**
   `dataflow_to_assign` handles all seven `IPut*` variants in one arm
   (`frontends/ctadl-dex/src/lib.rs:795-820`): it filters `f.b` back *out* of
   `source` (`:758`), takes the object from `f.b` directly (`:761`), and never reads
   `dest` on this path. So the erroneous `dest` is discarded and the output is correct
   today.
2. **`DataFlow` derives nothing** — no `Debug`, no `PartialEq` (`:1255`), and its three
   fields are `Box<dyn DoubleEndedStreamingIterator>`. A unit test cannot compare two
   `DataFlow`s, and there are no tests of `data_flow()` anywhere in `dex-reader`; the
   only coverage is 9 tests of the *frontend's* translation in
   `frontends/ctadl-dex/src/tests.rs`, none of which exercise `iput-wide`.
3. **There is no invariant to assert against.** Dex has no pop/push table, so unlike
   CIL (whose `OpCode` carries `StackBehaviour`) there is no independent source of truth
   a property test could cross-check `data_flow()` against.

### What breaks when it stops being invisible

Any consumer that reads `dest` as "the registers this instruction defines" — a liveness
pass, a def-use chain, an SSA renamer, a register-level slicer — will conclude that
`iput-wide` kills `vB`. The object reference's incoming flow is cut at that point, and
every later use of `vB` is attributed to a definition that does not exist. The bug is
one `data_flow()` caller away from being a wrong-answer bug.

### Fix

One line, and behavior-preserving for the current consumer (which filters `f.b` out of
`source` anyway):

```rust
IPutWide(f) => dataflow_source_sink_ret!([f.a, f.a.next_reg(), f.b], [], []),
```

---

## Finding 2 — self-aliased `iput`/`aput` silently drop the stored value

Same root cause, but this one is wrong *today*.

The consumer recovers the value operands by removing the object register **by value**:

```rust
// frontends/ctadl-dex/src/lib.rs:806-810  (IPut* arm)
source.cloned().filter(|r| *r != f.b) ...
// frontends/ctadl-dex/src/lib.rs:872-877  (APut* arm)
source.cloned().filter(|r| *r != f.b && *r != f.c) ...
```

When the value register and the object register are the *same* register, that filter
removes the value too, and the generated statement is
`assign(temp, [])` — a temp with no sources — followed by `store(object.field, temp)`.
The edge `vX -> vX.field` is lost.

`iput-object p0, p0, LFoo;->cb:LBar;` is exactly this shape, and it is what
`this.mCallback = this` compiles to — a common Android idiom (self-registering
listeners, self-referential nodes). `aput-object v1, v1, v2` (`a[i] = a`) is the array
analogue.

The filter is only there because `source` is a bag with the roles erased; with roles
attached, no filtering is needed and the aliasing case cannot arise.

### Fix

Do not recover operands by value comparison. Take the value operands positionally from
the instruction fields (`f.a`, plus `f.a.next_reg()` for the wide variants) instead of
filtering `source`. Longer term, see below.

---

## Root cause

`DataFlow`'s `(source, dest, ret)` shape records *whether* a register participates, not
*which role* it plays. The consumer needs the roles, so it re-decodes the instruction —
and the two decodings can drift apart (finding 1) or the reconstruction can be
ambiguous (finding 2). Two further symptoms of the same shape:

- **`ret` is dead.** All 67 `dataflow_source_sink_ret!` invocations pass `[]` for it, and
  `dataflow_from_args` sets it empty; the sole consumer binds it as `ret: _`
  (`frontends/ctadl-dex/src/lib.rs:665`). The field costs a boxed empty iterator
  per instruction and carries no information.
- **Three heap allocations per instruction**, plus `RegSliceStream::new` copying the
  argument slice into a `Vec` at every call site, and a `streaming-iterator` dependency
  forced on every consumer.

The structural fix is a role-carrying, `PartialEq`-derivable per-instruction view — the
design being written for `cil_reader::flow` (`docs/cil-flow-api.md`). If
`dex-reader` is reworked, that is the shape to converge on.

---

## Regression protection

Any of these would have caught finding 1:

1. `#[derive(Debug, Clone, PartialEq)]` on a non-boxed `DataFlow` (e.g. `SmallVec<[Reg; 4]>`
   fields), plus per-opcode-family unit tests asserting the exact register lists.
2. A table-level property test: for every store-family opcode, the object/array register
   appears in `source` and not in `dest`.
3. A frontend test over `iput-wide` and `iput-object p0, p0, F` in
   `frontends/ctadl-dex/src/tests.rs` — neither shape is covered by the current 9.

## References

- `readers/dex-reader/src/instructions.rs:1274-1282` — `DataFlow`
- `readers/dex-reader/src/instructions.rs:2711-2714` — the inconsistent arms
- `readers/dex-reader/src/instructions.rs:2801-2804` — `APut*`, for contrast
- `frontends/ctadl-dex/src/lib.rs:655-674, 795-820, 863-884` — the sole consumer
- `docs/dex-jvm-reader-improvements.md` §1 — where this was first written up
