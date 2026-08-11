/*! Program-independent model specs: `in` scopes, bridging models, and declared access paths.

A bridge pins two sets of matches in two *different* programs, so it is the one model that
cannot be resolved against a single program. What it needs before matching starts -- the two
sides' scopes, their raw `where` constraints, and the port map -- needs no program at all, so it
is scanned out of the `--models` files **once, before the import loop**. Parsing needs no
program, hoisting it avoids re-parsing per import, and indexing learns up front whether any
bridge exists.

The `where` constraints stay as raw [`serde_json::Value`]: they are evaluated later by the
*existing* evaluator ([`super::json::ModelGeneratorIngest`]) against the *existing*
[`super::match_index::ProgramMatchIndex`], so nothing here has to understand them. There must
not be a second implementation of `where`; that is how `signature_match` ends up meaning two
different things in two places.

# Loader hardening

The JSON schema is editor-time only -- it is never evaluated at load. So every key this module
reads is checked explicitly, at the generator level, the `model` level, and inside `bridge`, and
a misspelling is a hard error. For the same reason nothing here inherits the
`.as_array().unwrap()` pattern the older `super_model_generator` / `super_model` traversal uses:
a non-array `where` is an error, not a panic.
*/

use std::collections::BTreeSet;
use std::path::{Path as FsPath, PathBuf};

use crate::codegen::{GLOBALS_INDEX, RETURN_INDEX};
use crate::error::{Error, ErrorContext, JsonModelError, JsonModelErrors};
use crate::facts::{self, FormalIndex};
use crate::models::FormalIndexTypeTag;
use crate::project::ArtifactLanguage;

/// What to do when a bridge side matches nothing, or when a pairing is ambiguous.
///
/// Both conditions default to [`Severity::Warn`]. A bridge that fires on nothing produces an
/// analysis with fewer flows rather than an error, which is indistinguishable from a clean app,
/// so the default has to say something; erroring by default would break a bridge written against
/// a family of optional symbols on the first artifact that lacks one.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Severity {
    Ignore,
    Warn,
    Error,
}

impl Severity {
    fn parse(value: &serde_json::Value, index: usize, key: &str) -> Result<Self, JsonModelError> {
        match value.as_str() {
            Some("ignore") => Ok(Severity::Ignore),
            Some("warn") => Ok(Severity::Warn),
            Some("error") => Ok(Severity::Error),
            Some(other) => Err(JsonModelError::UnexpectedField {
                index,
                field_name: key.to_string(),
                message: format!("'{other}' is not one of 'ignore', 'warn', 'error'"),
            }),
            None => Err(JsonModelError::FieldNotString {
                index,
                field_name: key.to_string(),
            }),
        }
    }
}

/// Which of a port pair's two `assign` rows get pushed.
///
/// `actual_param` is unconditionally bidirectional, which is why a bridge routes its ports
/// through a temporary: a pair of `assign` rows is two independent facts, so a direction is
/// simply which of them exist.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum Direction {
    /// Only into the callee: `t.to_path = formal.from_path`.
    In,
    /// Only out of the callee: `formal.from_path = t.to_path`.
    Out,
    /// Both, matching how the engine treats an ordinary call.
    #[default]
    Both,
}

impl Direction {
    #[inline]
    pub fn inward(self) -> bool {
        matches!(self, Direction::In | Direction::Both)
    }

    #[inline]
    pub fn outward(self) -> bool {
        matches!(self, Direction::Out | Direction::Both)
    }

    fn parse(value: &serde_json::Value, index: usize) -> Result<Self, JsonModelError> {
        match value.as_str() {
            Some("in") => Ok(Direction::In),
            Some("out") => Ok(Direction::Out),
            Some("both") => Ok(Direction::Both),
            Some(other) => Err(JsonModelError::UnexpectedField {
                index,
                field_name: "direction".to_string(),
                message: format!("'{other}' is not one of 'in', 'out', 'both'"),
            }),
            None => Err(JsonModelError::FieldNotString {
                index,
                field_name: "direction".to_string(),
            }),
        }
    }
}

