/*! The shape of a JSON model generator, checked without a program.

[`generator_errors`] reports what the JSON format does not allow in one generator, by walking
its structure alone: unknown and missing keys, `find`, every `where` constraint and predicate
tree, the `in` scope, the `model` object of each `find`, ports, access paths, `modes` and
bridges. The migrator runs it first, so a generator the format rejects fails the migration with
the error the JSON loader raised for it, in the order the loader raised them, rather than being
translated.

Every check is program-independent. A generator scoped by `in` to another import is checked as
well: a model file is either valid or not, whatever it is run against.

Also home to [`parse_port`], the port grammar bridges and the migrator share.
*/

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use super::FormalIndexTypeTag;
use super::spec;
use crate::error::JsonModelError;
use ctadl_ir::mir;
use ctadl_ir::mir::PathSegment;

/// A generator's `find`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FindMethod {
    Methods,
    Callsites,
    /// Selects call sites by their own static signature rather than by the callee they resolve
    /// to. See `docs/deprecated-json-model-generators.md`.
    Dispatch,
}

/// Everything wrong with generator `index`, in the order the JSON loader reported it.
///
/// A bridge is checked first, and a malformed bridge or `in` scope stops there. A clean one is
/// followed by the generator's own shape, then by side B's `where`, which is checked as a
/// `find: methods` generator's.
pub fn generator_errors(index: usize, generator: &Value) -> Vec<JsonModelError> {
    let mut errors = Vec::new();
    let bridge = match generator.pointer("/model/bridge") {
        Some(bridge) => {
            let path = std::path::Path::new("<validate>");
            let spec = spec::parse_bridge(path, index, generator, bridge, &mut errors);
            if !errors.is_empty() {
                return errors;
            }
            spec
        }
        None => None,
    };
    spec::ProgramScope::parse(generator.get("in"), index, &mut errors);
    if !errors.is_empty() {
        return errors;
    }
    let mut walk = Walk {
        index,
        find: None,
        errors,
    };
    walk.generator(generator);
    if let Some(bridge) = bridge {
        walk.find = Some(FindMethod::Methods);
        for c in &bridge.to.where_ {
            walk.constraint(c);
        }
    }
    walk.errors
}

/// Which scalar a predicate tree (the `inner` of `number_parameters`, `parent`, `extends`) is
/// applied to. Every error a predicate can have is a function of its shape and this.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SubjectKind {
    Int,
    Class,
}

/// One generator's walk. `find` is what its `find` parsed to, `None` when it did not: the
/// checks that depend on it are skipped then.
struct Walk {
    index: usize,
    find: Option<FindMethod>,
    errors: Vec<JsonModelError>,
}

impl Walk {
    fn push(&mut self, error: JsonModelError) {
        self.errors.push(error);
    }

    fn missing(&mut self, field: &str) {
        self.push(JsonModelError::MissingField {
            index: self.index,
            field_name: field.to_string(),
        });
    }

    fn not_string(&mut self, field: &str) {
        self.push(JsonModelError::FieldNotString {
            index: self.index,
            field_name: field.to_string(),
        });
    }

    fn not_array(&mut self, field: &str) {
        self.push(JsonModelError::FieldNotArray {
            index: self.index,
            field_name: field.to_string(),
        });
    }

    fn unexpected_field(&mut self, field: &str, message: impl Into<String>) {
        self.push(JsonModelError::UnexpectedField {
            index: self.index,
            field_name: field.to_string(),
            message: message.into(),
        });
    }

    fn unexpected_constraint(&mut self, kind: &str) {
        self.push(JsonModelError::UnexpectedConstraint {
            index: self.index,
            constraint_type: kind.to_string(),
        });
    }

    fn check_keys(&mut self, value: &Value, what: &str, honored: &[&str]) {
        if let Some(obj) = value.as_object() {
            spec::check_keys(obj, self.index, what, honored, &mut self.errors);
        }
    }

