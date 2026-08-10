/*! pest → [`ast`]: surface syntax to abstract syntax.

Two jobs beyond the mechanical walk. First, the argument positions of the output relations are
untyped in the grammar — one `head_arg` production covers attributes, flows, ports and strings —
so this is where `source(F::return <- F::arg(0))` is rejected as a flow where a port was wanted.
Second, `<-` is lexed in attribute position (see the grammar's note on maximal munch) precisely
so the message can be "arrow not allowed here" instead of a parse error pointing one token past
the mistake.
*/

use pest::Parser;
use pest::iterators::Pair;

use ctadl_ir::mir::{Offset, PathSegment, Symbol};

use super::ast::*;
use super::{DslError, DslErrors};

// The generated enum is called `Rule`, and so is the AST's rule type. Keeping the derive in its
// own module is what lets both be named in this file.
mod grammar {
    #[derive(pest_derive::Parser)]
    #[grammar = "models/dsl/grammar.pest"]
    pub struct DslParser;
}

use grammar::DslParser;
use grammar::Rule as Rule_;

/// Parses one model file's text into a [`Program`].
///
/// Errors accumulate: a file with three malformed rules reports three, in file order, rather
/// than stopping at the first. A *syntax* error is the exception — the parser cannot resynch,
/// so it is reported alone.
pub fn parse_program(text: &str) -> Result<Program, DslErrors> {
    let mut pairs = match DslParser::parse(Rule_::program, text) {
        Ok(pairs) => pairs,
        Err(e) => {
            let mut errors = DslErrors::default();
            errors.push(DslError::Syntax {
                message: pest_message(&e),
                span: pest_span(&e),
            });
            return Err(errors);
        }
    };
    let program_pair = pairs.next().expect("program always yields one pair");
    let mut builder = Builder {
        errors: DslErrors::default(),
    };
    let mut rules = Vec::new();
    for pair in program_pair.into_inner() {
        match pair.as_rule() {
            Rule_::rule_stmt => {
                let index = rules.len();
                if let Some(rule) = builder.rule_stmt(index, pair) {
                    rules.push(rule);
                }
            }
            Rule_::EOI => {}
            other => unreachable!("unexpected top-level {other:?}"),
        }
    }
    if builder.errors.is_empty() {
        Ok(Program { rules })
    } else {
        Err(builder.errors)
    }
}

fn pest_message(e: &pest::error::Error<Rule_>) -> String {
    // pest's own rendering names the internal rule identifiers, which mean nothing to a model
    // author. Keep its position and expectation set, drop the rule-name noise where we can.
    e.variant.message().to_string()
}

fn pest_span(e: &pest::error::Error<Rule_>) -> Span {
    match e.location {
        pest::error::InputLocation::Pos(p) => Span { start: p, end: p },
        pest::error::InputLocation::Span((s, e)) => Span { start: s, end: e },
    }
}

struct Builder {
    errors: DslErrors,
}

fn span_of(pair: &Pair<'_, Rule_>) -> Span {
    let s = pair.as_span();
    Span {
        start: s.start(),
        end: s.end(),
    }
}

