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
| **initializer_list (`{...}`) not ingested** | frontend | 31 | 🟠 open |
| **labeled_empty_statement (`L: ;`) not ingested** | frontend | 32 | 🟡 open |
| **F3 — write through a local's address doesn't taint the local** | soundness | 34 | 🔴 open |
| **F4 — union field overlap not modeled** | soundness | 35 | 🔴 open |
| **F5 — non-constant subscript doesn't may-alias a constant index** | soundness | 36 | 🔴 open |
| **cast_expression not ingested** | frontend | 37 | 🟠 open |
| **conditional_expression (ternary) not ingested** | frontend | 38 | 🟠 open |
| **sizeof_expression not ingested** | frontend | 39 | 🟠 open |
| **designated_initializer not ingested** | frontend | 40 | 🟠 open |
| **deref_paren_field (`(*p).x`) panics** | frontend | 41 | 🟠 open |

F2 and the two `31`/`32` frontend findings were surfaced by the **broadened M7 generator** (it
threads taint through arrays, `switch`, `goto`, and function-pointer combinations, then scans at
volume). **F3–F5 and the five `37`–`41` frontend gaps (cases 34–41, added 2026-06-30) were
cross-validated from `treesitter_feature_branch`'s aspirational `#[ignore]` unit tests** — each
documented static gap was reproduced as a DFSan case, and the runtime ground truth confirmed every
one (3 `soundness-disagree`, 5 `frontend-error`). Resolved entries are kept as a record of what the
approach caught and how each was fixed. **10 findings remain open** (3 soundness: F3/F4/F5; 7
frontend ingestion: `initializer_list`, `labeled_empty_statement`, `cast_expression`,
`conditional_expression`, `sizeof_expression`, `designated_initializer`, `deref_paren_field`).
NB: `31`/`32` are fixed on `treesitter_feature_branch` but still open on this branch until merged.

**Allowlist linkage.** While a finding is open, its case is allowlisted by a `"known_gap": "Fn"`
(soundness) or `"known_frontend_gap": "<id>"` (ingestion) field in its `manifest.json` (see
[README.md](README.md)). That turns a raw gap into an expected `known-gap`/`known-frontend-gap`
instead of a run-failing `NEW-GAP`/`FRONTEND-ERROR`. When a fix lands, the harness reports the
case as `resolved-known-gap` / `resolved-known-frontend-gap`; the closure step is to remove that
field from the manifest **and** mark the entry below RESOLVED.

---

## F5 — Non-constant subscript doesn't may-alias a constant index (OPEN)

- **Status:** **open.** Allowlisted `"known_gap": "F5"` in
  [`cases/36_nonconstant_subscript`](cases/36_nonconstant_subscript/).
- **Symptom:** writing `a[n]` (with `n` non-constant) then reading `a[0]` drops taint. CTADL lowers
  a non-constant subscript to a distinct `[_elem_]` field symbol and a constant subscript `a[0]` to
  `[0]`, and treats the two as **disjoint** field paths — so `a[n] = src` never reaches `a[0]`. A
  sound analysis must let `a[n]` may-alias every concrete index (`n` could be 0):
  ```c
  int a[4]; int n = /* 0 at runtime */; a[n] = src; sink(a[0]);   // F5: static=none, dynamic=flow
  ```
- **Reproduces with:** [`cases/36_nonconstant_subscript`](cases/36_nonconstant_subscript/) (`n` is
  read from a `volatile` so it isn't constant-folded) → `static=none dynamic=flow` (`known-gap F5`).
  Contrast: keeping two **constant** indices distinct (`a[0]` vs `a[1]`) is correct precision, not a
  bug — that distinction is what makes this a may-alias question, not a collapse.
- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `nonconstant_subscript_may_alias_constant` (`#[ignore]`); reproduced here as a DFSan case.
- **Root cause:** not yet investigated. Candidate: a non-constant `[_elem_]` subscript should
  may-alias its constant-index siblings (`[N]`) in the taint-index field model.

---

## F4 — Union field overlap not modeled (OPEN)

- **Status:** **open.** Allowlisted `"known_gap": "F4"` in
  [`cases/35_union_field_overlap`](cases/35_union_field_overlap/).
- **Symptom:** writing one union member and reading another drops taint. A `union` aliases its
  members (they share storage), so `u.a = src; … u.b` carries taint. CTADL models a union like an
  ordinary **field-sensitive struct** — `.a` and `.b` are disjoint paths — so the overlap flow is
  dropped:
  ```c
  union U { int a; int b; }; union U u; u.a = src; sink(u.b);   // F4: static=none, dynamic=flow
  ```
- **Reproduces with:** [`cases/35_union_field_overlap`](cases/35_union_field_overlap/) →
  `static=none dynamic=flow` (`known-gap F4`).
- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `union_write_overlaps_other_field` (`#[ignore]`); reproduced here as a DFSan case.
- **Root cause:** not yet investigated. Needs a union/overlap model (e.g. collapse all members of a
  `union` type to a shared field, or make sibling union members may-alias).

---

## F3 — Write through a local's address doesn't taint the local (OPEN)

- **Status:** **open.** Allowlisted `"known_gap": "F3"` in
  [`cases/34_addr_of_local_alias`](cases/34_addr_of_local_alias/).
- **Symptom:** taking a local's address and writing through it (`int *p = &x; *p = src;`) does not
  taint `x`, so a later read of `x` is clean. CTADL handles **reading** through a pointer alias
  (case `14`) and `*out = src` through a pointer **parameter** (case `15`), but does not model a
  *local's* address being captured and written through:
  ```c
  int x = 0; int *p = &x; *p = src; sink(x);   // F3: static=none, dynamic=flow
  ```
- **Reproduces with:** [`cases/34_addr_of_local_alias`](cases/34_addr_of_local_alias/) →
  `static=none dynamic=flow` (`known-gap F3`). The contrast with cases `14`/`15` (both `OK`)
  localizes the gap to write-through a *local* alias (`&local`), not pointer handling in general.
- **Found by / cross-validated:** the `treesitter_feature_branch` aspirational unit test
  `address_of_local_aliases` (`#[ignore]`); reproduced here as a DFSan case.
- **Root cause:** not yet investigated. Candidate: `p = &x` doesn't bind `p`'s pointee to `x`, so
  the `*p = src` store updates an abstract pointee rather than `x` (needs local points-to / address-of
  modeling).

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

## labeled_empty_statement — a label on an empty statement (`L: ;`) doesn't parse (OPEN)

- **Status:** **open.** Allowlisted as `"known_frontend_gap": "labeled_empty_statement"` in
  [`cases/32_label_empty_statement`](cases/32_label_empty_statement/).
- **Symptom:** a `labeled_statement` whose body is the null statement (`done: ;`) fails — the bare
  `;` reaches `flatten_expr`'s catch-all (`ERR 78`). A label on a **real** statement
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
