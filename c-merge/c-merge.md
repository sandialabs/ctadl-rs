# Merge cleanup: `c_merge_pre_preproc_cleanup` - DO-NOT-MERGE

This branch merges `origin/main` (`9535e572`, "Fix encoding in new testcases (#98)")
into the pre-preprocessor-cleanup tree-sitter C frontend. It arrived broken: the
workspace did not compile. This note records what was wrong, what changed, and what
is still known-failing on purpose.

Note the earlier merge write-up in `merge.md` describes a *different* line of C work
(the `origin/c` branch). That branch's frontend is roughly 650 lines ahead of this
one, and none of it was pulled in here. Where the two overlap, this note is what
applies to this branch.

## Starting state

`cargo build --workspace --all-targets` failed on two things:

- `xtask/src/discovery.rs` carried two byte-identical copies of `discover_lua` and
  `discover_jni` — a conflict resolution that kept both sides.
- `ctadl-ascent/tests/cli.rs` called `cli::index` and `cli::query` with main's old
  signatures. Main has since folded the tail arguments into `IndexOptions` and
  dropped a `bool` from `query`.

Once it compiled, `cargo test --workspace` failed 11 tree-sitter tests, `cargo xtask
regression` had never been wired to the C frontend's XFAIL list, and one integration
test failed for a real frontend gap.

## Changes

### 1. Removed the duplicated discovery functions

`xtask/src/discovery.rs`. Deleted the second copy of each. The two copies were
identical, so nothing had to be reconciled.

### 2. Fixed the `cli.rs` call sites

`ctadl-ascent/tests/cli.rs` now passes `cli::IndexOptions { strategy: …,
..Default::default() }` and calls `query` with five arguments. The values match what
`main.rs` passes for the CLI's own defaults, so the test exercises the shipped
configuration.

### 3. Updated 11 tests to main's access-path grammar

Main's #85 ("Canonicalize access path encoding") tightened the grammar: every path
segment carries a leading `.`, an unescaped `[N]` is a real `Offset(N)`, and a
*symbol* named `[N]` is written `\[N]`. The C tests were written against the old
spelling and git merged them in verbatim, so they either failed to parse
(`MissingLeadingDot`) or looked for a path the IR no longer prints.

Bare-path helpers now take a leading dot: `"a"` → `".a"`, `"a.b.c"` → `".a.b.c"`.
That is a pure spelling fix — a field named `a` is the same field either way — and
those tests pass.

Subscripts are a different matter; see the next section.

### 3a. Subscript tests assert offsets — now enforced, and the frontend was fixed

A constant subscript is pointer arithmetic, so it belongs in a path as `Offset(N)`,
spelled `.[N]`. Offsets are what make element paths *compose*: they are summed where
two paths meet. This frontend used to lower `a[N]` to one opaque `Symbol("[N]")` — a
symbol whose *name* contains brackets, spelled `.\[N]`. Nothing relates `Symbol("[1]")`
to `Symbol("[2]")`, so element paths never composed and `&a[i]` had no address to form.

Four tests spell a subscript path. They asserted the offset spelling and carried
`#[ignore]`. The `#[ignore]`s are gone and the frontend was fixed to match — see §7 for
the lowering and §8 for the tests added around it.

Tests that use subscripts in the *fixture* but assert only summary connectivity —
`array_declaration_element_flows_to_return`,
`array_of_struct_field_is_index_and_field_sensitive`,
`nonconstant_subscript_may_alias_constant` — spell no path and were unaffected. The two
DSL-parser tests in `test_utils.rs` (`subscript_is_symbol_segment`,
`unescaped_subscript_is_an_offset`) also still pass: they pin the *grammar*, which was
correct all along, not the frontend's choice of spelling.

### 4. Wired the C frontend into the xtask regression runner

Three pieces were present but disconnected — the compiler flagged all three as dead
code:

- `Task::Case` called `apply_jvm_allowlist` directly, so `apply_xfail_allowlists`
  (which composes the JVM and C allowlists) and `apply_c_allowlist` never ran, and
  `C_KNOWN_FAILURES` did nothing. `Task::Case` now calls `apply_xfail_allowlists`.
