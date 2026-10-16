# Plan: CIL / .NET bytecode support - DO-NOT-MERGE

Adds a `.NET` frontend to CTADL that mirrors what the Dex and JVM frontends do:
class-hierarchy analysis over the type graph, `CallStyle`-tagged virtual calls that
feed CHA + hybrid inlining, field-sensitive lowering of member access, and a
per-language default model file.

The bytecode reader already exists: `cil-reader` (M0–M7 of its original plan,
byte-identical to Mono.Cecil on 114/114 corpus files; its fixture-backed tests
now run in `xtask regression --frontend cil`, see §1). What it does **not** have is the layer both other bytecode frontends depend
on — a per-instruction dataflow view — so that is where this plan starts.

## Status (2026-10-15)

M0–M4 are done, and M5 but for source lines. M1–M3 have detailed designs in
their own documents, which say what was built; this plan is the only written
record of M5b–M7, so keep it until those land.

| M | Work | Status |
| --- | --- | --- |
| M0 | Workspace integration | **Done** (differently from the steps written; see §1) |
| M1 | `cil_reader::flow` Layer 1 | **Done.** [`cil-flow-api.md`](cil-flow-api.md) §10 says what was built and where it departs from the design. Reader helpers done (§2.1). |
| M2 | `flow` Layer 2: CFG and stack heights | **Done.** [`cil-flow-api.md`](cil-flow-api.md) §4, §10. Gated by `cil:flow`. |
| M3 | The `ctadl-cil` frontend | **Done**, in `frontends/ctadl-cil/`. [`cil-frontend-translation.md`](cil-frontend-translation.md) §14 says what was built and where it departs from the design. |
| M4 | Engine plumbing (`Cil` VMT, `CilCall`, dispatch key) | **Done**, as §5 lists, plus `pinvokes` and a fix the CIL checks found: the index now summarizes every return slot, so exceptions reach callers (Dex and JVM too). |
| M5 | Regression harness | **Done but for source lines (M5b).** `cil:frontend` (corpus sweep and IR goldens), `cil:taint` (summaries against .NET runs) and the `Cil:*` taint cases in `nightly/tests/cil`, which compile C# with `dotnet build` and run import → index → query. With no source lines, a case is judged by the sink functions its flows end in. |
| M5b | Portable-PDB sequence points | **Not started.** `MethodDebugInformation` exists only as a table and token id. |
| M6 | `cil-index.jsonl` default models | **Not started** |
| M7 | P/Invoke bridge, Xamarin | **Not started** (optional) |

---

## 0. Where the seams are

The analyzer's language-neutral core and its per-language extension points, as they
stand today:

| Concern | Extension point | Java/Dex value |
| --- | --- | --- |
| Import dispatch | `ArtifactLanguage` + `cli::import` match | `Dex`, `Apk`, `Jar`, `Jvm` |
| Frontend | `frontends/ctadl-<lang>/src/` → `ProgramInfo` | `ctadl-dex`, `ctadl-jvm` |
| Type hierarchy + method table | `VirtualMethodTable` variant | `Java { methods, hierarchy, natives }` |
| Virtual call site | `CallStyle` variant | `JavaCall { receiver, cls, simple_name, descriptor }` |
| Allocation site | `CallObject` variant | `JavaObject(JavaClass)` |
| CHA | `ChaLanguage` + `ClassHierarchyAnalysis::new` arm (shared `run_cha`) | `ChaLanguage::Java` |
| Hybrid inlining key | `facts::CallDispatchKey` / `CallTargetObject` | `Java(name, desc)` / `Symbol(cls)` |
| Model matching | `models::match_index` VMT arm | class / simple name / descriptor / fq id |
| Default models | `models::default_model_file` | `defaults/java-index.jsonl` |
| Regression harness | `xtask::discovery::{Frontend, Kind}` + `xtask/src/<lang>.rs` | `dex.rs`, `jvm.rs` |

`facts::CallDispatchKey`'s own doc comment names it "the *single extension point* for
a new language frontend's call-resolution scheme". Lua went through exactly these
seams and is the working precedent for adding one; this plan follows it.

