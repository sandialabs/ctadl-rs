//! Method bodies: tiny/fat headers, decoded instructions, exception handlers,
//! and the Cecil-test-Formatter-compatible renderer.
//!
//! Port of the read half of `Mono.Cecil.Cil/CodeReader.cs` and
//! `Test/Mono.Cecil.Tests/Formatter.cs`.

use crate::cil::decoder::{Ins, InstructionDecoder, RawOperand};
use crate::error::{Error, Result};
use crate::model::data::ModuleData;
use crate::model::handles::{MemberRefSig, MethodDef};
use crate::model::module::{Entity, lookup_token};
use crate::model::names::{GenericContext, method_params_full_name, sig_full_name};
use crate::model::sig::{SigReader, TypeSig};
use crate::raw::tables::Table;
use crate::token::{MetadataToken, TokenType};
use std::fmt::Write as _;

/// `Mono.Cecil.Cil.ExceptionHandlerType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExceptionHandlerType {
    Catch = 0,
    Filter = 1,
    Finally = 2,
    Fault = 4,
}

/// One exception handler, with instruction boundaries kept as offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionHandler {
    pub handler_type: ExceptionHandlerType,
    pub try_start: u32,
    pub try_end: u32,
    pub handler_start: u32,
    pub handler_end: u32,
    /// Catch handlers only.
    pub catch_type: Option<MetadataToken>,
    /// Filter handlers only.
    pub filter_start: Option<u32>,
}

/// A decoded method body (Cecil `MethodBody`).
pub struct MethodBody<'a> {
    pub(crate) m: &'a ModuleData<'a>,
    pub(crate) method_rid: u32,
    pub max_stack: u32,
    pub init_locals: bool,
    pub code_size: u32,
    pub local_var_token: MetadataToken,
    pub variables: Vec<TypeSig>,
    pub instructions: Vec<Ins<'a>>,
    /// Where decoding stopped, when the code has an instruction that does
    /// not decode, such as a byte past the end of the opcode tables: its
    /// offset, and why. `instructions` holds the ones before it, and the
    /// locals and exception handlers are read as usual. Cecil fails the
    /// whole body there.
    pub decode_error: Option<(u32, Error)>,
    pub exception_handlers: Vec<ExceptionHandler>,
}

impl<'a> MethodDef<'a> {
    /// Decodes the method body (Cecil `MethodDefinition.Body`, decoded on
    /// first access — here decoded per call).
    pub fn body(&self) -> Result<MethodBody<'a>> {
        let raw = self.raw()?;
        read_body(self.m, self.rid, raw.rva)
    }
}

fn read_body<'a>(m: &'a ModuleData<'a>, method_rid: u32, rva: u32) -> Result<MethodBody<'a>> {
    let data = m.image.data();
    let start = m.image.resolve_rva(rva)?;
    let mut cur = crate::raw::cursor::Cur::at(data, start);

    let mut body = MethodBody {
        m,
        method_rid,
        max_stack: 0,
        init_locals: false,
        code_size: 0,
        local_var_token: MetadataToken::ZERO,
        variables: Vec::new(),
        instructions: Vec::new(),
        decode_error: None,
        exception_handlers: Vec::new(),
    };

    let flags = cur.u8()?;
    match flags & 0x3 {
        0x2 => {
            // tiny
            body.code_size = (flags >> 2) as u32;
            body.max_stack = 8;
            read_code(&mut body, &mut cur);
        }
        0x3 => {
            // fat
            cur.set_pos(cur.pos() - 1);
            let fat_flags = cur.u16()?;
            body.max_stack = cur.u16()? as u32;
            body.code_size = cur.u32()?;
            body.local_var_token = MetadataToken(cur.u32()?);
            body.init_locals = fat_flags & 0x10 != 0;

            if body.local_var_token.rid() != 0 {
                if body.local_var_token.token_type() != Some(TokenType::Signature) {
                    return Err(Error::bad(cur.pos(), "bad local variable token"));
                }
                let row =
                    m.md.row::<crate::raw::tables::StandAloneSigRaw>(body.local_var_token.rid())?;
                let blob = m.md.blobs().view(row.signature)?;
                body.variables = SigReader::new(blob).local_var_sig()?;
            }

            read_code(&mut body, &mut cur);

            if fat_flags & 0x8 != 0 {
                read_sections(&mut body.exception_handlers, &mut cur)?;
            }
        }
        _ => return Err(Error::bad(start, "invalid method body header")),
    }
    Ok(body)
}

