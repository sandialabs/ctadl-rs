//! A concrete interpreter over cil-reader's Layer 2 ([`MethodFlow`]): it runs
//! a method's bound effects over real values, follows its CFG, and calls
//! into the module's other methods the same way.
//!
//! It is written to catch the flow layer out, not to be a CLR:
//!
//! - It moves only along the CFG. A branch, fall-through, `leave` or
//!   `endfinally` whose target is not among the block's `succs`, or an
//!   exception whose handler is not among its `exc_succs`, is a [`Stop::Bug`].
//! - It tracks the stack height from each instruction's pop and push counts
//!   and checks it against `BoundIns::height` and every block's
//!   `entry_height`. On entering a block, stack variables above its entry
//!   height are forgotten, and a read of a variable never written is a bug,
//!   so a value read from the wrong depth shows up even when the right value
//!   happens to be there.
//! - Before each instruction, every input `resolve_address` resolves must
//!   hold exactly the address of that place, since that analysis is a must;
//!   and every `ldloca`/`ldarga` it runs must be in `address_taken`.
//! - It matches only on `Effect`, `CallKind` and `Control`, except where the
//!   effect vocabulary leaves out what execution needs: the comparison a
//!   `Test` branches on, `isinst` vs `castclass`, `unbox` vs `unbox.any`,
//!   `ckfinite`, and `newarr` vs a multi-dimensional array's constructor.
//!
//! Every value carries a shadow ([`Sh`]) beside it, the labels `cil:taint`
//! tracks. Shadows never change what runs; `cil:exec`
//! starts with none and so carries none.
//!
//! Delegates are modeled as the frontend reads them: a
//! delegate constructor binds a target and a function pointer, and `Invoke`
//! calls the method with the target as `this` when it has one. The static
//! fields of compiler-generated types (the caches of lambdas and method
//! groups), which FlowRun neither prints nor resets, start at their defaults.
//!
//! Not modeled, so reported as [`Stop::Unsupported`]: strings, tokens,
//! function pointers to methods outside the module, generic methods and
//! types' own code, `localloc`, blocks, typed references, and calls outside
//! the module other than the `Object` and `Exception` constructors and
//! delegates. Filters run in the first pass of
//! their own frame only: a caller's filter runs after the callee's finally
//! blocks, where the CLR runs it before.

use std::collections::HashMap;
use std::rc::Rc;

use cil_reader::cil::Code;
use cil_reader::flow::delegate::{self, DelegateOp, ModuleTypes, TypeOracle};
use cil_reader::flow::{
    BlockId, BoundIns, CallKind, CallSite, CmpOp, Const, Control, Effect, Field, FlowOptions,
    Handler, HandlerKind, MethodFlow, NewKind, Op, Operands, Path, Place, Root, Transfer, TypeOp,
    Var,
};
use cil_reader::model::{MemberRefSig, MethodDef, TypeDef, TypeDefOrRef, TypeSig};
use cil_reader::{Entity, MetadataToken, Module, TokenType};

use super::value::{self, Base, Fault, Labels, Loc, Sh, Struct, Ty, Value, ARITHMETIC};

/// Why a call did not return normally.
#[derive(Debug, Clone)]
pub enum Stop {
    /// An exception, as a reference to the exception object.
    Throw(Value),
    /// Something this interpreter does not model.
    Unsupported(String),
    /// Behavior ECMA leaves unspecified.
    Unspecified(String),
    /// The flow layer contradicts itself or the program.
    Bug(String),
}

type R<T> = Result<T, Stop>;

fn bug<T>(msg: impl Into<String>) -> R<T> {
    Err(Stop::Bug(msg.into()))
}

fn unsupported<T>(msg: impl Into<String>) -> R<T> {
    Err(Stop::Unsupported(msg.into()))
}

/// A heap object.
#[derive(Debug, Clone)]
pub enum Obj {
    /// A class instance: its type's full name, and the fields this module's
    /// types declare, in token order.
    Object {
        ty: String,
        fields: Vec<(u32, Value)>,
    },
    Array {
        elem: Ty,
        /// The element code FlowRun prints.
        code: String,
        dims: Vec<usize>,
        items: Vec<Value>,
    },
    Boxed {
        ty: String,
        vty: Ty,
        value: Value,
    },
    /// A delegate: its type's full name, the target it binds (`null` for a static method),
    /// and the method (a MethodDef token).
    Delegate {
        ty: String,
        target: Value,
        method: u32,
    },
}

/// The shadow of a heap object, beside [`Obj`] at the same index.
#[derive(Debug, Clone)]
pub enum SObj {
    /// A class instance's fields; a field not here has no labels.
    Object(HashMap<u32, Sh>),
    /// An array's elements, and its length as a location of its own.
    Array { items: Vec<Sh>, length: Labels },
    /// A box's value.
    Boxed(Sh),
    /// A delegate's target.
    Delegate(Sh),
}

impl SObj {
    fn for_obj(o: &Obj) -> SObj {
        match o {
            Obj::Object { .. } => SObj::Object(HashMap::new()),
            Obj::Array { items, .. } => SObj::Array {
                items: vec![Sh::default(); items.len()],
                length: Labels::new(),
            },
            Obj::Boxed { .. } => SObj::Boxed(Sh::default()),
            Obj::Delegate { .. } => SObj::Delegate(Sh::default()),
        }
    }
}

/// A method ready to run.
pub struct Prepared {
    pub name: String,
    pub flow: MethodFlow,
    /// Argument types, `this` first.
    pub params: Vec<Ty>,
    /// For each argument, its pointee's type when it is `ref`/`out`.
    pub byref: Vec<Option<Ty>>,
    pub has_this: bool,
    pub locals: Vec<Ty>,
    pub ret: Option<Ty>,
}

/// What runs after an `endfinally`.
#[derive(Debug, Clone)]
enum Cont {
    /// A `leave` to `target` through `pending` finally blocks.
    Leave {
        running: usize,
        pending: Vec<usize>,
        target: u32,
    },
    /// An exception on its way to `catch` (or out of the method) through
    /// `pending` finally and fault blocks.
    Unwind {
        running: usize,
        pending: Vec<usize>,
        catch: Option<usize>,
        exc: Value,
    },
}

impl Cont {
    fn running(&self) -> usize {
        match self {
            Cont::Leave { running, .. } | Cont::Unwind { running, .. } => *running,
        }
    }
}

struct Frame {
    method: Rc<Prepared>,
    vars: HashMap<Var, Value>,
    /// The shadow of each variable in `vars`; a variable not here has none.
    svars: HashMap<Var, Sh>,
    conts: Vec<Cont>,
    /// The values the last `Test` effect read.
    tests: Vec<Value>,
    /// What the last `Return` effect returned.
    ret: Option<Option<Value>>,
    /// Its shadow.
    ret_sh: Sh,
}

enum Exit {
    Return(Option<Value>),
    /// A filter's verdict, and the block that ended it.
    EndFilter(Value, BlockId),
}

/// Instructions one trial may run.
const STEP_LIMIT: u64 = 5_000_000;
const DEPTH_LIMIT: usize = 200;

pub struct Interp<'m, 'd> {
    module: &'m Module<'d>,
    /// How methods' flow is built.
    options: FlowOptions,
    /// The reference runtime's measured ulong-to-double rounding behavior.
    pub uint64_double_via_signed: bool,
    /// Full names of the module's types.
    types: HashMap<String, u32>,
    methods: HashMap<u32, Rc<Prepared>>,
    field_tys: HashMap<u32, Ty>,
    instance_fields: HashMap<u32, Rc<Vec<(u32, Ty)>>>,
    pub heap: Vec<Obj>,
    pub statics: HashMap<u32, Value>,
    pub cells: Vec<(Ty, Value)>,
    /// The shadows of `heap`, index for index. [`Interp::sync_heap`] fills it in for
    /// objects pushed onto `heap` from outside.
    pub sheap: Vec<SObj>,
    /// The shadows of `cells`, index for index.
    pub scells: Vec<Sh>,
    /// The shadow an exception object was last thrown with, by heap index.
    pub exc_sh: HashMap<usize, Sh>,
    /// The compiler-generated types whose statics this trial has initialized.
    generated: std::collections::HashSet<u32>,
    frames: Vec<Frame>,
    steps: u64,
}

fn in_try(h: &Handler, offset: u32) -> bool {
    (h.try_start..h.try_end).contains(&offset)
}

fn in_handler(h: &Handler, offset: u32) -> bool {
    (h.handler_start..h.handler_end).contains(&offset)
}

impl<'m, 'd> Interp<'m, 'd> {
    pub fn new(module: &'m Module<'d>, options: FlowOptions) -> Interp<'m, 'd> {
        let mut types = HashMap::new();
        for t in module.all_types() {
            if let Ok(name) = t.full_name() {
                types.insert(name, t.token().raw());
            }
        }
        Interp {
            module,
            options,
            uint64_double_via_signed: false,
            types,
            methods: HashMap::new(),
            field_tys: HashMap::new(),
            instance_fields: HashMap::new(),
            heap: Vec::new(),
            statics: HashMap::new(),
            cells: Vec::new(),
            sheap: Vec::new(),
            scells: Vec::new(),
            exc_sh: HashMap::new(),
            generated: std::collections::HashSet::new(),
            frames: Vec::new(),
            steps: 0,
        }
    }

    /// Forget the state of the last trial.
    pub fn reset(&mut self) {
        self.heap.clear();
        self.statics.clear();
        self.cells.clear();
        self.sheap.clear();
        self.scells.clear();
        self.exc_sh.clear();
        self.generated.clear();
        self.frames.clear();
        self.steps = 0;
    }

    /// Gives every heap object and cell pushed from outside (a trial's starting state) an
    /// empty shadow, matching its kind.
    pub fn sync_heap(&mut self) {
        self.sheap.truncate(self.heap.len());
        for o in &self.heap[self.sheap.len()..] {
            self.sheap.push(SObj::for_obj(o));
        }
        for (i, o) in self.heap.iter().enumerate() {
            let mismatched = !matches!(
                (o, &self.sheap[i]),
                (Obj::Object { .. }, SObj::Object(_))
                    | (Obj::Array { .. }, SObj::Array { .. })
                    | (Obj::Boxed { .. }, SObj::Boxed(_))
                    | (Obj::Delegate { .. }, SObj::Delegate(_))
            );
            if mismatched {
                self.sheap[i] = SObj::for_obj(o);
            }
        }
        self.scells.resize(self.cells.len(), Sh::default());
    }

