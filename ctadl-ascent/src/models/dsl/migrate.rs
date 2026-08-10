/*! The JSON model-generator format, re-expressed in the DSL.

This is what keeps the model files already in the wild working: they are translated to DSL
source and run through the one engine, rather than matched by a second implementation of
`where`. It is also how the shipped defaults were converted — the `.ctadl` files next to them
are this migrator's output, checked in so a reader can see what a model says without running
anything.

# Disjunction becomes several rules

A `where` is a conjunction of constraints, and a constraint may be an `any_of` or a `not`. There
is no disjunction *inside* a Datalog body, so the constraint tree is pushed into negation normal
form and then into disjunctive normal form, and **each disjunct becomes one rule** with the same
heads. That is how Datalog spells "or", and it means the translation is exact rather than
approximate: `any_of` was already a union over the matched set.

# What does not translate

`taint`, `modes` and `forward_self` are schema-only in the JSON loader — a model using them
parses and has no effect. They are reported rather than translated, because translating a
construct into something that *looks* like it works would be worse than saying it does nothing.
`Variable(...)` ports and a bridge with no `arguments` map are reported for the opposite reason:
they mean something the DSL does not (yet) spell.
*/

use std::fmt::Write as _;

use serde_json::Value;

use ctadl_ir::mir::PathSegment;

/// What a migration could not carry across, and what it produced.
#[derive(Clone, Debug, Default)]
pub struct MigrationReport {
    /// One line per construct that was dropped or approximated, in generator order.
    pub warnings: Vec<String>,
    /// Generators read.
    pub generators: usize,
    /// Rules written. More than `generators` when a `where` had a disjunction in it.
    pub rules: usize,
}

impl MigrationReport {
    fn warn(&mut self, index: usize, message: impl std::fmt::Display) {
        self.warnings.push(format!("generator {index}: {message}"));
    }
}

/// Translates a whole `model_generators` list into DSL source.
pub fn migrate_generators<'a>(
    generators: impl IntoIterator<Item = &'a Value>,
    header: Option<&str>,
) -> (String, MigrationReport) {
    let mut out = String::new();
    let mut report = MigrationReport::default();
    if let Some(header) = header {
        for line in header.lines() {
            let _ = writeln!(out, "// {line}");
        }
        out.push('\n');
    }
    for (i, value) in generators.into_iter().enumerate() {
        report.generators += 1;
        let before = out.len();
        migrate_generator(i, value, &mut out, &mut report);
        if out.len() == before {
            let _ = writeln!(out, "// generator {i}: produced no rule");
        }
        out.push('\n');
    }
    (out, report)
}

/// Reads a `.json` / `.json5` / `.jsonl` model file and returns the DSL source for it.
pub fn migrate_file(
    path: &std::path::Path,
) -> Result<(String, MigrationReport), crate::error::Error> {
    use crate::error::ErrorContext;
    let generators: Vec<Value> = match path.extension().and_then(|e| e.to_str()) {
        Some("jsonl") => {
            let text = std::fs::read_to_string(path)
                .err_context(|| format!("reading model file: {}", path.display()))?;
            let mut out = Vec::new();
            for line in text.lines() {
                let trimmed = line.trim_start();
                if trimmed.is_empty() || trimmed.starts_with("//") {
                    continue;
                }
                out.push(
                    serde_json::from_str(trimmed)
                        .err_context(|| format!("reading model line of {}", path.display()))?,
                );
            }
            out
        }
        other => {
            let text = std::fs::read_to_string(path)
                .err_context(|| format!("reading model file: {}", path.display()))?;
            let root: Value = if other == Some("json5") {
                json5::from_str(&text)
                    .err_context(|| format!("parsing model JSON5 file: {}", path.display()))?
            } else {
                serde_json::from_str(&text)
                    .err_context(|| format!("parsing model JSON file: {}", path.display()))?
            };
            match root.get("model_generators").and_then(|v| v.as_array()) {
                Some(arr) => arr.clone(),
                None => {
                    return Err(crate::error::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "missing or invalid 'model_generators' array",
                    )));
                }
            }
        }
    };
    let header = format!(
        "Migrated from {} by `ctadl migrate-models`. Edit this file, not the original.",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
    );
    Ok(migrate_generators(generators.iter(), Some(&header)))
}

