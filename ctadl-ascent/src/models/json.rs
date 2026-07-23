/*! JSON model_generator handling

Handles the translation of `model_generator` format into our [`ModelBuilders`].

The code is architected so that models can be streamed in `jsonl` format.
To convert a `json` model file into `jsonl`, you can do:

```text
jq -c '.model_generators[] // empty' models.json > models.jsonl
```
*/
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use hashbrown::hash_map::HashMap;
use regex::Regex;

use super::universe_set::*;
use super::*;
use crate::error::Error;
use ctadl_ir::ProgramInfo;
use ctadl_ir::mir::FunctionData;
use ctadl_ir::mir::StatementKind;
use ctadl_ir::mir::call::VirtualMethodTable;

/// Ingests model_generators and matches them against a program, producing a set of summaries
/// usable for indexing.
///
/// This object indexes the metadata in a useful way so that model_generators can be efficiently
/// matched. It also implements a visitor for model_generators.
///
/// **Stage 1** of source/sink matching: match MIR elements (function names, signatures,
/// arity, regexes) → the name-based columnar
/// [`EndpointBatch`](crate::models::EndpointBatch) intermediate (see [`Self::emit_endpoints`]).
/// [`query_engine::build_query_endpoints`](crate::query_engine::build_query_endpoints) is
/// Stage 2, which resolves and expands that intermediate into `QueryEndpoint`s.
pub struct ModelGeneratorIngest<'p, 'b> {
    builder: &'b mut ModelBuilders,
    /// Keyed by generator index rather than positional: `find` is optional in the JSON, so a
    /// generator that omits it leaves no entry, and a `Vec` would then misalign every later
    /// generator (it used to panic outright).
    find_method: HashMap<usize, FindMethod>,
    methods: Vec<UniverseSet<&'p str>>,
    /// For `find: callsites`, the set of caller functions matched by `in_function`
    /// (parallel to `methods`, which holds the matched callee functions).
    in_functions: Vec<UniverseSet<&'p str>>,
    /// Which set the currently-executing constraint narrows (see [`CurrentSet`]).
    current_set: CurrentSet,

    vmt: &'p VirtualMethodTable,
    // maps simple names to fully qualified names
    program_method_names: HashMap<&'p str, Vec<&'p str>>,
    // maps parent to fully qualified name
    program_method_parents: HashMap<&'p str, Vec<&'p str>>,
    // maps signatures to fully qualified name
    program_method_signatures: HashMap<&'p str, Vec<&'p str>>,
    /// Maps a method's fully-qualified id to its fq-name, backing the exact-match
    /// `qualified-id` constraint. The key is whatever spelling uniquely names the
    /// method on this frontend: the `JavaMethod` id on jvm/dex, the
    /// namespace-qualified (but address-free) name on native. Unlike
    /// [`Self::program_method_names`] this is never keyed on a bare name, so it can
    /// disambiguate two same-named methods in different namespaces.
    program_method_qualified_ids: HashMap<&'p str, Vec<&'p str>>,
    /// fq-name (== `FunctionData.name`) → the function's IR data. Backs the
    /// `has_code` / `number_parameters` / `uses_field` constraints, which need
    /// per-function body/parameter/field information.
    program_functions: HashMap<&'p str, &'p FunctionData>,
    /// The full set of function fq-names, always [`UniverseSet::Explicit`]. Mirrors
    /// what [`matched_functions`]`(&All)` enumerates for this frontend, so a
    /// top-level `not X` can be materialized to `universe \ X`.
    universe: UniverseSet<&'p str>,
    /// Isolated sub-evaluation stack. While non-empty, [`Self::target_set_mut`]
    /// resolves to the top entry instead of `methods`/`in_functions`, so a
    /// combinator (`any_of` / `not`) can evaluate an inner constraint against a
    /// fresh working set and then combine the result.
    scratch: Vec<UniverseSet<&'p str>>,
    // collected JSON parsing errors
    pub errors: Vec<crate::error::JsonModelError>,
    /// What Stage 1 did per (generator index, direction) — see [`EndpointStats`]. Key
    /// presence means the generator *declared* a port of that direction; a zero
    /// `endpoints_matched` means it declared one and matched nothing. Drives the
    /// `CTADL0004` SARIF notification, so a `where` constraint that selects no function is
    /// reported instead of vanishing.
    pub endpoint_stats: BTreeMap<(usize, TaintDirection), EndpointStats>,
}

/// What Stage 1 did with one generator's port declarations in one direction.
///
/// `ports_declared` and `endpoints_matched` are the two ends of the same fan-out: a model
/// file declares *ports* (one `{"port": "Argument(0)"}` entry each), and each port matches
/// zero or more functions, every one of which becomes an endpoint row. Reporting the first
/// count as though it were the second is what made `CTADL0100` read as nonsense; the SARIF
/// writer now names the unit of every number it prints.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EndpointStats {
    /// Ports the generator declared in this direction: one per `sources`/`sinks` entry whose
    /// `port` parsed. A property of the model file alone, so [`Self::merge`] does *not* sum
    /// it — the same file is re-matched once per import.
    pub ports_declared: usize,
    /// Endpoint rows emitted for those ports, after fanning out over the matched functions.
    /// Summed by [`Self::merge`]: a generator dead against one import but live against
    /// another is live.
    pub endpoints_matched: usize,
    /// How many functions the generator's `where` constraints selected. Reported by
    /// `CTADL0004` to separate "nothing matched" from "functions matched but the port did
    /// not resolve in any of them".
    pub functions_matched: usize,
    /// Why nothing matched. Meaningful only when `endpoints_matched` is zero; a generator
    /// declaring several ports of one direction can fail for several reasons at once.
    pub unmatched: BTreeSet<UnmatchedReason>,
}

impl EndpointStats {
    /// Folds in the stats for the same (generator, direction) from another load of the same
    /// model file — one per import. Each field combines differently; see the field docs.
    pub fn merge(&mut self, other: &Self) {
        self.ports_declared = self.ports_declared.max(other.ports_declared);
        self.endpoints_matched += other.endpoints_matched;
        self.functions_matched = self.functions_matched.max(other.functions_matched);
        self.unmatched.extend(other.unmatched.iter().cloned());
    }
}

/// Why a declared port produced no endpoint row. The generator matching zero functions is
/// only one of the ways; `CTADL0004` used to report all of them as that one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnmatchedReason {
    /// The generator's `where` constraints selected no function in the program.
    NoFunctionMatched,
    /// Functions matched, but none of them declares a local by this name, so the
    /// `Variable(name)` port resolved in none of them.
    LocalNotFound(String),
    /// `find: callsites` with an `in_function` constraint that no caller satisfied.
    NoCallerMatched,
    /// The port is not usable in this generator's context. Always accompanied by a hard JSON
    /// error, so in practice the load fails before any SARIF is written.
    PortRejected,
}

static ARGUMENT_REGEX: OnceLock<Regex> = OnceLock::new();
static RETURN_REGEX: OnceLock<Regex> = OnceLock::new();
static VARIABLE_REGEX: OnceLock<Regex> = OnceLock::new();

