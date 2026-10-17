//! The flowy grammar and the syntax tree it produces.
//!
//! The grammar is a PEG, so alternatives are ordered and the first that matches wins. Keywords are
//! not reserved: `return` is an ordinary identifier wherever a keyword cannot appear, which is how
//! a summary names the return port (`return <- a`). A keyword must not run into an identifier, so
//! `returnx;` is a parse error rather than `return x;`.

use std::ops::Range;

use ctadl_ir::mir::ParameterType;

use crate::FlowSpec;

/// A line and column, both 1-based. The column counts characters, not bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

/// Maps byte offsets in a source text to [`Pos`]itions.
pub struct LineIndex<'s> {
    src: &'s str,
    /// The byte offset each line starts at.
    starts: Vec<usize>,
}

impl<'s> LineIndex<'s> {
    pub fn new(src: &'s str) -> Self {
        let starts = std::iter::once(0)
            .chain(src.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Self { src, starts }
    }

    pub fn pos(&self, offset: usize) -> Pos {
        let line = self.starts.partition_point(|&start| start <= offset);
        let start = self.starts[line - 1];
        Pos {
            line,
            col: self.src[start..offset].chars().count() + 1,
        }
    }
}

#[derive(Debug)]
pub enum Def<'s> {
    /// `var x;`, a global variable.
    Var(Ident<'s>),
    Function(FunctionDef<'s>),
}

#[derive(Debug)]
pub struct FunctionDef<'s> {
    pub name: Ident<'s>,
    pub params: Vec<Param<'s>>,
    /// `: n` after the parameters.
    pub return_arity: Option<u8>,
    /// `where summaries [...]`
    pub summaries: Option<Vec<SummaryEntry<'s>>>,
    pub blocks: Vec<Block<'s>>,
}

#[derive(Debug)]
pub struct Param<'s> {
    pub name: Ident<'s>,
    pub ty: ParameterType,
}

/// `dest <- source` or `dest </- source`.
#[derive(Debug)]
pub struct SummaryEntry<'s> {
    pub dest: Path<'s>,
    pub flow: FlowSpec,
    pub source: Path<'s>,
}

#[derive(Debug)]
pub struct Block<'s> {
    pub label: Ident<'s>,
    pub stmts: Vec<Stmt<'s>>,
    pub terminator: Terminator<'s>,
}

#[derive(Debug)]
pub struct Stmt<'s> {
    pub kind: StmtKind<'s>,
    /// Byte range of the statement, from its first token through the `;`.
    pub span: Range<usize>,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum StmtKind<'s> {
    /// `dest = [source := value];`
    Update {
        dest: Path<'s>,
        source: Path<'s>,
        value: Ap<'s>,
    },
    /// `dest = a, b, ...;`
    Assign {
        dest: Path<'s>,
        sources: Vec<Ap<'s>>,
    },
    /// `dest = callee(args);`
    AssignCall {
        dest: Path<'s>,
        callee: Path<'s>,
        args: Vec<Ap<'s>>,
    },
    /// `callee(args);`
    Call { callee: Path<'s>, args: Vec<Ap<'s>> },
}

