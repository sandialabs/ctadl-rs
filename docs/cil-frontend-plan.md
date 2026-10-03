# Plan: CIL / .NET bytecode support - DO-NOT-MERGE

Adds a `.NET` frontend to CTADL that mirrors what the Dex and JVM frontends do:
class-hierarchy analysis over the type graph, `CallStyle`-tagged virtual calls that
feed CHA + hybrid inlining, field-sensitive lowering of member access, and a
per-language default model file.

The bytecode reader already exists: `cil-reader` (M0–M7 of its own `PLAN.md`, 166
tests green off committed fixtures, byte-identical to Mono.Cecil on 114/114 corpus
files). What it does **not** have is the layer both other bytecode frontends depend
on — a per-instruction dataflow view — so that is where this plan starts.

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
> (`xtask/src/cil/cecil_tests`).

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

This is the piece the request calls out, and the place to improve on the existing
patterns rather than copy them.

### 2.1 What `dex-reader`'s pattern gets wrong

`dex_reader::instructions::DataFlow` is three boxed streaming iterators:

```rust
pub struct DataFlow {
    pub source: DynIter,   // Box<dyn DoubleEndedStreamingIterator<Item = Reg>>
    pub dest: DynIter,
    pub ret: DynIter,
}
```

Concrete problems, each visible in `ctadl-dex/src/lib.rs`:

1. **Three heap allocations per instruction**, plus `RegSliceStream` cloning the
   argument slice into a `Vec` for every call site. It also forces the
   `streaming-iterator` crate on every consumer (`ctadl-ascent` imports
   `StreamingIterator` solely to call `dest.owned()`).
2. **Bags, not roles.** `(source, dest, ret)` erases *which* source is the object
   base, which is the index, which is the stored value. So the consumer re-decodes
   the instruction anyway — `dataflow_to_assign`'s `IPut` arm reads
   `source.filter(|r| *r != f.b)` and then uses `f.b` directly for the object, and
   `APut` filters two registers out by hand. The dataflow view is doing half the job
   twice.
3. **No operand data.** Field/string/type indices are not in `DataFlow`, so the
   consumer goes back to `parser.get_field(f.idx)` / `constant_pool().strings`.
4. **Silently inconsistent.** `IPut*` lists the object register as a *source*:
   `dataflow_source_sink_ret!([f.a, f.b], [], [])`. `IPutWide` lists it as a *dest*:
   `dataflow_source_sink_ret!([f.a, f.a.next_reg()], [f.b], [])`. Nothing catches
   this because the one consumer ignores both fields for that opcode. There is no
   property that could be asserted, because there is no independent source of truth
   about pop/push counts in Dex.
5. **Untestable.** `DataFlow` derives nothing — no `Debug`, no `PartialEq`. There are
   no unit tests of `data_flow()` itself; the only coverage is 13 tests of the
   *frontend's* translation in `ctadl-dex/src/tests.rs`.

### 2.2 What `jvm-reader`'s pattern gets wrong

`jvm_reader::flow` is closer — it has a `Location` enum carrying `Constant`,
`FieldRef`, `ArrayElement`, `Allocation`, which is the right instinct. But:

1. **Two representations for one thing.** Decode emits `Location::StackInput(depth)`
   / `StackOutput`; a later `normalize_stack_slots_for_method` pass *mutates* them
   into `Location::StackSlot(id)`. Every consumer must handle both, and
   `ctadl-jvm`'s `convert_location_to_var_ref` has a `"Stack Local?"` bucket for
   the case that should be impossible.
2. **Slot ids are stack depths, not definitions.** `StackSlot(n)` is the absolute
   depth the value sits at. Two unrelated values at depth 2 in different blocks get
   the same name, which merges flows that never merge.
3. **Consequence: the frontend re-implements value numbering.** To answer "which
   object does this `putfield` write to", `ctadl-jvm/src/lib.rs` carries
   `stack_aliases`, `dup_slot_pairs`, `last_aload_reg`, `note_assign_aliases`,
   `stack_slot_alias`, `aload_for_stack_slot`, `aload_register_before`,
   `resolve_object_var`, `field_object_base`, and `init_survivor_link` — roughly 200
   lines of backwards-scanning heuristics, with comments like "Prefer a stable
   register/parameter for this stack slot when it differs from the most recent
   aload". That is the abstraction leaking.
