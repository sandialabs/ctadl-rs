# Design: `cil_reader::flow` — per-instruction dataflow for CIL - DO-NOT-MERGE

Status: implemented (milestones M1 and M2 of
[`cil-frontend-plan.md`](cil-frontend-plan.md)), in
`readers/cil-reader/crates/cil-reader/src/flow/`. Where the code departs from
the first version of this design, the text below says what it does and §10
lists why. What `dex-reader` and
`jvm-reader` get wrong, and so what this design avoids, is in
`docs/dex-jvm-reader-improvements.md` on the `dex-jvm-reader-fixes` branch. This
document specifies the API: the types, what every CIL opcode produces, the
invariants, and how to build and test it. How the CTADL frontend turns this
into CTADL IR is a separate design,
[`cil-frontend-translation.md`](cil-frontend-translation.md).

This layer is an API for getting dataflow information out of CIL. It does not
generate IR and it is not an analysis. Its job is to say, for each instruction,
what it reads, what it writes, and how values move between them, in a
vocabulary small enough that a client can use it without decoding CIL itself.
The CIL-specific pattern matching (operand order, stack shape, prefixes,
multi-dimensional array methods, `newobj`, managed pointers) is done here once,
so no client has to do it. Policy belongs to the client: which flows matter,
whether an index taints an element, whether a branch condition is implicit
flow, or whether `initobj` kills anything. The layer reports facts and makes
none of those choices.

---

## 1. Goals and non-goals

**Goals**

1. For every CIL instruction, say **what it reads, what it writes, and how
   values move between them**: a plain copy, a computed value, or a move
   through memory (a field, an array element, a managed pointer, a static).
2. Describe each instruction's flow with a **small, closed set of effects**
   (assign, load, store, call, return, and a few more). A client matches on
   effects, not on instructions: it should not need a `match` on `Code`.
3. **Report facts, not policy.** Every operand an instruction reads appears in
   some effect, including array indices and branch conditions. The layer never
   drops an input because a particular analysis would ignore it. Deciding which
   reads matter is the client's job.
4. **Every input has a role.** The API names the receiver, the index, the stored
   value and the address. It does not hand back an unlabelled list of sources the
   way `dex_reader::DataFlow` does.
5. Keep it **generic**: no CTADL types and no CTADL naming in `cil-reader`.
   The same output should also serve a dumper, a test oracle, or another analysis.
6. Make it **checkable**: every output type derives `Debug, Clone, PartialEq`, and
   pop and push counts are checked against the opcode table generated from Cecil.
7. **Degrade instead of failing.** An operand that does not resolve becomes an
   `Opaque` effect that lists every input and every output, so a client can
   relate them conservatively. The method is never dropped.

**Non-goals**

- Points-to analysis. The layer only resolves managed pointers that can be traced
  back syntactically (§6.3). Everything else stays an access through the
  pointer (`Field::Deref`), and what that means is left to the client.
- Choosing which flows an analysis follows. Implicit (control-dependence) flow,
  index-to-element flow, sanitizers, and strong updates are all analysis
  policy. The layer reports the inputs involved (branch conditions as `Test`
  effects, indices inside `Element` paths) and leaves the choice to the
  client.
- Resolving calls. A call is reported with its target token and signature.
  Dispatch belongs to CHA and the models.
- **Value numbering, SSA, and copy cleanup.** Variables are named by
  location, not by definition (§2), and every copy CIL makes is reported.
  Clients that want versions or fewer copies compute them (§6.5).
- **Generating IR.** That is a client's job.

---

## 2. Shape: two layers, one effect vocabulary

```
MethodBody ──► Layer 1: flow::InsFlow             per instruction, stateless,
               (Effect<Slot>: "2nd popped operand") no stack simulation
                     │
                     ▼  bind (CFG + stack heights)
               Layer 2: flow::MethodFlow           per method; In/Out replaced by
               (Effect<Var>: "stack depth 3")       depth-named mutable variables
```

The main change from an earlier sketch of this layer: **the per-instruction
semantics do not depend on stack simulation.** In that sketch, `InstructionFlow`
held `Value::Stack(id)`, a stack value named by its definition, so you could not
describe one instruction without running a worklist over the whole method. Here, one type `Effect<S>` is generic over
its slot type:

- `Effect<Slot>` (Layer 1) names positions: `In(1)` is the second operand popped,
  `Out(0)` is the first result pushed, plus `Arg(n)` and `Local(n)`. It is a pure
  function of `(instruction, resolved operand signature)`. You can test it opcode
  by opcode, with no method body or CFG.
- `Effect<Var>` (Layer 2) is the same effect after `In`/`Out` are replaced by
  **stack-depth variables**: an instruction at entry height `h` that pops `p`
  binds `In(i)` to `Stack(h - p + i)` and `Out(j)` to `Stack(h - p + j)`. Most
  clients consume this one.

Binding is `effect.map(|slot| bind(slot))`. There is no second effect type to
keep in sync.

**Why depth names and not values.** A `Stack(d)` variable is reassigned every
time something is pushed at depth `d`, just as a Dex register or a CIL local is
reassigned. Bound code is then ordinary register-machine code: mutable
variables, no phis. Naming values by definition would mean placing phis at
every join where two pushes meet, which is SSA construction, and a client that
wants SSA will build it with its own conventions anyway. So the reader does not
mint definition-unique values, place phis, or track origins. The earlier
sketch's `Origin` enum and `entry_phis` are dropped for that reason. "What
object does this `stfld` write to" is a reaching-definitions question over the
bound code (`s0 := p0` then `s0.f := v`), which any client can ask.

What Layer 2 *does* have to compute is what a client cannot easily recover from
the bound code:

- the CFG, including exception edges;
- each block's entry stack **height**, which is what depth naming needs;
- each depth's verification **kind** (`StackKind`), which decides whether an
  `ldfld` base is an object or a pointer to a struct;
- managed-pointer targets (§6.3): once bound, an address is just a value in a
  variable, and tracing it needs the stack-level view.

---

## 3. Layer 1 types

Module tree `cil-reader/src/flow/`: `mod.rs` (types), `opcode.rs` (the
per-opcode table, §5), `call.rs` (call-site construction), `prefix.rs` (prefix
folding and `MethodBody::flows`), `edges.rs` (the flat edge view, §4.3),
`operands.rs` (operand resolution and the names of §3.4), `delegate.rs`
(§3.5), `method.rs` (Layer 2) and `check.rs` (the §7 invariants, shared by the
unit tests and the corpus check). The crate stays dependency-free, so there is
no `smallvec` (§8.3).