impl Builder {
    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(DslError::Rule {
            message: message.into(),
            span,
        });
    }

    fn rule_stmt(&mut self, index: usize, pair: Pair<'_, Rule_>) -> Option<Rule> {
        let span = span_of(&pair);
        let before = self.errors.len();
        let mut heads = Vec::new();
        let mut body = Vec::new();
        for inner in pair.into_inner() {
            match inner.as_rule() {
                Rule_::head_list => {
                    for head in inner.into_inner() {
                        if let Some(h) = self.head_atom(head) {
                            heads.push(h);
                        }
                    }
                }
                Rule_::body_list => {
                    for item in inner.into_inner() {
                        if let Some(b) = self.body_item(item) {
                            body.push(b);
                        }
                    }
                }
                other => unreachable!("unexpected {other:?} in rule_stmt"),
            }
        }
        if self.errors.len() != before {
            return None;
        }
        Some(Rule {
            index,
            heads,
            body,
            span,
        })
    }

    // -----------------------------------------------------------------------
    // Heads
    // -----------------------------------------------------------------------

    fn head_atom(&mut self, pair: Pair<'_, Rule_>) -> Option<Head> {
        let span = span_of(&pair);
        let mut anchor = None;
        let mut relation = String::new();
        let mut args: Vec<HeadArg> = Vec::new();
        for inner in pair.into_inner() {
            match inner.as_rule() {
                Rule_::anchor => anchor = Some(self.anchor(inner)),
                Rule_::out_rel => relation = inner.as_str().to_string(),
                Rule_::head_arg_list => {
                    for arg in inner.into_inner() {
                        if let Some(a) = self.head_arg(arg) {
                            args.push(a);
                        }
                    }
                }
                other => unreachable!("unexpected {other:?} in head_atom"),
            }
        }
        let kind = self.head_kind(&relation, span, args)?;
        Some(Head { anchor, kind, span })
    }

    fn head_kind(&mut self, relation: &str, span: Span, args: Vec<HeadArg>) -> Option<HeadKind> {
        let mut ports: Vec<PortExpr> = Vec::new();
        let mut flows: Vec<Flow> = Vec::new();
        let mut literals: Vec<(Literal, Span)> = Vec::new();
        let mut attrs: Vec<(String, Literal, Span)> = Vec::new();
        for arg in args {
            match arg {
                HeadArg::Port(p) => ports.push(p),
                HeadArg::Flow(f) => flows.push(f),
                HeadArg::Lit(l, s) => literals.push((l, s)),
                HeadArg::Attr(name, lit, s) => attrs.push((name, lit, s)),
            }
        }
        // Every output relation takes exactly one subject; several subjects are written as
        // several comma-separated head atoms, which is what the design's multi-port examples do.
        let subjects = ports.len() + flows.len() + literals.len();
        if subjects != 1 {
            self.error(
                span,
                format!(
                    "'{relation}' takes exactly one port, flow or path; {subjects} were given. \
                     Write several head atoms separated by ',' instead."
                ),
            );
            return None;
        }
        match relation {
            "source" | "sink" => {
                if !flows.is_empty() {
                    self.error(
                        span,
                        format!(
                            "'{relation}' takes a port, not a flow; a flow is what \
                                 'propagation' and 'bridge' take"
                        ),
                    );
                    return None;
                }
                let Some(port) = ports.pop() else {
                    self.error(
                        span,
                        format!("'{relation}' takes a port such as 'F::return' or 'F::arg(0)'"),
                    );
                    return None;
                };
                let mut saturating = false;
                let mut wildcard = true;
                let mut label = default_label().to_string();
                for (name, lit, aspan) in attrs {
                    match (relation, name.as_str()) {
                        (_, "kind") => match lit {
                            Literal::Str(s) => label = s,
                            other => {
                                self.error(aspan, format!("'kind' must be a string; found {other}"))
                            }
                        },
                        ("source", "saturating") => match lit {
                            Literal::Bool(b) => saturating = b,
                            other => self.error(
                                aspan,
                                format!("'saturating' must be a boolean; found {other}"),
                            ),
                        },
                        ("sink", "wildcard") => match lit {
                            Literal::Bool(b) => wildcard = b,
                            other => self.error(
                                aspan,
                                format!("'wildcard' must be a boolean; found {other}"),
                            ),
                        },
                        ("source", "wildcard") => self.error(
                            aspan,
                            "'wildcard' is a sink attribute; a source uses 'saturating'",
                        ),
                        ("sink", "saturating") => self.error(
                            aspan,
                            "'saturating' is a source attribute; a sink uses 'wildcard'",
                        ),
                        (_, other) => self.error(
                            aspan,
                            format!(
                                "'{other}' is not an attribute of '{relation}'; expected one of \
                                 'kind', {}",
                                if relation == "source" {
                                    "'saturating'"
                                } else {
                                    "'wildcard'"
                                }
                            ),
                        ),
                    }
                }
                if relation == "source" {
                    Some(HeadKind::Source {
                        port,
                        saturating,
                        label,
                    })
                } else {
                    Some(HeadKind::Sink {
                        port,
                        wildcard,
                        label,
                    })
                }
            }
            "propagation" | "bridge" => {
                let Some(flow) = flows.pop() else {
                    self.error(
                        span,
                        format!(
                            "'{relation}' takes a flow such as 'F::return <- F::arg(0)', not a \
                             bare port"
                        ),
                    );
                    return None;
                };
                for (name, _, aspan) in attrs {
                    self.error(
                        aspan,
                        format!("'{name}' is not an attribute of '{relation}'"),
                    );
                }
                if relation == "propagation" {
                    Some(HeadKind::Propagation { flow })
                } else {
                    Some(HeadKind::Bridge { flow })
                }
            }
            "access_paths" => {
                for (name, _, aspan) in attrs {
                    self.error(
                        aspan,
                        format!("'{name}' is not an attribute of 'access_paths'"),
                    );
                }
                let Some((lit, lspan)) = literals.pop() else {
                    self.error(span, "'access_paths' takes a string such as \".next.next\"");
                    return None;
                };
                let Literal::Str(text) = lit else {
                    self.error(
                        lspan,
                        "'access_paths' takes a string such as \".next.next\"",
                    );
                    return None;
                };
                match ctadl_ir::mir::parse_segments(&text) {
                    Ok(segments) if segments.is_empty() => {
                        self.error(
                            lspan,
                            "the empty path is always registered; name at least one segment, \
                             e.g. \".next.next\"",
                        );
                        None
                    }
                    Ok(segments) => Some(HeadKind::AccessPath { text, segments }),
                    Err(e) => {
                        self.error(lspan, format!("malformed access path {text:?}: {e}"));
                        None
                    }
                }
            }
            other => {
                self.error(span, format!("unknown output relation '{other}'"));
                None
            }
        }
    }

    fn head_arg(&mut self, pair: Pair<'_, Rule_>) -> Option<HeadArg> {
        let inner = pair.into_inner().next().expect("head_arg has one child");
        let span = span_of(&inner);
        match inner.as_rule() {
            Rule_::head_attr => {
                let mut it = inner.into_inner();
                let name = it.next().expect("attr name").as_str().to_string();
                let lit = self.literal(it.next().expect("attr value"));
                Some(HeadArg::Attr(name, lit, span))
            }
            Rule_::flow => self.flow(inner).map(HeadArg::Flow),
            Rule_::port_expr => self.port_expr(inner).map(HeadArg::Port),
            Rule_::literal => Some(HeadArg::Lit(self.literal(inner), span)),
            other => unreachable!("unexpected {other:?} in head_arg"),
        }
    }

    fn flow(&mut self, pair: Pair<'_, Rule_>) -> Option<Flow> {
        let span = span_of(&pair);
        let mut it = pair.into_inner();
        let lhs = self.port_expr(it.next().expect("flow lhs"))?;
        let op = it.next().expect("flow op").as_str().to_string();
        let rhs = self.port_expr(it.next().expect("flow rhs"))?;
        let op = match op.as_str() {
            "->" => FlowOp::ToRight,
            "<-" => FlowOp::ToLeft,
            "<->" => FlowOp::Both,
            other => unreachable!("unexpected flow operator {other}"),
        };
        Some(Flow {
            left: lhs,
            right: rhs,
            op,
            span,
        })
    }

    fn port_expr(&mut self, pair: Pair<'_, Rule_>) -> Option<PortExpr> {
        let span = span_of(&pair);
        let mut anchor = None;
        let mut port = None;
        for inner in pair.into_inner() {
            match inner.as_rule() {
                Rule_::anchor => anchor = Some(self.anchor(inner)),
                Rule_::port => port = self.port(inner),
                other => unreachable!("unexpected {other:?} in port_expr"),
            }
        }
        Some(PortExpr {
            anchor,
            port: port?,
            span,
        })
    }

    fn port(&mut self, pair: Pair<'_, Rule_>) -> Option<Port> {
        let mut base = None;
        let mut path = Vec::new();
        for inner in pair.into_inner() {
            match inner.as_rule() {
                Rule_::port_base => base = self.port_base(inner),
                Rule_::path_seg => {
                    if let Some(seg) = self.path_seg(inner) {
                        path.push(seg);
                    }
                }
                other => unreachable!("unexpected {other:?} in port"),
            }
        }
        Some(Port { base: base?, path })
    }

    fn port_base(&mut self, pair: Pair<'_, Rule_>) -> Option<PortBase> {
        let inner = pair.into_inner().next().expect("port_base has one child");
        match inner.as_rule() {
            Rule_::return_kw => Some(PortBase::Return),
            Rule_::arg_port => {
                let span = span_of(&inner);
                let sel = inner
                    .into_inner()
                    .find(|p| p.as_rule() == Rule_::arg_sel)
                    .expect("arg_port has a selector");
                let sel_inner = sel.into_inner().next().expect("arg_sel has one child");
                match sel_inner.as_rule() {
                    Rule_::int => match sel_inner.as_str().parse::<i16>() {
                        Ok(i) if i >= 0 => Some(PortBase::Arg(i)),
                        Ok(_) => {
                            self.error(span, "an argument index must not be negative");
                            None
                        }
                        Err(e) => {
                            self.error(span, format!("argument index out of range: {e}"));
                            None
                        }
                    },
                    Rule_::wildcard => Some(PortBase::AnyArg),
                    Rule_::var => Some(PortBase::ArgVar(sel_inner.as_str().to_string())),
                    other => unreachable!("unexpected {other:?} in arg_sel"),
                }
            }
            other => unreachable!("unexpected {other:?} in port_base"),
        }
    }

    fn path_seg(&mut self, pair: Pair<'_, Rule_>) -> Option<PathSegment> {
        let inner = pair.into_inner().next().expect("path_seg has one child");
        let span = span_of(&inner);
        match inner.as_rule() {
            Rule_::offset_seg => {
                let int = inner
                    .into_inner()
                    .next()
                    .expect("offset_seg carries an integer");
                match int.as_str().parse::<i64>() {
                    Ok(v) => Some(PathSegment::Offset(Offset(v))),
                    Err(e) => {
                        self.error(span, format!("offset out of range: {e}"));
                        None
                    }
                }
            }
            Rule_::symbol_seg => {
                let name = inner
                    .into_inner()
                    .next()
                    .expect("symbol_seg carries a name");
                let text = match name.as_rule() {
                    Rule_::string => unescape(name.into_inner().next().unwrap().as_str()),
                    _ => name.as_str().to_string(),
                };
                if text.is_empty() {
                    self.error(span, "an access-path segment cannot be empty");
                    return None;
                }
                Some(PathSegment::Symbol(Symbol::from(text.as_str())))
            }
            other => unreachable!("unexpected {other:?} in path_seg"),
        }
    }

    fn anchor(&mut self, pair: Pair<'_, Rule_>) -> Term {
        let term = pair.into_inner().next().expect("anchor has a term");
        self.term(term)
    }

    // -----------------------------------------------------------------------
    // Bodies
    // -----------------------------------------------------------------------

    fn body_item(&mut self, pair: Pair<'_, Rule_>) -> Option<BodyItem> {
        let inner = pair.into_inner().next().expect("body_item has one child");
        self.or_expr(inner)
    }

    fn or_expr(&mut self, pair: Pair<'_, Rule_>) -> Option<BodyItem> {
        let mut parts = Vec::new();
        for inner in pair.into_inner() {
            parts.push(self.and_expr(inner)?);
        }
        Some(if parts.len() == 1 {
            parts.pop().expect("checked len")
        } else {
            BodyItem::Or(parts)
        })
    }

    fn and_expr(&mut self, pair: Pair<'_, Rule_>) -> Option<BodyItem> {
        let mut parts = Vec::new();
        for inner in pair.into_inner() {
            parts.push(self.unary_expr(inner)?);
        }
        Some(if parts.len() == 1 {
            parts.pop().expect("checked len")
        } else {
            BodyItem::And(parts)
        })
    }

    fn unary_expr(&mut self, pair: Pair<'_, Rule_>) -> Option<BodyItem> {
        let mut negations = 0usize;
        let mut item = None;
        for inner in pair.into_inner() {
            match inner.as_rule() {
                Rule_::neg => negations += 1,
                Rule_::atom_expr => item = self.atom_expr(inner),
                other => unreachable!("unexpected {other:?} in unary_expr"),
            }
        }
        let mut item = item?;
        // `!!a` is `a`; fold rather than nesting, so the checker sees the parity it cares about.
        for _ in 0..(negations % 2) {
            item = BodyItem::Not(Box::new(item));
        }
        Some(item)
    }

    fn atom_expr(&mut self, pair: Pair<'_, Rule_>) -> Option<BodyItem> {
        let inner = pair.into_inner().next().expect("atom_expr has one child");
        match inner.as_rule() {
            Rule_::paren_expr => {
                let inner = inner.into_inner().next().expect("paren_expr has one child");
                self.or_expr(inner)
            }
            Rule_::rel_atom => self.rel_atom(inner).map(BodyItem::Atom),
            Rule_::test_expr => self.test_expr(inner).map(BodyItem::Atom),
            other => unreachable!("unexpected {other:?} in atom_expr"),
        }
    }

    fn rel_atom(&mut self, pair: Pair<'_, Rule_>) -> Option<Atom> {
        let span = span_of(&pair);
        let mut name = String::new();
        let mut columns = Vec::new();
        let mut attrs = Vec::new();
        for inner in pair.into_inner() {
            match inner.as_rule() {
                Rule_::rel_name => name = inner.as_str().to_string(),
                Rule_::rel_arg_list => {
                    for arg in inner.into_inner() {
                        let arg = arg.into_inner().next().expect("rel_arg has one child");
                        match arg.as_rule() {
                            Rule_::attr_constraint => {
                                if let Some(a) = self.attr_constraint(arg) {
                                    attrs.push(a);
                                }
                            }
                            Rule_::term => {
                                if !attrs.is_empty() {
                                    // Positional columns come first; a column written after an
                                    // attribute reads as though the attribute bound it.
                                    self.error(
                                        span_of(&arg),
                                        "positional columns must come before attribute \
                                         constraints",
                                    );
                                }
                                columns.push(self.term(arg));
                            }
                            other => unreachable!("unexpected {other:?} in rel_arg"),
                        }
                    }
                }
                other => unreachable!("unexpected {other:?} in rel_atom"),
            }
        }
        Some(Atom {
            name,
            columns,
            attrs,
            span,
        })
    }

    fn attr_constraint(&mut self, pair: Pair<'_, Rule_>) -> Option<AttrConstraint> {
        let span = span_of(&pair);
        let mut it = pair.into_inner();
        let name = it.next().expect("attr name").as_str().to_string();
        let op_pair = it.next().expect("attr op");
        let op = self.cmp_op(&op_pair)?;
        let rhs = self.rhs(it.next().expect("attr rhs"));
        Some(AttrConstraint {
            name,
            op,
            rhs,
            span,
        })
    }

    /// `X = Y` and friends, in atom position. Modeled as a two-column atom named after the
    /// operator so the planner treats every filter the same way.
    fn test_expr(&mut self, pair: Pair<'_, Rule_>) -> Option<Atom> {
        let span = span_of(&pair);
        let mut it = pair.into_inner();
        let lhs = self.term(it.next().expect("test lhs"));
        let op_pair = it.next().expect("test op");
        let op = self.cmp_op(&op_pair)?;
        let rhs = self.rhs(it.next().expect("test rhs"));
        Some(Atom {
            name: format!("${op}"),
            columns: vec![lhs],
            attrs: vec![AttrConstraint {
                name: String::new(),
                op,
                rhs,
                span,
            }],
            span,
        })
    }

    fn cmp_op(&mut self, pair: &Pair<'_, Rule_>) -> Option<CmpOp> {
        match pair.as_str() {
            "=" => Some(CmpOp::Eq),
            "!=" => Some(CmpOp::Ne),
            "<" => Some(CmpOp::Lt),
            "<=" => Some(CmpOp::Le),
            ">" => Some(CmpOp::Gt),
            ">=" => Some(CmpOp::Ge),
            "in" => Some(CmpOp::In),
            "<-" => {
                // Lexed on purpose; see the grammar's note. `arity <- 1` and `arity < -1` differ
                // by one space, and maximal munch takes the arrow.
                self.error(
                    span_of(pair),
                    "'<-' is a flow arrow and is not allowed here. For a comparison against a \
                     negative number write '< -1', with a space.",
                );
                None
            }
            other => unreachable!("unexpected comparison operator {other}"),
        }
    }

    fn rhs(&mut self, pair: Pair<'_, Rule_>) -> Rhs {
        let inner = pair.into_inner().next().expect("rhs has one child");
        match inner.as_rule() {
            Rule_::set_lit => Rhs::Set(
                inner
                    .into_inner()
                    .map(|lit| self.literal(lit))
                    .collect::<Vec<_>>(),
            ),
            Rule_::term => Rhs::Term(self.term(inner)),
            other => unreachable!("unexpected {other:?} in rhs"),
        }
    }

    fn term(&mut self, pair: Pair<'_, Rule_>) -> Term {
        let inner = match pair.as_rule() {
            Rule_::term => pair.into_inner().next().expect("term has one child"),
            _ => pair,
        };
        match inner.as_rule() {
            Rule_::literal => Term::Lit(self.literal(inner)),
            Rule_::var => Term::Var(inner.as_str().to_string()),
            Rule_::wildcard => Term::Wildcard,
            other => unreachable!("unexpected {other:?} in term"),
        }
    }

    fn literal(&mut self, pair: Pair<'_, Rule_>) -> Literal {
        let inner = match pair.as_rule() {
            Rule_::literal => pair.into_inner().next().expect("literal has one child"),
            _ => pair,
        };
        match inner.as_rule() {
            Rule_::string => Literal::Str(unescape(
                inner.into_inner().next().expect("string body").as_str(),
            )),
            Rule_::int => Literal::Int(inner.as_str().parse::<i64>().unwrap_or_default()),
            Rule_::bool_lit => Literal::Bool(inner.as_str() == "true"),
            other => unreachable!("unexpected {other:?} in literal"),
        }
    }
}

/// One argument of an output atom, before it is sorted into the relation's own shape.
enum HeadArg {
    Attr(String, Literal, Span),
    Flow(Flow),
    Port(PortExpr),
    Lit(Literal, Span),
}

/// The taint label a `source`/`sink` head takes when it does not say.
///
/// One label for both directions, so the smallest useful pair of rules —
/// `source(F::return) :- …;` and `sink(G::arg(0)) :- …;` — reports a flow without the author
/// having to invent a vocabulary first. Give `kind = "…"` as soon as there is more than one.
pub const fn default_label() -> &'static str {
    "taint"
}

/// String-literal escapes: `\"`, `\\`, `\n`, `\t`, `\r`, and `\<anything>` for the literal char.
///
/// Deliberately *not* the access-path escape set. A quoted access-path segment
/// (`arg(4)."weird.field[0]"`) carries its dots and brackets literally: the quotes are what take
/// them out of the path grammar, so re-interpreting `\.` here would need a second escape level
/// for no gain.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}
