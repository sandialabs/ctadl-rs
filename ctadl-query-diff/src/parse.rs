//! SARIF files to raw runs.
//!
//! A file is read with `serde-sarif`, the crate `ctadl query` writes with, and only the fields
//! the comparison uses are taken out of it; every other field is never looked at, whatever it
//! holds. Strings are interned into the caller's [`Atoms`]. Logical location references stay
//! numbers: resolving them, and checking their range, is the canonical form's job.

use std::borrow::Cow;
use std::fmt;
use std::path::Path;

use sarif_canon::types::{
    Address, Keyed, Physical, Props, RawCodeFlow, RawLocation, RawLogical, RawLogicalRef,
    RawResult, RawRun, RawStep, RawThreadFlow, Region,
};
use serde_json::Value;
use serde_sarif::sarif;

use crate::Atoms;

/// Why a file has no raw runs.
#[derive(Debug)]
pub enum Error {
    /// The file could not be read.
    Read(std::io::Error),
    /// The file is not JSON, or not SARIF as `serde-sarif` reads it (an unknown `kind` or
    /// `level` is one way).
    Sarif(serde_json::Error),
    /// A result property ctadl writes in one shape holds another.
    Property {
        key: &'static str,
        expected: &'static str,
    },
    /// A logical location reference whose `index` is negative.
    NegativeIndex(i64),
    /// A logical location with neither a `fullyQualifiedName` nor a `name`.
    UnnamedLogical,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read(e) => write!(f, "cannot read the file: {e}"),
            Error::Sarif(e) => write!(f, "not SARIF 2.1.0: {e}"),
            Error::Property { key, expected } => {
                write!(f, "result property `{key}` is not {expected}")
            }
            Error::NegativeIndex(i) => write!(f, "logical location index {i} is negative"),
            Error::UnnamedLogical => {
                write!(
                    f,
                    "a logical location has neither `fullyQualifiedName` nor `name`"
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Read(e) => Some(e),
            Error::Sarif(e) => Some(e),
            _ => None,
        }
    }
}

/// The raw runs of the SARIF file at `path`, in file order, their strings interned into
/// `atoms`.
///
/// Only the parsed runs outlive the call: the file's bytes and its parsed document are dropped
/// before it returns.
pub fn read_runs(path: &Path, atoms: &mut Atoms) -> Result<Vec<RawRun<u32>>, Error> {
    let bytes = std::fs::read(path).map_err(Error::Read)?;
    parse_runs(&bytes, atoms)
}

fn parse_runs(bytes: &[u8], atoms: &mut Atoms) -> Result<Vec<RawRun<u32>>, Error> {
    let log: sarif::Sarif = serde_json::from_slice(bytes).map_err(Error::Sarif)?;
    log.runs.iter().map(|run| raw_run(run, atoms)).collect()
}

fn raw_run(run: &sarif::Run, atoms: &mut Atoms) -> Result<RawRun<u32>, Error> {
    let logical = run
        .logical_locations
        .iter()
        .flatten()
        .map(|l| {
            Ok(RawLogical {
                fqn: logical_name(l, atoms)?,
            })
        })
        .collect::<Result<_, Error>>()?;
    let results = run
        .results
        .iter()
        .flatten()
        .map(|r| raw_result(r, atoms))
        .collect::<Result<_, Error>>()?;
    Ok(RawRun { logical, results })
}

fn raw_result(r: &sarif::Result, atoms: &mut Atoms) -> Result<RawResult<u32>, Error> {
    // SARIF's defaults, so an absent kind or level compares equal to the one it stands for.
    let kind = r.kind.as_ref().map(ToString::to_string);
    let level = r.level.as_ref().map(ToString::to_string);
    Ok(RawResult {
        rule: r.rule_id.as_deref().map(|s| atoms.intern(s)),
        kind: atoms.intern(kind.as_deref().unwrap_or("fail")),
        level: atoms.intern(level.as_deref().unwrap_or("warning")),
        locations: r
            .locations
            .iter()
            .flatten()
            .map(|l| location(l, atoms))
            .collect::<Result<_, Error>>()?,
        code_flows: r
            .code_flows
            .iter()
            .flatten()
            .map(|cf| code_flow(cf, atoms))
            .collect::<Result<_, Error>>()?,
        props: props(r.properties.as_ref(), atoms)?,
    })
}

