# Restoring the `Update` Instruction - DO-NOT-MERGE

Re-adds a form of the `Update` instruction that was removed by
`74b7445` ("Move IR to Load & Store Instructions", #53), which replaced the
functional `Update` with the current `Load`/`Store` pair.

Implemented in commit `9c9beb3` ("Add update instruction"), 8 files, +358/-11.

## Design

`Update` is exactly the current `Store` instruction, except it names the **source
aggregate separately** from the destination. Like `Store`, it writes a single
symbolic `FieldPath`.

```rust
Update {
    /// Destination aggregate address (offset-only path, like `Store`).
    /// This instruction DEFINES `dest.variable_ref`.
    dest: AccessPath,
    /// Source aggregate copied into `dest` before the field write, named
    /// separately so SSA can version `dest` and `source` independently.
    source: VariableRef,
    /// Symbolic field written at `dest` (a single symbol, like `Store`).
    field: FieldPath,
    /// Value to store
    value: Exp,
}
```

Semantics: `dest = update (source, dest.field := value)` — the result `dest` is
`source` with `dest.path ++ field` set to `value`.

### How it differs from `Store`

| | `Store` | `Update` |
|---|---|---|
| Field written | one `FieldPath` | one `FieldPath` (same) |
| Destination | `AccessPath` (offset-only address) | `AccessPath` (same) |
| Source aggregate | — (only a destination) | explicit `source: VariableRef` |
| Defines a variable? | **No** — `dest` is read as a location | **Yes** — defines `dest.variable_ref` |
| SSA effect | no new version of the aggregate | fresh version of the aggregate |

The whole point of naming source and destination separately is SSA: it lets the
conversion rename the destination to a fresh version while still reading the
previous source. `s = update (s, .foo := new_value)` becomes
`s_2 = update (s_1, .foo := new_value)`.

This mirrors the rationale in the original (pre-removal) doc comment: *"It's
important to explicitly specify the source and destination so that SSA
conversion can rename the dest after the update."*

The one shape change versus the historical instruction: the old `Update` carried
`dest: (VariableRef, FieldAccesses)` — a variable plus a *multi-segment* field
path that could mix symbols and offsets. Since #53 split those types, the modern
equivalent is `dest: AccessPath` (variable + offset-only path) plus
`field: FieldPath` (the one symbol) — i.e. precisely the `Store` shape.

## Changes by area

### IR (`ctadl-ir/src/mir/mod.rs`)

- Added the `StatementKind::Update` variant.
- Added the `StatementKind::update(dest, source, field, value)` constructor,
  next to `store`.
- Def/use iterators wired so SSA does the right thing:
  - `iter_dst_var` / `iter_dst_var_mut` yield `dest.variable_ref` (defined).
  - `iter_src_var` / `iter_src_var_mut` yield `source` + `value`'s base
    variable (read). The destination is **not** a read: unlike `Store`, whose
    `dest` is an address it loads from, an `Update`'s destination is purely an
    output.
- `Display`: `q = update (p0.field := v)`.
- Module docs note `Update` as the functional-update alternative to `Store`.

### Visitor (`ctadl-ir/src/mir/visit/mod.rs`)

Added the `Update` arm to `super_statement_kind`: visits `dest` (access path),
`source` (variable ref), and `value` (exp).

### SSA (restored)

SSA renaming is driven by `iter_dst_var`/`iter_src_var`, so wiring those
restores versioning: the destination gets a fresh version, the source keeps the
incoming one.

Supporting passes:

- **`coalesce`** — added an explicit `Update` arm that substitutes pending
  copies into `value`, same as `Store`. (`source` is a bare `VariableRef`, not
  an `Exp`, so it is not an `Exp`-substitution site.) Invalidation already works
  via the generic `iter_dst_var`.
- **`copy_prop`** — no change needed; rewrites uses through
  `iter_src_var_mut`, and `Update` is not a copy so it is never registered as an
  alias.
- **`dead_temps`** — no change needed. `removable_dest` returns `None` for
  `Update`, so it is never deleted: like `Store` and `CallAssign`, it is
  treated as side-effecting.

### Codegen (`ctadl-ascent/src/codegen/mod.rs`)

An `Update` lowers to the two flows the original instruction produced:

1. **whole-aggregate copy** — `dest ← source` (both empty paths)
2. **field write** — `dest.(offsets ++ field) ← value`

plus the function-pointer / Java-object handling mirroring the current `Store`
arm (`call_target_assign`), so a callable stored into a field is still
resolvable at an indirect call site.

`Update` deliberately does **not** consult `cap_path` (the load-chain
re-anchoring that `Store`/`Load` use). That machinery exists because a write
through a load temporary (`store t2.y := v`) must be recorded at the formal path
it addresses (`v.f2.nf1.y`) rather than at a temporary no summary can name. An
`Update` has no such problem: its destination is a freshly-defined, nameable
aggregate. This also matches the pre-removal codegen, which predates `cap_path`.

### Frontend ergonomics

- `BasicBlockBuilder::create_update(dest, source, field, value)` added alongside
  `create_store` (`ctadl-ir/src/mir/builder.rs`).
- The model-generator field matcher (`ctadl-ascent/src/models/json.rs`) now
  recognizes field writes via `Update` in addition to `Load`/`Store`.

## Tests

Both new tests pass; full suites are green (ctadl-ir 103 + ctadl-ascent 126
lib tests, plus integration tests).

- **`ctadl-ir` — `ssa::tests::test_ssa_function_h_update`**
  The functional-update counterpart of the existing `program_h`. Builds
  `%global = update(%global, .bar := p)`, runs SSA, and asserts the destination
  is versioned *distinctly* from the source it reads — i.e.
  `%global_1 = update(%global_0, .bar := p_0)`.

- **`ctadl-ascent` — `codegen::tests::test_real_update_instruction`**
  Builds `q = update(p0, .field := p1); return q` and checks the taint summary
  contains **both** expected flows:
  - the field write, `p1 → ret.field`, and
  - the whole-aggregate copy, `p0 → ret`.

  The second is the discriminating assertion: it exists only because `Update`
  copies the entire source aggregate. An equivalent `Store` would not produce
  it. (Test helper `function_with_real_update`; note the pre-existing
  `function_with_update` was converted to a `Store` by #53 and is left as-is.)

## flowy syntax

`.tnt`/flowy sources can now author an `Update` with the bracket form
`x = [y.foo := v]`. See `update-instruction-in-flowy.md` for the grammar, the
lowering, the restrictions it imposes (one field, bare destination, no globals)
and why, and `ctadl-ascent/tests/tnt/update.tnt`.

## Scope / follow-ups

No frontend other than flowy emits `Update` — the request was to give frontends
the **option**, which the constructor and builder API provide. One place would
need a small extension if another frontend adopts it:

- **tree-sitter test helper** `writes_dest` (`languages/tree_sitter/test_utils.rs`)
  recognizes only `Store` writes, so `check_writes_to` would not count an
  `Update`.

This blocks nothing today: it falls through a `_ =>` catch-all.