- `xtask/src/assertions.rs` had two byte-identical line collectors,
  `collect_codeflow_source_lines` (the C side) and `collect_codeflow_start_lines`
  (main's, for Lua). Kept main's name, folded the C half of the doc comment into it,
  and pointed `run_c` at it.
- Added `C:defaultmodels` to `C_KNOWN_FAILURES` (below).

`discovery.rs` already registered every `nightly/tests/c/*.c` under both C frontends
— the bare stem for pcode, `C:`-prefixed for tree-sitter — so `cargo xtask
regression --frontend c` now runs 22 C cases plus the three `models:*` checks.

### 5. Quarantined two C cases as XFAIL

`C:ptrarith` was already listed. `C:defaultmodels` is new, and it cannot pass by
construction: it is a known-answer test for the *shipped* propagation defaults. The
chain it asserts (`strcpy`/`strcat`/`strdup`) lives in
`models/defaults/native-index.jsonl`, which `models::default_model_file` hands only
to a `VirtualMethodTable::Native` program — the pcode import. A `-l c` import has no
method table (`Unknown`) and so loads no default file at all, leaving every step
between source and sink an unmodeled external. Confirmed by running the import with
`RUST_LOG=debug`: no default file is loaded, and the CHA pass warns "unsupported
virtual method table".

The pcode twin (`defaultmodels`, unprefixed) is what enforces those defaults, and it
passes. Both entries and the reasoning are documented in `nightly/README.md`.

### 6. `test_cli_query_c_sources_and_sinks` is enforced again

It was `#[ignore]`d for the same gap as §3a, one level up: the test needs the frontend
to form the address of an array element. `transfer(&x[1], s)` passes an address, the
callee writes one element past it, and `sink(x[2])` reads the result, so the flow exists
only if the two paths compose. The frontend lowered `&e` to a *load* of `e`'s value
rather than forming its address, so `transfer` received a copy and pointer identity was
gone before access paths were involved.

§7 fixes that. The `#[ignore]` is removed and the fixture is unchanged; only the doc
comment was rewritten, from an excuse into a description of what the case now pins.

### 7. Subscripts and address-of now lower to IR offsets

`a[N]` is `*(a + N)`, so it now lowers to *two* segments instead of one:

| C | before | after |
| --- | --- | --- |
| `a[3]` | `a.\[3]` — one `Symbol("[3]")` | `a.[3].deref` — `Offset(3)` then `Symbol("deref")` |
| `a[0]` | `a.\[0]` | `a.deref` — `Offset(0)` is the identity, so it is elided |
| `a[n]` | `a.\[_elem_]` | `a.deref` — no offset to name, so it *is* the `a[0]` path |
| `&a[1]` | a load of the element's value | the address `a.[1]` |

The index becomes pointer arithmetic on the *address*; the memory at that address becomes
the symbolic field read or written there. Splitting the two is the whole point: offsets are
summed where paths meet, so an address `a.[1]` that a callee writes at `.[1].deref` lands
on `a.[2].deref` — exactly where a caller's `a[2]` reads. A single `Symbol("[N]")` cannot
compose that way. The name and the `base.[off].deref` shape are pcode's, so the two C
frontends spell a memory access alike; see §9.

In `ctadl-ascent/src/languages/tree_sitter/mod.rs`:

- New `DEREF_FIELD` (`deref`), `is_deref_field`,
  `constant_index`, `push_element`, `deref_of_pointee`. `constant_index` is where
  `a[0x10]`, `a[3u]` and `a[3]` become the same offset, and where `a['c']` and `a[n]`
  become none.
- `flatten_lvalue`'s `subscript_expression` arm calls `push_element` instead of
  synthesizing a `[N]` symbol. Every subscript — read, store target, or the base of a
  further access — goes through this one arm, so the read and write sides cannot drift.
- New `flatten_address_of`, called from `flatten_expr`'s `pointer_expression` arm for
  `&e`. It forms an address for an element access (dropping the `deref` field, and
  emitting loads for any symbolic prefix such as `&s.a[1]`), and returns `None`
  otherwise so `&x` and `&s.f` keep the historical value-copy model. `&s.f` cannot be
  named: it would need `f`'s byte offset, which this frontend has no type information to
  compute.
- `deref_of_pointee` teaches the address-of alias map about interior pointees. `p = &x[1]`
  binds the address `x.[1]`, so `*p` is `x.[1].deref`, on both the read side
  (`flatten_expr`) and the store side (`flatten_lvalue`). A pointee that is a bare
  variable still *is* the value, as before.
- `add_assign_to_program` terminates an offset-only store target with `DEREF_FIELD`. A
  store must end in a symbolic field; writing through `p = &x[1]` produces a target that
  is otherwise all offsets, and without this it would trip `assign_or_store`'s assertion.
  Same move the pcode frontend makes with `.deref`.
- `lower_initializer_list` uses `push_element` for both positional elements and `[n]`
  designators, so `int a[2] = {s, 0}` deposits taint where a later `a[0]` read finds it.

### 7a. The may-alias hack left `facts.rs`

Commit `38f3f8b2` (F5) had taught `match_prefix` that a symbol named `[_elem_]` matches any
other bracket-delimited symbol — a `subscripts_may_alias` helper reading frontend spelling
conventions inside the engine's path matcher. That knowledge belongs to whoever chose the
spelling, so both the helper and its `match_prefix` arm are gone. `match_prefix` again treats
a symbol as an opaque name: symbols match when they are equal, and only offsets get
arithmetic. `symbol_segments_match_only_themselves` pins that.

The C frontend keeps the aliasing without the hack, by *spelling* it. After §7 the only
concrete index the hack ever reached was `a[0]` — `a[2]` is `.[2].deref`, whose offset segment
mismatches `[_elem_]` before any symbol comparison happens — so the sentinel bought exactly
one alias pair. `push_element` now emits the bare `DEREF_FIELD` for a non-constant index, which
*is* the `a[0]` path, so the two are one path and no matcher special case is needed.
`UNKNOWN_ELEM_FIELD` is deleted. Behavior for C is unchanged, including the gap: `a[n] = v` is
still not observed at `a[2]`.

Lua, which emits `[_elem_]` for a dynamic key and `Symbol("[3]")` for a literal one, loses the
aliasing the hack gave it and returns to main's behavior — a dynamic-key write does not reach
a literal-key read. Giving lua the same treatment is not a spelling change: its literal keys
are symbols, so collapsing them would flatten every constant key together. If that flow is
wanted, it is a lua frontend decision to make deliberately.

### 8. Nine subscript/address tests, all enforced

The four from §3a lost their `#[ignore]` and were rewritten to the two-segment spelling
(`f.[3].deref`, not `f.[3]` — the old assertions could not have passed either way, since a
load's path always ends in the symbol it loads). Five are new, covering the address-of
half that had no unit test at all:

- `address_of_element_forms_an_address` — `&x[1]` is passed as the access path `x.[1]`,
  and the element is never loaded.
- `address_of_element_zero_is_the_base_address` — `&a[0]` is the bare base, pinning the
  offset-eliding branch of `push_element`.
- `address_of_element_composes_with_callee_index` — the payoff, in both directions:
  `transfer(&x[1], s)` with `a[1] = b` in the callee taints `x[2]`, and does *not* taint
  `x[1]`. The precision half is what proves the arithmetic is real rather than an
  array-blind collapse.
- `store_through_element_address_alias_flows` — `p = &x[1]; *p = s;` is observed at
  `x[1]`.
- `address_of_struct_member_keeps_value_model` — `&s.f` stays a loaded value, pinning
  the documented limitation next to the fix.

One invariant ties the nine together, and §9 states it directly.

`test_utils.rs` gained `local_ref` and `call_args` (argument-shape assertions need a
`VariableRef` and a call's arguments, and any call site is a multi-function fixture that
`check_loads` cannot handle). `check_assign_or_update` now splits a store destination
into an offset address plus one trailing symbol, instead of demanding exactly one field.

Checked by reverting `mod.rs` alone and re-running: 8 of the 9 fail against the old
frontend. The ninth, `address_of_struct_member_keeps_value_model`, passes on both — it
pins behavior the fix deliberately left alone.

### 9. Offsets are pointer arithmetic; a dereference is `.deref`

§7 split a subscript into an offset plus a symbolic field, but named that field `[]` — the
spelling the dex and jvm frontends use for an array element. Those are typed array loads
with no pointer arithmetic to keep separate, so the name carried no claim about *when* it
appears. C needs a stronger rule, and the frontend now holds to it:

- An `Offset(N)` segment means pointer arithmetic and nothing else. It moves an address; it
  never reads or writes.
- Every access that touches memory ends in the symbolic field `deref`.

So a path never ends in an offset unless it *is* an address (`&a[1]` is `a.[1]`), and a load
from an array reads `a.[3].deref` rather than trailing off after the arithmetic. That is what
pcode already does — `base.[off].deref` — so the two C frontends now spell a memory access
the same way, which matters because the shared model files and query DSL name ports by path.

The change is a rename and its documentation: `ELEM_FIELD` (`[]`) → `DEREF_FIELD` (`deref`),
`is_element_field` → `is_deref_field`. No control flow moved. Test assertions lost the escape
they needed for a bracket-initial symbol (`r"f.[3].\[]"` → `"f.[3].deref"`); `deref` is an
ordinary name, so the DSL takes it plain. `subscript_is_symbol_segment` and
`unescaped_subscript_is_an_offset` still pin the escaping *grammar*, which no frontend's
choice of spelling can change.

Regression totals are unchanged (23 pass / 0 fail / 2 xfail), as is every summary the unit
tests assert; only the field's name moved.

## Verification

| Check | Result |
| --- | --- |
| `cargo build --workspace --all-targets` | clean, no warnings |
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy -- -Dwarnings` (CI's command) | exit 0 |
| `cargo test --workspace` | all pass (350 lib, 15 `tests/cli.rs` with none ignored, 20 xtask, rest green) |
| `cargo xtask regression --frontend c` | 23 pass / 0 fail / 2 xfail of 25 |
| `nix develop .#regression -c cargo xtask regression` | **128 pass / 0 fail / 2 xfail of 130** |

The two XFAILs are `C:defaultmodels` and `C:ptrarith`. Nothing is skipped.

The regression totals are unchanged by §7 — the C cases that pass, passed before. What
changed is what is *enforced*: five `#[ignore]`s are gone (four in `tests.rs`, one in
`tests/cli.rs`), and five new tests were added, so the lib count moved 345 → 350 and
`tests/cli.rs` moved 14 + 1 ignored → 15.

`git diff origin/main -- xtask/src/` deletes 12 lines, all of them reworded doc
comments, reworded error messages, or the one-line allowlist swap. The xtask half of
the merge is otherwise pure addition, which is the property to re-check if these
files are ever merged again.

## Known gaps, deliberately left

- **`C:ptrarith`.** `*(p + 2) = source()` needs the binary `+` to lower to an offset
  address. Same family as §7 and the last piece of it: a subscript and `&a[i]` now
  produce offset addresses, but pointer arithmetic written as arithmetic does not.
- **`&s.f` has no address.** Naming it needs `f`'s byte offset, and this frontend has no
  type information. `&s.f` keeps the value-copy model, so a callee's write through that
  pointer is dropped. Pinned by `address_of_struct_member_keeps_value_model`.
- **A non-constant index is approximated.** `a[n]` is a bare `deref` on the base
  address — the same path as `a[0]`, so a write through it is not silently lost. It does
  not reach a nonzero constant index: `a[n] = v` is not observed at `a[2]`. The remaining
  half of the F5 gap. Closing it is a frontend job (over-approximate the index at lowering
  time), not a path-matcher one.
- **`C:defaultmodels`.** Goes away if `-l c` imports ever get a default model file of
  their own.
- **`cargo clippy --workspace --all-targets`** fails on `rustc_graphviz/src/tests.rs`
  (`wrong_self_convention`). Vendored crate, untouched by either side of the merge,
  and not reachable by CI's `cargo clippy -- -Dwarnings`. Pre-existing.