/// Which imports a generator (or one side of a bridge) applies to -- the `in` block.
///
/// Keys *within* one block are ANDed. An empty [`Self::languages`] means the language dimension
/// is unconstrained, not that nothing matches; `"languages": []` in a file is a hard error
/// precisely so the two cannot be confused.
///
/// `language` and `languages` normalize into the one vector at parse time, so [`Self::admits`]
/// has a single implementation and no caller ever asks which spelling the file used.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProgramScope {
    /// Admitted languages; empty means every language.
    pub languages: Vec<ArtifactLanguage>,
    /// Admitted import name; `None` means every import.
    pub import: Option<String>,
}

impl ProgramScope {
    /// Whether this scope admits the import `scope` identifies.
    ///
    /// An import whose identity is unknown (see [`ImportScope::unknown`]) is admitted only by a
    /// scope that constrains nothing. That is fail-closed: a caller with no import identity
    /// cannot honor `in`, and quietly matching would apply a `pcode`-scoped libc model to a dex
    /// program.
    pub fn admits(&self, scope: &ImportScope) -> bool {
        if !self.languages.is_empty() {
            match scope.language {
                Some(language) if self.languages.contains(&language) => {}
                _ => return false,
            }
        }
        if let Some(want) = &self.import {
            match &scope.import {
                Some(have) if have == want => {}
                _ => return false,
            }
        }
        true
    }

    /// True when the scope constrains nothing, so it admits every import.
    pub fn is_unconstrained(&self) -> bool {
        self.languages.is_empty() && self.import.is_none()
    }

    /// Parses an `in` block. `None` (an absent block) is the unconstrained scope.
    pub fn parse(
        value: Option<&serde_json::Value>,
        index: usize,
        errors: &mut Vec<JsonModelError>,
    ) -> Self {
        let mut scope = ProgramScope::default();
        let Some(value) = value else {
            return scope;
        };
        let Some(obj) = value.as_object() else {
            errors.push(JsonModelError::UnexpectedField {
                index,
                field_name: "in".to_string(),
                message: "must be an object, e.g. {\"language\": \"dex\"}".to_string(),
            });
            return scope;
        };
        check_keys(
            obj,
            index,
            "in",
            &["language", "languages", "import"],
            errors,
        );

        // Both spellings in one block is an error rather than a union: a reader cannot tell
        // which was meant, and the schema's `not: {required: [...]}` catches it only in an
        // editor.
        if obj.contains_key("language") && obj.contains_key("languages") {
            errors.push(JsonModelError::UnexpectedField {
                index,
                field_name: "languages".to_string(),
                message: "'language' and 'languages' are mutually exclusive in one 'in' block"
                    .to_string(),
            });
        }
        let mut push_language =
            |v: &serde_json::Value, errors: &mut Vec<JsonModelError>| match v.as_str() {
                Some(name) => match ArtifactLanguage::from_name(name) {
                    Some(language) => scope.languages.push(language),
                    None => errors.push(JsonModelError::UnexpectedField {
                        index,
                        field_name: "language".to_string(),
                        message: format!(
                            "'{name}' is not a known artifact language; expected one of {}",
                            language_list()
                        ),
                    }),
                },
                None => errors.push(JsonModelError::FieldNotString {
                    index,
                    field_name: "language".to_string(),
                }),
            };
        if let Some(one) = obj.get("language") {
            push_language(one, errors);
        }
        if let Some(many) = obj.get("languages") {
            match many.as_array() {
                Some(items) if items.is_empty() => errors.push(JsonModelError::UnexpectedField {
                    index,
                    field_name: "languages".to_string(),
                    message: "must not be empty; omit the key to mean 'any language'".to_string(),
                }),
                Some(items) => {
                    for item in items {
                        push_language(item, errors);
                    }
                }
                None => errors.push(JsonModelError::FieldNotArray {
                    index,
                    field_name: "languages".to_string(),
                }),
            }
        }
        match obj.get("import") {
            None => {}
            Some(v) => match v.as_str() {
                Some(name) => scope.import = Some(name.to_string()),
                None => errors.push(JsonModelError::FieldNotString {
                    index,
                    field_name: "import".to_string(),
                }),
            },
        }
        scope
    }
}