4. **Hand-written stack effects.** `stack_effect` and `misc_stack_effect` are
   opcode tables maintained by hand.
5. **Fails whole methods.** An inconsistent stack height at a join returns
   `Err(InvalidClassFileMessage(...))`, losing the method — and with it every call
   edge through it.

### 2.3 The design for CIL

Two things make CIL easier to do properly than either:

- `cil_reader::cil::opcodes::OpCode` **already carries `stack_pop` / `stack_push`
  (`StackBehaviour`) and `flow_control`**, generated from Cecil. Stack effect is
  derivable, not hand-written; only `Varpop`/`Varpush` (calls, `newobj`, `ret`) need
  the signature, which we have.
- Operands are metadata tokens, and the model layer already resolves them
  (`MethodBody::resolve_operand` → `Entity`, `full_name()` with Cecil parity).

New module `cil-reader/src/flow/` (`mod.rs`, `operand.rs`, `effect.rs`, `cfg.rs`,
`stack.rs`). Everything derives `Debug, Clone, PartialEq` so it is unit-testable.

**Values — one representation, minted at definition sites:**

```rust
/// A dataflow value. Stack values are named by their *definition site*, never by
/// stack depth: every push mints a fresh id, and a join mints a phi.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Value {
    /// Method argument slot (0 = `this` for instance methods).
    Arg(u16),
    /// Local variable slot (the `.locals init` signature).
    Local(u16),
    /// A stack temporary, id unique within the method.
    Stack(StackId),
}

/// Where a stack value came from. This is what lets a frontend anchor a `stfld` on
/// the underlying local instead of scanning backwards for the `ldarg` that pushed
/// the receiver — the thing `jvm-reader`'s consumer does by hand.
#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    Copy(Value),                 // ldarg / ldloc / dup
    AddressOf(Place),            // ldloca / ldarga / ldflda / ldelema
    Const(Const),                // ldc.* / ldstr / ldnull / ldtoken
    Alloc(TypeName),             // newobj / newarr / box
    FieldRead(FieldName),        // ldfld / ldsfld
    ElementRead,                 // ldelem
    CallResult { at: u32 },      // return value of the call at IL offset `at`
    FunctionPtr(MethodName),     // ldftn / ldvirtftn
    Exception,                   // handler / filter entry
    Phi(SmallVec<[Value; 4]>),   // block-join merge
    Computed,                    // arithmetic, conversions, comparisons
}
```

**Effects — one variant per CTADL IR statement, so the frontend is a translation
and not a re-decode:**

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// dest := sources (multiple sources = a join / non-deterministic merge)
    Assign { dest: Value, sources: SmallVec<[Value; 2]>, origin: Origin },
    /// dest := base.field   (field = Named / Element / Length / Deref)
    Load  { dest: Value, base: Value, field: Field },
    /// base.field := value
    Store { base: Value, field: Field, value: Value },
    /// dest := new T   (newobj/newarr/box; the ctor call is a separate Effect)
    New   { dest: Value, ty: TypeName },
    /// throwval := value  (throw / rethrow)
    Throw { value: Option<Value> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    /// `<[Asm]Ns.Type::name>` — an instance or static field.
    Named(FieldName),
    /// `[]` — array element.
    Element,
    /// `length` — array length.
    Length,
    /// `deref` — through a managed pointer (`ldind`/`stind`/`ldobj`/`stobj`).
    Deref,
}
```

**Calls — everything the frontend needs, resolved:**

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct CallInfo {
    pub kind: CallKind,
    pub target: Option<MethodName>,      // None for calli
    pub receiver: Option<Value>,         // instance calls; also newobj's fresh object
    pub arguments: SmallVec<[Value; 4]>, // in declaration order, receiver excluded
    pub result: Option<Value>,           // non-void
    pub signature: MethodSigName,        // params + return, for the CHA descriptor
}

#[derive(Debug, Clone, PartialEq)]
pub enum CallKind {
    Static,
    /// `call` on an instance method: non-virtual, exact target.
    Instance,
    /// `callvirt`. `constrained` carries the `constrained.` prefix's type, which is
    /// a *better* static receiver class than the declaring type on generic code.
    Virtual { constrained: Option<TypeName> },
    /// `newobj`: allocation + ctor in one instruction.
    Ctor,
    /// `calli` through a function pointer on the stack.
    Indirect { callee: Value },
    /// `callvirt D::Invoke` on a delegate.
    DelegateInvoke,
}
```

