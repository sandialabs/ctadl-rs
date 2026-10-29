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
approximate: `any_of` was already a union over the matched set. The empty disjunction is
"false": a generator whose `where` can never match writes no rule, as it derives nothing.

# What does not translate

A migration either says exactly what the JSON loader would have done or fails; it never writes
a rule that matches more or less. Three cases:

- **The loader rejects the generator.** The migration fails with the loader's own error, because
  the loader itself is run over each generator first (see [`json_loader_errors`]).
- **The loader accepts it and it has no effect.** `taint` and `forward_self` are schema-only, a
  `modes` list without `skip-analysis` does nothing, and `on-unmatched` / `on-ambiguous` other
  than `"error"` change only what is logged. These are warnings.
- **The loader accepts it with a meaning the DSL cannot say.** `Variable(...)` ports, the
  `skip-analysis` mode, a bridge with no `arguments` map (or an empty one), a scoped dispatch
  generator, `resolve: "inline"` with a `propagation` list, an `"error"` severity, and an
  `in_function` under `any_of` / `not`. Each is a [`JsonModelError::Untranslatable`] that says
  why.
*/

use std::fmt::Write as _;

use serde_json::Value;

use ctadl_ir::mir::PathSegment;
use ctadl_ir::mir::call::VirtualMethodTable;

use crate::error::{Error, JsonModelError, JsonModelErrors};
use crate::models::spec;

/// What a migration produced, and what it left out because the JSON loader ignores it too.
#[derive(Clone, Debug, Default)]
pub struct MigrationReport {
    /// One line per construct that was not translated because it has no effect, in generator
    /// order. Nothing that changes what a generator matches or derives is ever only a warning.
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
///
/// Fails with every generator's errors if any generator cannot be translated exactly: either
/// the JSON loader rejects it, and the error is the one the loader raises, or it means something
/// the DSL cannot say, which is a [`JsonModelError::Untranslatable`].
pub fn migrate_generators<'a>(
    generators: impl IntoIterator<Item = &'a Value>,
    header: Option<&str>,
) -> Result<(String, MigrationReport), Error> {
    let mut out = String::new();
    let mut report = MigrationReport::default();
    let mut errors = JsonModelErrors::default();
    if let Some(header) = header {
        for line in header.lines() {
            let _ = writeln!(out, "// {line}");
        }
        out.push('\n');
    }
    for (i, value) in generators.into_iter().enumerate() {
        report.generators += 1;
        let loader_errors = json_loader_errors(i, value);
        if !loader_errors.is_empty() {
            errors.extend(loader_errors);
            continue;
        }
        let before = out.len();
        if let Err(e) = migrate_generator(i, value, &mut out, &mut report) {
            errors.push(e);
            continue;
        }
        if out.len() == before {
            // Every way of losing a head is an error, so what reaches here derives nothing in
            // the JSON loader either: no `model`, empty lists, only inert keys.
            let _ = writeln!(out, "// generator {i}: produced no rule");
        }
        out.push('\n');
    }
    if !errors.is_empty() {
        return Err(Error::JsonModel(errors));
    }
    Ok((out, report))
}

/// What the JSON loader reports against generator `index`, in the order it reports it.
///
/// The loader is run rather than re-implemented: its checks are interleaved with matching, and a
/// copy of them would be a second set of rules to keep in step. A bridge is scanned first, as
/// [`spec::scan_model_files`] does before any import is loaded. A clean scan is followed by the
/// ordinary match pass, plus side B's `where`, which the loader evaluates once a bridge's scope
/// admits an import.
///
/// The program is empty, so nothing matches and no per-function check fires. Its method table
/// is an empty Java one rather than none, so that `parent` and `extends` do not log that the
/// frontend has no class hierarchy. The import is made up from the generator's own `in`, so the
/// scope never hides an error. That is stricter than the loader, which skips the rest of a
/// generator its scope excludes, but a migrated file is not tied to one import.
fn json_loader_errors(index: usize, generator: &Value) -> Vec<JsonModelError> {
    use crate::models::json::{ModelGeneratorIngest, ModelGeneratorVisitor};
    use crate::models::{ImportScope, ProgramMatchIndex, ProgramModelMatches};

    let mut errors = Vec::new();
    let bridge = match generator.pointer("/model/bridge") {
        Some(bridge) => {
            let path = std::path::Path::new("<migrate>");
            let spec = spec::parse_bridge(path, index, generator, bridge, &mut errors);
            if !errors.is_empty() {
                return errors;
            }
            spec
        }
        None => None,
    };
    let scope = spec::ProgramScope::parse(generator.get("in"), index, &mut errors);
    if !errors.is_empty() {
        return errors;
    }
    let program = ctadl_ir::mir::ProgramInfo {
        vmt: VirtualMethodTable::Java {
            methods: Vec::new(),
            hierarchy: Default::default(),
            interfaces: Vec::new(),
            abstract_methods: Vec::new(),
            natives: Vec::new(),
        },
        ..Default::default()
    };
    let import = ImportScope {
        language: scope.languages.first().copied(),
        import: scope.import,
    };
    let match_index = ProgramMatchIndex::new(&program, import);
    let mut out = ProgramModelMatches::default();
    let mut ingest = ModelGeneratorIngest::new(&match_index, &mut out);
    ingest.visit_model_generator(index, generator);
    if let Some(bridge) = bridge {
        ingest.match_where(index, &bridge.to.where_);
    }
    ingest.drain_errors()
}

