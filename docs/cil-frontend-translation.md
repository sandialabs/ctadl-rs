# Design: the CIL frontend's translation to CTADL IR - DO-NOT-MERGE

Status: design, not implemented. It builds on [`cil-flow-api.md`](cil-flow-api.md),
which describes the reader's dataflow layer. Engine plumbing (the `Cil` VMT
variant, `CallStyle`, the dispatch key, the `deref_paths` gate) is separate
work. It is touched on here only where the frontend has to fill it in.

**Decided:**
- Managed pointers lower to the place they point at whenever the flow layer can
  trace it. They fall back to the dereference field
  `ctadl_ir::mir::DEREF_FIELD` only when it cannot.
- Names are assembly-qualified and ILAsm-flavored (§3).

---

## 1. Strategy

The reader describes **what CIL does**. The frontend decides **how CTADL says
it**. A choice belongs in the reader if a non-CTADL consumer would want the same
answer, and in the frontend otherwise.

1. **One narrow interface.** The frontend consumes `MethodFlow`: blocks of
   `Effect<Var>`, `CallKind`, `Control`, stack kinds, and `resolve_address`. It
   never matches on `Code`.
2. **Every step is a total mapping written down as a table.** §5–§8 each have one.
   Each table is one `match` in the code with no `_` arm, so a review compares a
   row to an arm.
3. **Emit naive IR.** One obvious statement per effect, with no copy elimination
   and no merging of temporaries. `ssa::run_pipeline` does that cleanup.
4. **Policies are named and live in one place.** Every lowering choice that is
   not forced by the IR has a constant or function and a paragraph in §9.
5. **Each layer is checked against something it does not control.** For the
   frontend that is `FunctionData::verify` plus the SSA pass running without a
   panic on every corpus method, then end-to-end taint tests, and `cil:taint`
   (§12): the index's summaries must cover every flow a run of the exec
   fixtures shows.
6. **Build in vertical slices** (§11). Each slice adds table rows, a fixture,
   and a test, end to end.

---

## 2. Crate layout

`frontends/ctadl-cil/` is a sibling of `ctadl-dex` and `ctadl-jvm`. It has the same `Context` lifecycle as Dex: `new`, then
`process` once per assembly, then `finish` once. That way a multi-assembly app
becomes one `ProgramInfo` with one hierarchy. Unlike Dex, which keeps most of
its logic in one `impl Context`, each concern gets its own file:

| File | Owns | Table |
| --- | --- | --- |
| `lib.rs` | `import_assembly`, `import_directory`, `Context`, assembly selection | — |
| `names.rs` | All naming: classes, methods, descriptors, fields, assembly canonicalization | §3 |
| `hierarchy.rs` | VMT rows: methods, parents, interfaces, abstract methods, override aliases, P/Invoke | §4 |
| `method.rs` | `FunctionData` shape, blocks, terminators, source info | §5, §8 |
| `effects.rs` | `Effect<Var>` → statements | §6 |
| `ptr.rs` | Lowering of managed pointers (resolved place vs. `DEREF_FIELD`, copy-in/copy-out) | §6.3 |
| `calls.rs` | `CallSite` → `CallAssign`, delegates, extern stubs | §7 |
| `tests.rs` | Per-table unit tests on synthetic `MethodFlow`s | — |

`effects.rs` and `calls.rs` take a `MethodFlow` and an `Emitter` (see below) and
know nothing about assemblies. They can therefore be unit-tested from synthetic
flows without building a DLL.

```rust
/// Per-function lowering state: the one place statements are appended.
struct Emitter<'f> {
    fdat: &'f mut FunctionData,
    block: BasicBlockIdx,
    source_info: SourceInfo,   // set per instruction
}
impl Emitter<'_> {
    fn var(&mut self, v: Var) -> VariableRef;          // §5.2
    fn push(&mut self, kind: StatementKind);
}
```

---

## 3. Names

Names are the contract between the frontend, CHA, the model files, and SARIF.
All of them come from `names.rs`.

### 3.1 Spellings

| Thing | Spelling | Example |
| --- | --- | --- |
| Class (`cls`) | `[Asm]Ns.Type`, nested `Outer/Inner`, generic arity kept, **open definition** | `[MyApp]Acme.Widgets.Gadget`, `[System.Collections]System.Collections.Generic.List`1` |
| Simple name | Metadata name, generic method arity appended | `Parse`, `Select``2`, `.ctor`, `get_Item` |
| Descriptor | `(P1,P2)->Ret`, types spelled by §3.2 | `(System.String,!!0&)->System.Boolean` |
| Function id | `{cls}::{simple}{descriptor}` | `[MyApp]Acme.Gadget::Parse(System.String)->System.Int32` |
| Field | `<{cls}::{name}>` | `<[MyApp]Acme.Gadget::_name>` |
| Allocation | `CallObject::CilObject({cls})` | `cil<[MyApp]Acme.Gadget>` |
| Array element / length | `[]` / `length` | the same symbols as Dex and JVM |
| Pointer target | `ctadl_ir::mir::DEREF_FIELD` (`FieldRef::deref()`) | the one dereference spelling shared with C, pcode, the index and the query |

`Asm` is the **simple** assembly name, without version or public key token. So
`[System.Runtime]` is the same across framework versions.

### 3.2 Type spellings inside descriptors

Descriptors are **not** assembly-qualified. A descriptor separates overloads
within one class, and two overloads of one method rarely differ only in which
assembly a parameter type comes from. Leaving out the assembly makes descriptors
immune to facade forwarding, and keeps them readable in model files.

| `TypeSig` | Spelling |
| --- | --- |
| primitives, `String`, `Object`, `IntPtr`, `TypedByRef` | Cecil full names: `System.Int32`, `System.String`, … |
| `Named` | `Ns.Type`, nested `Ns.Outer/Inner` |
| `GenericInst` | `Ns.Type`1<A,B>` |
| `Var(n)` / `MVar(n)` | `!n` / `!!n`, **positional, never by name** |
| `SzArray` / `Array` | `T[]` / `T[,]` (rank only; bounds dropped) |
| `ByRef` / `Ptr` | `T&` / `T*` |
| `FnPtr` | `method R*(P..)` |
| `CModReq` / `CModOpt` / `Pinned` / `Sentinel` | dropped (the `...` vararg sentinel aside) |