    /// The string field `field` of `value`, or `None` after reporting it missing or not a string.
    fn required_str<'v>(&mut self, value: &'v Value, field: &str) -> Option<&'v str> {
        let Some(v) = value.get(field) else {
            self.missing(field);
            return None;
        };
        if v.as_str().is_none() {
            self.not_string(field);
        }
        v.as_str()
    }

    /// Whether `pattern` compiles, after reporting it if it does not.
    fn regex(&mut self, pattern: &str) -> bool {
        match Regex::new(pattern) {
            Ok(_) => true,
            Err(source) => {
                self.push(JsonModelError::InvalidRegex {
                    index: self.index,
                    pattern: pattern.to_string(),
                    source,
                });
                false
            }
        }
    }

    /// The key set of a leaf constraint: every key honored, and at least one honored key
    /// present. False if anything was reported.
    fn constraint_keys(&mut self, value: &Value, honored: &[&str]) -> bool {
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
            self.unexpected_field(
                key,
                format!(
                    "not a recognized field of the '{kind}' constraint; expected one of {expected}"
                ),
            );
            ok = false;
        }
        if !honored.iter().any(|h| obj.contains_key(*h)) {
            self.missing(&honored.join("' / '"));
            ok = false;
        }
        ok
    }

    fn generator(&mut self, generator: &Value) {
        // The generator object's own keys: a misspelled `where` would otherwise match the whole
        // program.
        self.check_keys(
            generator,
            "model generator",
            &["find", "where", "model", "in", "on-unmatched"],
        );
        self.find(&generator["find"]);
        if let Some(cs) = generator.get("where") {
            match cs.as_array() {
                Some(items) => items.iter().for_each(|c| self.constraint(c)),
                None => self.not_array("where"),
            }
        }
        self.model(&generator["model"]);
    }

    fn find(&mut self, value: &Value) {
        match value.as_str() {
            Some("methods") => self.find = Some(FindMethod::Methods),
            Some("callsites") => self.find = Some(FindMethod::Callsites),
            Some("dispatch") => self.find = Some(FindMethod::Dispatch),
            Some(other) => self.unexpected_constraint(other),
            None if value.is_null() => self.missing("find"),
            None => self.not_string("find"),
        }
    }

    /// One `where` constraint, recursively. `parameter` / `any_parameter` are not constraints,
    /// and are reported as unknown like any unrecognized discriminator.
    fn constraint(&mut self, c: &Value) {
        let is_dispatch = self.find == Some(FindMethod::Dispatch);
        match c["constraint"].as_str() {
            // A dispatch generator selects signature keys, which have no body, parameters or
            // caller to test.
            Some(rejected @ ("in_function" | "has_code" | "number_parameters" | "uses_field"))
                if is_dispatch =>
            {
                self.unexpected_field(
                    rejected,
                    format!(
                        "'{rejected}' is not supported with find: dispatch; it needs a function \
                         and a dispatch generator matches a call site's signature"
                    ),
                );
            }
            Some("signature_match") => self.signature_match(c),
            Some("signature" | "signature_pattern") => self.signature(c),
            Some("parent" | "extends") => self.predicate_constraint(c, SubjectKind::Class),
            Some("number_parameters") => self.predicate_constraint(c, SubjectKind::Int),
            Some("has_code") => match c.get("value") {
                Some(v) if v.as_bool().is_none() => self.not_string("value"),
                Some(_) => {}
                None => self.missing("value"),
            },
            Some("name") => self.name(c),
            Some("any_of" | "all_of") => match c.get("inners").and_then(|v| v.as_array()) {
                Some(inners) => inners.iter().for_each(|inner| self.constraint(inner)),
                None => self.not_array("inners"),
            },
            Some("not") => match c.get("inner") {
                Some(inner) => self.constraint(inner),
                None => self.missing("inner"),
            },
            Some("uses_field") => {
                if self.constraint_keys(c, &["name", "names"])
                    && c.get("names").is_some_and(|v| !v.is_array())
                {
                    self.not_array("names");
                }
            }
            Some("in_function") => self.in_function(c),
            Some(other) => self.unexpected_constraint(other),
            None => self.unexpected_constraint("<missing>"),
        }
    }

    fn signature_match(&mut self, c: &Value) {
        if !self.constraint_keys(
            c,
            &[
                "name",
                "names",
                "parent",
                "parents",
                "qualified-id",
                "qualified-ids",
            ],
        ) {
            return;
        }
        if self.find.is_none() {
            return;
        }
        for plural in ["names", "parents", "qualified-ids"] {
            if c.get(plural).is_some_and(|v| !v.is_array()) {
                self.not_array(plural);
            }
        }
    }

    fn signature(&mut self, c: &Value) {
        if !self.constraint_keys(c, &["pattern"]) || self.find.is_none() {
            return;
        }
        let Some(pattern) = c.get("pattern") else {
            return;
        };
        match pattern.as_str() {
            Some(pattern) => {
                self.regex(pattern);
            }
            None => self.not_string("pattern"),
        }
    }

    fn name(&mut self, c: &Value) {
        if self.find.is_none() {
            return;
        }
        match c.get("pattern") {
            Some(p) => match p.as_str() {
                Some(pattern) => {
                    self.regex(pattern);
                }
                None => self.not_string("pattern"),
            },
            None => self.missing("pattern"),
        }
    }

    /// `in_function` narrows the caller set, which only `find: callsites` has. Under a missing
    /// or unrecognized `find` it says nothing more: `find` was already reported.
    fn in_function(&mut self, c: &Value) {
        if !self.constraint_keys(c, &["inner"]) {
            return;
        }
        match self.find {
            Some(FindMethod::Callsites) => {
                if let Some(inner) = c.get("inner") {
                    self.constraint(inner);
                }
            }
            Some(FindMethod::Methods) => self.unexpected_field(
                "in_function",
                "'in_function' is only supported with find: callsites",
            ),
            _ => {}
        }
    }

    /// `number_parameters`, `parent` and `extends`: a required `inner` predicate tree.
    fn predicate_constraint(&mut self, c: &Value, kind: SubjectKind) {
        match c.get("inner") {
            Some(inner) => self.predicate(inner, kind),
            None => self.missing("inner"),
        }
    }

    /// A predicate tree applied to a [`SubjectKind`] scalar.
    fn predicate(&mut self, c: &Value, kind: SubjectKind) {
        match c["constraint"].as_str() {
            Some("any_of" | "all_of") => match c.get("inners").and_then(|v| v.as_array()) {
                Some(inners) => inners.iter().for_each(|inner| self.predicate(inner, kind)),
                None => self.not_array("inners"),
            },
            Some("not") => match c.get("inner") {
                Some(inner) => self.predicate(inner, kind),
                None => self.missing("inner"),
            },
            Some(op @ ("<" | "<=" | ">" | ">=" | "!=" | "==")) => {
                if kind != SubjectKind::Int {
                    self.unexpected_constraint(op);
                    return;
                }
                if c.get("value").and_then(|v| v.as_i64()).is_none() {
                    self.push(JsonModelError::InvalidInteger {
                        index: self.index,
                        source: "".parse::<i64>().unwrap_err(),
                    });
                    return;
                }
                self.constraint_keys(c, &["value"]);
            }
            Some("name") => {
                if kind != SubjectKind::Class {
                    self.unexpected_constraint("name");
                    return;
                }
                let Some(pattern) = c.get("pattern").and_then(|v| v.as_str()) else {
                    self.not_string("pattern");
                    return;
                };
                if self.regex(pattern) {
                    self.constraint_keys(c, &["pattern"]);
                }
            }
            Some("signature_match") => {
                if kind != SubjectKind::Class {
                    self.unexpected_constraint("signature_match");
                    return;
                }
                // The subject is already a class name, so only `name` / `names` mean anything.
                self.constraint_keys(c, &["name", "names"]);
            }
            Some(other) => self.unexpected_constraint(other),
            None => self.unexpected_constraint("<missing>"),
        }
    }

    fn model(&mut self, model: &Value) {
        if self.find == Some(FindMethod::Dispatch) {
            self.dispatch_model(model);
            return;
        }
        // An empty `model` is legal (a bridge generator's carries only `bridge`).
        self.check_keys(
            model,
            "model",
            &[
                "sources",
                "sinks",
                "taint",
                "propagation",
                "modes",
                "forward_self",
                "bridge",
                "access_paths",
            ],
        );
        for (field, visit) in [
            ("propagation", Self::propagation as fn(&mut Self, &Value)),
            ("sinks", Self::sink),
            ("sources", Self::source),
        ] {
            if let Some(list) = model.get(field) {
                match list.as_array() {
                    Some(items) => items.iter().for_each(|item| visit(self, item)),
                    None => self.not_array(field),
                }
            }
        }
        if let Some(paths) = model.get("access_paths") {
            self.access_paths(paths);
        }
        if let Some(modes) = model.get("modes") {
            self.modes(modes);
        }
        // A `bridge` was checked by `spec::parse_bridge` before the walk.
    }

    fn propagation(&mut self, value: &Value) {
        if self.find == Some(FindMethod::Callsites) {
            self.unexpected_field(
                "propagation",
                "'propagation' is not supported with find: callsites",
            );
            return;
        }
        if value.get("wildcard").is_some() {
            self.unexpected_field("wildcard", "'wildcard' is only valid on sink ports");
            return;
        }
        if self.find == Some(FindMethod::Methods) {
            self.propagation_ports(value);
        }
    }

    /// The `input` / `output` pair of a propagation. Neither may be a `Variable(...)`: a
    /// summary has no column for a named local.
    fn propagation_ports(&mut self, value: &Value) -> Option<()> {
        let input = self.required_str(value, "input")?;
        let output = self.required_str(value, "output")?;
        let input = self.port(input)?;
        let output = self.port(output)?;
        for (port, field) in [(input, "input"), (output, "output")] {
            if port == FormalIndexTypeTag::Local {
                self.unexpected_field(
                    field,
                    "'Variable(...)' ports are only valid on source/sink ports",
                );
                return None;
            }
        }
        Some(())
    }

    /// A port's tag, or `None` after reporting why it does not parse.
    fn port(&mut self, text: &str) -> Option<FormalIndexTypeTag> {
        match parse_port(text, self.index) {
            Ok(port) => Some(port.tag),
            Err(e) => {
                self.push(e);
                None
            }
        }
    }

    fn source(&mut self, value: &Value) {
        if self.required_str(value, "kind").is_none() {
            return;
        }
        let Some(port) = self.required_str(value, "port") else {
            return;
        };
        if value.get("wildcard").is_some() {
            self.unexpected_field("wildcard", "'wildcard' is only valid on sink ports");
            return;
        }
        if value.get("saturating").is_some_and(|v| !v.is_boolean()) {
            self.not_string("saturating");
            return;
        }
        self.endpoint_port(port);
    }

    fn sink(&mut self, value: &Value) {
        if self.required_str(value, "kind").is_none() {
            return;
        }
        let Some(port) = self.required_str(value, "port") else {
            return;
        };
        if value.get("saturating").is_some() {
            self.unexpected_field("saturating", "'saturating' is only valid on source ports");
            return;
        }
        if value.get("wildcard").is_some_and(|v| !v.is_boolean()) {
            self.not_string("wildcard");
            return;
        }
        self.endpoint_port(port);
    }

    /// A source or sink port. A callee's locals are not a call-site concept, so `find:
    /// callsites` refuses `Variable(...)`.
    fn endpoint_port(&mut self, text: &str) {
        if self.port(text) == Some(FormalIndexTypeTag::Local)
            && self.find == Some(FindMethod::Callsites)
        {
            self.unexpected_field(
                "port",
                "'Variable(...)' is not supported with find: callsites",
            );
        }
    }

    fn access_paths(&mut self, value: &Value) {
        let Some(items) = value.as_array() else {
            self.not_array("access_paths");
            return;
        };
        for item in items {
            let Some(text) = item.as_str() else {
                self.not_string("access_paths");
                continue;
            };
            if let Err(e) = spec::parse_declared_access_path(text, self.index) {
                self.push(e);
            }
        }
    }

    fn modes(&mut self, value: &Value) {
        let Some(items) = value.as_array() else {
            self.not_array("modes");
            return;
        };
        // A mode is a property of a function, and `find: callsites` selects call sites.
        if self.find == Some(FindMethod::Callsites) {
            self.unexpected_field("modes", "'modes' is not supported with find: callsites");
            return;
        }
        for item in items {
            match item.as_str() {
                Some(
                    "skip-analysis" | "mem2reg-full" | "mem2reg-window" | "frame-store"
                    | "frame-update",
                ) => {}
                Some(other) => self.unexpected_field(
                    "modes",
                    format!(
                        "unknown mode '{other}'; the defined values are 'skip-analysis', \
                         'mem2reg-full', 'mem2reg-window', 'frame-store' and 'frame-update'"
                    ),
                ),
                None => self.not_string("modes"),
            }
        }
    }

    /// The `model` of a `find: dispatch` generator: exactly one of `propagation` (possibly with
    /// `resolve`) and `closure_shaped`, and nothing else. An empty `propagation` list is legal
    /// and means "discard this call's targets".
    fn dispatch_model(&mut self, value: &Value) {
        let Some(obj) = value.as_object() else {
            self.missing("model");
            return;
        };
        let mut ok = true;
        for key in obj.keys() {
            if key == "propagation" || key == "resolve" || key == "closure_shaped" {
                continue;
            }
            self.unexpected_field(
                key,
                "not supported with find: dispatch; a dispatch model carries \
                 'propagation', 'resolve' (optionally with 'propagation') or \
                 'closure_shaped'",
            );
            ok = false;
        }
        let has_propagation = obj.contains_key("propagation");
        let has_resolve = obj.contains_key("resolve");
        let has_closure = obj.contains_key("closure_shaped");
        let kinds = usize::from(has_propagation || has_resolve) + usize::from(has_closure);
        if kinds != 1 {
            self.missing("propagation' / 'resolve' / 'closure_shaped");
            ok = false;
        }
        if !ok {
            return;
        }
        if has_closure {
            if !obj["closure_shaped"].is_boolean() {
                self.not_string("closure_shaped");
            }
            return;
        }
        if has_propagation {
            let Some(items) = obj["propagation"].as_array() else {
                self.not_array("propagation");
                return;
            };
            for item in items {
                if self.propagation_ports(item).is_none() {
                    return;
                }
            }
        }
        if has_resolve {
            match obj["resolve"].as_str() {
                Some("inline") => {}
                Some(other) => self.unexpected_field(
                    "resolve",
                    format!("unknown value '{other}'; the defined value is 'inline'"),
                ),
                None => self.not_string("resolve"),
            }
        }
    }
}

