# A value-only source in the IR (`Exp::Value`)

The plan for the sound form of "do not lower integer arithmetic as a merge". The experiment is
on branch `exp/int-arith-merge`: `da3ce42e` and `cc609511`, written up in its `problem.md` under
"Ideas tried, still open". That experiment showed the win, and showed that dropping operands at
import is not sound. This plan keeps the win and the taint.

## The problem

The pcode frontend lowers `t = a OP b` as `assign t = a, b`. The index reads an `Assign` source as
a copy of the whole value, so `t` is related to `a` at every path, extended by any suffix:

```
t                 <- a
t.deref           <- a.deref
t.[8].deref.deref <- a.[8].deref.deref
...
```

For a pointer copy that is right, and for `p + 4` it is roughly right. For integer arithmetic it
makes the result an alias of the memory behind every operand. That causes:

- **Cause 2 (`malloc`).** `nb = size + 11 & ~7` makes the size class alias `p0`'s memory. Every bin
  address computed from it becomes a path through `p0`, and the result is the dense
  `$globals <- p0` block that every caller inherits.
- **Cause 8 (libntfs).** `a->x + a->y` aliases both fields' memory. Storing the sum relates every
  field to every other: 9,560 summary rows per callee.
- **Cause 6 in `uclited`.** A table pointer indexed by the bit buffer shares all the buffer's
  paths.

**What the experiment showed.** Dropping integer operands at import (`ptr`, `none`) removes all
three:
- the allocator analyzed goes from 2.2-11.5x to 0.3-0.5x of budget;
- libntfs from 1.8x to 0.23x;
- `uclited` from 3.7x to 0.44x.

But it loses real taint through integers: the regression cases `loopflow` (`acc += data`) and
the 5 `Jni:JniWide*` cases (`(jint)(data >> 32) + used`). The `use` variant passes those only
because the sums happen to be returned or passed to a call.

## Why the decision is per operand, not per result

An obvious rule: if the program dereferences the result, merge; if not, only the value flows, so
the merge does no harm. It fails twice.

1. **A dereferenced result still has operands that only supply a value.** In `*(base + i)`, the
   memory reached is `base`'s; `i` picks an offset. `malloc`'s bin address is
   `bin_array + (idx << 3)`, with `idx` derived from `size`. The program does dereference it. A
   per-result rule gives the bin everything `idx` has, which is `p0`'s paths: exactly cause 2.
   `uclited`'s `table[bitbuf]` is the same shape. So the base gets the merge and the index gets
   value-only, even though the result is dereferenced.
2. **"Never dereferenced" is a whole-program question, and the index pays either way.**
   - The path set is program-wide. Once a path such as `p0.deref.[g].deref` exists anywhere, a
     merge into `t` materializes rows for it, whether or not anything reads `t.deref`.
   - libntfs's sum is only stored back. Nothing dereferences it, yet it produces the 9,560
     rows.
   - Whether `t` is ever dereferenced crosses memory (stored to `a->z`, loaded elsewhere), calls,
     and sink models on `arg.deref`. Answering it exactly would need a pointer analysis before
     this one.

So the frontend decides locally, from the op's shape. The base of a dereferenced `+`/`-` keeps
the full merge, and every other operand becomes value-only. When that guess is wrong, a real
pointer computed through integer arithmetic loses its aliasing but keeps its taint. Dropping the
operand loses both.

## Why a plain "empty-path-only edge" is not enough

A `locals` row `v.p <- a.q` means that `v.p` holds what formal `a.q` held on entry: the same
value, not just one derived from it. The formal-side rule depends on that: the `edge_split` /
`locals` join in `ctadl-index/src/index_engine/mod.rs`. If `v` is `a.q`, then `v.deref` is
`a.q.deref`.

Suppose a value-only edge produced an ordinary row `t <- a.q`. The first read of `t.deref` would
turn it back into `t.deref <- a.q.deref` through that rule. So a value-derived fact has to be
marked as weaker:
- it must never take part in suffix extension;
- it must stay marked through copies, stores, summaries and call sites.

## The design: a terminal `#value` field