fn read_code<'a>(body: &mut MethodBody<'a>, cur: &mut crate::raw::cursor::Cur<'a>) {
    let data = cur.data();
    let start = cur.pos();
    // CodeReader.ReadCode: clamp out-of-range code sizes to zero.
    let mut code_size = body.code_size as usize;
    if start + code_size > data.len() {
        code_size = 0;
    }
    let code = &data[start..start + code_size];
    let mut decoder = InstructionDecoder::new(code);
    loop {
        let offset = decoder.offset();
        match decoder.next() {
            None => break,
            Some(Ok(ins)) => body.instructions.push(ins),
            Some(Err(e)) => {
                body.decode_error = Some((offset, e));
                break;
            }
        }
    }
    cur.set_pos(start + code_size);
}

/// Reads the data sections that follow the code. Cecil's `ReadSection`
/// recurses once per MoreSects section; this loops, with the same result.
fn read_sections(
    handlers: &mut Vec<ExceptionHandler>,
    cur: &mut crate::raw::cursor::Cur<'_>,
) -> Result<()> {
    const FAT_FORMAT: u8 = 0x40;
    const MORE_SECTS: u8 = 0x80;

    loop {
        // CodeReader.ReadSection: align to a 4-byte file-position boundary.
        cur.set_pos((cur.pos() + 3) & !3);

        let flags = cur.u8()?;
        if flags & FAT_FORMAT == 0 {
            // small: count byte / 12, then 2 reserved bytes
            let count = (cur.u8()? / 12) as usize;
            cur.skip(2)?;
            for _ in 0..count {
                let ty = cur.u16()? & 0x7;
                let try_start = cur.u16()? as u32;
                let try_len = cur.u8()? as u32;
                let handler_start = cur.u16()? as u32;
                let handler_len = cur.u8()? as u32;
                read_handler(
                    handlers,
                    cur,
                    ty as u32,
                    try_start,
                    try_len,
                    handler_start,
                    handler_len,
                )?;
            }
        } else {
            cur.set_pos(cur.pos() - 1);
            // Cecil reads the flags and the 24-bit size as one int32, so a
            // size of 0x800000 or more gives a negative count, and no
            // clauses.
            let count = ((cur.i32()? >> 8) / 24).max(0) as usize;
            for _ in 0..count {
                let ty = cur.u32()? & 0x7;
                let try_start = cur.u32()?;
                let try_len = cur.u32()?;
                let handler_start = cur.u32()?;
                let handler_len = cur.u32()?;
                read_handler(
                    handlers,
                    cur,
                    ty,
                    try_start,
                    try_len,
                    handler_start,
                    handler_len,
                )?;
            }
        }

        if flags & MORE_SECTS == 0 {
            return Ok(());
        }
    }
}

fn read_handler(
    handlers: &mut Vec<ExceptionHandler>,
    cur: &mut crate::raw::cursor::Cur<'_>,
    ty: u32,
    try_start: u32,
    try_len: u32,
    handler_start: u32,
    handler_len: u32,
) -> Result<()> {
    let handler_type = match ty {
        0 => ExceptionHandlerType::Catch,
        1 => ExceptionHandlerType::Filter,
        2 => ExceptionHandlerType::Finally,
        4 => ExceptionHandlerType::Fault,
        _ => return Err(Error::bad(cur.pos(), "invalid exception handler type")),
    };
    // Cecil adds each length to its start as an unchecked int, so an end
    // past 2^32 wraps.
    let mut handler = ExceptionHandler {
        handler_type,
        try_start,
        try_end: try_start.wrapping_add(try_len),
        handler_start,
        handler_end: handler_start.wrapping_add(handler_len),
        catch_type: None,
        filter_start: None,
    };
    match handler_type {
        ExceptionHandlerType::Catch => handler.catch_type = Some(MetadataToken(cur.u32()?)),
        ExceptionHandlerType::Filter => handler.filter_start = Some(cur.u32()?),
        _ => cur.skip(4)?,
    }
    handlers.push(handler);
    Ok(())
}