#[inline]
fn argument_regex() -> &'static Regex {
    ARGUMENT_REGEX.get_or_init(|| Regex::new(r#"Argument\((\d+|[*])\)(.*)?"#).unwrap())
}

#[inline]
fn return_regex() -> &'static Regex {
    RETURN_REGEX.get_or_init(|| Regex::new(r#"Return(.*)?"#).unwrap())
}

#[inline]
fn variable_regex() -> &'static Regex {
    // `Variable(name)` selects a source/sink port by the local's source name, with an
    // optional trailing access path (`Variable(buf).headers`).
    VARIABLE_REGEX.get_or_init(|| Regex::new(r#"Variable\(([^)]+)\)(.*)?"#).unwrap())
}

#[derive(Copy, Clone, Debug)]
pub enum FindMethod {
    Methods,
    Callsites,
}

/// What one [`ModelGeneratorIngest::emit_endpoint_rows`] call did, folded into
/// [`EndpointStats`] by its caller.
struct PortEmit {
    /// Endpoint rows appended for this port.
    rows: usize,
    /// Functions the generator's `where` constraints selected (the callees, for
    /// `find: callsites`).
    functions: usize,
    /// Set exactly when `rows` is zero.
    reason: Option<UnmatchedReason>,
}

/// Put `value` in the slot for generator index `n`, growing `v` with empty sets as needed.
fn set_slot<'p>(v: &mut Vec<UniverseSet<&'p str>>, n: usize, value: UniverseSet<&'p str>) {
    if v.len() <= n {
        v.resize_with(n + 1, UniverseSet::empty);
    }
    v[n] = value;
}

/// Which universe set a set-narrowing constraint (e.g. `signature_match`) currently
/// intersects. Defaults to [`CurrentSet::Methods`] (the callee/method set). While
/// evaluating an `in_function` constraint's inner constraint it is flipped to
/// [`CurrentSet::InFunction`] so the same matching code narrows the caller set instead.
#[derive(Copy, Clone, Debug, PartialEq)]
enum CurrentSet {
    Methods,
    InFunction,
}

/// The value a predicate-style constraint (`number_parameters`, `parent`,
/// `extends`) applies its `inner` to. Unlike the set-narrowing constraints, the
/// inner here is evaluated against a scalar (an arity or a class name) rather
/// than a set of functions — see [`ModelGeneratorIngest::eval_predicate`].
#[derive(Copy, Clone, Debug)]
enum Subject<'a> {
    Int(i64),
    Class(&'a str),
}

/// Which [`Subject`] variant a predicate tree will be evaluated against.
///
/// The kind is fixed before evaluation begins and is what makes
/// [`ModelGeneratorIngest::validate_predicate`] possible: every authoring error a predicate
/// can have is a function of its shape plus this, never of the subject's value.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SubjectKind {
    Int,
    Class,
}

impl<'p, 'b> ModelGeneratorIngest<'p, 'b> {
    pub fn new(program_info: &'p ProgramInfo, builder: &'b mut ModelBuilders) -> Self {
        let vmt = &program_info.vmt;
        let mut program_method_names: HashMap<&'p str, Vec<&'p str>> = HashMap::new();
        let mut program_method_parents: HashMap<&'p str, Vec<&'p str>> = HashMap::new();
        let mut program_method_signatures: HashMap<&'p str, Vec<&'p str>> = HashMap::new();
        let mut program_method_qualified_ids: HashMap<&'p str, Vec<&'p str>> = HashMap::new();

        if let VirtualMethodTable::Java { methods, .. } = vmt {
            methods
                .iter()
                .map(|(_cls, name, _sig, fid)| (name.as_ref(), fid.as_ref()))
                .for_each(|(key, val)| program_method_names.entry(key).or_default().push(val));

            methods
                .iter()
                .map(|(cls, _name, _sig, fid)| (cls.as_ref(), fid.as_ref()))
                .for_each(|(key, val)| program_method_parents.entry(key).or_default().push(val));

            methods
                .iter()
                .map(|(_cls, _name, sig, fid)| (sig.as_ref(), fid.as_ref()))
                .for_each(|(key, val)| program_method_signatures.entry(key).or_default().push(val));

            // The `JavaMethod` id, e.g. `Lcom/example/Foo;->bar(I)V`. Descriptor-bearing and
            // stable, but until now only ever a *value* above — never a key — which is what
            // made exact fully-qualified matching impossible on jvm/dex.
            methods
                .iter()
                .map(|(_cls, _name, _sig, fid)| (fid.as_ref(), fid.as_ref()))
                .for_each(|(key, val)| {
                    program_method_qualified_ids
                        .entry(key)
                        .or_default()
                        .push(val)
                });
        } else if let VirtualMethodTable::Native { methods } = vmt {
            // Native frontends (pcode, clang) carry, per function, a simple
            // (un-decorated) name and a best-effort type signature alongside the
            // fully-qualified IR name. Key matching off the SIMPLE name so a model
            // pattern like `^system$` resolves even when the IR name is decorated
            // (e.g. Ghidra's `<EXTERNAL>::system@00101008`). The fully-qualified
            // name is also kept matchable for models that spell it out verbatim.
            for (simple, sig, fq, qualified) in methods {
                let simple = simple.as_ref();
                let fq = fq.as_ref();
                program_method_names.entry(simple).or_default().push(fq);
                program_method_signatures.entry(sig).or_default().push(fq);
                program_method_names.entry(fq).or_default().push(fq);
                program_method_signatures.entry(fq).or_default().push(fq);
                // The namespace-qualified name, e.g. `Foo::bar` or `<EXTERNAL>::system`.
                // Double-key on the fq id as well, mirroring the names/signatures maps
                // above, so a model that spells the decorated id out verbatim still
                // resolves through `qualified-id`.
                program_method_qualified_ids
                    .entry(qualified.as_ref())
                    .or_default()
                    .push(fq);
                program_method_qualified_ids.entry(fq).or_default().push(fq);
            }
        } else {
            // Fallback (Unknown / CplusPlus): use the IR function names directly.
            for func in &program_info.program.functions.functions {
                let name = func.name.as_str();
                program_method_signatures
                    .entry(name)
                    .or_default()
                    .push(name);
                program_method_names.entry(name).or_default().push(name);
                program_method_qualified_ids
                    .entry(name)
                    .or_default()
                    .push(name);
            }
        }
        // Index every IR function by its fq-name for the body/parameter/field
        // constraints (`has_code`, `number_parameters`, `uses_field`).
        let mut program_functions: HashMap<&'p str, &'p FunctionData> = HashMap::new();
        for func in &program_info.program.functions.functions {
            program_functions.entry(func.name.as_str()).or_insert(func);
        }

        let universe: UniverseSet<&'p str> = match vmt {
            VirtualMethodTable::Java { methods, .. } => {
                methods.iter().map(|(_, _, _, fid)| fid.as_ref()).collect()
            }
            VirtualMethodTable::Native { methods } => {
                methods.iter().map(|(_, _, fq, _)| fq.as_ref()).collect()
            }
            // Lua free functions (source/sink/main) are not class methods, so name-based
            // matching happens through the fallback indexing above; the universe stays empty
            // like the Unknown case (proven behavior; parent/extends constraints are Java-only).
            VirtualMethodTable::Lua { .. } | VirtualMethodTable::Unknown => UniverseSet::empty(),
        };

        // constructs index for the program
        Self {
            builder,
            find_method: HashMap::new(),
            methods: Vec::new(),
            in_functions: Vec::new(),
            current_set: CurrentSet::Methods,
            vmt,
            program_method_names,
            program_method_parents,
            program_method_signatures,
            program_method_qualified_ids,
            program_functions,
            universe,
            scratch: Vec::new(),
            errors: Vec::new(),
            endpoint_stats: BTreeMap::new(),
        }
    }

    /// Add a JSON parsing error to the collection
    fn add_json_error(&mut self, error: crate::error::JsonModelError) {
        self.errors.push(error);
    }

    /// Validates the key set of a leaf constraint object: every key must be one the visitor
    /// actually honors, and at least one honored key must be present.
    ///
    /// A constraint the loader cannot act on is *not* harmless. The working set starts as
    /// [`UniverseSet::all`] (see [`Self::visit_model_generator`]), so a constraint that
    /// narrows nothing leaves the generator matching **every function in the program** — a
    /// model meant to mark one method as a source silently becomes a global source, and
    /// `CTADL0004` only reports generators that matched *nothing*. Erroring only on "no
    /// honored key" would still let `{"constraint": "signature_match", "name": "x",
    /// "extends": "Y"}` drop `extends` on the floor, so the unknown-key half is the
    /// important one: it mirrors the schema's `additionalProperties: false` and makes key
    /// removals self-enforcing.
    ///
    /// `constraint` — the discriminator itself — is the only structural key; nothing in the
    /// `super_*` traversal injects or wraps anything, so the object seen here is verbatim
    /// from the model file.
    ///
    /// Returns false if anything was reported, so callers can skip the set math.
    fn check_constraint_keys(
        &mut self,
        n: usize,
        value: &serde_json::Value,
        honored: &[&str],
    ) -> bool {
        // `visit_where_constraint` already rejected anything without a string `constraint`
        // discriminator, so a non-object cannot reach a leaf visitor.
        let Some(obj) = value.as_object() else {
            return false;
        };
        let kind = obj
            .get("constraint")
            .and_then(|v| v.as_str())
            .unwrap_or("<missing>");
        let expected = honored
            .iter()
            .map(|h| format!("'{h}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut ok = true;
        for key in obj.keys() {
            if key == "constraint" || honored.contains(&key.as_str()) {
                continue;
            }
            self.add_json_error(crate::error::JsonModelError::UnexpectedField {
                index: n,
                field_name: key.clone(),
                message: format!(
                    "not a recognized field of the '{kind}' constraint; expected one of {expected}"
                ),
            });
            ok = false;
        }
        if !honored.iter().any(|h| obj.contains_key(*h)) {
            self.add_json_error(crate::error::JsonModelError::MissingField {
                index: n,
                field_name: honored.join("' / '"),
            });
            ok = false;
        }
        ok
    }

    /// The universe set that set-narrowing constraints currently intersect.
    ///
    /// While a combinator (`any_of` / `not`) is evaluating an inner constraint it
    /// pushes a scratch set (see [`Self::scratch`]); this resolves to that scratch
    /// top so leaf narrowers transparently operate on the sub-evaluation. Once the
    /// scratch pops it falls back to the active `methods`/`in_functions` set,
    /// redirected to the caller set while evaluating `in_function`'s inner
    /// constraint (see [`CurrentSet`]).
    #[inline]
    fn target_set_mut(&mut self, n: usize) -> &mut UniverseSet<&'p str> {
        if let Some(top) = self.scratch.last_mut() {
            return top;
        }
        match self.current_set {
            CurrentSet::Methods => &mut self.methods[n],
            CurrentSet::InFunction => &mut self.in_functions[n],
        }
    }

    /// Replace the active target set with the materialized universe if it is
    /// still `All`. Used by `not` so `universe \ inner` is well-defined. Respects
    /// the scratch stack, so a `not` nested inside an `any_of` materializes the
    /// any_of's scratch rather than the top-level method set.
    #[inline]
    fn materialize_target(&mut self, n: usize) {
        let universe = self.universe.clone();
        let target = self.target_set_mut(n);
        if matches!(target, UniverseSet::All) {
            *target = universe;
        }
    }

    /// Evaluates a predicate-style constraint against a scalar [`Subject`].
    ///
    /// This is the boolean counterpart to the set-narrowing visitor: `number_parameters`,
    /// `parent`, and `extends` bind their `inner` to a specific integer (arity) or class name
    /// rather than to the function working set. Combinators (`any_of`/`all_of`/`not`) are handled
    /// at the boolean level. A constraint that does not apply to the subject (e.g. an integer
    /// comparison on a class, or a `name` match on an integer) evaluates to `false`.
    ///
    /// **This function reports no errors.** It runs once per candidate — per function arity,
    /// per class, and for `extends` inside a short-circuiting `any` over supertypes — so
    /// reporting from here emitted one copy of the same authoring error per candidate
    /// (thousands of identical lines on a real program, a count that depended on
    /// short-circuiting for `extends`) and reported *nothing at all* when the candidate list
    /// was empty, e.g. `parent` on a non-Java program. Every such error is a property of the
    /// constraint's shape and the subject's kind, both fixed before the loop starts, so
    /// [`Self::validate_predicate`] is the single reporter and runs exactly once.
    fn eval_predicate(&self, c: &serde_json::Value, subj: Subject<'_>) -> bool {
        match c["constraint"].as_str() {
            Some("any_of") => match c.get("inners").and_then(|v| v.as_array()) {
                Some(inners) => inners.iter().any(|inner| self.eval_predicate(inner, subj)),
                None => false,
            },
            Some("all_of") => match c.get("inners").and_then(|v| v.as_array()) {
                Some(inners) => inners.iter().all(|inner| self.eval_predicate(inner, subj)),
                None => false,
            },
            Some("not") => match c.get("inner") {
                Some(inner) => !self.eval_predicate(inner, subj),
                None => false,
            },
            Some(op @ ("<" | "<=" | ">" | ">=" | "!=" | "==")) => {
                let Subject::Int(lhs) = subj else {
                    return false;
                };
                let Some(rhs) = c.get("value").and_then(|v| v.as_i64()) else {
                    return false;
                };
                match op {
                    "<" => lhs < rhs,
                    "<=" => lhs <= rhs,
                    ">" => lhs > rhs,
                    ">=" => lhs >= rhs,
                    "!=" => lhs != rhs,
                    "==" => lhs == rhs,
                    _ => unreachable!(),
                }
            }
            Some("name") => {
                let Subject::Class(cls) = subj else {
                    return false;
                };
                let Some(pattern) = c.get("pattern").and_then(|v| v.as_str()) else {
                    return false;
                };
                Regex::new(pattern).is_ok_and(|rx| rx.is_match(cls))
            }
            Some("signature_match") => {
                let Subject::Class(cls) = subj else {
                    return false;
                };
                // Equality of the class name against `name`/`names`.
                let name_matches = c
                    .get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|name| name == cls);
                let names_match = c
                    .get("names")
                    .and_then(|v| v.as_array())
                    .is_some_and(|names| names.iter().filter_map(|v| v.as_str()).any(|s| s == cls));
                name_matches || names_match
            }
            Some(_) | None => false,
        }
    }

    /// Structural validation of a predicate tree — the `inner` of `number_parameters`,
    /// `parent`, and `extends` — run exactly once, before evaluation.
    ///
    /// This is the sole reporter for that tree; see [`Self::eval_predicate`] for why the
    /// evaluator itself stays silent. The arms mirror the evaluator's one-for-one, so a
    /// predicate that passes here can still evaluate to `false`, but never for a reason the
    /// model author would want to hear about.
    fn validate_predicate(&mut self, n: usize, c: &serde_json::Value, kind: SubjectKind) {
        match c["constraint"].as_str() {
            Some("any_of" | "all_of") => match c.get("inners").and_then(|v| v.as_array()) {
                Some(inners) => {
                    for inner in inners {
                        self.validate_predicate(n, inner, kind);
                    }
                }
                None => self.add_json_error(crate::error::JsonModelError::FieldNotArray {
                    index: n,
                    field_name: "inners".to_string(),
                }),
            },
            Some("not") => match c.get("inner") {
                Some(inner) => self.validate_predicate(n, inner, kind),
                None => self.add_json_error(crate::error::JsonModelError::MissingField {
                    index: n,
                    field_name: "inner".to_string(),
                }),
            },
            Some(op @ ("<" | "<=" | ">" | ">=" | "!=" | "==")) => {
                if kind != SubjectKind::Int {
                    self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                        index: n,
                        constraint_type: op.to_string(),
                    });
                    return;
                }
                if c.get("value").and_then(|v| v.as_i64()).is_none() {
                    self.add_json_error(crate::error::JsonModelError::InvalidInteger {
                        index: n,
                        source: "".parse::<i64>().unwrap_err(),
                    });
                    return;
                }
                self.check_constraint_keys(n, c, &["value"]);
            }
            Some("name") => {
                if kind != SubjectKind::Class {
                    self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                        index: n,
                        constraint_type: "name".to_string(),
                    });
                    return;
                }
                let Some(pattern) = c.get("pattern").and_then(|v| v.as_str()) else {
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "pattern".to_string(),
                    });
                    return;
                };
                if let Err(source) = Regex::new(pattern) {
                    self.add_json_error(crate::error::JsonModelError::InvalidRegex {
                        index: n,
                        pattern: pattern.to_string(),
                        source,
                    });
                    return;
                }
                self.check_constraint_keys(n, c, &["pattern"]);
            }
            Some("signature_match") => {
                if kind != SubjectKind::Class {
                    self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                        index: n,
                        constraint_type: "signature_match".to_string(),
                    });
                    return;
                }
                // Only `name`/`names` are honored here: the subject is already a class name,
                // so `parent`/`parents` are meaningless and `qualified-id` would be a pure
                // synonym for `name`. Both would otherwise be silently dropped conjuncts.
                self.check_constraint_keys(n, c, &["name", "names"]);
            }
            Some(other) => {
                self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                    index: n,
                    constraint_type: other.to_string(),
                })
            }
            None => self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                index: n,
                constraint_type: "<missing>".to_string(),
            }),
        }
    }

    /// Emits source/sink endpoints for the matched elements of generator `n`.
    ///
    /// For `find: methods` this appends one function-anchored endpoint per matched function.
    /// For `find: callsites` it appends callsite-scoped endpoints for the cross product of
    /// matched callees (`self.methods[n]`) and matched callers (`self.in_functions[n]`); when
    /// no `in_function` constraint was given (caller set is `All`) it emits an unfiltered
    /// endpoint per callee that fans out to every callsite downstream.
    fn emit_endpoints(
        &mut self,
        n: usize,
        idx: (FormalIndexTypeTag, Option<i16>),
        var_name: Option<&str>,
        ap: &[&str],
        label: &str,
        direction: TaintDirection,
        wildcard: bool,
        saturating: bool,
    ) {
        let emit =
            self.emit_endpoint_rows(n, idx, var_name, ap, label, direction, wildcard, saturating);
        // Key presence records that generator `n` *declared* a port in this direction even
        // when it matched nothing; a zero `endpoints_matched` is exactly the `CTADL0004`
        // condition. Ports are counted per call because one generator may declare several of
        // the same direction; the rows are accumulated because matching in any of those ports
        // makes the generator live.
        let stats = self.endpoint_stats.entry((n, direction)).or_default();
        stats.ports_declared += 1;
        stats.endpoints_matched += emit.rows;
        // Max, not sum: every port of a generator re-derives the same matched-function set.
        stats.functions_matched = stats.functions_matched.max(emit.functions);
        if let Some(reason) = emit.reason {
            stats.unmatched.insert(reason);
        }
    }

    /// The body of [`Self::emit_endpoints`], reporting what it did so the caller can record
    /// it. Every early `return` here is a "matched nothing" path.
    fn emit_endpoint_rows(
        &mut self,
        n: usize,
        idx: (FormalIndexTypeTag, Option<i16>),
        var_name: Option<&str>,
        ap: &[&str],
        label: &str,
        direction: TaintDirection,
        wildcard: bool,
        saturating: bool,
    ) -> PortEmit {
        let tag = idx.0;
        let mut rows = 0usize;
        let is_callsites = matches!(self.find_method.get(&n), Some(FindMethod::Callsites));
        // A callsite-scoped endpoint models the callee at a call site; the callee's locals are
        // not a call-site concept and Stage 2 gives `Local` vars no call-site fan-out. Reject
        // rather than silently degrade to function-anchored.
        if tag == FormalIndexTypeTag::Local && is_callsites {
            self.add_json_error(crate::error::JsonModelError::UnexpectedField {
                index: n,
                field_name: "port".to_string(),
                message: "'Variable(...)' is not supported with find: callsites".to_string(),
            });
            return PortEmit {
                rows,
                functions: 0,
                reason: Some(UnmatchedReason::PortRejected),
            };
        }
        let callees = matched_functions(&self.methods[n], self.vmt);
        let functions = callees.len();
        if !is_callsites {
            for func in callees {
                // For a `Variable(name)` port, resolve the name to a base `LocalIdx` in *this*
                // matched function. Copy the `&FunctionData` out first so `self.program_functions`
                // is not borrowed across the `self.builder` mutable borrow below.
                let local_index = if tag == FormalIndexTypeTag::Local {
                    let name = var_name.expect("Local port without var_name");
                    let fd = self.program_functions.get(func.as_str()).copied();
                    match fd.and_then(|fd| {
                        fd.locals
                            .iter_enumerated()
                            .find(|(_, d)| d.name.as_str() == name)
                            .map(|(i, _)| u32::from(i))
                    }) {
                        Some(li) => Some(li),
                        None => {
                            // Skip only this function; other matched functions may have the local.
                            log::warn!("named local {name:?} not found in {func}");
                            continue;
                        }
                    }
                } else {
                    None
                };
                self.builder.endpoint.append(
                    &func,
                    idx,
                    local_index,
                    ap,
                    label,
                    direction,
                    wildcard,
                    saturating,
                    None,
                    false,
                );
                rows += 1;
            }
            // A non-`Local` port appends a row for every matched function, so `rows == 0`
            // with `functions > 0` can only be the `continue` above: the local named by a
            // `Variable(...)` port exists in none of them.
            let reason = match (rows, var_name) {
                (0, Some(name)) if functions > 0 => {
                    Some(UnmatchedReason::LocalNotFound(name.to_string()))
                }
                (0, _) => Some(UnmatchedReason::NoFunctionMatched),
                _ => None,
            };
            return PortEmit {
                rows,
                functions,
                reason,
            };
        }
        // Callsite-scoped: resolve the caller filter. `All` means "any caller". (`Local` ports
        // were rejected above, so `local_index` is always `None` here.)
        let callers: Vec<Option<String>> = match &self.in_functions[n] {
            UniverseSet::All => vec![None],
            _ => matched_functions(&self.in_functions[n], self.vmt)
                .into_iter()
                .map(Some)
                .collect(),
        };
        for func in &callees {
            for caller in &callers {
                self.builder.endpoint.append(
                    func,
                    idx,
                    None,
                    ap,
                    label,
                    direction,
                    wildcard,
                    saturating,
                    caller.as_deref(),
                    true,
                );
                rows += 1;
            }
        }
        // `callers` is `[None]` when no `in_function` was given, so an empty product with
        // matched callees means the `in_function` constraint itself selected no caller.
        let reason = match rows {
            0 if functions > 0 => Some(UnmatchedReason::NoCallerMatched),
            0 => Some(UnmatchedReason::NoFunctionMatched),
            _ => None,
        };
        PortEmit {
            rows,
            functions,
            reason,
        }
    }

    /// Encodes models. It is assumed that each json element of the iterator represents an element of `model_generators`.
    pub fn encode_models(
        &mut self,
        batch: impl IntoIterator<Item = serde_json::Value>,
    ) -> Result<(), Error> {
        self.encode_models_from(0, batch)
    }

    /// [`Self::encode_models`] for a batch that starts at generator index `start` in the
    /// model file. Loading is batched (see
    /// [`try_load_models_from_values`](crate::models::try_load_models_from_values)) but the
    /// generator index must keep counting across batches: it is what JSON error messages and
    /// the `CTADL0004` SARIF notification name the offending generator by, and what
    /// [`Self::endpoint_stats`] is keyed on, so restarting it per batch both misnames
    /// generators and collides their match counts.
    pub fn encode_models_from(
        &mut self,
        start: usize,
        batch: impl IntoIterator<Item = serde_json::Value>,
    ) -> Result<(), Error> {
        for (i, value) in batch.into_iter().enumerate() {
            self.visit_model_generator(start + i, &value);
        }
        // Check for any collected errors and return them
        let errors = std::mem::take(&mut self.errors);
        if errors.is_empty() {
            Ok(())
        } else {
            let mut json_errors = crate::error::JsonModelErrors::default();
            json_errors.extend(errors);
            Err(Error::JsonModel(json_errors))
        }
    }
}