fn language_list() -> String {
    ArtifactLanguage::ALL
        .iter()
        .map(|l| format!("'{}'", l.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The identity of the import a model file is being matched against: what [`ProgramScope`]
/// filters on.
///
/// Both fields are optional because not every caller has an import in hand -- a unit test
/// matching against a synthesized `ProgramInfo`, for instance. `ProgramInfo` itself carries
/// neither the language nor the name, which is why this is threaded separately.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportScope {
    pub language: Option<ArtifactLanguage>,
    pub import: Option<String>,
}

impl ImportScope {
    /// The identity of a real import.
    pub fn new(language: ArtifactLanguage, import: &str) -> Self {
        Self {
            language: Some(language),
            import: Some(import.to_string()),
        }
    }

    /// No identity at all. Only an `in`-less generator applies to such an import; see
    /// [`ProgramScope::admits`].
    pub fn unknown() -> Self {
        Self::default()
    }

    /// How to name this import in a diagnostic.
    pub fn describe(&self) -> String {
        match (&self.import, self.language) {
            (Some(name), Some(language)) => format!("{name} ({language})"),
            (Some(name), None) => name.clone(),
            (None, Some(language)) => language.to_string(),
            (None, None) => "<unknown import>".to_string(),
        }
    }
}

/// One end of a port map entry: a formal index plus a literal access path.
///
/// The index is a [`FormalIndex`] rather than a tag/index pair because the port space a bridge
/// admits is exactly `Argument(n)` and `Return`; the globals pseudo-parameter is mapped
/// unconditionally and is not user-visible, and `Argument(*)` is rejected (a wildcard has no
/// correspondent).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BridgePort {
    pub index: FormalIndex,
    pub path: facts::Path,
}

impl BridgePort {
    fn parse(
        value: &serde_json::Value,
        index: usize,
        key: &str,
    ) -> Result<BridgePort, JsonModelError> {
        let text = value
            .as_str()
            .ok_or_else(|| JsonModelError::FieldNotString {
                index,
                field_name: key.to_string(),
            })?;
        let port = super::json::parse_port(text, index)?;
        let formal = match port.tag {
            FormalIndexTypeTag::Index => FormalIndex::new(port.index.expect("Index carries one")),
            FormalIndexTypeTag::Return => RETURN_INDEX.into(),
            // A wildcard has no correspondent on the other side: there is nothing for
            // `Argument(*)` to map *to*.
            FormalIndexTypeTag::AnyArgument => {
                return Err(JsonModelError::UnexpectedField {
                    index,
                    field_name: key.to_string(),
                    message: "'Argument(*)' is not valid in a bridge port map: a wildcard has no \
                              correspondent on the other side"
                        .to_string(),
                });
            }
            FormalIndexTypeTag::Local => {
                return Err(JsonModelError::UnexpectedField {
                    index,
                    field_name: key.to_string(),
                    message: "'Variable(...)' ports are only valid on source/sink ports"
                        .to_string(),
                });
            }
            FormalIndexTypeTag::Global => {
                return Err(JsonModelError::UnexpectedField {
                    index,
                    field_name: key.to_string(),
                    message: "the globals pseudo-parameter is mapped unconditionally and is not \
                              user-visible"
                        .to_string(),
                });
            }
        };
        Ok(BridgePort {
            index: formal,
            path: facts::Path::from_accesses(port.ap),
        })
    }
}

/// One entry of a bridge's `arguments`: read as *"the caller's `from` vertex is the callee's
/// `to` vertex"*.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PortPair {
    pub from: BridgePort,
    pub to: BridgePort,
    pub direction: Direction,
}