impl<'a> MethodBody<'a> {
    pub fn method(&self) -> MethodDef<'a> {
        MethodDef {
            m: self.m,
            rid: self.method_rid,
        }
    }

    fn generic_context(&self) -> Result<GenericContext> {
        self.method().generic_context()
    }

    /// The `Formatter.FormatMethodBody` rendering, tabs and all — the format
    /// Cecil's expected-IL test strings use. A body whose code did not all
    /// decode fails with its `decode_error`, as Cecil has no body to format.
    pub fn formatted(&self) -> Result<String> {
        if let Some((_, e)) = &self.decode_error {
            return Err(e.clone());
        }
        let mut out = String::new();
        let ctx = self.generic_context()?;

        // WriteVariables
        out.push('\t');
        out.push_str(".locals ");
        if self.init_locals {
            out.push_str("init ");
        }
        out.push('(');
        for (i, var) in self.variables.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            sig_full_name(self.m, var, ctx, &mut out)?;
            write!(out, " V_{i}").unwrap();
        }
        out.push_str(")\n");

        for ins in &self.instructions {
            out.push('\t');
            self.write_instruction(ins, ctx, &mut out)?;
            out.push('\n');
        }

        // WriteExceptionHandlers
        for handler in &self.exception_handlers {
            out.push('\t');
            let type_str = match handler.handler_type {
                ExceptionHandlerType::Catch => {
                    let catch_type = handler
                        .catch_type
                        .ok_or(Error::Unsupported("catch without type"))?;
                    let mut s = String::from("catch ");
                    crate::model::names::token_full_name(self.m, catch_type, ctx, &mut s)?;
                    s
                }
                ExceptionHandlerType::Filter => "filter".to_string(),
                ExceptionHandlerType::Finally => "finally".to_string(),
                ExceptionHandlerType::Fault => "fault".to_string(),
            };
            writeln!(
                out,
                ".try {} to {} {} handler {} to {}",
                label(handler.try_start),
                label(handler.try_end),
                type_str,
                label(handler.handler_start),
                label(handler.handler_end),
            )
            .unwrap();
        }

        Ok(out)
    }

    /// `Formatter.FormatInstruction`.
    pub fn format_instruction(&self, ins: &Ins<'a>) -> Result<String> {
        let mut out = String::new();
        self.write_instruction(ins, self.generic_context()?, &mut out)?;
        Ok(out)
    }

    fn write_instruction(
        &self,
        ins: &Ins<'a>,
        ctx: GenericContext,
        out: &mut String,
    ) -> Result<()> {
        out.push_str(&label(ins.offset));
        out.push_str(": ");
        out.push_str(ins.opcode.name());
        // Cecil's ResolveBranches nulls out targets that fall outside the
        // method (MethodBodyTests.BranchOutsideMethod), and the Formatter
        // prints no operand for a null operand.
        if let RawOperand::Target(t) = ins.operand
            && !self.is_instruction_offset(t)
        {
            return Ok(());
        }
        // Likewise for arguments and locals that do not exist, and for
        // tokens Cecil looks up as null.
        if !self.names_arg_or_local(&ins.operand)? || !self.resolves_token(&ins.operand)? {
            return Ok(());
        }
        if !matches!(ins.operand, RawOperand::None) {
            out.push(' ');
            self.write_operand(ins, ctx, out)?;
        }
        Ok(())
    }

    /// False for an argument or local operand past the signature's
    /// parameters or the locals, which Cecil's `MethodBody.GetParameter` and
    /// `GetVariable` resolve to null. With `this`, argument 0 is `this`.
    fn names_arg_or_local(&self, operand: &RawOperand<'_>) -> Result<bool> {
        Ok(match *operand {
            RawOperand::Var(index) => (index as usize) < self.variables.len(),
            RawOperand::Arg(index) => {
                let sig = self.method().signature()?;
                let index = index as usize;
                if sig.has_this {
                    index == 0 || index - 1 < sig.params.len()
                } else {
                    index < sig.params.len()
                }
            }
            _ => true,
        })
    }

    /// False for a token operand that `CodeReader.ReadOperand` reads as
    /// null: one `LookupToken` does not find, or a call site whose rid is
    /// past the StandAloneSig table (`ReadCallSite`).
    fn resolves_token(&self, operand: &RawOperand<'_>) -> Result<bool> {
        Ok(match *operand {
            RawOperand::Token(token) => lookup_token(self.m, token)?.is_some(),
            RawOperand::CallSite(token) => token.rid() <= self.m.md.row_count(Table::StandAloneSig),
            _ => true,
        })
    }

    fn is_instruction_offset(&self, offset: u32) -> bool {
        self.instructions
            .binary_search_by_key(&offset, |i| i.offset)
            .is_ok()
    }

    fn write_operand(&self, ins: &Ins<'a>, ctx: GenericContext, out: &mut String) -> Result<()> {
        match &ins.operand {
            RawOperand::None => {}
            RawOperand::I8(v) => write!(out, "{v}").unwrap(),
            RawOperand::U8(v) => write!(out, "{v}").unwrap(),
            RawOperand::I32(v) => write!(out, "{v}").unwrap(),
            RawOperand::I64(v) => write!(out, "{v}").unwrap(),
            RawOperand::F32(v) => out.push_str(&format_r4(*v)),
            RawOperand::F64(v) => out.push_str(&format_r8(*v)),
            RawOperand::Target(t) => out.push_str(&label(*t)),
            RawOperand::Switch(targets) => {
                out.push('(');
                for (i, t) in targets.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&label(t));
                }
                out.push(')');
            }
            RawOperand::String(token) => {
                let s = self.m.md.user_strings().get(token.rid())?;
                out.push('"');
                out.push_str(&s.to_string_lossy());
                out.push('"');
            }
            RawOperand::Var(index) => {
                // VariableDefinition.ToString: "V_" + index.
                write!(out, "V_{index}").unwrap();
            }
            RawOperand::Arg(index) => {
                // Formatter prints ParameterDefinition.Sequence; see
                // MethodBody.GetParameter + ParameterDefinition.Sequence.
                let sig = self.method().signature()?;
                let seq = if sig.has_this && sig.explicit_this {
                    // explicit this: parameters are addressed directly but the
                    // sequence skips the this slot
                    (*index as i32) - 1
                } else {
                    *index as i32
                };
                write!(out, "{seq}").unwrap();
            }
            RawOperand::CallSite(token) => {
                // CallSite.FullName: "<ret>(<params>)".
                let row = self
                    .m
                    .md
                    .row::<crate::raw::tables::StandAloneSigRaw>(token.rid())?;
                let blob = self.m.md.blobs().view(row.signature)?;
                let sig = SigReader::new(blob).method_sig()?;
                sig_full_name(self.m, &sig.return_type, ctx, out)?;
                method_params_full_name(self.m, &sig, ctx, out)?;
            }
            RawOperand::Token(token) => {
                if let Some(entity) = lookup_token(self.m, *token)? {
                    write_entity(entity, ctx, out)?;
                }
            }
        }
        Ok(())
    }

    /// Resolves a raw operand's tokens against the module, mirroring
    /// `ModuleDefinition.LookupToken` for instruction operands: `None` for a
    /// token Cecil reads as a null operand.
    pub fn resolve_operand(&self, ins: &Ins<'a>) -> Result<Option<Entity<'a>>> {
        match ins.operand {
            RawOperand::Token(token) => lookup_token(self.m, token),
            _ => Ok(None),
        }
    }
}