impl<'p, 'b> ModelGeneratorVisitor for ModelGeneratorIngest<'p, 'b> {
    /// Entry point. Clear the model_generator set then visit it.
    fn visit_model_generator(&mut self, n: usize, value: &serde_json::Value) {
        // Assign at `n`, don't `Vec::insert` at `n`: insert *shifts* the tail, which is only
        // ever a no-op because generators arrive in index order. Grow-then-assign keeps the
        // slot for generator `n` at index `n` no matter how the caller batches.
        set_slot(&mut self.methods, n, UniverseSet::all());
        set_slot(&mut self.in_functions, n, UniverseSet::all());
        self.current_set = CurrentSet::Methods;
        self.super_model_generator(n, value);
        self.methods[n] = UniverseSet::empty();
    }

    fn visit_find(&mut self, n: usize, value: &serde_json::Value) {
        self.super_find(n, value);
        match value.as_str() {
            Some("methods") => {
                self.find_method.insert(n, FindMethod::Methods);
            }
            Some("callsites") => {
                self.find_method.insert(n, FindMethod::Callsites);
            }
            Some(other) => {
                self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                    index: n,
                    constraint_type: other.to_string(),
                })
            }
            // `super_model_generator` indexes `find` out of the object, so an absent field
            // arrives as `Null` — report that as missing rather than as a type error.
            None if value.is_null() => {
                self.add_json_error(crate::error::JsonModelError::MissingField {
                    index: n,
                    field_name: "find".to_string(),
                })
            }
            None => self.add_json_error(crate::error::JsonModelError::FieldNotString {
                index: n,
                field_name: "find".to_string(),
            }),
        }
    }

    /// Intersects existing `self.methods[n]` with the matches for the constraint
    fn visit_signature_match_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_signature_match_constraint(n, value);
        // Validate before the `find_method` gate: whether a model file is well-formed must
        // not depend on the frontend or on whether `find` parsed.
        //
        // `parent`/`parents` stay honored unconditionally even though
        // `program_method_parents` is populated only for the Java VMT. On a native VMT they
        // intersect with the empty set and match nothing, which is fail-*closed* and matches
        // the documented Java-only behavior of the standalone `parent` constraint. Rejecting
        // them per-frontend would make one model file valid or invalid depending on which
        // artifact it is loaded against.
        if !self.check_constraint_keys(
            n,
            value,
            &[
                "name",
                "names",
                "parent",
                "parents",
                "qualified-id",
                "qualified-ids",
            ],
        ) {
            self.target_set_mut(n).intersect_with(UniverseSet::empty());
            return;
        }
        if matches!(
            self.find_method.get(&n),
            Some(FindMethod::Methods | FindMethod::Callsites)
        ) {
            let has_names = value.get("names").or(value.get("name")).is_some();
            if has_names {
                // This horrific expression computes the set of names mentioned in the constraint
                // that match the program metadata
                let names_result: Result<UniverseSet<&'p str>, ()> = (|| {
                    let names_iter = value
                        .get("names")
                        .map(|v| {
                            v.as_array().ok_or_else(|| {
                                self.add_json_error(crate::error::JsonModelError::FieldNotArray {
                                    index: n,
                                    field_name: "names".to_string(),
                                });
                            })
                        })
                        .transpose()?
                        .into_iter()
                        .flatten();

                    let name_iter = value.get("name").into_iter();

                    let names: UniverseSet<&'p str> = names_iter
                        .chain(name_iter)
                        .filter_map(|n| {
                            n.as_str().and_then(|name| {
                                self.program_method_names
                                    .get(name)
                                    .map(|names| names.iter().copied())
                            })
                        })
                        .flatten()
                        .collect();

                    Ok(names)
                })();

                if let Ok(names) = names_result {
                    self.target_set_mut(n).intersect_with(names);
                }
            }
            let has_parents = value.get("parents").or(value.get("parent")).is_some();
            if has_parents {
                let parents_result: Result<UniverseSet<&'p str>, ()> = (|| {
                    let parents_iter = value
                        .get("parents")
                        .map(|v| {
                            v.as_array().ok_or_else(|| {
                                self.add_json_error(crate::error::JsonModelError::FieldNotArray {
                                    index: n,
                                    field_name: "parents".to_string(),
                                });
                            })
                        })
                        .transpose()?
                        .into_iter()
                        .flatten();

                    let parent_iter = value.get("parent").into_iter();

                    let parents: UniverseSet<&'p str> = parents_iter
                        .chain(parent_iter)
                        .filter_map(|p| {
                            p.as_str().and_then(|parent| {
                                self.program_method_parents
                                    .get(parent)
                                    .map(|parents| parents.iter().copied())
                            })
                        })
                        .flatten()
                        .collect();

                    Ok(parents)
                })();

                if let Ok(parents) = parents_result {
                    self.target_set_mut(n).intersect_with(parents);
                }
            }
            // Exact, whole-string match on a method's fully-qualified id. Unlike `name` this
            // is neither a regex nor keyed on the bare name, so it is the one lever that can
            // pick out a single method on a frontend with no class hierarchy: `parent` is
            // populated only for the Java VMT, and `signature_pattern` regexes (unanchored,
            // and only incidentally over the fq name). An id naming no function in the
            // program intersects with the empty set and matches nothing — the same
            // fail-closed behavior an unmatched `name` has today.
            let has_qualified_ids = value
                .get("qualified-ids")
                .or(value.get("qualified-id"))
                .is_some();
            if has_qualified_ids {
                let ids_result: Result<UniverseSet<&'p str>, ()> = (|| {
                    let ids_iter = value
                        .get("qualified-ids")
                        .map(|v| {
                            v.as_array().ok_or_else(|| {
                                self.add_json_error(crate::error::JsonModelError::FieldNotArray {
                                    index: n,
                                    field_name: "qualified-ids".to_string(),
                                });
                            })
                        })
                        .transpose()?
                        .into_iter()
                        .flatten();

                    let id_iter = value.get("qualified-id").into_iter();

                    let ids: UniverseSet<&'p str> = ids_iter
                        .chain(id_iter)
                        .filter_map(|v| {
                            v.as_str().and_then(|id| {
                                self.program_method_qualified_ids
                                    .get(id)
                                    .map(|fids| fids.iter().copied())
                            })
                        })
                        .flatten()
                        .collect();

                    Ok(ids)
                })();

                if let Ok(ids) = ids_result {
                    self.target_set_mut(n).intersect_with(ids);
                }
            }
        }
    }

    /// Intersects existing `self.methods[n]` with the matches for the constraint
    fn visit_signature_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_signature_constraint(n, value);
        // A missing `pattern` used to fall out of the `let` chain below as a no-op, so
        // `{"constraint": "signature", "name": ".*sink.*"}` — `name` where `pattern` was
        // meant — matched every function in the program instead of failing.
        if !self.check_constraint_keys(n, value, &["pattern"]) {
            self.target_set_mut(n).intersect_with(UniverseSet::empty());
            return;
        }
        if matches!(
            self.find_method.get(&n),
            Some(FindMethod::Methods | FindMethod::Callsites)
        ) && let Some(pattern) = value.get("pattern")
        {
            let pattern_str = match pattern.as_str() {
                Some(s) => s,
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "pattern".to_string(),
                    });
                    return;
                }
            };

            let rx = match Regex::new(pattern_str) {
                Ok(regex) => regex,
                Err(source) => {
                    self.add_json_error(crate::error::JsonModelError::InvalidRegex {
                        index: n,
                        pattern: pattern_str.to_string(),
                        source,
                    });
                    return;
                }
            };

            let matches: UniverseSet<&'p str> = self
                .program_method_signatures
                .iter()
                .filter_map(|(sig, fids)| if rx.is_match(sig) { Some(fids) } else { None })
                .flatten()
                .copied()
                .collect();
            self.target_set_mut(n).intersect_with(matches);
        }
    }

    /// Matches the containing (caller) function of a callsite by evaluating the wrapped
    /// `inner` constraint against the caller set. Only meaningful for `find: callsites`.
    fn visit_in_function_constraint(&mut self, n: usize, value: &serde_json::Value) {
        if !self.check_constraint_keys(n, value, &["inner"]) {
            self.target_set_mut(n).intersect_with(UniverseSet::empty());
            return;
        }
        match self.find_method.get(&n) {
            Some(FindMethod::Callsites) => {
                let prev = self.current_set;
                self.current_set = CurrentSet::InFunction;
                self.super_in_function_constraint(n, value);
                self.current_set = prev;
            }
            Some(FindMethod::Methods) => {
                // There is no caller set to narrow under `find: methods`, so the constraint
                // used to vanish silently, leaving the generator matching on its remaining
                // constraints alone. That is a mis-authored model, not a harmless one.
                self.add_json_error(crate::error::JsonModelError::UnexpectedField {
                    index: n,
                    field_name: "in_function".to_string(),
                    message: "'in_function' is only supported with find: callsites".to_string(),
                });
                self.target_set_mut(n).intersect_with(UniverseSet::empty());
            }
            // `find` itself was missing or unrecognized; `visit_find` already reported it,
            // so don't pile a second, more confusing error on top.
            None => {
                self.target_set_mut(n).intersect_with(UniverseSet::empty());
            }
        }
    }

    /// Dispatches a where-constraint, hard-erroring on removed/unknown constraint names.
    ///
    /// `ModelGeneratorIngest` is the sole implementor of the trait, so this is the single
    /// authority on which constraints are recognized. The removed `parameter`/`any_parameter`
    /// constraints (whose backing per-parameter data does not exist in the IR) and any
    /// unrecognized discriminator are collected as [`crate::error::JsonModelError::UnexpectedConstraint`]
    /// — a hard error that fails model loading rather than a silent skip. This intentionally
    /// removes forward-compatibility with schema-newer model files; model files are versioned
    /// with the analyzer, and silent skips previously masked real bugs (see docs).
    fn visit_where_constraint(&mut self, n: usize, value: &serde_json::Value) {
        match value["constraint"].as_str() {
            Some(
                "signature_match" | "signature" | "signature_pattern" | "parent" | "extends"
                | "in_function" | "has_code" | "number_parameters" | "name" | "any_of" | "all_of"
                | "not" | "uses_field",
            ) => self.super_where_constraint(n, value),
            Some(removed @ ("parameter" | "any_parameter")) => {
                self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                    index: n,
                    constraint_type: removed.to_string(),
                });
            }
            Some(other) => {
                self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                    index: n,
                    constraint_type: other.to_string(),
                });
            }
            None => {
                self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                    index: n,
                    constraint_type: "<missing>".to_string(),
                });
            }
        }
    }

    /// `all_of` is the intersection (AND) of its inner constraints. Visiting each inner in
    /// sequence intersects it into the shared working set, which is exactly AND.
    fn visit_all_of_constraint(&mut self, n: usize, value: &serde_json::Value) {
        let Some(inners) = value.get("inners").and_then(|v| v.as_array()) else {
            self.add_json_error(crate::error::JsonModelError::FieldNotArray {
                index: n,
                field_name: "inners".to_string(),
            });
            return;
        };
        for inner in inners {
            self.visit_where_constraint(n, inner);
        }
    }

    /// `any_of` is the union (OR) of its inner constraints. Each inner is evaluated against a
    /// fresh `All` scratch set (so inners don't narrow each other), the results are unioned, and
    /// the union is finally intersected into the active working set.
    fn visit_any_of_constraint(&mut self, n: usize, value: &serde_json::Value) {
        let Some(inners) = value.get("inners").and_then(|v| v.as_array()) else {
            self.add_json_error(crate::error::JsonModelError::FieldNotArray {
                index: n,
                field_name: "inners".to_string(),
            });
            return;
        };
        let mut acc = UniverseSet::empty();
        for inner in inners {
            self.scratch.push(UniverseSet::all());
            self.visit_where_constraint(n, inner);
            let matched = self.scratch.pop().expect("scratch stack balanced");
            acc.union_with(matched);
        }
        self.target_set_mut(n).intersect_with(acc);
    }

    /// `not` is set complement. The inner is evaluated against a fresh `All` scratch to obtain
    /// the set it matches; the active working set is then materialized (`All` → the function
    /// universe) and the matched set is subtracted. Because both the scratch push and
    /// [`Self::materialize_target`] respect the scratch stack, a `not` nested inside an `any_of`
    /// complements against the any_of's working set, while a top-level `not` complements against
    /// the whole function universe.
    fn visit_not_constraint(&mut self, n: usize, value: &serde_json::Value) {
        let Some(inner) = value.get("inner") else {
            self.add_json_error(crate::error::JsonModelError::MissingField {
                index: n,
                field_name: "inner".to_string(),
            });
            return;
        };
        self.scratch.push(UniverseSet::all());
        self.visit_where_constraint(n, inner);
        let matched = self.scratch.pop().expect("scratch stack balanced");
        self.materialize_target(n);
        self.target_set_mut(n).difference_with(matched);
    }

    /// Matches functions/variables by a regex on their (simple) name. All frontends.
    fn visit_name_constraint(&mut self, n: usize, value: &serde_json::Value) {
        if !matches!(
            self.find_method.get(&n),
            Some(FindMethod::Methods | FindMethod::Callsites)
        ) {
            return;
        }
        let pattern = match value.get("pattern") {
            Some(p) => match p.as_str() {
                Some(s) => s,
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "pattern".to_string(),
                    });
                    return;
                }
            },
            None => {
                self.add_json_error(crate::error::JsonModelError::MissingField {
                    index: n,
                    field_name: "pattern".to_string(),
                });
                return;
            }
        };
        let rx = match Regex::new(pattern) {
            Ok(rx) => rx,
            Err(source) => {
                self.add_json_error(crate::error::JsonModelError::InvalidRegex {
                    index: n,
                    pattern: pattern.to_string(),
                    source,
                });
                return;
            }
        };
        let matches: UniverseSet<&'p str> = self
            .program_method_names
            .iter()
            .filter(|(name, _)| rx.is_match(name))
            .flat_map(|(_, fids)| fids.iter().copied())
            .collect();
        self.target_set_mut(n).intersect_with(matches);
    }

    /// Matches functions by whether they have a body (`value: true`) or are external/stub
    /// (`value: false`). Universal across frontends.
    fn visit_has_code_constraint(&mut self, n: usize, value: &serde_json::Value) {
        let want = match value.get("value") {
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => {
                    // Reuse FieldNotString for the non-bool case (consistent with the existing
                    // loose usage for `saturating`/`wildcard`).
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "value".to_string(),
                    });
                    return;
                }
            },
            None => {
                self.add_json_error(crate::error::JsonModelError::MissingField {
                    index: n,
                    field_name: "value".to_string(),
                });
                return;
            }
        };
        let matches: UniverseSet<&'p str> = self
            .program_functions
            .iter()
            .filter(|(_, func)| func.blocks.is_empty() != want)
            .map(|(fid, _)| *fid)
            .collect();
        self.target_set_mut(n).intersect_with(matches);
    }

    /// Matches functions that read or write any of the named fields (via `Load`/`Store`).
    /// On-demand scan of every function body; frontends without symbolic loads/stores yield
    /// no match.
    fn visit_uses_field_constraint(&mut self, n: usize, value: &serde_json::Value) {
        if !self.check_constraint_keys(n, value, &["name", "names"]) {
            self.target_set_mut(n).intersect_with(UniverseSet::empty());
            return;
        }
        // Collect the wanted field names from `name` / `names`.
        let mut wanted: Vec<&str> = Vec::new();
        if let Some(names) = value.get("names") {
            match names.as_array() {
                Some(arr) => wanted.extend(arr.iter().filter_map(|v| v.as_str())),
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotArray {
                        index: n,
                        field_name: "names".to_string(),
                    });
                    return;
                }
            }
        }
        if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
            wanted.push(name);
        }
        // `check_constraint_keys` guaranteed `name` or `names` is present, so an empty
        // `wanted` here means every entry was a non-string or the array was empty; either
        // way, narrow to nothing rather than leaving the set untouched (== matching all).
        let matches: UniverseSet<&'p str> = self
            .program_functions
            .iter()
            .filter(|(_, func)| {
                func.blocks.iter().any(|block| {
                    block.statements.iter().any(|stmt| match &stmt.kind {
                        StatementKind::Load { field, .. } | StatementKind::Store { field, .. } => {
                            wanted.iter().any(|w| &*field.field == *w)
                        }
                        _ => false,
                    })
                })
            })
            .map(|(fid, _)| *fid)
            .collect();
        self.target_set_mut(n).intersect_with(matches);
    }

    /// Matches functions whose parameter count satisfies the integer `inner` constraint.
    fn visit_number_parameters_constraint(&mut self, n: usize, value: &serde_json::Value) {
        let Some(inner) = value.get("inner") else {
            self.add_json_error(crate::error::JsonModelError::MissingField {
                index: n,
                field_name: "inner".to_string(),
            });
            return;
        };
        self.validate_predicate(n, inner, SubjectKind::Int);
        // Snapshot (fid, arity) so `target_set_mut` below can take `self` mutably.
        let funcs: Vec<(&'p str, i64)> = self
            .program_functions
            .iter()
            .map(|(fid, func)| (*fid, func.num_parameters() as i64))
            .collect();
        let mut matched: Vec<&'p str> = Vec::new();
        for (fid, arity) in funcs {
            if self.eval_predicate(inner, Subject::Int(arity)) {
                matched.push(fid);
            }
        }
        let matched: UniverseSet<&'p str> = matched.into_iter().collect();
        self.target_set_mut(n).intersect_with(matched);
    }

    /// Matches methods whose owning class satisfies `inner`. Java-only (the class hierarchy
    /// exists only for the Java VMT); on other frontends it warns and matches nothing.
    fn visit_parent_constraint(&mut self, n: usize, value: &serde_json::Value) {
        let Some(inner) = value.get("inner") else {
            self.add_json_error(crate::error::JsonModelError::MissingField {
                index: n,
                field_name: "inner".to_string(),
            });
            return;
        };
        // Validate before the frontend check below: a mis-authored predicate is a property
        // of the model file, and reporting it only on Java programs would make the same file
        // load cleanly or fail depending on the artifact.
        self.validate_predicate(n, inner, SubjectKind::Class);
        let entries: Vec<(&'p str, &'p str)> = match self.vmt {
            VirtualMethodTable::Java { methods, .. } => methods
                .iter()
                .map(|(cls, _, _, fid)| (cls.as_ref(), fid.as_ref()))
                .collect(),
            _ => {
                log::warn!("'parent' constraint is Java-only; matching nothing on this frontend");
                self.target_set_mut(n).intersect_with(UniverseSet::empty());
                return;
            }
        };
        let mut matched: Vec<&'p str> = Vec::new();
        for (cls, fid) in entries {
            if self.eval_predicate(inner, Subject::Class(cls)) {
                matched.push(fid);
            }
        }
        let matched: UniverseSet<&'p str> = matched.into_iter().collect();
        self.target_set_mut(n).intersect_with(matched);
    }

    /// Matches methods a superclass/interface of whose owning class satisfies `inner`. Java-only;
    /// on other frontends it warns and matches nothing.
    fn visit_extends_constraint(&mut self, n: usize, value: &serde_json::Value) {
        let Some(inner) = value.get("inner") else {
            self.add_json_error(crate::error::JsonModelError::MissingField {
                index: n,
                field_name: "inner".to_string(),
            });
            return;
        };
        self.validate_predicate(n, inner, SubjectKind::Class);
        // Snapshot (fid, [supertypes]) — `hierarchy[cls]` is `[0]` = superclass, rest = interfaces.
        let entries: Vec<(&'p str, Vec<&'p str>)> = match self.vmt {
            VirtualMethodTable::Java { methods, hierarchy } => methods
                .iter()
                .map(|(cls, _, _, fid)| {
                    let supers = hierarchy
                        .get(cls)
                        .map(|scs| scs.iter().map(|sc| sc.as_ref()).collect())
                        .unwrap_or_default();
                    (fid.as_ref(), supers)
                })
                .collect(),
            _ => {
                log::warn!("'extends' constraint is Java-only; matching nothing on this frontend");
                self.target_set_mut(n).intersect_with(UniverseSet::empty());
                return;
            }
        };
        let mut matched: Vec<&'p str> = Vec::new();
        for (fid, supers) in &entries {
            if supers
                .iter()
                .any(|sc| self.eval_predicate(inner, Subject::Class(sc)))
            {
                matched.push(*fid);
            }
        }
        let matched: UniverseSet<&'p str> = matched.into_iter().collect();
        self.target_set_mut(n).intersect_with(matched);
    }

    /// Sends the methods in `self.methods[n]` to the SummaryBuilder
    fn visit_propagation(&mut self, n: usize, value: &serde_json::Value) {
        self.super_propagation(n, value);
        // Propagation (summaries) at a callsite is a function-level fact and is not
        // supported for `find: callsites`.
        if let Some(FindMethod::Callsites) = self.find_method.get(&n) {
            self.add_json_error(crate::error::JsonModelError::UnexpectedField {
                index: n,
                field_name: "propagation".to_string(),
                message: "'propagation' is not supported with find: callsites".to_string(),
            });
            return;
        }
        // `wildcard` is sink-only; reject it on a propagation.
        if value.get("wildcard").is_some() {
            self.add_json_error(crate::error::JsonModelError::UnexpectedField {
                index: n,
                field_name: "wildcard".to_string(),
                message: "'wildcard' is only valid on sink ports".to_string(),
            });
            return;
        }
        if let Some(FindMethod::Methods) = self.find_method.get(&n) {
            let input_str = match value.get("input") {
                Some(v) => match v.as_str() {
                    Some(s) => s,
                    None => {
                        self.add_json_error(crate::error::JsonModelError::FieldNotString {
                            index: n,
                            field_name: "input".to_string(),
                        });
                        return;
                    }
                },
                None => {
                    self.add_json_error(crate::error::JsonModelError::MissingField {
                        index: n,
                        field_name: "input".to_string(),
                    });
                    return;
                }
            };

            let output_str = match value.get("output") {
                Some(v) => match v.as_str() {
                    Some(s) => s,
                    None => {
                        self.add_json_error(crate::error::JsonModelError::FieldNotString {
                            index: n,
                            field_name: "output".to_string(),
                        });
                        return;
                    }
                },
                None => {
                    self.add_json_error(crate::error::JsonModelError::MissingField {
                        index: n,
                        field_name: "output".to_string(),
                    });
                    return;
                }
            };

            match parse_port(input_str, n) {
                Ok(input) => match parse_port(output_str, n) {
                    Ok(output) => {
                        // `Variable(name)` selects a named local; summaries carry no local-index
                        // column and do no per-function name resolution, so reject it here.
                        if input.tag == FormalIndexTypeTag::Local
                            || output.tag == FormalIndexTypeTag::Local
                        {
                            self.add_json_error(crate::error::JsonModelError::UnexpectedField {
                                index: n,
                                field_name: if input.tag == FormalIndexTypeTag::Local {
                                    "input".to_string()
                                } else {
                                    "output".to_string()
                                },
                                message:
                                    "'Variable(...)' ports are only valid on source/sink ports"
                                        .to_string(),
                            });
                            return;
                        }
                        for func in matched_functions(&self.methods[n], self.vmt) {
                            self.builder.summary.append(
                                &func,
                                (output.tag, output.index, &output.ap),
                                (input.tag, input.index, &input.ap),
                            );
                        }
                    }
                    Err(err) => self.add_json_error(err),
                },
                Err(err) => self.add_json_error(err),
            }
        }
    }

    fn visit_source(&mut self, n: usize, value: &serde_json::Value) {
        let label = match value.get("kind") {
            Some(v) => match v.as_str() {
                Some(s) => s,
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "kind".to_string(),
                    });
                    return;
                }
            },
            None => {
                self.add_json_error(crate::error::JsonModelError::MissingField {
                    index: n,
                    field_name: "kind".to_string(),
                });
                return;
            }
        };

        let port_str = match value.get("port") {
            Some(v) => match v.as_str() {
                Some(s) => s,
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "port".to_string(),
                    });
                    return;
                }
            },
            None => {
                self.add_json_error(crate::error::JsonModelError::MissingField {
                    index: n,
                    field_name: "port".to_string(),
                });
                return;
            }
        };

        // `wildcard` is sink-only; reject it here.
        if value.get("wildcard").is_some() {
            self.add_json_error(crate::error::JsonModelError::UnexpectedField {
                index: n,
                field_name: "wildcard".to_string(),
                message: "'wildcard' is only valid on sink ports".to_string(),
            });
            return;
        }

        // `saturating` (source-only, default false): mark a saturating source (any
        // subfield/offset read off the seeded vertex is also tainted). Must be a boolean if
        // present.
        let saturating = match value.get("saturating") {
            None => false,
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "saturating".to_string(),
                    });
                    return;
                }
            },
        };

        match parse_port(port_str, n) {
            Ok(ParsedPort {
                tag,
                index,
                var_name,
                ap,
            }) => {
                self.emit_endpoints(
                    n,
                    (tag, index),
                    var_name,
                    &ap,
                    label,
                    TaintDirection::Forward,
                    false,
                    saturating,
                );
            }
            Err(err) => self.add_json_error(err),
        }
    }

    fn visit_sink(&mut self, n: usize, value: &serde_json::Value) {
        let label = match value.get("kind") {
            Some(v) => match v.as_str() {
                Some(s) => s,
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "kind".to_string(),
                    });
                    return;
                }
            },
            None => {
                self.add_json_error(crate::error::JsonModelError::MissingField {
                    index: n,
                    field_name: "kind".to_string(),
                });
                return;
            }
        };

        let port_str = match value.get("port") {
            Some(v) => match v.as_str() {
                Some(s) => s,
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "port".to_string(),
                    });
                    return;
                }
            },
            None => {
                self.add_json_error(crate::error::JsonModelError::MissingField {
                    index: n,
                    field_name: "port".to_string(),
                });
                return;
            }
        };

        // `saturating` is source-only; reject it here.
        if value.get("saturating").is_some() {
            self.add_json_error(crate::error::JsonModelError::UnexpectedField {
                index: n,
                field_name: "saturating".to_string(),
                message: "'saturating' is only valid on source ports".to_string(),
            });
            return;
        }

        // `wildcard` (sink-only, default true): match any access-path extension of
        // the port. Must be a boolean if present.
        let wildcard = match value.get("wildcard") {
            None => true,
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotString {
                        index: n,
                        field_name: "wildcard".to_string(),
                    });
                    return;
                }
            },
        };

        match parse_port(port_str, n) {
            Ok(ParsedPort {
                tag,
                index,
                var_name,
                ap,
            }) => {
                self.emit_endpoints(
                    n,
                    (tag, index),
                    var_name,
                    &ap,
                    label,
                    TaintDirection::Backward,
                    wildcard,
                    false,
                );
            }
            Err(err) => self.add_json_error(err),
        }
    }
}