### 3.1 Slots

```rust
/// A position an instruction reads or writes, before stack simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// The i-th operand this instruction pops, in ECMA operand order: In(0) is
    /// the *deepest* (first pushed). For `stfld`, In(0) = object, In(1) = value.
    In(u8),
    /// The i-th value this instruction pushes (only `dup` pushes two).
    Out(u8),
    /// Argument slot n (0 is `this` on instance methods), as ldarg/starg number it.
    Arg(u16),
    /// Local variable slot n.
    Local(u16),
    /// A temporary that lives only inside this instruction (cpobj, cpblk).
    Temp(u8),
    /// The exception this instruction raises if it completes abruptly: the
    /// second result of a call, or the operand of throw. In Layer 2 it binds to
    /// `Var::Thrown`.
    Thrown,
    /// The exception the enclosing catch/filter handler caught (`rethrow`).
    Caught,
}
```

### 3.2 Access paths

```rust
/// One step of a memory access.
#[derive(Debug, Clone, PartialEq)]
pub enum Field<S> {
    /// An instance or static field (`ldfld`, `stsfld`, ...).
    Named(FieldOp),
    /// An array element: `ldelem*`, `stelem*`, `ldelema`, and the runtime
    /// `Get`/`Set`/`Address` methods of multi-dimensional arrays. `indices`
    /// holds one slot per dimension, in order. It is empty only when the
    /// element is reached through a pointer that `ldelema` produced earlier.
    Element { indices: Vec<S> },
    /// Array length (`ldlen`).
    Length,
    /// Through a managed or unmanaged pointer (`ldind*`, `stind*`, `ldobj`,
    /// `stobj`, `cpobj`). The CTADL frontend lowers it to
    /// `ctadl_ir::mir::DEREF_FIELD` (translation §6.3).
    Deref,
    /// Through a pointer, a block of `size` bytes (`cpblk`, `initblk`). A
    /// client that does not care about the extent reads it as `Deref`.
    Block { size: S },
}

/// The root of a memory access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Root<S> {
    Slot(S),
    /// The static-field base: one root shared by every static field.
    Static,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Path<S> {
    pub root: Root<S>,
    pub field: Field<S>,
}

/// What an address-producing instruction points at (ldloca, ldarga, ldflda,
/// ldsflda, ldelema, Address on a multi-dimensional array).
#[derive(Debug, Clone, PartialEq)]
pub enum Place<S> {
    Local(u16),
    Arg(u16),
    Field(S, FieldOp),
    Static(FieldOp),
    /// Array and indices, as in `Field::Element`.
    Element(S, Vec<S>),
}
```

### 3.3 Effects

```rust
/// How a value reaches its destination. In every kind the destination depends
/// on the sources. The kinds differ in what else carries over: object
/// *identity* (and therefore aliasing and field structure) or only contents.
#[derive(Debug, Clone, PartialEq)]
pub enum Transfer {
    /// Same value: ldloc, stloc, dup, starg, ckfinite. Identity is preserved.
    Copy,
    /// Same object, viewed as another type: castclass, isinst, unbox.any of a
    /// reference type. Identity is preserved.
    Cast(TypeOp),
    /// A value type boxed into a fresh heap object. Contents are preserved. The
    /// client may also treat it as an allocation of `TypeOp`.
    Box(TypeOp),
    /// A boxed object unwrapped (unbox.any to a value type, unbox). Contents
    /// are preserved.
    Unbox(TypeOp),
    /// A new value computed from the inputs: arithmetic, conversions,
    /// comparisons, mkrefany, refanytype. The result depends on the inputs but
    /// is not the same object and has none of their fields. `op` lets a client
    /// treat some operations specially, for example a comparison as a
    /// sanitizer.
    Derived(Op),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect<S> {
    /// dst := srcs   (several srcs only for Derived binary operations)
    Assign { dst: S, srcs: Vec<S>, transfer: Transfer },
    /// dst := a constant. No inputs, but it is a definition.
    Const { dst: S, value: Const },
    /// dst := root.field
    Load { dst: S, src: Path<S> },
    /// root.field := src
    Store { dst: Path<S>, src: S },
    /// dst := &place
    AddressOf { dst: S, place: Place<S> },
    /// dst := a fresh object, array, or stack block. `sizes` holds the
    /// operands that size it: one for newarr and localloc, one per `.ctor`
    /// argument of a multi-dimensional array, none otherwise.
    New { dst: S, kind: NewKind, ty: Option<TypeOp>, sizes: Vec<S> },
    /// dst := a function pointer to `method`. `receiver` is set for ldvirtftn,
    /// whose target depends on the receiver's runtime type.
    FunctionPtr { dst: S, method: MethodOp, receiver: Option<S> },
    /// The place at `addr` is reset to its default value (initobj). Whether
    /// that kills anything (a strong update) is the client's choice.
    Clear { addr: S },
    Call(CallSite<S>),
    /// Normal method exit with an optional value (ret).
    Return { src: Option<S> },
    /// Abrupt exit: Thrown := src (throw, rethrow with src = Caught).
    Throw { src: S },
    /// `jmp`: exit by transferring every argument slot to `method`.
    Jump { method: MethodOp },
    /// The inputs decide control flow: brtrue, beq, switch, endfilter.
    /// Reported so a client can model implicit flow. Ignoring it is also a
    /// valid choice.
    Test { srcs: Vec<S> },
    /// Fallback when an operand does not resolve or an opcode is undefined.
    /// The reader does not know the semantics, so it lists every input and
    /// every output. Relating all of them (Transfer::Derived) is the
    /// conservative reading. Logged.
    Opaque { srcs: Vec<S>, dsts: Vec<S>, reason: OpaqueReason },
}

pub enum NewKind { Object, ValueType, Array, StackAlloc /* localloc */ }

pub enum Const {
    I4(i32), I8(i64), R4(f32), R8(f64), Null,
    String(MetadataToken),   // ldstr; text via MethodBody::user_string
    Token(MetadataToken),    // ldtoken
    SizeOf(TypeOp),          // sizeof
    ArgList,                 // arglist (varargs handle; see §8.4)
}

pub enum Op {
    Add, Sub, Mul, Div, Rem, And, Or, Xor, Shl, Shr, Neg, Not,
    Conv(ConvTarget),
    Cmp(CmpOp),              // ceq/cgt/clt and their _un forms
    MkRefAny(TypeOp), RefAnyType, RefAnyVal(TypeOp),
    Opaque,                  // the unknown operation of an Opaque effect's edges
}
```