Writing generic parameters by position is a deliberate departure from Cecil,
which prints them by name (`T`). The definition `List`1::Add(!0)` and a call
site `List`1<String>::Add(!0)` then produce the same descriptor without any
substitution, because a `MemberRef` signature on a generic `TypeSpec` is
written in terms of `!0`.

### 3.3 Qualification and canonicalization

- **TypeDef**: the assembly being processed.
- **TypeRef**: by resolution scope. `AssemblyRef` gives that assembly's name.
  `ModuleRef` or a null scope gives the current assembly, through the
  `ExportedType` table for a null scope. A nested `TypeRef` takes its outer
  type's scope.
- **Forwarders**: in `finish`, rewrite every `cls` through an alias table built
  from `ExportedType` forwarder rows. The rows come from every processed assembly
  plus every metadata-only reference assembly (§4.3). So
  `[System.Runtime]System.Object` and `[System.Private.CoreLib]System.Object`
  become one class. The canonical name is the forwarding *target*.
- **Generics**: `cls` is always the open definition. A `TypeSpec` call target
  erases to its generic type definition, and `MethodSpec` erases to its method.
  The instantiation is recorded nowhere in the IR. That matches what Java
  erasure gives the other frontends, and keeps CHA tractable.

---

## 4. Hierarchy and VMT

Filled per assembly in `process` and finalized in `finish`. The columns mirror
`VirtualMethodTable::Java` (`methods`, `hierarchy`, `interfaces`,
`abstract_methods`) plus `pinvokes` in place of `natives`. The `Cil` variant
needs `interfaces` and `abstract_methods` too: the functional-interface
recognition that `abstract_methods` exists for is just what delegates and
lambdas need.

### 4.1 Rows

| Source | Row |
| --- | --- |
| each `TypeDef` | `hierarchy[cls] = [base] ++ interfaces` (erased) |
| `TypeDef` with the interface flag | `interfaces.push(cls)` |
| each method with a body, or `pinvoke`, or `internalcall` | `methods.push((cls, simple, desc, fid))` |
| each `abstract` method | `abstract_methods.push((cls, simple, desc))` |
| each `pinvoke` method | `pinvokes.push((cls, simple, desc, fid, module, entry_point))` |
| **override aliases** (§4.2) | extra `methods` rows: `(cls, simple, base_desc, fid)` |

### 4.2 Override aliases: CIL has no bridge methods

CHA keys on `(simple name, descriptor)`. Java gets cross-generic overrides for
free, because erasure plus `javac` bridge methods make the descriptors match.
CIL has neither:

```csharp
class B<T> { public virtual void M(T x) {} }   // B`1::M(!0)->System.Void
class D : B<string> { public override void M(string x) {} }  // D::M(System.String)->System.Void
```

A `callvirt B`1<string>::M(!0)` never resolves to `D::M`. Explicit
implementations (`MethodImpl` rows, `void IFoo.Bar()`) have the same problem in
a different form: the implementing method's *name* differs from the interface
method's name.

`hierarchy.rs` fixes both, and nothing else in the pipeline has to know:

1. For each virtual method `m` of `D` that is not `newslot`, walk `D`'s base
   chain. Substitute each base's generic arguments (from the `extends`
   `TypeSpec`) into the base's virtual methods. For every base method `b` with
   `m`'s name whose substituted signature equals `m`'s signature, add the row
   `(D, b.simple, b.desc, m.fid)`.
2. Do the same for every interface `D` implements, using implicit name+signature
   matching.
3. For each `MethodImpl` row `(D, body = m, declaration = I::b)`, add
   `(D, b.simple, b.desc, m.fid)`.

The rows are written under `D`'s class with the *base's* key. CHA, walking up
from the receiver's class, therefore finds `D::M` under the key the call site
uses. Plain overrides that match exactly produce a duplicate row, which is
harmless and is deduplicated.

### 4.3 Metadata of referenced assemblies

Step 1 needs the virtual methods of a base type. That type is often in the BCL:
`IEquatable<T>.Equals(T)`, `IComparer<T>.Compare`,
`IEnumerable<T>.GetEnumerator`. The assembly-selection policy does not import
BCL bodies, but the frontend still **resolves BCL metadata** through
`cil-reader`'s existing `AssemblyCache`/resolver, from the app directory and
the framework reference directory. Only signatures, base types and forwarders
are read; nothing is lowered. If the metadata cannot be found, no alias is
made, and the frontend logs the base type and counts it in the import
summary.