Value-derived facts live under a reserved field on the destination. `t.#value <- a.q` reads "`t`'s
value is computed from `a.q`". `t` itself, and so `t.deref`, gets nothing from it.

### IR (`ctadl-ir/src/mir/mod.rs`)

A new source kind for `Assign`:

```rust
pub enum Exp {
    Variable(VariableRef),
    /// The value of `v`, not its identity: the destination's value depends on `v`'s, but
    /// nothing reachable through the destination is reachable through `v`.
    Value(VariableRef),
    ...
}
```

- `t = a + i`, with `a` the base, lowers to `assign t = a, value(i)`.
- `nb = size + 11` lowers to `assign nb = value(size)`.
- A comparison, `a < b`, lowers to `assign t = value(a), value(b)`.

The reserved name sits beside `DEREF_FIELD`, e.g. `VALUE_FIELD = "#value"`. It is spelled so that
no frontend's field name can collide with it.

### Codegen (`ctadl-ascent/src/codegen/mod.rs`, `visit_statement`)

Each `Exp::Value(v)` becomes two ordinary `assign` facts:

```
dest.#value <- v           v's own value
dest.#value <- v.#value    whatever v's value was already computed from
```

### Index (`PathSet`, `ctadl-index/src/index_engine/path_set.rs`)

One change:
- `p·#value` is admissible for every admissible `p`. This is implicit, a rule in the
  admissibility test, not stored, so the set does not double.
- Nothing may follow `#value`: `#value·rest` is never admissible.

With that, the existing rules do the right thing, with no new relation and no new join:

- **The destination side** (`ext_dst`). `t.#value <- v` takes only `v`'s root rows. Anything under
  `v` would need `#value·rest`, which is inadmissible. So `t.#value` learns that `v`'s value came
  from `a.q`, and nothing of `v`'s memory.
- **The formal side.** The source path of `t.#value <- v` is empty, so the only split is `(empty,
  empty)`: `t.#value` gets everything `v`'s root is reached from. For the second fact,
  `t.#value <- v.#value`, a root row `v <- a.q` gives `t.#value <- a.q·#value`. That is correct:
  if `v` is `a.q`, then `v`'s derived value is `a.q`'s derived value.
- **Copies carry it.** `u = t` gives `u.#value <- …`, with `#value` as the rest of the suffix.
- **Stores carry it.** `store a->z := t` gives `p0.deref.[z].#value <- p0.deref.[x]`: one row per
  summed field, with no `.deref.deref` cross product.
- **Dereferences ignore it.** `load t.deref` never touches `t.#value`, so a sum or a size class
  never gains memory identity.
- **Summaries and call sites carry it with no change.** A summary row `ret.#value <- p0` is an
  ordinary row; at a call site it becomes `r.#value <- n`.

### Query (`ctadl-ascent/src/query_engine/endpoints.rs`)

A source or sink on path `p` also matches `p.#value`.
- `system(arg0.deref)` counts `arg0.deref.#value` as tainted. That is how `sprintf(buf, "%d", n)`
  with a tainted `n` reaches the sink: the model `arg0.deref <- arg2` carries `arg2.#value` to
  `arg0.deref.#value`.
- A source on `ret.deref` also seeds rows whose formal path ends in `#value`.

This is the one change outside the index rules.

### IR passes

Every pass must see `Value(v)` as a use of `v`:
- `ssa`, `mem2reg`, `dead_temps` and `coalesce`;
- `copy_prop`, which must not treat `t = value(a)` as a copy. Today it matches only
  `Exp::Variable`, so it is safe, but that needs a test;
- the allocator detector (`ctadl-ascent/src/models/portcullis/allocator.rs`), which walks
  `Assign` sources. It must count `Value` operands as it counts `Variable` ones today, or its
  shape features shift.

### Frontend (`frontends/ctadl-pcode/src/lib.rs`, `handle_binop`)

Choose per operand, from the op's shape:

- **`INT_ADD`/`INT_SUB` whose result is dereferenced:** "dereferenced" means a `LOAD`/`STORE`
  address or a `PTRADD`/`PTRSUB` base, directly or through copies, phis, `+` and `-`
  (`address_vnodes` from `cc609511`). The base keeps `Exp::Variable`. The base is the
  pointer-typed operand, or else the one that is not a scaled index (the output of `INT_MULT`,
  `INT_LEFT`, or a constant). If neither is clearly the base, both keep `Exp::Variable`.
- **Every other operand** of every arithmetic op, comparison and bool op becomes `Exp::Value`.
  That includes the operands of a dereferenced op that are not the base.
- **Unchanged:** `MULTIEQUAL`, `PIECE` and `SUBPIECE` stay full merges.

A call argument or a return no longer counts as an address use, as it did in `use`. The value-only
fact carries the taint there, so the merge isn't needed to keep `loopflow` and `JniWide`.

Gate it at import as `CTADL_EXP_INT_ARITH=value`, so the existing scripts can measure it beside
`merge`, `ptr`, `none` and `use`.

## Expected effect

| Case | Expected |
|---|---|
| `malloc` | `nb.#value <- p0`; the bin address keeps only `bin_array`'s paths. The `$globals <- p0` block is gone. |
| libntfs | One `#value` row per summed field, not 9,560 per callee |
| `uclited` | The table pointer keeps the table's paths only |
| `loopflow`, `JniWide` | Kept: `acc.#value <- data`, then returned and matched by the sink |
| `strlen`'s `p - s` | `n.#value <- s`: the pointer value, not its contents. A length used to index a buffer no longer carries the string's taint. |
| `nginx` | Unchanged: `p = p + 1` is a base and stays a full merge. That is cause 6 and needs the offset collapse. |

## Open questions

- **Spurious root taint stays.** `malloc` splits the top chunk with `top = victim + nb`, so
  `ret.#value` can still trace back to `size`. That is one row per call rather than a block, but
  it is still the false flow problem.md notes ("`malloc`'s return should carry nothing from its
  size"). Staging step 7 addresses it, once the rest works.
- **Taint through the allocator's free lists stays.** With the allocator analyzed, `free(p)` links
  `p` into a bin in `$globals` and `malloc` returns from the bins. So `ret.deref` aliases every
  freed block's contents. That is memory flow, so neither `#value` nor step 7's `#addr` touches
  it. The allocator model covers it: the default skips the allocator's body (`8e5c4179` and the
  by-shape detector), and that should stay the default. Value-only lowering makes allocators the
  detector misses affordable to analyze; it does not replace the model.
- **Taint through an index, by design.** With `table[i]` and a tainted `i`, the loaded element
  is not tainted: the load reads `table`'s memory, and `i` contributes only `#value` to the
  address. That is the usual choice, but it is a choice, and a case should pin it down.
- **The length of a tainted string.** `n = strlen(s)` with `s.deref` tainted gives an untainted
  `n`, because taint is on the contents, not the pointer. Decide whether that is wanted.
- **Cost of the extra key.** `ps.splits` gives one more key per path that ends in `#value`. That
  needs measuring, and `edge_split` is already 3-4x `assign_like`.
- **Other frontends.** C, Lua, CIL and the JVM lower arithmetic their own way. Check whether any
  of them merges integer operands the same way and should emit `Value` too. Start with pcode only.
- **Models.** A model that names `#value` explicitly is probably never needed: models describe
  memory, and `#value` rides along by suffix. Check that the Portcullis model parser rejects or
  escapes the reserved name.

## Staging

1. **IR, codegen and `PathSet`.** Add `Exp::Value`, its two `assign` facts, `#value`
   admissibility and its terminality, and the IR passes' handling of the new use.
2. **Index unit tests** (`ctadl-index`, `ctadl-ascent/tests/integration`):
   - `t = value(a)`, with `a.deref` tainted, leaves `t.deref` clean and `t.#value` tainted;
   - copies, stores, summaries and call sites carry `#value`;
   - no path with `#value·rest` is ever derived;
   - `copy_prop` leaves `t = value(a)` alone.
3. **Query endpoint expansion,** with C regression cases:
   - a field sum stored back (no field mixing);
   - an index into a table;
   - `p + i` (aliasing kept);
   - `sprintf("%d")` of a tainted int reaching `system`;
   - `acc += data` (`loopflow` must pass under `value`).