#[derive(Debug)]
pub struct Terminator<'s> {
    pub kind: TerminatorKind<'s>,
    /// Byte range of the terminator, from its keyword through the `;`.
    pub span: Range<usize>,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum TerminatorKind<'s> {
    Goto(Vec<Ident<'s>>),
    Return(Option<Ap<'s>>),
}

/// An operand.
#[derive(Debug)]
pub enum Ap<'s> {
    /// `ptr<F>`
    FunctionPtr(Ident<'s>),
    Path(Path<'s>),
    Int(i64),
    /// The text between the quotes, with escapes left as written.
    Str(&'s str),
}

/// A variable followed by any number of segments: `x.foo.[8].bar`.
#[derive(Debug)]
pub struct Path<'s> {
    pub base: Ident<'s>,
    pub segments: Vec<Segment<'s>>,
}

#[derive(Debug)]
pub enum Segment<'s> {
    /// `.foo`
    Field(&'s str),
    /// `.[8]`
    Offset(i64),
    /// `.*`, which parses but is not an access-path segment, so lowering rejects it. The position
    /// is the `.`'s.
    Star(Pos),
}

#[derive(Debug)]
pub struct Ident<'s> {
    pub name: &'s str,
    pub pos: Pos,
}

peg::parser! {
    pub grammar flowy<'a>(lines: &LineIndex<'a>) for str {
        pub rule program() -> Vec<Def<'input>> = _ defs:(def() ** _) _ { defs }

        rule def() -> Def<'input>
            = kw("var") _ name:ident() _ ";" { Def::Var(name) }
            / f:function_def() { Def::Function(f) }

        rule function_def() -> FunctionDef<'input>
            = kw("def") _ name:ident() _ params:params() _
              return_arity:(":" _ n:arity() _ { n })?
              summaries:(s:where_clause() _ { s })?
              "{" _ blocks:(block() ** _) _ "}"
            { FunctionDef { name, params, return_arity, summaries, blocks } }

        rule params() -> Vec<Param<'input>> = "(" _ ps:(param() ** (_ "," _)) _ ")" { ps }

        rule param() -> Param<'input>
            = name:ident() ty:(_ t:param_type() { t })?
            { Param { name, ty: ty.unwrap_or(ParameterType::SharedFields) } }

        rule param_type() -> ParameterType
            = kw("shared_fields") { ParameterType::SharedFields }
            / kw("shared") { ParameterType::Shared }

        rule arity() -> u8
            = n:int() {? u8::try_from(n).or(Err("a return arity from 0 to 255")) }

        rule where_clause() -> Vec<SummaryEntry<'input>>
            = kw("where") _ kw("summaries") _
              "[" _ entries:(summary_entry() ** (_ "," _)) _ "]"
            { entries }

        rule summary_entry() -> SummaryEntry<'input>
            = dest:path() _ flow:summary_op() _ source:path()
            { SummaryEntry { dest, flow, source } }

        rule summary_op() -> FlowSpec
            = "<-" { FlowSpec::FlowPresent }
            / "</-" { FlowSpec::FlowAbsent }

        rule block() -> Block<'input>
            = label:ident() _ ":" _ stmts:(stmt() ** _) _ terminator:terminator()
            { Block { label, stmts, terminator } }

        rule stmt() -> Stmt<'input>
            = start:position!() kind:stmt_kind() end:position!()
            { Stmt { kind, span: start..end, pos: lines.pos(start) } }

        // A functional update `x2 = [x1.foo := val];` reads "x2 is x1, but with .foo set to val".
        // The brackets are what distinguish it from the assignment `x2.foo = val`, which writes
        // through x1.
        rule stmt_kind() -> StmtKind<'input>
            = dest:path() _ "=" _ "[" _ source:path() _ ":=" _ value:ap() _ "]" _ ";"
              { StmtKind::Update { dest, source, value } }
            / dest:path() _ "=" _ sources:(ap() ++ (_ "," _)) _ ";"
              { StmtKind::Assign { dest, sources } }
            / dest:path() _ "=" _ callee:path() _ args:actuals() _ ";"
              { StmtKind::AssignCall { dest, callee, args } }
            / callee:path() _ args:actuals() _ ";"
              { StmtKind::Call { callee, args } }

        rule terminator() -> Terminator<'input>
            = start:position!() kind:terminator_kind() end:position!()
            { Terminator { kind, span: start..end, pos: lines.pos(start) } }

        rule terminator_kind() -> TerminatorKind<'input>
            = kw("goto") _ targets:(ident() ++ (_ "," _)) _ ";" { TerminatorKind::Goto(targets) }
            / kw("return") _ value:ap()? _ ";" { TerminatorKind::Return(value) }

        rule actuals() -> Vec<Ap<'input>> = "(" _ args:(ap() ** (_ "," _)) _ ")" { args }

        rule ap() -> Ap<'input>
            = kw("ptr") _ "<" _ f:ident() _ ">" { Ap::FunctionPtr(f) }
            / p:path() { Ap::Path(p) }
            / n:int() { Ap::Int(n) }
            / s:string() { Ap::Str(s) }

        rule path() -> Path<'input>
            = base:ident() segments:(_ s:segment() { s })*
            { Path { base, segments } }

        rule segment() -> Segment<'input>
            = p:position!() "." _ "*" { Segment::Star(lines.pos(p)) }
            / "." _ f:ident() { Segment::Field(f.name) }
            / "." _ "[" _ n:int() _ "]" { Segment::Offset(n) }

        // A keyword is a whole word, so it cannot run into an identifier: `returnx;` is a parse
        // error, not `return x;`.
        rule kw(k: &'static str)
            = quiet!{ w:$(ident_char()+) {? if w == k { Ok(()) } else { Err(k) } } }
            / expected!(k)

        rule ident() -> Ident<'input>
            = quiet!{
                p:position!() name:$(['a'..='z' | 'A'..='Z' | '_'] ident_char()*)
                { Ident { name, pos: lines.pos(p) } }
            }
            / expected!("identifier")

        rule ident_char() = ['a'..='z' | 'A'..='Z' | '0'..='9' | '_']

        rule int() -> i64
            = n:$(quiet!{ ['+' | '-']? ['0'..='9']+ } / expected!("integer"))
            {? n.parse().or(Err("a 64-bit integer")) }

        rule string() -> &'input str = "\"" s:$(string_char()*) "\"" { s }

        rule string_char()
            = !['"' | '\\'] [_]
            / "\\" ['"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't']
            / "\\u" ['0'..='9' | 'a'..='f' | 'A'..='F']*<4>

        // Whitespace and `//` comments.
        rule _ = quiet!{ ([' ' | '\t' | '\r' | '\n'] / "//" (!"\n" [_])*)* }
    }
}
