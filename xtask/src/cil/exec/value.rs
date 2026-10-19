//! Values for the `cil:exec` interpreter: what a location holds, what the
//! evaluation stack holds, and the ECMA-335 arithmetic, comparisons and
//! conversions over them (§III.1.5, Tables III.2–III.8).
//!
//! The stack types are ECMA's, except that floats keep their width: RyuJIT
//! computes `float32 op float32` in single precision, and so does this.
//! A location holds its value already in stack form (a `uint8` field holds an
//! `I4` in 0..=255): `Ty::store` narrows on the way in, so a load is a copy.

use std::collections::{BTreeMap, BTreeSet};

use cil_reader::flow::{CmpOp, ConvTarget, ConvType, Op, Var};

/// A set of taint labels (`cil:taint`): indices into the labeler's table of input locations.
pub type Labels = BTreeSet<u32>;

/// The shadow of a value (`cil:taint`): the labels of the input locations
/// it carries data from, and for a struct value, the shadows of its fields. An object's fields
/// are not here: they are locations of their own, shadowed on the heap.
///
/// The shadow never changes what the interpreter computes. With no labels anywhere, as in
/// `cil:exec`, every shadow is empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sh {
    pub labels: Labels,
    pub fields: BTreeMap<u32, Sh>,
}

impl Sh {
    pub fn of(labels: Labels) -> Sh {
        Sh {
            labels,
            fields: BTreeMap::new(),
        }
    }

    /// The shadow of field `f` of a struct value.
    pub fn field(&self, f: u32) -> Sh {
        self.fields.get(&f).cloned().unwrap_or_default()
    }

    /// The shadow of field `f`, created empty if need be.
    pub fn field_mut(&mut self, f: u32) -> &mut Sh {
        self.fields.entry(f).or_default()
    }

    /// The labels of a value computed from these (`Transfer::Derived`).
    pub fn union<'a>(shs: impl IntoIterator<Item = &'a Sh>) -> Sh {
        let mut labels = Labels::new();
        for s in shs {
            labels.extend(s.labels.iter().copied());
        }
        Sh::of(labels)
    }
}

/// The declared type of a location: a local, an argument, a field, an array
/// element, a box. Decides how a store narrows and how a value renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Bool,
    Char,
    I1,
    U1,
    I2,
    U2,
    I4,
    U4,
    I8,
    U8,
    R4,
    R8,
    I,
    U,
    /// An object reference of any class or array type.
    Ref,
    /// A struct of this module: its TypeDef token.
    Struct(u32),
    /// A managed or unmanaged pointer.
    Ptr,
}