fn code_flow(cf: &sarif::CodeFlow, atoms: &mut Atoms) -> Result<RawCodeFlow<u32>, Error> {
    let thread_flows = cf
        .thread_flows
        .iter()
        .map(|tf| {
            let steps = tf
                .locations
                .iter()
                .map(|s| step(s, atoms))
                .collect::<Result<_, Error>>()?;
            Ok(RawThreadFlow { steps })
        })
        .collect::<Result<_, Error>>()?;
    Ok(RawCodeFlow { thread_flows })
}

fn step(s: &sarif::ThreadFlowLocation, atoms: &mut Atoms) -> Result<RawStep<u32>, Error> {
    Ok(RawStep {
        order: s.execution_order,
        kinds: s.kinds.iter().flatten().map(|k| atoms.intern(k)).collect(),
        location: s
            .location
            .as_ref()
            .map(|l| location(l, atoms))
            .transpose()?,
        vertex: None,
        callee: None,
    })
}

fn location(l: &sarif::Location, atoms: &mut Atoms) -> Result<RawLocation<u32>, Error> {
    Ok(RawLocation {
        physical: l.physical_location.as_ref().map(|p| physical(p, atoms)),
        logical: l
            .logical_locations
            .iter()
            .flatten()
            .map(|r| logical_ref(r, atoms))
            .collect::<Result<_, Error>>()?,
    })
}

fn physical(p: &sarif::PhysicalLocation, atoms: &mut Atoms) -> Physical<u32> {
    let artifact = p.artifact_location.as_ref();
    let mut intern = |s: Option<&String>| s.map(|s| atoms.intern(s));
    Physical {
        // The base's name only: its value is machine data, and lives in the run.
        base: intern(artifact.and_then(|a| a.uri_base_id.as_ref())),
        uri: intern(artifact.and_then(|a| a.uri.as_ref())),
        region: p.region.as_ref().map_or(
            Region {
                start_line: None,
                start_column: None,
                end_line: None,
                end_column: None,
                byte_offset: None,
                byte_length: None,
            },
            |r| Region {
                start_line: r.start_line,
                start_column: r.start_column,
                end_line: r.end_line,
                end_column: r.end_column,
                byte_offset: r.byte_offset,
                byte_length: r.byte_length,
            },
        ),
        address: p.address.as_ref().map(|a| Address {
            absolute: a.absolute_address,
            relative: a.relative_address,
            kind: intern(a.kind.as_ref()),
        }),
    }
}

/// A logical location reference: its `index` if it has one, else the name it carries.
fn logical_ref(l: &sarif::LogicalLocation, atoms: &mut Atoms) -> Result<RawLogicalRef<u32>, Error> {
    match l.index {
        Some(i) => usize::try_from(i)
            .map(RawLogicalRef::Index)
            .map_err(|_| Error::NegativeIndex(i)),
        None => logical_name(l, atoms).map(RawLogicalRef::Inline),
    }
}

fn logical_name(l: &sarif::LogicalLocation, atoms: &mut Atoms) -> Result<u32, Error> {
    l.fully_qualified_name
        .as_deref()
        .or(l.name.as_deref())
        .map(|s| atoms.intern(s))
        .ok_or(Error::UnnamedLogical)
}

/// The result properties that are compared, each in the shape ctadl writes it.
fn props(bag: Option<&sarif::PropertyBag>, atoms: &mut Atoms) -> Result<Props<u32>, Error> {
    let get = |key| bag.and_then(|b| b.additional_properties.get(key));
    Ok(Props {
        labels: get("taintLabels")
            .map(|v| strings(v, "taintLabels", atoms, as_is))
            .transpose()?,
        vertex: get("taintVertex")
            .map(|v| string(v, "taintVertex", atoms, without_call_site))
            .transpose()?,
        vertices: get("taintVertices")
            .map(|v| keyed(v, "taintVertices", atoms, without_call_site))
            .transpose()?,
        source_fns: get("sourceFunctions")
            .map(|v| strings(v, "sourceFunctions", atoms, as_is))
            .transpose()?,
        sink_fns: get("sinkFunctions")
            .map(|v| strings(v, "sinkFunctions", atoms, as_is))
            .transpose()?,
        tainted_formals: get("taintedFormals")
            .map(|v| keyed(v, "taintedFormals", atoms, as_is))
            .transpose()?,
        import: get("import")
            .map(|v| string(v, "import", atoms, as_is))
            .transpose()?,
    })
}