impl PortPair {
    /// The globals pair, emitted for every bridge whether the model asks for it or not. Without
    /// it heap flows do not cross the boundary at all: taint in through one native function,
    /// held in a native global, out through another never comes back.
    pub fn globals() -> Self {
        let port = BridgePort {
            index: GLOBALS_INDEX.into(),
            path: facts::Path::empty(),
        };
        PortPair {
            from: port,
            to: port,
            direction: Direction::Both,
        }
    }

    /// The normal-return pair. A Java function's return arity is 2 (`-1` normal, `-2`
    /// exception) while a native function's is 1, so only `-1` is ever mapped.
    pub fn ret() -> Self {
        let port = BridgePort {
            index: RETURN_INDEX.into(),
            path: facts::Path::empty(),
        };
        PortPair {
            from: port,
            to: port,
            direction: Direction::Both,
        }
    }
}

/// One side of a bridge: which programs it looks in, what it matches there, and how loudly to
/// complain when it matches nothing.
#[derive(Clone, Debug)]
pub struct SideSpec {
    pub scope: ProgramScope,
    /// Raw `where` constraints, handed back to the shared evaluator unchanged.
    pub where_: Vec<serde_json::Value>,
    pub on_unmatched: Severity,
}

/// A parsed `model.bridge`, together with the generator it came from.
#[derive(Clone, Debug)]
pub struct BridgeSpec {
    /// Provenance, carried into every diagnostic exactly as the other loader messages do.
    pub source: PathBuf,
    pub index: usize,
    /// Side A, the call side: the generator's own `in` / `where`.
    pub from: SideSpec,
    /// Side B, the implementation side: the `to` block's `in` / `where`.
    pub to: SideSpec,
    /// The port map. Empty means "not given"; see [`Self::ports_given`].
    pub ports: Vec<PortPair>,
    /// Whether `arguments` was written out. When it was not, emission falls back to an identity
    /// map over the arity the two sides share (plus `Return`), which needs the fact base and so
    /// cannot be resolved here.
    pub ports_given: bool,
    pub on_ambiguous: Severity,
}

impl BridgeSpec {
    /// `path:index`, the provenance every loader message carries.
    pub fn provenance(&self) -> String {
        format!("{}:{}", self.source.display(), self.index)
    }
}

/// A bridge whose two sides are already known, with no `where` left to evaluate.
///
/// This is what the DSL engine produces. A [`BridgeSpec`] carries two *unevaluated* sides
/// because the JSON loader has to match them per import and pair them afterwards; a DSL rule
/// has already been grounded by the time its heads are instantiated, so both function names are
/// in hand and there is nothing left to diagnose. Phase 2 of codegen emits it through the same
/// path as a paired spec — see [`crate::codegen::model_matches`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedBridge {
    /// `file:rule`, for the per-bridge `info` line.
    pub provenance: String,
    /// The rule this came from, which is also what groups several ports into one bridge.
    pub rule: usize,
    /// Side A: the call side.
    pub from: facts::Str,
    /// Side B: the implementation side.
    pub to: facts::Str,
    /// The port map, in written order. The globals pair is added at emission, as it is for a
    /// spec-driven bridge.
    pub ports: Vec<PortPair>,
}

/// Everything a set of `--models` files contributes that does not depend on any one program.
#[derive(Clone, Debug, Default)]
pub struct ModelFileSpecs {
    pub bridges: Vec<BridgeSpec>,
}

impl ModelFileSpecs {
    pub fn is_empty(&self) -> bool {
        self.bridges.is_empty()
    }
}