**Per-instruction view:**

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct InstructionFlow {
    pub offset: u32,          // IL offset within the method body
    pub file_offset: u32,     // byte offset in the PE image, for SourceInfo spans
    pub size: u32,
    pub opcode: OpCode,       // prefixes already folded in
    pub effects: SmallVec<[Effect; 2]>,
    pub call: Option<CallInfo>,
    pub branch: Branch,       // None / Fallthrough / Uncond / Cond / Switch / Leave / Ret / Throw
}
```

Prefixes (`volatile.`, `unaligned.`, `no.`, `readonly.`, `tail.`, `constrained.`) are
folded into the following instruction — they never appear as standalone steps, the
same way `jvm-reader` folds `wide`.

### 2.4 Reader-side helpers M1 also needs

Small additions to the existing `cil-reader` model layer, all mechanical:

- `MethodBody::code_file_offset() -> u32` — the PE byte offset of the first code
  byte. Requires threading `PeImage::resolve_rva` (already public on `raw`) out
  through the model; `MethodBody` currently holds a private `&ModuleData`.
- `Ins::size()` / `Ins::end_offset()`.
- `MethodBody::user_string(tok) -> Result<String>` for `ldstr`. The `#US` heap is
  reachable today only via `Module::metadata().user_strings()`; a body cannot get
  back to its module (`MethodBody::m` is private), so a frontend walking bodies has
  to thread the `Module` alongside.
- Uniform operand resolution: `resolve_method`, `resolve_field`, `resolve_type`
  returning name structs, each handling all of Def / Ref / Spec / MemberRef /
  MethodSpec. `resolve_operand` already returns the `Entity`; these put a name on it.