impl Ty {
    /// The element code FlowRun names a primitive element type by.
    pub fn code(&self) -> Option<&'static str> {
        Some(match self {
            Ty::Bool => "bool",
            Ty::Char => "char",
            Ty::I1 => "i1",
            Ty::U1 => "u1",
            Ty::I2 => "i2",
            Ty::U2 => "u2",
            Ty::I4 => "i4",
            Ty::U4 => "u4",
            Ty::I8 => "i8",
            Ty::U8 => "u8",
            Ty::R4 => "r4",
            Ty::R8 => "r8",
            Ty::I => "i",
            Ty::U => "u",
            Ty::Ref | Ty::Struct(_) | Ty::Ptr => return None,
        })
    }

    pub fn from_code(code: &str) -> Option<Ty> {
        [
            Ty::Bool,
            Ty::Char,
            Ty::I1,
            Ty::U1,
            Ty::I2,
            Ty::U2,
            Ty::I4,
            Ty::U4,
            Ty::I8,
            Ty::U8,
            Ty::R4,
            Ty::R8,
            Ty::I,
            Ty::U,
        ]
        .into_iter()
        .find(|t| t.code() == Some(code))
    }

    /// The primitive a `System.*` type name boxes.
    pub fn from_system_name(name: &str) -> Option<Ty> {
        Some(match name {
            "System.Boolean" => Ty::Bool,
            "System.Char" => Ty::Char,
            "System.SByte" => Ty::I1,
            "System.Byte" => Ty::U1,
            "System.Int16" => Ty::I2,
            "System.UInt16" => Ty::U2,
            "System.Int32" => Ty::I4,
            "System.UInt32" => Ty::U4,
            "System.Int64" => Ty::I8,
            "System.UInt64" => Ty::U8,
            "System.Single" => Ty::R4,
            "System.Double" => Ty::R8,
            "System.IntPtr" => Ty::I,
            "System.UIntPtr" => Ty::U,
            _ => return None,
        })
    }

    /// `v` as a location of this type holds it: integers narrowed (ECMA
    /// §III.1.6, implicit truncation on store), floats rounded to the
    /// location's width. A value of the wrong kind is an error: well-formed
    /// code never stores one, so it means a variable was bound wrong.
    pub fn store(&self, v: Value) -> Result<Value, String> {
        let int = |v: &Value| match v {
            Value::I4(x) => Some(*x as i64),
            Value::I8(x) | Value::I(x) => Some(*x),
            _ => None,
        };
        let out = match self {
            Ty::Bool | Ty::U1 => int(&v).map(|x| Value::I4(x as u8 as i32)),
            Ty::I1 => int(&v).map(|x| Value::I4(x as i8 as i32)),
            Ty::Char | Ty::U2 => int(&v).map(|x| Value::I4(x as u16 as i32)),
            Ty::I2 => int(&v).map(|x| Value::I4(x as i16 as i32)),
            Ty::I4 | Ty::U4 => int(&v).map(|x| Value::I4(x as i32)),
            Ty::I8 | Ty::U8 => int(&v).map(Value::I8),
            Ty::I | Ty::U => match v {
                Value::Ptr(_) | Value::Fn(_) => Some(v),
                _ => int(&v).map(Value::I),
            },
            Ty::R4 => match v {
                Value::R4(x) => Some(Value::R4(x)),
                Value::R8(x) => Some(Value::R4(x as f32)),
                _ => None,
            },
            Ty::R8 => match v {
                Value::R4(x) => Some(Value::R8(x as f64)),
                Value::R8(x) => Some(Value::R8(x)),
                _ => None,
            },
            Ty::Ref => matches!(v, Value::Null | Value::Ref(_)).then_some(v),
            Ty::Struct(t) => matches!(&v, Value::Struct(s) if s.ty == *t).then_some(v),
            Ty::Ptr => match v {
                Value::Ptr(_) | Value::I(_) | Value::Null | Value::Fn(_) => Some(v),
                _ => None,
            },
        };
        out.ok_or_else(|| "a value of the wrong kind for its location".to_string())
    }
}

/// A value on the stack or in a location.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    I4(i32),
    I8(i64),
    /// Native int, 64-bit.
    I(i64),
    R4(f32),
    R8(f64),
    Null,
    /// An object: an index into the heap.
    Ref(usize),
    Ptr(Loc),
    Struct(Struct),
    /// A function pointer (`ldftn`, `ldvirtftn`): a MethodDef token of the module.
    Fn(u32),
}

impl Value {
    pub fn describe(&self) -> String {
        match self {
            Value::Struct(s) => format!("struct {:08x}", s.ty),
            other => format!("{other:?}"),
        }
    }
}

/// A struct value. Fields are in token order.
#[derive(Debug, Clone, PartialEq)]
pub struct Struct {
    pub ty: u32,
    pub fields: Vec<(u32, Value)>,
}

