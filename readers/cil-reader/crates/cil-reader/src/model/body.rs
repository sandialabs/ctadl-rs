//! Method bodies: tiny/fat headers, decoded instructions, exception handlers,
//! and the Cecil-test-Formatter-compatible renderer.
//!
//! Port of the read half of `Mono.Cecil.Cil/CodeReader.cs` and
//! `Test/Mono.Cecil.Tests/Formatter.cs`.

use crate::cil::decoder::{Ins, InstructionDecoder, RawOperand};
use crate::error::{Error, Result};
use crate::model::data::ModuleData;
use crate::model::handles::{MemberRefSig, MethodDef};
use crate::model::names::{GenericContext, method_params_full_name, sig_full_name};
use crate::model::sig::{SigReader, TypeSig};
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
        exception_handlers: Vec::new(),
    };

    let flags = cur.u8()?;
    match flags & 0x3 {
        0x2 => {
            // tiny
            body.code_size = (flags >> 2) as u32;
            body.max_stack = 8;
            read_code(&mut body, &mut cur)?;
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
                let blob = m.md.blobs().get(row.signature)?;
                body.variables = SigReader::new(blob).local_var_sig()?;
            }

            read_code(&mut body, &mut cur)?;

            if fat_flags & 0x8 != 0 {
                read_sections(&mut body, &mut cur)?;
            }
        }
        _ => return Err(Error::bad(start, "invalid method body header")),
    }
    Ok(body)
}

fn read_code<'a>(body: &mut MethodBody<'a>, cur: &mut crate::raw::cursor::Cur<'a>) -> Result<()> {
    let data = cur.data();
    let start = cur.pos();
    // CodeReader.ReadCode: clamp out-of-range code sizes to zero.
    let mut code_size = body.code_size as usize;
    if start + code_size > data.len() {
        code_size = 0;
    }
    let code = &data[start..start + code_size];
    for ins in InstructionDecoder::new(code) {
        body.instructions.push(ins?);
    }
    cur.set_pos(start + code_size);
    Ok(())
}

fn read_sections<'a>(
    body: &mut MethodBody<'a>,
    cur: &mut crate::raw::cursor::Cur<'a>,
) -> Result<()> {
    // CodeReader.ReadSection: align to a 4-byte file-position boundary.
    cur.set_pos((cur.pos() + 3) & !3);

    const FAT_FORMAT: u8 = 0x40;
    const MORE_SECTS: u8 = 0x80;

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
                body,
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
        let count = ((cur.i32()? >> 8) / 24) as usize;
        for _ in 0..count {
            let ty = cur.u32()? & 0x7;
            let try_start = cur.u32()?;
            let try_len = cur.u32()?;
            let handler_start = cur.u32()?;
            let handler_len = cur.u32()?;
            read_handler(
                body,
                cur,
                ty,
                try_start,
                try_len,
                handler_start,
                handler_len,
            )?;
        }
    }

    if flags & MORE_SECTS != 0 {
        read_sections(body, cur)?;
    }
    Ok(())
}