- `MethodDef::is_virtual() / is_newslot() / is_abstract()` from `attributes()` (the
  masks exist, the accessors don't).

### 2.5 Testing M1

- Unit tests per opcode family against the 19 committed fixtures, asserting exact
  `Vec<Effect>` — impossible today for either existing reader, cheap here because
  everything is `PartialEq`.
- **The property test the existing readers cannot have:** for every instruction in
  every fixture, `effects`/`call` consume exactly `stack_pop` values and produce
  exactly `stack_push`, cross-checked against the generated opcode table. This is
  what would have caught the `IPutWide` inconsistency in `dex-reader`.
- A handful of new `.il` fixtures for the shapes the Cecil corpus does not stress:
  `out`/`ref` parameters, delegates, `constrained.` callvirt, try/catch/finally/filter.

---

## 3. Milestone M2 — `cil_reader::flow`, part 2: CFG and abstract stack

### 3.1 Basic blocks

`flow::basic_blocks(&MethodBody) -> MethodCfg`, following `dex_reader::basic_blocks`
and `jvm_reader::compute_basic_blocks_for_method`:

- Leaders: offset 0, every branch/switch target, the fall-through of every
  conditional, every `try` start, every handler start, every filter start.
- Successors: branch targets + fall-through; `leave` → its target *and* the enclosing
  `finally` entry; `endfinally`/`endfilter` → the appropriate continuation.
- Exception edges: every block overlapping a `try` range → that try's handler and
  filter blocks (matching the approximation `jvm-reader` uses).

### 3.2 Abstract stack interpretation

A worklist pass in reverse post-order that turns the operand stack into named values:

- Block entry state is a `Vec<Value>`. Entry block starts empty; handler and filter
  entry blocks start with a single synthetic `Value` of origin `Origin::Exception`.
- Each instruction pops `stack_pop` values (naming its operands) and pushes
  `stack_push` fresh `Stack(id)` values, each recorded with its `Origin`.
- `Varpop`/`Varpush` resolve from the call/`ret` signature.
- At a join where predecessor states disagree on the value at a depth, mint a fresh
  `Stack(id)` with `Origin::Phi([...])` and record it in the block's `entry_phis`.
  The frontend emits one `Assign { dest: phi, sources }` per phi at block entry —
  which is exactly what the shared SSA pass expects to see.
- `leave` empties the stack (ECMA-335 §12.4.2.5).

**Deliberately unlike `jvm-reader`: never fail the method.** On a depth mismatch at a
join, log at `debug`, take the common prefix, mint fresh values for the remainder, and
continue. Obfuscated and hand-written IL is real, and losing a method loses every call
edge through it.

### 3.3 Testing M2

Property tests over every method in every fixture:

- depth never goes negative and never exceeds the header's `max_stack`;
- every `Value::Stack` has exactly one definition;
- every use is dominated by its definition, or is a phi input;
- block successor/predecessor relations are mutually consistent.

---

## 4. Milestone M3 — the frontend, `ctadl-ascent/src/languages/cil/`

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
- Blocks 1:1 with `MethodCfg` blocks; `entry_phis` become leading `Assign`s.
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
| `Assign { dest, sources, .. }` | `StatementKind::assign(var(dest), sources)` |
| `Load { dest, base, field }` | `StatementKind::load(var(dest), var(base), field_path(field))` |
| `Store { base, field, value }` | `StatementKind::store(AccessPath::without_fields(var(base)), field_path(field), value)` |
| `New { dest, ty }` | `assign(var(dest), [Exp::new_object_ref(CallObject::CilObject(ty))])` |
| `Throw { value }` | `assign(throwval, [value])` |

with `Field::Named(f)` → `FieldPath::symbol("<f>")`, `Element` → `"[]"`,
`Length` → `"length"`, `Deref` → `"deref"`. A static field's `base` is
`VariableRef::new_global()`, matching how Dex lowers `SGet`/`SPut`.

Variables: `Value::Arg(n)` → `VariableRef::new_parameter(n)`; `Value::Local(n)` →
local `loc{n}`; `Value::Stack(id)` → local `s{id}`.

**No aliasing heuristics.** Because `Origin` travels with every value, the frontend
answers "what object does this `stfld` write to" by reading
`Origin::Copy(Value::Arg(0))` off the receiver, rather than scanning the block
backwards for an `ldarg` — the ~200 lines that `ctadl-jvm/src/lib.rs` spends on that
question do not get written here.

### 4.5 Calls

```rust
CallKind::Static | CallKind::Instance | CallKind::Ctor
    => CallStyle::DirectCall { call_edges: CallEdges::Explicit([target]) }
CallKind::Virtual { constrained }
    => CallStyle::CilCall { receiver, cls: constrained.unwrap_or(declaring_type),
                            name, descriptor }
CallKind::Indirect { callee } | CallKind::DelegateInvoke
    => CallStyle::FuncPtrCall { callee, signature }
```

`CallStyle::CilCall` is what codegen turns into either an exact `call` edge (one CHA
resolvent), or a `callee_info` fact keyed on `CallDispatchKey::Cil(name, descriptor)`
for hybrid inlining — the identical treatment `CallStyle::JavaCall` gets today.

Every callee that is never defined in the imported assemblies gets a stub function
plus a VMT row at `finish`, mirroring `Context::ext` in the Dex and JVM frontends.
That is what makes an external callee nameable by a model.

### 4.6 CIL-specific lowering the Java frontends have no analogue for

These are not polish; they are where most real .NET taint lives.

- **Managed pointers / `out` and `ref` parameters.** `ldloca`/`ldarga`/`ldflda`/
  `ldelema` push an address (`Origin::AddressOf(place)`); `stind.*`/`stobj` write
  through it; `ldind.*`/`ldobj` read through it. Lowered as `Store`/`Load` with the
  `deref` field, the convention the C frontend already uses. Without this,
  `int.TryParse(s, out n)` — and every `TryGetValue`, every `ref` accumulator — loses
  its flow.
- **`newobj`** is allocation + constructor in one instruction: `New` then a `Ctor`
  call with the fresh object as receiver. This is strictly simpler than the JVM's
  `new` / `dup` / `invokespecial` sequence, and the `init_survivor_link` hack in
  `ctadl-jvm/src/lib.rs` has no counterpart here.
- **Delegates.** `ldftn`/`ldvirtftn` push `Origin::FunctionPtr(method)`; the
  `newobj D::.ctor(object, native int)` that consumes it makes the delegate object
  carry that function pointer; `callvirt D::Invoke` is a `DelegateInvoke`. Map to
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

`languages/cil::import_assembly(path)` for a single `.dll`/`.exe`, and
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
| `ctadl-ir/src/mir/call.rs` | `VirtualMethodTable::Cil { methods, hierarchy, pinvokes }`, `CallStyle::CilCall`, `CallObject::CilObject`, `CilClass`/`CilMethod`/`CilSignature` newtypes, `new_cil()`, `Display` arms |
| `ctadl-ir/src/mir/visit/mod.rs` | visitor arm for `CilCall` |
| `ctadl-ascent/src/facts.rs` | `CallTargetObject::CilClass`, `CallDispatchKey::Cil(name, desc)` |
| `ctadl-ascent/src/facts/parquet.rs` | two tag bytes + encode/decode arms |
| `ctadl-import/src/project.rs` | `ArtifactLanguage::Cil`, `ALL`, `name()`; bump `INDEX_FORMAT_VERSION` |
| `ctadl-ascent/src/cli/mod.rs` | import dispatch arm |
| `ctadl-ascent/src/codegen/mod.rs` | `ChaLanguage::Cil` (reuses `run_cha` unchanged), `CilCall` codegen arm mirroring `JavaCall`, `emit_callee_resolvents` arm, `InstantiationFinder` arm |
| `ctadl-ascent/src/models/{mod,match_index,json,codegen}.rs` | VMT arms; `CIL_DEFAULT_MODELS` |
| `ctadl-ascent/src/languages/mod.rs` | `pub mod cil;` |
| `ctadl-ascent/src/languages/jni.rs` | `SlotModel::for_language` arm (CIL is not a JNI language) |

`run_cha` itself needs no change: it is already language-neutral, and the `Cil` arm
feeds it `method_implemented` + `direct_superclass` exactly as the Java and Lua arms
do.

---

## 6. Milestone M5 — regression harness

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
| Obfuscated / non-verifiable IL breaks the stack simulation | The graceful-degradation rule in §3.2 is exactly this; log and continue instead of dropping the method |
| Self-contained publishes drop ~200 assemblies | The assembly-selection policy in §4.7, plus `skip-analysis` models |
| Generic instantiation explosion in CHA | Erasure to the open definition (§4.1) |
| Reusing the Java VMT for speed, then flipping | Invalidates indexes and model files; decided against in §5 |

---

## 10. Sequencing and effort

| M | Work | Est. | Depends on |
| --- | --- | --- | --- |
| M0 | Workspace integration | 0.5 d | — |
| M1 | `flow`: per-instruction effects, operand resolution, reader helpers | 3–4 d | M0 |
| M2 | `flow`: CFG + abstract stack + phis | 4–5 d | M1 |
| M3 | `languages/cil` frontend | 5–7 d | M2 |
| M4 | Engine plumbing (`Cil` VMT / CallStyle / dispatch key) | 1–2 d | — (parallel with M1–M3) |
| M5 | xtask regression harness | 2–3 d | M3, M4 |
| M5b | Portable-PDB sequence points → source lines | 2–3 d | M5 |
| M6 | `cil-index.jsonl` default models | 2–3 d | M5 |
| M7 | P/Invoke bridge, Xamarin | optional | M6 |

Roughly **3–4 weeks** to a frontend at parity with the JVM one, plus the optional
bridge work.

## 11. Acceptance

1. `cargo test -p cil-reader` green from the repo root with no .NET SDK, including
   the new `flow` unit and property tests.
2. `cargo xtask regression --frontend cil` green, or cleanly skipped without the SDK.
3. `ctadl go app app.dll query.json` reports a planted source→sink path, with SARIF
   regions pointing at `.cs` lines (after M5b).
4. A virtual call with more than one CHA resolvent produces `callee_info` facts and
   is resolved by hybrid inlining — verifiable in `ctadl index`'s
   `hybrid inlining: ... resolvent: N` line, the same way the Java frontends are.
5. `languages/cil/mod.rs` contains **no** backwards-scanning heuristics to recover
   the object base of a field access. If it does, M1's `Origin` design failed and
   should be fixed there rather than worked around here.