/// The operand's `FullName`, as the Formatter prints a member or type.
fn write_entity(entity: Entity<'_>, ctx: GenericContext, out: &mut String) -> Result<()> {
    match entity {
        Entity::TypeDef(t) => crate::model::names::typedef_full_name(t.m, t.rid, out)?,
        Entity::TypeRef(t) => crate::model::names::typeref_full_name(t.m, t.rid, out)?,
        Entity::TypeSpec(t) => out.push_str(&t.full_name_ctx(ctx)?),
        Entity::Field(f) => out.push_str(&f.full_name()?),
        Entity::Method(m) => out.push_str(&m.full_name()?),
        Entity::MemberRef(m) => out.push_str(&m.full_name_with_outer(ctx)?),
        Entity::MethodSpec(m) => out.push_str(&m.full_name_with_outer(ctx)?),
    }
    Ok(())
}

/// `Formatter.FormatLabel`.
fn label(offset: u32) -> String {
    format!("IL_{offset:04x}")
}

/// `float.ToString(CultureInfo.InvariantCulture)`, as the Formatter prints
/// an `ldc.r4` operand.
fn format_r4(v: f32) -> String {
    format_float(
        v.is_nan(),
        v.is_sign_negative(),
        v.is_infinite(),
        &format!("{:e}", v.abs()),
        9,
    )
}

