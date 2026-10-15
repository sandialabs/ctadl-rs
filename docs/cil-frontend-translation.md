# Design: the CIL frontend's translation to CTADL IR - DO-NOT-MERGE

Status: design, not implemented. It builds on [`cil-flow-api.md`](cil-flow-api.md),
which describes the reader's dataflow layer. Engine plumbing (the `Cil` VMT
variant, `CallStyle`, the dispatch key) is separate work. It is touched on here
only where the frontend has to fill it in.

**Decided:**
- Managed pointers lower to the place they point at whenever the flow layer can
  trace it. They fall back to `deref` only when it cannot.
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
   panic on every corpus method, then end-to-end taint tests.
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
| `ptr.rs` | Lowering of managed pointers (resolved place vs. `deref`, copy-in/copy-out) | §6.3 |
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
| Array element / length / pointer target | `[]` / `length` / `deref` | the same symbols as Dex, JVM and C |

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

All of them are unversioned (see `cil-flow-api.md` §6.5). The frontend's own
temporaries (§6.3's `t`, `c_k`, `u_k`, `cell_k`, and §7's delegate loads) are
fresh locals with prefixes of their own, so they never collide with `t{k}`.

---

## 6. Effects → statements (`effects.rs`)

`F(field)` means `FieldRef::symbol(..)`: `Named(f)` gives `<cls::f>`, `Element`
gives `"[]"`, `Length` gives `"length"`, `Deref` gives `"deref"`. `ap(v)` means
`AccessPath::without_fields(var(v))`.

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

The decided rule is to lower to the traced place, with `deref` only as a
fallback. `MethodFlow::resolve_address(at, i)` gives the `Place` for input `i`.

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
| `Load dst := v.*` | read `P` into `dst` | `load(dst, v, deref)` |
| `Store v.* := x` | write `x` into `P` | `store(v, deref, x)` |
| `Load dst := v.f` (struct through pointer) | field `f` of `P` | `t := load v.deref; load(dst, t, f)` |
| `Store v.f := x` | field `f` of `P` | `t := load v.deref; store(t, f, x)` |
| `Clear v` | `assign(P, [])` for `Local`/`Arg`; nothing otherwise | nothing |

**Byref call arguments: copy-in/copy-out.** For each index in
`CallSite::byref_args` whose argument resolves to a place `P`:

```text
c_k := read P               copy-in
store cell_k.deref := c_k
call f(..., cell_k, ...)    cell_k is passed where the pointer was
u_k := load cell_k.deref    copy-out
write u_k into P
```

`cell_k` is a fresh local per call site and argument. Under
`ParamWriteBack::Entry`, the callee's `store p.deref := v` comes back to the
caller's `cell_k`, so `u_k` sees it, and through it `P` does too. When
the argument is unresolved, it is passed as is: it is already a pointer
variable, and the callee's `deref` writes come back to it.

**The rare case: an address merged at a join.** C# `ref` conditionals
(`ref (c ? ref a : ref b)`) and hand-written IL can merge two addresses, and
then `resolve_address` returns `None` at the use. Reads still need to work, so
the fallback is: an `AddressOf { dst, P }` whose `dst` reaches an unresolved
use (the flow layer reports this, see §10) also emits `c := read P; store
dst.deref := c`. Reads through the merged pointer then see both places. Writes
through it reach only `dst.deref`, not `a` or `b`. That is a known
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
in `names.rs`.

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
| Pointer lowering | Traced place, else `deref`; copy-in/copy-out for byref arguments | `ptr.rs` |
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
blessed, its table rows are implemented with no `_` arms, and an end-to-end
taint test passes under `xtask regression --frontend cil`.

| # | Slice | Rows | Fixture / test |
| --- | --- | --- | --- |
| 1 | Straight-line static methods | §5, §6.1 `Assign`/`Const`, §8 `Return` | `string Id(string s) => s;` |
| 2 | Branches and loops | §8 `Goto`/`Cond`/`Switch` | taint through `?:` and `for` |
| 3 | Fields, statics, arrays | §6.1 `Load`/`Store`/`New` | field and array round trips |
| 4 | Direct calls and extern stubs | §7 `Static`/`Instance`/`Ctor`, §4.4 | source → helper → sink |
| 5 | Hierarchy, virtual calls, CHA | §4.1, §7 `Virtual`, sealed/non-virtual | interface dispatch with two implementations |
| 6 | Generic overrides and explicit implementations | §4.2, §4.3 | `D : B<string>`; `IComparer<T>`; `void IFoo.Bar()` |
| 7 | Exceptions | §8 handlers, block splitting | `catch (E e) { sink(e.Message); }` |
| 8 | Managed pointers | §6.3 | `TryParse(s, out n)`, `ref` accumulator, struct field through `ldloca` |
| 9 | Delegates and lambdas | §7 delegates | `Func<string,string>` and an event handler |
| 10 | Multiple assemblies and forwarding | §3.3, `import_directory` | app + library DLL; `System.Object` via a facade |

Slices 1–4 need only `ctadl-ir` as it is today, and can use `CallStyle::DirectCall`
everywhere. Slice 5 is the first that needs the engine plumbing (`CilCall` and
the `Cil` VMT variant), so that should land by then.

---

## 12. Open questions

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