/// A construct the JSON loader accepts and the DSL cannot express.
fn untranslatable(index: usize, what: &str, message: impl Into<String>) -> JsonModelError {
    JsonModelError::Untranslatable {
        index,
        what: what.to_string(),
        message: message.into(),
    }
}

/// Reads a `.json` / `.json5` / `.jsonl` model file and returns the DSL source for it.
pub fn migrate_file(path: &std::path::Path) -> Result<(String, MigrationReport), Error> {
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
                    return Err(Error::from(std::io::Error::new(
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
    migrate_generators(generators.iter(), Some(&header))
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
/// The signature key of a `find: dispatch` generator.
const DISPATCH_KEY: &str = "K";

fn migrate_generator(
    index: usize,
    generator: &Value,
    out: &mut String,
    report: &mut MigrationReport,
) -> Result<(), JsonModelError> {
    let (callsites, dispatch) = match generator.get("find").and_then(|v| v.as_str()) {
        Some("methods") => (false, false),
        Some("callsites") => (true, false),
        Some("dispatch") => (false, true),
        other => {
            return Err(JsonModelError::UnexpectedConstraint {
                index,
                constraint_type: other.unwrap_or("<missing>").to_string(),
            });
        }
    };
    // `fun` for a method or call-site generator, `callsig` for a dispatch one. The subject is
    // renamed with it so a migrated file reads as what it ranges over.
    let (relation, subject) = if dispatch {
        ("callsig", DISPATCH_KEY)
    } else {
        ("fun", SUBJECT)
    };

    let scope = scope_attrs(generator.get("in"));
    if generator.get("on-unmatched").is_some() && generator.pointer("/model/bridge").is_none() {
        report.warn(
            index,
            "'on-unmatched' is read only for a bridge, so it has no effect here either",
        );
    }
    let mut fresh = Fresh::default();

    let mut base: Vec<String> = Vec::new();
    if callsites {
        base.push(format!(
            "callsite({CALLER}, {SITE}, callee_string = {SUBJECT})"
        ));
    }

    let expr = where_expr(index, generator.get("where"), "where")?;
    let ctx = Ctx {
        index,
        subject,
        callsites,
        relation,
    };
    let disjuncts = compile(&expr, false, &ctx, &mut fresh)?;

    let anchor = if callsites { SITE } else { subject };
    let heads = if dispatch {
        Heads {
            plain: migrate_dispatch_model(index, generator, anchor)?,
            bridge: None,
        }
    } else {
        migrate_model(index, generator, anchor, &mut fresh, report)?
    };
    if heads.plain.is_empty() && heads.bridge.is_none() {
        return Ok(());
    }
    if disjuncts.is_empty() {
        let _ = writeln!(out, "// generator {index}: its 'where' can never match");
        return Ok(());
    }

    if !heads.plain.is_empty() {
        for disjunct in &disjuncts {
            let mut body = base.clone();
            body.extend(with_subject(disjunct.clone(), relation, subject, &scope));
            write_rule(out, &heads.plain, &body);
            report.rules += 1;
        }
    }
    // A bridge is written as its own rule: side B has a whole match block of its own, and
    // folding it into the shared body would make every other head depend on it. It is *one*
    // rule, with any disjunction kept inside it: the engine makes one bridge per (rule, from,
    // to), so a rule per port would split the port map across several bridges, and a rule per
    // disjunct would bridge a pair twice when it satisfied two of them.
    if let Some(bridge) = heads.bridge
        && let Some(side_b) = bridge.side_b
    {
        let mut body = base;
        body.extend(with_subject(one_of(&disjuncts), relation, subject, &scope));
        body.extend(side_b);
        write_rule(out, &bridge.heads, &body);
        report.rules += 1;
    }
    Ok(())
}

/// A `where` list, as a conjunction. An absent one is the empty conjunction, which matches
/// everything, as it does in the JSON loader.
fn where_expr<'a>(
    index: usize,
    value: Option<&'a Value>,
    field: &str,
) -> Result<Expr<'a>, JsonModelError> {
    match value {
        None => Ok(Expr::And(Vec::new())),
        Some(v) => match v.as_array() {
            Some(items) => Ok(Expr::And(items.iter().map(Expr::Prim).collect())),
            None => Err(JsonModelError::FieldNotArray {
                index,
                field_name: field.to_string(),
            }),
        },
    }
}

/// A non-empty DNF as body atoms: its one conjunction when it has one, otherwise one filter that
/// holds when any conjunction does.
///
/// The filter is spelled `!(!a && !b)` rather than `a || b` because only a negated group may have
/// locals of its own, and a conjunction usually has some (the `N` a `name` regex binds). It
/// binds nothing, so the subject must be bound before it, which [`with_subject`] sees to. That
/// makes it the same union as one rule per conjunction, without the duplicate groundings.
fn one_of(disjuncts: &Dnf) -> Vec<String> {
    if disjuncts.len() == 1 {
        return disjuncts[0].clone();
    }
    if disjuncts.iter().any(|d| d.is_empty()) {
        return Vec::new();
    }
    let negated: Vec<String> = disjuncts.iter().map(|d| negate(d)).collect();
    vec![format!("!({})", negated.join(" && "))]
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
/// to be bound by *something*. Either way that atom goes first: a filter such as a leading
/// `not` can only test the subject once it is bound.
fn with_subject(
    mut atoms: Vec<String>,
    relation: &str,
    subject: &str,
    scope: &[String],
) -> Vec<String> {
    let open = format!("{relation}({subject}, ");
    let bare = format!("{relation}({subject})");
    match atoms
        .iter()
        .position(|a| a.starts_with(&open) || *a == bare)
    {
        Some(i) => {
            let mut binder = atoms.remove(i);
            if !scope.is_empty() {
                binder = match binder.strip_prefix(&open) {
                    Some(rest) => format!("{open}{}, {rest}", scope.join(", ")),
                    None => atom(relation, subject, scope),
                };
            }
            atoms.insert(0, binder);
        }
        None => atoms.insert(0, atom(relation, subject, scope)),
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

#[derive(Clone, Copy)]
struct Ctx<'a> {
    index: usize,
    subject: &'a str,
    callsites: bool,
    /// The relation the subject ranges over: `fun` for `find: methods` / `callsites`,
    /// `callsig` for `find: dispatch`. The attribute names are the same in both, which is what
    /// lets one constraint compiler serve them.
    relation: &'a str,
}

/// A disjunction of conjunctions of body atoms, as text. The empty disjunction is false, and a
/// disjunction holding the empty conjunction is true.
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
) -> Result<Dnf, JsonModelError> {
    match (expr, negated) {
        // De Morgan: the negation is pushed to the leaves, where a single atom can carry it.
        (Expr::And(items), false) | (Expr::Or(items), true) => {
            let mut acc: Dnf = vec![Vec::new()];
            for item in items {
                acc = dnf_and(acc, compile(item, negated, ctx, fresh)?);
            }
            Ok(acc)
        }
        (Expr::Or(items), false) | (Expr::And(items), true) => {
            let mut acc: Dnf = Vec::new();
            for item in items {
                acc.extend(compile(item, negated, ctx, fresh)?);
            }
            // Empty for an empty `any_of`, which matches nothing in the JSON loader too.
            Ok(acc)
        }
        (Expr::Prim(value), n) => compile_prim(value, n, ctx, fresh),
    }
}

fn compile_prim(
    value: &Value,
    negated: bool,
    ctx: &Ctx<'_>,
    fresh: &mut Fresh,
) -> Result<Dnf, JsonModelError> {
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
                .ok_or_else(|| JsonModelError::FieldNotArray {
                    index: ctx.index,
                    field_name: "inners".to_string(),
                })?
                .iter()
                .map(Expr::Prim)
                .collect();
            let expr = if kind == "all_of" {
                Expr::And(inners)
            } else {
                Expr::Or(inners)
            };
            return compile(&expr, negated, ctx, fresh);
        }
        "not" => {
            let inner = value
                .get("inner")
                .ok_or_else(|| JsonModelError::MissingField {
                    index: ctx.index,
                    field_name: "inner".to_string(),
                })?;
            return compile(&Expr::Prim(inner), !negated, ctx, fresh);
        }
        _ => {}
    }
    let alternatives = leaf_atoms(value, ctx, fresh)?;
    if !negated {
        return Ok(alternatives);
    }
    // `!(a || b)` is `!a && !b`; each alternative contributes one negated conjunct. An
    // alternative with no atoms is true, so the negation of the whole is false.
    let mut conj: Vec<String> = Vec::new();
    for atoms in alternatives {
        if atoms.is_empty() {
            return Ok(Vec::new());
        }
        conj.push(negate(&atoms));
    }
    Ok(vec![conj])
}

/// One leaf constraint's body atoms, as a disjunction (usually of one).
fn leaf_atoms(value: &Value, ctx: &Ctx<'_>, fresh: &mut Fresh) -> Result<Dnf, JsonModelError> {
    let subject = ctx.subject;
    let index = ctx.index;
    let missing = |field: &str| JsonModelError::MissingField {
        index,
        field_name: field.to_string(),
    };
    let kind = value
        .get("constraint")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match kind {
        "signature_match" => {
            let pairs = [
                ("name", "names"),
                ("parent", "parents"),
                ("qualified-id", "qualified-ids"),
            ];
            let mut attrs = Vec::new();
            for (single, plural) in pairs {
                // A field that is present and names nothing narrows to nothing, as it does in
                // the JSON loader: leaving it out would match every function.
                if value.get(single).is_none() && value.get(plural).is_none() {
                    continue;
                }
                match string_attr(value, single, plural, single) {
                    Some(a) => attrs.push(a),
                    None => return Ok(Vec::new()),
                }
            }
            if attrs.is_empty() {
                return Err(missing(
                    "name' / 'names' / 'parent' / 'parents' / 'qualified-id' / 'qualified-ids",
                ));
            }
            Ok(vec![vec![atom(ctx.relation, subject, &attrs)]])
        }
        "name" => {
            let pattern = value
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| missing("pattern"))?;
            let v = fresh.next("N");
            Ok(vec![vec![
                atom(ctx.relation, subject, &[format!("name = {v}")]),
                format!("regex_match({v}, {})", quote(pattern)),
            ]])
        }
        "signature" | "signature_pattern" => {
            let pattern = value
                .get("pattern")
                .and_then(|v| v.as_str())
                .ok_or_else(|| missing("pattern"))?;
            let v = fresh.next("Sig");
            Ok(vec![vec![
                atom(ctx.relation, subject, &[format!("signature = {v}")]),
                format!("regex_match({v}, {})", quote(pattern)),
            ]])
        }
        "has_code" => {
            let b = value
                .get("value")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| missing("value"))?;
            Ok(vec![vec![atom(
                ctx.relation,
                subject,
                &[format!("has_code = {b}")],
            )]])
        }
        "uses_field" => {
            if value.get("name").is_none() && value.get("names").is_none() {
                return Err(missing("name' / 'names"));
            }
            let names = collect_strings(value, "name", "names");
            match names.len() {
                // Present and naming nothing: no function matches, as in the JSON loader.
                0 => Ok(Vec::new()),
                1 => Ok(vec![vec![format!(
                    "uses_field({subject}, {})",
                    quote(&names[0])
                )]]),
                _ => {
                    let v = fresh.next("Fld");
                    Ok(vec![vec![
                        format!("uses_field({subject}, {v})"),
                        format!(
                            "{v} in {{{}}}",
                            names
                                .iter()
                                .map(|n| quote(n))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ]])
                }
            }
        }
        "number_parameters" => {
            let inner = value.get("inner").ok_or_else(|| missing("inner"))?;
            int_predicate(inner, subject, fresh, ctx)
        }
        "parent" => {
            let inner = value.get("inner").ok_or_else(|| missing("inner"))?;
            class_predicate(inner, subject, false, fresh, ctx)
        }
        "extends" => {
            let inner = value.get("inner").ok_or_else(|| missing("inner"))?;
            class_predicate(inner, subject, true, fresh, ctx)
        }
        "in_function" => {
            if !ctx.callsites {
                return Err(JsonModelError::UnexpectedField {
                    index,
                    field_name: "in_function".to_string(),
                    message: "'in_function' is only supported with find: callsites".to_string(),
                });
            }
            let inner = value.get("inner").ok_or_else(|| missing("inner"))?;
            // Always the caller, under `any_of` and `not` too. The JSON matcher narrows the
            // combinator's scratch copy of the *callee* set there, which is a bug in it, not a
            // meaning to carry across.
            let caller_ctx = Ctx {
                index,
                subject: CALLER,
                callsites: false,
                relation: "fun",
            };
            compile(&Expr::Prim(inner), false, &caller_ctx, fresh)
        }
        other => Err(JsonModelError::UnexpectedConstraint {
            index,
            constraint_type: if other.is_empty() { "<missing>" } else { other }.to_string(),
        }),
    }
}