// ---------------------------------------------------------------------------
// One generator
// ---------------------------------------------------------------------------

/// The subject variable of the matched element. `F` throughout, so a migrated file reads the
/// same way everywhere.
const SUBJECT: &str = "F";
/// The caller of a matched call site, for `find: callsites`.
const CALLER: &str = "C";
/// The call site itself.
const SITE: &str = "S";
/// Side B of a bridge.
const OTHER: &str = "G";

fn migrate_generator(
    index: usize,
    generator: &Value,
    out: &mut String,
    report: &mut MigrationReport,
) {
    let find = generator
        .get("find")
        .and_then(|v| v.as_str())
        .unwrap_or("methods");
    let callsites = match find {
        "methods" => false,
        "callsites" => true,
        other => {
            report.warn(index, format!("'find: {other}' is not supported; skipped"));
            return;
        }
    };

    let scope = scope_attrs(generator.get("in"));
    let mut fresh = Fresh::default();

    let mut base: Vec<String> = Vec::new();
    if callsites {
        base.push(format!(
            "callsite({CALLER}, {SITE}, callee_string = {SUBJECT})"
        ));
    }

    let where_ = generator.get("where").and_then(|v| v.as_array());
    let expr = match where_ {
        Some(items) => Expr::And(items.iter().map(Expr::Prim).collect()),
        None => Expr::And(Vec::new()),
    };
    let ctx = Ctx {
        index,
        subject: SUBJECT,
        callsites,
    };
    let disjuncts = compile(&expr, false, &ctx, &mut fresh, report);

    let anchor = if callsites { SITE } else { SUBJECT };
    let heads = migrate_model(index, generator, anchor, &mut fresh, report);
    if heads.is_empty() {
        return;
    }

    // A bridge is written as its own rule: side B has a whole match block of its own, and
    // folding it into the shared body would make every other head depend on it.
    for disjunct in &disjuncts {
        let mut body = base.clone();
        body.extend(with_subject(disjunct.clone(), SUBJECT, &scope));
        let (bridge_heads, plain_heads): (Vec<_>, Vec<_>) =
            heads.iter().partition(|h| h.needs_side_b);
        if !plain_heads.is_empty() {
            write_rule(
                out,
                &plain_heads
                    .iter()
                    .map(|h| h.text.clone())
                    .collect::<Vec<_>>(),
                &body,
            );
            report.rules += 1;
        }
        for head in bridge_heads {
            let mut body = body.clone();
            body.extend(head.extra_body.iter().cloned());
            write_rule(out, std::slice::from_ref(&head.text), &body);
            report.rules += 1;
        }
    }
}

fn write_rule(out: &mut String, heads: &[String], body: &[String]) {
    let head_text = heads.join(",\n  ");
    if body.is_empty() {
        let _ = writeln!(out, "{head_text};");
        return;
    }
    let _ = writeln!(out, "{head_text} :-\n  {};", body.join(",\n  "));
}

/// Makes sure the subject is bound, and that the `in` scope is asserted about it.
///
/// A `where` that narrows the subject already contains a `fun(F, …)`, so the scope attributes
/// are folded into the first one rather than written as a second atom over the same variable.
/// A `where` that does not — or an absent one — gets the bare atom, because a head variable has
/// to be bound by *something*.
fn with_subject(mut atoms: Vec<String>, subject: &str, scope: &[String]) -> Vec<String> {
    let open = format!("fun({subject}, ");
    match atoms.iter().position(|a| a.starts_with(&open)) {
        Some(i) if !scope.is_empty() => {
            atoms[i] = format!("{open}{}, {}", scope.join(", "), &atoms[i][open.len()..]);
        }
        Some(_) => {}
        None if atoms.iter().any(|a| a == &format!("fun({subject})")) => {}
        None => atoms.insert(0, atom("fun", subject, scope)),
    }
    atoms
}

