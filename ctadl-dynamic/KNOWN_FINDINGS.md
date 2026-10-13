# Known findings

This file records **analyzer findings the comparison harness has surfaced** — soundness gaps and
frontend ingestion gaps — and tracks their disposition.

**Current state (2026-06-29):**

| Finding | Kind | Case(s) | Status |
|---------|------|---------|--------|
| F1 — indirect/function-pointer calls drop taint | soundness | 05/07/08/09 | ✅ resolved (`8484566`+`e01b58e`) |
| array_declarator not ingested | frontend | 18 | ✅ resolved (`d1ccd07`) |
| switch_statement not ingested | frontend | 25–29 | ✅ resolved (`5d5a695`) |
| F2 — multiple funcptr stores into an aggregate drop taint | soundness | 30, 33 | ✅ resolved (index-engine path fix) |
| initializer_list (`{...}`) not ingested | frontend | 31, 34–36 | ✅ resolved (spec 019) |
| labeled_empty_statement (`L: ;`) not ingested | frontend | 32 | ✅ resolved (spec 018) |
| F3 — a non-constant subscript reads/writes only element 0 | soundness | 37, CPP_133 | ❌ **open** (IR path model) |
| F4 — a field path composed across a call is never admissible | soundness | 38, CPP_161, CPP_179, CPP_229 | ❌ **open** (codegen `paths` gate) |
| F5 — `(p + 1)->f` and `arr[1].f` lower to different paths | soundness | 39 | ❌ **open** (C pointer model) |
| F6 — a `static` local does not persist across calls | soundness | 40 | ❌ **open** (C frontend) |

F2 and the two frontend findings above were surfaced by the **broadened M7 generator** (it threads
taint through arrays, `switch`, `goto`, and function-pointer combinations, then scans at volume).
Resolved entries are kept as a record of what the approach caught and how each was fixed. **F3–F6
are open** (2026-10-13).

**Allowlist linkage.** While a finding is open, its case is allowlisted by a `"known_gap": "Fn"`
(soundness) or `"known_frontend_gap": "<id>"` (ingestion) field in its `manifest.json` (see
[README.md](README.md)). That turns a raw gap into an expected `known-gap`/`known-frontend-gap`
instead of a run-failing `NEW-GAP`/`FRONTEND-ERROR`. When a fix lands, the harness reports the
case as `resolved-known-gap` / `resolved-known-frontend-gap`; the closure step is to remove that
field from the manifest **and** mark the entry below RESOLVED.

---

## F6 — A `static` local does not persist across calls (OPEN)

- **Status:** **open**, allowlisted as `known_gap: "F6"` on case `40`. Found 2026-10-13 by the
  round-2 C++ candidate scan (`g26_static_local_counter`); C has it identically.
- **What:** `static int last = 0;` inside a function has static storage duration: one object for
  every call, initialized once. The frontend declares it as an ordinary local, so a value stored
  in one call is gone in the next.
- **Fix direction:** in `walk_declaration`, a declaration with a `static` storage-class
  specifier inside a function declares a global named after its function
  (`<function>::<name>`), resolved by `build_access_path`, with its initializer as the global's
  initialization rather than a per-call store. This is a C-frontend change for the C owners.

---

## F5 — `p->f` and `p[0].f` name a member at different paths (OPEN)

- **Status:** **open**, allowlisted as `known_gap: "F5"` on case `39`. Found 2026-10-13 while
  checking that C++ operator dispatch leaves pointer arithmetic alone (spec 031).
- **What:** in the C pointer model (tsfb `27ff7da3`, "prototype"), `p->f` lowers as `p.f` while
  `p[i].f` / `arr[1].f` lowers as `p.[i].deref.f`. One member written through one spelling is
  not read through the other:
  ```c
  arr[1].v = source(); struct V* q = arr + 1; sink(q->v);   /* s=none, d=flow */
  ```
  `p[1].v` (same spelling as the write) agrees.
- **Fix direction:** one path for "the record at address `e`": either `p->f` as `p.deref.f`, or
  subscripted records without the trailing `deref`. This belongs to the owners of the C pointer
  model.

