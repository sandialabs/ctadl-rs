# Known findings

This file records **analyzer findings the comparison harness has surfaced** — soundness gaps and
frontend ingestion gaps — and tracks their disposition.

**Current state (2026-06-30):**

| Finding | Kind | Case(s) | Status |
|---------|------|---------|--------|
| F1 — indirect/function-pointer calls drop taint | soundness | 05/07/08/09 | ✅ resolved (`8484566`+`e01b58e`) |
| array_declarator not ingested | frontend | 18 | ✅ resolved (`d1ccd07`) |
| switch_statement not ingested | frontend | 25–29 | ✅ resolved (`5d5a695`) |
| F2 — multiple funcptr stores into an aggregate drop taint | soundness | 30, 33 | ✅ resolved (index-engine path fix) |
| initializer_list (`{...}`) not ingested | frontend | 31 | ✅ resolved (frontend lowering, 2026-06-30) |
| labeled_empty_statement (`L: ;`) not ingested | frontend | 32 | ✅ resolved (frontend lowering, 2026-07-01) |
| F3 — write through a local's address doesn't taint the local | soundness | 34 | ✅ resolved (frontend address-of alias, 2026-07-01) |
| F4 — union field overlap not modeled | soundness | 35 | ✅ resolved (frontend union-member collapse, 2026-07-01) |
| F5 — non-constant subscript doesn't may-alias a constant index | soundness | 36 | ✅ resolved (taint-index path may-alias, 2026-07-01) |
| cast_expression not ingested | frontend | 37 | ✅ resolved (frontend lowering, 2026-06-30) |
| conditional_expression (ternary) not ingested | frontend | 38 | ✅ resolved (frontend lowering, 2026-06-30) |
| sizeof_expression not ingested | frontend | 39 | ✅ resolved (frontend lowering, 2026-06-30) |
| designated_initializer not ingested | frontend | 40 | ✅ resolved (frontend lowering, 2026-06-30) |
| deref_paren_field (`(*p).x`) panics | frontend | 41 | ✅ resolved (frontend lowering, 2026-06-30) |

F2 and the two `31`/`32` frontend findings were surfaced by the **broadened M7 generator** (it
threads taint through arrays, `switch`, `goto`, and function-pointer combinations, then scans at
volume). **F3–F5 and the five `37`–`41` frontend gaps (cases 34–41, added 2026-06-30) were
cross-validated from `treesitter_feature_branch`'s aspirational `#[ignore]` unit tests** — each
documented static gap was reproduced as a DFSan case, and the runtime ground truth confirmed every
one (3 `soundness-disagree`, 5 `frontend-error`). **The five expression-level frontend gaps (37–41)
plus positional `initializer_list` (31) were then FIXED this session** in
`ctadl-ascent/src/languages/tree_sitter/mod.rs` — cast/sizeof/ternary arms in `flatten_expr`, a
parens/deref peel in `extract_field_expression`, and an `initializer_list` lowering that handles the
positional **and** designated (`{.a = e}`) forms — with matching unit tests in `tests.rs`; cases
`31`/`37`/`38`/`39`/`40`/`41` now run as plain `OK`. **`labeled_empty_statement` (case 32) was also
fixed (2026-07-01)** — `walk_statement` skips an empty `;` body (guards `child.child(0)` with
`!_is_empty`) — closing the last open frontend ingestion gap. **F3 (case 34) was also fixed
(2026-07-01)** — the frontend resolves a same-block dereference `*p` to its address-of pointee, so a
write `*p = src` through `int *p = &x` taints `x`. **F4 (case 35) was also fixed (2026-07-01)** — a
variable declared with a `union` type has its member accesses collapsed to one synthetic field, so
`u.a` and `u.b` alias (structs are untouched). **F5 (case 36) was also fixed (2026-07-01)** — the
taint-index path matcher (`match_prefix`) treats the non-constant-subscript symbol `[_elem_]` as
may-aliasing every concrete `[N]`, so `a[n] = src` reaches `sink(a[0])` while distinct constant
indices stay disjoint. **All findings are now resolved (0 open).** The frontend fixes here are also
on `treesitter_feature_branch` (which carries the un-ignored / live
aspirational tests).