/// A parsed source/sink/propagation port. `var_name` is `Some` only for a `Variable(name)`
/// port (`tag == Local`); `index` is `Some` only for a positional `Argument(n)` port
/// (`tag == Index`). Both `var_name` and `ap` borrow from the port `text`.
struct ParsedPort<'a> {
    tag: FormalIndexTypeTag,
    index: Option<i16>,
    var_name: Option<&'a str>,
    ap: Vec<&'a str>,
}

/// Entry point for parsing propagation inputs and inputs, which are called ports
fn parse_port(text: &str, index: usize) -> Result<ParsedPort<'_>, crate::error::JsonModelError> {
    if let Some(m) = variable_regex().captures(text) {
        // `Variable(name)` — name-based local selector. The base `LocalIdx` is resolved
        // per-function later (in `emit_endpoints`), so no index is known here.
        Ok(ParsedPort {
            tag: FormalIndexTypeTag::Local,
            index: None,
            var_name: m.get(1).map(|m| m.as_str()),
            ap: parse_access_path(m.get(2).map(|m| m.as_str())),
        })
    } else if let Some(m) = return_regex().captures(text) {
        Ok(ParsedPort {
            tag: FormalIndexTypeTag::Return,
            index: None,
            var_name: None,
            ap: parse_access_path(m.get(1).map(|m| m.as_str())),
        })
    } else {
        parse_argument(text)
            .map(|(tag, idx, ap)| ParsedPort {
                tag,
                index: idx,
                var_name: None,
                ap,
            })
            .map_err(|mut err| {
                // Update the index in the error
                match &mut err {
                    crate::error::JsonModelError::InvalidArgumentFormat {
                        index: err_index,
                        ..
                    } => *err_index = index,
                    crate::error::JsonModelError::InvalidInteger {
                        index: err_index, ..
                    } => *err_index = index,
                    _ => {}
                }
                err
            })
    }
}