---

## F4 — A field path composed across a call is never admissible (OPEN)

- **Status:** **open**, allowlisted as `known_gap: "F4"` on cases `38` (C), `CPP_161` (C++
  member-object receivers) and `CPP_179` (`operator->`). Found 2026-10-13 while adding member-object receivers (spec 030).
- **What:** taint lives at a path only if that path is in the `paths` gate. Codegen seeds the
  gate with syntactic paths and composes them along a **load chain within one basic block**
  (`cap_path` in `codegen/mod.rs`: `t = load o.in; u = load t.v` yields `.in.v`).
  `compute_paths` then adds only one-level concatenations with *summary/model* paths. A field
  read that happens **inside a callee**, on a value loaded from a field, therefore never makes
  the composed path admissible:
  ```c
  o.in = i;            /* i.v tainted */
  sink(get(o.in));     /* get reads p.v: needs o.in.v -- s=none, d=flow */
  ```
  The same program with `sink(o.in.v)` agrees, because the read spells `.in.v` in-block. A
  **getter returning a member pointer** fails the same way (`w.get()->v`, with `get` returning
  `p`: needs `w.p.v`, `.p` read in the callee), as does an overloaded `operator->` (CPP_179). In C++
  this hits every method call on a member object whose result depends on a member the callee
  reads: `o.in.set(s); sink(o.in.get())`, `in.get()` inside a method, a member object's
  destructor reading its own fields.
- **Not the frontend:** the frontend emits the right calls (receivers loaded from `this.in`,
  copy-in/copy-out). A frontend "hint" (dead loads of each member after the receiver load) would
  seed the path in the harness, but `ctadl index` runs dead-temp elimination first, so the CLI
  and the harness would disagree. It was rejected for that reason.
