/*! The abstract syntax of the model-matching DSL.

Everything here is program-independent: a parsed file can be checked, printed and diffed with no
artifact in hand, which is what lets `ctadl` validate a model file before anything is imported.
Nothing in this module knows what a `ProgramMatchIndex` is.
*/

use std::fmt;

use ctadl_ir::mir::PathSegment;

use crate::models::FormalIndexTypeTag;

/// A byte range in the source file, for error reporting.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    /// The 1-based (line, column) this span starts at, given the file it came from.
    pub fn line_col(&self, source: &str) -> (usize, usize) {
        let mut line = 1usize;
        let mut col = 1usize;
        for (i, ch) in source.char_indices() {
            if i >= self.start {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

/// A whole model file: the rules, in file order.
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub rules: Vec<Rule>,
}

/// One `heads :- body;` statement. An empty `body` is a fact.
#[derive(Clone, Debug)]
pub struct Rule {
    /// Position in the file, 0-based. This is what diagnostics name a rule by, and it is the
    /// DSL's counterpart of a JSON generator index.
    pub index: usize,
    pub heads: Vec<Head>,
    pub body: Vec<BodyItem>,
    pub span: Span,
}

impl Rule {
    /// Which phase consumes this rule's heads. A rule may contribute to both.
    pub fn phases(&self) -> (bool, bool) {
        let mut index_time = false;
        let mut query_time = false;
        for head in &self.heads {
            match head.kind {
                HeadKind::Source { .. } | HeadKind::Sink { .. } => query_time = true,
                HeadKind::Propagation { .. }
                | HeadKind::Bridge { .. }
                | HeadKind::AccessPath { .. } => index_time = true,
            }
        }
        (index_time, query_time)
    }
}

/// One output atom.
#[derive(Clone, Debug)]
pub struct Head {
    /// The `S::` prefix, if written. A port inside the atom that carries no anchor of its own
    /// takes this one.
    pub anchor: Option<Term>,
    pub kind: HeadKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HeadKind {
    Source {
        port: PortExpr,
        /// Any access path extending the source port is tainted too. Default false.
        saturating: bool,
        /// The taint label. `kind` in the JSON schema; defaults to `"UserInput"` when the rule
        /// does not say, so a one-line model still works.
        label: String,
    },
    Sink {
        port: PortExpr,
        /// Match any access-path extension of the port. Default true.
        wildcard: bool,
        label: String,
    },
    Propagation {
        flow: Flow,
    },
    Bridge {
        flow: Flow,
    },
    AccessPath {
        /// Written literally, in the canonical access-path grammar (`.next.next`).
        text: String,
        segments: Vec<PathSegment>,
    },
}

impl HeadKind {
    pub fn relation_name(&self) -> &'static str {
        match self {
            HeadKind::Source { .. } => "source",
            HeadKind::Sink { .. } => "sink",
            HeadKind::Propagation { .. } => "propagation",
            HeadKind::Bridge { .. } => "bridge",
            HeadKind::AccessPath { .. } => "access_paths",
        }
    }
}

/// `left -> right`, `left <- right`, `left <-> right`.
///
/// The written operands are kept as written, and the arrow with them. Normalizing to src/dst at
/// parse time would lose which side is which, and a bridge needs it: the left port is side A
/// (the call side, in the caller's own vocabulary) and the right is side B (the implementation),
/// whichever way the data flows.
#[derive(Clone, Debug)]
pub struct Flow {
    pub left: PortExpr,
    pub right: PortExpr,
    pub op: FlowOp,
    pub span: Span,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FlowOp {
    /// `->`
    ToRight,
    /// `<-`
    ToLeft,
    /// `<->`
    Both,
}

impl Flow {
    /// Where the data comes from, for a one-directional flow. `<->` reports the right operand
    /// and the caller is expected to also emit the mirror row.
    pub fn src(&self) -> &PortExpr {
        match self.op {
            FlowOp::ToRight => &self.left,
            FlowOp::ToLeft | FlowOp::Both => &self.right,
        }
    }

    /// Where the data arrives. See [`Self::src`].
    pub fn dst(&self) -> &PortExpr {
        match self.op {
            FlowOp::ToRight => &self.right,
            FlowOp::ToLeft | FlowOp::Both => &self.left,
        }
    }
}

/// A port, possibly anchored: `F::arg(0).foo[2]`.
#[derive(Clone, Debug)]
pub struct PortExpr {
    pub anchor: Option<Term>,
    pub port: Port,
    pub span: Span,
}

/// The port itself, with no anchor.
#[derive(Clone, Debug)]
pub struct Port {
    pub base: PortBase,
    pub path: Vec<PathSegment>,
}

#[derive(Clone, Debug)]
pub enum PortBase {
    Return,
    /// `arg(2)`.
    Arg(i16),
    /// `arg(_)` — expanded by the engine over the function's arity.
    AnyArg,
    /// `arg(I)` where `I` is bound in the body.
    ArgVar(String),
}

impl PortBase {
    /// The `(tag, index)` pair the match structures carry, for the two cases that need no
    /// binding. `ArgVar` has none until the rule is grounded.
    pub fn tag(&self) -> Option<(FormalIndexTypeTag, Option<i16>)> {
        match self {
            PortBase::Return => Some((FormalIndexTypeTag::Return, None)),
            PortBase::Arg(i) => Some((FormalIndexTypeTag::Index, Some(*i))),
            PortBase::AnyArg => Some((FormalIndexTypeTag::AnyArgument, None)),
            PortBase::ArgVar(_) => None,
        }
    }
}

/// A body conjunct. `,` between two of these is the conjunction; `&&` / `||` inside one is a
/// boolean combination of tests.
#[derive(Clone, Debug)]
pub enum BodyItem {
    Atom(Atom),
    /// `!atom`, and the boolean forms, which are only ever filters.
    Not(Box<BodyItem>),
    And(Vec<BodyItem>),
    Or(Vec<BodyItem>),
}

/// One relation atom or one operator application.
#[derive(Clone, Debug)]
pub struct Atom {
    pub name: String,
    /// Positional columns.
    pub columns: Vec<Term>,
    /// Attribute constraints, in written order.
    pub attrs: Vec<AttrConstraint>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct AttrConstraint {
    pub name: String,
    pub op: CmpOp,
    pub rhs: Rhs,
    pub span: Span,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
}

impl fmt::Display for CmpOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::In => "in",
        };
        f.write_str(s)
    }
}

#[derive(Clone, Debug)]
pub enum Rhs {
    Term(Term),
    Set(Vec<Literal>),
}

#[derive(Clone, Debug)]
pub enum Term {
    Var(String),
    Lit(Literal),
    /// `_`: matches anything, binds nothing, and every occurrence is independent.
    Wildcard,
}

impl Term {
    pub fn as_var(&self) -> Option<&str> {
        match self {
            Term::Var(v) => Some(v),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    Str(String),
    Int(i64),
    Bool(bool),
}

impl fmt::Display for Literal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Str(s) => write!(f, "{s:?}"),
            Literal::Int(i) => write!(f, "{i}"),
            Literal::Bool(b) => write!(f, "{b}"),
        }
    }
}