(`Op` keeps the `.ovf` and `.un` distinctions as `OpFlags`, on `Add`, `Sub`,
`Mul`, `Div`, `Rem`, `Shr`, `Cmp` and `Conv`. They are omitted above for
brevity.)

Every type has `fn map<T>(self, f: impl FnMut(S) -> T) -> Effect<T>`, which is how
Layer 2 binds slots.

**Effect order.** An instruction's effects run in order, and Layer 1 assumes `In`
and `Out` are *distinct* slots. Under depth naming they are not: `Out(0)` and
`In(0)` share a depth. For most opcodes that does not matter, because the single
effect reads every input before writing its output. It does matter when an
instruction writes an output and a *later* effect still reads an input at the
same depth. `newobj` is the case: `New o0` would overwrite the first ctor
argument before the `Call` reads it. Layer 2 handles this (§4.2), so Layer 1 can
keep its plain positional semantics.

### 3.4 Operand handles

These are lightweight and lazily named. Layer 1 resolves only what it needs to
know the instruction's shape. Names are derived on demand, through the existing
`names` module with its Cecil-parity spellings. These are for display and
debugging. A client that needs other spellings builds them from the handles.

```rust
pub struct FieldOp  { pub token: MetadataToken, pub is_static: bool, pub ty: TypeSig }
pub struct TypeOp   { pub token: MetadataToken }
pub struct MethodOp {
    pub token: MetadataToken,        // MethodDef / MemberRef / MethodSpec
    pub sig: MethodSig,              // arity, has_this, return type, byref params
    pub generic_args: Vec<TypeSig>,  // MethodSpec instantiation, if any
}

impl MethodBody<'a> {
    pub fn field_name(&self, f: &FieldOp) -> Result<String>;
    pub fn type_name(&self, t: &TypeOp) -> Result<String>;
    pub fn method_name(&self, m: &MethodOp) -> Result<String>;
    pub fn declaring_type(&self, m: &MethodOp) -> Result<TypeOp>;
    pub fn user_string(&self, tok: MetadataToken) -> Result<String>;
}
```

`is_static` on `FieldOp` comes from the opcode (`ldsfld` vs `ldfld`), not from
resolving the field. A `MemberRef` into an assembly that is not loaded still
produces a complete effect.

### 3.5 Calls

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct CallSite<S> {
    pub kind: CallKind,
    /// Callee. `None` only for calli, which has a call-site signature instead.
    pub method: Option<MethodOp>,
    pub sig: MethodSig,
    /// Instance calls: In(0). newobj: Out(0), the freshly allocated object.
    pub receiver: Option<S>,
    /// Declared parameters in order, receiver excluded (varargs extras included).
    pub args: Vec<S>,
    /// Non-void result. Never set for newobj, whose "result" is the receiver.
    pub result: Option<S>,
    /// Exceptional result. Always Slot::Thrown in Layer 1.
    pub thrown: S,
    /// Indices into `args` of `ByRef` (`ref`/`out`/`in`) parameters. These are
    /// *outputs* of the call too, which matters for TryParse and TryGetValue.
    pub byref_args: Vec<u16>,
    pub prefixes: Prefixes,
}

pub enum CallKind {
    /// `call` of a static method.
    Static,
    /// `call` of an instance method: non-virtual, exact target (base.M(), struct methods).
    Instance,
    /// `callvirt`. `constrained` holds the `constrained.` prefix's type.
    Virtual { constrained: Option<TypeOp> },
    /// `newobj`. It comes right after the `New` effect for Out(0).
    Ctor,
    /// `calli`. The function pointer is the last popped operand.
    Indirect { callee: S },
}
```

Delegate construction and invocation are **not** separate `CallKind`s in Layer 1.
Recognizing them needs the declaring type's base class, and that type can live
in another assembly. Instead, `flow::delegate::classify(&CallSite, &dyn
TypeOracle) -> Option<DelegateOp>` gives `Ctor { target: S, fnptr: S }` or
`Invoke`. `TypeOracle` is a small trait: `declaring_type(&MethodOp)`,
`method_name(&MethodOp)` and `is_delegate(&TypeOp) -> Option<bool>` (derives
from `System.MulticastDelegate`). It answers `is_delegate` rather than
`base_type`, because a `TypeOp` is a token that means something only in its own
module, so a base-type walk across assemblies has to happen inside the oracle.
`delegate::ModuleTypes` is an oracle over one module and, given an
`AssemblyCache`, over every assembly the cache loads (through
`Module::resolve_type`); `MethodBody` is an oracle over its own module. When the
oracle cannot tell, a structural fallback applies: a ctor with signature
`(object, native int)` counts, and `ModuleTypes` takes a type whose base does not
resolve but whose ctor has that shape for a delegate, so its `Invoke` and
`BeginInvoke` count too.

### 3.6 The per-instruction record

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct InsFlow {
    pub offset: u32,
    pub size: u32,               // including folded prefixes
    pub opcode: OpCode,          // the instruction itself, never a prefix
    pub prefixes: Prefixes,      // volatile/unaligned(n)/tail/readonly/no(n)/constrained(T)
    /// One role per popped operand: inputs[i] describes In(i).
    pub inputs: Vec<Role>,
    pub outputs: u8,             // number of Out slots
    pub effects: Vec<Effect<Slot>>,
    pub control: Control,
}

/// What an input operand *is*. This replaces dex-reader's unlabelled bags.
pub enum Role {
    Value, Object, Receiver, Arg(u16), Index, Address, SrcAddress,
    Size, Lhs, Rhs, Condition, Callee, Exception, ReturnValue, Discarded,
}

pub enum Control {
    Next,
    Goto(u32),
    Cond { taken: Vec<u32> /* plus fall-through */ },
    Switch(Vec<u32>),
    Leave(u32),       // empties the stack; runs enclosed finally blocks
    EndFinally, EndFilter,
    Return, Throw, Jump,
}

impl<'a> MethodBody<'a> {
    /// Layer 1 over the whole body, with prefixes folded into the next
    /// instruction. Cheap: no CFG, no stack simulation.
    pub fn flows(&self) -> impl Iterator<Item = Result<InsFlow>> + '_;
}

/// Layer 1 for one instruction, given its signature context. Public so that
/// tests and other tools can call it without a body.
pub fn instruction_flow(ins: &Ins<'_>, prefixes: Prefixes, cx: &FlowCx<'_>) -> Result<InsFlow>;

/// What Layer 1 needs beyond the instruction: operand resolution and the
/// enclosing method's signature (for `ret`).
pub struct FlowCx<'a> { pub operands: &'a dyn Operands, pub method_sig: &'a MethodSig }

/// Resolves field, method and call-site tokens, and says whether a type is
/// known to be a value type. `Module` and `MethodBody` implement it; a test
/// can stub it.
pub trait Operands { /* field, method, call_site, is_value_type */ }
```