4. **Frontend lowering,** behind `CTADL_EXP_INT_ARITH=value`.
5. **Measurement,** with the scripts in `/Volumes/Shampoo/nb-work/ia/` (`alloc/`, `libs/`,
   `prec/`):
   - the 5 allocator binaries, skipped and analyzed;
   - ntfsdebug, ntfslabel, `uclited`, `nginx`, and the 7 sweep binaries;
   - the regression suite;
   - findings diffed against `merge`.

   **Success means:**
   - the allocator analyzed is within about 1.5x of budget;
   - libntfs and `uclited` are under budget;
   - the regression suite gives 246 passed and the 3 expected failures;
   - no lost tainted path that is not explained as spurious.
6. **If it holds,** make it the default and drop the `ptr`, `none` and `use` variants.
7. **Later, only once steps 1-6 are implemented and work well:** separate address offsets from
   value flow (below).

## Later: separate address offsets from value flow (`#addr`)

Not part of the first implementation. Start it only once `Exp::Value` is in, is the default, and
measures well.

**Where the spurious root taint comes from.** The design above puts two different facts into
`#value`:
- "this value was computed from `x`";
- "this address was offset by `x`".

In `malloc`:

```
nb   = value(size)          nb.#value   <- p0
top' = victim + value(nb)   top'.#value <- p0      victim is the base; nb only the offset
store $globals.[top] := top'
...
victim = load $globals.[top]                victim.#value <- p0
ret    = victim + 8                         ret.#value    <- p0
```

`nb` is not data the pointer carries; it only decides where `top'` points. Because it lands in
`#value`, any sink on the pointer's value fires. Sinks on contents (`arg.deref.#value`) do not.
But root-level sinks such as `call-arg(n, 0)` do occur in the sweep's results.

**The fix: a second terminal field, `#addr`.** The index operand of address arithmetic goes to
`#addr` instead of `#value`:

| Operand | Facts |
|---|---|
| Base of a dereferenced `+`/`-` | full merge, as before |
| Index or offset of a dereferenced `+`/`-` | `dest.#addr <- i`, `i.#value`, `i.#addr` |
| Any operand of any other arithmetic, comparison or bool op | `dest.#value <- v`, `v.#value`, `v.#addr` |

- **The index treats `#addr` like `#value`.** It is terminal, it rides copies, stores, summaries
  and call sites by suffix, and dereferences ignore it.
- **The query does not.** Sinks match `p` and `p.#value`, and `p.#addr` only when a sink asks for
  it, e.g. a heap-layout or arbitrary-write query that wants "the attacker controls this pointer".
- **`malloc`:** `top'.#addr <- p0`, so `ret.#addr <- p0`. That is one row per call, and it is true:
  the size did move the pointer. No sink sees it by default.
- **Pointer difference turns an offset back into data, correctly.** `n = p - s` with `p = s + i` is
  a value op, so it reads `p.#addr` into `n.#value`, and `n` is tainted by `i`.
- **What stays spurious:** a pointer offset by a tainted size, then subtracted from a pointer it
  was never derived from (`end - r`, for `r = malloc(n)`). That is far narrower than tainting
  every allocation.

**Changes.**
- **IR:** `ADDR_FIELD = "#addr"` beside `VALUE_FIELD`, and an `Exp::Addr(v)` source; or one
  `Exp::Value` with a kind.
- **Codegen:** the three `assign` facts per row of the table.
- **`PathSet`:** admits `#addr` the way it admits `#value`.
- **Query:** match `#addr` on request.
- **Frontend:** a dereferenced `+`/`-` emits its non-base operands as `Exp::Addr`.

**Tests.**
- `malloc`-shaped code gives `ret.#addr` and no `ret.#value`.
- `p - s` with `p = s + i` and `i` tainted reaches a value sink.
- A pointer offset by a tainted index, passed to a root-level sink, does not fire.

**Measure** as in step 5. It should change no cost, since it is the same row count under another
field. It should only remove findings that trace a pointer's value to its offset.
