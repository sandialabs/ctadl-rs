# Flowy Syntax for the `Update` Instruction - DO-NOT-MERGE

Adds a surface syntax to the flowy textual IR (`ctadl-flowy`) for the `Update`
instruction restored in `9c9beb3`. Companion to `update-instruction.md`, which
covers the IR-level work and closes out its "flowy has no `update` syntax"
follow-up.

3 files, +117/-1 plus a 72-line test program.

## Syntax

```text
x = [y.foo := v];      // x is y, but with .foo set to v
x = [x.foo := v];      // a fresh version of x
x = [y.[8].foo := v];  // offsets are address arithmetic, as in a store
q = [p0.f := p1.g];    // the value may be a field read (loads first)
q = [p0.op := ptr<F>]; // ...or a function pointer
```

It reads "x *is* y, but with `.foo` replaced by v", which puts the source and
destination in exactly the positions the instruction wants them.

## Why brackets

Two alternatives were weighed: a keyword call form `x = update(y, .foo := v)`,
and an infix `x = y with .foo = v`.

| | brackets | `update(...)` | `with` |
|---|---|---|---|
| Introduces a reserved word | **no** | yes | yes |
| Can break an existing program | **no** | yes | yes |
| Visually distinct from a call | **yes** | no | yes |
| Matches how the IR prints | **nearly** | closest | no |

The deciding factor is the first row. Flowy has no reserved-word mechanism —
functions and variables can be named anything, and `parse_ref` treats an
unrecognized identifier as a local — so any keyword is a potential breaking
change. The brackets carry the whole syntax and cost nothing.

The runner-up matters too: the IR displays an `Update` as
`x = update (y.foo := v)`, so `:=` and the operand order already line up. A
dumped IR reads almost like the source that produced it.

## Grammar

`ctadl-flowy/src/flowy.pest` — one rule, placed first in `stmt`:

```pest
stmt = { update_stmt | assign_stmt | assign_call_stmt | call_stmt }

update_stmt = { ap ~ "=" ~ "[" ~ ap ~ ":=" ~ ap ~ "]" ~ ";" }
```

No ambiguity to resolve: `[` cannot begin an `ap`, so `assign_stmt` fails on it
and PEG backtracks regardless of ordering. Ordering it first is for clarity.
`[` is already used inside `offset_p` (`.[10]`) and `summaries`, but never in
statement position.

## Lowering

`ctadl-flowy/src/lib.rs`, a `Rule::update_stmt` arm in
`parse_stmt_or_terminator`. One statement lowers to exactly **one** `Update`
instruction — the surface form denotes the instruction rather than a lowering.

1. Reject globals on either side (see below).
2. `parse_ap` both operands. The destination must be a bare name.
3. Split the source path at its single symbolic field.
4. Lower the value with `lower_ref`, which emits loads for a field read.
5. Run the residual offset-only path through `load_access_path` — it merges
   consecutive offsets and emits nothing — and use the merged offsets as the
   `Update`'s destination path.

New helper `names_global` reports whether an operand's leading identifier
resolves to a global, mirroring `parse_ref`'s parameter-before-global order.

## Restrictions

Each is a compile error with a suggested form.

### The path names exactly one field

A nested update cannot be one instruction. An update produces a *new*
aggregate, so every enclosing aggregate must be rebuilt around it:
`x = [y.a.b := v]` means

```text
t0 = load y.a;
t1 = update (t0.b := v);
x  = update (y.a := t1);
```

This was implemented first, as a general `update_access_path` helper in
`ctadl-ir` (the functional counterpart of `store_access_path`), and the IR it
emitted was correct. **It was reverted because the engine cannot see through
it**: `return.a.b <- p1` is not derivable from that chain. Verified it is not a
defect in the lowering by spelling the chain out by hand in flowy — it fails
identically:

```text
def D3(p0, p1): 1
where summaries [return.a.b <- p1]
{
start:
  t = p0.a;
  t2 = [t.b := p1];
  q = [p0.a := t2];
  return q;
}
// Function D3 required summary flow is absent:   return.a.b <- @p1
```