fn read_handler<'a>(
    body: &mut MethodBody<'a>,
    cur: &mut crate::raw::cursor::Cur<'a>,
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
    let mut handler = ExceptionHandler {
        handler_type,
        try_start,
        try_end: try_start + try_len,
        handler_start,
        handler_end: handler_start + handler_len,
        catch_type: None,
        filter_start: None,
    };
    match handler_type {
        ExceptionHandlerType::Catch => handler.catch_type = Some(MetadataToken(cur.u32()?)),
        ExceptionHandlerType::Filter => handler.filter_start = Some(cur.u32()?),
        _ => cur.skip(4)?,
    }
    body.exception_handlers.push(handler);
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
    /// Cecil's expected-IL test strings use.
    pub fn formatted(&self) -> Result<String> {
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
        if !matches!(ins.operand, RawOperand::None) {
            out.push(' ');
            self.write_operand(ins, ctx, out)?;
        }
        Ok(())
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
            RawOperand::F32(v) => out.push_str(&format_float(*v as f64)),
            RawOperand::F64(v) => out.push_str(&format_float(*v)),
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
                let blob = self.m.md.blobs().get(row.signature)?;
                let sig = SigReader::new(blob).method_sig()?;
                sig_full_name(self.m, &sig.return_type, ctx, out)?;
                method_params_full_name(self.m, &sig, ctx, out)?;
            }
            RawOperand::Token(token) => {
                self.write_token_operand(*token, ctx, out)?;
            }
        }
        Ok(())
    }

    fn write_token_operand(
        &self,
        token: MetadataToken,
        ctx: GenericContext,
        out: &mut String,
    ) -> Result<()> {
        use crate::model::handles::{MemberRef, MethodSpec, TypeSpec};
        match token.token_type() {
            Some(TokenType::TypeDef) => {
                crate::model::names::typedef_full_name(self.m, token.rid(), out)
            }
            Some(TokenType::TypeRef) => {
                crate::model::names::typeref_full_name(self.m, token.rid(), out)
            }
            Some(TokenType::TypeSpec) => {
                let spec = TypeSpec {
                    m: self.m,
                    rid: token.rid(),
                };
                out.push_str(&spec.full_name_ctx(ctx)?);
                Ok(())
            }
            Some(TokenType::Field) => {
                let field = crate::model::handles::FieldDef {
                    m: self.m,
                    rid: token.rid(),
                };
                out.push_str(&field.full_name()?);
                Ok(())
            }
            Some(TokenType::Method) => {
                let method = MethodDef {
                    m: self.m,
                    rid: token.rid(),
                };
                out.push_str(&method.full_name()?);
                Ok(())
            }
            Some(TokenType::MemberRef) => {
                let mr = MemberRef {
                    m: self.m,
                    rid: token.rid(),
                };
                out.push_str(&mr.full_name_with_outer(ctx)?);
                Ok(())
            }
            Some(TokenType::MethodSpec) => {
                let ms = MethodSpec {
                    m: self.m,
                    rid: token.rid(),
                };
                out.push_str(&ms.full_name_with_outer(ctx)?);
                Ok(())
            }
            _ => Err(Error::Unsupported("unsupported token operand")),
        }
    }

    /// Resolves a raw operand's tokens against the module, mirroring
    /// `ModuleDefinition.LookupToken` for instruction operands.
    pub fn resolve_operand(
        &self,
        ins: &Ins<'a>,
    ) -> Result<Option<crate::model::module::Entity<'a>>> {
        match &ins.operand {
            RawOperand::Token(token) => {
                let module_shim = self.m;
                // Reuse the lookup logic through a temporary Module-like path:
                let rid = token.rid();
                if rid == 0 {
                    return Ok(None);
                }
                use crate::model::handles as h;
                use crate::model::module::Entity;
                Ok(match token.token_type() {
                    Some(TokenType::TypeDef) => Some(Entity::TypeDef(h::TypeDef {
                        m: module_shim,
                        rid,
                    })),
                    Some(TokenType::TypeRef) => Some(Entity::TypeRef(h::TypeRef {
                        m: module_shim,
                        rid,
                    })),
                    Some(TokenType::TypeSpec) => Some(Entity::TypeSpec(h::TypeSpec {
                        m: module_shim,
                        rid,
                    })),
                    Some(TokenType::Field) => Some(Entity::Field(h::FieldDef {
                        m: module_shim,
                        rid,
                    })),
                    Some(TokenType::Method) => Some(Entity::Method(h::MethodDef {
                        m: module_shim,
                        rid,
                    })),
                    Some(TokenType::MemberRef) => Some(Entity::MemberRef(h::MemberRef {
                        m: module_shim,
                        rid,
                    })),
                    Some(TokenType::MethodSpec) => Some(Entity::MethodSpec(h::MethodSpec {
                        m: module_shim,
                        rid,
                    })),
                    _ => None,
                })
            }
            _ => Ok(None),
        }
    }
}

/// `Formatter.FormatLabel`.
fn label(offset: u32) -> String {
    format!("IL_{offset:04x}")
}

/// Invariant-culture float formatting close to .NET's shortest round-trip.
fn format_float(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        // .NET prints integral doubles without a decimal point.
        return format!("{}", v as i64);
    }
    format!("{v}")
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
                out.push_str(self.name()?);
            }
            MemberRefSig::Method(sig) => {
                sig_full_name(self.m, &sig.return_type, ctx, &mut out)?;
                out.push(' ');
                self.declaring_full_name(outer, &mut out)?;
                out.push_str("::");
                out.push_str(self.name()?);
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
                out.push_str(m.name()?);
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
                out.push_str(mr.name()?);
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