/// The `inner` of `number_parameters`, against the subject's arity.
fn int_predicate(
    inner: &Value,
    subject: &str,
    fresh: &mut Fresh,
    ctx: &Ctx<'_>,
) -> Result<Dnf, JsonModelError> {
    let kind = inner
        .get("constraint")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if let Some(op) = dsl_op(kind)
        && let Some(v) = inner.get("value").and_then(|v| v.as_i64())
    {
        // `arity < -1` needs the space: `<-` is the flow arrow. `format!` writes one anyway,
        // but the rule is worth stating where the string is built.
        return Ok(vec![vec![atom(
            ctx.relation,
            subject,
            &[format!("arity {op} {v}")],
        )]]);
    }
    // Anything richer becomes a bound arity plus a boolean expression over it.
    let var = fresh.next("A");
    match int_expr(inner, &var) {
        Some(IntExpr::Const(true)) => Ok(vec![Vec::new()]),
        Some(IntExpr::Const(false)) => Ok(Vec::new()),
        Some(IntExpr::Text(expr)) => Ok(vec![vec![
            atom(ctx.relation, subject, &[format!("arity = {var}")]),
            expr,
        ]]),
        // The loader validates the same tree and rejects every shape that lands here.
        None => Err(JsonModelError::UnexpectedConstraint {
            index: ctx.index,
            constraint_type: kind.to_string(),
        }),
    }
}