static ARGUMENT_REGEX: OnceLock<Regex> = OnceLock::new();
static RETURN_REGEX: OnceLock<Regex> = OnceLock::new();
static VARIABLE_REGEX: OnceLock<Regex> = OnceLock::new();

// All three are anchored, so "MyReturnType" is not a `Return` port with access path `Type`, and
// no text may surround an `Argument(n)`. A non-match falls through to `InvalidArgumentFormat`.

#[inline]
fn argument_regex() -> &'static Regex {
    ARGUMENT_REGEX.get_or_init(|| Regex::new(r#"^Argument\((\d+|[*])\)(.*)$"#).unwrap())
}

#[inline]
fn return_regex() -> &'static Regex {
    RETURN_REGEX.get_or_init(|| Regex::new(r#"^Return(.*)$"#).unwrap())
}

#[inline]
fn variable_regex() -> &'static Regex {
    // `Variable(name)` selects a source/sink port by the local's source name, with an
    // optional trailing access path (`Variable(buf).headers`).
    VARIABLE_REGEX.get_or_init(|| Regex::new(r#"^Variable\(([^)]+)\)(.*)$"#).unwrap())
}

/// A parsed source/sink/propagation port. `index` is `Some` only for a positional
/// `Argument(n)` port (`tag == Index`); a `Variable(name)` port is `tag == Local`, and `local`
/// is its name.
#[derive(Debug)]
pub(crate) struct ParsedPort {
    pub(crate) tag: FormalIndexTypeTag,
    pub(crate) index: Option<i16>,
    /// The local a `Variable(name)` port names.
    pub(crate) local: Option<String>,
    /// The port's access path, parsed with the canonical grammar. A `PathSegment::Offset` here
    /// is what lets a port name a real offset (`Argument(1).[8].deref`) and so match the
    /// offsets pcode emits.
    pub(crate) ap: Vec<PathSegment>,
}

/// Parses a port, `Argument(n)`, `Argument(*)`, `Return` or `Variable(name)`, with an optional
/// trailing access path. `index` is the generator the port belongs to, for error messages.
pub(crate) fn parse_port(text: &str, index: usize) -> Result<ParsedPort, JsonModelError> {
    // Note the absence of `?` in the two `if let` arms: an early return would skip the
    // index-patching `map_err` below, and the error would name generator 0.
    let parsed = if let Some(m) = variable_regex().captures(text) {
        // `Variable(name)` — name-based local selector, which no index can stand for.
        parse_access_path(m.get(2).map(|m| m.as_str())).map(|ap| ParsedPort {
            tag: FormalIndexTypeTag::Local,
            index: None,
            local: Some(m[1].to_string()),
            ap,
        })
    } else if let Some(m) = return_regex().captures(text) {
        parse_access_path(m.get(1).map(|m| m.as_str())).map(|ap| ParsedPort {
            tag: FormalIndexTypeTag::Return,
            index: None,
            local: None,
            ap,
        })
    } else {
        parse_argument(text).map(|(tag, idx, ap)| ParsedPort {
            tag,
            index: idx,
            local: None,
            ap,
        })
    };
    parsed.map_err(|mut err| {
        // These are raised with `index: 0` from helpers that do not know it; fill it in.
        match &mut err {
            JsonModelError::InvalidArgumentFormat {
                index: err_index, ..
            } => *err_index = index,
            JsonModelError::InvalidInteger {
                index: err_index, ..
            } => *err_index = index,
            JsonModelError::InvalidAccessPath {
                index: err_index, ..
            } => *err_index = index,
            _ => {}
        }
        err
    })
}

/// Parses a port's trailing access path with the one canonical grammar, so `.[8]` is a real
/// offset and a field name beginning with `[` is written `\[`.
///
/// `".*"` parses as `Symbol("*")`, a literal field, as it does everywhere else, and `.[*]` is
/// an `InvalidOffset` error.
fn parse_access_path(input_text: Option<&str>) -> Result<Vec<PathSegment>, JsonModelError> {
    let Some(s) = input_text else {
        return Ok(Vec::new());
    };
    mir::parse_segments(s).map_err(|source| JsonModelError::InvalidAccessPath {
        index: 0, // filled in by `parse_port`
        text: s.to_string(),
        source,
    })
}

fn parse_argument(
    input_text: &str,
) -> Result<(FormalIndexTypeTag, Option<i16>, Vec<PathSegment>), JsonModelError> {
    let m = argument_regex().captures(input_text).ok_or_else(|| {
        JsonModelError::InvalidArgumentFormat {
            index: 0, // filled in by `parse_port`
            text: input_text.to_string(),
        }
    })?;
    let arg_text =
        m.get(1)
            .map(|m| m.as_str())
            .ok_or_else(|| JsonModelError::InvalidArgumentFormat {
                index: 0,
                text: input_text.to_string(),
            })?;
    let (tag, index) = match arg_text {
        "*" => (FormalIndexTypeTag::AnyArgument, None),
        _ => (
            FormalIndexTypeTag::Index,
            Some(
                arg_text
                    .parse::<i16>()
                    .map_err(|source| JsonModelError::InvalidInteger { index: 0, source })?,
            ),
        ),
    };
    let p = parse_access_path(m.get(2).map(|m| m.as_str()))?;
    Ok((tag, index, p))
}

#[cfg(test)]
mod parse_port_tests {
    use super::*;

    #[test]
    fn parses_variable_selector() {
        let p = parse_port("Variable(buf)", 0).expect("parse");
        assert_eq!(p.tag, FormalIndexTypeTag::Local);
        assert_eq!(p.index, None);
        assert!(p.ap.is_empty());
    }

    #[test]
    fn parses_variable_selector_with_access_path() {
        let p = parse_port("Variable(buf).headers", 0).expect("parse");
        assert_eq!(p.tag, FormalIndexTypeTag::Local);
        assert_eq!(p.ap, vec![PathSegment::symbol("headers")]);
    }

    #[test]
    fn argument_and_return_still_parse() {
        let a = parse_port("Argument(1)", 0).expect("parse");
        assert_eq!(a.tag, FormalIndexTypeTag::Index);
        assert_eq!(a.index, Some(1));

        let r = parse_port("Return", 0).expect("parse");
        assert_eq!(r.tag, FormalIndexTypeTag::Return);
    }

    /// A model port can name an offset: `Argument(1).[8]` is `Offset(8)`, which is what pcode's
    /// `push_offset` emits, not `Symbol("[8]")`.
    #[test]
    fn port_can_name_an_offset() {
        let p = parse_port("Argument(1).[8].deref", 0).expect("parse");
        assert_eq!(p.tag, FormalIndexTypeTag::Index);
        assert_eq!(p.index, Some(1));
        assert_eq!(
            p.ap,
            vec![PathSegment::offset(8), PathSegment::symbol("deref")]
        );
    }

    /// ... and a field name that begins with `[` is still reachable, written `\[`. This is how a
    /// model names a Java array element, whose frontend segment is `Symbol("[]")`.
    #[test]
    fn port_can_name_a_bracketed_field() {
        let p = parse_port(r"Argument(0).\[]", 0).expect("parse");
        assert_eq!(p.ap, vec![PathSegment::symbol("[]")]);

        let p = parse_port(r"Argument(0).\[_elem_]", 0).expect("parse");
        assert_eq!(p.ap, vec![PathSegment::symbol("[_elem_]")]);
    }

    /// `.*` is `Symbol("*")`, a literal field, as it is everywhere else in the tree.
    #[test]
    fn trailing_star_is_a_literal_field() {
        let p = parse_port("Argument(0).*", 0).expect("parse");
        assert_eq!(p.ap, vec![PathSegment::symbol("*")]);

        let r = parse_port("Return.*", 0).expect("parse");
        assert_eq!(r.tag, FormalIndexTypeTag::Return);
        assert_eq!(r.ap, vec![PathSegment::symbol("*")]);
    }

    #[test]
    fn malformed_access_paths_are_hard_errors() {
        use crate::error::JsonModelError;
        use ctadl_ir::mir::PathSyntaxErrorKind;

        // `*` is not an offset.
        let e = parse_port("Argument(0).[*]", 7).unwrap_err();
        assert!(
            matches!(&e, JsonModelError::InvalidAccessPath { index: 7, source, .. }
                     if source.kind == PathSyntaxErrorKind::InvalidOffset("*".into())),
            "got {e:?}"
        );

        // An empty segment is an error, not `Symbol("")`.
        let e = parse_port("Argument(0).a..b", 3).unwrap_err();
        assert!(
            matches!(&e, JsonModelError::InvalidAccessPath { index: 3, source, .. }
                     if source.kind == PathSyntaxErrorKind::EmptySegment),
            "got {e:?}"
        );

        // A bare bracketed name is an offset position, and the message says how to fix it.
        let e = parse_port("Argument(0).[_elem_]", 0).unwrap_err();
        assert!(
            matches!(&e, JsonModelError::InvalidAccessPath { source, .. }
                     if source.kind == PathSyntaxErrorKind::InvalidOffset("_elem_".into())),
            "got {e:?}"
        );
        assert!(
            e.to_string().contains(r"\[_elem_]"),
            "message must name the fix: {e}"
        );
    }

    /// The regexes are anchored: "MyReturnType" is not a `Return` port with access path `Type`,
    /// and no text may surround an `Argument(n)`.
    #[test]
    fn regexes_are_anchored() {
        use crate::error::JsonModelError;

        // Junk *before* the selector: no regex matches at all, so this is
        // InvalidArgumentFormat.
        for text in [
            "MyReturnType",
            "prefixReturn",
            "xxArgument(0)",
            "aVariable(b)",
        ] {
            let e = parse_port(text, 5).unwrap_err();
            assert!(
                matches!(&e, JsonModelError::InvalidArgumentFormat { index: 5, text: t } if t == text),
                "{text:?} should be InvalidArgumentFormat, got {e:?}"
            );
        }

        // Junk *after* the selector is captured as the trailing access path, so it is rejected
        // by the grammar instead -- also a hard error, and a more specific message.
        for text in ["Argument(0)yy", "ReturnType", "Variable(b)zz"] {
            let e = parse_port(text, 5).unwrap_err();
            assert!(
                matches!(&e, JsonModelError::InvalidAccessPath { index: 5, .. }),
                "{text:?} should be InvalidAccessPath, got {e:?}"
            );
        }
    }
}