fn parse_access_path(input_text: Option<&str>) -> Vec<&str> {
    match input_text {
        Some(".*") | None => Vec::new(),
        Some(s) => split_dot_segments(s),
    }
}

fn parse_argument(
    input_text: &str,
) -> Result<(FormalIndexTypeTag, Option<i16>, Vec<&str>), crate::error::JsonModelError> {
    let m = argument_regex().captures(input_text).ok_or_else(|| {
        crate::error::JsonModelError::InvalidArgumentFormat {
            index: 0, // We don't have the index here, will be set by caller
            text: input_text.to_string(),
        }
    })?;
    let arg_text = m.get(1).map(|m| m.as_str()).ok_or_else(|| {
        crate::error::JsonModelError::InvalidArgumentFormat {
            index: 0,
            text: input_text.to_string(),
        }
    })?;
    let (tag, index) = match arg_text {
        "*" => (FormalIndexTypeTag::AnyArgument, None),
        _ => (
            FormalIndexTypeTag::Index,
            Some(arg_text.parse::<i16>().map_err(|source| {
                crate::error::JsonModelError::InvalidInteger { index: 0, source }
            })?),
        ),
    };
    let p = parse_access_path(m.get(2).map(|m| m.as_str()));
    Ok((tag, index, p))
}

fn split_dot_segments(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();

    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'.' {
            break;
        }
        i += 1; // past '.'
        let start = i;

        while i < bytes.len() && bytes[i] != b'.' {
            i += 1;
        }
        out.push(&s[start..i]); // does NOT include the leading '.'
        // next iteration will see the next '.' (or end)
    }

    out
}