**The IR contract every Java-shaped frontend honors, which CIL will too:**

- every function returns arity 2: `(normal_return, exception_return)`;
- `retval` / `throwval` are the conventional local names for a call's two results;
- fields are `FieldPath::symbol("<Class::field>")`, array elements `"[]"`, array
  length `"length"`;
- statics are fields of `VariableRef::new_global()`;
- `this` is parameter 0 and `ParameterType::ByRef`; reference-typed parameters are
  `ByRef`, primitives `ByVal`.

Keeping these means the shared codegen, the SSA pipeline, the hybrid-inlining rules,
and the JSON model language all work on CIL with no changes.

---

## 1. Milestone M0 — fold `cil-reader` into the workspace

> **Status: done, differently from steps 1–3 below.** The crates stayed in place:
> `readers/cil-reader/crates/{cil-reader,cil-dump}` are root workspace members, and
> the nested `[workspace]`, `Cargo.lock` and xtask are gone. The parity harness runs
> nightly as `xtask regression --frontend cil` (`cil:*` checks) against a CecilDump
> the flake builds offline as a cached derivation (`packages.cecil-dump`); the
> nested xtask's artifact-regenerating commands became `cargo xtask cil
> {bless,codegen}`, and `vendor` was dropped in favor of the flake's pinned `cecil`
> input (`$CECIL_SRC` in the dev shell). The fixture DLLs are no longer committed:
> the flake builds them reproducibly as `packages.cil-fixtures`
> (`$CIL_READER_FIXTURES`). The ported Cecil test suites need both, so they left
> `cargo test` for the `cil:cecil-tests` check in `xtask regression`
> (`xtask/src/cil/cecil_tests`). The parity rules (`xtask/src/cil/parity.rs`,
> formerly `cil_dump::parity`) and the goldens (`xtask/cil-goldens`, formerly
> `readers/cil-reader/tests/expected`) moved into xtask too.

`readers/cil-reader/` is currently a nested Cargo workspace with its own lock file, an xtask
that needs nix + the .NET SDK, and a vendored (gitignored) Mono.Cecil checkout. The
library itself has **zero dependencies** and its 166 tests run off 19 committed
`.dll` fixtures with no .NET involved — so the useful half integrates cleanly and the
Cecil-parity harness can stay behind.

1. Move `readers/cil-reader/crates/cil-reader` → `<root>/cil-reader/` as a plain package
   (`version.workspace = true`, `edition = "2024"`), and `readers/cil-reader/crates/cil-dump`
   → `<root>/cil-dump/` (its `cli` feature is already off by default, and its manifest
   already anticipates "depending on cil-reader from an outer workspace").
2. Add both to the root `[workspace] members`. Delete the nested `[workspace]` and
   `Cargo.lock`.
3. Keep the Cecil-parity harness (`xtask`, `tools/CecilDump`, `extern/cecil`,
   `tests/expected/`) in an `exclude`d `cil-reader-parity/` subtree. It stays runnable
   by hand and in a nix job; it is not on the main `cargo test` path.
4. `crates/cil-reader/tests/resolve.rs` reads
   `extern/cecil/symbols/mdb/Test/Resources/assemblies`. Make that a helper that
   returns `None` and skips when the directory is absent, so the suite is green on a
   plain checkout.
5. Replace the `DO NOT PUSH cil reader crate for testing` commit with a real
   vendoring commit.

**Acceptance:** `cargo test -p cil-reader` green from the repo root with no .NET SDK
and no nix.

---

## 2. Milestone M1 — `cil_reader::flow`, part 1: per-instruction dataflow

> **Status: done.** See [`cil-flow-api.md`](cil-flow-api.md) §10. The design is
> [`cil-flow-api.md`](cil-flow-api.md) §3, §5 and §7.1–7.6 (types, per-opcode
> effects, tests). What the Dex and JVM readers get wrong, which this design
> avoids, is in `docs/dex-jvm-reader-improvements.md` on the
> `dex-jvm-reader-fixes` branch.

### 2.1 Reader-side helpers M1 also needs

Small additions to the existing `cil-reader` model layer, all mechanical. Status
of each is in brackets.

- **[Done]** `MethodBody::code_file_offset() -> u32` — the PE byte offset of the first code
  byte. Requires threading `PeImage::resolve_rva` (already public on `raw`) out
  through the model; `MethodBody` currently holds a private `&ModuleData`.
- **[Done]** `Ins::size()` / `Ins::end_offset()`. Only `OpCode::size()` (the
  opcode bytes, not the operand) exists.
- **[Done]** `MethodBody::user_string(tok) -> Result<String>` for `ldstr`. The `#US` heap is
  reachable today only via `Module::metadata().user_strings()`; a body cannot get
  back to its module (`MethodBody::m` is private), so a frontend walking bodies has
  to thread the `Module` alongside.