/// Every relation name the language reserves, whether or not a rule can currently use it.
///
/// The design reserves *all* relation names: they are built in, so a model file cannot define
/// one, and a name that is not here is a typo rather than a user relation.
pub const BUILTIN_RELATIONS: &[&str] = &[
    "fun",
    "param",
    "callsite",
    "subclass",
    "subclass*",
    "subclass+",
    "uses_field",
];

/// Operators usable in atom position. Not relations: they filter, they never generate.
pub const BUILTIN_OPERATORS: &[&str] = &["regex_match"];

/// The output relations a head may name.
pub const OUTPUT_RELATIONS: &[&str] = &["source", "sink", "propagation", "bridge", "access_paths"];

/// Attribute names each built-in relation honors.
pub fn relation_attributes(name: &str) -> &'static [&'static str] {
    match name {
        "fun" => &[
            "name",
            "arity",
            "language",
            "parent",
            "signature",
            "has_code",
            "qualified-id",
            "import",
        ],
        "callsite" => &["callee_string"],
        _ => &[],
    }
}

/// How many positional columns each built-in relation takes.
pub fn relation_arity(name: &str) -> Option<usize> {
    match name {
        "fun" => Some(1),
        "param" => Some(2),
        "callsite" => Some(2),
        "subclass" | "subclass*" | "subclass+" => Some(2),
        "uses_field" => Some(2),
        "regex_match" => Some(2),
        _ => None,
    }
}
