/*! Surface syntax to [`ast`]: a PEG grammar, then the checks it cannot make.

The grammar yields a syntax tree that mirrors the text; [`Builder`] turns that into the AST. Two
jobs beyond the mechanical walk. First, the argument positions of the output relations are
untyped in the grammar — one `head_arg` production covers attributes, flows, ports and strings —
so this is where `source(F::return <- F::arg(0))` is rejected as a flow where a port was wanted.
Second, `<-` is lexed in attribute position (see the note on `attr_op`) precisely so the message
can be "arrow not allowed here" instead of a parse error pointing one token past the mistake.

The language is Datalog-shaped and non-recursive: a file is a list of rules, each deriving one or
more *output* atoms (`source`, `sink`, `propagation`, `bridge`, `access_paths`, `dispatch`) from a
conjunction of *input* atoms over the built-in relations. See `models/dsl/mod.rs` for the
semantics.
*/

use ctadl_ir::mir::{Offset, PathSegment, Symbol};

use super::ast::*;
use super::{DslError, DslErrors};

/// Parses one model file's text into a [`Program`].
///
/// Errors accumulate: a file with three malformed rules reports three, in file order, rather
/// than stopping at the first. A *syntax* error is the exception — the parser cannot resynch,
/// so it is reported alone.
pub fn parse_program(text: &str) -> Result<Program, DslErrors> {
    let raw_rules = match dsl::program(text) {
        Ok(rules) => rules,
        Err(e) => {
            let mut errors = DslErrors::default();
            let at = e.location.offset;
            errors.push(DslError::Syntax {
                message: format!("expected {}", e.expected),
                span: Span { start: at, end: at },
            });
            return Err(errors);
        }
    };
    let mut builder = Builder {
        errors: DslErrors::default(),
    };
    let mut rules = Vec::new();
    for raw in raw_rules {
        let index = rules.len();
        if let Some(rule) = builder.rule_stmt(index, raw) {
            rules.push(rule);
        }
    }
    if builder.errors.is_empty() {
        Ok(Program { rules })
    } else {
        Err(builder.errors)
    }
}

// ---------------------------------------------------------------------------
// Syntax tree
// ---------------------------------------------------------------------------

/// A rule as written. A head with no body is a fact: `access_paths(".next.next");`
struct RawRule<'s> {
    heads: Vec<RawHead<'s>>,
    body: Vec<RawOr<'s>>,
    span: Span,
}

struct RawHead<'s> {
    anchor: Option<Term>,
    relation: &'s str,
    args: Vec<RawHeadArg<'s>>,
    span: Span,
}

enum RawHeadArg<'s> {
    Attr(&'s str, Literal, Span),
    Flow(RawFlow<'s>),
    Port(RawPortExpr<'s>),
    Lit(Literal, Span),
}

struct RawFlow<'s> {
    left: RawPortExpr<'s>,
    op: FlowOp,
    right: RawPortExpr<'s>,
    span: Span,
}

struct RawPortExpr<'s> {
    anchor: Option<Term>,
    base: RawPortBase<'s>,
    path: Vec<RawPathSeg<'s>>,
    span: Span,
}

enum RawPortBase<'s> {
    Return,
    Arg(RawArgSel<'s>, Span),
}

enum RawArgSel<'s> {
    Int(&'s str),
    Wildcard,
    Var(&'s str),
}

enum RawPathSeg<'s> {
    Offset(&'s str, Span),
    Symbol(String, Span),
}

/// `a || b || ...`, each disjunct a conjunction `x && y && ...`.
type RawOr<'s> = Vec<Vec<RawUnary<'s>>>;

struct RawUnary<'s> {
    negations: usize,
    atom: RawAtomExpr<'s>,
}

enum RawAtomExpr<'s> {
    Paren(RawOr<'s>),
    Rel(RawRelAtom<'s>),
    Test {
        lhs: Term,
        op: RawOp<'s>,
        rhs: Rhs,
        span: Span,
    },
}

struct RawRelAtom<'s> {
    name: &'s str,
    args: Vec<RawRelArg<'s>>,
    span: Span,
}

