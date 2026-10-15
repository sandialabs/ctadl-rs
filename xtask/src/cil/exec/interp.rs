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
//! Not modeled, so reported as [`Stop::Unsupported`]: strings, tokens,
//! function pointers, delegates, generics, interfaces, `localloc`, blocks,
//! typed references, and calls outside the module other than the
//! `Object` and `Exception` constructors. Filters run in the first pass of
//! their own frame only: a caller's filter runs after the callee's finally
//! blocks, where the CLR runs it before.

use std::collections::HashMap;
use std::rc::Rc;

use cil_reader::cil::Code;
use cil_reader::flow::{
    BlockId, BoundIns, CallKind, CallSite, CmpOp, Const, Control, Effect, Field, FlowOptions,
    Handler, HandlerKind, MethodFlow, NewKind, Op, Operands, Path, Place, Root, Transfer, TypeOp,
    Var,
};
use cil_reader::model::{MemberRefSig, MethodDef, TypeDef, TypeDefOrRef, TypeSig};
use cil_reader::{Entity, MetadataToken, Module, TokenType};

use super::value::{self, Base, Fault, Loc, Struct, Ty, Value, ARITHMETIC};

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
    conts: Vec<Cont>,
    /// The values the last `Test` effect read.
    tests: Vec<Value>,
    /// What the last `Return` effect returned.
    ret: Option<Option<Value>>,
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
    /// Full names of the module's types.
    types: HashMap<String, u32>,
    methods: HashMap<u32, Rc<Prepared>>,
    field_tys: HashMap<u32, Ty>,
    instance_fields: HashMap<u32, Rc<Vec<(u32, Ty)>>>,
    pub heap: Vec<Obj>,
    pub statics: HashMap<u32, Value>,
    pub cells: Vec<(Ty, Value)>,
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
            types,
            methods: HashMap::new(),
            field_tys: HashMap::new(),
            instance_fields: HashMap::new(),
            heap: Vec::new(),
            statics: HashMap::new(),
            cells: Vec::new(),
            frames: Vec::new(),
            steps: 0,
        }
    }

    /// Forget the state of the last trial.
    pub fn reset(&mut self) {
        self.heap.clear();
        self.statics.clear();
        self.cells.clear();
        self.frames.clear();
        self.steps = 0;
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
            | "System.ArrayTypeMismatchException" => "System.SystemException",
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
            Obj::Object { ty, .. } | Obj::Boxed { ty, .. } => ty.clone(),
            Obj::Array { .. } => {
                return match target.as_str() {
                    "System.Object" | "System.Array" => Ok(true),
                    t if t.ends_with(']') => unsupported("array type tests"),
                    _ => Ok(false),
                };
            }
        };
        loop {
            if name == target {
                return Ok(true);
            }
            match self.parent(&name)? {
                Some(p) => name = p,
                None => return Ok(false),
            }
        }
    }

    // ---- heap ----------------------------------------------------------------

    fn alloc(&mut self, o: Obj) -> Value {
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
        for (k, (a, ty)) in args.into_iter().zip(&p.params).enumerate() {
            let a = ty
                .store(a)
                .or_else(|e| bug(format!("{} argument {k}: {e}", p.name)))?;
            vars.insert(Var::Arg(k as u16), a);
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
            conts: Vec::new(),
            tests: Vec::new(),
            ret: None,
        });
        let fi = self.frames.len() - 1;
        let exit = self.run(fi, 0, 0, false);
        self.frames.pop();
        match exit? {
            Exit::Return(v) => match (&p.ret, v) {
                (Some(ty), Some(v)) => Ok(Some(
                    ty.store(v)
                        .or_else(|e| bug(format!("{} returns: {e}", p.name)))?,
                )),
                (None, None) => Ok(None),
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
        self.frames[fi]
            .vars
            .retain(|v, _| !matches!(v, Var::Stack(d) if *d >= height));
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
        self.frames[fi].vars.insert(Var::Caught(c as u16), exc);
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
                    self.frames[fi]
                        .vars
                        .insert(Var::Caught(h as u16), exc.clone());
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
                    self.frames[fi].vars.insert(Var::Caught(c as u16), exc);
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
                for s in srcs {
                    vals.push(self.read(fi, *s)?);
                }
                let v = self.transfer(code, transfer, vals)?;
                self.write(fi, *dst, v)
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
                self.write(fi, *dst, v)
            }
            Effect::Load { dst, src } => {
                let v = self.load(fi, src)?;
                self.write(fi, *dst, v)
            }
            Effect::Store { dst, src } => {
                let v = self.read(fi, *src)?;
                self.store(fi, dst, v)
            }
            Effect::AddressOf { dst, place } => {
                let loc = self.place(fi, place)?;
                self.write(fi, *dst, Value::Ptr(loc))
            }
            Effect::New {
                dst,
                kind,
                ty,
                sizes,
            } => {
                let v = self.new_value(fi, code, *kind, ty.as_ref(), sizes)?;
                self.write(fi, *dst, v)
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
                self.write_loc(&loc, v)
            }
            Effect::Call(site) => self.call_site(fi, site),
            Effect::Return { src } => {
                let v = match src {
                    Some(s) => Some(self.read(fi, *s)?),
                    None => None,
                };
                self.frames[fi].ret = Some(v);
                Ok(())
            }
            Effect::Throw { src } => {
                let v = self.read(fi, *src)?;
                if v == Value::Null {
                    return self.throw("System.NullReferenceException");
                }
                if !matches!(v, Value::Ref(_)) {
                    return bug(format!("throw of {}", v.describe()));
                }
                self.frames[fi].vars.insert(Var::Thrown, v.clone());
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
            Effect::FunctionPtr { .. } => unsupported("function pointers"),
            Effect::Jump { .. } => unsupported("jmp"),
            Effect::Opaque { reason, .. } => bug(format!("an opaque effect ({reason:?})")),
        }
    }

    fn transfer(&mut self, code: Code, transfer: &Transfer, vals: Vec<Value>) -> R<Value> {
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
                    [a] => value::unary(op, a),
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

    fn load(&mut self, fi: usize, p: &Path<Var>) -> R<Value> {
        let base = match p.root {
            Root::Static => {
                let Field::Named(f) = &p.field else {
                    return bug("a static root without a field");
                };
                return match self.statics.get(&f.token.raw()) {
                    Some(v) => Ok(v.clone()),
                    None => unsupported(format!("static {:08x}", f.token.raw())),
                };
            }
            Root::Slot(v) => self.read(fi, v)?,
        };
        match &p.field {
            Field::Named(f) => {
                let f = f.token.raw();
                match base {
                    Value::Null => self.throw("System.NullReferenceException"),
                    Value::Struct(s) => match s.fields.iter().find(|(t, _)| *t == f) {
                        Some((_, v)) => Ok(v.clone()),
                        None => bug(format!("struct {:08x} has no field {f:08x}", s.ty)),
                    },
                    other => {
                        let loc = self.field_loc(other, f)?;
                        self.read_loc(&loc)
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
                Ok(items[i].clone())
            }
            Field::Length => match base {
                Value::Null => self.throw("System.NullReferenceException"),
                Value::Ref(a) => match &self.heap[a] {
                    Obj::Array { items, .. } => Ok(Value::I(items.len() as i64)),
                    _ => bug("ldlen of an object that is not an array"),
                },
                other => bug(format!("ldlen of {}", other.describe())),
            },
            Field::Deref => self.deref(base),
            Field::Block { .. } => unsupported("block copies"),
        }
    }

    fn deref(&mut self, base: Value) -> R<Value> {
        match base {
            Value::Ptr(loc) => self.read_loc(&loc),
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

    fn store(&mut self, fi: usize, p: &Path<Var>, v: Value) -> R<()> {
        let base = match p.root {
            Root::Static => {
                let Field::Named(f) = &p.field else {
                    return bug("a static root without a field");
                };
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
        self.write_loc(&loc, v)
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
        let mut args = Vec::with_capacity(site.args.len() + 1);
        if let Some(r) = site.receiver {
            args.push(self.read(fi, r)?);
        }
        for a in &site.args {
            args.push(self.read(fi, *a)?);
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
                self.call(target, args)
            }
            Some(TokenType::MemberRef) => {
                let name = self.member_name(token)?;
                match name.as_str() {
                    "System.Object::.ctor" | "System.Exception::.ctor" => Ok(None),
                    _ => return unsupported(format!("call of {name}")),
                }
            }
            _ => return unsupported(format!("call of {:08x}", token.raw())),
        };
        match result {
            Ok(ret) => {
                if let Some(dst) = site.result {
                    let Some(v) = ret else {
                        return bug("a call result from a void method");
                    };
                    self.write(fi, dst, v)?;
                }
                Ok(())
            }
            Err(Stop::Throw(exc)) => {
                self.write(fi, site.thrown, exc.clone())?;
                Err(Stop::Throw(exc))
            }
            Err(e) => Err(e),
        }
    }
}