/// The `in` block, as attributes on the subject's `fun` atom.
fn scope_attrs(scope: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    let Some(scope) = scope else { return out };
    if let Some(one) = scope.get("language").and_then(|v| v.as_str()) {
        out.push(format!("language = {}", quote(one)));
    }
    if let Some(many) = scope.get("languages").and_then(|v| v.as_array()) {
        let items: Vec<String> = many.iter().filter_map(|v| v.as_str()).map(quote).collect();
        if !items.is_empty() {
            out.push(format!("language in {{{}}}", items.join(", ")));
        }
    }
    if let Some(name) = scope.get("import").and_then(|v| v.as_str()) {
        out.push(format!("import = {}", quote(name)));
    }
    out
}

// ---------------------------------------------------------------------------
// Constraints: NNF, then DNF
// ---------------------------------------------------------------------------

/// The constraint tree, before it is pushed into DNF.
///
/// There is no `Not` variant: a JSON `not` is a *constraint*, so it arrives as a [`Self::Prim`]
/// and [`compile_prim`] flips the negation flag when it sees one. Keeping the flag on the
/// recursion rather than in the tree is what makes the De Morgan step a pair of match arms
/// instead of a rewriting pass.
enum Expr<'a> {
    Prim(&'a Value),
    And(Vec<Expr<'a>>),
    Or(Vec<Expr<'a>>),
}

struct Ctx<'a> {
    index: usize,
    subject: &'a str,
    callsites: bool,
}

/// A disjunction of conjunctions of body atoms, as text.
type Dnf = Vec<Vec<String>>;

fn dnf_and(a: Dnf, b: Dnf) -> Dnf {
    let mut out = Vec::with_capacity(a.len() * b.len());
    for x in &a {
        for y in &b {
            let mut merged = x.clone();
            merged.extend(y.iter().cloned());
            out.push(merged);
        }
    }
    out
}

fn compile(
    expr: &Expr<'_>,
    negated: bool,
    ctx: &Ctx<'_>,
    fresh: &mut Fresh,
    report: &mut MigrationReport,
) -> Dnf {
    match (expr, negated) {
        // De Morgan: the negation is pushed to the leaves, where a single atom can carry it.
        (Expr::And(items), false) | (Expr::Or(items), true) => {
            let mut acc: Dnf = vec![Vec::new()];
            for item in items {
                acc = dnf_and(acc, compile(item, negated, ctx, fresh, report));
            }
            acc
        }
        (Expr::Or(items), false) | (Expr::And(items), true) => {
            let mut acc: Dnf = Vec::new();
            for item in items {
                acc.extend(compile(item, negated, ctx, fresh, report));
            }
            if acc.is_empty() {
                // An empty `any_of` matches nothing. There is no "false" atom, so say so and
                // emit an unsatisfiable equality rather than silently matching everything.
                acc.push(vec!["\"\" = \"unsatisfiable\"".to_string()]);
            }
            acc
        }
        (Expr::Prim(value), n) => compile_prim(value, n, ctx, fresh, report),
    }
}

fn compile_prim(
    value: &Value,
    negated: bool,
    ctx: &Ctx<'_>,
    fresh: &mut Fresh,
    report: &mut MigrationReport,
) -> Dnf {
    let kind = value
        .get("constraint")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // The combinators are the one shape that is not a leaf; re-enter `compile` for them so
    // negation keeps being pushed down.
    match kind {
        "all_of" | "any_of" => {
            let inners: Vec<Expr<'_>> = value
                .get("inners")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().map(Expr::Prim).collect())
                .unwrap_or_default();
            let expr = if kind == "all_of" {
                Expr::And(inners)
            } else {
                Expr::Or(inners)
            };
            return compile(&expr, negated, ctx, fresh, report);
        }
        "not" => {
            let Some(inner) = value.get("inner") else {
                report.warn(ctx.index, "'not' with no 'inner'; skipped");
                return vec![Vec::new()];
            };
            return compile(&Expr::Prim(inner), !negated, ctx, fresh, report);
        }
        _ => {}
    }
    let alternatives = leaf_atoms(value, ctx, fresh, report);
    if alternatives.is_empty() {
        return vec![Vec::new()];
    }
    if !negated {
        return alternatives;
    }
    // `!(a || b)` is `!a && !b`; each alternative contributes one negated conjunct.
    let mut conj: Vec<String> = Vec::new();
    for atoms in alternatives {
        conj.push(negate(&atoms));
    }
    vec![conj]
}