Two operand facts need knowledge of a type. `newobj` gives `NewKind::ValueType`
when the ctor's declaring type is known to be a value type in this module (a
TypeDef, a `valuetype` generic instance, or a primitive TypeRef), and
`NewKind::Object` otherwise. `unbox.any T` gives `Cast(T)` when `T` is known to
be a reference type, and `Unbox(T)` otherwise (value types and generic
parameters).

Prefixes at the very end of a body have no instruction to fold into. They get
a record of their own: the last prefix as `opcode`, no inputs, no effects,
`Control::Next`. That is the only record whose `opcode` is a prefix.

---

## 4. Layer 2 types (binding)

### 4.1 Variables

```rust
/// A location-named variable. All of these are ordinary mutable variables that
/// may be assigned many times (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Var {
    Arg(u16),
    Local(u16),
    /// The stack slot at this depth (0 = bottom).
    Stack(u16),
    /// An instruction-local temporary. Numbered per instruction and reused
    /// across instructions.
    Temp(u8),
    /// The exception caught by handler `h`, set on entry to the handler and
    /// read by `rethrow`. A separate variable because depth 0 can be
    /// overwritten before a `rethrow`.
    Caught(u16),
    /// The exceptional result of the current instruction.
    Thrown,
}

/// ECMA-335 §III.1.1 verification type of a stack slot. Tracking it is cheap,
/// and it is what decides whether `ldfld`'s base is an object or a managed
/// pointer to a struct.
pub enum StackKind { Int32, Int64, NativeInt, Float, ObjRef, ManagedPtr, ValueType(TypeOp), Unknown }
```

The variable naming carries no information about definitions. Two unrelated
values pushed at depth 2 are both `Stack(2)`, and a client that needs to tell
them apart uses reaching definitions or SSA.

### 4.2 The method view

```rust
pub struct MethodFlow {
    pub blocks: Vec<Block>,
    pub handlers: Vec<Handler>,          // try range, handler/filter entry block, kind
    pub warnings: Vec<FlowWarning>,      // height mismatches, stray prefixes, Opaque effects
}

pub struct Block {
    pub start: u32, pub end: u32,
    /// Stack height on entry, and the kind at each depth below it.
    pub entry_height: u16,
    pub entry_kinds: Vec<StackKind>,
    /// For a catch handler, filter, or filtered handler entry:
    /// `Stack(0) := Caught(h)`, prepended by the binder so that a client
    /// has nothing to special-case. Finally and fault entries get nothing:
    /// they start with an empty stack.
    pub instrs: Vec<BoundIns>,
    pub exit_height: u16,                // 0 after leave, endfinally, endfilter
    pub succs: Vec<BlockId>,
    pub preds: Vec<BlockId>,             // normal predecessors
    pub exc_succs: Vec<BlockId>,         // handler entries reachable from here
    pub handler_entry: Option<u16>,      // set on a handler's or filter's entry
}

pub struct BoundIns {
    pub flow: InsFlow,                   // Layer 1 record, unchanged, for reference
    pub effects: Vec<Effect<Var>>,       // the same effects with slots bound
    pub inputs: Vec<Var>,                // the variable bound to each In(i)
    pub outputs: Vec<Var>,               // the variable bound to each Out(i)
    pub input_kinds: Vec<StackKind>,     // the kind of each In(i)
    pub input_places: Vec<Option<Place<Var>>>, // resolve_address of each In(i)
    pub height: u16,                     // stack height before the instruction
}

/// An instruction: its block and its index in the block's `instrs`.
pub struct InsId { pub block: BlockId, pub index: usize }

impl MethodFlow {
    pub fn build(body: &MethodBody<'_>) -> MethodFlow;   // never fails (§8.1)
    /// §6.3: what the pointer held in input `i` of instruction `at` points at,
    /// when that can be traced (see §6.3 for the rule).
    pub fn resolve_address(&self, at: InsId, i: u8) -> Option<&Place<Var>>;
    /// Locals and args whose address is taken anywhere (ldloca, ldarga).
    pub fn address_taken(&self) -> &BitSet;
    /// Whether the address that the AddressOf at `at` produces reaches a use
    /// where `resolve_address` returns None (a merge of two addresses), so a
    /// client knows that pointer may refer to more than one place.
    pub fn address_escapes_resolution(&self, at: InsId) -> bool;
    /// §4.3, for the whole method.
    pub fn edges(&self) -> impl Iterator<Item = Edge<Var>> + '_;
}
```

**The output/input collision rule.** Binding `Out(j)` to the same depth as an
`In(i)` that a later effect in the same instruction still reads would make the
earlier effect overwrite the input. When that happens, the binder sends `Out(j)`
through a `Temp` and appends `Stack(d) := Temp` as the instruction's last
effect. For `newobj C::.ctor(a, b)` at entry height 2:

```text
New    t0 := new C
Call   Ctor recv=t0 args=[s0, s1]
Assign s0 := [t0] Copy
```

`t0` is a single-use copy, which any copy coalescing removes. The rule is
mechanical, applies to any instruction, and is tested by invariant 7.8.

**Block splitting.** `MethodFlow::build_with(body, FlowOptions {
split_after_throwing_in_try: true })` ends a block after every `Call` or `Throw`
inside a protected range. Each such point then gets its own exception edge, and
a client that tracks state per block sees, at the handler, the exception and
locals as of that point, not as of the block's end.

