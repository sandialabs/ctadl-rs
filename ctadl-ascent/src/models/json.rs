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

    vmt: &'p VirtualMethodTable,
    // maps simple names to fully qualified names
    program_method_names: HashMap<&'p str, Vec<&'p str>>,
    // maps parent to fully qualified name
    program_method_parents: HashMap<&'p str, Vec<&'p str>>,
    // maps signatures to fully qualified name
    program_method_signatures: HashMap<&'p str, Vec<&'p str>>,
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
        } else {
            // For non-Java (e.g. PCODE), use the function names as signatures
            for func in &program_info.program.functions.functions {
                let name = func.name.as_str();
                program_method_signatures
                    .entry(name)
                    .or_default()
                    .push(name);
                program_method_names.entry(name).or_default().push(name);
            }
        }
        // constructs index for the program
        Self {
            builder,
            find_method: Vec::new(),
            methods: Vec::new(),
            vmt,
            program_method_names,
            program_method_parents,
            program_method_signatures,
            errors: Vec::new(),
        }
    }

    /// Add a JSON parsing error to the collection
    fn add_json_error(&mut self, error: crate::error::JsonModelError) {
        self.errors.push(error);
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
        self.super_model_generator(n, value);
        self.methods[n] = UniverseSet::empty();
    }

    fn visit_find(&mut self, n: usize, value: &serde_json::Value) {
        self.super_find(n, value);
        match value.as_str() {
            Some("methods") => self.find_method.insert(n, FindMethod::Methods),
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
        if let Some(FindMethod::Methods) = self.find_method.get(n) {
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
                    self.methods[n].intersect_with(names);
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
                    self.methods[n].intersect_with(parents);
                }
            }
        }
    }

    /// Intersects existing `self.methods[n]` with the matches for the constraint
    fn visit_signature_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_signature_constraint(n, value);
        if let Some(FindMethod::Methods) = self.find_method.get(n)
            && let Some(pattern) = value.get("pattern")
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
            self.methods[n].intersect_with(matches);
        }
    }

    /// Sends the methods in `self.methods[n]` to the SummaryBuilder
    fn visit_propagation(&mut self, n: usize, value: &serde_json::Value) {
        self.super_propagation(n, value);
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

        match parse_port(port_str, n) {
            Ok((tag, index, ap)) => {
                for func in matched_functions(&self.methods[n], self.vmt) {
                    self.builder.endpoint.append(
                        &func,
                        (tag, index),
                        &ap,
                        label,
                        TaintDirection::Forward,
                    );
                }
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

        match parse_port(port_str, n) {
            Ok((tag, index, ap)) => {
                for func in matched_functions(&self.methods[n], self.vmt) {
                    self.builder.endpoint.append(
                        &func,
                        (tag, index),
                        &ap,
                        label,
                        TaintDirection::Backward,
                    );
                }
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
            Some("parameter") => self.visit_parameter_constraint(n, value),
            Some("any_parameter") => self.visit_any_parameter_constraint(n, value),
            Some("has_code") => self.visit_has_code_constraint(n, value),
            Some("number_parameters") => self.visit_number_parameters_constraint(n, value),
            Some("any_of") => self.visit_any_of_constraint(n, value),
            Some("all_of") => self.visit_all_of_constraint(n, value),
            Some("not") => self.visit_not_constraint(n, value),
            Some("uses_field") => self.visit_uses_field_constraint(n, value),
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
    fn visit_has_code_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_has_code_constraint(n, value)
    }

    #[inline]
    fn super_has_code_constraint(&mut self, _n: usize, _value: &serde_json::Value) {
        // Nothing
    }

    #[inline]
    fn visit_any_parameter_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_any_parameter_constraint(n, value)
    }

    #[inline]
    fn super_any_parameter_constraint(&mut self, n: usize, value: &serde_json::Value) {
        value
            .get("inner")
            .into_iter()
            .for_each(|c| self.visit_where_constraint(n, c));
    }

    #[inline]
    fn visit_parameter_constraint(&mut self, n: usize, value: &serde_json::Value) {
        self.super_parameter_constraint(n, value)
    }

    #[inline]
    fn super_parameter_constraint(&mut self, _n: usize, _value: &serde_json::Value) {
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