- **Fix direction:** compose actual-parameter capture paths with the callee's formal paths
  (`cap_path` of an actual × the paths its callee's formal reaches), or admit
  `program_paths × summary_paths` concatenations for computed summaries, not just model ones.
  Either is an index-engine change that main owns.

---

## F3 — A non-constant subscript only meets element 0 (OPEN)

- **Status:** **open**, allowlisted as `known_gap: "F3"` on cases `37` (C) and `CPP_133` (C++
  range-for). Found 2026-10-13 while adding range-for (spec 026).
- **What:** `a[i]` with a non-constant `i` lowers to `a.deref`, the same path as `a[0]`
  (`push_element`: offset 0 is elided, and a non-constant index has no offset to name). A write
  at a nonzero constant index is `a.[1].deref`, so the two never meet:
  ```c
  a[1] = source(); for (i = 0; i < 3; i++) sink(a[i]);   /* s=none, d=flow */
  for (i = 0; i < 3; i++) a[i] = source(); sink(a[1]);   /* s=none, d=flow */
  ```
  Writes and reads at `a[0]` and at non-constant indices do agree. Every loop over an array
  touches this.
- **History:** F5 on the old symbolic path model (`38f3f8b2`) fixed it with a `[_elem_]`
  sentinel that `match_prefix` let may-alias every `[N]`. main's Offset+deref model replaced
  symbolic subscripts with numeric offsets and the sentinel went with it; the module note in
  `frontends/ctadl-c/src/lib.rs` ("Non-constant subscript indices") records the regression as a
  limitation.
- **Fix direction:** an "unknown offset" `OffsetAccess`/`PathSegment` in `ctadl-ir` that the
  index and query engines' path matchers treat as may-aliasing every concrete offset, emitted by
  `push_element` for a non-constant index. This is a change to the IR and engines that main owns,
  not a frontend change.

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

- **Frontend** (`frontends/ctadl-c/src/lib.rs`): the variable-declarator query
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

## initializer_list — aggregate `{...}` initializers are now ingested (RESOLVED)

- **Status:** **resolved** (spec 019). The `known_frontend_gap: "initializer_list"` allowlist has
  been removed from [`cases/31_aggregate_initializer`](cases/31_aggregate_initializer/) **and**
  [`cases/CPP_31_aggregate_initializer`](cases/CPP_31_aggregate_initializer/), which now run as
  plain `OK`.
- **Was:** an aggregate (brace) initializer failed with `ERR 78: Unsupported expression type:
  initializer_list`. The declaration walker handed the `init_declarator`'s value to
  `collect_assignment`, and the expression flattener has no `initializer_list` arm, so the
  `{ ... }` node hit the catch-all. Affected **both** array and struct aggregates:
  ```c
  int a[2]      = { s, 0 };   // ERR 78
  struct P p    = { s, 0 };   // ERR 78 (same gap)
  int (*fps[2])(int) = { id, id };  // ERR 78 — this is what masked F2 until element-assignment
  ```
- **Fix:** `walk_declaration` in
  [`frontends/ctadl-c/src/lib.rs`](../frontends/ctadl-c/src/lib.rs)
  intercepts a value node of kind `initializer_list` and routes it to `collect_initializer_list`,
  which **desugars the brace element-wise into the assignments it stands for** — no new IR, no new
  taint rule, just the existing subscript-/field-/funcptr-store lowering:
  - an **array** element gets the `Offset(i)` + `deref` path a constant-index read `a[i]` resolves
    to (`Offset(0)` elided, so element 0 is `a.deref`);
  - a **record** element writes the *member* its position names, from a new neutral layout registry
    `Context::struct_layouts` (record tag → members in declaration order, filled once per
    translation unit by a `kind()` walk — not a query, since `class_specifier` exists only in the
    C++ grammar). Positional precision is the point: a write at `p.deref` is **not** observed at a
    read of `p.x`, so numbering a record's elements would silently drop taint rather than
    over-approximate it. For the same reason there is no "assign the aggregate's base path"
    fallback — measured, a write at a variable's root does not reach a later read of a field under
    it (propagation goes through `substitute_prefix`), so that "sound over-approximation" is
    actually a silent false negative.
  - nested braces recurse with whatever the inner level is: an array's element type, a member's own
    record type, or an **anonymous** inline record member's layout (carried on the slot itself,
    since it has no tag);
  - a **union**'s elements write the shared `$union` field, the same collapsed path a member read
    resolves to under the F4 union model;
  - designated initializers (`.x = e`, `[1] = e`) write the member/index they name.

  The node kind is identical in tree-sitter-c and tree-sitter-cpp, so this one shared-core change
  (no hook, no language branch, `parse_c_program` untouched) closed both mirrors. A C++ brace on a
  *class with a constructor* (`Widget w{…}`) is claimed earlier by the `construct` hook and is
  unaffected.
- **Residual (over-approximate, not precise):** where a record's layout is not knowable, the
  elements keep the pre-existing element numbering — the taint lands on `p.[i].deref` while a
  member read looks at `p.<name>`, so it is **dropped**. This happens for a record whose definition
  is not in the parse buffer (`unknown_record_type_falls_back_to_positional_elements`), a brace at
  a **pointer**/array member's position (`pointer_member_is_not_treated_as_an_inline_record`), and
  a record body the walk cannot read completely (an unnamed bitfield shifts every later position,
  so the layout is dropped rather than mis-mapped). Each is pinned by the named unit test above.
- **Verified resolved by:** [`cases/31_aggregate_initializer`](cases/31_aggregate_initializer/) and
  [`cases/CPP_31_aggregate_initializer`](cases/CPP_31_aggregate_initializer/) now report
  `static=flow dynamic=flow` (`OK`) — the harness first flagged both `resolved-known-frontend-gap`
  with the allowlists still in place. Locked in by six new cases: `34`/`CPP_95` (struct positional,
  `flow`), `35`/`CPP_96` (**negative** — the tainted element is in the *other* member's position, so
  `none`; this is what an over-approximating lowering would fail), `36`/`CPP_97` (funcptr array,
  `flow`). Unit-covered in both grammars (`array_aggregate_initializer`,
  `struct_aggregate_initializer_positional`/`_distinct_member`,
  `funcptr_array_aggregate_initializer`, `union_aggregate_initializer_writes_the_shared_field`,
  `anonymous_nested_record_initializer_maps_onto_members`/`_distinct_member`, and the `cpp_`-
  prefixed mirrors incl. `cpp_class_brace_init_is_not_an_aggregate`).