**The `rethrow` lookup.** `rethrow` binds `Caught` to `Caught(h)` for the
innermost catch or filter handler that encloses it.

**`leave` and `finally`.** A `leave` has edges to its target and to the entry
of every finally it exits. The `endfinally` of each exited finally has an edge
to the next finally out, and the last one to the target. That is a superset of
the paths, so a value defined in a finally reaches whatever follows it.

**Stack kinds.** Each output's kind comes from the effect that writes it: the
declared type of an argument, local, field or call result; the input's kind
for a copy; ECMA-335 §III.1.5 for arithmetic; `ManagedPtr` for an address. An
enum value is `Int32`. A disagreement at a join becomes `Unknown`.

**Managed pointers.** `resolve_address` is a must-analysis over the bound code:
an `AddressOf` sets a fact, copies move it, any other write kills it and every
fact whose place mentions the written variable, and a join keeps only the
facts every predecessor agrees on. A stack base is traced back to the argument
or local it copies, so `ldarg.0; ldflda f; ldfld g` resolves to `arg0.f`, which
`Field(Stack(0), f)` could not be, since the address overwrites its own base.
`address_escapes_resolution` is the matching may-analysis: an `AddressOf`
escapes when its address may reach an input that the must-analysis cannot
resolve.

### 4.3 The flat edge view

Some clients only need "operand A flows to output B" (a quick prototype, a
debugging dump, or a test). For them, both layers expose a derived view:

```rust
pub struct Edge<S> { pub from: Loc<S>, pub to: Loc<S>, pub transfer: Transfer }
pub enum Loc<S> { Var(S), Path(Path<S>), Place(Place<S>) }

impl InsFlow { pub fn edges(&self) -> impl Iterator<Item = Edge<Slot>> + '_; }
```

Derivation: `Assign`/`Load`/`Store` give one edge per source. `AddressOf` gives
`Place -> dst` with `Transfer::Copy`; the address *aliases* the place. `New`
gives `size -> dst.length` for each size: `Copy` for a single one,
`Derived(Mul)` for the dimensions of a multi-dimensional array. `Opaque` gives
the full cross product with `Derived(Op::Opaque)`. `Call`, `Const`, `Test` and `Clear` give no intra-instruction
edges, because calls are inter-procedural and the models decide them. Element
indices give no edges either. Unlike the effects, this view applies a policy (the
same one Dex and JVM use), so it is a convenience, not the specification. It is
computed from the effects and is never stored separately.

---

## 5. Per-opcode semantics

This is the full table, written as Layer 1 effects. Notation: `iN` = `In(N)` in
ECMA operand order (i0 is deepest), `oN` = `Out(N)`, `T` = `Temp(0)`,
`·.f` = `Path{.., Named(f)}`, `·[i..]` = `Element { indices: [i..] }`, `·*` = `Deref`, `G` = `Root::Static`.
Macro forms (`ldarg.0`, `ldc.i4.3`, `br.s`, ...) behave exactly like their long
forms and are listed once. "Roles" lists `inputs[i]`.

### 5.1 Locals, arguments, constants, stack

| Opcodes | Roles | Effects |
| --- | --- | --- |
| `nop` `break` | — | — |
| `ldarg n` | — | `Assign o0 := [Arg n] Copy` |
| `ldloc n` | — | `Assign o0 := [Local n] Copy` |
| `starg n` | Value | `Assign Arg n := [i0] Copy` |
| `stloc n` | Value | `Assign Local n := [i0] Copy` |
| `ldarga n` | — | `AddressOf o0 = Place::Arg n` |
| `ldloca n` | — | `AddressOf o0 = Place::Local n` |
| `ldnull` | — | `Const o0 = Null` |
| `ldc.i4*` `ldc.i8` `ldc.r4` `ldc.r8` | — | `Const o0 = I4/I8/R4/R8` |
| `ldstr` | — | `Const o0 = String(tok)` |
| `ldtoken` | — | `Const o0 = Token(tok)` |
| `sizeof` | — | `Const o0 = SizeOf(T)` |
| `arglist` | — | `Const o0 = ArgList` (§8.4) |
| `dup` | Value | `Assign o0 := [i0] Copy; Assign o1 := [i0] Copy` |
| `pop` | Discarded | — |

### 5.2 Arithmetic, logic, conversion, comparison

| Opcodes | Roles | Effects |
| --- | --- | --- |
| `add[.ovf[.un]]` `sub*` `mul*` `div[.un]` `rem[.un]` `and` `or` `xor` `shl` `shr[.un]` | Lhs, Rhs | `Assign o0 := [i0, i1] Derived(op)` |
| `neg` `not` | Value | `Assign o0 := [i0] Derived(op)` |
| `conv.*` `conv.ovf.*[.un]` `conv.r.un` | Value | `Assign o0 := [i0] Derived(Conv(..))` |
| `ckfinite` | Value | `Assign o0 := [i0] Copy` (the value is unchanged or an exception is thrown) |
| `ceq` `cgt[.un]` `clt[.un]` | Lhs, Rhs | `Assign o0 := [i0, i1] Derived(Cmp)` |

### 5.3 Branches and control

| Opcodes | Roles | Effects | Control |
| --- | --- | --- | --- |
| `br` | — | — | `Goto` |
| `brtrue` `brfalse` | Condition | `Test [i0]` | `Cond` |
| `beq` `bne.un` `bge*` `bgt*` `ble*` `blt*` | Lhs, Rhs | `Test [i0, i1]` | `Cond` |
| `switch` | Condition | `Test [i0]` | `Switch` |
| `leave` | — | — | `Leave` (stack emptied in Layer 2) |
| `endfinally` | — | — | `EndFinally` |
| `endfilter` | Condition | `Test [i0]` | `EndFilter` |
| `ret` (void) | — | `Return None` | `Return` |
| `ret` (non-void) | ReturnValue | `Return Some(i0)` | `Return` |
| `throw` | Exception | `Throw i0` | `Throw` |
| `rethrow` | — | `Throw Caught` | `Throw` |
| `jmp` | — | `Jump m` | `Jump` |

### 5.4 Fields