/// A boolean expression over an arity, with the empty combinators folded away: the DSL has no
/// `true` or `false` to write for them.
enum IntExpr {
    Const(bool),
    Text(String),
}

fn int_expr(inner: &Value, var: &str) -> Option<IntExpr> {
    let kind = inner.get("constraint").and_then(|v| v.as_str())?;
    match kind {
        "any_of" | "all_of" => {
            // `any_of` is decided by a true part and `all_of` by a false one; the other
            // constant drops out.
            let decisive = kind == "any_of";
            let mut parts = Vec::new();
            for i in inner.get("inners")?.as_array()? {
                match int_expr(i, var)? {
                    IntExpr::Const(b) if b == decisive => return Some(IntExpr::Const(b)),
                    IntExpr::Const(_) => {}
                    IntExpr::Text(t) => parts.push(t),
                }
            }
            if parts.is_empty() {
                return Some(IntExpr::Const(!decisive));
            }
            let sep = if decisive { " || " } else { " && " };
            Some(IntExpr::Text(format!("({})", parts.join(sep))))
        }
        "not" => Some(match int_expr(inner.get("inner")?, var)? {
            IntExpr::Const(b) => IntExpr::Const(!b),
            IntExpr::Text(t) => IntExpr::Text(format!("!({t})")),
        }),
        other => {
            let op = dsl_op(other)?;
            let v = inner.get("value")?.as_i64()?;
            Some(IntExpr::Text(format!("{var} {op} {v}")))
        }
    }
}