/// Reads every model generator in `path`, in file order, handing each to `f` with its generator
/// index.
///
/// The index counts *generators*, not lines, so it names the same generator the matching pass
/// and the `CTADL0004` notification name.
pub fn visit_model_file<F>(path: &FsPath, mut f: F) -> Result<(), Error>
where
    F: FnMut(usize, &serde_json::Value) -> Result<(), Error>,
{
    use std::io::{BufRead, BufReader, Read};

    let extension = path.extension().and_then(|s| s.to_str());
    match extension {
        Some("jsonl") => {
            let file = std::fs::File::open(path)
                .err_context(|| format!("opening model JSONL file: {}", path.display()))?;
            let mut n = 0usize;
            for (lineno, line) in BufReader::new(file).lines().enumerate() {
                // Report the line the way an editor counts them, 1-based.
                let lineno = lineno + 1;
                let line =
                    line.err_context(|| format!("reading model JSONL file: {}", path.display()))?;
                let trimmed = line.trim_start();
                if trimmed.is_empty() || trimmed.starts_with("//") {
                    continue;
                }
                let value: serde_json::Value = serde_json::from_str(trimmed)
                    .err_context(|| format!("reading model line {lineno} of {}", path.display()))?;
                f(n, &value)?;
                n += 1;
            }
            Ok(())
        }
        other => {
            let root: serde_json::Value = if other == Some("json5") {
                let mut file = std::fs::File::open(path)
                    .err_context(|| format!("opening model JSON5 file: {}", path.display()))?;
                let mut content = String::new();
                file.read_to_string(&mut content)
                    .err_context(|| format!("reading model JSON5 file: {}", path.display()))?;
                json5::from_str(&content)
                    .err_context(|| format!("parsing model JSON5 file: {}", path.display()))?
            } else {
                let file = std::fs::File::open(path)
                    .err_context(|| format!("opening model JSON file: {}", path.display()))?;
                serde_json::from_reader(file)
                    .err_context(|| format!("reading model JSON file: {}", path.display()))?
            };
            let generators = match root.get("model_generators").and_then(|v| v.as_array()) {
                Some(arr) => arr,
                None => {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "missing or invalid 'model_generators' array",
                    )));
                }
            };
            for (n, value) in generators.iter().enumerate() {
                f(n, value)?;
            }
            Ok(())
        }
    }
}

/// Scans every `--models` file for the specs that need no program: bridges.
///
/// Run once, before the import loop. Errors carry the file they came from.
pub fn scan_model_files(paths: &[PathBuf]) -> Result<ModelFileSpecs, Error> {
    let mut specs = ModelFileSpecs::default();
    for path in paths {
        // A DSL file has no unevaluated bridge sides to scan: its rules are grounded by the
        // engine, and what comes out is a [`ResolvedBridge`] with both function names already
        // in it. Reading it as JSON here would fail on the first rule.
        if super::dsl::is_dsl_path(path) {
            continue;
        }
        let mut errors: Vec<JsonModelError> = Vec::new();
        visit_model_file(path, |n, value| {
            if let Some(bridge) = value.pointer("/model/bridge") {
                match parse_bridge(path, n, value, bridge, &mut errors) {
                    Some(spec) => specs.bridges.push(spec),
                    None => debug_assert!(!errors.is_empty(), "a rejected bridge must say why"),
                }
            }
            Ok(())
        })
        .err_context(|| format!("scanning model file: {}", path.display()))?;
        if !errors.is_empty() {
            let mut json_errors = JsonModelErrors::default();
            json_errors.extend(errors);
            return Err(Error::JsonModel(json_errors))
                .err_context(|| format!("scanning model file: {}", path.display()));
        }
    }
    if !specs.bridges.is_empty() {
        log::debug!("loaded {} bridging model(s)", specs.bridges.len());
    }
    Ok(specs)
}