| Opcodes | Roles | Effects |
| --- | --- | --- |
| `ldfld f` | Object | `Load o0 := i0.f` |
| `stfld f` | Object, Value | `Store i0.f := i1` |
| `ldflda f` | Object | `AddressOf o0 = Field(i0, f)` |
| `ldsfld f` | — | `Load o0 := G.f` |
| `stsfld f` | Value | `Store G.f := i0` |
| `ldsflda f` | — | `AddressOf o0 = Static(f)` |

The `Object` input of `ldfld`/`stfld`/`ldflda` can be an object reference, a
managed pointer to a struct, or (for `ldfld` only) a struct value. Layer 1 does
not distinguish them. Layer 2 does, through `StackKind`, and §6.3 lets a
client read `ldloca s; ldfld f` as `loc_s.f` instead of `loc_s.*.f`.

### 5.5 Arrays

| Opcodes | Roles | Effects |
| --- | --- | --- |
| `newarr T` | Size | `New o0 Array(T) sizes=[i0]` |
| `ldlen` | Object | `Load o0 := i0.length` |
| `ldelem.*` `ldelem.ref` `ldelem T` | Object, Index | `Load o0 := i0[i1]` |
| `stelem.*` `stelem.ref` `stelem T` | Object, Index, Value | `Store i0[i1] := i2` |
| `ldelema T` | Object, Index | `AddressOf o0 = Element(i0, [i1])` |
| `call T[,]::Get(i..)` | Receiver, Index… | `Load result := recv[i..]` |
| `call T[,]::Set(i.., v)` | Receiver, Index…, Value | `Store recv[i..] := v` |
| `call T[,]::Address(i..)` | Receiver, Index… | `AddressOf result = Element(recv, [i..])` |
| `newobj T[,]::.ctor(n..)` | Size… | `New o0 Array(T[,]) sizes=[i..]` |

The indices are part of the path, so the effect records exactly what the
instruction reads. Whether a tainted index taints the element read is not
decided here. The client makes that choice. **Multi-dimensional arrays** do not use opcodes: they are `MemberRef`
calls on a `TypeSpec` array type, with runtime-provided methods. Layer 1
recognizes the declaring type `TypeSig::Array` and the names
`Get`/`Set`/`Address`/`.ctor`, and emits element effects instead of a `Call`.
Without this, every `int[,]` access becomes a call to a method that has no body
and no model.

### 5.6 Indirect access and value types

| Opcodes | Roles | Effects |
| --- | --- | --- |
| `ldind.*` `ldind.ref` | Address | `Load o0 := i0.*` |
| `stind.*` `stind.ref` | Address, Value | `Store i0.* := i1` |
| `ldobj T` | Address | `Load o0 := i0.*` |
| `stobj T` | Address, Value | `Store i0.* := i1` |
| `cpobj T` | Address, SrcAddress | `Load T := i1.*; Store i0.* := T` |
| `initobj T` | Address | `Clear i0` |
| `cpblk` | Address, SrcAddress, Size | `Load T := i1.*[..i2]; Store i0.*[..i2] := T` (`Field::Block`) |
| `initblk` | Address, Value, Size | `Store i0.*[..i2] := i1` (`Field::Block`) |
| `localloc` | Size | `New o0 StackAlloc sizes=[i0]` |

### 5.7 Objects, casts, boxing, typed references

| Opcodes | Roles | Effects |
| --- | --- | --- |
| `newobj ctor` (class) | Arg… | `New o0 Object(T); Call Ctor recv=o0 args=i..` |
| `newobj ctor` (value type) | Arg… | `New o0 ValueType(T); Call Ctor recv=o0 args=i..` |
| `castclass T` `isinst T` | Object | `Assign o0 := [i0] Cast(T)` |
| `box T` | Value | `Assign o0 := [i0] Box(T)` |
| `unbox T` | Object | `Assign o0 := [i0] Unbox(T)` (a pointer into the box, which aliases it) |
| `unbox.any T` | Object | `Assign o0 := [i0] Unbox(T)`, or `Cast(T)` when T is a reference type |
| `mkrefany T` | Address | `Assign o0 := [i0] Derived(MkRefAny)` |
| `refanyval T` | Value | `Assign o0 := [i0] Derived(RefAnyVal)` |
| `refanytype` | Value | `Assign o0 := [i0] Derived(RefAnyType)` |

`box` and `unbox` are *contents-preserving* assigns rather than a `New` plus a
field store. A struct's fields are then reachable through the box with the same
field names, which is what a field-sensitive analysis needs. `Transfer::Box(T)`
still tells a client that an allocation happened, which class-hierarchy
analysis needs.

### 5.8 Calls and function pointers

| Opcodes | Roles | Effects |
| --- | --- | --- |
| `call m` (static) | Arg… | `Call Static args=i0.. result=o0? thrown` |
| `call m` (instance) | Receiver, Arg… | `Call Instance recv=i0 args=i1..` |
| `callvirt m` | Receiver, Arg… | `Call Virtual{constrained} recv=i0 args=i1..` |
| `calli sig` | [Receiver], Arg…, Callee | `Call Indirect{callee=i_last}` |
| `ldftn m` | — | `FunctionPtr o0 = m` |
| `ldvirtftn m` | Object | `FunctionPtr o0 = m receiver=i0` |

The pop count is `has_this + params.len()` (plus 1 for calli). The push count is
`return_type != Void`. Both come from `MethodOp.sig` (for vararg call sites,
from the `MemberRef`'s call-site signature, with the arguments after the
sentinel included).

### 5.9 Prefixes

`volatile.` `unaligned.` `tail.` `readonly.` `no.` `constrained.` produce no
`InsFlow` of their own. `MethodBody::flows` folds them into the next
instruction's `prefixes`, and its `size` covers them. `constrained. T` gives
`CallKind::Virtual { constrained: Some(T) }`, and `tail.` sets
`CallSite::prefixes.tail`. Prefix folding is the only state Layer 1 carries
between instructions. A prefix at the end of the body, or one followed by an
instruction it does not apply to, is recorded on that instruction and logged.
It is never an error.

### 5.10 Undefined opcodes

`decoder::UNDEFINED`, Cecil's `default(OpCode)`, produces
`Opaque { srcs: [], dsts: [], reason: UndefinedOpcode }` with `Control::Goto` to
its bogus `InlineBrTarget`, the same way Cecil reads it.

---

## 6. Design decisions worth calling out