**Allowlist linkage.** While a finding is open, its case is allowlisted by a `"known_gap": "Fn"`
(soundness) or `"known_frontend_gap": "<id>"` (ingestion) field in its `manifest.json` (see
[README.md](README.md)). That turns a raw gap into an expected `known-gap`/`known-frontend-gap`
instead of a run-failing `NEW-GAP`/`FRONTEND-ERROR`. When a fix lands, the harness reports the
case as `resolved-known-gap` / `resolved-known-frontend-gap`; the closure step is to remove that
field from the manifest **and** mark the entry below RESOLVED.

---

## F5 — Non-constant subscript doesn't may-alias a constant index (RESOLVED)

- **Status:** **resolved** (2026-07-01, taint-index path may-alias). The `known_gap: "F5"` allowlist
  was removed from [`cases/36_nonconstant_subscript`](cases/36_nonconstant_subscript/), which now runs
  as plain `OK` (`static=flow dynamic=flow`).
- **Was:** writing `a[n]` (with `n` non-constant) then reading `a[0]` dropped taint. The frontend
  lowers a non-constant subscript to the field symbol `[_elem_]` and a constant subscript `a[0]` to
  `[0]`, and the taint-index treated the two as **disjoint** — so `a[n] = src` never reached `a[0]`,
  even though `n` could be 0:
  ```c
  int a[4]; int n = /* 0 at runtime */; a[n] = src; sink(a[0]);   // was: static=none, dynamic=flow
  ```

### Root cause (exact field-path matching in the taint index)