/// Visitor for JSON model generators
pub trait ModelGeneratorVisitor {
    #[inline]
    fn visit_model_generators(&mut self, value: &serde_json::Value) {
        self.super_model_generators(value);
    }

    #[inline]
    fn super_model_generators(&mut self, value: &serde_json::Value) {
        value["model_generators"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .for_each(|(i, m)| self.visit_model_generator(i, m));
    }

    #[inline]
    fn visit_model_generator(&mut self, n: usize, value: &serde_json::Value) {
        self.super_model_generator(n, value);
    }

    #[inline]
    fn super_model_generator(&mut self, n: usize, model_generator: &serde_json::Value) {
        self.visit_find(n, &model_generator["find"]);
        model_generator.get("where").into_iter().for_each(|cs| {
            cs.as_array()
                .unwrap()
                .iter()
                .for_each(|c| self.visit_where_constraint(n, c))
        });
        self.visit_model(n, &model_generator["model"]);
    }

    #[inline]
    fn visit_find(&mut self, n: usize, value: &serde_json::Value) {
        self.super_find(n, value)
    }

    #[inline]
    fn super_find(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing else to do
    }

    #[inline]
    fn visit_where_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_where_constraint(n, value);
    }

    #[inline]
    fn super_where_constraint(&mut self, n: usize, value: &serde_json::Value) {
        match &value["constraint"].as_str() {
            Some("signature_match") => self.visit_signature_match_constraint(n, value),
            Some("signature" | "signature_pattern") => self.visit_signature_constraint(n, value),
            Some("parent") => self.visit_parent_constraint(n, value),
            Some("extends") => self.visit_extends_constraint(n, value),
            Some("has_code") => self.visit_has_code_constraint(n, value),
            Some("number_parameters") => self.visit_number_parameters_constraint(n, value),
            Some("name") => self.visit_name_constraint(n, value),
            Some("any_of") => self.visit_any_of_constraint(n, value),
            Some("all_of") => self.visit_all_of_constraint(n, value),
            Some("not") => self.visit_not_constraint(n, value),
            Some("uses_field") => self.visit_uses_field_constraint(n, value),
            Some("in_function") => self.visit_in_function_constraint(n, value),
            Some(c) => log::warn!("unhandled model_generator constraint: {c}"),
            None => (),
        }
    }

