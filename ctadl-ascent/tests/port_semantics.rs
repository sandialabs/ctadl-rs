//! The Lua half of the model-port semantics matrix.
//!
//! The flowy half lives in `tests/tnt/port_*.tnt`, where it belongs: flowy needs no toolchain,
//! gets no default models of its own, and its `where summaries [...]` clause asserts on the
//! index summary relation directly. Those six fixtures pin the shapes flowy can spell -- bare,
//! `.symbol` on each side, `.[<numeric>]`, `.deref`, and the two-segment case.
//!
//! Lua is here for two reasons. It is a real frontend rather than a test DSL, and it is the only
//! one available in-tree that can spell the shape flowy *cannot*: flowy's identifier production
//! is `[A-Za-z_][A-Za-z0-9_]*`, a strict subset of the canonical symbol production, so it can
//! never write a field name that begins with `[`. That name is exactly what the lua, dex, jvm and
//! tree-sitter C frontends emit for a container element (`[_elem_]`, `[]`), and a port matches it
//! only when the bracket is escaped: `.\[_elem_]`, JSON `"\\[_elem_]"`.
//!
//! That distinction is the substance of the whole grammar change. Unescaped, `.[1]` is a real
//! `Offset(1)` and matches what pcode emits; escaped, `.\[1]` is a `Symbol` named `[1]` and
//! matches what Lua emits for `t[1]`. The two ports were indistinguishable before, and getting
//! them backwards silently matches nothing rather than failing.
//!
//! Every case probes at the level its port *predicts* under prefix substitution rather than at
//! one fixed depth -- probing `u.f` for all six rows is what made the original semantics matrix
//! read as four failures out of six when it was four correct answers.
//!
//! The remaining matrix row -- an *unescaped* `.[_elem_]`, which must be a hard load error rather
//! than a silent `Symbol("[_elem_]")` -- needs no program at all and is pinned in
//! `json_error_handling.rs`.
//!
//! Not covered: pcode. It needs Ghidra, which is not available here. `native-index.jsonl` is
//! built entirely on `.deref`, which `tests/tnt/port_in_deref.tnt` pins as the ordinary symbol
//! segment it is, and on `.[<numeric>]`, pinned by `tests/tnt/port_in_offset.tnt`.

use ctadl_ascent::cli;
use ctadl_ascent::codegen::CallResolutionStrategy;
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, init_store_path};
use ctadl_ascent::query_engine::formatter::SarifProfile;
use std::path::{Path, PathBuf};
use std::sync::Once;

static INIT: Once = Once::new();

/// One store for the whole test binary; each case gets its own import/project name.
fn store_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("temp store"));
    INIT.call_once(|| {
        init_store_path(Some(dir.path())).expect("init store");
    });
    dir.path()
}

/// The program every case runs, with the store and the probe filled in.
///
/// `id` is defined and propagates nothing -- it returns a constant -- so any flow from `handler`'s
/// parameter to its return is the model on `id` and nothing else. The source is `handler`'s
/// argument 0 and the sink is its bare return, so the *probe expression* is what says at which
/// level the case is looking.
fn program(store: &str, probe: &str) -> String {
    format!(
        r#"
local function id(v)
  local w = "nothing"
  return w
end

local function handler(x, k)
  local t = {{}}
  {store}
  local u = id(t)
  local p = {probe}
  return p
end

return handler
"#
    )
}

/// Imports the program, indexes it with `index_model` (and no defaults, so the model under test
/// is the only summary in play), then queries with a source on `handler`'s argument 0 and a sink
/// on its bare return. Returns whether a source-to-sink flow was reported.
fn flows(case: &str, store: &str, index_model: &str, probe: &str) -> bool {
    let dir = store_dir();
    let src = dir.join(format!("{case}.lua"));
    std::fs::write(&src, program(store, probe)).expect("writing lua source");

    let index_models = dir.join(format!("{case}-index.jsonl"));
    std::fs::write(&index_models, index_model).expect("writing index model");

    let query_models = dir.join(format!("{case}-query.json"));
    std::fs::write(
        &query_models,
        r#"{"model_generators":[
{"find":"methods","where":[{"constraint":"signature_match","name":"handler"}],
 "model":{"sources":[{"port":"Argument(0)","kind":"K"}]}},
{"find":"methods","where":[{"constraint":"signature_match","name":"handler"}],
 "model":{"sinks":[{"port":"Return","kind":"K"}]}}
]}"#,
    )
    .expect("writing query model");

    let import =
        ArtifactImport::try_create(case, ArtifactLanguage::Lua, &src).expect("import args");
    cli::import(&import).expect("importing lua");
    let project = AnalysisProject::try_create(case, &[case.to_string()]).expect("project");
    cli::index(
        &project,
        &[],
        &[index_models],
        // Defaults suppressed. `lua-index.jsonl` models none of the names this program calls,
        // but a case about what is *not* modeled should not also depend on that staying true.
        true,
        // No Java or native import here, so the JNI bridge has nothing to do either way.
        false,
        CallResolutionStrategy::Mixed,
        true,
        true,
        None,
    )
    .expect("indexing");

    let sarif: PathBuf = dir.join(format!("{case}.sarif"));
    cli::query(
        &project,
        &[query_models],
        false,
        &sarif,
        SarifProfile::Human,
        None,
    )
    .expect("querying");

    let text = std::fs::read_to_string(&sarif).expect("reading sarif");
    let value: serde_json::Value = serde_json::from_str(&text).expect("parsing sarif");
    // `kind`, not mere presence: a run that found nothing still emits a `C0001.tainted-path`
    // result, as `kind: "open"` with the message that CTADL does not prove the absence of a
    // flow. Only a real flow is a `fail`.
    value["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|run| run["results"].as_array())
        .flatten()
        .any(|r| r["ruleId"] == "C0001.tainted-path" && r["kind"] == "fail")
}