/// Parses one generator's `model.bridge`. Returns `None` when anything was rejected; the reason
/// is in `errors`.
fn parse_bridge(
    source: &FsPath,
    index: usize,
    generator: &serde_json::Value,
    bridge: &serde_json::Value,
    errors: &mut Vec<JsonModelError>,
) -> Option<BridgeSpec> {
    let before = errors.len();

    // The generator object itself. `find` is a constant here (see below), so the honored keys
    // are the same ones any generator has plus the two this design adds.
    if let Some(obj) = generator.as_object() {
        check_keys(
            obj,
            index,
            "model generator",
            &["find", "where", "model", "in", "on-unmatched"],
            errors,
        );
    }

    // `find: callsites` with a bridge is a hard error, not a silently ignored key. `call` is an
    // EDB relation -- indirect and virtual dispatch resolve *inside* the fixpoint -- so a
    // callsite bridge would see only the statically emitted call rows: exactly the sites a
    // callsite bridge does not need, and none of the ones it does.
    match generator.get("find").and_then(|v| v.as_str()) {
        Some("methods") => {}
        Some(other) => errors.push(JsonModelError::UnexpectedField {
            index,
            field_name: "find".to_string(),
            message: format!(
                "a generator carrying a 'bridge' must use find: methods, not '{other}'; a bridge \
                 attaches inside the matched method so that every call site of it composes with \
                 the resulting summary"
            ),
        }),
        None => errors.push(JsonModelError::MissingField {
            index,
            field_name: "find".to_string(),
        }),
    }

    let from = SideSpec {
        scope: ProgramScope::parse(generator.get("in"), index, errors),
        where_: parse_where(generator.get("where"), index, "where", errors),
        on_unmatched: parse_severity(generator.get("on-unmatched"), index, "on-unmatched", errors),
    };

    let Some(bridge_obj) = bridge.as_object() else {
        errors.push(JsonModelError::UnexpectedField {
            index,
            field_name: "bridge".to_string(),
            message: "must be an object".to_string(),
        });
        return None;
    };
    check_keys(
        bridge_obj,
        index,
        "bridge",
        &["to", "arguments", "on-ambiguous"],
        errors,
    );

    let to = match bridge_obj.get("to") {
        Some(to) => match to.as_object() {
            Some(to_obj) => {
                check_keys(
                    to_obj,
                    index,
                    "bridge.to",
                    &["in", "where", "on-unmatched"],
                    errors,
                );
                SideSpec {
                    scope: ProgramScope::parse(to.get("in"), index, errors),
                    where_: parse_where(to.get("where"), index, "to.where", errors),
                    on_unmatched: parse_severity(
                        to.get("on-unmatched"),
                        index,
                        "to.on-unmatched",
                        errors,
                    ),
                }
            }
            None => {
                errors.push(JsonModelError::UnexpectedField {
                    index,
                    field_name: "to".to_string(),
                    message: "must be an object of the form {\"in\": …, \"where\": [ … ]}"
                        .to_string(),
                });
                return None;
            }
        },
        None => {
            errors.push(JsonModelError::MissingField {
                index,
                field_name: "to".to_string(),
            });
            return None;
        }
    };

    let (ports, ports_given) = match bridge_obj.get("arguments") {
        None => (Vec::new(), false),
        Some(args) => match args.as_array() {
            Some(items) => {
                let mut ports = Vec::with_capacity(items.len() + 2);
                for item in items {
                    if let Some(pair) = parse_port_pair(item, index, errors) {
                        ports.push(pair);
                    }
                }
                (ports, true)
            }
            None => {
                errors.push(JsonModelError::FieldNotArray {
                    index,
                    field_name: "arguments".to_string(),
                });
                (Vec::new(), true)
            }
        },
    };

    let on_ambiguous = parse_severity(
        bridge_obj.get("on-ambiguous"),
        index,
        "on-ambiguous",
        errors,
    );

    if errors.len() != before {
        return None;
    }
    Some(BridgeSpec {
        source: source.to_path_buf(),
        index,
        from,
        to,
        ports,
        ports_given,
        on_ambiguous,
    })
}