    // ---- types ---------------------------------------------------------------

    fn type_def(&self, token: u32) -> R<TypeDef<'_>> {
        let token = MetadataToken(token);
        match self.module.type_def(token.rid()) {
            Some(t) if token.token_type() == Some(TokenType::TypeDef) => Ok(t),
            _ => bug(format!("{:08x} is not a TypeDef", token.raw())),
        }
    }

    pub fn type_token(&self, name: &str) -> R<u32> {
        match self.types.get(name) {
            Some(t) => Ok(*t),
            None => unsupported(format!("type {name} is not in the module")),
        }
    }

    /// The full name of the type a TypeDef, TypeRef or TypeSpec names.
    pub fn type_name(&self, token: MetadataToken) -> R<String> {
        let name = match self.module.lookup_token(token) {
            Ok(Some(Entity::TypeDef(t))) => t.full_name(),
            Ok(Some(Entity::TypeRef(t))) => t.full_name(),
            Ok(Some(Entity::TypeSpec(t))) => t.full_name(),
            _ => return bug(format!("{:08x} names no type", token.raw())),
        };
        name.or_else(|e| bug(format!("name of {:08x}: {e}", token.raw())))
    }

    fn value_type_ty(&mut self, token: MetadataToken) -> R<Ty> {
        match self.module.lookup_token(token) {
            Ok(Some(Entity::TypeDef(t))) => {
                if t.is_enum().unwrap_or(false) {
                    let fields = t.fields().or_else(|e| bug(e.to_string()))?;
                    for f in fields {
                        if !f.is_static().unwrap_or(true) {
                            let sig = f.field_type().or_else(|e| bug(e.to_string()))?;
                            return self.ty_of_sig(&sig);
                        }
                    }
                    bug("an enum with no value field")
                } else {
                    Ok(Ty::Struct(t.token().raw()))
                }
            }
            Ok(Some(Entity::TypeRef(t))) => match t.primitive() {
                Ok(Some(sig)) => self.ty_of_sig(&sig),
                _ => unsupported(format!("value type {}", t.full_name().unwrap_or_default())),
            },
            Ok(Some(Entity::TypeSpec(t))) => {
                let sig = t.signature().or_else(|e| bug(e.to_string()))?;
                self.ty_of_sig(&sig)
            }
            _ => bug(format!("{:08x} names no type", token.raw())),
        }
    }

    /// The type of a location declared with `sig`.
    pub fn ty_of_sig(&mut self, sig: &TypeSig) -> R<Ty> {
        Ok(match sig {
            TypeSig::Boolean => Ty::Bool,
            TypeSig::Char => Ty::Char,
            TypeSig::I1 => Ty::I1,
            TypeSig::U1 => Ty::U1,
            TypeSig::I2 => Ty::I2,
            TypeSig::U2 => Ty::U2,
            TypeSig::I4 => Ty::I4,
            TypeSig::U4 => Ty::U4,
            TypeSig::I8 => Ty::I8,
            TypeSig::U8 => Ty::U8,
            TypeSig::R4 => Ty::R4,
            TypeSig::R8 => Ty::R8,
            TypeSig::IntPtr => Ty::I,
            TypeSig::UIntPtr => Ty::U,
            TypeSig::String | TypeSig::Object | TypeSig::SzArray(_) | TypeSig::Array { .. } => {
                Ty::Ref
            }
            TypeSig::Named {
                is_value_type: false,
                ..
            }
            | TypeSig::GenericInst {
                is_value_type: false,
                ..
            } => Ty::Ref,
            TypeSig::Named {
                token,
                is_value_type: true,
            } => self.value_type_ty(*token)?,
            TypeSig::ByRef(_) | TypeSig::Ptr(_) => Ty::Ptr,
            TypeSig::Pinned(inner) | TypeSig::Sentinel(inner) => self.ty_of_sig(inner)?,
            TypeSig::CModReq { inner, .. } | TypeSig::CModOpt { inner, .. } => {
                self.ty_of_sig(inner)?
            }
            other => return unsupported(format!("type {other:?}")),
        })
    }

    /// The type a type operand names, as a location type.
    fn ty_of_op(&mut self, t: &TypeOp) -> R<Ty> {
        match self.module.lookup_token(t.token) {
            Ok(Some(Entity::TypeDef(d))) => {
                if d.is_value_type().unwrap_or(false) {
                    self.value_type_ty(t.token)
                } else {
                    Ok(Ty::Ref)
                }
            }
            Ok(Some(Entity::TypeRef(r))) => {
                if let Ok(Some(sig)) = r.primitive() {
                    return self.ty_of_sig(&sig);
                }
                match self.module.is_value_type(t) {
                    Some(false) => Ok(Ty::Ref),
                    _ => unsupported(format!("type {}", r.full_name().unwrap_or_default())),
                }
            }
            Ok(Some(Entity::TypeSpec(s))) => {
                let sig = s.signature().or_else(|e| bug(e.to_string()))?;
                self.ty_of_sig(&sig)
            }
            _ => bug(format!("{} names no type", t.token.raw())),
        }
    }

    /// The element code FlowRun prints for an element type.
    fn elem_code(&self, ty: &Ty, name: &str) -> String {
        match ty {
            Ty::Struct(_) => format!("vt:{name}"),
            Ty::Ref => format!("ref:{name}"),
            other => other.code().unwrap_or("?").to_string(),
        }
    }

    /// An array element type from its code.
    pub fn ty_of_code(&self, code: &str) -> R<Ty> {
        if let Some(t) = Ty::from_code(code) {
            return Ok(t);
        }
        if let Some(name) = code.strip_prefix("vt:") {
            return Ok(Ty::Struct(self.type_token(name)?));
        }
        if code.starts_with("ref:") {
            return Ok(Ty::Ref);
        }
        unsupported(format!("element code {code}"))
    }

    /// The type a boxed value of type `name` holds.
    pub fn ty_of_boxed(&self, name: &str) -> R<Ty> {
        if let Some(t) = Ty::from_system_name(name) {
            return Ok(t);
        }
        Ok(Ty::Struct(self.type_token(name)?))
    }

    pub fn field_ty(&mut self, token: u32) -> R<Ty> {
        if let Some(t) = self.field_tys.get(&token) {
            return Ok(t.clone());
        }
        let tok = MetadataToken(token);
        let sig = match self.module.lookup_token(tok) {
            Ok(Some(Entity::Field(f))) => f.field_type(),
            Ok(Some(Entity::MemberRef(r))) => match r.signature() {
                Ok(MemberRefSig::Field(sig)) => Ok(sig),
                _ => return bug(format!("{token:08x} is not a field")),
            },
            _ => return bug(format!("{token:08x} is not a field")),
        }
        .or_else(|e| bug(e.to_string()))?;
        let ty = self.ty_of_sig(&sig)?;
        self.field_tys.insert(token, ty.clone());
        Ok(ty)
    }

    /// The instance fields of a module type and its module base types, in
    /// token order, as FlowRun shows them.
    pub fn instance_fields(&mut self, token: u32) -> R<Rc<Vec<(u32, Ty)>>> {
        if let Some(f) = self.instance_fields.get(&token) {
            return Ok(f.clone());
        }
        let mut tokens = Vec::new();
        let mut next = Some(self.type_def(token)?);
        while let Some(t) = next {
            for f in t.fields().or_else(|e| bug(e.to_string()))? {
                if !f.is_static().unwrap_or(true) {
                    tokens.push(f.token().raw());
                }
            }
            next = match t.base_type() {
                Ok(Some(TypeDefOrRef::Def(base))) => Some(base),
                _ => None,
            };
        }
        tokens.sort_unstable();
        let mut fields = Vec::new();
        for f in tokens {
            fields.push((f, self.field_ty(f)?));
        }
        let fields = Rc::new(fields);
        self.instance_fields.insert(token, fields.clone());
        Ok(fields)
    }

    pub fn default_of(&mut self, ty: &Ty) -> R<Value> {
        Ok(match ty {
            Ty::Bool | Ty::Char | Ty::I1 | Ty::U1 | Ty::I2 | Ty::U2 | Ty::I4 | Ty::U4 => {
                Value::I4(0)
            }
            Ty::I8 | Ty::U8 => Value::I8(0),
            Ty::I | Ty::U => Value::I(0),
            Ty::R4 => Value::R4(0.0),
            Ty::R8 => Value::R8(0.0),
            Ty::Ref | Ty::Ptr => Value::Null,
            Ty::Struct(t) => {
                let fields = self.instance_fields(*t)?;
                let mut values = Vec::with_capacity(fields.len());
                for (f, fty) in fields.iter() {
                    values.push((*f, self.default_of(fty)?));
                }
                Value::Struct(Struct {
                    ty: *t,
                    fields: values,
                })
            }
        })
    }

    /// The parent of a type, by name: from the module's metadata, or for the
    /// few framework types the fixtures meet, from this table.
    fn parent(&self, name: &str) -> R<Option<String>> {
        if let Some(&token) = self.types.get(name) {
            let t = self.type_def(token)?;
            return Ok(match t.base_type() {
                Ok(Some(base)) => Some(base.full_name().or_else(|e| bug(e.to_string()))?),
                _ => None,
            });
        }
        let parent = match name {
            "System.Object" => return Ok(None),
            "System.Exception" | "System.ValueType" | "System.Array" => "System.Object",
            "System.SystemException" => "System.Exception",
            "System.ArithmeticException"
            | "System.NullReferenceException"
            | "System.IndexOutOfRangeException"
            | "System.InvalidCastException"
            | "System.ArrayTypeMismatchException"
            | "System.ArgumentException" => "System.SystemException",
            "System.DivideByZeroException"
            | "System.OverflowException"
            | "System.NotFiniteNumberException" => "System.ArithmeticException",
            "System.Enum" => "System.ValueType",
            n if Ty::from_system_name(n).is_some() => "System.ValueType",
            _ => return unsupported(format!("the base type of {name}")),
        };
        Ok(Some(parent.to_string()))
    }

    /// Whether `v` is an instance of the type `t` names (null is not).
    fn is_instance(&mut self, v: &Value, t: &TypeOp) -> R<bool> {
        let target = self.type_name(t.token)?;
        let Value::Ref(o) = v else {
            return Ok(false);
        };
        let mut name = match &self.heap[*o] {
            Obj::Object { ty, .. } | Obj::Boxed { ty, .. } | Obj::Delegate { ty, .. } => ty.clone(),
            Obj::Array { .. } => {
                return match target.as_str() {
                    "System.Object" | "System.Array" => Ok(true),
                    t if t.ends_with(']') => unsupported("array type tests"),
                    _ => Ok(false),
                };
            }
        };
        loop {
            if name == target || self.implements(&name, &target)? {
                return Ok(true);
            }
            match self.parent(&name)? {
                Some(p) => name = p,
                None => return Ok(false),
            }
        }
    }

    /// Whether the module type `name` lists the interface `target`, directly or through the
    /// interfaces it lists.
    fn implements(&self, name: &str, target: &str) -> R<bool> {
        let Some(&token) = self.types.get(name) else {
            return Ok(false);
        };
        let mut queue = vec![self.type_def(token)?];
        let mut seen = 0;
        while let Some(t) = queue.pop() {
            seen += 1;
            if seen > 256 {
                return bug("an interface cycle");
            }
            for i in t.interfaces().or_else(|e| bug(e.to_string()))? {
                let Some(iface) = i.interface else { continue };
                let iname = iface.full_name().or_else(|e| bug(e.to_string()))?;
                if iname == target {
                    return Ok(true);
                }
                if let Some(&it) = self.types.get(&iname) {
                    queue.push(self.type_def(it)?);
                }
            }
        }
        Ok(false)
    }

    // ---- heap ----------------------------------------------------------------

    fn alloc(&mut self, o: Obj) -> Value {
        self.sheap.push(SObj::for_obj(&o));
        self.heap.push(o);
        Value::Ref(self.heap.len() - 1)
    }

    /// A new exception of a framework type, thrown.
    fn throw<T>(&mut self, name: &str) -> R<T> {
        let e = self.alloc(Obj::Object {
            ty: name.to_string(),
            fields: Vec::new(),
        });
        Err(Stop::Throw(e))
    }

    fn fault<T>(&mut self, f: Fault) -> R<T> {
        match f {
            Fault::Throw(name) => self.throw(name),
            Fault::Unspecified(m) => Err(Stop::Unspecified(m)),
            Fault::Invalid(m) => bug(m),
        }
    }

    // ---- methods -------------------------------------------------------------

    fn method_def(&self, token: u32) -> R<MethodDef<'_>> {
        let tok = MetadataToken(token);
        match self.module.method_def(tok.rid()) {
            Some(m) if tok.token_type() == Some(TokenType::Method) => Ok(m),
            _ => bug(format!("{token:08x} is not a MethodDef")),
        }
    }

    pub fn prepare(&mut self, token: u32) -> R<Rc<Prepared>> {
        if let Some(p) = self.methods.get(&token) {
            return Ok(p.clone());
        }
        let module = self.module;
        let tok = MetadataToken(token);
        let md = match module.method_def(tok.rid()) {
            Some(m) if tok.token_type() == Some(TokenType::Method) => m,
            _ => return bug(format!("{token:08x} is not a MethodDef")),
        };
        let decl = md.declaring_type().ok().flatten();
        let name = format!(
            "{}::{}",
            decl.and_then(|t| t.full_name().ok()).unwrap_or_default(),
            md.name().unwrap_or_default()
        );
        if !md.has_body().unwrap_or(false) || md.rva().unwrap_or(0) == 0 {
            return unsupported(format!("{name} has no body"));
        }
        let body = md.body().or_else(|e| bug(format!("{name}: {e}")))?;
        let sig = md.signature().or_else(|e| bug(format!("{name}: {e}")))?;
        if sig.generic_arity > 0 {
            return unsupported(format!("generic method {name}"));
        }
        let flow = MethodFlow::build_with(&body, self.options);
        let mut params = Vec::new();
        let mut byref = Vec::new();
        if sig.has_this {
            let value_type = decl.is_some_and(|t| t.is_value_type().unwrap_or(false));
            params.push(if value_type { Ty::Ptr } else { Ty::Ref });
            byref.push(None);
        }
        for p in &sig.params {
            params.push(self.ty_of_sig(p)?);
            byref.push(match p.unwrap_modifiers() {
                TypeSig::ByRef(inner) => Some(self.ty_of_sig(inner)?),
                _ => None,
            });
        }
        let mut locals = Vec::new();
        for l in &body.variables {
            locals.push(self.ty_of_sig(l)?);
        }
        let ret = match sig.return_type {
            TypeSig::Void => None,
            ref t => Some(self.ty_of_sig(t)?),
        };
        let p = Rc::new(Prepared {
            name,
            flow,
            params,
            byref,
            has_this: sig.has_this,
            locals,
            ret,
        });
        self.methods.insert(token, p.clone());
        Ok(p)
    }

    /// The method a `callvirt` of `token` reaches on an object of type
    /// `runtime`: the nearest override, walking up from the runtime type.
    fn dispatch(&self, token: u32, runtime: &str) -> R<u32> {
        let md = self.method_def(token)?;
        let Some(&start) = self.types.get(runtime) else {
            return Ok(token);
        };
        let decl = match md.declaring_type() {
            Ok(Some(t)) => t.token().raw(),
            _ => return bug("a method with no declaring type"),
        };
        let name = md.name().or_else(|e| bug(e.to_string()))?;
        let sig = md.signature().or_else(|e| bug(e.to_string()))?;
        if self.type_def(decl)?.is_interface().unwrap_or(false) {
            return self.dispatch_interface(token, &name, &sig, start);
        }
        let mut next = Some(self.type_def(start)?);
        while let Some(t) = next {
            if t.token().raw() == decl {
                return Ok(token);
            }
            for m in t.methods().or_else(|e| bug(e.to_string()))? {
                if m.is_virtual().unwrap_or(false)
                    && !m.is_newslot().unwrap_or(true)
                    && m.name().ok().as_deref() == Some(&*name)
                    && m.signature().ok().as_ref() == Some(&sig)
                {
                    return Ok(m.token().raw());
                }
            }
            next = match t.base_type() {
                Ok(Some(TypeDefOrRef::Def(base))) => Some(base),
                _ => None,
            };
        }
        Ok(token)
    }

    /// The implementation of the interface method `token` an object of type `start` has: a
    /// method whose `MethodImpl` row names it (an explicit implementation), else a virtual
    /// method of the same name and signature, up the base chain.
    fn dispatch_interface(
        &self,
        token: u32,
        name: &str,
        sig: &cil_reader::model::MethodSig,
        start: u32,
    ) -> R<u32> {
        let mut next = Some(self.type_def(start)?);
        while let Some(t) = next {
            let methods = t.methods().or_else(|e| bug(e.to_string()))?;
            for m in &methods {
                let overrides = m.overrides().or_else(|e| bug(e.to_string()))?;
                if overrides.iter().any(|o| o.raw() == token) {
                    return Ok(m.token().raw());
                }
            }
            for m in &methods {
                if m.is_virtual().unwrap_or(false)
                    && !m.is_abstract().unwrap_or(true)
                    && m.name().ok().as_deref() == Some(name)
                    && m.signature().ok().as_ref() == Some(sig)
                {
                    return Ok(m.token().raw());
                }
            }
            next = match t.base_type() {
                Ok(Some(TypeDefOrRef::Def(base))) => Some(base),
                _ => None,
            };
        }
        unsupported("an interface method no module type implements")
    }

    /// `Ns.Type::Name` of a method outside the module.
    fn member_name(&self, token: MetadataToken) -> R<String> {
        let Ok(Some(Entity::MemberRef(r))) = self.module.lookup_token(token) else {
            return unsupported(format!("call of {:08x}", token.raw()));
        };
        let class = r.class_token().or_else(|e| bug(e.to_string()))?;
        Ok(format!(
            "{}::{}",
            self.type_name(class)?,
            r.name().or_else(|e| bug(e.to_string()))?
        ))
    }

    /// Run a method of the module. `args` holds `this` first.
    pub fn call(&mut self, token: u32, args: Vec<Value>) -> R<Option<Value>> {
        let shs = vec![Sh::default(); args.len()];
        self.call_sh(token, args, shs).map(|(v, _)| v)
    }

    /// [`Interp::call`] with the arguments' shadows; the result's shadow too.
    pub fn call_sh(
        &mut self,
        token: u32,
        args: Vec<Value>,
        shs: Vec<Sh>,
    ) -> R<(Option<Value>, Sh)> {
        if self.frames.len() >= DEPTH_LIMIT {
            return unsupported("call depth");
        }
        let p = self.prepare(token)?;
        if args.len() != p.params.len() {
            return bug(format!(
                "{} takes {} arguments, given {}",
                p.name,
                p.params.len(),
                args.len()
            ));
        }
        let mut vars = HashMap::new();
        let mut svars = HashMap::new();
        for (k, ((a, ty), sh)) in args.into_iter().zip(&p.params).zip(shs).enumerate() {
            let a = ty
                .store(a)
                .or_else(|e| bug(format!("{} argument {k}: {e}", p.name)))?;
            vars.insert(Var::Arg(k as u16), a);
            svars.insert(Var::Arg(k as u16), sh);
        }
        for (n, ty) in p.locals.iter().enumerate() {
            let v = self.default_of(ty)?;
            vars.insert(Var::Local(n as u16), v);
        }
        if p.flow.blocks.is_empty() {
            return bug(format!("{} has no blocks", p.name));
        }
        if !p.flow.warnings.is_empty() {
            return bug(format!("{}: flow warnings {:?}", p.name, p.flow.warnings));
        }
        self.frames.push(Frame {
            method: p.clone(),
            vars,
            svars,
            conts: Vec::new(),
            tests: Vec::new(),
            ret: None,
            ret_sh: Sh::default(),
        });
        let fi = self.frames.len() - 1;
        let exit = self.run(fi, 0, 0, false);
        let ret_sh = std::mem::take(&mut self.frames[fi].ret_sh);
        self.frames.pop();
        match exit? {
            Exit::Return(v) => match (&p.ret, v) {
                (Some(ty), Some(v)) => Ok((
                    Some(
                        ty.store(v)
                            .or_else(|e| bug(format!("{} returns: {e}", p.name)))?,
                    ),
                    ret_sh,
                )),
                (None, None) => Ok((None, Sh::default())),
                _ => bug(format!("{} returns the wrong arity", p.name)),
            },
            Exit::EndFilter(..) => bug(format!("{} ended in endfilter", p.name)),
        }
    }

    // ---- the machine ---------------------------------------------------------

    fn read(&self, fi: usize, v: Var) -> R<Value> {
        match self.frames[fi].vars.get(&v) {
            Some(x) => Ok(x.clone()),
            None => bug(format!("read of {v}, which holds no value")),
        }
    }

    fn sread(&self, fi: usize, v: Var) -> Sh {
        self.frames[fi].svars.get(&v).cloned().unwrap_or_default()
    }

    fn swrite(&mut self, fi: usize, v: Var, sh: Sh) {
        self.frames[fi].svars.insert(v, sh);
    }

    /// Writes a value and its shadow.
    fn write_sh(&mut self, fi: usize, v: Var, x: Value, sh: Sh) -> R<()> {
        self.write(fi, v, x)?;
        self.swrite(fi, v, sh);
        Ok(())
    }

    fn write(&mut self, fi: usize, v: Var, x: Value) -> R<()> {
        let frame = &self.frames[fi];
        let ty = match v {
            Var::Arg(n) => frame.method.params.get(n as usize),
            Var::Local(n) => frame.method.locals.get(n as usize),
            _ => None,
        };
        let x = match ty {
            Some(ty) => ty.store(x).or_else(|e| bug(format!("write of {v}: {e}")))?,
            None => x,
        };
        self.frames[fi].vars.insert(v, x);
        Ok(())
    }

    /// Forget the stack at and above depth `height`.
    fn clear_stack(&mut self, fi: usize, height: u16) {
        let above = |v: &Var| matches!(v, Var::Stack(d) if *d >= height);
        self.frames[fi].vars.retain(|v, _| !above(v));
        self.frames[fi].svars.retain(|v, _| !above(v));
    }

    /// The shadow exception `exc` was thrown with.
    pub fn exc_shadow(&self, exc: &Value) -> Sh {
        match exc {
            Value::Ref(o) => self.exc_sh.get(o).cloned().unwrap_or_default(),
            _ => Sh::default(),
        }
    }

    /// Hands handler `c` of frame `fi` the exception, and its shadow.
    fn set_caught(&mut self, fi: usize, c: usize, exc: Value) {
        let sh = self.exc_shadow(&exc);
        self.frames[fi].vars.insert(Var::Caught(c as u16), exc);
        self.frames[fi].svars.insert(Var::Caught(c as u16), sh);
    }

    /// Follow a normal edge from `from` to the block at `target`.
    fn goto(
        &mut self,
        fi: usize,
        mf: &MethodFlow,
        from: BlockId,
        target: u32,
        height: u16,
    ) -> R<BlockId> {
        let Some(to) = mf.blocks.iter().position(|b| b.start == target) else {
            return bug(format!("no block starts at IL_{target:04x}"));
        };
        if !mf.blocks[from].succs.contains(&to) {
            return bug(format!(
                "no edge from the block at IL_{:04x} to IL_{target:04x}",
                mf.blocks[from].start
            ));
        }
        if mf.blocks[to].entry_height != height {
            return bug(format!(
                "IL_{target:04x} entered at height {height}, its entry height is {}",
                mf.blocks[to].entry_height
            ));
        }
        self.clear_stack(fi, height);
        Ok(to)
    }

    /// Check an exceptional transfer from `from` to `to`.
    fn exc_edge(mf: &MethodFlow, from: BlockId, to: BlockId) -> R<()> {
        if mf.blocks[from].exc_succs.contains(&to) {
            Ok(())
        } else {
            bug(format!(
                "no exception edge from the block at IL_{:04x} to IL_{:04x}",
                mf.blocks[from].start, mf.blocks[to].start
            ))
        }
    }

    fn step(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            return unsupported("step limit");
        }
        Ok(())
    }

    /// Run frame `fi` from block `start` at `height` until it returns, or,
    /// for a filter, until its `endfilter`.
    fn run(&mut self, fi: usize, start: BlockId, height: u16, filter: bool) -> R<Exit> {
        let m = self.frames[fi].method.clone();
        let mf = &m.flow;
        let (mut b, mut i, mut height) = (start, 0usize, height);
        self.clear_stack(fi, height);
        loop {
            self.step()?;
            let block = &mf.blocks[b];
            let Some(ins) = block.instrs.get(i) else {
                return bug(format!(
                    "{}: ran off the block at IL_{:04x}",
                    m.name, block.start
                ));
            };
            let here = ins.flow.offset;
            let at = |msg: String| Stop::Bug(format!("{} IL_{here:04x}: {msg}", m.name));
            if ins.height != height {
                return Err(at(format!("height {} bound, {height} reached", ins.height)));
            }
            match self.exec(fi, mf, ins) {
                Ok(()) => {}
                Err(Stop::Throw(exc)) => {
                    if filter {
                        // An exception in a filter rejects.
                        return Ok(Exit::EndFilter(Value::I4(0), b));
                    }
                    (b, height) = self.raise(fi, mf, b, here, exc).map_err(|e| match e {
                        Stop::Bug(msg) => at(msg),
                        e => e,
                    })?;
                    i = 0;
                    continue;
                }
                Err(Stop::Bug(msg)) => return Err(at(msg)),
                Err(e) => return Err(e),
            }
            let pops = ins.inputs.len() as u16;
            if pops > height {
                return Err(at(format!("pops {pops} at height {height}")));
            }
            height = height - pops + ins.outputs.len() as u16;
            let last = i + 1 == block.instrs.len();
            let control = &ins.flow.control;
            if !last && *control != Control::Next {
                return Err(at(format!("{control} in the middle of a block")));
            }
            let end = ins.flow.end_offset();
            let result = match control {
                Control::Next if !last => {
                    i += 1;
                    continue;
                }
                Control::Next => self.goto(fi, mf, b, end, height),
                Control::Goto(t) => self.goto(fi, mf, b, *t, height),
                Control::Cond { taken } => {
                    let take = self.condition(fi, ins.flow.opcode.code)?;
                    let target = if take { taken[0] } else { end };
                    self.goto(fi, mf, b, target, height)
                }
                Control::Switch(targets) => {
                    let target = match self.frames[fi].tests.first() {
                        Some(Value::I4(k)) => {
                            targets.get(*k as u32 as usize).copied().unwrap_or(end)
                        }
                        other => return Err(at(format!("switch on {other:?}"))),
                    };
                    self.goto(fi, mf, b, target, height)
                }
                Control::Leave(target) => {
                    height = 0;
                    self.clear_stack(fi, 0);
                    self.leave(fi, mf, b, here, *target)
                }
                Control::EndFinally => {
                    height = 0;
                    self.clear_stack(fi, 0);
                    match self.end_finally(fi, mf, b, here) {
                        Ok(Some(to)) => Ok(to),
                        Ok(None) => unreachable!("end_finally resumes or stops"),
                        Err(e) => Err(e),
                    }
                }
                Control::EndFilter => {
                    if !filter {
                        return Err(at("endfilter outside a filter".to_string()));
                    }
                    let verdict = self.frames[fi].tests.first().cloned();
                    return match verdict {
                        Some(v) => Ok(Exit::EndFilter(v, b)),
                        None => Err(at("endfilter without a Test".to_string())),
                    };
                }
                Control::Return => {
                    let Some(v) = self.frames[fi].ret.take() else {
                        return Err(at("ret without a Return effect".to_string()));
                    };
                    return Ok(Exit::Return(v));
                }
                Control::Throw => return Err(at("throw completed normally".to_string())),
                Control::Jump => return unsupported("jmp"),
            };
            b = result.map_err(|e| match e {
                Stop::Bug(msg) => at(msg),
                e => e,
            })?;
            i = 0;
        }
    }

    /// `leave` from `here` in block `from` to `target`, through the finally
    /// blocks it exits.
    fn leave(
        &mut self,
        fi: usize,
        mf: &MethodFlow,
        from: BlockId,
        here: u32,
        target: u32,
    ) -> R<BlockId> {
        let finallies: Vec<usize> = mf
            .handlers
            .iter()
            .enumerate()
            .filter(|(_, h)| {
                h.kind == HandlerKind::Finally && in_try(h, here) && !in_try(h, target)
            })
            .map(|(i, _)| i)
            .collect();
        match finallies.split_first() {
            None => self.goto(fi, mf, from, target, 0),
            Some((&first, rest)) => {
                self.frames[fi].conts.push(Cont::Leave {
                    running: first,
                    pending: rest.to_vec(),
                    target,
                });
                let entry = Self::handler_block(mf, first)?;
                self.goto(fi, mf, from, mf.blocks[entry].start, 0)
            }
        }
    }

    fn handler_block(mf: &MethodFlow, h: usize) -> R<BlockId> {
        match mf.handlers[h].handler_block {
            Some(b) => Ok(b),
            None => bug(format!("handler {h} has no block")),
        }
    }

    /// Where `endfinally` in block `from` goes: the next finally, the
    /// `leave` target, the handler an exception is going to, or out of the
    /// method with the exception.
    fn end_finally(
        &mut self,
        fi: usize,
        mf: &MethodFlow,
        from: BlockId,
        here: u32,
    ) -> R<Option<BlockId>> {
        let Some(cont) = self.frames[fi].conts.pop() else {
            return bug("endfinally with no finally running");
        };
        if !in_handler(&mf.handlers[cont.running()], here) {
            return bug(format!(
                "endfinally outside the running handler {}",
                cont.running()
            ));
        }
        match cont {
            Cont::Leave {
                pending, target, ..
            } => match pending.split_first() {
                None => self.goto(fi, mf, from, target, 0).map(Some),
                Some((&next, rest)) => {
                    self.frames[fi].conts.push(Cont::Leave {
                        running: next,
                        pending: rest.to_vec(),
                        target,
                    });
                    let entry = Self::handler_block(mf, next)?;
                    self.goto(fi, mf, from, mf.blocks[entry].start, 0).map(Some)
                }
            },
            Cont::Unwind {
                pending,
                catch,
                exc,
                ..
            } => match pending.split_first() {
                Some((&next, rest)) => {
                    let entry = Self::handler_block(mf, next)?;
                    if !mf.blocks[from].succs.contains(&entry) {
                        Self::exc_edge(mf, from, entry)?;
                    }
                    self.frames[fi].conts.push(Cont::Unwind {
                        running: next,
                        pending: rest.to_vec(),
                        catch,
                        exc,
                    });
                    Ok(Some(entry))
                }
                None => match catch {
                    Some(c) => self.enter_handler(fi, mf, Some(from), c, exc).map(Some),
                    None => Err(Stop::Throw(exc)),
                },
            },
        }
    }

    /// Start handler `c` with the exception `exc`. `from` is the block the
    /// exception edge leaves, when it is one (not a filter's verdict).
    fn enter_handler(
        &mut self,
        fi: usize,
        mf: &MethodFlow,
        from: Option<BlockId>,
        c: usize,
        exc: Value,
    ) -> R<BlockId> {
        let entry = Self::handler_block(mf, c)?;
        if let (Some(from), HandlerKind::Catch) = (from, mf.handlers[c].kind) {
            Self::exc_edge(mf, from, entry)?;
        }
        self.set_caught(fi, c, exc);
        self.clear_stack(fi, 0);
        Ok(entry)
    }

    /// An exception at `here` in block `from`: find its handler (running
    /// filters), then run the finally and fault blocks it leaves. Returns
    /// where to resume, at height 0.
    fn raise(
        &mut self,
        fi: usize,
        mf: &MethodFlow,
        from: BlockId,
        here: u32,
        exc: Value,
    ) -> R<(BlockId, u16)> {
        let mut catch = None;
        let mut filtered_by = None;
        for (h, handler) in mf.handlers.iter().enumerate() {
            if !in_try(handler, here) {
                continue;
            }
            match handler.kind {
                HandlerKind::Catch => {
                    let Some(ty) = handler.catch_type else {
                        return bug(format!("catch handler {h} with no type"));
                    };
                    if self.is_instance(&exc, &ty)? {
                        catch = Some(h);
                        break;
                    }
                }
                HandlerKind::Filter => {
                    let Some(entry) = handler.entry else {
                        return bug(format!("filter {h} has no block"));
                    };
                    Self::exc_edge(mf, from, entry)?;
                    self.set_caught(fi, h, exc.clone());
                    let Exit::EndFilter(verdict, end) = self.run(fi, entry, 0, true)? else {
                        return bug("a filter returned");
                    };
                    if value::truthy(&verdict).or_else(|f| self.fault(f))? {
                        catch = Some(h);
                        filtered_by = Some(end);
                        break;
                    }
                }
                HandlerKind::Finally | HandlerKind::Fault => {}
            }
        }
        // The exception leaves the finally blocks it was raised in, unless
        // its handler is inside them.
        let catch_start = catch.map(|c| mf.handlers[c].handler_start);
        self.frames[fi].conts.retain(|cont| {
            let running = &mf.handlers[cont.running()];
            !in_handler(running, here) || catch_start.is_some_and(|s| in_handler(running, s))
        });
        let limit = catch.unwrap_or(mf.handlers.len());
        let unwound: Vec<usize> = (0..limit)
            .filter(|&h| {
                let handler = &mf.handlers[h];
                in_try(handler, here)
                    && matches!(handler.kind, HandlerKind::Finally | HandlerKind::Fault)
            })
            .collect();
        if let Some((&first, rest)) = unwound.split_first() {
            let entry = Self::handler_block(mf, first)?;
            Self::exc_edge(mf, from, entry)?;
            self.frames[fi].conts.push(Cont::Unwind {
                running: first,
                pending: rest.to_vec(),
                catch,
                exc,
            });
            self.clear_stack(fi, 0);
            return Ok((entry, 0));
        }
        match catch {
            None => Err(Stop::Throw(exc)),
            Some(c) => {
                if let Some(end) = filtered_by {
                    let entry = Self::handler_block(mf, c)?;
                    if !mf.blocks[end].succs.contains(&entry) {
                        return bug(format!(
                            "no edge from the filter's end at IL_{:04x} to its handler",
                            mf.blocks[end].start
                        ));
                    }
                    self.set_caught(fi, c, exc);
                    self.clear_stack(fi, 0);
                    return Ok((entry, 0));
                }
                Ok((self.enter_handler(fi, mf, Some(from), c, exc)?, 0))
            }
        }
    }

    /// Whether the conditional branch `code` is taken, from the values its
    /// `Test` read.
    fn condition(&mut self, fi: usize, code: Code) -> R<bool> {
        let tests = self.frames[fi].tests.clone();
        let result = match (code, tests.as_slice()) {
            (Code::Brtrue | Code::Brtrue_S, [v]) => value::truthy(v),
            (Code::Brfalse | Code::Brfalse_S, [v]) => value::truthy(v).map(|t| !t),
            (code, [a, b]) => {
                // ECMA §III.3: a branch is its comparison and brtrue or
                // brfalse; for floats the `.un` forms are unordered.
                let float = matches!(a, Value::R4(_) | Value::R8(_));
                let cmp = |op, un| value::compare(op, un, a, b);
                match code {
                    Code::Beq | Code::Beq_S => cmp(CmpOp::Eq, false),
                    Code::Bne_Un | Code::Bne_Un_S => cmp(CmpOp::Eq, false).map(|x| !x),
                    Code::Bge | Code::Bge_S => cmp(CmpOp::Lt, float).map(|x| !x),
                    Code::Bge_Un | Code::Bge_Un_S => cmp(CmpOp::Lt, !float).map(|x| !x),
                    Code::Bgt | Code::Bgt_S => cmp(CmpOp::Gt, false),
                    Code::Bgt_Un | Code::Bgt_Un_S => cmp(CmpOp::Gt, true),
                    Code::Ble | Code::Ble_S => cmp(CmpOp::Gt, float).map(|x| !x),
                    Code::Ble_Un | Code::Ble_Un_S => cmp(CmpOp::Gt, !float).map(|x| !x),
                    Code::Blt | Code::Blt_S => cmp(CmpOp::Lt, false),
                    Code::Blt_Un | Code::Blt_Un_S => cmp(CmpOp::Lt, true),
                    other => return bug(format!("{other:?} is not a conditional branch")),
                }
            }
            (code, tests) => return bug(format!("{code:?} tested {} values", tests.len())),
        };
        result.or_else(|f| self.fault(f))
    }

    // ---- effects -------------------------------------------------------------

    fn exec(&mut self, fi: usize, mf: &MethodFlow, ins: &BoundIns) -> R<()> {
        self.check_places(fi, ins)?;
        self.frames[fi].tests.clear();
        let code = ins.flow.opcode.code;
        for e in &ins.effects {
            let taken = match e {
                Effect::AddressOf {
                    place: Place::Local(n),
                    ..
                } => Some(Var::Local(*n)),
                Effect::AddressOf {
                    place: Place::Arg(n),
                    ..
                } => Some(Var::Arg(*n)),
                _ => None,
            };
            if let Some(v) = taken {
                if !mf.address_taken().contains(v) {
                    return bug(format!(
                        "the address of {v} is taken, but not in address_taken"
                    ));
                }
            }
            self.effect(fi, code, e)?;
        }
        Ok(())
    }

    /// `resolve_address` is a must-analysis: where it names the place an
    /// input points at, the input must hold exactly that address.
    fn check_places(&mut self, fi: usize, ins: &BoundIns) -> R<()> {
        for (i, place) in ins.input_places.iter().enumerate() {
            let Some(place) = place else { continue };
            let Some(&var) = ins.inputs.get(i) else {
                return bug(format!("a place for input {i}, which does not exist"));
            };
            let actual = self.read(fi, var)?;
            let claimed = match self.place(fi, place) {
                Ok(loc) => Value::Ptr(loc),
                // Evaluating the place threw (a null base, say): the
                // address it names cannot be the one the input holds.
                Err(Stop::Throw(_)) => Value::Null,
                Err(e) => return Err(e),
            };
            if actual != claimed {
                return bug(format!(
                    "resolve_address says input {i} ({var}) points at {place}, \
                     but it holds {}",
                    actual.describe()
                ));
            }
        }
        Ok(())
    }

    fn effect(&mut self, fi: usize, code: Code, e: &Effect<Var>) -> R<()> {
        match e {
            Effect::Assign {
                dst,
                srcs,
                transfer,
            } => {
                let mut vals = Vec::with_capacity(srcs.len());
                let mut shs = Vec::with_capacity(srcs.len());
                for s in srcs {
                    vals.push(self.read(fi, *s)?);
                    shs.push(self.sread(fi, *s));
                }
                let (v, sh) = self.transfer(code, transfer, vals, shs)?;
                self.write_sh(fi, *dst, v, sh)
            }
            Effect::Const { dst, value } => {
                let v = match value {
                    Const::I4(x) => Value::I4(*x),
                    Const::I8(x) => Value::I8(*x),
                    Const::R4(x) => Value::R4(*x),
                    Const::R8(x) => Value::R8(*x),
                    Const::Null => Value::Null,
                    other => return unsupported(format!("constant {other}")),
                };
                self.write_sh(fi, *dst, v, Sh::default())
            }
            Effect::Load { dst, src } => {
                let (v, sh) = self.load(fi, src)?;
                self.write_sh(fi, *dst, v, sh)
            }
            Effect::Store { dst, src } => {
                let v = self.read(fi, *src)?;
                let sh = self.sread(fi, *src);
                self.store(fi, dst, v, sh)
            }
            Effect::AddressOf { dst, place } => {
                let loc = self.place(fi, place)?;
                self.write_sh(fi, *dst, Value::Ptr(loc), Sh::default())
            }
            Effect::New {
                dst,
                kind,
                ty,
                sizes,
            } => {
                let v = self.new_value(fi, code, *kind, ty.as_ref(), sizes)?;
                self.write_sh(fi, *dst, v, Sh::default())
            }
            Effect::Clear { addr } => {
                let Value::Ptr(loc) = self.read(fi, *addr)? else {
                    return bug("initobj of a value that is not an address");
                };
                let ty = match self.loc_ty(&loc)? {
                    Some(ty) => ty,
                    None => match self.read_loc(&loc)? {
                        Value::Struct(s) => Ty::Struct(s.ty),
                        other => return bug(format!("initobj of {}", other.describe())),
                    },
                };
                let v = self.default_of(&ty)?;
                self.write_loc(&loc, v)?;
                self.swrite_loc(&loc, Sh::default())
            }
            Effect::Call(site) => self.call_site(fi, site),
            Effect::Return { src } => {
                let (v, sh) = match src {
                    Some(s) => (Some(self.read(fi, *s)?), self.sread(fi, *s)),
                    None => (None, Sh::default()),
                };
                self.frames[fi].ret = Some(v);
                self.frames[fi].ret_sh = sh;
                Ok(())
            }
            Effect::Throw { src } => {
                let v = self.read(fi, *src)?;
                if v == Value::Null {
                    return self.throw("System.NullReferenceException");
                }
                let Value::Ref(o) = v else {
                    return bug(format!("throw of {}", v.describe()));
                };
                let sh = self.sread(fi, *src);
                self.exc_sh.insert(o, sh.clone());
                self.write_sh(fi, Var::Thrown, v.clone(), sh)?;
                Err(Stop::Throw(v))
            }
            Effect::Test { srcs } => {
                let mut vals = Vec::with_capacity(srcs.len());
                for s in srcs {
                    vals.push(self.read(fi, *s)?);
                }
                self.frames[fi].tests = vals;
                Ok(())
            }
            Effect::FunctionPtr {
                dst,
                method,
                receiver,
            } => {
                if method.token.token_type() != Some(TokenType::Method) {
                    return unsupported("a function pointer to a method outside the module");
                }
                let mut target = method.token.raw();
                if let Some(r) = receiver {
                    // ldvirtftn: the override the receiver's type has.
                    match self.read(fi, *r)? {
                        Value::Null => return self.throw("System.NullReferenceException"),
                        Value::Ref(o) => {
                            if let Obj::Object { ty, .. } = &self.heap[o] {
                                let ty = ty.clone();
                                target = self.dispatch(target, &ty)?;
                            }
                        }
                        other => return bug(format!("ldvirtftn on {}", other.describe())),
                    }
                }
                self.write_sh(fi, *dst, Value::Fn(target), Sh::default())
            }
            Effect::Jump { .. } => unsupported("jmp"),
            Effect::Opaque { reason, .. } => bug(format!("an opaque effect ({reason:?})")),
        }
    }

    /// An `Assign`'s value and its shadow: a copy, cast, box or unbox keeps its
    /// operand's labels, and a computed value takes the union of its operands'.
    fn transfer(
        &mut self,
        code: Code,
        transfer: &Transfer,
        vals: Vec<Value>,
        shs: Vec<Sh>,
    ) -> R<(Value, Sh)> {
        let operand = shs.first().cloned().unwrap_or_default();
        let v = self.transfer_value(code, transfer, vals.clone())?;
        let sh = match transfer {
            Transfer::Copy | Transfer::Cast(_) => operand,
            Transfer::Box(_) => {
                // A fresh box holds the operand; the reference carries its labels too, since
                // the frontend lowers `box` as a copy.
                if let (Value::Ref(o), [old]) = (&v, vals.as_slice()) {
                    if *old != v {
                        if let Some(SObj::Boxed(content)) = self.sheap.get_mut(*o) {
                            *content = operand.clone();
                        }
                    }
                }
                Sh::of(operand.labels.clone())
            }
            Transfer::Unbox(_) => match vals.first() {
                Some(Value::Ref(o)) if code != Code::Unbox => {
                    let content = match self.sheap.get(*o) {
                        Some(SObj::Boxed(c)) => c.clone(),
                        _ => Sh::default(),
                    };
                    let mut sh = content;
                    sh.labels.extend(operand.labels.iter().copied());
                    sh
                }
                _ => Sh::of(operand.labels.clone()),
            },
            Transfer::Derived(_) => Sh::union(&shs),
        };
        Ok((v, sh))
    }

    fn transfer_value(&mut self, code: Code, transfer: &Transfer, vals: Vec<Value>) -> R<Value> {
        let one = |vals: &[Value]| match vals {
            [v] => Ok(v.clone()),
            _ => bug(format!("{transfer} of {} values", vals.len())),
        };
        match transfer {
            Transfer::Copy => {
                let v = one(&vals)?;
                if code == Code::Ckfinite {
                    let finite = match v {
                        Value::R4(x) => x.is_finite(),
                        Value::R8(x) => x.is_finite(),
                        _ => return bug(format!("ckfinite of {}", v.describe())),
                    };
                    if !finite {
                        return self.throw(ARITHMETIC);
                    }
                }
                Ok(v)
            }
            Transfer::Cast(t) => {
                let v = one(&vals)?;
                match v {
                    Value::Null => Ok(Value::Null),
                    Value::Ref(_) => {
                        if self.is_instance(&v, t)? {
                            Ok(v)
                        } else if code == Code::Isinst {
                            Ok(Value::Null)
                        } else {
                            self.throw("System.InvalidCastException")
                        }
                    }
                    other => bug(format!("{code:?} of {}", other.describe())),
                }
            }
            Transfer::Box(t) => {
                let v = one(&vals)?;
                let vty = self.ty_of_op(t)?;
                if vty == Ty::Ref {
                    return Ok(v);
                }
                let ty = self.type_name(t.token)?;
                let value = vty.store(v).or_else(|e| bug(format!("box {ty}: {e}")))?;
                Ok(self.alloc(Obj::Boxed { ty, vty, value }))
            }
            Transfer::Unbox(t) => {
                let v = one(&vals)?;
                let o = match v {
                    Value::Null => return self.throw("System.NullReferenceException"),
                    Value::Ref(o) => o,
                    other => return bug(format!("unbox of {}", other.describe())),
                };
                let want = self.type_name(t.token)?;
                let Obj::Boxed { ty, value, .. } = &self.heap[o] else {
                    return self.throw("System.InvalidCastException");
                };
                if *ty != want {
                    return self.throw("System.InvalidCastException");
                }
                Ok(if code == Code::Unbox {
                    Value::Ptr(Loc {
                        base: Base::Boxed(o),
                        path: Vec::new(),
                    })
                } else {
                    value.clone()
                })
            }
            Transfer::Derived(op) => {
                if matches!(
                    op,
                    Op::MkRefAny(_) | Op::RefAnyType | Op::RefAnyVal(_) | Op::Opaque
                ) {
                    return unsupported(format!("{op}"));
                }
                let r = match vals.as_slice() {
                    [a] => value::unary(op, a, self.uint64_double_via_signed),
                    [a, b] => value::binary(op, a, b),
                    _ => return bug(format!("{op} of {} values", vals.len())),
                };
                r.or_else(|f| self.fault(f))
            }
        }
    }

    /// The element `indices` select in the array `base`: its index in the
    /// heap and in the items.
    fn element(&mut self, base: &Value, indices: &[Value]) -> R<(usize, usize)> {
        let a = match base {
            Value::Null => return self.throw("System.NullReferenceException"),
            Value::Ref(a) => *a,
            other => return bug(format!("element of {}", other.describe())),
        };
        let Obj::Array { dims, .. } = &self.heap[a] else {
            return bug("element of an object that is not an array");
        };
        if dims.len() != indices.len() {
            return bug(format!(
                "{} indices into a {}-dimensional array",
                indices.len(),
                dims.len()
            ));
        }
        let mut flat = 0usize;
        let mut out_of_range = false;
        for (i, d) in indices.iter().zip(dims.clone()) {
            let i = match i {
                Value::I4(x) => *x as i64,
                Value::I(x) => *x,
                other => return bug(format!("index {}", other.describe())),
            };
            if i < 0 || i as usize >= d {
                out_of_range = true;
            }
            flat = flat * d + i.max(0) as usize;
        }
        if out_of_range {
            return self.throw("System.IndexOutOfRangeException");
        }
        Ok((a, flat))
    }

    /// The value at a path, and the shadow of the location it is read from (a load takes
    /// the loaded location's labels; statics carry none).
    fn load(&mut self, fi: usize, p: &Path<Var>) -> R<(Value, Sh)> {
        let (base, base_sh) = match p.root {
            Root::Static => {
                let Field::Named(f) = &p.field else {
                    return bug("a static root without a field");
                };
                self.ensure_static(f.token.raw())?;
                return match self.statics.get(&f.token.raw()) {
                    Some(v) => Ok((v.clone(), Sh::default())),
                    None => unsupported(format!("static {:08x}", f.token.raw())),
                };
            }
            Root::Slot(v) => (self.read(fi, v)?, self.sread(fi, v)),
        };
        match &p.field {
            Field::Named(f) => {
                let f = f.token.raw();
                match base {
                    Value::Null => self.throw("System.NullReferenceException"),
                    Value::Struct(s) => match s.fields.iter().find(|(t, _)| *t == f) {
                        Some((_, v)) => Ok((v.clone(), base_sh.field(f))),
                        None => bug(format!("struct {:08x} has no field {f:08x}", s.ty)),
                    },
                    other => {
                        let loc = self.field_loc(other, f)?;
                        Ok((self.read_loc(&loc)?, self.sread_loc(&loc)))
                    }
                }
            }
            Field::Element { indices } => {
                if indices.is_empty() {
                    return self.deref(base);
                }
                let mut idx = Vec::with_capacity(indices.len());
                for i in indices {
                    idx.push(self.read(fi, *i)?);
                }
                let (a, i) = self.element(&base, &idx)?;
                let Obj::Array { items, .. } = &self.heap[a] else {
                    unreachable!("element checked the array")
                };
                let sh = match &self.sheap[a] {
                    SObj::Array { items, .. } => items.get(i).cloned().unwrap_or_default(),
                    _ => Sh::default(),
                };
                Ok((items[i].clone(), sh))
            }
            Field::Length => match base {
                Value::Null => self.throw("System.NullReferenceException"),
                Value::Ref(a) => match (&self.heap[a], &self.sheap[a]) {
                    (Obj::Array { items, .. }, SObj::Array { length, .. }) => {
                        Ok((Value::I(items.len() as i64), Sh::of(length.clone())))
                    }
                    _ => bug("ldlen of an object that is not an array"),
                },
                other => bug(format!("ldlen of {}", other.describe())),
            },
            Field::Deref => self.deref(base),
            Field::Block { .. } => unsupported("block copies"),
        }
    }

    fn deref(&mut self, base: Value) -> R<(Value, Sh)> {
        match base {
            Value::Ptr(loc) => Ok((self.read_loc(&loc)?, self.sread_loc(&loc))),
            Value::Null => self.throw("System.NullReferenceException"),
            other => unsupported(format!("dereference of {}", other.describe())),
        }
    }

    /// The location of field `f` of the object or struct `base` points at.
    fn field_loc(&mut self, base: Value, f: u32) -> R<Loc> {
        match base {
            Value::Null => self.throw("System.NullReferenceException"),
            Value::Ref(o) => match &self.heap[o] {
                Obj::Object { fields, .. } if fields.iter().any(|(t, _)| *t == f) => Ok(Loc {
                    base: Base::Field(o, f),
                    path: Vec::new(),
                }),
                Obj::Object { ty, .. } => bug(format!("{ty} has no field {f:08x}")),
                _ => bug(format!("field {f:08x} of an array or box")),
            },
            Value::Ptr(mut loc) => {
                loc.path.push(f);
                Ok(loc)
            }
            other => bug(format!("field {f:08x} of {}", other.describe())),
        }
    }

    /// Stores a value and its shadow at a path (a store gives the location the stored
    /// value's labels).
    fn store(&mut self, fi: usize, p: &Path<Var>, v: Value, sh: Sh) -> R<()> {
        let base = match p.root {
            Root::Static => {
                let Field::Named(f) = &p.field else {
                    return bug("a static root without a field");
                };
                self.ensure_static(f.token.raw())?;
                let loc = Loc {
                    base: Base::Static(f.token.raw()),
                    path: Vec::new(),
                };
                return self.write_loc(&loc, v);
            }
            Root::Slot(r) => self.read(fi, r)?,
        };
        let loc = match &p.field {
            Field::Named(f) => self.field_loc(base, f.token.raw())?,
            Field::Element { indices } if indices.is_empty() => match base {
                Value::Ptr(loc) => loc,
                Value::Null => return self.throw("System.NullReferenceException"),
                other => return bug(format!("store through {}", other.describe())),
            },
            Field::Element { indices } => {
                let mut idx = Vec::with_capacity(indices.len());
                for i in indices {
                    idx.push(self.read(fi, *i)?);
                }
                let (a, i) = self.element(&base, &idx)?;
                Loc {
                    base: Base::Elem(a, i),
                    path: Vec::new(),
                }
            }
            Field::Deref => match base {
                Value::Ptr(loc) => loc,
                Value::Null => return self.throw("System.NullReferenceException"),
                other => return unsupported(format!("store through {}", other.describe())),
            },
            Field::Length => return bug("a store to an array's length"),
            Field::Block { .. } => return unsupported("block stores"),
        };
        self.write_loc(&loc, v)?;
        self.swrite_loc(&loc, sh)
    }

    /// Initializes the statics of a compiler-generated type (`<>c`, `<>O`: the caches of
    /// lambdas and method groups, and `<>c.<>9`, the instance non-capturing lambdas bind)
    /// the first time a trial touches one, as .NET does: every static field at its default,
    /// then the type's static constructor. FlowRun leaves these types alone.
    fn ensure_static(&mut self, f: u32) -> R<()> {
        if self.statics.contains_key(&f) {
            return Ok(());
        }
        let Ok(Some(Entity::Field(fd))) = self.module.lookup_token(MetadataToken(f)) else {
            return Ok(());
        };
        let Some(t) = fd.declaring_type().ok().flatten() else {
            return Ok(());
        };
        let generated = t
            .full_name()
            .is_ok_and(|n| n.split('/').any(|part| part.starts_with('<')));
        if !generated || !self.generated.insert(t.token().raw()) {
            return Ok(());
        }
        let mut cctor = None;
        for field in t.fields().or_else(|e| bug(e.to_string()))? {
            if field.is_static().unwrap_or(false) && !field.is_literal().unwrap_or(false) {
                let token = field.token().raw();
                let ty = self.field_ty(token)?;
                let v = self.default_of(&ty)?;
                self.statics.insert(token, v);
            }
        }
        for m in t.methods().or_else(|e| bug(e.to_string()))? {
            if m.name().ok().as_deref() == Some(".cctor") {
                cctor = Some(m.token().raw());
            }
        }
        if let Some(c) = cctor {
            self.call(c, Vec::new())?;
        }
        Ok(())
    }

    /// The shadow at the base of a location, if it has one: statics have none.
    fn sbase_mut(&mut self, base: &Base) -> Option<&mut Sh> {
        match base {
            Base::Var { frame, var } => {
                Some(self.frames.get_mut(*frame)?.svars.entry(*var).or_default())
            }
            Base::Static(_) => None,
            Base::Field(o, f) => match self.sheap.get_mut(*o)? {
                SObj::Object(fields) => Some(fields.entry(*f).or_default()),
                _ => None,
            },
            Base::Elem(a, i) => match self.sheap.get_mut(*a)? {
                SObj::Array { items, .. } => items.get_mut(*i),
                _ => None,
            },
            Base::Boxed(o) => match self.sheap.get_mut(*o)? {
                SObj::Boxed(c) => Some(c),
                _ => None,
            },
            Base::Cell(k) => self.scells.get_mut(*k),
        }
    }

    /// `newobj D::.ctor(target, fnptr)`: the delegate `New` made binds them.
    fn delegate_ctor(&mut self, fi: usize, site: &CallSite<Var>, target: Var, fnptr: Var) -> R<()> {
        let Some(r) = site.receiver else {
            return bug("a delegate constructor with no receiver");
        };
        let Value::Ref(o) = self.read(fi, r)? else {
            return bug("a delegate constructor on something New did not make");
        };
        let Value::Fn(method) = self.read(fi, fnptr)? else {
            return unsupported("a delegate over a function pointer not from ldftn");
        };
        let target_value = self.read(fi, target)?;
        if target_value == Value::Null && !self.method_def(method)?.is_static().unwrap_or(true) {
            // .NET refuses to bind an instance method to null, at the constructor.
            return self.throw("System.ArgumentException");
        }
        let target_sh = self.sread(fi, target);
        let ty = match &self.heap[o] {
            Obj::Object { ty, .. } => ty.clone(),
            _ => return bug("a delegate constructor on an array or box"),
        };
        self.heap[o] = Obj::Delegate {
            ty,
            target: target_value,
            method,
        };
        self.sheap[o] = SObj::Delegate(target_sh);
        Ok(())
    }

    /// `callvirt D::Invoke(args)`: the bound method, with the target first when it has `this`.
    fn delegate_invoke(&mut self, fi: usize, site: &CallSite<Var>) -> R<()> {
        let oracle = ModuleTypes::new(self.module);
        let name = site.method.as_ref().and_then(|m| oracle.method_name(m));
        if name.as_deref() != Some("Invoke") {
            return unsupported(format!("delegate {}", name.unwrap_or_default()));
        }
        let Some(r) = site.receiver else {
            return bug("a delegate invocation with no receiver");
        };
        let (target, method, target_sh) = match self.read(fi, r)? {
            Value::Null => return self.throw("System.NullReferenceException"),
            Value::Ref(o) => match (&self.heap[o], &self.sheap[o]) {
                (Obj::Delegate { target, method, .. }, SObj::Delegate(sh)) => {
                    (target.clone(), *method, sh.clone())
                }
                _ => return bug("Invoke on an object that is not a delegate"),
            },
            other => return bug(format!("Invoke on {}", other.describe())),
        };
        let mut args = Vec::with_capacity(site.args.len() + 1);
        let mut shs = Vec::with_capacity(site.args.len() + 1);
        if !self.method_def(method)?.is_static().unwrap_or(true) {
            args.push(target);
            shs.push(target_sh);
        }
        for a in &site.args {
            args.push(self.read(fi, *a)?);
            shs.push(self.sread(fi, *a));
        }
        let result = self.call_sh(method, args, shs);
        self.finish_call(fi, site, result)
    }

    /// Writes a call's result, or its exception, where the call site says.
    fn finish_call(
        &mut self,
        fi: usize,
        site: &CallSite<Var>,
        result: R<(Option<Value>, Sh)>,
    ) -> R<()> {
        match result {
            Ok((ret, sh)) => {
                if let Some(dst) = site.result {
                    let Some(v) = ret else {
                        return bug("a call result from a void method");
                    };
                    self.write_sh(fi, dst, v, sh)?;
                }
                Ok(())
            }
            Err(Stop::Throw(exc)) => {
                let sh = self.exc_shadow(&exc);
                self.write_sh(fi, site.thrown, exc.clone(), sh)?;
                Err(Stop::Throw(exc))
            }
            Err(e) => Err(e),
        }
    }

    /// The shadow of the value at `loc`.
    fn sread_loc(&mut self, loc: &Loc) -> Sh {
        let Some(mut sh) = self.sbase_mut(&loc.base).map(|s| s.clone()) else {
            return Sh::default();
        };
        for f in &loc.path {
            sh = sh.field(*f);
        }
        sh
    }

    /// Writes the shadow of the value at `loc`.
    fn swrite_loc(&mut self, loc: &Loc, sh: Sh) -> R<()> {
        let Some(mut slot) = self.sbase_mut(&loc.base) else {
            return Ok(());
        };
        for f in &loc.path {
            slot = slot.field_mut(*f);
        }
        *slot = sh;
        Ok(())
    }

    fn place(&mut self, fi: usize, place: &Place<Var>) -> R<Loc> {
        let base = match place {
            Place::Local(n) => Base::Var {
                frame: fi,
                var: Var::Local(*n),
            },
            Place::Arg(n) => Base::Var {
                frame: fi,
                var: Var::Arg(*n),
            },
            Place::Static(f) => Base::Static(f.token.raw()),
            Place::Field(v, f) => {
                let base = self.read(fi, *v)?;
                // A struct held in the variable itself (the place
                // resolve_address names for `ldloca s; ldflda f`).
                if let Value::Struct(_) = base {
                    return Ok(Loc {
                        base: Base::Var { frame: fi, var: *v },
                        path: vec![f.token.raw()],
                    });
                }
                return self.field_loc(base, f.token.raw());
            }
            Place::Element(v, indices) => {
                let base = self.read(fi, *v)?;
                let mut idx = Vec::with_capacity(indices.len());
                for i in indices {
                    idx.push(self.read(fi, *i)?);
                }
                let (a, i) = self.element(&base, &idx)?;
                Base::Elem(a, i)
            }
        };
        Ok(Loc {
            base,
            path: Vec::new(),
        })
    }

    /// The declared type of a location, when it has one (stack variables
    /// do not).
    fn loc_ty(&mut self, loc: &Loc) -> R<Option<Ty>> {
        if let Some(&f) = loc.path.last() {
            return self.field_ty(f).map(Some);
        }
        Ok(match &loc.base {
            Base::Var { frame, var } => {
                let m = &self.frames[*frame].method;
                match var {
                    Var::Arg(n) => m.params.get(*n as usize).cloned(),
                    Var::Local(n) => m.locals.get(*n as usize).cloned(),
                    _ => None,
                }
            }
            Base::Static(f) | Base::Field(_, f) => Some(self.field_ty(*f)?),
            Base::Elem(a, _) => match &self.heap[*a] {
                Obj::Array { elem, .. } => Some(elem.clone()),
                _ => return bug("an element of an object that is not an array"),
            },
            Base::Boxed(o) => match &self.heap[*o] {
                Obj::Boxed { vty, .. } => Some(vty.clone()),
                _ => return bug("the box of an object that is not a box"),
            },
            Base::Cell(k) => Some(self.cells[*k].0.clone()),
        })
    }

    fn base_mut(&mut self, base: &Base) -> R<&mut Value> {
        let slot = match base {
            Base::Var { frame, var } => self
                .frames
                .get_mut(*frame)
                .and_then(|f| f.vars.get_mut(var)),
            Base::Static(f) => self.statics.get_mut(f),
            Base::Field(o, f) => match &mut self.heap[*o] {
                Obj::Object { fields, .. } => {
                    fields.iter_mut().find(|(t, _)| t == f).map(|(_, v)| v)
                }
                _ => None,
            },
            Base::Elem(a, i) => match &mut self.heap[*a] {
                Obj::Array { items, .. } => items.get_mut(*i),
                _ => None,
            },
            Base::Boxed(o) => match &mut self.heap[*o] {
                Obj::Boxed { value, .. } => Some(value),
                _ => None,
            },
            Base::Cell(k) => self.cells.get_mut(*k).map(|(_, v)| v),
        };
        match slot {
            Some(v) => Ok(v),
            None => bug(format!("a pointer to nothing: {base:?}")),
        }
    }

    fn read_loc(&mut self, loc: &Loc) -> R<Value> {
        let mut v = self.base_mut(&loc.base)?.clone();
        for f in &loc.path {
            v = match v {
                Value::Struct(s) => match s.fields.into_iter().find(|(t, _)| t == f) {
                    Some((_, x)) => x,
                    None => return bug(format!("no field {f:08x} along a pointer")),
                },
                other => return bug(format!("field {f:08x} of {}", other.describe())),
            };
        }
        Ok(v)
    }

    fn write_loc(&mut self, loc: &Loc, v: Value) -> R<()> {
        let v = match self.loc_ty(loc)? {
            Some(ty) => ty
                .store(v)
                .or_else(|e| bug(format!("store through {loc:?}: {e}")))?,
            None => v,
        };
        let mut slot = self.base_mut(&loc.base)?;
        for f in &loc.path {
            slot = match slot {
                Value::Struct(s) => match s.fields.iter_mut().find(|(t, _)| t == f) {
                    Some((_, x)) => x,
                    None => return bug(format!("no field {f:08x} along a pointer")),
                },
                other => return bug(format!("field {f:08x} of {}", other.describe())),
            };
        }
        *slot = v;
        Ok(())
    }

    fn new_value(
        &mut self,
        fi: usize,
        code: Code,
        kind: NewKind,
        ty: Option<&TypeOp>,
        sizes: &[Var],
    ) -> R<Value> {
        let Some(t) = ty else {
            return unsupported(format!("{kind:?} with no type"));
        };
        match kind {
            NewKind::Object => {
                let name = self.type_name(t.token)?;
                let fields = match self.types.get(&name) {
                    Some(&def) => {
                        let fields = self.instance_fields(def)?;
                        let mut values = Vec::with_capacity(fields.len());
                        for (f, fty) in fields.iter() {
                            values.push((*f, self.default_of(fty)?));
                        }
                        values
                    }
                    None => Vec::new(),
                };
                Ok(self.alloc(Obj::Object { ty: name, fields }))
            }
            NewKind::ValueType => {
                let ty = self.ty_of_op(t)?;
                self.default_of(&ty)
            }
            NewKind::Array => {
                let mut dims = Vec::with_capacity(sizes.len());
                for s in sizes {
                    let n = match self.read(fi, *s)? {
                        Value::I4(n) => n as i64,
                        Value::I(n) => n,
                        other => return bug(format!("array size {}", other.describe())),
                    };
                    if n < 0 {
                        return self.throw(value::OVERFLOW);
                    }
                    if n > 1 << 20 {
                        return unsupported(format!("an array of {n} elements"));
                    }
                    dims.push(n as usize);
                }
                // newarr names the element type; a multi-dimensional
                // array's constructor names the array type.
                let (elem, name) = if code == Code::Newarr {
                    (self.ty_of_op(t)?, self.type_name(t.token)?)
                } else {
                    let Ok(Some(Entity::TypeSpec(spec))) = self.module.lookup_token(t.token) else {
                        return bug("an array constructor not on a TypeSpec");
                    };
                    let Ok(TypeSig::Array { element, .. }) = spec.signature() else {
                        return bug("an array constructor not on an array type");
                    };
                    let name = self
                        .module
                        .type_sig_full_name(&element, Default::default())
                        .or_else(|e| bug(e.to_string()))?;
                    (self.ty_of_sig(&element)?, name)
                };
                let code = self.elem_code(&elem, &name);
                let len = dims.iter().product();
                let zero = self.default_of(&elem)?;
                Ok(self.alloc(Obj::Array {
                    elem,
                    code,
                    dims,
                    items: vec![zero; len],
                }))
            }
            NewKind::StackAlloc => unsupported("localloc"),
        }
    }

    fn call_site(&mut self, fi: usize, site: &CallSite<Var>) -> R<()> {
        let Some(method) = &site.method else {
            return unsupported("calli");
        };
        match delegate::classify(site, &ModuleTypes::new(self.module)) {
            Some(DelegateOp::Ctor { target, fnptr }) => {
                return self.delegate_ctor(fi, site, target, fnptr);
            }
            Some(DelegateOp::Invoke) => return self.delegate_invoke(fi, site),
            None => {}
        }
        let mut args = Vec::with_capacity(site.args.len() + 1);
        let mut shs = Vec::with_capacity(site.args.len() + 1);
        if let Some(r) = site.receiver {
            args.push(self.read(fi, r)?);
            shs.push(self.sread(fi, r));
        }
        for a in &site.args {
            args.push(self.read(fi, *a)?);
            shs.push(self.sread(fi, *a));
        }
        let token = method.token;
        let result = match token.token_type() {
            Some(TokenType::Method) => {
                let mut target = token.raw();
                match &site.kind {
                    CallKind::Ctor => {
                        // A struct's constructor takes the address of the
                        // value New made.
                        if let (Some(Value::Struct(_)), Some(r)) = (args.first(), site.receiver) {
                            args[0] = Value::Ptr(Loc {
                                base: Base::Var { frame: fi, var: r },
                                path: Vec::new(),
                            });
                            shs[0] = Sh::default();
                        }
                    }
                    CallKind::Virtual { constrained } => match (&args[0], constrained) {
                        (Value::Null, _) => return self.throw("System.NullReferenceException"),
                        (Value::Ref(o), None) => {
                            if self.method_def(target)?.is_virtual().unwrap_or(false) {
                                let runtime = match &self.heap[*o] {
                                    Obj::Object { ty, .. } => ty.clone(),
                                    _ => return unsupported("a virtual call on an array or box"),
                                };
                                target = self.dispatch(target, &runtime)?;
                            }
                        }
                        (Value::Ptr(_), Some(_)) => {
                            let md = self.method_def(target)?;
                            let on_value_type = md
                                .declaring_type()
                                .ok()
                                .flatten()
                                .is_some_and(|t| t.is_value_type().unwrap_or(false));
                            if !on_value_type {
                                return unsupported(
                                    "a constrained call to a reference type's method",
                                );
                            }
                        }
                        (other, _) => {
                            return bug(format!("callvirt on {}", other.describe()));
                        }
                    },
                    CallKind::Static | CallKind::Instance => {}
                    CallKind::Indirect { .. } => return unsupported("calli"),
                }
                self.call_sh(target, args, shs)
            }
            Some(TokenType::MemberRef) => {
                let name = self.member_name(token)?;
                match name.as_str() {
                    "System.Object::.ctor" | "System.Exception::.ctor" => Ok((None, Sh::default())),
                    _ => return unsupported(format!("call of {name}")),
                }
            }
            _ => return unsupported(format!("call of {:08x}", token.raw())),
        };
        self.finish_call(fi, site, result)
    }
}
