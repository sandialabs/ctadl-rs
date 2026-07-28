# Propagating call targets through returns - DO-NOT-MERGE

A language-neutral gap in `index_engine`: a `CallTargetObject` created inside a callee never
reaches its callers. Any frontend whose dispatch depends on tracking a call target out of a
factory hits it — a C function pointer manufactured and returned (`h = lookup(); h();`), a
returned object whose concrete type drives virtual dispatch, a Lua metatable instance
(`acct = Account.new()`). Frontends carrying a *declared* type at the call site (JVM/Dex) are
insulated because CHA resolves without dataflow; for the rest the tag path is the only route.

## The gap

Tags cross frames in exactly one direction. Rule 2.1 (`index_engine/mod.rs:1152`) projects a
*caller's* `call_target_assign_like` onto a callee's `critical_summary` formal — that is how
argument-passed receivers resolve. There is no return-direction twin.

Data flow itself is compositional — `summary` (`:1105`) relates out-formals to in-formals in one
step and `:1096` instantiates it caller-side — but the tag does not ride it:

- `locals` is `(FunctionId, FlowVariable, Path, FormalIndex, Path)` (`:986`), seeded only from
  `formal_param` (`:1056`), so `summary` is formal-to-formal **by construction**. A callee that
  returns a freshly created object/pointer has no in-formal source, hence no summary tuple.
- Every `assign_like` tuple is single-frame (one `FunctionId`, both endpoints its variables) even
  though its derivation is interprocedural, and `:1268` seeds `call_target_assign_like` only from
  the *same* function's `call_target_assign` facts. So the closure can redistribute tags a frame
  already holds; it can never introduce one.

Consequence: the tag crosses a return **iff** the returned value is reachable from an in-formal
(pass-through). Allocation-in-callee dies at the boundary, and the call site falls back to whatever
static over-approximation the frontend supplies.

Reproducer on this branch: `nightly/tests/lua/metatable-oop-account.lua`. The tag is a fact of
`Account.new`'s frame, `acct` in `main` has none, the `:1254` bypass never fires
(`Resolvent (0)` / `critical_summary 0/7`, per `cha-summary.md`), and dispatch degrades to the
method-name union at `codegen/mod.rs:539`.

## Step 1 — Widen the tag-closure gate

`:1268` and `:1272` are gated on `critical_call(func_id)`. A pure factory contains no indirect call
and calls nothing with a critical summary, so its tag closure is empty and the tag never reaches
its own return formal. Introduce:

```rust
relation tag_closure_func(FunctionId);
tag_closure_func(f) <-- critical_call(f);
tag_closure_func(f) <-- call_target_assign(f, _, _), call(_, _, f);
```

and gate `:1268` / `:1272` on `tag_closure_func` instead. Leave `critical_call` itself alone —
`:1280-1283` and the consumer rules still want its narrower meaning.

This is the only part with a cost: the `critical_call` guard exists to keep the closure small
(`:1273`, "large reduction on some test cases"), and this widens it to every called function
holding a call-target fact — a large set on binaries with many function-pointer facts. Measure
before committing. If it regresses badly, the fallback is to restrict the second seed rule (by
`CallTargetObject` variant, or to functions with at least one non-pass-through return), accepting
that the fix then lands for some frontends and not others.

## Step 2 — The return-direction rule

Place near the existing tag rules at `:1268`. Inlined rather than staged through an intermediate
`ret_target` relation: the projection is non-recursive, so materializing it buys nothing.

```rust
call_target_assign_like(caller, cv, p.clone(), cto) <--
    call_target_assign_like(callee, v, p, cto),   // delta driver
    formal_param(callee, v, formal_ty),           // probe (func, var) — prunes hard, keep second
    if let Some(n) = v.as_formal(),
    if isout(&n, *formal_ty, p),
    call(caller, insn, callee),                   // probe by target column
    critical_call(caller),
    let cv = call_arg!(*insn, *n);
```

Notes:

- **No new indices.** `formal_param` is already probed by `(func, var)` at `:1109`; `call` is
  already probed by its target column at `:1098` and `:1154`.
- **Clause order is load-bearing** — same reason as the comment at `:1115-1118`. Driving on the
  `call_target_assign_like` delta and probing `formal_param` second prunes to tagged out-formals
  before the caller fan-out.
- **No call string needed.** The head names the specific `insn`, so each call site of a factory
  gets its own tagged vertex; context sensitivity is inherent to the return direction.
- **No `paths` gate.** `p` is carried unchanged (no `substitute_prefix`), so no path growth.
- **`isout` (`facts.rs:1352`) is true for every negative index**, so `RETURN_INDEX = -1` and the
  `-2, -3` multi-return slots all qualify, and by-ref out-params (a callee that installs a target
  into a caller-owned object) fall out for free.
- Head and first body atom sharing a relation is already the shape of `:1272`; termination is
  unaffected (finite vertex/object domains, bounded call-graph depth, cycles saturate).

Downstream needs no change: `:1272` walks the new tuple from `call_arg(insn, -1)` to the receiver
over the symmetric `actual_param` edges at `:1088-1089`, `:1254` resolves the local indirect call
exactly, and if the receiver is passed onward, rule 2.1 (`:1152`) derives the resolvent with a
properly constructed call string. Do **not** seed `resolvent` directly — it is a callee-frame
relation keyed on the callee's formals, so a tuple for a frame whose receiver is a local has no
consumer, and it would bypass the `CallString::new().push(...)` construction.

## Step 3 — Retire the frontend workarounds this was propping up

Any static over-approximation a frontend emits *because* the tag doesn't survive a return can now
be demoted to a fallback for genuinely opaque receivers. On this branch that is
`codegen/mod.rs:539`'s `lua_resolvents_by_method` union; under Phase 3 of
`lua-call-scheme-plan.md`, `Mixed` should emit `callee_info` alone plus the union only when
`method_is_opaque`, and `Hi` should become viable unassisted.

## Verification

- `Resolvent` / `critical_summary` counters non-zero on the reproducer.
- `metatable-oop-account`, `metatable-inherit-flow`, `method-colon-flow` green with the
  `lua_resolvents_by_method` union **deleted**, not merely demoted — that is the real signal that
  the engine rule is doing the work. Then restore it as the opaque-receiver fallback and reconfirm.
- Full `nightly/tests/lua/` (15 pass, `closure-flow` a known unrelated XFAIL).
- A C/pcode case with a function-pointer factory (`h = lookup(); h();`) should newly resolve —
  worth adding as a fixture, since it is the same defect independent of Lua.
- Java/pcode regressions otherwise unchanged: Step 1 is the only rule that can move them. Compare
  `relation increase: call_target_assign_like` (`:337`) and wall clock before/after on
  `examples/kong` and `examples/prosody`, plus a binary target for the Step 1 cost.