/// `double.ToString(CultureInfo.InvariantCulture)`, as the Formatter prints
/// an `ldc.r8` operand.
fn format_r8(v: f64) -> String {
    format_float(
        v.is_nan(),
        v.is_sign_negative(),
        v.is_infinite(),
        &format!("{:e}", v.abs()),
        17,
    )
}

/// .NET's general format with the shortest round-trip digits
/// (`Number.FormatFloat` / `FormatGeneral`): `abs` is the magnitude in Rust's
/// exponent form, whose digits are the same shortest round-trip ones.
/// Exponent form is used when the decimal point would sit more than
/// `round_trip_digits` (or the digit count, if greater) places right of
/// the first digit, or more than 3 places left of it; the exponent has a
/// sign and at least two digits. Negative zero keeps its sign.
fn format_float(
    nan: bool,
    negative: bool,
    infinite: bool,
    abs: &str,
    round_trip_digits: usize,
) -> String {
    if nan {
        return "NaN".to_string();
    }
    let sign = if negative { "-" } else { "" };
    if infinite {
        return format!("{sign}Infinity");
    }
    let (mantissa, exponent) = abs.split_once('e').expect("Rust's {:e} has an exponent");
    let digits: String = mantissa.chars().filter(|&c| c != '.').collect();
    let exponent: i32 = exponent
        .parse()
        .expect("Rust's {:e} exponent is an integer");
    if digits == "0" {
        return format!("{sign}0");
    }
    // The value is 0.<digits> × 10^scale.
    let scale = exponent + 1;
    let count = digits.len() as i32;
    if scale > count.max(round_trip_digits as i32) || scale < -3 {
        let (first, rest) = digits.split_at(1);
        let point = if rest.is_empty() { "" } else { "." };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!(
            "{sign}{first}{point}{rest}E{exponent_sign}{:02}",
            exponent.abs()
        );
    }
    if scale <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat(-scale as usize))
    } else if scale >= count {
        format!("{sign}{digits}{}", "0".repeat((scale - count) as usize))
    } else {
        let (whole, fraction) = digits.split_at(scale as usize);
        format!("{sign}{whole}.{fraction}")
    }
}

