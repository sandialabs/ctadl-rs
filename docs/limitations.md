# Limitations

- **C# structs are not copied.** The CIL frontend treats a value type like an object, so a
  struct copy (an assignment or a by-value argument) aliases the original, and taint written
  into the copy shows up in the original as a false positive.
- **The address of a C struct member is not nameable.** The C frontend has no struct layout,
  so `&s.f` has no address in the IR and falls back to a copy of `s.f`: a callee that writes
  through that pointer (`init(&s.f)`) does not taint `s.f` in the caller, a false negative.
  `&s` and `&a[1]` are nameable and not affected.
- **A C file-scope initializer is not lowered.** `int *p = &x;` or `struct ops o = {.f = h};` at
  file scope gives the global no value, so a flow or a call target that only the initializer
  supplies is lost, a false negative.
- **JVM and Dex drop an exception a callee does not catch.** An exception is the last result of
  every call, but the JVM and Dex front ends do not pass a callee's escaping exception on to the
  caller's handler, so taint carried by an exception thrown in a callee and caught in a caller is
  lost, a false negative. The CIL front end accumulates them (`excout`) and is not affected.
- **A Java method handle resolves only to the method its constant names.** `mh.invoke(..)` and
  `mh.invokeExact(..)` call through the handle's function reference, which only a method-handle
  constant (`ldc`, `const-method-handle`) supplies. A handle built at run time
  (`lookup.findVirtual(..)`), adapted (`bindTo`, `asType`) or invoked with
  `invokeWithArguments` calls nothing, and a virtual handle reaches the method it names, not an
  override. Arguments are passed by position, so a call that relies on `invoke`'s boxing or
  varargs adaptation lines up wrongly. These are false negatives.

## Approximations register

The index and the query deviate from the semantics of [ir.md](ir.md) only here, and the IR
passes only where a row says so. "Loses" means a flow the semantics derives may be absent;
"adds" means a flow the semantics does not derive may be present.

| Name | Where | What | Direction | Reported |
| --- | --- | --- | --- | --- |
| `admissible paths` | index, query | Admissible path set: suffixes outside program ∪ model ∪ one-level concatenations are dropped | loses | no (structural) |
| `callee cells merge` | index | Cells inside a callee merge all callers (S14) | adds | no (structural) |
| `resolution ladder` | index | Resolution ladder: dispatch model, CHA under threshold | adds | config recorded |
| `hybrid context` | index | Hybrid context widening (`collapse`, `bounded`, `spill`) | adds | config recorded |
| `backward alias rule` | index | Backward (S8) only along whole-variable copy chains from a formal and at program-store destinations: `l = p.f; l.g = a` reaches `p.f.g` only when `l` is a temporary codegen's load-chain composition anchors on `p`; `y.f = p; y.f.g = a` never reaches `p.g` | loses | no (doc comment on the rule) |
| `shared formals` | index | `Shared` formals (pcode, endpoints, JNI): whole-variable write-back through a parameter | adds | no (doc comment on the rule) |
| `copied formals` | index | `Copied` formals: no field write-back through a parameter | loses | no (doc comment on the rule) |
| `object hop` | query | Object hop at the empty path | adds | no |
|  | query | Saturating sources read below the port; wildcard sinks match below the port | adds (model opt-in) | model text |
| `mem2reg window` | passes | `mem2reg window:N` (and `exact`, a window of one slot): an escape reaching past the window | loses | no; `above` avoids it |
| `mem2reg full` | passes | `mem2reg full` | loses | per function |
| `copy_prop` | passes | `copy_prop` rewrites a store through a copy `t = v` onto `v`: a store at `t.#value` lands on `v.#value`, and stores through `t` and `v` merge as S9's do | adds | no |
|  | query | Call/return matching by one bit, not a site stack | adds (witness only) | no |
|  | front ends | Conventions (S19 of [ir.md](ir.md)); C `&s.f`; CIL value types; Lua `err` never filled | both | the list above |
|  | front ends | pcode `merge` arithmetic | adds | config recorded |
|  | front ends | C: a file-scope initializer is not lowered | loses | the list above |
| `global passed at calls` | index, query | Every global is a field of one pseudo-formal that each function declares and each call site passes, so a flow through a global is found only between instances a call chain connects, and a store in one function is not read in a function with no common caller (G2). Removed by stage 2b of the compositional-globals plan | loses | no |
| `call target held in a global` | index | A target label at a global path is a hybrid-inlining decision, so an indirect call through it resolves only on a call chain that establishes it (G6, R17). Removed by stage 2b of the compositional-globals plan | loses | no |

Anything not in this table that differs from the reference implementation is a bug. A test
program that shows a row names it on a `// parity: <name> <adds|loses>: ...` line, which
`ctadl-ascent/tests/integration/reference_parity.rs` checks.