### 6.1 A small, closed effect vocabulary

Most effects (`Assign`, `Const`, `Load`, `Store`, `New`, `FunctionPtr`, `Call`,
`Return`, `Throw`) are the primitive moves any dataflow client already has a
notion of, so a client handles each with one case. `AddressOf`, `Clear`, `Test`
and `Opaque` are CIL facts that clients interpret differently, and §6.3
supplies what that interpretation needs. The CTADL frontend's mapping is
translation §6.

### 6.2 Roles make the dex-reader bug class impossible

`IPutWide`'s object register is listed as a source in one opcode and as a dest
in another. That can only happen when roles are implicit. Here, `stfld` *is*
`Store { dst: Path { root: i0, .. }, src: i1 }`: the object can only be a path
root. §7 checks that every input is consumed exactly as its role says.

### 6.3 Managed pointers: resolve syntactically, fall back to `Deref`

`ref`/`out` and `ldloca` are where CIL dataflow differs most from Java bytecode.
Two cases:

1. **Resolvable**: the address comes from an `AddressOf` through copies only
   (`ldloca 3; ldc 1; stind.i4`, `ldflda f; ldfld g`, `ldloca s; call
   S::M()`). `MethodFlow::resolve_address(at, i)` returns the `Place`, so a
   client can read `Store v.* := x` as a write to `loc3` or `obj.f.g`. Most C#
   compiler output is this case.

   A client cannot easily do this on bound code, so the reader does it. Resolution is a forward pass that runs alongside the height
   pass. Its state is a map from depth (and from address-holding local) to
   `Place`. `AddressOf` sets an entry, copies move it, and any other write clears
   it. At a join, the maps are intersected, keeping only entries on which every
   predecessor agrees. A `Place` whose base is itself a stack slot
   (`Field(Stack(d), f)`) is valid only while `Stack(d)` has not been
   reassigned, and the pass clears it when `d` is. That also covers `?:`
   operators that produce an address, which the C# compiler sometimes emits
   across blocks.
2. **Not resolvable**: a byref parameter, addresses that differ across a join, an address read
   from memory. The access stays `root.*` (`Field::Deref`).

The two meet at **byref call arguments**. `int.TryParse(s, out n)` passes
`&loc_n`. The layer reports which arguments are addresses
(`CallSite::byref_args`), where each one points when it can be traced
(`resolve_address`), and which locals ever have their address taken
(`address_taken()`). That is what a client needs to connect writes the callee
makes through the pointer back to `loc_n`. How the CTADL frontend does it
(copy-in/copy-out through a fresh cell) is in translation §6.3.

### 6.4 Exceptions are values

A call's exceptional result is `Slot::Thrown`, bound to `Var::Thrown`, and
`throw`'s operand is too. Layer 2 adds the exception edge from every block in a
try range to its handler entry. The handler entry starts with
`Stack(0) := Caught(h)`. A client relates `Thrown` at the raising points to
`Caught(h)` along those edges, so a value stored in an exception object reaches
`e.Message` in `catch (Exception e) { sink(e.Message); }` with no special
casing. Implicit
runtime exceptions (a null `ldfld`, an array bounds check) are not reported:
the runtime creates them, so they hold no value from the method.

### 6.5 Output is raw: unversioned, every copy kept

Variables are named by location (§4.1), never versioned. Every copy CIL makes
(`ldloc`, `stloc`, `dup`, the collision temporaries of §4.2) is reported as an
`Assign`. Unreachable blocks (common after `leave` into a `finally`) are kept.
Cleaning any of this up is a client choice, and doing it here would make the
effects harder to check against the opcode table (§7).

---

## 7. Invariants (asserted by tests)

Layer 1, for every instruction in every fixture method:

1. `inputs.len()` equals the pop count and `outputs` equals the push count,
   computed independently from `OpCode::stack_pop`/`stack_push`, with
   `Varpop`/`Varpush` resolved from the signature. Because the table is generated
   from Cecil, this is an external oracle that neither existing reader has.
2. Every `In(i)` is used by some effect, unless `inputs[i]` is `Discarded`.
3. Every `Out(i)` is defined by exactly one effect.
4. Every `Temp` is defined before it is used, within the instruction.
5. An input's role matches its use. The table of §5 fixes which uses each
   role allows: `Value` only as an `Assign`/`Store` source; `Object` as a path
   root, a `Place` base, the source of a `Cast`/`Unbox` (castclass, isinst,
   unbox) or ldvirtftn's receiver; `Address` as a path root, `Clear`'s address
   or mkrefany's source; `SrcAddress` only as a path root; `Receiver` as a
   call's receiver, or a path root or place base (multi-dimensional arrays);
   `Arg(k)` only as argument `k`; `Index` only in element indices; `Size` only
   as a `New` size or a `Block` size; `Lhs`/`Rhs` as a `Derived` source or a
   `Test` source; `Condition` only in `Test`; and `Callee`, `Exception` and
   `ReturnValue` only as calli's callee, the thrown value and the returned
   value. Any input may be an `Opaque` source.
6. The `match` in `opcode.rs` is exhaustive over `Code` with **no `_` arm**, so
   adding an opcode is a compile error.

Layer 2, for every method:

7. Stack depth never goes negative and never exceeds `max_stack` (or is logged
   when the body says otherwise). Every predecessor of a block agrees on its
   entry height (or the mismatch is in `warnings`).
8. **No effect reads a variable that an earlier effect of the same instruction
   wrote, unless Layer 1 meant it to.** Concretely: bind with distinct names and
   bind with depth names, then run both through a small interpreter. The two
   must agree. This is what checks the collision rule in §4.2. (The
   interpreter is symbolic: a copy is the identity, any other effect makes a
   term of what it read, and memory writes, calls, tests and exits are events.
   Both runs must give the same events, outputs and argument/local values.)
9. `succs`/`preds` are consistent, every catch, filter and filtered-handler
   entry has height 1, every finally and fault entry has height 0, and every
   `leave` target has height 0.

Not tested here: anything about what a client builds from this output.

---

## 8. Open points and decisions taken

1. **Bad stack at a join**: take the smaller height, mark the dropped depths'
   kinds `Unknown`, log at `debug`, and continue. Depth names keep this cheap:
   no values have to be invented.
   `MethodFlow::build` is infallible. Diagnostics go to `MethodFlow::warnings`.