- **[Done, as `flow::Operands` and `MethodBody::{field,type,method}_name`]** Uniform operand resolution: `resolve_method`, `resolve_field`, `resolve_type`
  returning name structs, each handling all of Def / Ref / Spec / MemberRef /
  MethodSpec. `resolve_operand` already returns the `Entity`; these put a name on it.
  `MethodBody::resolve_operand` and the resolver's `resolve_type` exist. The
  name-returning trio does not.
- **[Done, with `is_final`]** `MethodDef::is_virtual() / is_newslot() / is_abstract()` from `attributes()` (the
  masks exist, the accessors don't). Still true. The frontend needs these for
  override aliases and for lowering a non-virtual `callvirt` as a direct call.
- **[Done]** `MethodDef::overrides()` (`MethodImpl` rows), `MethodDef::pinvoke_info()`,
  and `TypeDef::is_sealed()` / `is_interface()` / `is_abstract()`, which the
  frontend also needs.

---

## 3. Milestone M2 — `cil_reader::flow`, part 2: CFG and stack heights

> **Status: done.** See [`cil-flow-api.md`](cil-flow-api.md) §10. The design is
> [`cil-flow-api.md`](cil-flow-api.md) §2, §4, §7.7–7.9 and §8.1: basic blocks
> and exception edges, stack heights and kinds (no values, no phis), and never
> failing a method on a bad stack.

---

## 4. Milestone M3 — the frontend, `frontends/ctadl-cil/`

> **Status: designed, not implemented.**
> [`cil-frontend-translation.md`](cil-frontend-translation.md) replaces this
> section, and where they differ it wins. The differences:
> - The crate goes in `frontends/ctadl-cil/`, re-exported from
>   `ctadl-ascent/src/languages/mod.rs` the way `ctadl_dex` is.
> - §4.1: descriptors write generic parameters by position (`!0`, `!!0`), not
>   by Cecil's names (translation §3.2).
> - §4.2: the hierarchy also needs `interfaces` and `abstract_methods`.
>   Generic overrides and `MethodImpl` rows become override-alias `methods`
>   rows, not hierarchy edges (translation §4.2).
> - §4.5: there is no `CallKind::DelegateInvoke`; delegates come from
>   `flow::delegate::classify`. A sealed or non-virtual `callvirt` is a
>   `DirectCall`, and `constrained.` is used only when the type declares or
>   inherits the method (translation §7).
> - §4.6: `box`/`unbox` preserve contents, not identity. `ldobj`/`stobj`/
>   `initobj`/`cpobj` and `ldelema` go through managed-pointer lowering, not
>   whole-object assigns (translation §6).

Structurally a sibling of `ctadl-dex/src/lib.rs`: a `Context` with
`process(module, artifact_key, &mut Builders)` callable once per assembly and
`finish(builders)` once, so a multi-assembly application lands in one `ProgramInfo`
with one hierarchy — the same shape the Dex frontend uses for multiple
`classes*.dex` and the JVM frontend for multiple `.class` in a jar.

### 4.1 Naming

Names are the contract between the frontend, CHA, the model files, and the SARIF
output. Proposal, ILAsm-flavored and assembly-qualified:

| Thing | Spelling | Example |
| --- | --- | --- |
| Class (VMT `cls`) | `[Asm]Namespace.Type` (nested: `/Inner`) | `[MyApp]Acme.Widgets.Gadget` |
| Simple name | metadata name, generic arity appended | `Parse`, `Select``1` |
| Descriptor | `(P1,P2)->Ret`, Cecil type spellings | `(System.String)->System.Int32` |
| Function id | `cls::name descriptor` | `[MyApp]Acme.Gadget::Parse(System.String)->System.Int32` |
| Field | `<[Asm]Ns.Type::field>` | `<[MyApp]Acme.Gadget::_name>` |

Assembly qualification is what unifies names **across** assemblies: a `callvirt` in
`A.dll` to a `TypeRef` whose resolution scope is `AssemblyRef B` spells the class
`[B]Ns.Base`, and the definition in `B.dll` spells it `[B]Ns.Base` too, so CHA joins
them. Use the *simple* assembly name (no version, no public key token) so
`[System.Runtime]` matches across framework versions.

**Known hazard:** type forwarding. `mscorlib`/`System.Runtime` facades forward
`System.Object` to `System.Private.CoreLib`, so two spellings can name one type.
Mitigation: build an alias table from each assembly's `ExportedType` forwarder rows
(cil-reader already reads them, and `AssemblyCache`/`resolve_type` already follows
them) and canonicalize class names through it in `Context::finish`.

**Generics erasure:** key CHA on the *open* definition, so `List``1<System.String>::Add`
and `List``1<System.Int32>::Add` both resolve to `List``1::Add`. This mirrors what
Java erasure gives the existing frontends for free, and is what keeps CHA over
generic .NET code tractable. Keep the instantiation for display only.

### 4.2 Type hierarchy → VMT

For each `TypeDef` from `module.all_types()` (includes nested):

- `hierarchy[cls] = [base_type()] ++ interfaces()`, exactly as
  `ctadl-dex/src/lib.rs` builds it from `superclass_idx` + `class_interfaces`.
- `methods.push((cls, simple_name, descriptor, function_id))` for every declared
  method, body or not — the Dex frontend's comment on `natives` explains why a
  bodyless method still needs a row: without one, a virtual call to it resolves to
  nothing and the call site is lost.
- `MethodDef::overrides()` (`MethodImpl` rows) gives explicit interface
  implementations, which are *not* recoverable from names alone in .NET (unlike
  Java). Feed them into the hierarchy so `IFoo::Bar` resolves to
  `Type::IFoo.Bar`. This has no Java analogue and must not be skipped.

### 4.3 Method → `FunctionData`

- Params: `has_this` → param 0, `ByRef`. Then one param per `MethodSig.params`
  entry: `ByRef` for `TypeSig::{Named{is_value_type:false}, String, Object, SzArray,
  Array, ByRef, Ptr, GenericInst{is_value_type:false}, Var, MVar}`, `ByVal` for the
  primitives. (`Var`/`MVar` are unconstrained generic parameters — `ByRef` is the
  conservative choice.)
- `return_type = ReturnType { arity: 2 }` — `(normal, exception)`, matching Dex/JVM
  so the shared Java-style models and the `throwval` convention carry over.
- Blocks 1:1 with `MethodFlow` blocks. The frontend emits no phis, because SSA places them.
- Terminators: `ret` → `Return { args: [result_or_empty, empty] }`; `throw`/`rethrow`
  → handler successors if any, else `Return { args: [empty, throwval] }`;
  everything else → `Goto { targets: successors }`, or a two-empty `Return` when
  there are none. Exactly the Dex terminator logic.
- `SourceInfo` from `builders.source_info_builder.span_for(key,
  instr.file_offset, SpanLen::ByteLen(instr.size))`.

### 4.4 `Effect` → `StatementKind`

Nearly mechanical, which is the point of M1's design:

| Effect | Statement |
| --- | --- |
| `Assign { dst, srcs, .. }` | `StatementKind::assign(var(dst), srcs)` |
| `Load { dst, src: Path { root, field } }` | `StatementKind::load(var(dst), var(root), field_path(field))` |
| `Store { dst: Path { root, field }, src }` | `StatementKind::store(AccessPath::without_fields(var(root)), field_path(field), src)` |
| `New { dst, ty, .. }` | `assign(var(dst), [Exp::new_object_ref(CallObject::CilObject(ty))])` |
| `Throw { src }` | `assign(throwval, [src])` |

with `Field::Named(f)` → `FieldPath::symbol("<f>")`, `Element` → `"[]"`,
`Length` → `"length"`, `Deref` → `"deref"`. `Root::Static` is
`VariableRef::new_global()`, matching how Dex lowers `SGet`/`SPut`. The full
table, including pointer roots, is translation §6.

Variables: `Var::Arg(n)` → `VariableRef::new_parameter(n)`; `Var::Local(n)` →
local `loc{n}`; `Var::Stack(depth)` → local `s{depth}`. All of them are unversioned.

**No aliasing heuristics.** `ldarg.0; …; stfld f` lowers to `s0 := p0; s0.f := v`.
After SSA, `propagate_copies` rewrites the store's base to `p0`. So the frontend does
not scan backwards for the `ldarg`; the pipeline does that work.

**SSA settings.** `ArtifactLanguage::Cil` returns `ParamWriteBack::Entry` from
`param_write_back()`, as JVM and Dex do. `starg` rebinds only the callee's own copy of
an argument, while a write through a `ref`/`out` parameter is a `deref` store on the
entry version, which is exactly what `Entry` hands back to the caller.

### 4.5 Calls

```rust
CallKind::Static | CallKind::Instance | CallKind::Ctor
    => CallStyle::DirectCall { call_edges: CallEdges::Explicit([target]) }
CallKind::Virtual { constrained }
    => CallStyle::CilCall { receiver, cls: constrained.unwrap_or(declaring_type),
                            name, descriptor }
CallKind::Indirect { callee }
    => CallStyle::FuncPtrCall { callee, signature }
```

A `callvirt D::Invoke` that `flow::delegate::classify` reports as `Invoke` is
also a `FuncPtrCall`, on the function pointer stored in the delegate
(translation §7).

`CallStyle::CilCall` is what codegen turns into either an exact `call` edge (one CHA
resolvent), or a `callee_info` fact keyed on `CallDispatchKey::Cil(name, descriptor)`
for hybrid inlining — the identical treatment `CallStyle::JavaCall` gets today.

Every callee that is never defined in the imported assemblies gets a stub function
plus a VMT row at `finish`, mirroring `Context::ext` in the Dex and JVM frontends.
That is what makes an external callee nameable by a model.

### 4.6 CIL-specific lowering the Java frontends have no analogue for

These are not polish; they are where most real .NET taint lives.

- **Managed pointers / `out` and `ref` parameters.** `ldloca`/`ldarga`/`ldflda`/
  `ldelema` push an address (`Effect::AddressOf`); `stind.*`/`stobj` write
  through it; `ldind.*`/`ldobj` read through it. Lowered as `Store`/`Load` with the
  `deref` field, the convention the C frontend already uses. Without this,
  `int.TryParse(s, out n)` — and every `TryGetValue`, every `ref` accumulator — loses
  its flow.
- **`newobj`** is allocation + constructor in one instruction: `New` then a `Ctor`
  call with the fresh object as receiver. This is strictly simpler than the JVM's
  `new` / `dup` / `invokespecial` sequence, and the `init_survivor_link` hack in
  `ctadl-jvm/src/lib.rs` has no counterpart here.
- **Delegates.** `ldftn`/`ldvirtftn` push a function pointer (`Effect::FunctionPtr`); the
  `newobj D::.ctor(object, native int)` that consumes it makes the delegate object
  carry that function pointer; `callvirt D::Invoke` calls it.
  `flow::delegate::classify` recognizes both. Map to
  `CallObject::FunctionPtr` + `CallStyle::FuncPtrCall`, which the engine already
  resolves through the identity `callee_resolvents(FunctionId(f), C, f)`. This is how
  events, callbacks, and LINQ lambdas get call edges at all.
- **Value types.** `ldobj`/`stobj`/`initobj`/`cpobj` are whole-struct copies; lower
  as whole-object assigns so field-sensitive taint follows the fields.
- **Boxing / casts.** `box`/`unbox`/`unbox.any`/`castclass`/`isinst` are
  identity-preserving copies; `box` additionally records the value type as an
  allocation so CHA's `instantiated_classes` sees it.
- **Arrays.** `newarr` → `New`; `ldelem*`/`ldelema` → `Load .[]`; `stelem*` →
  `Store .[]`; `ldlen` → `Load .length`. Same field symbols as Dex/JVM, so array
  models are shared.
- **Statics.** `ldsfld`/`stsfld` → `Load`/`Store` on `VariableRef::new_global()`.
- **P/Invoke.** `MethodDef::pinvoke_info()` gives the module and entry-point name of
  every `[DllImport]` method. Record it in a VMT column (see §5) — it is the .NET
  analogue of the Java `natives` column, and the input a future CIL↔pcode bridge
  needs. It is strictly *easier* than JNI: the symbol name is stated outright rather
  than derived from a mangling convention.

### 4.7 Import entry point

`ctadl_cil::import_assembly(path)` for a single `.dll`/`.exe`, and
`import_directory(path)` walking a published-app directory, both producing one
`ProgramInfo`. One `ArtifactLanguage::Cil` accepts either, as `ArtifactLanguage::C`
already does for a file or a directory of sources.

**Assembly-selection policy** (mirrors the Dex frontend not importing the Android
framework): by default import only assemblies that are not framework/BCL — detect via
the presence of a matching `.runtimeconfig.json`/`.deps.json`, or a
`System.*`/`Microsoft.*` name prefix — and let the default model file cover the
framework. A self-contained publish drops ~200 assemblies in one directory; importing
all of them is neither necessary nor affordable. Provide a flag to override.

---

## 5. Milestone M4 — engine plumbing

> **Status: not started.** No `VirtualMethodTable::Cil`, `CallStyle::CilCall`,
> `CallObject::CilObject`, `CallTargetObject::CilClass`, `CallDispatchKey::Cil`,
> `ArtifactLanguage::Cil` or `ChaLanguage::Cil` exists. The translation doc's
> slice 5 is the first that needs this. The VMT row should also add
> `interfaces` and `abstract_methods` (translation doc §4).

**Recommendation: add a dedicated `Cil` variant everywhere rather than reusing the
`Java` one.**

Reusing `VirtualMethodTable::Java` would work — the frontend would just produce
Java-shaped names, and codegen/CHA/hybrid inlining would need no changes at all. It
is genuinely the fastest route to something running. The reasons not to:

1. `models::default_model_file` keys on the VMT variant, so a `Java` VMT gets
   `java-index.jsonl` — 55 generators that cannot match a .NET name — and there is
   nowhere to put .NET defaults, which §7 argues are load-bearing.
2. `callee_resolvents` is keyed on `(CallTargetObject, CallDispatchKey)`. A Xamarin
   or MAUI app is *both* Dex and CIL in one project, and that is exactly the
   collision `CallTargetObject::LuaClass` was introduced to avoid.
3. The JNI observer runs on `VirtualMethodTable::Java` and would be handed CIL
   methods.
4. Flipping later invalidates every on-disk index and every model file written in
   the meantime.

The additive change set, following Lua's precedent exactly:

| File | Change |
| --- | --- |
| `ctadl-ir/src/mir/call.rs` | `VirtualMethodTable::Cil { methods, hierarchy, interfaces, abstract_methods, pinvokes }`, `CallStyle::CilCall`, `CallObject::CilObject`, `CilClass`/`CilMethod`/`CilSignature` newtypes, `new_cil()`, `Display` arms |
| `ctadl-ir/src/mir/visit/mod.rs` | visitor arm for `CilCall` |
| `ctadl-ascent/src/facts.rs` | `CallTargetObject::CilClass`, `CallDispatchKey::Cil(name, desc)` |
| `ctadl-ascent/src/facts/parquet.rs` | two tag bytes + encode/decode arms |
| `ctadl-import/src/project.rs` | `ArtifactLanguage::Cil`, `ALL`, `name()`, `param_write_back()` → `Entry`; bump `INDEX_FORMAT_VERSION` |
| `ctadl-ascent/src/cli/mod.rs` | import dispatch arm |
| `ctadl-ascent/src/codegen/mod.rs` | `ChaLanguage::Cil` (reuses `run_cha` unchanged), `CilCall` codegen arm mirroring `JavaCall`, `emit_callee_resolvents` arm, `InstantiationFinder` arm |
| `ctadl-ascent/src/models/{mod,match_index,json,codegen}.rs` | VMT arms; `CIL_DEFAULT_MODELS` |
| `ctadl-ascent/src/languages/mod.rs` | `pub use ctadl_cil as cil;` (the crate is `frontends/ctadl-cil`) |
| `ctadl-ascent/src/languages/jni.rs` | `SlotModel::for_language` arm (CIL is not a JNI language) |

`run_cha` itself needs no change: it is already language-neutral, and the `Cil` arm
feeds it `method_implemented` + `direct_superclass` exactly as the Java and Lua arms
do.

---

## 6. Milestone M5 — regression harness

> **Status: reader checks only.** `Frontend::Cil` and `xtask/src/cil.rs` exist,
> but they hold the Cecil-parity checks for the reader, not what this section
> describes. There is no `Kind::Cil`, no `xtask/tests/cil/*.cs`, and no
> end-to-end check. The flow-layer property tests are written: `cil:flow`. Source
> lines (step 2 below, M5b) are not started.

`xtask/src/cil.rs` + `Frontend::Cil` + `Kind::Cil { cs, config }` in
`xtask/src/discovery.rs`, following `xtask/src/jvm.rs`:

- Compile `xtask/tests/cil/*.cs` with `dotnet build` (skip the whole frontend with
  `Outcome::Skip` when the SDK is absent, exactly as the jvm harness does for
  `javac`).
- Reader-level checks: decode every method in every produced assembly; assert the
  M1/M2 property tests hold; compare the instruction dump against `ildasm`/`ilspycmd`
  where available.
- End-to-end: `ctadl import` → `index` → `query` with a planted source→sink, assert
  the SARIF findings.

**Source-line mapping is a real gap.** cil-reader's M8 (portable PDB) is unimplemented,
so there is nothing to map an IL offset back to a `.cs` line with. Two steps:

1. **Now:** assert on IL offsets / function names rather than source lines, the way
   the pcode harness works around the same problem.
2. **Then:** implement the *sequence points* subset of portable PDB in cil-reader.
   The full M8 is ~2600 lines in Cecil, but `MethodDebugInformation`'s document +
   sequence-point tables are a small, well-specified, self-contained slice, and
   `dotnet build` emits a `.pdb` next to every assembly by default. Without it, SARIF
   output for .NET points at byte offsets, which is a poor deliverable.

---

## 7. Milestone M6 — default models (`defaults/cil-index.jsonl`)

> **Status: not started.** `ctadl-ascent/src/models/defaults/` holds only the
> Java, Lua and native model files.

Not polish. The Java defaults are 55 generators, and without the .NET equivalent
every BCL call is an opaque barrier that taint does not cross. Minimum viable set:

- **Propagation:** `System.String::{Concat,Format,Join,Substring,Trim,Replace,Split}`,
  `System.Text.StringBuilder::{Append,AppendFormat,ToString}`, `System.Convert::*`,
  `System.Text.Encoding::{GetString,GetBytes}`, `System.IO.Path::Combine`,
  `System.Uri::.ctor`, LINQ (`Enumerable::{Select,Where,ToList,ToArray,First}`),
  collection `Add`/indexer/`TryGetValue` (the last one is the `out`-parameter case),
  `System.Text.Json.JsonSerializer::{Serialize,Deserialize}`,
  `Newtonsoft.Json.JsonConvert::*`.
- **Dangerous sinks** worth shipping as recognized endpoints:
  `System.Diagnostics.Process::Start`, `System.Data.*::ExecuteNonQuery` /
  `SqlCommand::.ctor`, `System.IO.File::*`, `System.Reflection.Assembly::Load`,
  `System.Type::GetType` + `Activator::CreateInstance`,
  `System.Runtime.InteropServices.Marshal::*`, `XmlDocument::LoadXml`,
  `BinaryFormatter::Deserialize`.
- **`modes: ["skip-analysis"]`** for anything huge and uninteresting that would
  otherwise multiply hybrid-inlining resolvents.

---

## 8. Milestone M7 (optional) — cross-language bridges

> **Status: not started.**

- **P/Invoke ↔ pcode**, reusing the JNI bridge machinery: `[DllImport("libfoo",
  EntryPoint="bar")]` names the native symbol outright, so the join is a direct
  symbol lookup rather than JNI's mangling convention plus `RegisterNatives`
  recovery. The `pinvokes` VMT column from §4.6 is the input.
- **Xamarin / .NET-Android**, where an APK holds *both* Dex and CIL. This is the
  concrete case that justifies the separate key space in §5.

---

## 9. Risks and open questions

| Risk | Mitigation |
| --- | --- |
| Type-forwarding facades give one type two assembly-qualified names | Canonicalize through an `ExportedType` forwarder alias table in `Context::finish`; cil-reader already reads forwarders |
| `async`/`await` and iterators compile to state machines; locals become fields of a generated struct | Field-sensitive taint should follow this *for free* — but it must be pinned by a dedicated regression case, because the whole method body moves into `MoveNext` |
| Lambdas/closures compile to display classes | Same story as above; same test |
| NativeAOT-published apps contain no IL | Out of scope — that is the pcode path |
| Obfuscated / non-verifiable IL breaks the stack simulation | The graceful-degradation rule in `cil-flow-api.md` §8.1 is exactly this; log and continue instead of dropping the method |
| Self-contained publishes drop ~200 assemblies | The assembly-selection policy in §4.7, plus `skip-analysis` models |
| Generic instantiation explosion in CHA | Erasure to the open definition (§4.1) |
| Reusing the Java VMT for speed, then flipping | Invalidates indexes and model files; decided against in §5 |

---

## 10. Sequencing and effort

| M | Work | Est. | Depends on | Status |
| --- | --- | --- | --- | --- |
| M0 | Workspace integration | 0.5 d | — | Done |
| M1 | `flow`: per-instruction effects, operand resolution, reader helpers | 3–4 d | M0 | Done |
| M2 | `flow`: CFG + stack heights (no phis; SSA places them) | 4–5 d | M1 | Done |
| M3 | `ctadl-cil` frontend | 5–7 d | M2 | Designed |
| M4 | Engine plumbing (`Cil` VMT / CallStyle / dispatch key) | 1–2 d | — (parallel with M1–M3) | Not started |
| M5 | xtask regression harness | 2–3 d | M3, M4 | Reader checks only |
| M5b | Portable-PDB sequence points → source lines | 2–3 d | M5 | Not started |
| M6 | `cil-index.jsonl` default models | 2–3 d | M5 | Not started |
| M7 | P/Invoke bridge, Xamarin | optional | M6 | Not started |

Roughly **3–4 weeks** to a frontend at parity with the JVM one, plus the optional
bridge work.

## 11. Acceptance

> **Status:** item 1 is met. Item 2 is met for the reader and the flow layer
> (`cil:flow` included); the frontend's own checks do not exist yet. Items 3–5
> need the frontend.

1. `cargo test -p cil-reader` green from the repo root with no .NET SDK,
   including the new `flow` unit tests. The fixture and corpus property tests
   are the `cil:flow` check under item 2 (`cil-flow-api.md` §9).
2. `cargo xtask regression --frontend cil` green, or cleanly skipped without the SDK.
3. `ctadl go app app.dll query.json` reports a planted source→sink path, with SARIF
   regions pointing at `.cs` lines (after M5b).
4. A virtual call with more than one CHA resolvent produces `callee_info` facts and
   is resolved by hybrid inlining — verifiable in `ctadl index`'s
   `hybrid inlining: ... resolvent: N` line, the same way the Java frontends are.
5. `frontends/ctadl-cil/` contains **no** backwards-scanning heuristics to recover
   the object base of a field access. If it does, either copy propagation is missing
   a case or the flow layer is, and the fix belongs there rather than in a
   workaround here.