type Rewrite = fn(&str) -> Cow<'_, str>;

fn as_is(s: &str) -> Cow<'_, str> {
    Cow::Borrowed(s)
}

fn string(v: &Value, key: &'static str, atoms: &mut Atoms, rw: Rewrite) -> Result<u32, Error> {
    v.as_str()
        .map(|s| atoms.intern(&rw(s)))
        .ok_or(Error::Property {
            key,
            expected: "a string",
        })
}

fn strings(
    v: &Value,
    key: &'static str,
    atoms: &mut Atoms,
    rw: Rewrite,
) -> Result<Vec<u32>, Error> {
    let err = Error::Property {
        key,
        expected: "an array of strings",
    };
    let Value::Array(xs) = v else {
        return Err(err);
    };
    xs.iter()
        .map(|x| x.as_str().map(|s| atoms.intern(&rw(s))))
        .collect::<Option<_>>()
        .ok_or(err)
}

/// An object whose values are arrays of strings, as one entry per key. `rw` applies to the
/// values, not the keys.
fn keyed(
    v: &Value,
    key: &'static str,
    atoms: &mut Atoms,
    rw: Rewrite,
) -> Result<Vec<Keyed<u32>>, Error> {
    let err = || Error::Property {
        key,
        expected: "an object of arrays of strings",
    };
    let Value::Object(m) = v else {
        return Err(err());
    };
    m.iter()
        .map(|(k, xs)| {
            Ok(Keyed {
                key: atoms.intern(k),
                values: strings(xs, key, atoms, rw).map_err(|_| err())?,
            })
        })
        .collect()
}