2. **Indices and conditions** are reported, not interpreted. Both appear in
   effects (`Element` indices, `Test`) with their roles. Whether they carry
   flow is decided by the client.
3. **Allocation**: the crate is dependency-free, so `Vec` throughout to start.
   Non-call instructions have at most two effects and three inputs. If profiling
   on the corpus shows allocation matters, swap in a hand-rolled fixed-capacity
   array behind the same field types. Measure it first, with output captured
   to a file.
4. **`arglist`/varargs**: rare outside C++/CLI. `Const ArgList` drops the link
   to the extra arguments, so it breaks goal 3. If the corpus shows it
   matters, upgrade it to `Assign o0 := [extra args] Derived`.
5. **`Clear` (initobj)** is reported on every address. A client can treat it
   as a strong update only when `resolve_address` traces the address.
6. **Pinned locals** (`TypeSig::Pinned`) and **unmanaged pointers** (`T*`,
   `conv.u` of a managed pointer) are treated like managed pointers. Pointer
   arithmetic on them yields `Derived`, and `resolve_address` returns `None`.
   A client then sees an ordinary `Deref` access through a derived value.

---

## 9. Plan

Each step lands green with its own tests. Steps 1–5 are Layer 1 and step 6 is
Layer 2.

| # | Step | Tests |
| --- | --- | --- |
| 1 | Reader helpers: `Ins::size`/`end_offset`, `MethodBody::user_string`, `MethodBody::code_file_offset` (for source spans), name lookups for `FieldOp`/`MethodOp`/`TypeOp`, `MethodOp` construction (MethodDef/MemberRef/MethodSpec → `MethodSig`), the current method's sig on `FlowCx` | Unit tests on fixtures |
| 2 | `flow` types + `map` + `edges`, with no opcode logic | Unit tests: `map` keeps structure; edge derivation per effect |
| 3 | `opcode.rs`: the exhaustive table of §5.1–5.7, without calls | One test per family on hand-assembled IL bytes (a tiny test-only encoder), asserting exact `InsFlow` |
| 4 | `call.rs` + `prefix.rs`: §5.8–5.9, multi-dimensional array methods, `byref_args`, `delegate::classify` | Fixture tests: instance/static/virtual/constrained/calli/newobj/vararg/out-param/delegate |
| 5 | Property test of §7.1–7.6 over every method in `$CIL_READER_FIXTURES` and the Cecil corpus, plus `cil-dump --flow` to print Layer 1 per instruction (for review and for goldens under `xtask/cil-goldens`) | The property test is the acceptance gate for Layer 1 |
| 6 | Layer 2: CFG, stack heights and `StackKind`, depth binding with the collision rule, handler `Caught` wiring, `resolve_address`, `address_taken`. No phis and no value numbering (§2) | Properties §7.7–7.9 over the corpus; targeted tests for `out` params, `ldloca`+struct fields, try/catch/filter/finally, `newobj` with arguments, `dup`+`stfld`, an address across a `?:` join, and a join with mismatched heights |

**Where the tests live.** `cil-reader` has no `tests/` directory and no
committed DLLs: the fixtures come from the flake (`$CIL_READER_FIXTURES`), and
every fixture-backed reader test already runs in `xtask regression --frontend
cil` (frontend plan §1). Flow tests follow the same split. Steps 2 and 3, which
need no DLL, are `cargo test -p cil-reader` unit tests. The fixture tests of
steps 4 and 6 and the corpus property test of step 5 are a new `cil:flow` check
in xtask.

New `.il` or C# fixtures for what the Cecil corpus barely exercises: `out`/`ref`
arguments, `constrained.` callvirt on a generic, delegates and lambdas,
multi-dimensional arrays, filters, `cpobj`/`initobj` on structs, and `calli`.

**Acceptance for this layer**: the unit tests green under `cargo test -p
cil-reader`, and `cil:flow` green under `xtask regression --frontend cil`. `cil-dump
--flow` output for the new fixtures is reviewed and blessed. A client can then
be written with no `match` on `Code`: it matches only on `Effect`, `CallKind`
and `Control`.

---

## 10. As implemented

All of §9 is done. `cargo test -p cil-reader` holds the unit tests of steps 2
and 3 (and the collision rule's), and `cil:flow` in `cargo xtask regression
--frontend cil` holds the rest:

- **property**: §7.1–7.5 and coverage of every instruction byte, then §7.7–7.9,
  over every method body of the Cecil corpus and the compiled fixtures
  (185,390 bodies, 4,056,136 instructions, 669,573 blocks). Opaque effects and
  Layer 2 warnings appear only in the fixtures built to be malformed
  (`gap-*`, `branch-out`) and in `flow.il`'s deliberate height mismatch. A
  summary is written to `<work>/cil-flow/summary.txt`.
- **fixtures**: targeted tests on `readers/cil-reader/fixtures/flow.il`, the
  new fixture §9 asks for, one per listed case.
- **goldens**: `cil-dump --flow` of `flow.il` (both layers) against
  `xtask/cil-goldens/flow`, regenerated by `cargo xtask cil bless-flow`.
- **client**: `xtask/src/cil/flow/client.rs`, an argument-to-return taint
  analysis that matches only on `Effect`, `CallKind` and `Control`.

Departures from the first version of this design, each reflected above:

1. `Field::Block { size }` and `New::sizes: Vec<S>`. The table gave `cpblk`
   and `initblk` a `Size` input and multi-dimensional array constructors
   their dimensions, but no effect used them, which broke goal 3 and
   invariant 7.2.
2. `Op::Opaque`, so an `Opaque` effect's edges can be `Derived`.
3. Invariant 7.5 is a per-role table of allowed uses. The summary in the
   first version contradicted §5, which uses `Object` inputs as cast and unbox
   sources and an `Address` as mkrefany's.
4. `TypeOracle` answers `is_delegate` instead of `base_type` (§3.5).
5. `FlowCx` holds an `Operands` resolver rather than module data, so Layer 1
   runs without a module in unit tests.
6. Layer 2 records more than the first sketch: `Block::{exit_height, preds,
   handler_entry}`, `BoundIns::{input_places, height}`, `InsId`, and
   `FlowWarning` as an enum. A catch or filter entry's exception assignment is
   a `BoundIns` of size 0 (`is_handler_entry`).