### 4.4 Extern stubs

Every callee that no imported assembly defines gets a stub `FunctionData` and a
VMT row in `finish`, exactly as `Context::ext` does in Dex. The stub's shape
comes from the call site's `MethodSig` (§5.1), so a model can name it.

---

## 5. Function shape

### 5.1 Signature

| CIL | `FunctionData` |
| --- | --- |
| `has_this` | param 0, `ByRef` |
| param of reference type (`Named{is_value_type: false}`, `String`, `Object`, arrays, `GenericInst` class, `Var`/`MVar`) | `ByRef` |
| param `T&` (`ref`/`out`/`in`) or `T*` | `ByRef` |
| primitive or value-type param | `ByVal` |
| return | `ReturnType { arity: 2 }`: `(normal, exception)` |
| `name` | function id (§3.1) |

`Var`/`MVar` are `ByRef` because an unconstrained generic parameter may be
instantiated with a reference type. That is the conservative choice.

### 5.2 Variables

| `Var` | `VariableRef` |
| --- | --- |
| `Arg(n)` | `new_parameter(n)` |
| `Local(n)` | local `loc{n}` |
| `Stack(d)` | local `s{d}` |
| `Temp(k)` | local `t{k}` |
| `Caught(h)` | local `caught{h}` |
| `Thrown` | local `throwval` (shared, as in Dex) |
| call result | local `retval` (shared, as in Dex) |
| `Root::Static` | `new_global()` |

All of them are unversioned (the flow layer never versions, `cil-flow-api.md`
§6.5). The frontend's own temporaries (§6.3's `t`, `c_k`, `u_k`, `cell_k`, and
§7's delegate loads) are fresh locals with prefixes of their own, so they never
collide with `t{k}`.

### 5.3 What the ctadl-ir pipeline does for the frontend

The frontend's MIR goes through `ssa::run_pipeline` in `ctadl index`
(`Pipeline::index_default()`): dead-temp elimination, copy coalescing, pruned
Cytron SSA, then copy propagation and trivial-phi elimination. The flow layer's
output is raw (unversioned, every copy kept), and the frontend relies on this
pipeline instead of cleaning it up itself:

- **Unversioned variables.** `ssa::transform` requires IR that is not yet in
  SSA, and `coalesce_copies` only touches unversioned `Variable::Local`s. §5.2
  maps every `Var` to such a local, a parameter, or the global.
- **Depth names.** A `Stack(d)` is reassigned at every push at depth `d`. Pruned
  Cytron SSA gives each definition of `s{d}` its own version and puts a phi only
  where two definitions really meet. "What object does this `stfld` write to"
  is answered by copy propagation rewriting `s0.f := v` to `p0.f := v` after
  `s0 := p0`.
- **Copies.** `ldloc`, `stloc`, `dup` and the flow layer's collision
  temporaries are single-use copies, which `coalesce_copies` merges before SSA.
  Multi-use copies are removed by `propagate_copies` after SSA, as are phis
  that merge a value only with copies of itself. Removing them in the frontend
  would duplicate those passes.
- **Exceptions.** SSA places the phi that merges the `throwval` versions
  reaching a handler (§8).
- **Terminators and reachability.** Every block gets a terminator. Multiple
  `ret`s are fine, because SSA adds the single exit block itself. Unreachable
  blocks (common after `leave` into a `finally`) are fine, because `ctadl index`
  runs with `prune_unreachable`.

The frontend does not test single definitions or dominance: `ssa::transform`
asserts `FunctionData::verify` itself. The frontend's tests run each corpus
method through `run_pipeline` and require no panic.

---

## 6. Effects → statements (`effects.rs`)

`F(field)` means `FieldRef::symbol(..)`: `Named(f)` gives `<cls::f>`, `Element`
gives `"[]"`, `Length` gives `"length"`, and `Deref` and `Block` (a `cpblk`
or `initblk` extent) give `FieldRef::deref()`, whose symbol is `DEREF_FIELD`. `ap(v)` means `AccessPath::without_fields(var(v))`.
The frontend never spells the dereference as a string literal. `DEREF_FIELD` is
the field the index and the query treat as a pointer target (`PathSegment::is_deref`),
and the `ctadl-ir` docs ask any frontend that needs a pointer-like field to use it
rather than mint its own.

### 6.1 The table