/// A taint vertex without the call site of a `call-arg` vertex: `call-arg(<insn id>,
/// <formal>)<path>` becomes `call-arg(_, <formal>)<path>`. The instruction id is a per-index
/// number, and the result's location already names the call site; the formal and the path
/// stay, since they tell apart two endpoints on one call. A vertex is a variable followed by
/// its access path, so `call-arg` can only be its prefix; any other string is returned as is.
fn without_call_site(v: &str) -> Cow<'_, str> {
    fn digits(s: &str) -> usize {
        s.bytes().take_while(u8::is_ascii_digit).count()
    }
    let Some(rest) = v.strip_prefix("call-arg(") else {
        return Cow::Borrowed(v);
    };
    let n = digits(rest);
    if n == 0 {
        return Cow::Borrowed(v);
    }
    let Some(formal_on) = rest[n..].strip_prefix(", ") else {
        return Cow::Borrowed(v);
    };
    let unsigned = formal_on.strip_prefix('-').unwrap_or(formal_on);
    let m = digits(unsigned);
    if m == 0 || !unsigned[m..].starts_with(')') {
        return Cow::Borrowed(v);
    }
    Cow::Owned(format!("call-arg(_, {formal_on}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A run with one result per shape ctadl writes, every kept field present, and dropped
    /// fields set to values the tests change.
    fn doc() -> Value {
        json!({
            "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
            "version": "2.1.0",
            "properties": {"parquet_dirs": ["/a/b"]},
            "runs": [{
                "tool": {"driver": {"name": "ctadl", "version": "0.1.7"}},
                "invocations": [{
                    "executionSuccessful": true,
                    "commandLine": "ctadl query /a/b",
                    "startTimeUtc": "2026-01-01T00:00:00Z"
                }],
                "originalUriBaseIds": {"SRC0": {"uri": "file:///a/src/"}},
                "properties": {"parquet_dir": "/a/b"},
                "logicalLocations": [
                    {"kind": "member", "name": "main", "fullyQualifiedName": "main"},
                    {"kind": "function", "name": "sink_fn"}
                ],
                "results": [
                    {
                        "ruleId": "C0001",
                        "ruleIndex": 0,
                        "kind": "fail",
                        "level": "error",
                        "message": {"text": "Taint flow labelled 'L'"},
                        "locations": [{
                            "message": {"text": "here"},
                            "physicalLocation": {
                                "artifactLocation": {
                                    "uri": "a.c",
                                    "uriBaseId": "SRC0",
                                    "index": 0
                                },
                                "region": {
                                    "startLine": 3,
                                    "startColumn": 1,
                                    "endLine": 4,
                                    "endColumn": 9,
                                    "byteOffset": 10,
                                    "byteLength": 4,
                                    "message": {"text": "r"},
                                    "snippet": {"text": "f(x)"}
                                }
                            },
                            "logicalLocations": [
                                {"index": 0},
                                {"name": "n", "fullyQualifiedName": "inline_fqn"}
                            ]
                        }],
                        "codeFlows": [{
                            "message": {"text": "flow"},
                            "threadFlows": [{
                                "message": {"text": "thread"},
                                "locations": [
                                    {
                                        "executionOrder": 1,
                                        "kinds": ["taint"],
                                        "location": {
                                            "message": {"text": "step one"},
                                            "logicalLocations": [{"index": 0}]
                                        }
                                    },
                                    {
                                        "executionOrder": 2,
                                        "kinds": ["call", "taint"],
                                        "location": {
                                            "physicalLocation": {
                                                "address": {
                                                    "absoluteAddress": 4096,
                                                    "relativeAddress": 16,
                                                    "kind": "instruction"
                                                }
                                            },
                                            "logicalLocations": [{"index": 1}]
                                        }
                                    },
                                    {"executionOrder": 3}
                                ]
                            }]
                        }],
                        "properties": {
                            "taintLabels": ["L"],
                            "taintVertices": {"L": ["call-arg(12, 0).f", "local(x)"]},
                            "sourceCallee": "src",
                            "sourceFunctions": ["src"],
                            "sinkCallee": "snk",
                            "sinkFunctions": ["snk"],
                            "import": "imp",
                            "fileSpanId": 4,
                            "funcId": 5,
                            "insnId": 6
                        }
                    },
                    {
                        "ruleId": "C0003",
                        "kind": "informational",
                        "level": "none",
                        "message": {"text": "a source"},
                        "locations": [{"logicalLocations": [{"index": 1}]}],
                        "properties": {"taintLabels": ["L"], "taintVertex": "call-arg(99, -1)"}
                    },
                    {
                        "ruleId": "C0007",
                        "kind": "informational",
                        "level": "none",
                        "message": {"text": "Absorbing function: main receives tainted data"},
                        "locations": [{"logicalLocations": [{"index": 0}]}],
                        "properties": {"taintedFormals": {"0": ["L", "M"]}}
                    },
                    {"message": {"text": "no rule, no kind, no level"}}
                ]
            }]
        })
    }

    fn parse(v: &Value, atoms: &mut Atoms) -> Result<Vec<RawRun<u32>>, Error> {
        parse_runs(&serde_json::to_vec(v).unwrap(), atoms)
    }

    /// `doc()` with `value` at each JSON pointer (a missing last key is added).
    fn edit<P: AsRef<str>>(edits: &[(P, Value)]) -> Value {
        let mut d = doc();
        for (ptr, value) in edits {
            let (parent, key) = ptr.as_ref().rsplit_once('/').unwrap();
            match d.pointer_mut(parent) {
                Some(Value::Object(m)) => {
                    m.insert(key.to_string(), value.clone());
                }
                Some(Value::Array(xs)) => xs[key.parse::<usize>().unwrap()] = value.clone(),
                _ => panic!("no object or array at {parent}"),
            }
        }
        d
    }

    /// `doc()` without the key at each JSON pointer.
    fn remove<P: AsRef<str>>(ptrs: &[P]) -> Value {
        let mut d = doc();
        for ptr in ptrs {
            let (parent, key) = ptr.as_ref().rsplit_once('/').unwrap();
            let m = d.pointer_mut(parent).unwrap().as_object_mut().unwrap();
            assert!(m.remove(key).is_some(), "nothing at {}", ptr.as_ref());
        }
        d
    }

    const FIRST: &str = "/runs/0/results/0";

    #[test]
    fn call_arg_rewrite() {
        for (v, want) in [
            ("call-arg(123, 0).f", "call-arg(_, 0).f"),
            ("call-arg(7, -2)", "call-arg(_, -2)"),
            ("call-arg(0, 3).f.g", "call-arg(_, 3).f.g"),
            ("local(call-arg(1, 0))", "local(call-arg(1, 0))"),
            ("formal(1).x", "formal(1).x"),
            ("call-arg(_, 0)", "call-arg(_, 0)"),
            ("call-arg(1,0)", "call-arg(1,0)"),
            ("call-arg(1, x)", "call-arg(1, x)"),
            ("call-arg(1, -)", "call-arg(1, -)"),
            ("call-arg(1, 2", "call-arg(1, 2"),
        ] {
            assert_eq!(without_call_site(v), want, "{v}");
        }
    }

    /// Every kept field of `doc()` lands where the raw types say.
    #[test]
    fn reads_kept_fields() {
        use sarif_canon::types::RawLogicalRef::{Index, Inline};
        let mut atoms = Atoms::new();
        let runs = parse(&doc(), &mut atoms).unwrap();
        let a = |s: &str| atoms.get_id(s).unwrap_or_else(|| panic!("no atom {s}"));
        let none = Props {
            labels: None,
            vertex: None,
            vertices: None,
            source_fns: None,
            sink_fns: None,
            tainted_formals: None,
            import: None,
        };
        let no_region = Region {
            start_line: None,
            start_column: None,
            end_line: None,
            end_column: None,
            byte_offset: None,
            byte_length: None,
        };
        let logical_only = |r| RawLocation {
            physical: None,
            logical: vec![Index(r)],
        };
        let want = vec![RawRun {
            logical: vec![
                RawLogical { fqn: a("main") },
                RawLogical { fqn: a("sink_fn") },
            ],
            results: vec![
                RawResult {
                    rule: Some(a("C0001")),
                    kind: a("fail"),
                    level: a("error"),
                    locations: vec![RawLocation {
                        physical: Some(Physical {
                            base: Some(a("SRC0")),
                            uri: Some(a("a.c")),
                            region: Region {
                                start_line: Some(3),
                                start_column: Some(1),
                                end_line: Some(4),
                                end_column: Some(9),
                                byte_offset: Some(10),
                                byte_length: Some(4),
                            },
                            address: None,
                        }),
                        logical: vec![Index(0), Inline(a("inline_fqn"))],
                    }],
                    code_flows: vec![RawCodeFlow {
                        thread_flows: vec![RawThreadFlow {
                            steps: vec![
                                RawStep {
                                    order: Some(1),
                                    kinds: vec![a("taint")],
                                    location: Some(logical_only(0)),
                                    vertex: None,
                                    callee: None,
                                },
                                RawStep {
                                    order: Some(2),
                                    kinds: vec![a("call"), a("taint")],
                                    location: Some(RawLocation {
                                        physical: Some(Physical {
                                            base: None,
                                            uri: None,
                                            region: no_region.clone(),
                                            address: Some(Address {
                                                absolute: Some(4096),
                                                relative: Some(16),
                                                kind: Some(a("instruction")),
                                            }),
                                        }),
                                        logical: vec![Index(1)],
                                    }),
                                    vertex: None,
                                    callee: None,
                                },
                                RawStep {
                                    order: Some(3),
                                    kinds: vec![],
                                    location: None,
                                    vertex: None,
                                    callee: None,
                                },
                            ],
                        }],
                    }],
                    props: Props {
                        labels: Some(vec![a("L")]),
                        vertices: Some(vec![Keyed {
                            key: a("L"),
                            values: vec![a("call-arg(_, 0).f"), a("local(x)")],
                        }]),
                        source_fns: Some(vec![a("src")]),
                        sink_fns: Some(vec![a("snk")]),
                        import: Some(a("imp")),
                        ..none.clone()
                    },
                },
                RawResult {
                    rule: Some(a("C0003")),
                    kind: a("informational"),
                    level: a("none"),
                    locations: vec![logical_only(1)],
                    code_flows: vec![],
                    props: Props {
                        labels: Some(vec![a("L")]),
                        vertex: Some(a("call-arg(_, -1)")),
                        ..none.clone()
                    },
                },
                RawResult {
                    rule: Some(a("C0007")),
                    kind: a("informational"),
                    level: a("none"),
                    locations: vec![logical_only(0)],
                    code_flows: vec![],
                    props: Props {
                        tainted_formals: Some(vec![Keyed {
                            key: a("0"),
                            values: vec![a("L"), a("M")],
                        }]),
                        ..none.clone()
                    },
                },
                RawResult {
                    rule: None,
                    kind: a("fail"),
                    level: a("warning"),
                    locations: vec![],
                    code_flows: vec![],
                    props: none,
                },
            ],
        }];
        assert_eq!(runs, want);
        // No dropped string was interned.
        for s in [
            "Taint flow labelled 'L'",
            "here",
            "r",
            "f(x)",
            "member",
            "n",
            "call-arg(12, 0).f",
            "call-arg(99, -1)",
            "file:///a/src/",
            "/a/b",
            "ctadl",
            "0.1.7",
        ] {
            assert_eq!(atoms.get_id(s), None, "{s}");
        }
    }

    /// A change to any kept field changes the parse.
    #[test]
    fn kept_fields_matter() {
        let tf = format!("{FIRST}/codeFlows/0/threadFlows/0/locations");
        let phys = format!("{FIRST}/locations/0/physicalLocation");
        let props = format!("{FIRST}/properties");
        let edits: Vec<(String, Value)> = vec![
            (format!("{FIRST}/ruleId"), json!("C0002")),
            (format!("{FIRST}/kind"), json!("pass")),
            (format!("{FIRST}/level"), json!("note")),
            (format!("{phys}/artifactLocation/uri"), json!("b.c")),
            (format!("{phys}/artifactLocation/uriBaseId"), json!("SRC1")),
            (format!("{phys}/region/startLine"), json!(30)),
            (format!("{phys}/region/startColumn"), json!(2)),
            (format!("{phys}/region/endLine"), json!(40)),
            (format!("{phys}/region/endColumn"), json!(10)),
            (format!("{phys}/region/byteOffset"), json!(11)),
            (format!("{phys}/region/byteLength"), json!(5)),
            (
                format!("{FIRST}/locations/0/logicalLocations/0/index"),
                json!(1),
            ),
            (
                format!("{FIRST}/locations/0/logicalLocations/1/fullyQualifiedName"),
                json!("other"),
            ),
            (
                "/runs/0/logicalLocations/0/fullyQualifiedName".into(),
                json!("main2"),
            ),
            ("/runs/0/logicalLocations/1/name".into(), json!("sink_fn2")),
            (format!("{tf}/0/executionOrder"), json!(0)),
            (format!("{tf}/0/kinds/0"), json!("return")),
            (
                format!("{tf}/0/location/logicalLocations/0/index"),
                json!(1),
            ),
            (
                format!("{tf}/1/location/physicalLocation/address/absoluteAddress"),
                json!(4097),
            ),
            (
                format!("{tf}/1/location/physicalLocation/address/relativeAddress"),
                json!(17),
            ),
            (
                format!("{tf}/1/location/physicalLocation/address/kind"),
                json!("data"),
            ),
            (format!("{props}/taintLabels/0"), json!("M")),
            (format!("{props}/taintVertices/L/1"), json!("local(y)")),
            (
                format!("{props}/taintVertices/L/0"),
                json!("call-arg(12, 1).f"),
            ),
            (format!("{props}/taintVertices"), json!({"M": ["local(x)"]})),
            (format!("{props}/sourceFunctions/0"), json!("src2")),
            (format!("{props}/sinkFunctions/0"), json!("snk2")),
            (format!("{props}/import"), json!("imp2")),
            (
                "/runs/0/results/1/properties/taintVertex".into(),
                json!("call-arg(99, -2)"),
            ),
            (
                "/runs/0/results/2/properties/taintedFormals/0/1".into(),
                json!("N"),
            ),
            (
                "/runs/0/results/2/properties/taintedFormals".into(),
                json!({"1": ["L", "M"]}),
            ),
        ];
        let mut atoms = Atoms::new();
        let base = parse(&doc(), &mut atoms).unwrap();
        for (ptr, value) in edits {
            let edited = edit(&[(&ptr, value)]);
            assert_ne!(edited, doc(), "{ptr}: the edit changed nothing");
            assert_ne!(parse(&edited, &mut atoms).unwrap(), base, "{ptr}");
        }
    }

    #[test]
    fn drops_unread_fields() {
        let tf = format!("{FIRST}/codeFlows/0/threadFlows/0");
        let phys = format!("{FIRST}/locations/0/physicalLocation");
        let props = format!("{FIRST}/properties");
        let edits: Vec<(String, Value)> = vec![
            (format!("{FIRST}/message/text"), json!("changed")),
            (
                format!("{FIRST}/locations/0/message/text"),
                json!("changed"),
            ),
            (
                format!("{FIRST}/codeFlows/0/message/text"),
                json!("changed"),
            ),
            (format!("{tf}/message/text"), json!("changed")),
            (
                format!("{tf}/locations/0/location/message/text"),
                json!("changed"),
            ),
            (format!("{FIRST}/ruleIndex"), json!(7)),
            (format!("{phys}/artifactLocation/index"), json!(3)),
            (format!("{phys}/region/message/text"), json!("changed")),
            (format!("{phys}/region/snippet/text"), json!("changed")),
            (
                format!("{FIRST}/locations/0/logicalLocations/1/name"),
                json!("m"),
            ),
            ("/runs/0/logicalLocations/0/name".into(), json!("main2")),
            ("/runs/0/logicalLocations/0/kind".into(), json!("function")),
            (format!("{props}/fileSpanId"), json!(40)),
            (format!("{props}/funcId"), json!(50)),
            (format!("{props}/insnId"), json!(60)),
            (format!("{props}/sourceCallee"), json!("other")),
            (format!("{props}/sinkCallee"), json!("other")),
            (
                format!("{props}/taintVertices/L/0"),
                json!("call-arg(13, 0).f"),
            ),
            (
                "/runs/0/results/1/properties/taintVertex".into(),
                json!("call-arg(1, -1)"),
            ),
            (
                "/runs/0/invocations/0/commandLine".into(),
                json!("ctadl query /c"),
            ),
            (
                "/runs/0/invocations/0/startTimeUtc".into(),
                json!("2026-02-02T00:00:00Z"),
            ),
            ("/runs/0/tool/driver/version".into(), json!("0.1.8")),
            (
                "/runs/0/originalUriBaseIds/SRC0/uri".into(),
                json!("file:///c/src/"),
            ),
            ("/runs/0/properties/parquet_dir".into(), json!("/c")),
            ("/properties/parquet_dirs".into(), json!(["/c"])),
        ];
        let mut atoms = Atoms::new();
        let base = parse(&doc(), &mut atoms).unwrap();
        let all: Vec<(&str, Value)> = edits.iter().map(|(p, v)| (p.as_str(), v.clone())).collect();
        for edit1 in &all {
            let edited = edit(std::slice::from_ref(edit1));
            assert_ne!(edited, doc(), "{}: the edit changed nothing", edit1.0);
            assert_eq!(parse(&edited, &mut atoms).unwrap(), base, "{}", edit1.0);
        }
        assert_eq!(parse(&edit(&all), &mut atoms).unwrap(), base);
        // Removing what is dropped is no change either.
        let removed = remove(&[
            format!("{FIRST}/ruleIndex"),
            format!("{props}/sourceCallee"),
            format!("{props}/fileSpanId"),
            "/runs/0/invocations".into(),
            "/runs/0/originalUriBaseIds".into(),
            "/runs/0/properties".into(),
        ]);
        assert_eq!(parse(&removed, &mut atoms).unwrap(), base);
    }

    #[test]
    fn kind_level_defaults() {
        let mut atoms = Atoms::new();
        let absent = parse(
            &remove(&[&format!("{FIRST}/kind"), &format!("{FIRST}/level")]),
            &mut atoms,
        );
        let explicit = parse(
            &edit(&[
                (&format!("{FIRST}/kind"), json!("fail")),
                (&format!("{FIRST}/level"), json!("warning")),
            ]),
            &mut atoms,
        );
        assert_eq!(absent.unwrap(), explicit.unwrap());
        // `doc()`'s level is `error`, not the default.
        let no_level = parse(&remove(&[&format!("{FIRST}/level")]), &mut atoms).unwrap();
        assert_ne!(no_level, parse(&doc(), &mut atoms).unwrap());
        let pass = edit(&[(&format!("{FIRST}/kind"), json!("pass"))]);
        let no_kind = remove(&[&format!("{FIRST}/kind")]);
        assert_ne!(
            parse(&pass, &mut atoms).unwrap(),
            parse(&no_kind, &mut atoms).unwrap()
        );
    }

    #[test]
    fn absent_and_empty_differ() {
        let mut atoms = Atoms::new();
        let empty = parse(
            &edit(&[(&format!("{FIRST}/properties/taintLabels"), json!([]))]),
            &mut atoms,
        );
        let absent = parse(
            &remove(&[&format!("{FIRST}/properties/taintLabels")]),
            &mut atoms,
        );
        assert_ne!(empty.unwrap(), absent.unwrap());
    }

    #[test]
    fn bad_inputs_error() {
        let mut atoms = Atoms::new();
        assert!(matches!(
            parse_runs(b"{not json", &mut atoms),
            Err(Error::Sarif(_))
        ));
        assert!(matches!(
            parse(
                &edit(&[(&format!("{FIRST}/kind"), json!("bogus"))]),
                &mut atoms
            ),
            Err(Error::Sarif(_))
        ));
        for (key, value) in [
            ("taintLabels", json!("L")),
            ("taintLabels", json!([1])),
            ("taintVertex", json!(["x"])),
            ("taintVertices", json!(["x"])),
            ("taintVertices", json!({"L": "x"})),
            ("sourceFunctions", json!({})),
            ("sinkFunctions", json!(null)),
            ("taintedFormals", json!({"0": [0]})),
            ("import", json!(3)),
        ] {
            let bad = edit(&[(&format!("{FIRST}/properties/{key}"), value)]);
            match parse(&bad, &mut atoms) {
                Err(Error::Property { key: k, .. }) => assert_eq!(k, key),
                other => panic!("{key}: {other:?}"),
            }
        }
        assert!(matches!(
            parse(
                &edit(&[(
                    &format!("{FIRST}/locations/0/logicalLocations/0/index"),
                    json!(-1)
                )]),
                &mut atoms
            ),
            Err(Error::NegativeIndex(-1))
        ));
        assert!(matches!(
            parse(&remove(&["/runs/0/logicalLocations/1/name"]), &mut atoms),
            Err(Error::UnnamedLogical)
        ));
        assert!(matches!(
            parse(
                &edit(&[(
                    &format!("{FIRST}/locations/0/logicalLocations/1"),
                    json!({})
                )]),
                &mut atoms
            ),
            Err(Error::UnnamedLogical)
        ));
        let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("no-such-file.sarif");
        assert!(matches!(
            read_runs(&missing, &mut atoms),
            Err(Error::Read(_))
        ));
    }

    #[test]
    fn atoms_shared() {
        let mut atoms = Atoms::new();
        let a = parse(&doc(), &mut atoms).unwrap();
        // B interns a string A never had before the strings both have.
        let b = parse(
            &edit(&[(
                "/runs/0/logicalLocations/0/fullyQualifiedName",
                json!("fresh"),
            )]),
            &mut atoms,
        )
        .unwrap();
        assert_eq!(a[0].results, b[0].results);
        assert_eq!(a[0].logical[1], b[0].logical[1]);
        assert_ne!(a[0].logical[0], b[0].logical[0]);
        assert_eq!(b[0].logical[0].fqn, atoms.get_id("fresh").unwrap());
    }

    #[test]
    fn reads_file_and_pools_runs() {
        let mut two_runs = doc();
        let run = two_runs["runs"][0].clone();
        two_runs["runs"].as_array_mut().unwrap().push(run);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sarif");
        std::fs::write(&path, serde_json::to_vec(&two_runs).unwrap()).unwrap();
        let mut atoms = Atoms::new();
        let runs = read_runs(&path, &mut atoms).unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0], runs[1]);
        assert_eq!(runs[0], parse(&doc(), &mut atoms).unwrap()[0]);
    }
}