    #[inline]
    fn visit_uses_field_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_uses_field_constraint(n, value)
    }

    #[inline]
    fn super_uses_field_constraint(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing
    }

    #[inline]
    fn visit_not_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_not_constraint(n, value)
    }

    #[inline]
    fn super_not_constraint(&mut self, n: usize, value: &serde_json::Value) {
        value
            .get("inner")
            .into_iter()
            .for_each(|c| self.visit_where_constraint(n, c));
    }

    #[inline]
    fn visit_all_of_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_all_of_constraint(n, value)
    }

    #[inline]
    fn super_all_of_constraint(&mut self, n: usize, value: &serde_json::Value) {
        value.get("inners").into_iter().for_each(|a| {
            a.as_array()
                .unwrap()
                .iter()
                .for_each(|c| self.visit_where_constraint(n, c))
        });
    }

    #[inline]
    fn visit_any_of_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_any_of_constraint(n, value)
    }

    #[inline]
    fn super_any_of_constraint(&mut self, n: usize, value: &serde_json::Value) {
        value.get("inners").into_iter().for_each(|a| {
            a.as_array()
                .unwrap()
                .iter()
                .for_each(|c| self.visit_where_constraint(n, c))
        });
    }

    #[inline]
    fn visit_number_parameters_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_number_parameters_constraint(n, value)
    }

    #[inline]
    fn super_number_parameters_constraint(&mut self, n: usize, value: &serde_json::Value) {
        value
            .get("inner")
            .into_iter()
            .for_each(|c| self.visit_where_constraint(n, c));
    }

    #[inline]
    fn visit_name_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_name_constraint(n, value)
    }

    #[inline]
    fn super_name_constraint(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing
    }

    #[inline]
    fn visit_has_code_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_has_code_constraint(n, value)
    }

    #[inline]
    fn super_has_code_constraint(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing
    }

    #[inline]
    fn visit_extends_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_extends_constraint(n, value)
    }

    #[inline]
    fn super_extends_constraint(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing
    }

    #[inline]
    fn visit_parent_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_parent_constraint(n, value)
    }

    #[inline]
    fn super_parent_constraint(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing
    }

    #[inline]
    fn visit_signature_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_signature_constraint(n, value)
    }

    #[inline]
    fn super_signature_constraint(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing
    }

    #[inline]
    fn visit_signature_match_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_signature_match_constraint(n, value)
    }

    #[inline]
    fn super_signature_match_constraint(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing
    }

    #[inline]
    fn visit_in_function_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_in_function_constraint(n, value)
    }

    #[inline]
    fn super_in_function_constraint(&mut self, n: usize, value: &serde_json::Value) {
        value
            .get("inner")
            .into_iter()
            .for_each(|c| self.visit_where_constraint(n, c));
    }

    #[inline]
    fn visit_model(&mut self, n: usize, _value: &serde_json::Value) {
        self.super_model(n, _value)
    }

    #[inline]
    fn super_model(&mut self, n: usize, value: &serde_json::Value) {
        if let Some(propagation) = value.get("propagation") {
            propagation
                .as_array()
                .unwrap()
                .iter()
                .for_each(|p| self.visit_propagation(n, p));
        }
        if let Some(sinks) = value.get("sinks") {
            sinks
                .as_array()
                .unwrap()
                .iter()
                .for_each(|s| self.visit_sink(n, s));
        }
        if let Some(sources) = value.get("sources") {
            sources
                .as_array()
                .unwrap()
                .iter()
                .for_each(|s| self.visit_source(n, s));
        }
    }

    #[inline]
    fn visit_propagation(&mut self, n: usize, value: &serde_json::Value) {
        self.super_propagation(n, value);
    }

    #[inline]
    fn super_propagation(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing
    }

    #[inline]
    fn visit_sink(&mut self, n: usize, value: &serde_json::Value) {
        self.super_sink(n, value)
    }

    #[inline]
    fn super_sink(&mut self, _n: usize, _value: &serde_json::Value) {}

    #[inline]
    fn visit_source(&mut self, n: usize, value: &serde_json::Value) {
        self.super_source(n, value)
    }

    #[inline]
    fn super_source(&mut self, _n: usize, _value: &serde_json::Value) {}
}