fn model(input: &str, output: &str) -> String {
    format!(
        r#"{{"find":"methods","where":[{{"constraint":"signature_match","name":"id"}}],"model":{{"propagation":[{{"input":"{input}","output":"{output}"}}]}}}}"#
    )
}

/// The control: with no propagation on `id` at all, nothing reaches the return. Every assertion
/// below is a difference from this one.
#[test]
fn without_a_model_nothing_flows() {
    assert!(!flows("port_control", "t.f = x", "", "u.f"));
}

/// A bare port is a pure level-preserving copy: `Argument(0) -> Return` maps `t.X` to `u.X` for
/// every `X`, so taint stored at `t.f` is readable at `u.f` -- no deeper, and no shallower.
///
/// Only the "no deeper" half is asserted here. A model **sink port materializes over the paths
/// reachable at its vertex**, so a sink written `Return` seeds `formal(-1)` *and* `formal(-1).f`;
/// reading a whole object therefore observes taint in its fields, and no bare-port sink can say
/// "the object itself is clean". flowy's declared endpoints are exact vertices instead, which is
/// why `tests/tnt/port_bare.tnt` asserts the shallow half and this does not. The difference is
/// worth knowing before writing a model whose sink is meant to be precise.
#[test]
fn a_bare_port_preserves_the_suffix() {
    let m = model("Argument(0)", "Return");
    assert!(
        flows("port_lua_bare", "t.f = x", &m, "u.f"),
        "taint at t.f is readable at u.f"
    );
    assert!(
        !flows("port_lua_bare_deep", "t.f = x", &m, "u.f.f"),
        "and no deeper -- the port adds nothing"
    );
}

/// A named field on the INPUT side unwraps a level: `Argument(0).f -> Return` maps `t.f.X` to
/// `u.X`, so taint at `t.f` lands on BARE `u`, one level *above* where a first reading expects
/// it. This is the row a fixed `u.f` probe scores 0 on while the model is behaving correctly.
#[test]
fn an_input_field_port_unwraps_a_level() {
    let m = model("Argument(0).f", "Return");
    assert!(
        flows("port_lua_in_field", "t.f = x", &m, "u"),
        "taint at t.f lands on the bare return value"
    );
    assert!(
        !flows("port_lua_in_field_deep", "t.f = x", &m, "u.f"),
        "and not one level down -- the port is a level shift, not a filter"
    );
}

/// The mirror image: a named field on the OUTPUT side adds a level. `Argument(0) -> Return.f`
/// maps `t.X` to `u.f.X`, so taint at `t.f` lands at `u.f.f`.
#[test]
fn an_output_field_port_adds_a_level() {
    let m = model("Argument(0)", "Return.f");
    assert!(
        flows("port_lua_out_field", "t.f = x", &m, "u.f.f"),
        "taint at t.f lands at u.f.f"
    );
    assert!(
        !flows("port_lua_out_field_shallow", "t.f = x", &m, "u.f"),
        "and not at u.f"
    );
}

/// The escaped bracketed port is the one the Lua frontend actually emits: `t[k]` with a dynamic
/// key stores at the `Symbol` named `[_elem_]`, and only `.\[_elem_]` names it.
#[test]
fn escaped_element_port_matches_what_lua_emits() {
    assert!(
        flows(
            "port_escaped_elem",
            "t[k] = x",
            &model(r"Argument(0).\\[_elem_]", "Return"),
            "u"
        ),
        r"a port spelled .\[_elem_] must match the Symbol named [_elem_] the frontend emits"
    );
}

/// The row that could not be written at all before the canonical grammar, when both spellings
/// were the same `Symbol`. `t[1]` in Lua source is the SYMBOL named `[1]`; the unescaped port
/// `.[1]` asks for `Offset(1)`, which is a different thing and which Lua never produces.
#[test]
fn an_unescaped_numeric_port_is_an_offset_and_lua_has_none() {
    assert!(
        flows(
            "port_escaped_one",
            "t[1] = x",
            &model(r"Argument(0).\\[1]", "Return"),
            "u"
        ),
        r"a port spelled .\[1] must match the Symbol named [1] the frontend emits for t[1]"
    );
    assert!(
        !flows(
            "port_offset_one",
            "t[1] = x",
            &model("Argument(0).[1]", "Return"),
            "u"
        ),
        "an offset port must not match a Lua symbol that merely prints the same way"
    );
}