/// The `inner` of `parent` / `extends`, against the subject's owning class.
///
/// The inner is a boolean tree over *one* class, so it is compiled as one formula over one
/// class variable, bound once: to the owning class for `parent`, to one *direct* supertype of
/// it for `extends`, which is what the JSON tests (it reads the hierarchy table, not its
/// closure). Spreading the tree through the rule-level DNF instead would bind a class per leaf,
/// and `extends{all_of[A, B]}` would hold for a class with one supertype named `A` and another
/// named `B`.
///
/// On Lua the class is a metatable-recovered one, which the DSL's `parent` and `subclass` cover
/// and the JSON's did not (it matched nothing there): a migrated rule takes the DSL's meaning.
fn class_predicate(
    inner: &Value,
    subject: &str,
    extends: bool,
    fresh: &mut Fresh,
    ctx: &Ctx<'_>,
) -> Result<Dnf, JsonModelError> {
    let formula = class_formula(inner, ctx)?;
    if matches!(formula, ClassFormula::Const(false)) {
        return Ok(Vec::new());
    }
    // A predicate that holds of every class still requires there to be one.
    let any = matches!(formula, ClassFormula::Const(true));
    let mut attrs = Vec::new();
    let mut atoms = Vec::new();
    let class_var = if extends {
        let owner = fresh.next("P");
        let sup = if any {
            "_".to_string()
        } else {
            fresh.next("Sup")
        };
        attrs.push(format!("parent = {owner}"));
        atoms.push(format!("subclass({owner}, {sup})"));
        sup
    } else {
        let owner = if any {
            "_".to_string()
        } else {
            fresh.next("P")
        };
        attrs.push(format!("parent = {owner}"));
        owner
    };
    atoms.insert(0, atom(ctx.relation, subject, &attrs));
    if !any {
        formula.conjuncts(&class_var, &mut atoms);
    }
    Ok(vec![atoms])
}

/// A `parent` / `extends` inner, as a formula over one class.
enum ClassFormula {
    Const(bool),
    /// Equal to one of these names.
    Is(Vec<String>),
    /// Matches this pattern anywhere: the JSON uses `is_match`, and so does `regex_match`.
    Matches(String),
    Not(Box<ClassFormula>),
    And(Vec<ClassFormula>),
    Or(Vec<ClassFormula>),
}

/// Reads a class predicate the way the JSON's `eval_predicate` does, folding what it can decide
/// without a class. Every error is one the JSON loader's `validate_predicate` raises too.
fn class_formula(inner: &Value, ctx: &Ctx<'_>) -> Result<ClassFormula, JsonModelError> {
    let index = ctx.index;
    let kind = inner
        .get("constraint")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match kind {
        "any_of" | "all_of" => {
            let inners = inner
                .get("inners")
                .and_then(|v| v.as_array())
                .ok_or_else(|| JsonModelError::FieldNotArray {
                    index,
                    field_name: "inners".to_string(),
                })?;
            let parts = inners
                .iter()
                .map(|i| class_formula(i, ctx))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(if kind == "all_of" {
                ClassFormula::and(parts)
            } else {
                ClassFormula::or(parts)
            })
        }
        "not" => {
            let i = inner
                .get("inner")
                .ok_or_else(|| JsonModelError::MissingField {
                    index,
                    field_name: "inner".to_string(),
                })?;
            Ok(ClassFormula::not(class_formula(i, ctx)?))
        }
        // The subject is already a class, so only `name` / `names` mean anything; the JSON
        // loader rejects any other key, and a predicate with neither.
        "signature_match" => {
            let other = inner
                .as_object()
                .into_iter()
                .flat_map(|o| o.keys())
                .find(|k| !matches!(k.as_str(), "constraint" | "name" | "names"));
            if let Some(key) = other {
                return Err(JsonModelError::UnexpectedField {
                    index,
                    field_name: key.clone(),
                    message: "not a recognized field of the 'signature_match' constraint; \
                              expected one of 'name', 'names'"
                        .to_string(),
                });
            }
            if inner.get("name").is_none() && inner.get("names").is_none() {
                return Err(JsonModelError::MissingField {
                    index,
                    field_name: "name' / 'names".to_string(),
                });
            }
            // Present and naming nothing: no class is equal to it, as in the JSON loader.
            let names = collect_strings(inner, "name", "names");
            Ok(if names.is_empty() {
                ClassFormula::Const(false)
            } else {
                ClassFormula::Is(names)
            })
        }
        "name" => match inner.get("pattern").and_then(|v| v.as_str()) {
            Some(pattern) => Ok(ClassFormula::Matches(pattern.to_string())),
            None => Err(JsonModelError::FieldNotString {
                index,
                field_name: "pattern".to_string(),
            }),
        },
        other => Err(JsonModelError::UnexpectedConstraint {
            index,
            constraint_type: if other.is_empty() { "<missing>" } else { other }.to_string(),
        }),
    }
}