// Add the outer-context-aware full names used by operand rendering.
impl<'a> crate::model::handles::MemberRef<'a> {
    /// `FullName` with the *using* context supplied for the declaring type's
    /// signature (Cecil resolves TypeSpec Vars against the reading context).
    pub fn full_name_with_outer(&self, outer: GenericContext) -> Result<String> {
        let ctx = self.sig_context(outer)?;
        let mut out = String::new();
        match self.signature()? {
            MemberRefSig::Field(ty) => {
                sig_full_name(self.m, &ty, ctx, &mut out)?;
                out.push(' ');
                self.declaring_full_name(outer, &mut out)?;
                out.push_str("::");
                out.push_str(&self.name()?);
            }
            MemberRefSig::Method(sig) => {
                sig_full_name(self.m, &sig.return_type, ctx, &mut out)?;
                out.push(' ');
                self.declaring_full_name(outer, &mut out)?;
                out.push_str("::");
                out.push_str(&self.name()?);
                method_params_full_name(self.m, &sig, ctx, &mut out)?;
            }
        }
        Ok(out)
    }
}

impl<'a> crate::model::handles::MethodSpec<'a> {
    /// `GenericInstanceMethod.FullName` with the instantiation arguments
    /// rendered against the *using* context.
    pub fn full_name_with_outer(&self, outer: GenericContext) -> Result<String> {
        let method = self.method_token()?;
        let args = self.generic_args()?;
        let mut out = String::new();
        match method.token_type() {
            Some(TokenType::Method) => {
                let m = MethodDef {
                    m: self.m,
                    rid: method.rid(),
                };
                let ctx = m.generic_context()?;
                let sig = m.signature()?;
                sig_full_name(self.m, &sig.return_type, ctx, &mut out)?;
                out.push(' ');
                if let Some(decl) = m.declaring_type()? {
                    crate::model::names::typedef_full_name(self.m, decl.rid(), &mut out)?;
                    out.push_str("::");
                }
                out.push_str(&m.name()?);
                write_generic_args(self.m, &args, outer, &mut out)?;
                method_params_full_name(self.m, &sig, ctx, &mut out)?;
            }
            Some(TokenType::MemberRef) => {
                let mr = crate::model::handles::MemberRef {
                    m: self.m,
                    rid: method.rid(),
                };
                let ctx = mr.generic_context()?;
                let MemberRefSig::Method(sig) = mr.signature()? else {
                    return Err(Error::Unsupported("MethodSpec over a field MemberRef"));
                };
                sig_full_name(self.m, &sig.return_type, ctx, &mut out)?;
                out.push(' ');
                mr.declaring_full_name(outer, &mut out)?;
                out.push_str("::");
                out.push_str(&mr.name()?);
                write_generic_args(self.m, &args, outer, &mut out)?;
                method_params_full_name(self.m, &sig, ctx, &mut out)?;
            }
            _ => return Err(Error::Unsupported("bad MethodSpec method token")),
        }
        Ok(out)
    }
}

