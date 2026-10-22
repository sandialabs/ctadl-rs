# Turning lattice relations into BYODS - DO-NOT-MERGE

Ascent rejects a custom data structure on a `lattice` relation ("`lattice`s cannot have custom
data structure providers", `ascent_macro-0.8.1/src/ascent_hir.rs`). This note asks whether a
BYODS store on a plain `relation` can still implement a mathematical lattice, and what that
trades.

The short answer is yes. Ascent leaves the three things that decide this to the data
structure. The store takes on correctness work that Ascent does for a real `lattice`, and in
exchange controls the layout, the delta and the indexes.

## How Ascent handles each kind

The code that inserts a derived row is in `ascent_macro-0.8.1/src/ascent_codegen.rs:1198-1265`.

**A plain `relation`** (which can use BYODS):

1. It checks `contains_key(total, row)` and `contains_key(delta, row)`.
2. If neither holds, it calls `insert_if_not_present(new, row)`.
3. If that returns true, it pushes the row to the physical relation, inserts it into every
   other index's `new` copy, and marks the relation changed.
4. At the end of each iteration, it calls `merge_delta_to_total_new_to_delta` on the indexes.

Ascent never looks inside the row. Whether a row is new, and what the next `delta` contains,
is up to the store.

**A `lattice`** (which cannot use BYODS):

- It looks the row up by every column except the last, then calls `join_mut` on the stored
  value in place.
- If the value grew, it re-inserts the row's position into the `new` indexes, so the next
  `delta` carries the row with its whole joined value.
- The parallel engine guards each row with an `RwLock` and takes a sharded mutex on first
  insert.

## Making a `relation` behave as a lattice

Declare the relation with the lattice value as its last column, `relation r(K..., V)`, and give
the store these meanings:

| Hook | Lattice meaning |
|---|---|
| `contains_key(total, (k, v))` | `v ⊑ total[k]` |
| `insert_if_not_present(new, (k, v))` | join `v` into `new[k]`; return true if it grew |
| `merge_delta_to_total_new_to_delta` | join `delta` into `total`; turn `new` into the next `delta`, minus what `total ⊔ delta` already holds |
| reading `total` or `delta` | one row `(k, value)` per key |

Ascent checks `total` and `delta` separately, not their join. So a `v` that is below
`total[k] ⊔ delta[k]` but below neither one still reaches `new`. The merge has to drop it, or
the fixpoint does redundant work. The results stay correct either way.

This gives lattice semantics with a value column.

### The per-element encoding

For a powerset lattice there is a second option, which `context_summary_store` uses: show
Ascent one row per element, `(k, d)`, and keep the set only in the physical layout. Taking the
union of sets is the same as taking the union of the per-element rows, so a plain relation of
elements has the same meaning. This is what makes an exact join on an element possible, such
as rule 3.2's join on `(f, d)`.

It only works while the lattice is a plain powerset. Collapse mode's ⊤ and the widening
bound's ⊤ are not sets of elements, so ⊤ needs its own relation (`context_summary_top`), and
every rule that reads the relation needs a ⊤ variant.

## What you gain

- **Indexes Ascent cannot express on a lattice.** In particular, with the per-element
  encoding, an exact join on an element of the set, which no index on a lattice's columns
  can give.
- **Layout.** Values can be interned and shared, rows can be grouped by value, and the store
  does not have to keep a copy of each row beside its indexes (`CountingVec` only counts).
- **The delta.** A `lattice` re-sends the whole joined value every time it grows. A store can
  send just the increment. That is sound only for rules that preserve joins, meaning each
  rule's result on `a ⊔ b` is its result on `a` joined with its result on `b`.
  - Rules that carry `ds` through unchanged, or iterate over `ds.ids()`, qualify.
  - Rules that test the value as a whole do not: `ds.is_top()`, `decision::spills(ds)`,
    size-based widening, collapse turning two singletons into ⊤. The increment also has to be
    defined as ⊤ when the row widens.
  - Sending the increment creates more distinct interned sets, which means more pressure on
    the union memo.
- **Parallelism.** The store can avoid Ascent's per-row `RwLock` and global insertion mutex.

## What it costs

- **Correctness is on you.** The `⊑` test, the join in the merge, the dedup described above,
  and keeping the `total`, `delta` and `new` copies consistent are all things the store has
  to get right. Ascent checks none of it.
- **The meaning depends on the encoding.** Ascent treats the relation as a set of tuples.
  With a value column, `(k, {a})` and `(k, {b})` are two tuples to Ascent and one row to the
  store. The physical relation's length counts accepted inserts, not keys. Anything that
  inspects the relation from outside the store, such as `len()`, the debug dumps or tests,
  has to go through the store.
- **Indexes are limited to the columns Ascent sees.** On the value-column encoding, an index
  on the value column means nothing because the value keeps changing. Only the per-element
  encoding opens an index on elements, and then Ascent does work per element, not per row.
- **Code.** `context_summary_store.rs` is about 940 lines for one relation, covering both
  engines. Doing this for more relations is only reasonable with a generic core shared by
  all of them.

## For this branch

- `context_assign` and `context_locals` would use the value-column encoding. That keeps the
  closure's work at one row per `(f, v, p, a, p4)`, as now.
- `context_summary` uses the per-element encoding, because rule 3.2 needs `d` as a column.
- All three could share one generic store core. Their rules would still differ in shape,
  because the two encodings show Ascent different columns.