impl ClassFormula {
    fn and(parts: Vec<Self>) -> Self {
        Self::fold(parts, true)
    }

    fn or(parts: Vec<Self>) -> Self {
        Self::fold(parts, false)
    }

    /// `and` when `unit` is true, `or` when it is false: `unit` is the empty case, and its
    /// opposite absorbs the rest. So an empty `all_of` holds and an empty `any_of` does not,
    /// as `Iterator::all` / `any` decide them in the JSON.
    fn fold(parts: Vec<Self>, unit: bool) -> Self {
        let mut kept = Vec::new();
        for part in parts {
            match part {
                Self::Const(b) if b == unit => {}
                Self::Const(_) => return Self::Const(!unit),
                other => kept.push(other),
            }
        }
        match kept.len() {
            0 => Self::Const(unit),
            1 => kept.pop().expect("checked len"),
            _ if unit => Self::And(kept),
            _ => Self::Or(kept),
        }
    }

    fn not(inner: Self) -> Self {
        match inner {
            Self::Const(b) => Self::Const(!b),
            Self::Not(x) => *x,
            other => Self::Not(Box::new(other)),
        }
    }

    /// Appends the formula to a body as conjuncts over `var`; a top-level `all_of` is several.
    fn conjuncts(&self, var: &str, out: &mut Vec<String>) {
        match self {
            Self::And(parts) => {
                for p in parts {
                    p.conjuncts(var, out);
                }
            }
            other => out.push(other.text(var)),
        }
    }