fn write_generic_args(
    m: &ModuleData<'_>,
    args: &[TypeSig],
    ctx: GenericContext,
    out: &mut String,
) -> Result<()> {
    out.push('<');
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        sig_full_name(m, a, ctx, out)?;
    }
    out.push('>');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{format_r4, format_r8, read_sections};
    use crate::raw::cursor::Cur;

    // Each value as .NET 8 prints it (CecilDump on gap-formatter.il.dll).
    #[test]
    fn floats_print_as_dotnet_does() {
        let singles: &[(u32, &str)] = &[
            (0x3dcccccd, "0.1"),
            (0x80000000, "-0"),
            (0x3fc00000, "1.5"),
            (0x3727c5ac, "1E-05"),
            (0x38d1b717, "0.0001"),
            (0x4b189680, "10000000"),
            (0x4cbebc20, "100000000"),
            (0x4b800000, "16777216"),
            (0x4ceb79a3, "123456790"),
            (0x7f7fffff, "3.4028235E+38"),
            (0x00800000, "1.1754944E-38"),
            (0x00000001, "1E-45"),
            (0x7fc00000, "NaN"),
            (0xffc00000, "NaN"),
            (0x7f800000, "Infinity"),
            (0xff800000, "-Infinity"),
        ];
        for &(bits, expected) in singles {
            assert_eq!(format_r4(f32::from_bits(bits)), expected, "r4 0x{bits:08x}");
        }
        let doubles: &[(u64, &str)] = &[
            (0x3fb999999999999a, "0.1"),
            (0x8000000000000000, "-0"),
            (0xbff4000000000000, "-1.25"),
            (0x3ee4f8b588e368f1, "1E-05"),
            (0x3f1a36e2eb1c432d, "0.0001"),
            (0x3f202c9dedbc309d, "0.0001234"),
            (0x42d6bcc41e900000, "100000000000000"),
            (0x430c6bf526340000, "1000000000000000"),
            (0x4341c37937e08000, "10000000000000000"),
            (0x4376345785d8a000, "1E+17"),
            (0x437b69b4ba630f35, "1.2345678901234568E+17"),
            (0x4340000000000000, "9007199254740992"),
            (0x7fefffffffffffff, "1.7976931348623157E+308"),
            (0x0010000000000000, "2.2250738585072014E-308"),
            (0x0000000000000001, "5E-324"),
            (0x3fd3333333333334, "0.30000000000000004"),
            (0x7ff8000000000000, "NaN"),
            (0x7ff0000000000000, "Infinity"),
            (0xfff0000000000000, "-Infinity"),
        ];
        for &(bits, expected) in doubles {
            assert_eq!(
                format_r8(f64::from_bits(bits)),
                expected,
                "r8 0x{bits:016x}"
            );
        }
    }

    // Each MoreSects section is read in turn, however many there are: a
    // chain of 200,000 empty small sections, then one with a clause.
    #[test]
    fn many_sections_read_in_turn() {
        const SECTIONS: usize = 200_000;
        let mut data = [0x80, 0, 0, 0].repeat(SECTIONS);
        data.extend([0x01, 12, 0, 0]);
        data.extend([2, 0, 1, 0, 3, 4, 0, 5, 0, 0, 0, 0]);
        let mut handlers = Vec::new();
        let mut cur = Cur::at(&data, 0);
        read_sections(&mut handlers, &mut cur).unwrap();
        assert_eq!(cur.pos(), data.len());
        assert_eq!(handlers.len(), 1);
        let h = &handlers[0];
        assert_eq!((h.try_start, h.try_end), (1, 4));
        assert_eq!((h.handler_start, h.handler_end), (4, 9));
    }

    // A fat clause whose start plus length passes 2^32 ends where the sum
    // wraps to.
    #[test]
    fn handler_ends_wrap() {
        let mut data = vec![0x41, 28, 0, 0];
        for field in [2, 0xffff_ffff, 2, 0xffff_fff0, 0x20, 0] {
            data.extend(u32::to_le_bytes(field));
        }
        let mut handlers = Vec::new();
        read_sections(&mut handlers, &mut Cur::at(&data, 0)).unwrap();
        let h = &handlers[0];
        assert_eq!((h.try_start, h.try_end), (0xffff_ffff, 1));
        assert_eq!((h.handler_start, h.handler_end), (0xffff_fff0, 0x10));
    }

    // A fat size of 0x800000 or more reads no clauses, and a MoreSects
    // section after it starts right after its 4-byte header.
    #[test]
    fn negative_fat_sizes_read_no_clauses() {
        for size in [0x80_0000u32, 0xff_ffe8, 0xff_ffff] {
            let mut data = u32::to_le_bytes(0xc1 | size << 8).to_vec();
            data.extend([0x01, 16, 0, 0]);
            data.extend([2, 0, 1, 0, 3, 4, 0, 5, 0, 0, 0, 0]);
            let mut handlers = Vec::new();
            let mut cur = Cur::at(&data, 0);
            read_sections(&mut handlers, &mut cur).unwrap();
            assert_eq!(cur.pos(), data.len());
            assert_eq!(handlers.len(), 1);
            assert_eq!((handlers[0].try_start, handlers[0].try_end), (1, 4));
        }
    }
}