- **Found by:** the broadened M7 generator (its array-of-function-pointers transform used a brace
  initializer; switching to element assignment dodged this and exposed F2). The generator now
  carries brace-initializer transforms (array, struct, funcptr array) again.

## labeled_empty_statement — the null statement (`;`, incl. `L: ;`) is now ingested (RESOLVED)

- **Status:** **resolved** (spec 018). The `known_frontend_gap: "labeled_empty_statement"`
  allowlist has been removed from `cases/32_label_empty_statement` **and**
  `cases/CPP_32_label_empty_statement`, which now run as plain `OK`.
- **Was:** a `labeled_statement` whose body is the null statement (`done: ;`) failed — the bare
  `;` reached `flatten_expr`'s catch-all (`ERR 78`), taking the whole program down. A bare `;`
  anywhere in a body failed the same way; a label on a **real** statement (`done: r = r;`)
  ingested fine, so the gap was specific to the empty-statement body. (`goto` and labels
  generally worked — added in `d1ccd07`; this was the one residual form.)
- **Fix:** `walk_statement`'s `expression_statement` arm in
  [`frontends/ctadl-c/src/lib.rs`](../frontends/ctadl-c/src/lib.rs)
  now reads the **named** child instead of `child(0)`. The null statement parses as an
  `expression_statement` whose only child is the anonymous `;` token, so it now has no named child
  and lowers to a no-op (nothing emitted, control falls through unchanged) rather than handing the
  `;` to `flatten_expr`. Every non-empty expression statement is unaffected — its first child is
  always the expression. The node shape is identical in tree-sitter-c and tree-sitter-cpp, so this
  one shared-core change (no hook, no language branch) closed both mirrors.
- **Verified resolved by:** [`cases/32_label_empty_statement`](cases/32_label_empty_statement/) and
  [`cases/CPP_32_label_empty_statement`](cases/CPP_32_label_empty_statement/) now report
  `static=flow dynamic=flow` (`OK`) — the harness first flagged both `resolved-known-frontend-gap`
  with the allowlists still in place. Unit-covered by `null_statement_is_noop`,
  `labeled_null_statement_goto_target`, `trailing_labeled_null_statement` (C) and
  `cpp_null_statement_is_noop`, `cpp_labeled_null_statement_goto_target` (C++).
- **Found by:** the broadened M7 generator (its goto transform labeled an empty statement).

## array_declarator — array declarations are now ingested (RESOLVED)

- **Status:** **resolved** (`d1ccd07` "Treesitter array decl and goto"). The
  `known_frontend_gap: "array_declarator"` allowlist has been removed from
  `cases/18_array_subscript`, which now runs as plain `OK`.
- **Was:** a declaration like `int a[3];` failed with
  `ERR 78: Unsupported expression type: array_declarator` — the statement walker had no
  `array_declarator` arm, so it routed to `flatten_expr`, which had no case for it.
- **Fix:** `walk_declaration` in `frontends/ctadl-c/src/lib.rs` now handles
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
  [`frontends/ctadl-c/src/lib.rs`](../frontends/ctadl-c/src/lib.rs)
  lowers a `switch` path-insensitively, the same way `if` is lowered — the entry block branches
  non-deterministically to every `case`/`default` arm, arms fall through to the next unless a
  `break` redirects to the switch continuation. `break`/`continue` resolve against per-construct
  target stacks on `Context` (also enabling `break`/`continue` inside loops). No backend changes.
- **Covered by:** unit tests `switch_case_flows_to_return`, `switch_default_flows_to_return`,
  `switch_fallthrough_flows_to_return`, `break_exits_loop_flows_to_return`,
  `continue_in_loop_flows_to_return` (`ctadl-ascent/.../tree_sitter/tests.rs`); and DFSan cases
  `25_switch_taint`, `26_switch_merge_paths`, `27_switch_untaken_case` (precision-gap, expected),
  `28_switch_default_taint`, `29_switch_fallthrough`.