Field-path propagation in both the index engine and the query engine goes through
`match_prefix`/`substitute_prefix` (`ctadl-ascent/src/facts.rs`), which compared field symbols by
**exact string equality**. `"[_elem_]"` and `"[0]"` are distinct interned symbols, so a store to
`a.[_elem_]` never matched a load of `a.[0]` (and vice versa). This is correct for two *constant*
indices (`[0]` vs `[1]` genuinely don't alias — sound precision) but wrong for the non-constant case,
which can be any element.

### The fix

Make `match_prefix` treat the non-constant-subscript sentinel `[_elem_]` as **may-aliasing** every
concrete bracketed index `[N]` (in either matching position), so a write/read through one is observed
at the other. Distinct constant indices are never `[_elem_]`, so they stay disjoint; the bracket
check keeps `[_elem_]` from aliasing non-subscript field symbols (struct members). One match arm plus
a small `subscripts_may_alias` helper in `facts.rs` — no frontend, codegen, or datalog-rule changes,
and it fixes both the index-engine and query-engine propagation (both call `match_prefix`).

### Confirmed by DFSan

Before: `36_nonconstant_subscript` reported `static=none dynamic=flow`. After: `static=flow
dynamic=flow` (`OK`); `scan cases/` reports **0 soundness-disagree** (only the 3 expected
precision-gaps remain). Tests: `nonconstant_subscript_may_alias_constant` and
`distinct_constant_subscripts_stay_disjoint` (`tree_sitter/tests.rs`, end-to-end) plus
`test_subscript_may_alias` (`facts.rs`, direct `match_prefix` unit test). Full `ctadl-ascent` suite
green (116 lib tests).

- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `nonconstant_subscript_may_alias_constant` (`#[ignore]`); reproduced here as DFSan case `36`.

---

## F4 — Union field overlap not modeled (RESOLVED)

- **Status:** **resolved** (2026-07-01, frontend union-member collapse). The `known_gap: "F4"`
  allowlist was removed from [`cases/35_union_field_overlap`](cases/35_union_field_overlap/), which
  now runs as plain `OK` (`static=flow dynamic=flow`).
- **Was:** writing one union member and reading another dropped taint. A `union` aliases its members
  (they share storage), so `u.a = src; … u.b` carries taint. CTADL modeled a union like an ordinary
  **field-sensitive struct** — `.a` and `.b` disjoint paths — so the overlap flow was dropped:
  ```c
  union U { int a; int b; }; union U u; u.a = src; sink(u.b);   // was: static=none, dynamic=flow
  ```

### Root cause (no type awareness in the frontend)

The tree-sitter C frontend did no type tracking at all (`toplevel` only walks functions; `union`/
`struct` type declarations were ignored), so a union member access lowered exactly like a struct
member access: `u.a = src` → `update u.a := src`, read `u.b` → `u.b`. Field-sensitivity is correct
for structs (disjoint members) but wrong for unions (all members overlap), so the flow was dropped.

### The fix

Teach the frontend which locals are unions and collapse their member accesses, entirely in
`ctadl-ascent/src/languages/tree_sitter/mod.rs`: `walk_declaration` flags a variable whose
declaration type is a `union_specifier` (`union U u;`, inline `union U { .. } u;`, or anonymous
`union { .. } u;`) in a `Context::union_vars` set; the `field_expression` arm of `flatten_expr` then
rewrites the accessed member (the first path segment) of any access off a union variable to a single
synthetic field `$union`. So `u.a` and `u.b` both become `u.$union` — a write to one member is
observed at a read of another — while structs (not in `union_vars`) keep genuinely disjoint fields.
No backend/index-engine changes.

**Scope / limitations (deliberate, no worse than before):** only locals *directly* declared with a
`union_specifier` are collapsed. Not yet covered — `typedef union { .. } U; U u;` (the declaration
type is a `type_identifier`, needs typedef tracking), union *parameters* and *globals*, pointer/array
union declarators, and unions *nested* as a struct field. Those still use the value-copy/field-
sensitive model (the old behaviour), so they are unchanged, not regressed.

### Confirmed by DFSan

Before: `35_union_field_overlap` reported `static=none dynamic=flow`. After: `static=flow
dynamic=flow` (`OK`); `scan cases/` drops to a single remaining soundness-disagree (F5). Regression
tests in `tests.rs`: `union_member_write_aliases_other_member`, `struct_members_stay_disjoint` (the
control proving struct fields stay disjoint). Full `ctadl-ascent` suite green (113 lib tests).

- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `union_write_overlaps_other_field` (`#[ignore]`); reproduced here as DFSan case `35`.

---

## F3 — Write through a local's address doesn't taint the local (RESOLVED)

- **Status:** **resolved** (2026-07-01, frontend address-of aliasing). The `known_gap: "F3"`
  allowlist was removed from [`cases/34_addr_of_local_alias`](cases/34_addr_of_local_alias/), which
  now runs as plain `OK` (`static=flow dynamic=flow`).
- **Was:** taking a local's address and writing through it (`int *p = &x; *p = src;`) did not taint
  `x`, so a later read of `x` was clean. CTADL handled **reading** through a pointer alias (case `14`)
  and `*out = src` through a pointer **parameter** (case `15`), but not a *local's* address being
  captured and written through:
  ```c
  int x = 0; int *p = &x; *p = src; sink(x);   // was: static=none, dynamic=flow
  ```

### Root cause (frontend value-copy pointer model)

The tree-sitter C frontend models pointers as **value copies** and does not distinguish `&`/`*` from
a plain access (`flatten_expr`'s `pointer_expression` arm passed the operand straight through). So
`int *p = &x` lowered to `assign p = x` and `*p = src` to `assign p = src` — the *pointer* `p`, not
the *pointee* `x`, received the value. This is sound for **reads** (`y = *p` → `y = p`, and `p`
carries `x`'s taint by the copy) but drops the **write-back**: the store deposited taint on `p`, and
nothing flowed from `p` back to `x`. The interprocedural out-param case (`15`) works via a different
mechanism (a `byref` formal + the call-site summary binding writes back to the caller's `&x`); there
is no such binding for a purely-local alias.

### The fix

An intraprocedural must-points-to for address-taken locals, entirely in the frontend
(`ctadl-ascent/src/languages/tree_sitter/mod.rs`): `collect_assignment` records `p = &x` in a
`Context::addr_alias` map (pointer `VariableRef` → pointee `AccessPath`, tagged with the current
basic block); `flatten_expr`'s `pointer_expression` arm then resolves a dereference `*p` — whether it
appears as a store LHS (`*p = src`) or a load RHS (`y = *p`) — to the pointee `x`, so the store lowers
to `assign x = src` (a real def of `x`). The binding is **keyed by basic block**, so it only applies
within the straight-line region where `&x` was taken; once control flow intervenes (or `p` is
reassigned to anything but `&x`), the lookup falls back to the value-copy model. That keeps the
must-points-to exact and never less sound than before (cross-branch may-alias is deliberately not
modeled). No backend/index-engine changes.

### Confirmed by DFSan

Before: `34_addr_of_local_alias` reported `static=none dynamic=flow`. After: `static=flow
dynamic=flow` (`OK`); `scan cases/` drops from 3 to 2 soundness-disagree (only F4/F5 remain).
Regression tests in `tests.rs`: `addr_of_local_write_through_taints_pointee`,
`addr_of_local_read_through_resolves_pointee`, `addr_of_alias_does_not_cross_basic_blocks`. The full
`ctadl-ascent` suite stays green (111 lib tests).

- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `address_of_local_aliases` (`#[ignore]`); reproduced here as DFSan case `34`.

---

## F2 — Static taint dropped through multiple function-pointer stores into an aggregate (RESOLVED)

- **Status:** **resolved.** Fixed in `ctadl-ascent/src/index_engine/mod.rs` (the taint-index
  datalog). The `known_gap: "F2"` allowlist was removed from case `30`; cases `30` (array form)
  and `33` (struct form) now run as plain `OK` regression tests.
- **Was:** a soundness false-negative whenever **two or more** function pointers were stored into
  the **same aggregate** (array OR struct) before the indirect call:
  ```c
  int (*fps[2])(int); fps[0] = id; fps[1] = id; int r = fps[0](s);   // F2: was static=none
  struct Ops { int (*a)(int); int (*b)(int); };
  struct Ops o;      o.a = id;     o.b = id;     int r = o.a(s);     // same gap
  ```
  A **single** store always worked (`05`/`07`/`08`/`09`) — which is why the F1 fix looked
  complete and this slipped through.

### Root cause (NOT codegen — the index engine)

Contrary to the initial guess, the F1 codegen fix already fired correctly here: the store lowers
to `update (%fps.[0] := ptr<id>)` and codegen emits the right `func_ptr_assign` fact for each
store. The gap was in **resolution**. The second store (`fps[1] = id`) creates a new SSA version
of the receiver (`%fps`), so the call `fps[0](s)` reads a *later* version than the one the binding
was recorded on. The binding must propagate across that version via the transitive
`func_ptr_assign_like` rule — which gates on `paths(p_new)`. But `program_paths`/`paths` was
populated **only from `actual_param`** (call-argument access paths); the *receiver* path of an
indirect call (`.[0]` / `.a`) was never registered. So the propagation silently failed and the
target never reached the call site.

### The fix

Register indirect-/virtual-call receiver paths as program paths, so the transitive propagation
can fire:

```rust
program_paths(p) <-- indirect_call(_, _, _, p);
program_paths(p) <-- java_call(_, _, _, p, _, _);   // same latent bug on the Java object path
```

(One-line root cause; the `java_call` line fixes the identical gap for `java_obj_assign_like`.)
Full `ctadl-ascent` test suite stays green (101 lib + integration tests).

### Confirmed by DFSan

Before: `30_funcptr_array_elem` / `33_funcptr_struct_multistore` reported `static=none
dynamic=flow`. After: both `static=flow dynamic=flow` (`OK`); `scan cases/` reports 0
soundness-disagree.

---

## F1 — Static taint dropped through indirect (function-pointer) calls (RESOLVED)

- **Status:** **resolved** (fixed on `treesitter_feature_branch`, merged to `auto_test` as
  `8484566` + `e01b58e`). The `known_gap: "F1"` allowlist has been removed from cases
  `05`/`07`/`08`/`09`; they now run as plain `OK` and serve as regression tests.
- **Was:** a soundness false-negative — CTADL did not carry taint through a call made via a
  function pointer, even though the tainted value really reached the sink at runtime.
- **Verified resolved by:** `cargo run -p ctadl-dynamic` reports `05`/`07`/`08`/`09` as
  `static=flow dynamic=flow` (all `OK`); `scan cases/` reports **0 soundness-disagree**.

### What it was

The corpus isolated the variable. Two cases call the same identity function `int id(int p)
{ return p; }`:

| Case                  | Call form                          | Static result (before fix) |
|-----------------------|------------------------------------|----------------------------|
| `03_call_summary`     | `r = id(s);` (direct)              | flow ✓        |
| `05_funcptr_indirect` | `fp = id; r = fp(s);` (indirect)   | **no flow** ✗ |

The only difference was the indirection, so the dropped taint was attributable to indirect-call
handling. The gap was **broad** — all four function-pointer forms dropped taint: `05` local
initialized (`int (*fp)(int) = id;`), `07` local assigned separately (`fp = id;`), `08` a
parameter (`int apply(int (*f)(int), int x)`), `09` a struct field (`o.op = id; o.op(s)`).

### The fix

The C frontend already *detected* indirect calls (emitting `funcptr-call`, pushing
`facts.indirect_call`); the gap was that the binding from the function value to the pointer
variable wasn't recorded, so indirect-call resolution had nothing to follow to the call site.
Two changes closed it:

- **Frontend** (`ctadl-ascent/src/languages/tree_sitter/mod.rs`): the variable-declarator query
  gained a `function_declarator` arm for parenthesized pointer declarators, so a
  function-pointer declaration `int (*fp)(int)` captures the pointer variable name (fixes the
  local forms `05`/`07`/`08`).
- **Codegen** (`ctadl-ascent/src/codegen/mod.rs`): the field-store form of the `Assign` arm now
  pushes a `func_ptr_assign` (and `java_obj_assign`) fact when the stored value is an
  `Exp::ObjectRef(CallObject::FunctionPtr(..))`, **before** `trans_exp` lowers the value (which
  returns `None` for an `ObjectRef` and would otherwise drop the binding). This records
  `o.op = id` at its field path so resolution can follow it (fixes the struct-field form `09`).

### How DFSan caught it

The harness flagged all four cases as `static=none dynamic=flow` — DFSan watched the `Test`
label reach the sink through the indirect call while CTADL reported no flow. That runtime
ground truth is exactly the kind of soundness violation this harness exists to find, and it now
confirms the fix the same way (both sides `flow`).

---

# Frontend ingestion gaps

The tree-sitter C frontend is incomplete: some valid C cannot be parsed/lowered to IR. These are
tracked separately from soundness gaps and allowlisted with `"known_frontend_gap": "<id>"` in a
case manifest (an un-allowlisted ingestion failure is a run-failing `FRONTEND-ERROR`). DFSan
compiles these with clang regardless, so each case already carries the runtime ground truth to
compare against the moment the frontend learns to ingest it (the harness will then report
`resolved-known-frontend-gap`).

## initializer_list — aggregate `{...}` initializers don't parse (OPEN)

- **Status:** **open.** Allowlisted as `"known_frontend_gap": "initializer_list"` in
  [`cases/31_aggregate_initializer`](cases/31_aggregate_initializer/).
- **Symptom:** an aggregate (brace) initializer fails with `ERR 78: Unsupported expression type:
  initializer_list`. The expression flattener (`flatten_expr` in
  `ctadl-ascent/src/languages/tree_sitter/mod.rs`) has no `initializer_list` arm, so the `{ ... }`
  node hits the catch-all. Affects **both** array and struct aggregates:
  ```c
  int a[2]      = { s, 0 };   // ERR 78
  struct P p    = { s, 0 };   // ERR 78 (same gap)
  int (*fps[2])(int) = { id, id };  // ERR 78 — this is what masked F2 until element-assignment
  ```
- **Reproduces with:** [`cases/31_aggregate_initializer`](cases/31_aggregate_initializer/) →
  `known-frontend-gap (initializer_list)`. DFSan observes `a[0] <- s`, so the expected result once
  it parses is `flow`. Everyday C — the broadest of the open frontend gaps.
- **Found by:** the broadened M7 generator (its array-of-function-pointers transform used a brace
  initializer; switching to element assignment dodged this and exposed F2).

## labeled_empty_statement — a label on an empty statement (`L: ;`) (RESOLVED)

- **Status:** **resolved** (2026-07-01, frontend lowering). Fixed in `walk_statement`: the
  `expression_statement`/`update_expression` arm now guards `child.child(0)` with `!_is_empty(..)`,
  so an empty `;` body carries no expression to lower and is skipped (rather than falling through to
  `flatten_expr`'s catch-all). The `known_frontend_gap` allowlist was removed from
  [`cases/32_label_empty_statement`](cases/32_label_empty_statement/) (now plain `OK`); regression
  test `labeled_empty_statement_parses` in `tests.rs`. Already live on `treesitter_feature_branch`.
- **Was:** a `labeled_statement` whose body is the null statement (`done: ;`) failed — the bare
  `;` reached `flatten_expr`'s catch-all (`ERR 78`). A label on a **real** statement
  (`done: r = r;`) ingests fine, so the gap is specific to the empty-statement body. (`goto` and
  labels generally work — added in `d1ccd07`; this is the one residual form.)
- **Reproduces with:** [`cases/32_label_empty_statement`](cases/32_label_empty_statement/) →
  `known-frontend-gap (labeled_empty_statement)`. The `goto` jumps over the kill, so DFSan observes
  the flow; expected result once it parses is `flow`.
- **Found by:** the broadened M7 generator (its goto transform labeled an empty statement).

## cast_expression — casts don't parse (OPEN)

- **Status:** **open.** Allowlisted `"known_frontend_gap": "cast_expression"` in
  [`cases/37_cast_expression`](cases/37_cast_expression/).
- **Symptom:** a cast `(long)x` fails `ERR 78` — `flatten_expr` has no `cast_expression` arm. A cast
  is value-preserving for taint, so the expected result once it lowers is `flow`.
- **Reproduces with:** [`cases/37_cast_expression`](cases/37_cast_expression/) →
  `known-frontend-gap (cast_expression)`. DFSan observes `flow`.
- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `cast_passthrough` (`#[ignore]`).

## conditional_expression (ternary) — doesn't parse (OPEN)

- **Status:** **open.** Allowlisted `"known_frontend_gap": "conditional_expression"` in
  [`cases/38_conditional_expression`](cases/38_conditional_expression/).
- **Symptom:** a ternary `c ? x : 0` fails `ERR 78`. The value is whichever arm is taken, so both
  arms should flow to the result; expected result once it lowers is `flow` (both arms).
- **Reproduces with:** [`cases/38_conditional_expression`](cases/38_conditional_expression/) →
  `known-frontend-gap (conditional_expression)`. DFSan observes `flow`.
- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `ternary_both_arms_flow` (`#[ignore]`).

## sizeof_expression — doesn't parse (OPEN)

- **Status:** **open.** Allowlisted `"known_frontend_gap": "sizeof_expression"` in
  [`cases/39_sizeof_expression`](cases/39_sizeof_expression/).
- **Symptom:** `sizeof(x)` fails `ERR 78`. Semantically `sizeof` does **not** evaluate its operand
  (it yields a compile-time size), so the correct result is **no flow** — this case has
  `expect_flow: false`. It is the one frontend gap whose expected post-fix result is a *negative*:
  once `sizeof` lowers, the operand must stay unevaluated and this becomes a no-flow regression test.
- **Reproduces with:** [`cases/39_sizeof_expression`](cases/39_sizeof_expression/) →
  `known-frontend-gap (sizeof_expression)`. DFSan correctly observes **no** flow.
- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `sizeof_does_not_evaluate` (`#[ignore]`).

## designated_initializer — designated brace initializers don't parse (OPEN)

- **Status:** **open.** Allowlisted `"known_frontend_gap": "designated_initializer"` in
  [`cases/40_designated_initializer`](cases/40_designated_initializer/).
- **Symptom:** `struct S s = { .a = x }` fails `ERR 78`. This is the **designated** form of
  `initializer_list`, tracked separately from the **positional** form (case `31`): on
  `treesitter_feature_branch` the positional form was fixed but the designated form remained a gap,
  so the two close independently. Expected result once it lowers is `flow`.
- **Reproduces with:** [`cases/40_designated_initializer`](cases/40_designated_initializer/) →
  `known-frontend-gap (designated_initializer)`. DFSan observes `flow`.
- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `designated_initializer_flows` (`#[ignore]`).

## deref_paren_field — `(*p).x` panics (OPEN)

- **Status:** **open.** Allowlisted `"known_frontend_gap": "deref_paren_field"` in
  [`cases/41_deref_paren_field`](cases/41_deref_paren_field/).
- **Symptom:** `(*p).x` (a parenthesized deref then field access) **panics** in `mod.rs` — the
  field-access handler expects particular `field_expression` object shapes and doesn't handle a
  parenthesized-deref object. Unlike the `ERR 78` gaps this is a *panic*, caught by the runner's
  `catch_unwind` and reported as `frontend-error`. It should be identical to `p->x`; expected result
  once handled is `flow`.
- **Reproduces with:** [`cases/41_deref_paren_field`](cases/41_deref_paren_field/) →
  `known-frontend-gap (deref_paren_field)`. DFSan observes `flow`.
- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `deref_paren_field_equivalent` (`#[ignore]`).

## array_declarator — array declarations are now ingested (RESOLVED)

- **Status:** **resolved** (`d1ccd07` "Treesitter array decl and goto"). The
  `known_frontend_gap: "array_declarator"` allowlist has been removed from
  `cases/18_array_subscript`, which now runs as plain `OK`.
- **Was:** a declaration like `int a[3];` failed with
  `ERR 78: Unsupported expression type: array_declarator` — the statement walker had no
  `array_declarator` arm, so it routed to `flatten_expr`, which had no case for it.
- **Fix:** `walk_declaration` in `ctadl-ascent/src/languages/tree_sitter/mod.rs` now handles
  `array_declarator` (alongside `pointer_declarator`/`function_declarator`). (Same commit also
  added `goto` lowering.)
- **Verified resolved by:** [`cases/18_array_subscript`](cases/18_array_subscript/) now reports
  `static=flow dynamic=flow` (`OK`); DFSan observes the flow `a[1] = source(); sink(a[1])`, and
  CTADL now agrees.

## switch_statement — `switch` is now ingested (RESOLVED)

- **Status:** **resolved.** `switch`/`case`/`default` (and the `break`/`continue` they need)
  are now lowered by the tree-sitter frontend. The `known_frontend_gap` allowlist has been
  removed from `cases/25_switch_taint`.
- **Was:** a `switch` statement failed with `ERR 78: Unsupported expression type:
  switch_statement` — the statement walker in `mod.rs` had no `switch_statement` arm, so it fell
  through to `flatten_expr`, which had no case for it.
- **Fix:** `walk_switch` in
  [`ctadl-ascent/src/languages/tree_sitter/mod.rs`](../ctadl-ascent/src/languages/tree_sitter/mod.rs)
  lowers a `switch` path-insensitively, the same way `if` is lowered — the entry block branches
  non-deterministically to every `case`/`default` arm, arms fall through to the next unless a
  `break` redirects to the switch continuation. `break`/`continue` resolve against per-construct
  target stacks on `Context` (also enabling `break`/`continue` inside loops). No backend changes.
- **Covered by:** unit tests `switch_case_flows_to_return`, `switch_default_flows_to_return`,
  `switch_fallthrough_flows_to_return`, `break_exits_loop_flows_to_return`,
  `continue_in_loop_flows_to_return` (`ctadl-ascent/.../tree_sitter/tests.rs`); and DFSan cases
  `25_switch_taint`, `26_switch_merge_paths`, `27_switch_untaken_case` (precision-gap, expected),
  `28_switch_default_taint`, `29_switch_fallthrough`.