| Effect | Statements |
| --- | --- |
| `Assign { dst, srcs, Copy \| Cast \| Unbox \| Derived }` | `assign(dst, srcs)` |
| `Assign { dst, srcs, Box(T) }` | `assign(dst, srcs)`, and record `T` as instantiated (§9) |
| `Const { dst, I4/I8 }` | `assign(dst, [new_int(v)])` |
| `Const { dst, R4/R8 }` | `assign(dst, [new_int(bits)])` (raw bits, as Dex does for `const-wide`) |
| `Const { dst, Null }` | `assign(dst, [new_int(0)])` (as Dex's `const/4 vA, 0`) |
| `Const { dst, String(tok) }` | `assign(dst, [new_str(user_string)])` |
| `Const { dst, Token(tok) }` | `assign(dst, [new_str(name of tok)])` (as Dex's `const-class`) |
| `Const { dst, SizeOf \| ArgList }` | `assign(dst, [new_int(0)])` |
| `Load { dst, Path { Slot(b), f } }` | §6.3 if `b` is a pointer, else `load(dst, ap(b), F(f))` |
| `Load { dst, Path { Static, f } }` | `load(dst, ap(global), F(f))` |
| `Store { Path { Slot(b), f }, src }` | §6.3 if `b` is a pointer, else `store(ap(b), F(f), src)` |
| `Store { Path { Static, f }, src }` | `store(ap(global), F(f), src)` |
| `AddressOf { dst, place }` | nothing, or the fallback in §6.3 |
| `New { dst, Object \| ValueType, T }` | `assign(dst, [new_object_ref(CilObject(T))])` |
| `New { dst, Array, T, size }` | `assign(dst, [new_object_ref(CilObject(T[]))])` |
| `New { dst, StackAlloc }` | `assign(dst, [new_int(0)])` |
| `FunctionPtr { dst, m, .. }` | `assign(dst, [new_object_ref(FunctionPtr(fid(m)))])` |
| `Clear { addr }` | resolved: `assign(place, [])` (a kill); unresolved: nothing |
| `Call(site)` | §7 |
| `Return` / `Throw` / `Jump` | terminators, §8 |
| `Test` | nothing (§9) |
| `Opaque { srcs, dsts }` | for each `d`: `assign(d, srcs)`; count it |

"`b` is a pointer" means that `input_kinds` for that input is `ManagedPtr`, or
that the variable is a byref parameter or a byref local.

### 6.2 Value types

A struct is lowered like an object. `ldloc` of a struct local is `assign`, and
`ldfld` on a struct is `load`. CTADL's assign carries access paths, so a struct
copy carries its tainted fields. The cost is that two copies of one struct
alias in the analysis, where at runtime they are independent. For taint that
over-approximates, which is acceptable.

### 6.3 Managed pointers (`ptr.rs`)

The decided rule is to lower to the traced place, with `DEREF_FIELD` only as a
fallback. Below, `D` is `FieldRef::deref()` and `v.D` is the path through it.
`MethodFlow::resolve_address(at, i)` gives the `Place` for input `i`.

Reading and writing a resolved place:

| Place | read `P` into `x` | write `x` into `P` | field `f` of `P` |
| --- | --- | --- | --- |
| `Local(n)` | `assign(x, [loc{n}])` | `assign(loc{n}, [x])` | `load/store loc{n}.f` |
| `Arg(n)` | `assign(x, [p{n}])` | `assign(p{n}, [x])` | `load/store p{n}.f` |
| `Field(b, g)` | `load(x, b, g)` | `store(b, g, x)` | `t := load b.g`, then `load/store t.f` |
| `Static(g)` | `load(x, global, g)` | `store(global, g, x)` | `t := load global.g`, then `load/store t.f` |
| `Element(b, _)` | `load(x, b, [])` | `store(b, [], x)` | `t := load b.[]`, then `load/store t.f` |

So each pointer effect lowers like this:

| Effect on pointer `v` | Resolved to `P` | Unresolved |
| --- | --- | --- |
| `Load dst := v.*` | read `P` into `dst` | `load(dst, v, D)` |
| `Store v.* := x` | write `x` into `P` | `store(v, D, x)` |
| `Load dst := v.f` (struct through pointer) | field `f` of `P` | `t := load v.D; load(dst, t, f)` |
| `Store v.f := x` | field `f` of `P` | `t := load v.D; store(t, f, x)` |
| `Clear v` | `assign(P, [])` for `Local`/`Arg`; nothing otherwise | nothing |

**Byref call arguments: copy-in/copy-out.** For each index in
`CallSite::byref_args` whose argument resolves to a place `P`:

```text
c_k := read P               copy-in
store cell_k.D := c_k
call f(..., cell_k, ...)    cell_k is passed where the pointer was
u_k := load cell_k.D        copy-out
write u_k into P
```

`cell_k` is a fresh local per call site and argument. Under
`ParamWriteBack::Entry`, the callee's `store p.D := v` comes back to the
caller's `cell_k`, so `u_k` sees it, and through it `P` does too. When
the argument is unresolved, it is passed as is: it is already a pointer
variable, and the callee's writes through `D` come back to it.

This depends on `ArtifactLanguage::Cil` returning `Entry` from
`param_write_back()` (`ctadl-import/src/project.rs`), as JVM and Dex do. CIL
arguments are passed by value: `starg` only rebinds the callee's own copy, and
the caller must not see it. A write that must reach the caller goes through a
`ref`/`out` parameter, which is a store through `D` on the parameter's entry
version, and that is exactly what `Entry` writes back. (C uses `Exit` only
because the C frontend lowers `*out = x` to `out = x`; the comment on
`param_write_back` calls that a deficiency of the C frontend and notes that C
could use `Entry` too once `*p` lowers to `p.deref`.)

**Admitting `.deref` paths in the index.** `ctadl index` admits the path
`p.deref` for every program path `p` only when `IndexConfig::deref_paths` is set
(or a loaded model mentions `.deref`). `ctadl-ascent/src/cli/mod.rs` sets it
when any import is C or pcode. Without it, an index with no propagation model
cannot carry a pointee's taint through a pointer held in memory: the store
`o.p := b` reaches only `o.p`, and `o.p.deref` is not a path the index knows
(`ptrfield.c` is the regression test). CIL has such pointers: `T*` fields in
`unsafe` code, `ref` fields of ref structs (`Span<T>`'s reference to its first
element), and an unresolved address stored through a pointer. So
`ArtifactLanguage::Cil` joins C and pcode in that gate. Pointers that only live
in locals and parameters (`cell_k`, byref parameters) do not need it, because
their frames spell `v.deref` directly.

**The rare case: an address merged at a join.** C# `ref` conditionals
(`ref (c ? ref a : ref b)`) and hand-written IL can merge two addresses, and
then `resolve_address` returns `None` at the use. Reads still need to work, so
the fallback is: an `AddressOf { dst, P }` whose `dst` reaches an unresolved
use (the flow layer reports this, see §10) also emits `c := read P; store
dst.D := c`. Reads through the merged pointer then see both places. Writes
through it reach only `dst.D`, not `a` or `b`. That is a known
false-negative, counted in the import summary as `cil: unresolved address
writes`.

---

## 7. Calls (`calls.rs`)

Each call becomes one `CallAssign { style, rets: [retval, throwval], args }`,
plus the copy-in/copy-out statements of §6.3. After the call, `retval` is copied
to `site.result` if there is one, as Dex does for `move-result`.

| `CallKind` | `args` | `style` |
| --- | --- | --- |
| `Static` | `site.args` | `DirectCall { Explicit([fid(m)]) }` |
| `Instance` | `[recv] ++ site.args` | `DirectCall { Explicit([fid(m)]) }` |
| `Ctor` | `[recv] ++ site.args` | `DirectCall { Explicit([fid(m)]) }` |
| `Virtual { constrained: None }` | `site.args` (receiver in `style`) | `CilCall { receiver, cls: erased declaring type, simple, desc }` |
| `Virtual { constrained: Some(T) }` | as above | `CilCall { .., cls: T }` if `T` declares or inherits `m` in loaded metadata; else as unconstrained |
| `Indirect { callee }` | `site.args` | `FuncPtrCall { callee: ap(callee), signature: desc(sig) }` |

`CilCall` keeps `args` without the receiver, which mirrors how `JavaCall` is
lowered in Dex today (`args.skip(1)`).

A **sealed** declaring type, or a method that is not `virtual` (C# emits
`callvirt` on non-virtual instance methods to get a null check), lowers as
`DirectCall`. That saves CHA work and hybrid-inlining resolvents, and both
facts are in the metadata.

**Delegates.** After `flow::delegate::classify(&site, &oracle)`, where the
oracle is backed by the metadata of §4.3:

| Classified as | Lowering |
| --- | --- |
| `Ctor { target, fnptr }` on `newobj D::.ctor` | `store(recv, "<delegate::target>", target)`, `store(recv, "<delegate::method>", fnptr)`, plus the ctor call |
| `Invoke` on `callvirt D::Invoke` | `t := load recv.<delegate::method>`, `FuncPtrCall { callee: t, .. }` with `args = [load recv.<delegate::target>] ++ site.args` |

The engine resolves a `FuncPtrCall` through `CallObject::FunctionPtr(fid)`,
which `ldftn` put into `fnptr`. Passing the target object as argument 0 gives a
closure (a display-class method) its `this`. For a static target, argument 0
is an extra the callee ignores. The two field names are fixed internal symbols
in `names.rs`. A delegate passed through helpers that only forward it is still
resolved at the `Invoke`: the index matches a field-held call target at every
split of its path, so it reaches a frame that never touches the field itself
(`tests/tnt/funcptr_field_forward.tnt`).

---

## 8. Control → terminators (`method.rs`)

Blocks map one-to-one onto `MethodFlow` blocks. `Block::succs ++ exc_succs` are
the `Goto` targets.

| Last `Control` of the block | Terminator |
| --- | --- |
| `Next`, `Goto`, `Cond`, `Switch`, `Leave`, `EndFinally`, `EndFilter` | `Goto { succs ++ exc_succs }`; with no successors, `Return [empty, empty]` |
| `Return` (value `v`) | `Return [v, empty]` |
| `Return` (void) | `Return [empty, empty]` |
| `Throw` (`throwval := src` already emitted) | `Goto { exc_succs }` if protected, else `Return [empty, throwval]` |
| `Jump m` | `CallAssign` to `m` with every parameter, then `Return [retval, throwval]` |

`empty` is `Exp::new_bytes(vec![])`, as in Dex. Each catch, filter, or
filtered-handler entry block starts with `caught{h} := throwval`. Finally and
fault entries start with nothing. The flow layer's prepended `s0 :=
caught{h}` follows it.

**Precise exception edges.** In Dex, a block with two calls in a try range
reaches the handler with only the *last* call's `throwval`, and with the
block's *final* local versions. That loses or misattributes flows. The CIL
frontend asks the flow layer to end a block after every instruction that can
throw (`Call`, `Throw`) **inside a protected range** (§10). That is one extra
block per call inside a `try`, and SSA then merges the right versions at the
handler.

`SourceInfo` comes from `span_for(key, file_offset, ByteLen(size))`, set on the
`Emitter` per instruction, so every statement an instruction produces shares its
span. `file_offset` needs the reader helper `MethodBody::code_file_offset()`.

---

## 9. Policies (one place each)

| Policy | Choice | Where |
| --- | --- | --- |
| Pointer lowering | Traced place, else `DEREF_FIELD`; copy-in/copy-out for byref arguments | `ptr.rs` |
| `.deref` admission | `IndexConfig::deref_paths` on for Cil imports (§6.3) | `ctadl-ascent` `cli/mod.rs` |
| Index taint | An array index does not taint the element read (as Dex and JVM) | `effects.rs` |
| Branch conditions | `Test` emits nothing | `effects.rs` |
| `box` | Contents-preserving assign; `T` recorded as instantiated for CHA | `effects.rs` |
| Value types | Lowered like objects (§6.2) | `effects.rs` |
| Non-virtual or sealed `callvirt` | `DirectCall` | `calls.rs` |
| Assembly selection | Not framework/BCL by default, with a flag to override; BCL metadata always resolved | `lib.rs` |
| Generic instantiation | Erased everywhere (§3.3) | `names.rs` |
| `ParamWriteBack` | `Entry` | `ctadl-import` `param_write_back()` |

---

## 10. What this asks of the flow layer

Changes to `cil-flow-api.md` that this design depends on. All four are now in
that document (§4.2, §6.3, §9 step 1).

1. **Block splitting** after `Call` and `Throw` inside a protected range
   (§8). This is a `MethodFlow::build` option, on for this frontend.
2. **`AddressOf` reaching an unresolved use**:
   `MethodFlow::address_escapes_resolution(at) -> bool`, for the fallback in
   §6.3. It falls out of the same forward pass that computes `resolve_address`.
3. **Byref locals**: `C# ref` locals (`T&` in the locals signature) are
   address-holding locals in `resolve_address`'s state, and their uses count as
   pointers in §6.1. The flow API already says this ("from depth and from
   address-holding local"). The frontend relies on it.
4. `MethodBody::code_file_offset()` and `user_string()`, already in the flow
   plan's step 1.

---

## 11. Slices

Each slice is done when its fixture's `cil-dump --flow` and MIR goldens are
blessed, its table rows are implemented with no `_` arms, an end-to-end
taint test passes under `xtask regression --frontend cil`, and `cil:taint`
(§12) covers the slice's exec fixture methods.

| # | Slice | Rows | Fixture / test |
| --- | --- | --- | --- |
| 1 | Straight-line static methods | §5, §6.1 `Assign`/`Const`, §8 `Return` | `string Id(string s) => s;` |
| 2 | Branches and loops | §8 `Goto`/`Cond`/`Switch` | taint through `?:` and `for` |
| 3 | Fields, statics, arrays | §6.1 `Load`/`Store`/`New` | field and array round trips |
| 4 | Direct calls and extern stubs | §7 `Static`/`Instance`/`Ctor`, §4.4 | source → helper → sink |
| 5 | Hierarchy, virtual calls, CHA | §4.1, §7 `Virtual`, sealed/non-virtual | interface dispatch with two implementations |
| 6 | Generic overrides and explicit implementations | §4.2, §4.3 | `D : B<string>`; `IComparer<T>`; `void IFoo.Bar()` |
| 7 | Exceptions | §8 handlers, block splitting | `catch (E e) { sink(e.Message); }` |
| 8 | Managed pointers | §6.3 | `TryParse(s, out n)`, `ref` accumulator, struct field through `ldloca`, taint through a `T*` field with no models loaded |
| 9 | Delegates and lambdas | §7 delegates | `Func<string,string>` and an event handler |
| 10 | Multiple assemblies and forwarding | §3.3, `import_directory` | app + library DLL; `System.Object` via a facade |

Slices 1–4 need only `ctadl-ir` as it is today, and can use `CallStyle::DirectCall`
everywhere. Slice 5 is the first that needs the engine plumbing (`CilCall` and
the `Cil` VMT variant), so that should land by then.

---

## 12. Validation against dynamic flows (`cil:taint`)

The checks in §1.5 show that the IR is well formed and that some chosen flows
are found. They do not show that the index's summaries agree with what the
code does. `cil:taint` checks that: every data flow a run of a fixture method
actually shows must be covered by a summary that `ctadl index` computed for
that method. The run is checked against something the frontend does not
control: `cil:exec`'s interpreter, whose end states already match .NET's on
the exec fixtures (`cil-flow-api.md`).

It checks the whole chain at once: the flow layer, this translation, the SSA
pipeline, the index's rules, and summary composition across calls. A miss is
localized afterwards, from the method's `cil-dump --flow`, its MIR, and its
summary rows.

### 12.1 Static side

A summary row is `(f, i, p, j, q)` (`index_engine::FunctionSummary`): the
value at formal `i`, path `p`, on entry reaches formal `j`, path `q`, on
exit. Formals are numbered as the codegen numbers them: `n ≥ 0` is parameter
`n` (`this` is 0), `-1` is the normal return and `-2` the exceptional one
(the two slots of §5.1's `ReturnType`), and `GLOBALS_INDEX` is the global. A
formal `j ≥ 0` is an output only where `isout` says so: a `ByVal` parameter
with an empty path is not, so rebinding a parameter (`starg`) is never a flow
to the caller, which matches CIL.

The check runs `ctadl import` and `ctadl index` on each exec fixture, with
`IndexConfig::deref_paths` on as §6.3 requires, and reads the summary rows of
the fixture's methods by function id (§3.1).

### 12.2 Dynamic side

The interpreter gets a shadow: every value it holds also carries a set of
labels. A label is an input location, `(i, p)`. The shadow never changes what
the interpreter computes, and the comparison with .NET still runs on every
trial, so the concrete semantics stay checked.

- **Entry.** Each argument, and every location reachable from it, is
  labeled with the path it was reached by: `arg1`, `arg1.<C::f>`,
  `arg1.<C::f>.[]`. Paths use §3.1's spellings: `<cls::f>`, `[]`, `length`,
  and `DEREF_FIELD` for the target of a pointer argument. A location reached
  by two paths gets both labels.
- **Propagation.** Labels follow what each operation computes, from the
  values it reads: a copy, cast, box or unbox keeps its operand's labels;
  arithmetic, comparison and conversion take the union of their operands';
  a load takes the loaded location's; a store gives the location the stored
  value's; a constant or a fresh object has none; a call propagates through
  the callee's interpreted body. These rules come from the semantics, never
  from §6.1's table, which they exist to check.
- **What CTADL does not track by design.** These are dropped in the shadow,
  and listed in one place in the code, each citing the §9 policy it mirrors:
  implicit flows (a `Test` or a branch taints nothing), and an array index
  tainting the element read.
- **Exit.** Walk the normal result (`-1`), the thrown object (`-2`), and
  everything reachable from each argument. Each label `(i, p)` found at
  output location `(j, q)` is a dynamic flow `(i, p) → (j, q)`. The value at
  `-1` itself, path empty, counts, as does each field reached from it.

### 12.3 Which paths the index can reach

The index has no length bound on paths, and it never cuts one short. It
works over a closed set of **admitted paths**, `compute_paths`
(`ctadl-ascent/src/index_engine/mod.rs`), fixed before the main run, and
every rule that builds a longer path asks `PathSet::concat` for it. When the
result is not admitted, the rule derives nothing: the flow is dropped, with
no shorter flow kept in its place. A path is admitted if it is:

- a **program path**: the path on an assignment edge, an actual argument, or
  the receiver of an indirect call; or a path codegen composes along a chain
  of loads, copies and stores (`CodegenVisitor::visit_basic_block_data`,
  `ctadl-ascent/src/codegen/mod.rs`), which it does **within one basic
  block** only;
- a **model path**: a path a loaded model's summary mentions, or one such
  path joined once with a program path. Summaries the index derives add
  nothing;
- with `deref_paths`, a program path extended by `DEREF_FIELD`.

The set is global: a path one function spells is admitted in every function.

So a field chain the code builds across blocks reaches no summary. Small
`flowy` fixtures show it (`p.f.g` read as `t = p.f; r = t.g`):

| Shape | Summary | End to end |
| --- | --- | --- |
| both loads in one block, or three; a store through a temporary | found | found |
| a `goto` between the loads, or between the load and the store | absent | lost |
| the second load's base a phi of two loads | absent | — |
| `n = n.next` in a loop, then `n.val` | absent | lost |
| callee reads `q.g`, caller passes `p.f` | absent | lost |

No coarser row (`return <- p.f`, `return <- p`) stands in for the missing
one. Dex, JVM, pcode and Lua all lower field reads to `Load`, so this holds
for them too; the skipped `substitute_prefix_demo.tnt` is a case of the same
gate. CIL meets it more often than they do: the flow layer starts a block at
every branch target, the split CFG (§8) ends one after every call in a
`try`, and C#'s `?.`, `??`, `?:` and loops all put a field chain across
blocks.

A fix is engine work, not this frontend's: compose paths along SSA
definitions across blocks, joining over phis and bounding the depth around
loops. Until then, §12.4 keeps these misses apart from the frontend's.

### 12.4 The comparison

A dynamic flow `(i, p) → (j, q)` is **covered** if there is a summary row
`(f, i, p', j, q')` with `p = p'·s` and `q = q'·s` for some suffix `s`: a
summary about a whole object also covers its fields. Each uncovered flow is
then sorted, in this order:

1. **Unknown field: fail.** Every field of `p` and `q` (`<cls::f>`, `[]`,
   `length`, `DEREF_FIELD`) must occur in some admitted path. A load or
   store always admits the path of its own field, so a field the index has
   never seen means the frontend misspelled it (§3.1) or never lowered the
   access. This is checked before the filter below, which would otherwise
   hide exactly these bugs.
2. **Path not admitted: report.** `p` or `q` is not in the index's admitted
   set (the index writes it out as `paths`, `docs/debugging.md`). The index
   cannot produce that row at all, so the miss is §12.3's, not the
   frontend's. These are listed per method under their own heading and
   counted; the count is what the gate costs on CIL, and a jump in it when a
   slice lands means something to read.
3. **Known miss: XFAIL.** The flow matches an entry of the known-miss file
   (§12.5).
4. **Otherwise: fail**, with the method, the trial's starting state, and the
   flow.

The filter in step 2 tests only the endpoints, while the index tests every
path it builds on the way. So a flow with both endpoints admitted can still
be dropped on an intermediate path and fail in step 4. That is an engine
miss to triage, and, once understood, a known-miss entry. The other way
round cannot happen: a row whose endpoint is not admitted is one the index
never derives, so step 2 hides nothing the index would have found. If step
4 turns out noisy, the interpreter can model the gate instead: it knows the
block each load ran in, so it can mark a flow whose path was built inside
one basic block as one the index must admit, and filter only the rest.

**Precision** is reported, not gated: per method, the summary rows that no
trial showed. The static side covers every path and every aliasing, the
dynamic side only those 64 random trials reach, so a static-only row is not
a bug, and an over-approximation the design accepts (§6.2's struct
aliasing, CHA) needs no marking. A ratio that jumps between runs is worth a
look.

Under the same convention as `cil:exec`, `cil:taint` writes the dynamic
flows, the admitted paths, the summary rows and every sorted flow under
`<work>/cil-taint/`, and runs in `xtask regression --frontend cil` beside
`cil:exec`.

### 12.5 Known misses

A miss the project knows and accepts is marked per flow, in a checked-in
file next to the goldens, `xtask/cil-taint-known.toml`:

```toml
[[known]]
method = "Exec.Lists::Walk"          # Class::Method, with the descriptor if overloaded
flow   = "arg0.<next>*.<val> -> ret" # src -> dst; `f*` matches the field zero or more times
kind   = "engine:path-composition"
why    = "n = n.next in a loop composes no path (tnt: path_loop.tnt)"
```

- **Per flow, not per method.** One method can hold an accepted miss and
  enforced flows beside it. This follows `regression.rs`'s XFAIL allowlists
  (`LUA_XFAIL`, `C_KNOWN_FAILURES`), at a finer grain.
- **`flow` is a pattern.** A loop makes a different dynamic path per trip,
  so a field may repeat (`f*`); otherwise paths are spelled exactly, with
  §12.2's spelling (field names may drop the class when unambiguous in the
  method).
- **`kind` is one of a fixed list**, each tied to a section or a reproducer:
  `engine:path-composition` (§12.3, on an intermediate path), `engine:alias-rule`
  (the aliasing rule summarizes only paths the program stores through),
  `design:unresolved-address-write` (§6.3). The report counts XFAILs per
  kind, so an engine fix shows as its kind going to zero. A new kind is
  added to the list with its section.
- **`why` is required**, and an `engine:` entry names a `.tnt` reproducer in
  `ctadl-ascent/tests/tnt`, so the limitation is tested where it lives and
  not only through CIL. Reproducers that fail today go in as known
  limitations, the way `substitute_prefix_demo.tnt` does.
- **A stale entry fails.** An entry that matches no uncovered flow in any
  trial is reported XPASS and fails the check: either the index now covers
  the flow or the fixture no longer exercises it, and either way the entry
  goes. Trials are seeded (`SEED`), so this does not flake.

Entries live in the file, not as attributes on the fixture methods, so that
changing one does not rebuild the fixtures.

### 12.6 Limits

- **Fixtures only.** It covers the exec fixtures, not real assemblies. Each
  slice (§11) adds methods for its constructs to `exec.cs` or `exec.il`, and
  the interpreter must model them: virtual dispatch for slice 5, `ref` and
  `out` for slice 8, delegates for slice 9.
- **No BCL bodies.** A trial that calls into the BCL is outside the oracle
  unless the interpreter models the callee and `ctadl` loads a model for it
  that says the same. The fixtures stay self-contained instead.
- **No aliased inputs yet.** `flow-run` makes a fresh object for every
  argument, so flows that exist only when two arguments are the same object
  (the index's aliasing rule) are never shown. `flow-run` needs an option to
  pass one object as several arguments, and `cil:taint` should use it.
- **Statics.** Flows through static fields go through `global` (§5.2), the
  formal `GLOBALS_INDEX` in a summary row. Nothing yet checks how the index
  summarizes it (flowy's own checker, `port_to_index`, refuses a global
  port), so until the rows through it are understood, statics carry no
  labels and are not outputs.
- **Must agree on names.** A spelling that differs between the frontend's
  fields and the shadow's paths shows up as an unknown field (§12.4, step
  1). That is a check of §3.1 too, but the first failures of a new slice
  should be read with it in mind.
- **The admission gate.** Flows §12.3 drops are counted, not checked, so
  `cil:taint` says nothing about the frontend's lowering along them. The
  count bounds how much is unchecked.

### 12.7 When

Build it with slice 1, not after slice 10. Slices 1–4 lower every call as
`DirectCall`, which `ctadl index` needs nothing new for, so from the first
slice on each one is gated by `cil:taint` on its own fixture methods (§11's
"done" includes it).

---

## 13. Open questions

1. **Descriptor qualification** (§3.2). The descriptor leaves out assemblies.
   The catch is that it cannot tell apart two overloads that differ only by the
   assembly of a parameter type. I believe that is negligible, but the corpus
   should confirm it: count collisions of function ids per class.
2. **Value-type aliasing** (§6.2). Lowering structs like objects
   over-approximates. If the corpus shows false positives from struct copies,
   `Update` (the functional field update in MIR) is the precise alternative,
   at the cost of a copy per struct assignment.
3. **Delegate field names** (§7). Two fixed symbols on every delegate object.
   The default models will need to refer to them for `Delegate.Combine`,
   `DynamicInvoke` and events.