/// Iterates over the functions denoted by the set. This requires consulting the
/// [`VirtualMethodTable`] if the set is "all."
pub fn matched_functions(set: &UniverseSet<&str>, vmt: &VirtualMethodTable) -> Vec<String> {
    match set {
        UniverseSet::Explicit(set) => set.iter().map(|s| (*s).to_owned()).collect(),
        UniverseSet::All => match vmt {
            VirtualMethodTable::Java { methods, .. } => {
                methods.iter().map(|t| t.3.to_string()).collect()
            }
            VirtualMethodTable::Native { methods } => {
                methods.iter().map(|t| t.2.to_string()).collect()
            }
            VirtualMethodTable::Lua { methods, .. } => {
                methods.iter().map(|t| t.2.to_string()).collect()
            }
            VirtualMethodTable::Unknown => {
                // For PCODE (which uses Unknown), we don't have a list of all methods in the VMT
                // but we should have been able to match them via names/signatures in ModelGeneratorIngest.
                // If it's 'All', we might need to return all known functions in the program.
                log::warn!(
                    "'all' methods requested for non-Java VMT; this may not return all functions"
                );
                Vec::new()
            }
        },
    }
}

#[cfg(test)]
mod parse_port_tests {
    use super::*;

    #[test]
    fn parses_variable_selector() {
        let p = parse_port("Variable(buf)", 0).expect("parse");
        assert_eq!(p.tag, FormalIndexTypeTag::Local);
        assert_eq!(p.var_name, Some("buf"));
        assert_eq!(p.index, None);
        assert!(p.ap.is_empty());
    }

    #[test]
    fn parses_variable_selector_with_access_path() {
        let p = parse_port("Variable(buf).headers", 0).expect("parse");
        assert_eq!(p.tag, FormalIndexTypeTag::Local);
        assert_eq!(p.var_name, Some("buf"));
        assert_eq!(p.ap, vec!["headers"]);
    }

    #[test]
    fn argument_and_return_still_parse() {
        let a = parse_port("Argument(1)", 0).expect("parse");
        assert_eq!(a.tag, FormalIndexTypeTag::Index);
        assert_eq!(a.index, Some(1));
        assert_eq!(a.var_name, None);

        let r = parse_port("Return", 0).expect("parse");
        assert_eq!(r.tag, FormalIndexTypeTag::Return);
        assert_eq!(r.var_name, None);
    }
}