fn parse_port_pair(
    value: &serde_json::Value,
    index: usize,
    errors: &mut Vec<JsonModelError>,
) -> Option<PortPair> {
    let Some(obj) = value.as_object() else {
        errors.push(JsonModelError::UnexpectedField {
            index,
            field_name: "arguments".to_string(),
            message: "every entry must be an object of the form {\"from\": …, \"to\": …}"
                .to_string(),
        });
        return None;
    };
    check_keys(
        obj,
        index,
        "port map entry",
        &["from", "to", "direction"],
        errors,
    );
    let mut port = |key: &str| match obj.get(key) {
        Some(v) => match BridgePort::parse(v, index, key) {
            Ok(p) => Some(p),
            Err(e) => {
                errors.push(e);
                None
            }
        },
        None => {
            errors.push(JsonModelError::MissingField {
                index,
                field_name: key.to_string(),
            });
            None
        }
    };
    let from = port("from");
    let to = port("to");
    let direction = match obj.get("direction") {
        None => Direction::Both,
        Some(v) => match Direction::parse(v, index) {
            Ok(d) => d,
            Err(e) => {
                errors.push(e);
                Direction::Both
            }
        },
    };
    Some(PortPair {
        from: from?,
        to: to?,
        direction,
    })
}

fn parse_where(
    value: Option<&serde_json::Value>,
    index: usize,
    key: &str,
    errors: &mut Vec<JsonModelError>,
) -> Vec<serde_json::Value> {
    match value {
        // An absent `where` matches every function of the `find` kind, exactly as it does for
        // any other generator.
        None => Vec::new(),
        Some(v) => match v.as_array() {
            Some(items) => items.clone(),
            None => {
                errors.push(JsonModelError::FieldNotArray {
                    index,
                    field_name: key.to_string(),
                });
                Vec::new()
            }
        },
    }
}

fn parse_severity(
    value: Option<&serde_json::Value>,
    index: usize,
    key: &str,
    errors: &mut Vec<JsonModelError>,
) -> Severity {
    match value {
        None => Severity::Warn,
        Some(v) => match Severity::parse(v, index, key) {
            Ok(s) => s,
            Err(e) => {
                errors.push(e);
                Severity::Warn
            }
        },
    }
}

/// Rejects any key of `obj` that `honored` does not list.
///
/// The schema's `additionalProperties: false` is checked by an editor, never at load, so this is
/// the only thing standing between a misspelled `on-unmached` and a bridge that silently uses
/// the default.
pub(crate) fn check_keys(
    obj: &serde_json::Map<String, serde_json::Value>,
    index: usize,
    what: &str,
    honored: &[&str],
    errors: &mut Vec<JsonModelError>,
) {
    let expected = honored
        .iter()
        .map(|h| format!("'{h}'"))
        .collect::<Vec<_>>()
        .join(", ");
    for key in obj.keys() {
        if honored.contains(&key.as_str()) {
            continue;
        }
        errors.push(JsonModelError::UnexpectedField {
            index,
            field_name: key.clone(),
            message: format!("not a recognized field of the {what}; expected one of {expected}"),
        });
    }
}

/// Parses one entry of a `model.access_paths` list: a path in the canonical grammar, written the
/// way a port's trailing path is (`.next.next.next`).
pub(crate) fn parse_declared_access_path(
    text: &str,
    index: usize,
) -> Result<facts::Path, JsonModelError> {
    let segments = ctadl_ir::mir::parse_segments(text).map_err(|source| {
        JsonModelError::InvalidAccessPath {
            index,
            text: text.to_string(),
            source,
        }
    })?;
    if segments.is_empty() {
        return Err(JsonModelError::UnexpectedField {
            index,
            field_name: "access_paths".to_string(),
            message: "the empty path is always registered; name at least one segment, e.g. \
                      '.next.next'"
                .to_string(),
        });
    }
    Ok(facts::Path::from_accesses(segments))
}

/// The declared-access-path registry: paths that occur in no `assign` and no `actual_param`, so
/// nothing else would ever register them.
pub type DeclaredAccessPaths = BTreeSet<facts::Path>;

#[cfg(test)]
mod tests;