/// What a managed pointer points at: a base location and the struct fields
/// below it.
#[derive(Debug, Clone, PartialEq)]
pub struct Loc {
    pub base: Base,
    pub path: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Base {
    /// A variable of the frame at this depth of the call stack.
    Var {
        frame: usize,
        var: Var,
    },
    Static(u32),
    Field(usize, u32),
    Elem(usize, usize),
    /// The value inside a box (`unbox`).
    Boxed(usize),
    /// A `ref`/`out` argument of the method under test.
    Cell(usize),
}

/// Why an operation did not produce a value.
#[derive(Debug, Clone, PartialEq)]
pub enum Fault {
    /// The runtime throws this exception.
    Throw(&'static str),
    /// ECMA leaves the result unspecified (an out-of-range float to integer
    /// conversion, an oversized shift): the trial says nothing.
    Unspecified(String),
    /// Operands no verifiable code has: a binding error.
    Invalid(String),
}

pub const DIVIDE_BY_ZERO: &str = "System.DivideByZeroException";
pub const OVERFLOW: &str = "System.OverflowException";
pub const ARITHMETIC: &str = "System.ArithmeticException";

fn invalid(op: &Op, a: &Value, b: Option<&Value>) -> Fault {
    let b = b.map(|b| format!(", {}", b.describe())).unwrap_or_default();
    Fault::Invalid(format!("{op} of {}{b}", a.describe()))
}

/// Integer operands of a binary operation, in a common type.
enum Ints {
    I4(i32, i32),
    I8(i64, i64),
    I(i64, i64),
}

fn ints(a: &Value, b: &Value) -> Option<Ints> {
    Some(match (a, b) {
        (Value::I4(x), Value::I4(y)) => Ints::I4(*x, *y),
        (Value::I8(x), Value::I8(y)) => Ints::I8(*x, *y),
        (Value::I(x), Value::I(y)) => Ints::I(*x, *y),
        (Value::I4(x), Value::I(y)) => Ints::I(*x as i64, *y),
        (Value::I(x), Value::I4(y)) => Ints::I(*x, *y as i64),
        _ => return None,
    })
}

fn floats(a: &Value, b: &Value) -> Option<(f64, f64, bool)> {
    Some(match (a, b) {
        (Value::R4(x), Value::R4(y)) => (*x as f64, *y as f64, true),
        (Value::R4(x), Value::R8(y)) => (*x as f64, *y, false),
        (Value::R8(x), Value::R4(y)) => (*x, *y as f64, false),
        (Value::R8(x), Value::R8(y)) => (*x, *y, false),
        _ => return None,
    })
}

/// A binary operation of `Transfer::Derived`.
pub fn binary(op: &Op, a: &Value, b: &Value) -> Result<Value, Fault> {
    match op {
        Op::Shl | Op::Shr(_) => return shift(op, a, b),
        Op::Cmp(cmp, flags) => return compare(*cmp, flags.un, a, b).map(|x| Value::I4(x as i32)),
        _ => {}
    }
    if let Some((x, y, single)) = floats(a, b) {
        let r = match op {
            Op::Add(_) => x + y,
            Op::Sub(_) => x - y,
            Op::Mul(_) => x * y,
            Op::Div(_) => x / y,
            Op::Rem(_) => x % y,
            _ => return Err(invalid(op, a, Some(b))),
        };
        // Single precision: the exact result rounded once to float32, which
        // is what a float32 operation gives for + - * / and fmod.
        return Ok(if single {
            Value::R4(if let Op::Rem(_) = op {
                (x as f32) % (y as f32)
            } else {
                r as f32
            })
        } else {
            Value::R8(r)
        });
    }
    let Some(ops) = ints(a, b) else {
        return Err(invalid(op, a, Some(b)));
    };
    match ops {
        Ints::I4(x, y) => int_op32(op, x, y).map(Value::I4),
        Ints::I8(x, y) => int_op64(op, x, y).map(Value::I8),
        Ints::I(x, y) => int_op64(op, x, y).map(Value::I),
    }
}

macro_rules! int_op {
    ($name:ident, $s:ty, $u:ty) => {
        fn $name(op: &Op, x: $s, y: $s) -> Result<$s, Fault> {
            let ovf = || Fault::Throw(OVERFLOW);
            let (ux, uy) = (x as $u, y as $u);
            Ok(match op {
                Op::Add(f) if f.ovf && f.un => ux.checked_add(uy).ok_or_else(ovf)? as $s,
                Op::Add(f) if f.ovf => x.checked_add(y).ok_or_else(ovf)?,
                Op::Add(_) => x.wrapping_add(y),
                Op::Sub(f) if f.ovf && f.un => ux.checked_sub(uy).ok_or_else(ovf)? as $s,
                Op::Sub(f) if f.ovf => x.checked_sub(y).ok_or_else(ovf)?,
                Op::Sub(_) => x.wrapping_sub(y),
                Op::Mul(f) if f.ovf && f.un => ux.checked_mul(uy).ok_or_else(ovf)? as $s,
                Op::Mul(f) if f.ovf => x.checked_mul(y).ok_or_else(ovf)?,
                Op::Mul(_) => x.wrapping_mul(y),
                Op::Div(f) | Op::Rem(f) => {
                    if y == 0 {
                        return Err(Fault::Throw(DIVIDE_BY_ZERO));
                    }
                    let div = matches!(op, Op::Div(_));
                    if f.un {
                        (if div { ux / uy } else { ux % uy }) as $s
                    } else if x == <$s>::MIN && y == -1 {
                        // ECMA: ArithmeticException; .NET throws its
                        // OverflowException subclass.
                        return Err(Fault::Throw(OVERFLOW));
                    } else if div {
                        x / y
                    } else {
                        x % y
                    }
                }
                Op::And => x & y,
                Op::Or => x | y,
                Op::Xor => x ^ y,
                _ => return Err(Fault::Invalid(format!("{op} on integers"))),
            })
        }
    };
}

int_op!(int_op32, i32, u32);
int_op!(int_op64, i64, u64);

fn shift(op: &Op, a: &Value, b: &Value) -> Result<Value, Fault> {
    let amount = match b {
        Value::I4(n) => *n as i64,
        Value::I(n) => *n,
        _ => return Err(invalid(op, a, Some(b))),
    };
    let width = match a {
        Value::I4(_) => 32,
        Value::I8(_) | Value::I(_) => 64,
        _ => return Err(invalid(op, a, Some(b))),
    };
    if !(0..width).contains(&amount) {
        return Err(Fault::Unspecified(
            "a shift by the width or more".to_string(),
        ));
    }
    let n = amount as u32;
    let un = matches!(op, Op::Shr(f) if f.un);
    Ok(match (op, a) {
        (Op::Shl, Value::I4(x)) => Value::I4(x.wrapping_shl(n)),
        (Op::Shl, Value::I8(x)) => Value::I8(x.wrapping_shl(n)),
        (Op::Shl, Value::I(x)) => Value::I(x.wrapping_shl(n)),
        (_, Value::I4(x)) if un => Value::I4(((*x as u32) >> n) as i32),
        (_, Value::I8(x)) if un => Value::I8(((*x as u64) >> n) as i64),
        (_, Value::I(x)) if un => Value::I(((*x as u64) >> n) as i64),
        (_, Value::I4(x)) => Value::I4(x >> n),
        (_, Value::I8(x)) => Value::I8(x >> n),
        (_, Value::I(x)) => Value::I(x >> n),
        _ => unreachable!("width checked above"),
    })
}

/// A unary operation of `Transfer::Derived`: neg, not, conv.
pub fn unary(op: &Op, a: &Value) -> Result<Value, Fault> {
    match (op, a) {
        (Op::Neg, Value::I4(x)) => Ok(Value::I4(x.wrapping_neg())),
        (Op::Neg, Value::I8(x)) => Ok(Value::I8(x.wrapping_neg())),
        (Op::Neg, Value::I(x)) => Ok(Value::I(x.wrapping_neg())),
        (Op::Neg, Value::R4(x)) => Ok(Value::R4(-x)),
        (Op::Neg, Value::R8(x)) => Ok(Value::R8(-x)),
        (Op::Not, Value::I4(x)) => Ok(Value::I4(!x)),
        (Op::Not, Value::I8(x)) => Ok(Value::I8(!x)),
        (Op::Not, Value::I(x)) => Ok(Value::I(!x)),
        (Op::Conv(t), _) => conv(t, a),
        _ => Err(invalid(op, a, None)),
    }
}

/// The range of an integer conversion target, as i128.
fn range(ty: ConvType) -> (i128, i128) {
    match ty {
        ConvType::I1 => (i8::MIN as i128, i8::MAX as i128),
        ConvType::U1 => (0, u8::MAX as i128),
        ConvType::I2 => (i16::MIN as i128, i16::MAX as i128),
        ConvType::U2 => (0, u16::MAX as i128),
        ConvType::I4 => (i32::MIN as i128, i32::MAX as i128),
        ConvType::U4 => (0, u32::MAX as i128),
        ConvType::I8 | ConvType::I => (i64::MIN as i128, i64::MAX as i128),
        ConvType::U8 | ConvType::U => (0, u64::MAX as i128),
        ConvType::R4 | ConvType::R8 | ConvType::R => unreachable!("not an integer target"),
    }
}

/// An integer in range of `ty`, in its stack form.
fn int_result(ty: ConvType, x: i128) -> Value {
    match ty {
        ConvType::I1 => Value::I4(x as i8 as i32),
        ConvType::U1 => Value::I4(x as u8 as i32),
        ConvType::I2 => Value::I4(x as i16 as i32),
        ConvType::U2 => Value::I4(x as u16 as i32),
        ConvType::I4 | ConvType::U4 => Value::I4(x as i32),
        ConvType::I8 | ConvType::U8 => Value::I8(x as i64),
        ConvType::I | ConvType::U => Value::I(x as i64),
        ConvType::R4 | ConvType::R8 | ConvType::R => unreachable!("not an integer target"),
    }
}

/// `conv.*`, `conv.ovf.*[.un]` and `conv.r.un` (Tables III.8 and III.9).
pub fn conv(t: &ConvTarget, a: &Value) -> Result<Value, Fault> {
    // The source as an integer, read unsigned for `.un`, or as a float.
    // conv.u8 and conv.u zero-extend an int32 (Table III.8).
    let widen_unsigned = !t.flags.ovf && matches!(t.ty, ConvType::U8 | ConvType::U);
    let source: Result<i128, f64> = match a {
        Value::I4(x) if t.flags.un || widen_unsigned => Ok(*x as u32 as i128),
        Value::I4(x) => Ok(*x as i128),
        Value::I8(x) | Value::I(x) if t.flags.un => Ok(*x as u64 as i128),
        Value::I8(x) | Value::I(x) => Ok(*x as i128),
        Value::R4(x) => Err(*x as f64),
        Value::R8(x) => Err(*x),
        _ => {
            return Err(Fault::Invalid(format!(
                "conv.{:?} of {}",
                t.ty,
                a.describe()
            )));
        }
    };
    match t.ty {
        ConvType::R4 => Ok(Value::R4(match source {
            Ok(x) => x as f32,
            Err(f) => f as f32,
        })),
        ConvType::R8 | ConvType::R => Ok(Value::R8(match source {
            Ok(x) => x as f64,
            Err(f) => match a {
                // conv.r8 of a float32 widens exactly.
                Value::R4(x) => *x as f64,
                _ => f,
            },
        })),
        ty => {
            let (lo, hi) = range(ty);
            let x = match source {
                Ok(x) if t.flags.ovf => {
                    if x < lo || x > hi {
                        return Err(Fault::Throw(OVERFLOW));
                    }
                    x
                }
                // Truncation to the target's width.
                Ok(x) => x,
                Err(f) => {
                    let truncated = f.trunc();
                    let fits = !f.is_nan() && truncated >= lo as f64 && truncated <= hi as f64;
                    if !fits {
                        if t.flags.ovf {
                            return Err(Fault::Throw(OVERFLOW));
                        }
                        return Err(Fault::Unspecified(format!(
                            "conv.{:?} of a float out of range",
                            t.ty
                        )));
                    }
                    truncated as i128
                }
            };
            Ok(int_result(ty, x))
        }
    }
}

/// `ceq`/`cgt`/`clt` and the conditional branches: whether `a op b`.
pub fn compare(op: CmpOp, un: bool, a: &Value, b: &Value) -> Result<bool, Fault> {
    if let Some((x, y, _)) = floats(a, b) {
        let unordered = x.is_nan() || y.is_nan();
        return Ok(match op {
            CmpOp::Eq => x == y,
            CmpOp::Gt => x > y || (un && unordered),
            CmpOp::Lt => x < y || (un && unordered),
        });
    }
    if let Some(ops) = ints(a, b) {
        let (x, y, ux, uy) = match ops {
            Ints::I4(x, y) => (x as i64, y as i64, x as u32 as u64, y as u32 as u64),
            Ints::I8(x, y) | Ints::I(x, y) => (x, y, x as u64, y as u64),
        };
        return Ok(match (op, un) {
            (CmpOp::Eq, _) => x == y,
            (CmpOp::Gt, false) => x > y,
            (CmpOp::Lt, false) => x < y,
            (CmpOp::Gt, true) => ux > uy,
            (CmpOp::Lt, true) => ux < uy,
        });
    }
    // Object references and pointers: equality, and cgt.un against null
    // (`x != null`).
    let is_ref = |v: &Value| {
        matches!(
            v,
            Value::Null | Value::Ref(_) | Value::Ptr(_) | Value::Fn(_)
        )
    };
    if is_ref(a) && is_ref(b) {
        return match op {
            CmpOp::Eq => Ok(a == b),
            CmpOp::Gt if un => Ok(*b == Value::Null && *a != Value::Null),
            CmpOp::Lt if un => Ok(*a == Value::Null && *b != Value::Null),
            _ => Err(Fault::Invalid(format!(
                "{op:?} of {} and {}",
                a.describe(),
                b.describe()
            ))),
        };
    }
    Err(Fault::Invalid(format!(
        "{op:?} of {} and {}",
        a.describe(),
        b.describe()
    )))
}

/// Whether `brtrue` branches on `v`.
pub fn truthy(v: &Value) -> Result<bool, Fault> {
    Ok(match v {
        Value::I4(x) => *x != 0,
        Value::I8(x) | Value::I(x) => *x != 0,
        Value::Null => false,
        Value::Ref(_) | Value::Ptr(_) | Value::Fn(_) => true,
        _ => return Err(Fault::Invalid(format!("brtrue of {}", v.describe()))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cil_reader::flow::OpFlags;

    const PLAIN: OpFlags = OpFlags {
        ovf: false,
        un: false,
    };
    const UN: OpFlags = OpFlags {
        ovf: false,
        un: true,
    };
    const OVF: OpFlags = OpFlags {
        ovf: true,
        un: false,
    };

    #[test]
    fn integer_division_edges() {
        let div = Op::Div(PLAIN);
        assert_eq!(
            binary(&div, &Value::I4(1), &Value::I4(0)),
            Err(Fault::Throw(DIVIDE_BY_ZERO))
        );
        assert_eq!(
            binary(&div, &Value::I4(i32::MIN), &Value::I4(-1)),
            Err(Fault::Throw(OVERFLOW))
        );
        assert_eq!(
            binary(&Op::Div(UN), &Value::I4(-2), &Value::I4(2)),
            Ok(Value::I4(0x7fff_ffff))
        );
        assert_eq!(
            binary(&Op::Rem(PLAIN), &Value::I4(-7), &Value::I4(3)),
            Ok(Value::I4(-1))
        );
    }

    #[test]
    fn overflow_checks() {
        assert_eq!(
            binary(&Op::Add(OVF), &Value::I4(i32::MAX), &Value::I4(1)),
            Err(Fault::Throw(OVERFLOW))
        );
        assert_eq!(
            binary(
                &Op::Sub(OpFlags {
                    ovf: true,
                    un: true
                }),
                &Value::I4(1),
                &Value::I4(2)
            ),
            Err(Fault::Throw(OVERFLOW))
        );
        let to_u1 = ConvTarget {
            ty: ConvType::U1,
            flags: OVF,
        };
        assert_eq!(conv(&to_u1, &Value::I4(256)), Err(Fault::Throw(OVERFLOW)));
        assert_eq!(conv(&to_u1, &Value::I4(255)), Ok(Value::I4(255)));
    }

    #[test]
    fn conversions_truncate_and_extend() {
        let to = |ty| ConvTarget { ty, flags: PLAIN };
        assert_eq!(
            conv(&to(ConvType::I1), &Value::I4(0x1ff)),
            Ok(Value::I4(-1))
        );
        assert_eq!(
            conv(&to(ConvType::U2), &Value::I4(-1)),
            Ok(Value::I4(0xffff))
        );
        assert_eq!(
            conv(&to(ConvType::U8), &Value::I4(-1)),
            Ok(Value::I8(0xffff_ffff))
        );
        assert_eq!(conv(&to(ConvType::I8), &Value::I4(-1)), Ok(Value::I8(-1)));
        assert_eq!(
            conv(&to(ConvType::U), &Value::I4(-2)),
            Ok(Value::I(0xffff_fffe))
        );
        assert_eq!(
            conv(
                &ConvTarget {
                    ty: ConvType::U8,
                    flags: UN
                },
                &Value::I4(-1)
            ),
            Ok(Value::I8(0xffff_ffff))
        );
        assert_eq!(conv(&to(ConvType::I4), &Value::R8(-2.7)), Ok(Value::I4(-2)));
        assert!(matches!(
            conv(&to(ConvType::I4), &Value::R8(f64::NAN)),
            Err(Fault::Unspecified(_))
        ));
    }

    #[test]
    fn single_precision_stays_single() {
        let a = Value::R4(16_777_216.0);
        let b = Value::R4(1.0);
        assert_eq!(binary(&Op::Add(PLAIN), &a, &b), Ok(Value::R4(16_777_216.0)));
    }

    #[test]
    fn comparisons() {
        let nan = Value::R8(f64::NAN);
        assert_eq!(compare(CmpOp::Gt, true, &nan, &Value::R8(0.0)), Ok(true));
        assert_eq!(compare(CmpOp::Gt, false, &nan, &Value::R8(0.0)), Ok(false));
        assert_eq!(
            compare(CmpOp::Lt, true, &Value::I4(-1), &Value::I4(0)),
            Ok(false)
        );
        assert_eq!(
            compare(CmpOp::Gt, true, &Value::Ref(3), &Value::Null),
            Ok(true)
        );
    }

    #[test]
    fn stores_narrow() {
        assert_eq!(Ty::U1.store(Value::I4(300)), Ok(Value::I4(44)));
        assert_eq!(Ty::I2.store(Value::I4(70000)), Ok(Value::I4(4464)));
        assert_eq!(Ty::R4.store(Value::R8(0.1)), Ok(Value::R4(0.1)));
        assert!(Ty::I4.store(Value::Null).is_err());
    }
}