/// One leaf constraint's body atoms, as a disjunction (usually of one).
fn leaf_atoms(
    value: &Value,
    ctx: &Ctx<'_>,
    fresh: &mut Fresh,
    report: &mut MigrationReport,
) -> Dnf {
    let subject = ctx.subject;
    let kind = value
        .get("constraint")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match kind {
        "signature_match" => {
            let mut attrs = Vec::new();
            if let Some(a) = string_attr(value, "name", "names", "name") {
                attrs.push(a);
            }
            if let Some(a) = string_attr(value, "parent", "parents", "parent") {
                attrs.push(a);
            }
            if let Some(a) = string_attr(value, "qualified-id", "qualified-ids", "qualified-id") {
                attrs.push(a);
            }
            if attrs.is_empty() {
                report.warn(ctx.index, "'signature_match' with no usable field; skipped");
                return Vec::new();
            }
            vec![vec![atom("fun", subject, &attrs)]]
        }
        "name" => match value.get("pattern").and_then(|v| v.as_str()) {
            Some(pattern) => {
                let v = fresh.next("N");
                vec![vec![
                    atom("fun", subject, &[format!("name = {v}")]),
                    format!("regex_match({v}, {})", quote(pattern)),
                ]]
            }
            None => {
                report.warn(ctx.index, "'name' with no 'pattern'; skipped");
                Vec::new()
            }
        },
        "signature" | "signature_pattern" => match value.get("pattern").and_then(|v| v.as_str()) {
            Some(pattern) => {
                let v = fresh.next("Sig");
                vec![vec![
                    atom("fun", subject, &[format!("signature = {v}")]),
                    format!("regex_match({v}, {})", quote(pattern)),
                ]]
            }
            None => {
                report.warn(ctx.index, "'signature' with no 'pattern'; skipped");
                Vec::new()
            }
        },
        "has_code" => match value.get("value").and_then(|v| v.as_bool()) {
            Some(b) => vec![vec![atom("fun", subject, &[format!("has_code = {b}")])]],
            None => {
                report.warn(ctx.index, "'has_code' with no boolean 'value'; skipped");
                Vec::new()
            }
        },
        "uses_field" => {
            let names = collect_strings(value, "name", "names");
            if names.is_empty() {
                report.warn(ctx.index, "'uses_field' with no field name; skipped");
                return Vec::new();
            }
            if names.len() == 1 {
                vec![vec![format!("uses_field({subject}, {})", quote(&names[0]))]]
            } else {
                let v = fresh.next("Fld");
                vec![vec![
                    format!("uses_field({subject}, {v})"),
                    format!(
                        "{v} in {{{}}}",
                        names
                            .iter()
                            .map(|n| quote(n))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ]]
            }
        }
        "number_parameters" => match value.get("inner") {
            Some(inner) => int_predicate(inner, subject, fresh, ctx, report),
            None => {
                report.warn(ctx.index, "'number_parameters' with no 'inner'; skipped");
                Vec::new()
            }
        },
        "parent" => match value.get("inner") {
            Some(inner) => class_predicate(inner, subject, None, fresh, ctx, report),
            None => {
                report.warn(ctx.index, "'parent' with no 'inner'; skipped");
                Vec::new()
            }
        },
        "extends" => match value.get("inner") {
            Some(inner) => {
                let sup = fresh.next("Sup");
                class_predicate(inner, subject, Some(sup), fresh, ctx, report)
            }
            None => {
                report.warn(ctx.index, "'extends' with no 'inner'; skipped");
                Vec::new()
            }
        },
        "in_function" => {
            if !ctx.callsites {
                report.warn(
                    ctx.index,
                    "'in_function' is only meaningful with find: callsites; skipped",
                );
                return Vec::new();
            }
            let Some(inner) = value.get("inner") else {
                report.warn(ctx.index, "'in_function' with no 'inner'; skipped");
                return Vec::new();
            };
            let caller_ctx = Ctx {
                index: ctx.index,
                subject: CALLER,
                callsites: false,
            };
            compile(&Expr::Prim(inner), false, &caller_ctx, fresh, report)
        }
        other => {
            report.warn(
                ctx.index,
                format!("'{other}' is not a recognized constraint; skipped"),
            );
            Vec::new()
        }
    }
}

/// The `inner` of `number_parameters`, against the subject's arity.
fn int_predicate(
    inner: &Value,
    subject: &str,
    fresh: &mut Fresh,
    ctx: &Ctx<'_>,
    report: &mut MigrationReport,
) -> Dnf {
    let kind = inner
        .get("constraint")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if let Some(op) = dsl_op(kind)
        && let Some(v) = inner.get("value").and_then(|v| v.as_i64())
    {
        // `arity < -1` needs the space: `<-` is the flow arrow. `format!` writes one anyway,
        // but the rule is worth stating where the string is built.
        return vec![vec![atom("fun", subject, &[format!("arity {op} {v}")])]];
    }
    // Anything richer becomes a bound arity plus a boolean expression over it.
    let var = fresh.next("A");
    let Some(expr) = int_expr(inner, &var) else {
        report.warn(
            ctx.index,
            "'number_parameters' inner is not an integer comparison; skipped",
        );
        return Vec::new();
    };
    vec![vec![
        atom("fun", subject, &[format!("arity = {var}")]),
        expr,
    ]]
}

fn int_expr(inner: &Value, var: &str) -> Option<String> {
    let kind = inner.get("constraint").and_then(|v| v.as_str())?;
    match kind {
        "any_of" | "all_of" => {
            let sep = if kind == "any_of" { " || " } else { " && " };
            let parts: Vec<String> = inner
                .get("inners")?
                .as_array()?
                .iter()
                .map(|i| int_expr(i, var))
                .collect::<Option<Vec<_>>>()?;
            Some(format!("({})", parts.join(sep)))
        }
        "not" => Some(format!("!({})", int_expr(inner.get("inner")?, var)?)),
        other => {
            let op = dsl_op(other)?;
            let v = inner.get("value")?.as_i64()?;
            Some(format!("{var} {op} {v}"))
        }
    }
}

/// The `inner` of `parent` / `extends`, against the subject's owning class.
///
/// `via_super` turns it into `extends`: the class the predicate applies to is a *direct*
/// supertype of the owning class, which is exactly what the JSON constraint tests (it reads the
/// hierarchy table, not its closure).
fn class_predicate(
    inner: &Value,
    subject: &str,
    via_super: Option<String>,
    fresh: &mut Fresh,
    ctx: &Ctx<'_>,
    report: &mut MigrationReport,
) -> Dnf {
    let kind = inner
        .get("constraint")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match kind {
        "any_of" | "all_of" => {
            // Wrap each inner back into a `parent` / `extends` constraint of its own and
            // re-enter the shared machinery. That keeps one code path for the leaf shapes and
            // lets a class-level `any_of` fan out into rules as every other one does.
            let wrapper = if via_super.is_some() {
                "extends"
            } else {
                "parent"
            };
            let rewritten: Vec<Value> = inner
                .get("inners")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|i| serde_json::json!({ "constraint": wrapper, "inner": i.clone() }))
                        .collect()
                })
                .unwrap_or_default();
            let exprs: Vec<Expr<'_>> = rewritten.iter().map(Expr::Prim).collect();
            let expr = if kind == "all_of" {
                Expr::And(exprs)
            } else {
                Expr::Or(exprs)
            };
            // `rewritten` owns what `exprs` borrows, so the compile has to finish before it
            // goes out of scope; the explicit `return` is what makes that ordering visible.
            return compile(&expr, false, ctx, fresh, report);
        }
        _ => {}
    }
    let class_var = via_super.clone().unwrap_or_else(|| fresh.next("P"));
    let mut atoms: Vec<String> = Vec::new();
    match via_super {
        Some(sup) => {
            let owner = fresh.next("P");
            atoms.push(atom("fun", subject, &[format!("parent = {owner}")]));
            atoms.push(format!("subclass({owner}, {sup})"));
        }
        None => atoms.push(atom("fun", subject, &[format!("parent = {class_var}")])),
    }
    match kind {
        "signature_match" => {
            let names = collect_strings(inner, "name", "names");
            if names.is_empty() {
                report.warn(ctx.index, "class predicate with no name; skipped");
                return Vec::new();
            }
            if names.len() == 1 {
                atoms.push(format!("{class_var} = {}", quote(&names[0])));
            } else {
                atoms.push(format!(
                    "{class_var} in {{{}}}",
                    names
                        .iter()
                        .map(|n| quote(n))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        "name" => match inner.get("pattern").and_then(|v| v.as_str()) {
            Some(pattern) => atoms.push(format!("regex_match({class_var}, {})", quote(pattern))),
            None => {
                report.warn(
                    ctx.index,
                    "class 'name' predicate with no 'pattern'; skipped",
                );
                return Vec::new();
            }
        },
        other => {
            report.warn(
                ctx.index,
                format!("'{other}' is not a class predicate; skipped"),
            );
            return Vec::new();
        }
    }
    vec![atoms]
}

// ---------------------------------------------------------------------------
// Models → heads
// ---------------------------------------------------------------------------

struct HeadText {
    text: String,
    /// A bridge head brings its own side-B atoms.
    needs_side_b: bool,
    extra_body: Vec<String>,
}

fn migrate_model(
    index: usize,
    generator: &Value,
    anchor: &str,
    fresh: &mut Fresh,
    report: &mut MigrationReport,
) -> Vec<HeadText> {
    let mut heads = Vec::new();
    let Some(model) = generator.get("model") else {
        report.warn(index, "no 'model'; nothing to derive");
        return heads;
    };

    for key in ["taint", "modes", "forward_self"] {
        if model.get(key).is_some() {
            report.warn(
                index,
                format!("'{key}' has no effect in the JSON loader either, and is not translated"),
            );
        }
    }

    if let Some(items) = model.get("sources").and_then(|v| v.as_array()) {
        for item in items {
            let Some(port) = port_text(item.get("port"), index, report) else {
                continue;
            };
            let mut attrs = vec![format!("kind = {}", quote(kind_of(item)))];
            if item.get("saturating").and_then(|v| v.as_bool()) == Some(true) {
                attrs.push("saturating = true".to_string());
            }
            heads.push(HeadText {
                text: format!("source({anchor}::{port}, {})", attrs.join(", ")),
                needs_side_b: false,
                extra_body: Vec::new(),
            });
        }
    }
    if let Some(items) = model.get("sinks").and_then(|v| v.as_array()) {
        for item in items {
            let Some(port) = port_text(item.get("port"), index, report) else {
                continue;
            };
            let mut attrs = vec![format!("kind = {}", quote(kind_of(item)))];
            // `wildcard` defaults to true in both formats, so only `false` is written out.
            if item.get("wildcard").and_then(|v| v.as_bool()) == Some(false) {
                attrs.push("wildcard = false".to_string());
            }
            heads.push(HeadText {
                text: format!("sink({anchor}::{port}, {})", attrs.join(", ")),
                needs_side_b: false,
                extra_body: Vec::new(),
            });
        }
    }
    if let Some(items) = model.get("propagation").and_then(|v| v.as_array()) {
        for item in items {
            let (Some(input), Some(output)) = (
                port_text(item.get("input"), index, report),
                port_text(item.get("output"), index, report),
            ) else {
                continue;
            };
            heads.push(HeadText {
                text: format!("propagation({anchor}::{output} <- {anchor}::{input})"),
                needs_side_b: false,
                extra_body: Vec::new(),
            });
        }
    }
    if let Some(items) = model.get("access_paths").and_then(|v| v.as_array()) {
        for item in items {
            match item.as_str() {
                Some(text) => heads.push(HeadText {
                    text: format!("access_paths({})", quote(text)),
                    needs_side_b: false,
                    extra_body: Vec::new(),
                }),
                None => report.warn(index, "'access_paths' entry is not a string; skipped"),
            }
        }
    }
    if let Some(bridge) = model.get("bridge") {
        heads.extend(migrate_bridge(index, bridge, anchor, fresh, report));
    }
    heads
}

fn migrate_bridge(
    index: usize,
    bridge: &Value,
    anchor: &str,
    fresh: &mut Fresh,
    report: &mut MigrationReport,
) -> Vec<HeadText> {
    let Some(to) = bridge.get("to") else {
        report.warn(index, "'bridge' with no 'to' block; skipped");
        return Vec::new();
    };
    for key in ["on-ambiguous"] {
        if bridge.get(key).is_some() {
            report.warn(
                index,
                format!(
                    "'{key}' is a pairing diagnostic for an unevaluated bridge; a DSL rule is \
                     already grounded, so it is not translated"
                ),
            );
        }
    }

    // Side B's own match block, compiled with `G` as its subject.
    let scope = scope_attrs(to.get("in"));
    let ctx = Ctx {
        index,
        subject: OTHER,
        callsites: false,
    };
    let expr = match to.get("where").and_then(|v| v.as_array()) {
        Some(items) => Expr::And(items.iter().map(Expr::Prim).collect()),
        None => Expr::And(Vec::new()),
    };
    let disjuncts = compile(&expr, false, &ctx, fresh, report);
    if disjuncts.len() > 1 {
        report.warn(
            index,
            "the bridge's 'to' block has a disjunction in it; only the first alternative is \
             carried across. Split the generator instead.",
        );
    }
    let side_b = with_subject(
        disjuncts.first().cloned().unwrap_or_default(),
        OTHER,
        &scope,
    );

    let pairs = match bridge.get("arguments").and_then(|v| v.as_array()) {
        Some(items) => items.clone(),
        None => {
            report.warn(
                index,
                "'bridge' with no 'arguments' map: the JSON loader falls back to an identity map \
                 over the arity the two sides share, which needs the fact base and cannot be \
                 written as a rule. Only the return value is carried across; write the map.",
            );
            vec![serde_json::json!({"from": "Return", "to": "Return", "direction": "both"})]
        }
    };

    let mut heads = Vec::new();
    for pair in &pairs {
        let (Some(from), Some(to_port)) = (
            port_text(pair.get("from"), index, report),
            port_text(pair.get("to"), index, report),
        ) else {
            continue;
        };
        let arrow = match pair.get("direction").and_then(|v| v.as_str()) {
            Some("in") | None => "->",
            Some("out") => "<-",
            Some("both") => "<->",
            Some(other) => {
                report.warn(
                    index,
                    format!("unknown bridge direction '{other}'; skipped"),
                );
                continue;
            }
        };
        heads.push(HeadText {
            text: format!("bridge({anchor}::{from} {arrow} {OTHER}::{to_port})"),
            needs_side_b: true,
            extra_body: side_b.clone(),
        });
    }
    heads
}

fn kind_of(item: &Value) -> &str {
    item.get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or(super::parse::default_label())
}

/// `Argument(0).deref` → `arg(0).deref`, `Return` → `return`, `Argument(*)` → `arg(_)`.
fn port_text(value: Option<&Value>, index: usize, report: &mut MigrationReport) -> Option<String> {
    let text = match value.and_then(|v| v.as_str()) {
        Some(t) => t,
        None => {
            report.warn(index, "a port is missing or is not a string; skipped");
            return None;
        }
    };
    let parsed = match super::super::json::parse_port(text, index) {
        Ok(p) => p,
        Err(e) => {
            report.warn(index, format!("port {text:?}: {e}"));
            return None;
        }
    };
    let base = match parsed.tag {
        crate::models::FormalIndexTypeTag::Return => "return".to_string(),
        crate::models::FormalIndexTypeTag::AnyArgument => "arg(_)".to_string(),
        crate::models::FormalIndexTypeTag::Index => {
            format!("arg({})", parsed.index.expect("Index carries one"))
        }
        crate::models::FormalIndexTypeTag::Local => {
            report.warn(
                index,
                format!(
                    "port {text:?}: 'Variable(...)' selects a named local, which the DSL does \
                     not spell; skipped"
                ),
            );
            return None;
        }
        crate::models::FormalIndexTypeTag::Global => {
            report.warn(
                index,
                format!("port {text:?}: the globals port is implicit"),
            );
            return None;
        }
    };
    let mut out = base;
    for seg in &parsed.ap {
        out.push_str(&segment_text(seg));
    }
    Some(out)
}

/// One access-path segment in DSL syntax. A name that is not a plain identifier is quoted, which
/// is the DSL's way of taking it out of the path grammar — `.\[]` in JSON becomes `."[]"` here.
pub fn segment_text(seg: &PathSegment) -> String {
    match seg {
        PathSegment::Offset(off) => format!(".[{}]", off.0),
        PathSegment::Symbol(sym) => {
            let s: &str = sym;
            let plain = !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_$-%*#<>".contains(c));
            if plain {
                format!(".{s}")
            } else {
                format!(".{}", quote(s))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Fresh variable names, so two constraints in one rule never collide.
#[derive(Default)]
struct Fresh {
    next: usize,
}

impl Fresh {
    fn next(&mut self, stem: &str) -> String {
        let n = self.next;
        self.next += 1;
        format!("{stem}{n}")
    }
}

fn atom(name: &str, subject: &str, attrs: &[String]) -> String {
    if attrs.is_empty() {
        format!("{name}({subject})")
    } else {
        format!("{name}({subject}, {})", attrs.join(", "))
    }
}

/// `!a` for one atom, `!(a && b)` for several.
///
/// The parenthesized form is not sugar: negating each atom on its own would test them
/// independently and lose the variable they share, so `not name{pattern}` — a `fun(F, name = N)`
/// and a `regex_match(N, …)` — would stop meaning "no name of F matches".
fn negate(atoms: &[String]) -> String {
    match atoms.len() {
        0 => "true = true".to_string(),
        1 => format!("!{}", atoms[0]),
        _ => format!("!({})", atoms.join(" && ")),
    }
}

fn dsl_op(kind: &str) -> Option<&'static str> {
    match kind {
        "<" => Some("<"),
        "<=" => Some("<="),
        ">" => Some(">"),
        ">=" => Some(">="),
        "!=" => Some("!="),
        "==" => Some("="),
        _ => None,
    }
}

/// `name`/`names` (or any singular/plural pair) as one attribute constraint.
fn string_attr(value: &Value, single: &str, plural: &str, attr: &str) -> Option<String> {
    let items = collect_strings(value, single, plural);
    match items.len() {
        0 => None,
        1 => Some(format!("{attr} = {}", quote(&items[0]))),
        _ => Some(format!(
            "{attr} in {{{}}}",
            items
                .iter()
                .map(|s| quote(s))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn collect_strings(value: &Value, single: &str, plural: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = value.get(plural).and_then(|v| v.as_array()) {
        out.extend(arr.iter().filter_map(|v| v.as_str()).map(str::to_string));
    }
    if let Some(one) = value.get(single).and_then(|v| v.as_str()) {
        out.push(one.to_string());
    }
    out
}

/// A DSL string literal. Only `"` and `\` need escaping; a quoted access-path segment carries
/// its dots and brackets literally.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}