    /// One self-delimiting operand: a compound is parenthesized, so it can stand in a `&&`
    /// group or under a `!` as written. Constants are folded away before this is reached.
    fn text(&self, var: &str) -> String {
        match self {
            Self::Const(_) => unreachable!("constants are folded before a formula is written"),
            Self::Is(names) if names.len() == 1 => format!("{var} = {}", quote(&names[0])),
            Self::Is(names) => format!(
                "{var} in {{{}}}",
                names
                    .iter()
                    .map(|n| quote(n))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Matches(pattern) => format!("regex_match({var}, {})", quote(pattern)),
            Self::Not(inner) => match &**inner {
                Self::Is(names) if names.len() == 1 => {
                    format!("{var} != {}", quote(&names[0]))
                }
                Self::Matches(_) | Self::And(_) | Self::Or(_) => format!("!{}", inner.text(var)),
                other => format!("!({})", other.text(var)),
            },
            Self::And(parts) => Self::join(parts, var, " && "),
            Self::Or(parts) => Self::join(parts, var, " || "),
        }
    }

    fn join(parts: &[Self], var: &str, sep: &str) -> String {
        let parts: Vec<String> = parts.iter().map(|p| p.text(var)).collect();
        format!("({})", parts.join(sep))
    }
}

// ---------------------------------------------------------------------------
// Models → heads
// ---------------------------------------------------------------------------

/// The heads one generator derives.
struct Heads {
    /// Heads that need nothing but the generator's own body.
    plain: Vec<String>,
    bridge: Option<BridgeHeads>,
}

/// A bridge's port map, as one head per port, and side B's body atoms. `side_b` is `None` when
/// side B's `where` can never match, so nothing is bridged.
struct BridgeHeads {
    heads: Vec<String>,
    side_b: Option<Vec<String>>,
}

fn migrate_model(
    index: usize,
    generator: &Value,
    anchor: &str,
    fresh: &mut Fresh,
    report: &mut MigrationReport,
) -> Result<Heads, JsonModelError> {
    let mut heads = Heads {
        plain: Vec::new(),
        bridge: None,
    };
    let Some(model) = generator.get("model") else {
        report.warn(
            index,
            "no 'model'; nothing to derive, as in the JSON loader",
        );
        return Ok(heads);
    };

    for key in ["taint", "forward_self"] {
        if model.get(key).is_some() {
            report.warn(
                index,
                format!("'{key}' has no effect in the JSON loader either, and is not translated"),
            );
        }
    }
    if let Some(modes) = model.get("modes") {
        let skip = modes
            .as_array()
            .is_some_and(|m| m.iter().any(|m| m.as_str() == Some("skip-analysis")));
        if skip {
            return Err(untranslatable(
                index,
                "'modes'",
                "'skip-analysis' drops the matched functions' bodies from the index, and the \
                 DSL has no head that says so",
            ));
        }
        report.warn(
            index,
            "'modes' names no mode, so it has no effect in the JSON loader either",
        );
    }

    if let Some(items) = model.get("sources").and_then(|v| v.as_array()) {
        for item in items {
            let port = port_text(item.get("port"), "port", index)?;
            let mut attrs = vec![format!("kind = {}", quote(kind_of(item)))];
            if item.get("saturating").and_then(|v| v.as_bool()) == Some(true) {
                attrs.push("saturating = true".to_string());
            }
            heads
                .plain
                .push(format!("source({anchor}::{port}, {})", attrs.join(", ")));
        }
    }
    if let Some(items) = model.get("sinks").and_then(|v| v.as_array()) {
        for item in items {
            let port = port_text(item.get("port"), "port", index)?;
            let mut attrs = vec![format!("kind = {}", quote(kind_of(item)))];
            // `wildcard` defaults to true in both formats, so only `false` is written out.
            if item.get("wildcard").and_then(|v| v.as_bool()) == Some(false) {
                attrs.push("wildcard = false".to_string());
            }
            heads
                .plain
                .push(format!("sink({anchor}::{port}, {})", attrs.join(", ")));
        }
    }
    if let Some(items) = model.get("propagation").and_then(|v| v.as_array()) {
        for item in items {
            let input = port_text(item.get("input"), "input", index)?;
            let output = port_text(item.get("output"), "output", index)?;
            heads.plain.push(format!(
                "propagation({anchor}::{output} <- {anchor}::{input})"
            ));
        }
    }
    if let Some(items) = model.get("access_paths").and_then(|v| v.as_array()) {
        for item in items {
            let text = item
                .as_str()
                .ok_or_else(|| JsonModelError::FieldNotString {
                    index,
                    field_name: "access_paths".to_string(),
                })?;
            heads.plain.push(format!("access_paths({})", quote(text)));
        }
    }
    if let Some(bridge) = model.get("bridge") {
        heads.bridge = Some(migrate_bridge(
            index, generator, bridge, anchor, fresh, report,
        )?);
    }
    Ok(heads)
}

/// The `model` of a `find: "dispatch"` generator.
///
/// Its own translator rather than a branch of [`migrate_model`] because the shapes do not
/// overlap: a dispatch model carries exactly one of `propagation`, `resolve` and
/// `closure_shaped`, and an *empty* `propagation` list means "this call moves nothing" rather
/// than "no heads" — which is `resolve = "skip"` here, since a head is an atom and there is no
/// empty one to write.
fn migrate_dispatch_model(
    index: usize,
    generator: &Value,
    anchor: &str,
) -> Result<Vec<String>, JsonModelError> {
    let Some(model) = generator.get("model") else {
        return Err(JsonModelError::MissingField {
            index,
            field_name: "model".to_string(),
        });
    };
    if let Some(value) = model.get("closure_shaped") {
        return match value.as_bool() {
            Some(true) => Ok(vec![format!("{anchor}::dispatch(closure_shaped = true)")]),
            // `false` derives nothing, exactly as in the JSON loader.
            Some(false) => Ok(Vec::new()),
            None => Err(JsonModelError::FieldNotString {
                index,
                field_name: "closure_shaped".to_string(),
            }),
        };
    }
    let propagation = model.get("propagation").map(|v| {
        v.as_array().ok_or_else(|| JsonModelError::FieldNotArray {
            index,
            field_name: "propagation".to_string(),
        })
    });
    let propagation = propagation.transpose()?;
    // `resolve` beside a `propagation` list inlines and summarizes too, which the DSL says with
    // the flow inside the `resolve = "inline"` head.
    let resolve = match model.get("resolve").map(|v| v.as_str()) {
        None => "",
        Some(Some("inline")) => "resolve = \"inline\", ",
        Some(Some(other)) => {
            return Err(JsonModelError::UnexpectedField {
                index,
                field_name: "resolve".to_string(),
                message: format!("unknown value '{other}'; the defined value is 'inline'"),
            });
        }
        Some(None) => {
            return Err(JsonModelError::FieldNotString {
                index,
                field_name: "resolve".to_string(),
            });
        }
    };
    let items = match propagation {
        Some(items) if !items.is_empty() => items,
        _ if !resolve.is_empty() => {
            return Ok(vec![format!("{anchor}::dispatch(resolve = \"inline\")")]);
        }
        Some(_) => return Ok(vec![format!("{anchor}::dispatch(resolve = \"skip\")")]),
        None => {
            return Err(JsonModelError::MissingField {
                index,
                field_name: "propagation' / 'resolve' / 'closure_shaped".to_string(),
            });
        }
    };
    let mut heads = Vec::new();
    for item in items {
        let input = port_text(item.get("input"), "input", index)?;
        let output = port_text(item.get("output"), "output", index)?;
        heads.push(format!("{anchor}::dispatch({resolve}{output} <- {input})"));
    }
    Ok(heads)
}

/// A bridge diagnostic setting. `"ignore"` and `"warn"` change only what is logged; `"error"`
/// fails the run when a side matches nothing or a pairing is ambiguous, and a DSL rule cannot
/// fail a run.
fn bridge_severity(
    index: usize,
    owner: &Value,
    key: &str,
    report: &mut MigrationReport,
) -> Result<(), JsonModelError> {
    match owner.get(key).and_then(|v| v.as_str()) {
        None => Ok(()),
        Some("error") => Err(untranslatable(
            index,
            &format!("'{key}': \"error\""),
            "it fails the run when the bridge matches badly, and a DSL rule cannot; use \
             \"warn\", and read the dead-rule warning",
        )),
        Some(_) => {
            report.warn(
                index,
                format!(
                    "'{key}' only changes what the JSON loader logs about the pairing, and is \
                     not translated"
                ),
            );
            Ok(())
        }
    }
}

fn migrate_bridge(
    index: usize,
    generator: &Value,
    bridge: &Value,
    anchor: &str,
    fresh: &mut Fresh,
    report: &mut MigrationReport,
) -> Result<BridgeHeads, JsonModelError> {
    let to = bridge
        .get("to")
        .ok_or_else(|| JsonModelError::MissingField {
            index,
            field_name: "to".to_string(),
        })?;
    bridge_severity(index, generator, "on-unmatched", report)?;
    bridge_severity(index, to, "on-unmatched", report)?;
    bridge_severity(index, bridge, "on-ambiguous", report)?;

    // Side B's own match block, compiled with `G` as its subject. A disjunction in it stays in
    // one rule, as side A's does; see `migrate_generator`.
    let scope = scope_attrs(to.get("in"));
    let ctx = Ctx {
        index,
        subject: OTHER,
        callsites: false,
        relation: "fun",
    };
    let expr = where_expr(index, to.get("where"), "to.where")?;
    let disjuncts = compile(&expr, false, &ctx, fresh)?;
    let side_b =
        (!disjuncts.is_empty()).then(|| with_subject(one_of(&disjuncts), "fun", OTHER, &scope));

    // Without a map the loader pairs the arguments the two sides share, which needs the fact
    // base. With an empty one it still bridges the pair, with only the implicit globals port,
    // and a DSL bridge is made of its port heads.
    let pairs = match bridge.get("arguments").and_then(|v| v.as_array()) {
        Some(items) if !items.is_empty() => items,
        Some(_) => {
            return Err(untranslatable(
                index,
                "'bridge'",
                "an empty 'arguments' map bridges only the globals, and a DSL bridge needs at \
                 least one port",
            ));
        }
        None => {
            return Err(untranslatable(
                index,
                "'bridge'",
                "with no 'arguments' map the JSON loader pairs the arguments the two sides \
                 share, which needs the fact base and cannot be written as a rule; write the map",
            ));
        }
    };

    let mut heads = Vec::new();
    for pair in pairs {
        let from = port_text(pair.get("from"), "from", index)?;
        let to_port = port_text(pair.get("to"), "to", index)?;
        // An absent direction is both ways, as in the JSON loader.
        let arrow = match pair.get("direction").and_then(|v| v.as_str()) {
            Some("in") => "->",
            Some("out") => "<-",
            Some("both") | None => "<->",
            Some(other) => {
                return Err(JsonModelError::UnexpectedField {
                    index,
                    field_name: "direction".to_string(),
                    message: format!("'{other}' is not one of 'in', 'out', 'both'"),
                });
            }
        };
        heads.push(format!(
            "bridge({anchor}::{from} {arrow} {OTHER}::{to_port})"
        ));
    }
    Ok(BridgeHeads { heads, side_b })
}

fn kind_of(item: &Value) -> &str {
    item.get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or(super::parse::default_label())
}

/// `Argument(0).deref` → `arg(0).deref`, `Return` → `return`, `Argument(*)` → `arg(_)`.
fn port_text(value: Option<&Value>, field: &str, index: usize) -> Result<String, JsonModelError> {
    let text = match value {
        None => {
            return Err(JsonModelError::MissingField {
                index,
                field_name: field.to_string(),
            });
        }
        Some(v) => v.as_str().ok_or_else(|| JsonModelError::FieldNotString {
            index,
            field_name: field.to_string(),
        })?,
    };
    let parsed = super::super::json::parse_port(text, index)?;
    let base = match parsed.tag {
        crate::models::FormalIndexTypeTag::Return => "return".to_string(),
        crate::models::FormalIndexTypeTag::AnyArgument => "arg(_)".to_string(),
        crate::models::FormalIndexTypeTag::Index => {
            format!("arg({})", parsed.index.expect("Index carries one"))
        }
        crate::models::FormalIndexTypeTag::Local => {
            return Err(untranslatable(
                index,
                &format!("port {text:?}"),
                "'Variable(...)' selects a named local, which the DSL does not spell",
            ));
        }
        // `parse_port` never produces it: the globals port has no spelling in either format.
        crate::models::FormalIndexTypeTag::Global => {
            return Err(untranslatable(
                index,
                &format!("port {text:?}"),
                "the globals port is implicit",
            ));
        }
    };
    let mut out = base;
    for seg in &parsed.ap {
        out.push_str(&segment_text(seg));
    }
    Ok(out)
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

/// `!a` for one atom, `!(a && b)` for several. The empty conjunction is true and has no negation
/// to write; [`compile_prim`] folds it to false before it gets here.
///
/// The parenthesized form is not sugar: negating each atom on its own would test them
/// independently and lose the variable they share, so `not name{pattern}` — a `fun(F, name = N)`
/// and a `regex_match(N, …)` — would stop meaning "no name of F matches".
fn negate(atoms: &[String]) -> String {
    debug_assert!(
        !atoms.is_empty(),
        "the negation of true is folded by the caller"
    );
    match atoms.len() {
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