enum RawRelArg<'s> {
    Attr {
        name: &'s str,
        op: RawOp<'s>,
        rhs: Rhs,
        span: Span,
    },
    Term(Term, Span),
}

/// A comparison operator's text and where it is.
struct RawOp<'s>(&'s str, Span);

fn span(start: usize, end: usize) -> Span {
    Span { start, end }
}

peg::parser! {
    grammar dsl() for str {
        pub rule program() -> Vec<RawRule<'input>> = _ rules:(rule_stmt() ** _) _ { rules }

        rule rule_stmt() -> RawRule<'input>
            = s:position!() heads:(head_atom() ++ (_ "," _)) _
              body:(":-" _ b:(or_expr() ++ (_ "," _)) _ { b })? ";" e:position!()
            { RawRule { heads, body: body.unwrap_or_default(), span: span(s, e) } }

        // -------------------------------------------------------------------
        // Heads
        // -------------------------------------------------------------------

        rule head_atom() -> RawHead<'input>
            = s:position!() anchor:(a:anchor() _ { a })? relation:out_rel() _
              "(" _ args:(head_arg() ** (_ "," _)) _ ")" e:position!()
            { RawHead { anchor, relation, args, span: span(s, e) } }

        // `!name_char()` keeps `sourcefoo` from lexing as `source` followed by junk.
        rule out_rel() -> &'input str
            = quiet!{
                $(("source" / "sink" / "propagation" / "bridge" / "access_paths" / "dispatch")
                  !name_char())
            }
            / expected!("output relation")

        // `F::` — the function (or call site) an otherwise bare port hangs off.
        rule anchor() -> Term = t:term() _ "::" { t }

        // Ordered choice, and the order is the whole trick:
        //   `saturating = true`  is an attribute (lowercase name, then `=`)
        //   `F::return <- F::arg(0)` is a flow (a port, an arrow, a port)
        //   `F::arg(2)`          is a lone port
        //   `".foo.bar"`         is a literal, which is what `access_paths` takes
        // A flow must come before a lone port: `port_expr` succeeds on the left half of a flow
        // and would leave the arrow dangling.
        rule head_arg() -> RawHeadArg<'input>
            = s:position!() name:lname() _ "=" _ lit:literal() e:position!()
              { RawHeadArg::Attr(name, lit, span(s, e)) }
            / f:flow() { RawHeadArg::Flow(f) }
            / p:port_expr() { RawHeadArg::Port(p) }
            / s:position!() lit:literal() e:position!() { RawHeadArg::Lit(lit, span(s, e)) }

        rule flow() -> RawFlow<'input>
            = s:position!() left:port_expr() _ op:flow_op() _ right:port_expr() e:position!()
            { RawFlow { left, op, right, span: span(s, e) } }

        rule flow_op() -> FlowOp
            = "<->" { FlowOp::Both }
            / "<-" { FlowOp::ToLeft }
            / "->" { FlowOp::ToRight }

        // -------------------------------------------------------------------
        // Ports
        // -------------------------------------------------------------------

        rule port_expr() -> RawPortExpr<'input>
            = s:position!() anchor:(a:anchor() _ { a })? base:port_base()
              path:(_ p:path_seg() { p })* e:position!()
            { RawPortExpr { anchor, base, path, span: span(s, e) } }

        rule port_base() -> RawPortBase<'input>
            = "return" !name_char() { RawPortBase::Return }
            / s:position!() ("arg" / "param") !name_char() _ "(" _ sel:arg_sel() _ ")"
              e:position!()
              { RawPortBase::Arg(sel, span(s, e)) }

        rule arg_sel() -> RawArgSel<'input>
            = i:int() { RawArgSel::Int(i) }
            / wildcard() { RawArgSel::Wildcard }
            / v:var() { RawArgSel::Var(v) }

        // `.foo`, `[2]`, `.[2]`, `."weird.field[0]"`. The offset alternative is tried first so
        // that `.[2]` is an offset rather than a symbol whose name begins with `[`.
        rule path_seg() -> RawPathSeg<'input>
            = s:position!() ("." _)? "[" _ i:int() _ "]" e:position!()
              { RawPathSeg::Offset(i, span(s, e)) }
            / s:position!() "." _ name:(string() / n:$(seg_char()+) { n.to_string() })
              e:position!()
              { RawPathSeg::Symbol(name, span(s, e)) }

        rule seg_char()
            = ['a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '$' | '-' | '%' | '*' | '#' | '<' | '>']

        // -------------------------------------------------------------------
        // Bodies
        // -------------------------------------------------------------------

        rule or_expr() -> RawOr<'input> = and_expr() ++ (_ "||" _)

        rule and_expr() -> Vec<RawUnary<'input>> = unary_expr() ++ (_ "&&" _)

        rule unary_expr() -> RawUnary<'input>
            = negs:("!" !"=" _)* atom:atom_expr()
            { RawUnary { negations: negs.len(), atom } }

        rule atom_expr() -> RawAtomExpr<'input>
            = "(" _ o:or_expr() _ ")" { RawAtomExpr::Paren(o) }
            / r:rel_atom() { RawAtomExpr::Rel(r) }
            / s:position!() lhs:term() _ op:attr_op() _ rhs:rhs() e:position!()
              { RawAtomExpr::Test { lhs, op, rhs, span: span(s, e) } }

        // `subclass*` and `subclass+` are relation names, not postfix operators: there is no
        // recursion in this language, so the closure is a property of the built-in, not of the
        // rule.
        rule rel_atom() -> RawRelAtom<'input>
            = s:position!() name:$(lname() ("*" / "+")?) _
              "(" _ args:(rel_arg() ** (_ "," _)) _ ")" e:position!()
            { RawRelAtom { name, args, span: span(s, e) } }

        rule rel_arg() -> RawRelArg<'input>
            = s:position!() name:lname() _ op:attr_op() _ rhs:rhs() e:position!()
              { RawRelArg::Attr { name, op, rhs, span: span(s, e) } }
            / s:position!() t:term() e:position!() { RawRelArg::Term(t, span(s, e)) }

        // `"<-"` sits between `"<="` and `"<"` on purpose. Maximal munch takes the arrow, so a
        // mistyped `arity <- 1` parses here and the builder reports "arrow not allowed in this
        // position" rather than the parser reporting a syntax error pointing at `1`. A real
        // `arity < -1` is unaffected: the space stops `<-` from matching.
        rule attr_op() -> RawOp<'input>
            = s:position!()
              op:$("<=" / ">=" / "!=" / "<-" / "<" / ">" / "=" / "in" !name_char())
              e:position!()
            { RawOp(op, span(s, e)) }

        rule rhs() -> Rhs
            = "{" _ lits:(literal() ** (_ "," _)) _ "}" { Rhs::Set(lits) }
            / t:term() { Rhs::Term(t) }

        // -------------------------------------------------------------------
        // Terminals
        // -------------------------------------------------------------------

        rule term() -> Term
            = l:literal() { Term::Lit(l) }
            / v:var() { Term::Var(v.to_string()) }
            / wildcard() { Term::Wildcard }

        rule literal() -> Literal
            = s:string() { Literal::Str(s) }
            / i:int() { Literal::Int(i.parse::<i64>().unwrap_or_default()) }
            / b:$("true" / "false") !name_char() { Literal::Bool(b == "true") }

        rule wildcard() = "_" !name_char()

        // Variables are uppercase so they never collide with the many built-in relation names.
        rule var() -> &'input str
            = quiet!{ $(['A'..='Z'] name_char()*) } / expected!("variable")

        // Relation and attribute names. The interior `-` is what lets `qualified-id` be one
        // name; a trailing `-` is excluded so `arity -1` cannot lex as one token.
        rule lname() -> &'input str
            = quiet!{ $(['a'..='z' | '_'] name_char()* ("-" name_char()+)*) }
            / expected!("name")

        rule int() -> &'input str
            = quiet!{ $(['+' | '-']? ['0'..='9']+) } / expected!("integer")

        rule string() -> String
            = "\"" s:$((!['"' | '\\'] [_] / "\\" [_])*) "\"" { unescape(s) }

        rule name_char() = ['a'..='z' | 'A'..='Z' | '0'..='9' | '_']

        // Whitespace, `//` and `/* */` comments.
        rule _ = quiet!{
            ([' ' | '\t' | '\r' | '\n'] / "//" (!"\n" [_])* / "/*" (!"*/" [_])* "*/")*
        }
    }
}

// ---------------------------------------------------------------------------
// Syntax tree to AST
// ---------------------------------------------------------------------------

struct Builder {
    errors: DslErrors,
}

impl Builder {
    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(DslError::Rule {
            message: message.into(),
            span,
        });
    }

    fn rule_stmt(&mut self, index: usize, raw: RawRule<'_>) -> Option<Rule> {
        let before = self.errors.len();
        let heads: Vec<Head> = raw
            .heads
            .into_iter()
            .filter_map(|h| self.head_atom(h))
            .collect();
        let body: Vec<BodyItem> = raw
            .body
            .into_iter()
            .filter_map(|b| self.or_expr(b))
            .collect();
        if self.errors.len() != before {
            return None;
        }
        Some(Rule {
            index,
            heads,
            body,
            span: raw.span,
        })
    }

    // -----------------------------------------------------------------------
    // Heads
    // -----------------------------------------------------------------------

    fn head_atom(&mut self, raw: RawHead<'_>) -> Option<Head> {
        let args: Vec<HeadArg> = raw
            .args
            .into_iter()
            .filter_map(|a| self.head_arg(a))
            .collect();
        let kind = self.head_kind(raw.relation, raw.span, args)?;
        Some(Head {
            anchor: raw.anchor,
            kind,
            span: raw.span,
        })
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
        // `dispatch` is settled first: its attribute forms carry no subject at all, so the
        // one-subject rule below does not apply to them.
        if relation == "dispatch" {
            return self.dispatch_head(span, ports, flows, literals, attrs);
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

    /// A `dispatch` head: `K::dispatch(return <- arg(0))`, or one of the attribute forms
    /// `resolve = "inline"` / `resolve = "skip"` / `closure_shaped = true`.
    ///
    /// Exactly one of the three, mirroring the JSON loader, so that a generator that forgot its
    /// model and one that meant to discard the call's targets stay different documents. The one
    /// combination is `resolve = "inline"` with a flow, the JSON's `resolve` beside a
    /// `propagation`: inlined, and summarized too.
    fn dispatch_head(
        &mut self,
        span: Span,
        ports: Vec<PortExpr>,
        mut flows: Vec<Flow>,
        literals: Vec<(Literal, Span)>,
        attrs: Vec<(String, Literal, Span)>,
    ) -> Option<HeadKind> {
        if !ports.is_empty() {
            self.error(
                span,
                "'dispatch' takes a flow such as 'K::dispatch(return <- arg(0))', not a bare port",
            );
            return None;
        }
        if let Some((_, lspan)) = literals.first() {
            self.error(
                *lspan,
                "'dispatch' takes a flow or an attribute, not a string",
            );
            return None;
        }
        let mut disposition: Option<DispatchHead> = None;
        let mut set = |this: &mut Self, d: DispatchHead, at: Span| {
            if disposition.is_some() {
                this.error(
                    at,
                    "a 'dispatch' head says exactly one thing; write a separate head atom for                      each, or a separate rule",
                );
            } else {
                disposition = Some(d);
            }
        };
        for (name, lit, aspan) in attrs {
            match name.as_str() {
                "resolve" => match &lit {
                    Literal::Str(v) if v == "inline" => set(self, DispatchHead::Inline, aspan),
                    Literal::Str(v) if v == "skip" => set(self, DispatchHead::Skip, aspan),
                    Literal::Str(other) => self.error(
                        aspan,
                        format!(
                            "unknown 'resolve' value {other:?}; expected \"inline\" or \"skip\""
                        ),
                    ),
                    other => {
                        self.error(aspan, format!("'resolve' must be a string; found {other}"))
                    }
                },
                "closure_shaped" => match lit {
                    Literal::Bool(true) => set(self, DispatchHead::ClosureShaped, aspan),
                    // `false` is not a head: it derives nothing, exactly as in the JSON loader.
                    Literal::Bool(false) => return None,
                    other => self.error(
                        aspan,
                        format!("'closure_shaped' must be a boolean; found {other}"),
                    ),
                },
                other => self.error(
                    aspan,
                    format!(
                        "'{other}' is not an attribute of 'dispatch'; expected 'resolve' or \
                         'closure_shaped'"
                    ),
                ),
            }
        }
        if let Some(flow) = flows.pop() {
            if !flows.is_empty() {
                self.error(
                    span,
                    "'dispatch' takes one flow per head atom; write several head atoms \
                     separated by ','",
                );
                return None;
            }
            // Inlining may carry a summary as well; nothing else may.
            let disposition = match disposition {
                None => DispatchHead::Flow(flow),
                Some(DispatchHead::Inline) => DispatchHead::InlineFlow(flow),
                Some(_) => {
                    self.error(
                        span,
                        "a 'dispatch' head carries a flow or an attribute, never both, except \
                         'resolve = \"inline\"': a signature that is modelled is not also skipped \
                         or closure-shaped",
                    );
                    return None;
                }
            };
            return Some(HeadKind::Dispatch { disposition });
        }
        match disposition {
            Some(disposition) => Some(HeadKind::Dispatch { disposition }),
            None => {
                self.error(
                    span,
                    "'dispatch' needs a flow such as 'K::dispatch(return <- arg(0))', or one of \
                     'resolve = \"inline\"', 'resolve = \"skip\"', 'closure_shaped = true'",
                );
                None
            }
        }
    }

    fn head_arg(&mut self, raw: RawHeadArg<'_>) -> Option<HeadArg> {
        match raw {
            RawHeadArg::Attr(name, lit, span) => Some(HeadArg::Attr(name.to_string(), lit, span)),
            RawHeadArg::Flow(f) => self.flow(f).map(HeadArg::Flow),
            RawHeadArg::Port(p) => self.port_expr(p).map(HeadArg::Port),
            RawHeadArg::Lit(lit, span) => Some(HeadArg::Lit(lit, span)),
        }
    }

    fn flow(&mut self, raw: RawFlow<'_>) -> Option<Flow> {
        let left = self.port_expr(raw.left)?;
        let right = self.port_expr(raw.right)?;
        Some(Flow {
            left,
            right,
            op: raw.op,
            span: raw.span,
        })
    }

    fn port_expr(&mut self, raw: RawPortExpr<'_>) -> Option<PortExpr> {
        let base = self.port_base(raw.base);
        let path = raw
            .path
            .into_iter()
            .filter_map(|seg| self.path_seg(seg))
            .collect();
        Some(PortExpr {
            anchor: raw.anchor,
            port: Port { base: base?, path },
            span: raw.span,
        })
    }

    fn port_base(&mut self, raw: RawPortBase<'_>) -> Option<PortBase> {
        match raw {
            RawPortBase::Return => Some(PortBase::Return),
            RawPortBase::Arg(RawArgSel::Int(text), span) => match text.parse::<i16>() {
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
            RawPortBase::Arg(RawArgSel::Wildcard, _) => Some(PortBase::AnyArg),
            RawPortBase::Arg(RawArgSel::Var(v), _) => Some(PortBase::ArgVar(v.to_string())),
        }
    }

    fn path_seg(&mut self, raw: RawPathSeg<'_>) -> Option<PathSegment> {
        match raw {
            RawPathSeg::Offset(text, span) => match text.parse::<i64>() {
                Ok(v) => Some(PathSegment::Offset(Offset(v))),
                Err(e) => {
                    self.error(span, format!("offset out of range: {e}"));
                    None
                }
            },
            RawPathSeg::Symbol(text, span) => {
                if text.is_empty() {
                    self.error(span, "an access-path segment cannot be empty");
                    return None;
                }
                Some(PathSegment::Symbol(Symbol::from(text.as_str())))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Bodies
    // -----------------------------------------------------------------------

    fn or_expr(&mut self, raw: RawOr<'_>) -> Option<BodyItem> {
        let mut parts = Vec::new();
        for conj in raw {
            parts.push(self.and_expr(conj)?);
        }
        Some(if parts.len() == 1 {
            parts.pop().expect("checked len")
        } else {
            BodyItem::Or(parts)
        })
    }

    fn and_expr(&mut self, raw: Vec<RawUnary<'_>>) -> Option<BodyItem> {
        let mut parts = Vec::new();
        for unary in raw {
            parts.push(self.unary_expr(unary)?);
        }
        Some(if parts.len() == 1 {
            parts.pop().expect("checked len")
        } else {
            BodyItem::And(parts)
        })
    }

    fn unary_expr(&mut self, raw: RawUnary<'_>) -> Option<BodyItem> {
        let mut item = self.atom_expr(raw.atom)?;
        // `!!a` is `a`; fold rather than nesting, so the checker sees the parity it cares about.
        for _ in 0..(raw.negations % 2) {
            item = BodyItem::Not(Box::new(item));
        }
        Some(item)
    }

    fn atom_expr(&mut self, raw: RawAtomExpr<'_>) -> Option<BodyItem> {
        match raw {
            RawAtomExpr::Paren(inner) => self.or_expr(inner),
            RawAtomExpr::Rel(rel) => Some(BodyItem::Atom(self.rel_atom(rel))),
            RawAtomExpr::Test { lhs, op, rhs, span } => {
                self.test_expr(lhs, op, rhs, span).map(BodyItem::Atom)
            }
        }
    }

    fn rel_atom(&mut self, raw: RawRelAtom<'_>) -> Atom {
        let mut columns = Vec::new();
        let mut attrs = Vec::new();
        for arg in raw.args {
            match arg {
                RawRelArg::Attr {
                    name,
                    op,
                    rhs,
                    span,
                } => {
                    if let Some(op) = self.cmp_op(&op) {
                        attrs.push(AttrConstraint {
                            name: name.to_string(),
                            op,
                            rhs,
                            span,
                        });
                    }
                }
                RawRelArg::Term(term, span) => {
                    if !attrs.is_empty() {
                        // Positional columns come first; a column written after an attribute
                        // reads as though the attribute bound it.
                        self.error(
                            span,
                            "positional columns must come before attribute constraints",
                        );
                    }
                    columns.push(term);
                }
            }
        }
        Atom {
            name: raw.name.to_string(),
            columns,
            attrs,
            span: raw.span,
        }
    }

    /// `X = Y` and friends, in atom position. Modeled as a two-column atom named after the
    /// operator so the planner treats every filter the same way.
    fn test_expr(&mut self, lhs: Term, op: RawOp<'_>, rhs: Rhs, span: Span) -> Option<Atom> {
        let op = self.cmp_op(&op)?;
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

    fn cmp_op(&mut self, op: &RawOp<'_>) -> Option<CmpOp> {
        match op.0 {
            "=" => Some(CmpOp::Eq),
            "!=" => Some(CmpOp::Ne),
            "<" => Some(CmpOp::Lt),
            "<=" => Some(CmpOp::Le),
            ">" => Some(CmpOp::Gt),
            ">=" => Some(CmpOp::Ge),
            "in" => Some(CmpOp::In),
            "<-" => {
                // Lexed on purpose; see the note on `attr_op`. `arity <- 1` and `arity < -1`
                // differ by one space, and maximal munch takes the arrow.
                self.error(
                    op.1,
                    "'<-' is a flow arrow and is not allowed here. For a comparison against a \
                     negative number write '< -1', with a space.",
                );
                None
            }
            other => unreachable!("unexpected comparison operator {other}"),
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