This is the `substitute_prefix` gap already documented in `flowy_tests.rs` for
`substitute_prefix_demo.tnt`: the composed path `.a.b` is not a *syntactic*
program path, so the terminating `paths` gate stops it. The proper fix is the
planned Smaragdakis/Balatsouras points-to analysis.

A `Store` has no such problem, which is why `p0.a.b = p1; return p0` *does*
satisfy `return.a.b <- p1`. A store writes *through* `y.a` rather than
rebuilding it, and codegen's `cap_path` re-anchoring records the write at
`p0.a.b` directly as a single fact.

Auto-lowering the nested form would therefore silently under-approximate — bad
in a language whose job is testing the engine. Rejecting it makes the limit
visible at the point of writing.

### The destination takes no field path

`q.z = [p0.f := v]` is an error. The destination is the variable the
instruction *defines* — the fresh version of the aggregate — so it is a bare
name. The path being written belongs to the source, which is where it reads.

### Globals cannot be updated

A global is modeled as a symbolic field of the global heap, so `g` already
spends the one field an `Update` carries and `g.f` would need two. Nothing is
lost today: the pre-existing store baseline for the same shape already fails.

```text
var g;
def G2(p): 1
where summaries [return.f <- p]
{
start:
  g.f = p;
  r = g;
  return r;
}
// Function G2 required summary flow is absent:   return.f <- @p0
```

### Offsets are the exception

Offsets are address arithmetic, not field accesses, so they ride on the
update's destination address exactly as they ride on a store's:
`q = [p0.[8].field := p1]` becomes `%q = update (@p0.[0x8].field := @p1)`.

## Tests

`ctadl-ascent/tests/tnt/update.tnt`, 13 checks, picked up automatically by
`all_flowy_tests` (which globs the `tnt/` directory).

| Function | Covers |
|---|---|
| `Update` | both flows: `return.field <- p1` **and** `return <- p0` |
| `UpdateInPlace` | SSA versioning: `p0 = [p0.field := p1]` |
| `UpdateOffset` | offsets on the updated location |
| `UpdateFromField` | a value that is itself a field read |
| `UpdateTaint` | taint through an update, with an `errsink` on an untouched field |
| `UpdateFuncPtr` | a function pointer stored by an update, resolved at an indirect call |

The `return <- p0` requirement in `Update` is the discriminating one — it is
the whole-aggregate copy, which exists only because an update names its source
separately. Confirmed it is not vacuous by swapping the update for a store:

```text
def Store(p0, p1): 1
where summaries [return.field <- p1, return <- p0]
{
start:
  q.field = p1;
  return q;
}
// Function Store required summary flow is absent:   return <- @p0
```

`UpdateInPlace` is the reason the instruction exists at all. Its SSA output:

```text
@p0_1 = update (@p0_0.field := @p1_0)
```

The destination is versioned distinctly from the source it reads, which is what
naming the two apart buys and what a `Store` cannot express.

## Verification

Green: ctadl-ir 103 lib tests, ctadl-ascent 126 lib tests, all 36 `.tnt`
programs (the 35 pre-existing ones confirm the new `stmt` alternative changed
no existing parse), integration tests, `cargo fmt`, and
`cargo clippy --workspace --all-targets`.

Unrelated pre-existing issue: `cargo check -p ctadl-flowy` on its own fails
inside `serde-sarif` on a feature-unification problem (serde's `derive` feature
is not enabled when the build set excludes `ctadl-ascent`). Confirmed it fails
identically with these changes stashed. Build or test through `ctadl-ascent` or
the workspace.

## Follow-ups

- **Nested updates** — unblocked by the points-to analysis that also restores
  `substitute_prefix_demo.tnt`. The lowering is written down above and the
  reverted `update_access_path` is a small, well-understood addition to
  `ctadl-ir` when that lands.
- **Globals** — would need `Update` to carry a multi-segment field path, or the
  global heap to be reachable as an aggregate in its own right. Neither is
  worth doing before the store-based baseline works.
- **Round-tripping** — making `Display for StatementKind::Update` emit the
  bracket form would let IR dumps be re-parsed as flowy. Cheap, but it churns
  existing test expectations, so it was left alone.
