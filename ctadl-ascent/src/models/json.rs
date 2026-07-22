/*! JSON model_generator handling

Handles the translation of `model_generator` format into our [`ModelBuilders`].

The code is architected so that models can be streamed in `jsonl` format.
To convert a `json` model file into `jsonl`, you can do:

```text
jq -c '.model_generators[] // empty' models.json > models.jsonl
```
*/
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
pub struct ModelGeneratorIngest<'p, 'b> {
    builder: &'b mut ModelBuilders,
    find_method: Vec<FindMethod>,
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
}

static ARGUMENT_REGEX: OnceLock<Regex> = OnceLock::new();
static RETURN_REGEX: OnceLock<Regex> = OnceLock::new();

#[inline]
fn argument_regex() -> &'static Regex {
    ARGUMENT_REGEX.get_or_init(|| Regex::new(r#"Argument\((\d+|[*])\)(.*)?"#).unwrap())
}

#[inline]
fn return_regex() -> &'static Regex {
    RETURN_REGEX.get_or_init(|| Regex::new(r#"Return(.*)?"#).unwrap())
}

#[derive(Copy, Clone, Debug)]
pub enum FindMethod {
    Methods,
    Callsites,
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

impl<'p, 'b> ModelGeneratorIngest<'p, 'b> {
    pub fn new(program_info: &'p ProgramInfo, builder: &'b mut ModelBuilders) -> Self {
        let vmt = &program_info.vmt;
        let mut program_method_names: HashMap<&'p str, Vec<&'p str>> = HashMap::new();
        let mut program_method_parents: HashMap<&'p str, Vec<&'p str>> = HashMap::new();
        let mut program_method_signatures: HashMap<&'p str, Vec<&'p str>> = HashMap::new();

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
        } else if let VirtualMethodTable::Native { methods } = vmt {
            // Native frontends (pcode, clang) carry, per function, a simple
            // (un-decorated) name and a best-effort type signature alongside the
            // fully-qualified IR name. Key matching off the SIMPLE name so a model
            // pattern like `^system$` resolves even when the IR name is decorated
            // (e.g. Ghidra's `<EXTERNAL>::system@00101008`). The fully-qualified
            // name is also kept matchable for models that spell it out verbatim.
            for (simple, sig, fq) in methods {
                let simple = simple.as_ref();
                let fq = fq.as_ref();
                program_method_names.entry(simple).or_default().push(fq);
                program_method_signatures.entry(sig).or_default().push(fq);
                program_method_names.entry(fq).or_default().push(fq);
                program_method_signatures.entry(fq).or_default().push(fq);
            }
        } else if let VirtualMethodTable::Python { methods } = vmt {
            // The Python frontend qualifies every IR id (`module.Class.method`) and
            // carries, per function, its simple (bare) name, optional class, and the
            // qualified id repurposed as the "signature". Key the SIMPLE name so bare
            // models (`name:"get"`) keep matching, the qualified id so
            // `signature_pattern` can scope by module/class, and the class so
            // `parent`/`parents` scope by class exactly. The fully-qualified id is
            // also kept matchable for models that spell it out verbatim.
            for (cls, simple, sig, fq) in methods {
                let simple = simple.as_ref();
                let sig = sig.as_ref();
                let fq = fq.as_ref();
                program_method_names.entry(simple).or_default().push(fq);
                program_method_names.entry(fq).or_default().push(fq);
                program_method_signatures.entry(sig).or_default().push(fq);
                program_method_signatures.entry(fq).or_default().push(fq);
                if let Some(cls) = cls {
                    program_method_parents
                        .entry(cls.as_ref())
                        .or_default()
                        .push(fq);
                }
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
                methods.iter().map(|(_, _, fq)| fq.as_ref()).collect()
            }
            VirtualMethodTable::Python { methods } => {
                methods.iter().map(|(_, _, _, fq)| fq.as_ref()).collect()
            }
            VirtualMethodTable::Unknown | VirtualMethodTable::CplusPlus => UniverseSet::empty(),
        };

        // constructs index for the program
        Self {
            builder,
            find_method: Vec::new(),
            methods: Vec::new(),
            in_functions: Vec::new(),
            current_set: CurrentSet::Methods,
            vmt,
            program_method_names,
            program_method_parents,
            program_method_signatures,
            program_functions,
            universe,
            scratch: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Add a JSON parsing error to the collection
    fn add_json_error(&mut self, error: crate::error::JsonModelError) {
        self.errors.push(error);
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
    /// comparison on a class, or a `name` match on an integer) is a hard authoring error
    /// (`UnexpectedConstraint`) and evaluates to `false`.
    fn eval_predicate(&mut self, n: usize, c: &serde_json::Value, subj: Subject<'_>) -> bool {
        match c["constraint"].as_str() {
            Some("any_of") => match c.get("inners").and_then(|v| v.as_array()) {
                Some(inners) => inners
                    .iter()
                    .any(|inner| self.eval_predicate(n, inner, subj)),
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotArray {
                        index: n,
                        field_name: "inners".to_string(),
                    });
                    false
                }
            },
            Some("all_of") => match c.get("inners").and_then(|v| v.as_array()) {
                Some(inners) => inners
                    .iter()
                    .all(|inner| self.eval_predicate(n, inner, subj)),
                None => {
                    self.add_json_error(crate::error::JsonModelError::FieldNotArray {
                        index: n,
                        field_name: "inners".to_string(),
                    });
                    false
                }
            },
            Some("not") => match c.get("inner") {
                Some(inner) => !self.eval_predicate(n, inner, subj),
                None => {
                    self.add_json_error(crate::error::JsonModelError::MissingField {
                        index: n,
                        field_name: "inner".to_string(),
                    });
                    false
                }
            },
            Some(op @ ("<" | "<=" | ">" | ">=" | "!=" | "==")) => {
                let Subject::Int(lhs) = subj else {
                    self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                        index: n,
                        constraint_type: op.to_string(),
                    });
                    return false;
                };
                let rhs = match c.get("value").and_then(|v| v.as_i64()) {
                    Some(v) => v,
                    None => {
                        self.add_json_error(crate::error::JsonModelError::InvalidInteger {
                            index: n,
                            source: "".parse::<i64>().unwrap_err(),
                        });
                        return false;
                    }
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
                    self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                        index: n,
                        constraint_type: "name".to_string(),
                    });
                    return false;
                };
                let pattern = match c.get("pattern").and_then(|v| v.as_str()) {
                    Some(p) => p,
                    None => {
                        self.add_json_error(crate::error::JsonModelError::FieldNotString {
                            index: n,
                            field_name: "pattern".to_string(),
                        });
                        return false;
                    }
                };
                match Regex::new(pattern) {
                    Ok(rx) => rx.is_match(cls),
                    Err(source) => {
                        self.add_json_error(crate::error::JsonModelError::InvalidRegex {
                            index: n,
                            pattern: pattern.to_string(),
                            source,
                        });
                        false
                    }
                }
            }
            Some("signature_match") => {
                let Subject::Class(cls) = subj else {
                    self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                        index: n,
                        constraint_type: "signature_match".to_string(),
                    });
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
            Some(other) => {
                self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                    index: n,
                    constraint_type: other.to_string(),
                });
                false
            }
            None => {
                self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                    index: n,
                    constraint_type: "<missing>".to_string(),
                });
                false
            }
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
        ap: &[&str],
        label: &str,
        direction: TaintDirection,
        wildcard: bool,
        saturating: bool,
    ) {
        let callees = matched_functions(&self.methods[n], self.vmt);
        if !matches!(self.find_method.get(n), Some(FindMethod::Callsites)) {
            for func in callees {
                self.builder.endpoint.append(
                    &func, idx, ap, label, direction, wildcard, saturating, None, false,
                );
            }
            return;
        }
        // Callsite-scoped: resolve the caller filter. `All` means "any caller".
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
                    ap,
                    label,
                    direction,
                    wildcard,
                    saturating,
                    caller.as_deref(),
                    true,
                );
            }
        }
    }

    /// Encodes models. It is assumed that each json element of the iterator represents an element of `model_generators`.
    pub fn encode_models(
        &mut self,
        batch: impl IntoIterator<Item = serde_json::Value>,
    ) -> Result<(), Error> {
        for (i, value) in batch.into_iter().enumerate() {
            self.visit_model_generator(i, &value);
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
        self.methods.insert(n, UniverseSet::all());
        self.in_functions.insert(n, UniverseSet::all());
        self.current_set = CurrentSet::Methods;
        self.super_model_generator(n, value);
        self.methods[n] = UniverseSet::empty();
    }

    fn visit_find(&mut self, n: usize, value: &serde_json::Value) {
        self.super_find(n, value);
        match value.as_str() {
            Some("methods") => self.find_method.insert(n, FindMethod::Methods),
            Some("callsites") => self.find_method.insert(n, FindMethod::Callsites),
            Some(other) => {
                self.add_json_error(crate::error::JsonModelError::UnexpectedConstraint {
                    index: n,
                    constraint_type: other.to_string(),
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
        if matches!(
            self.find_method.get(n),
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
        }
    }

    /// Intersects existing `self.methods[n]` with the matches for the constraint
    fn visit_signature_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_signature_constraint(n, value);
        if matches!(
            self.find_method.get(n),
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
        if let Some(FindMethod::Callsites) = self.find_method.get(n) {
            let prev = self.current_set;
            self.current_set = CurrentSet::InFunction;
            self.super_in_function_constraint(n, value);
            self.current_set = prev;
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
            self.find_method.get(n),
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
        // Collect the wanted field names from `name` / `names` / `unqualified-id`.
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
        if let Some(uid) = value.get("unqualified-id").and_then(|v| v.as_str()) {
            wanted.push(uid);
        }
        if wanted.is_empty() {
            return;
        }
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
        // Snapshot (fid, arity) so the predicate evaluator can borrow `self` mutably.
        let funcs: Vec<(&'p str, i64)> = self
            .program_functions
            .iter()
            .map(|(fid, func)| (*fid, func.num_parameters() as i64))
            .collect();
        let mut matched: Vec<&'p str> = Vec::new();
        for (fid, arity) in funcs {
            if self.eval_predicate(n, inner, Subject::Int(arity)) {
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
        let entries: Vec<(&'p str, &'p str)> = match self.vmt {
            VirtualMethodTable::Java { methods, .. } => methods
                .iter()
                .map(|(cls, _, _, fid)| (cls.as_ref(), fid.as_ref()))
                .collect(),
            // Python carries an optional class per method; feed the classed methods
            // (skipping module-level functions) so `parent{inner}` scopes by class.
            VirtualMethodTable::Python { methods } => methods
                .iter()
                .filter_map(|(cls, _, _, fid)| cls.as_ref().map(|c| (c.as_ref(), fid.as_ref())))
                .collect(),
            _ => {
                log::warn!(
                    "'parent' constraint has no class table on this frontend; matching nothing"
                );
                self.target_set_mut(n).intersect_with(UniverseSet::empty());
                return;
            }
        };
        let mut matched: Vec<&'p str> = Vec::new();
        for (cls, fid) in entries {
            if self.eval_predicate(n, inner, Subject::Class(cls)) {
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
                .any(|sc| self.eval_predicate(n, inner, Subject::Class(sc)))
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
        if let Some(FindMethod::Callsites) = self.find_method.get(n) {
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
        if let Some(FindMethod::Methods) = self.find_method.get(n) {
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
                Ok((in_tag, in_index, in_ap)) => match parse_port(output_str, n) {
                    Ok((out_tag, out_index, out_ap)) => {
                        for func in matched_functions(&self.methods[n], self.vmt) {
                            self.builder.summary.append(
                                &func,
                                (out_tag, out_index, &out_ap),
                                (in_tag, in_index, &in_ap),
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
            Ok((tag, index, ap)) => {
                self.emit_endpoints(
                    n,
                    (tag, index),
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
            Ok((tag, index, ap)) => {
                self.emit_endpoints(
                    n,
                    (tag, index),
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

/// Entry point for parsing propagation inputs and inputs, which are called ports
fn parse_port(
    text: &str,
    index: usize,
) -> Result<(FormalIndexTypeTag, Option<i16>, Vec<&str>), crate::error::JsonModelError> {
    if let Some(m) = return_regex().captures(text) {
        let tag = FormalIndexTypeTag::Return;
        let index = None;
        Ok((tag, index, parse_access_path(m.get(1).map(|m| m.as_str()))))
    } else {
        parse_argument(text).map_err(|mut err| {
            // Update the index in the error
            match &mut err {
                crate::error::JsonModelError::InvalidArgumentFormat {
                    index: err_index, ..
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
            VirtualMethodTable::Python { methods } => {
                methods.iter().map(|t| t.3.to_string()).collect()
            }
            VirtualMethodTable::Unknown | VirtualMethodTable::CplusPlus => {
                // For PCODE (which uses Unknown or CplusPlus), we don't have a list of all methods in the VMT
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
