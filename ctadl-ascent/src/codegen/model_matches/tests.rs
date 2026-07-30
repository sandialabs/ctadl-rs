//! Emission tests for phase 2: the exact rows a bridge and a propagation produce.
//!
//! `languages/jni/tests.rs` is the model for this layer, and the JNI-shape case below asserts
//! the degenerate collapse leaves that pass's rows byte-identical.

use std::path::PathBuf;

use super::*;
use crate::facts::{FlowVariableKind, Function, InsnSiteId};
use crate::models::matches::{BridgeSideMatches, ModelPort};
use crate::models::spec::{BridgePort, Direction, ProgramScope, SideSpec};
use crate::models::{FormalIndexTypeTag, ProgramModelMatches};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Interns `functions` in order and gives each `arity` declared formals, mimicking what codegen
/// leaves behind.
fn fact_base(functions: &[(&str, i16)]) -> (IndexFacts, IndexSourceInfo) {
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    for (name, arity) in functions {
        let id = source_info
            .sites
            .get_or_add_function(Function((*name).into()));
        for i in 0..*arity {
            facts.formal_param.push((
                id,
                FlowVariable::formal_index(FormalIndex::new(i)),
                FormalType::ByRef,
            ));
        }
    }
    (facts, source_info)
}

fn id_of(source_info: &IndexSourceInfo, name: &str) -> FunctionId {
    source_info
        .sites
        .get_function_id(Function(name.into()))
        .unwrap_or_else(|| panic!("{name} was not interned"))
}

fn port(index: i16, path: &str) -> BridgePort {
    BridgePort {
        index: FormalIndex::new(index),
        path: if path.is_empty() {
            facts::Path::empty()
        } else {
            crate::models::spec::parse_declared_access_path(path, 0).expect("path")
        },
    }
}

fn pair(from: BridgePort, to: BridgePort, direction: Direction) -> PortPair {
    PortPair {
        from,
        to,
        direction,
    }
}

/// A spec with the given port map, side scopes unconstrained.
fn spec_with(ports: Vec<PortPair>) -> BridgeSpec {
    let side = || SideSpec {
        scope: ProgramScope::default(),
        where_: Vec::new(),
        on_unmatched: Severity::Warn,
    };
    BridgeSpec {
        source: PathBuf::from("test.jsonl"),
        index: 0,
        from: side(),
        to: side(),
        ports_given: !ports.is_empty(),
        ports,
        on_ambiguous: Severity::Ignore,
    }
}

/// The `(callee index, caller formal index)` pairs a site's `actual_param` rows carry, for
/// vertices that are plain formals.
fn actual_formals(facts: &IndexFacts, site: PackedInsnSiteId) -> Vec<(i16, i16)> {
    let mut rows: Vec<(i16, i16)> = facts
        .actual_param
        .iter()
        .filter(|(s, _, _)| *s == site)
        .filter_map(|(_, index, FlowVertex(var, path))| {
            var.as_formal().map(|f| {
                assert!(path.is_empty(), "a collapsed bridge port carries no path");
                (**index, *f)
            })
        })
        .collect();
    rows.sort();
    rows
}

/// The temporaries passed at a site, as `(callee index, variable)`.
fn actual_temps(facts: &IndexFacts, site: PackedInsnSiteId) -> Vec<(i16, FlowVariable)> {
    let mut rows: Vec<(i16, FlowVariable)> = facts
        .actual_param
        .iter()
        .filter(|(s, _, _)| *s == site)
        .filter(|(_, _, FlowVertex(var, _))| var.as_formal().is_none())
        .map(|(_, index, FlowVertex(var, path))| {
            assert!(path.is_empty(), "the temporary is passed whole");
            (**index, *var)
        })
        .collect();
    rows.sort_by_key(|(i, _)| *i);
    rows
}

fn formals_of(facts: &IndexFacts, func: FunctionId) -> Vec<i16> {
    let mut v: Vec<i16> = facts
        .formal_param
        .iter()
        .filter(|(f, _, _)| *f == func)
        .map(|(_, var, _)| *var.as_formal().unwrap())
        .collect();
    v.sort();
    v.dedup();
    v
}

fn run(
    spec: &BridgeSpec,
    from: &[&str],
    to: &[&str],
    facts: &mut IndexFacts,
    source_info: &mut IndexSourceInfo,
) -> ModelCodegenReport {
    let mut matches = ProgramModelMatches::default();
    matches.bridges.prepare(std::slice::from_ref(spec));
    let side: &mut BridgeSideMatches = matches.bridges.side_mut(0);
    side.from.extend(from.iter().map(|n| facts::Str::from(*n)));
    side.to.extend(to.iter().map(|n| facts::Str::from(*n)));
    codegen_model_matches(&matches, std::slice::from_ref(spec), facts, source_info)
        .expect("emission")
}

// ---------------------------------------------------------------------------
// The shape of one bridge
// ---------------------------------------------------------------------------

/// The JNI shape: every port is an empty-path formal flowing both ways, so every one collapses
/// to a direct `actual_param` row -- byte-identical to what `jni::emit_bridge` writes.
#[test]
fn the_degenerate_case_collapses_to_direct_actual_params() {
    let (mut facts, mut source_info) = fact_base(&[("stub", 0), ("impl", 3)]);
    // A site that already exists in the caller, so "the bridge mints a fresh one" is testable.
    let existing = source_info.add_insn_site(id_of(&source_info, "stub"));

    let spec = spec_with(vec![
        pair(port(0, ""), port(1, ""), Direction::Both),
        pair(port(1, ""), port(2, ""), Direction::Both),
        pair(port(RETURN_INDEX, ""), port(RETURN_INDEX, ""), Direction::Both),
    ]);
    run(&spec, &["stub"], &["impl"], &mut facts, &mut source_info);

    let stub = id_of(&source_info, "stub");
    let imp = id_of(&source_info, "impl");
    assert_eq!(facts.call.len(), 1);
    let (site, target) = facts.call[0];
    assert_eq!(target, imp);

    let unpacked = InsnSiteId::try_from(site).unwrap();
    assert_eq!(unpacked.func_id, stub, "the site lives in the caller");
    assert_ne!(unpacked.insn_id, existing.insn_id, "the site is fresh");

    // Globals ride along unconditionally, and nothing routes through a temporary.
    assert_eq!(
        actual_formals(&facts, site),
        vec![
            (GLOBALS_INDEX, GLOBALS_INDEX),
            (RETURN_INDEX, RETURN_INDEX),
            (1, 0),
            (2, 1),
        ]
    );
    assert!(actual_temps(&facts, site).is_empty());
    assert!(
        facts.assign.is_empty(),
        "a fully collapsed bridge emits no assign rows"
    );

    // Formals on *both* sides, for every mapped port.
    assert_eq!(formals_of(&facts, stub), vec![GLOBALS_INDEX, RETURN_INDEX, 0, 1]);
    assert_eq!(
        formals_of(&facts, imp),
        vec![GLOBALS_INDEX, RETURN_INDEX, 0, 1, 2]
    );

    // Registration is the engine's job: `program_paths` is seeded from every assign endpoint
    // and every actual_param vertex.
    assert!(facts.paths.is_empty(), "a bridge pushes no facts.paths rows");
}

/// The Lua shape: three ports on one callee parameter at different sub-paths. One temporary,
/// three sub-paths -- which is what keeps the caller's ports from aliasing each other.
#[test]
fn ports_sharing_a_callee_index_share_one_temporary_and_do_not_alias() {
    let (mut facts, mut source_info) = fact_base(&[("mylib.add", 0), ("l_add", 1)]);
    let spec = spec_with(vec![
        pair(port(0, ""), port(0, ".stack.[1]"), Direction::In),
        pair(port(1, ""), port(0, ".stack.[2]"), Direction::In),
        pair(port(RETURN_INDEX, ""), port(0, ".stack.[-1]"), Direction::Out),
    ]);
    run(&spec, &["mylib.add"], &["l_add"], &mut facts, &mut source_info);

    let site = facts.call[0].0;
    // One temporary for callee index 0, passed whole; globals still collapse.
    let temps = actual_temps(&facts, site);
    assert_eq!(temps.len(), 1, "one temporary per distinct callee index");
    assert_eq!(temps[0].0, 0);
    let t = temps[0].1;
    assert!(
        matches!(t.kind(), FlowVariableKind::Local(_)),
        "the temporary is a local, not a formal"
    );
    assert_eq!(
        actual_formals(&facts, site),
        vec![(GLOBALS_INDEX, GLOBALS_INDEX)],
        "only the globals pair collapses here"
    );

    // The three ports land at their own sub-paths of the one temporary. `direction: in` emits
    // one assign and not its converse.
    let rows: Vec<(String, String)> = facts
        .assign
        .iter()
        .map(|(_, dst, src)| (vertex_str(dst), vertex_str(src)))
        .collect();
    let t_name = local_name(t);
    assert!(rows.contains(&(
        format!("{t_name}.stack.[1]"),
        "formal(0)".to_string()
    )));
    assert!(rows.contains(&(
        format!("{t_name}.stack.[2]"),
        "formal(1)".to_string()
    )));
    // ... and the out-direction port is the converse, writing the caller's return formal.
    assert!(rows.contains(&(
        format!("formal({RETURN_INDEX})"),
        format!("{t_name}.stack.[-1]")
    )));
    assert_eq!(rows.len(), 3, "one assign per port, not two: {rows:?}");

    // The negative that matters: taint on Argument(0) must not reach Argument(1). Nothing
    // connects the two caller formals directly, and their temporary paths are siblings.
    assert!(
        !rows
            .iter()
            .any(|(dst, src)| dst.starts_with("formal(1)") && src.starts_with("formal(0)")),
        "the caller's ports must not alias: {rows:?}"
    );
}

/// Cross-product pairing puts several bridge sites in one caller. Temporaries are keyed on
/// `(site, index)` so those sites' parameters do not merge.
#[test]
fn two_bridge_sites_in_one_caller_get_distinct_temporaries() {
    let (mut facts, mut source_info) = fact_base(&[("a", 1), ("b1", 1), ("b2", 1)]);
    let spec = spec_with(vec![pair(port(0, ""), port(0, ".slot"), Direction::In)]);
    run(&spec, &["a"], &["b1", "b2"], &mut facts, &mut source_info);

    assert_eq!(facts.call.len(), 2, "one site per pair");
    let sites: Vec<_> = facts.call.iter().map(|(s, _)| *s).collect();
    assert_ne!(sites[0], sites[1], "each pair mints its own site");

    let t0 = actual_temps(&facts, sites[0]);
    let t1 = actual_temps(&facts, sites[1]);
    assert_eq!(t0.len(), 1);
    assert_eq!(t1.len(), 1);
    assert_ne!(
        t0[0].1, t1[0].1,
        "temporaries are keyed on (site, index), not on the index alone"
    );
}

/// `direction` is exactly which of the two assign rows get pushed.
#[test]
fn direction_selects_which_assign_rows_exist() {
    for (direction, want) in [
        (Direction::In, 1),
        (Direction::Out, 1),
        (Direction::Both, 2),
    ] {
        let (mut facts, mut source_info) = fact_base(&[("a", 1), ("b", 1)]);
        let spec = spec_with(vec![pair(port(0, ""), port(0, ".f"), direction)]);
        run(&spec, &["a"], &["b"], &mut facts, &mut source_info);
        assert_eq!(
            facts.assign.len(),
            want,
            "direction {direction:?} should emit {want} assign row(s)"
        );
    }
}

/// `assign` is keyed on the packed instruction site, not on the function id. (The
/// function-keyed shape is the persisted parquet form.)
#[test]
fn assign_rows_are_keyed_on_the_bridge_site() {
    let (mut facts, mut source_info) = fact_base(&[("a", 1), ("b", 1)]);
    let spec = spec_with(vec![pair(port(0, ""), port(0, ".f"), Direction::Both)]);
    run(&spec, &["a"], &["b"], &mut facts, &mut source_info);
    let site = facts.call[0].0;
    for (s, _, _) in &facts.assign {
        assert_eq!(*s, site);
        assert_eq!(site_function(*s), id_of(&source_info, "a"));
    }
}

/// The port map is the more authoritative statement of the callee's ABI, so a port past the
/// callee's recovered arity still gets its `formal_param` row -- and a warning, never a dropped
/// fact.
#[test]
fn ports_past_the_callee_arity_are_still_emitted() {
    // The callee declares one parameter; the map names three.
    let (mut facts, mut source_info) = fact_base(&[("a", 3), ("b", 1)]);
    let spec = spec_with(vec![
        pair(port(0, ""), port(0, ""), Direction::Both),
        pair(port(1, ""), port(1, ""), Direction::Both),
        pair(port(2, ""), port(2, ""), Direction::Both),
    ]);
    run(&spec, &["a"], &["b"], &mut facts, &mut source_info);
    let b = id_of(&source_info, "b");
    assert_eq!(
        formals_of(&facts, b),
        vec![GLOBALS_INDEX, 0, 1, 2],
        "the model's parameters are asserted whatever the frontend recovered"
    );
}

/// A callee index carrying a *pathful* port keeps its temporary even when another port on the
/// same index would have collapsed. Collapsing one of them would bind the call-argument
/// pseudo-variable whole and re-alias exactly what the temporary exists to keep apart.
#[test]
fn a_shared_callee_index_never_half_collapses() {
    let (mut facts, mut source_info) = fact_base(&[("a", 2), ("b", 1)]);
    let spec = spec_with(vec![
        pair(port(0, ""), port(0, ""), Direction::Both),
        pair(port(1, ""), port(0, ".f"), Direction::Both),
    ]);
    run(&spec, &["a"], &["b"], &mut facts, &mut source_info);
    let site = facts.call[0].0;
    assert_eq!(
        actual_formals(&facts, site),
        vec![(GLOBALS_INDEX, GLOBALS_INDEX)],
        "only globals collapse; callee index 0 routes through its temporary"
    );
    assert_eq!(actual_temps(&facts, site).len(), 1);
}

// ---------------------------------------------------------------------------
// Reporting semantics
// ---------------------------------------------------------------------------

fn classify_only(spec: &BridgeSpec, from: &[&str], to: &[&str]) -> Result<(), Error> {
    let mut side = BridgeSideMatches::default();
    side.from.extend(from.iter().map(|n| facts::Str::from(*n)));
    side.to.extend(to.iter().map(|n| facts::Str::from(*n)));
    classify(spec, &side)
}

#[test]
fn an_empty_from_side_suppresses_the_to_side_report() {
    let mut spec = spec_with(vec![]);
    spec.from.on_unmatched = Severity::Ignore;
    // Side B is empty too, but "if the from side doesn't match anything, the to side isn't even
    // attempted" -- so the `error` setting on the to side must not fire.
    spec.to.on_unmatched = Severity::Error;
    assert!(classify_only(&spec, &[], &[]).is_ok());
}

#[test]
fn a_matched_from_with_an_empty_to_reports_the_to_side() {
    let mut spec = spec_with(vec![]);
    spec.to.on_unmatched = Severity::Error;
    let err = classify_only(&spec, &["a"], &[]).expect_err("should report");
    assert!(format!("{err}").contains("'to' side matched none"), "{err}");
}

#[test]
fn an_empty_from_side_reports_under_its_own_setting() {
    let mut spec = spec_with(vec![]);
    spec.from.on_unmatched = Severity::Error;
    let err = classify_only(&spec, &[], &["b"]).expect_err("should report");
    assert!(format!("{err}").contains("'from' side matched no function"), "{err}");
}

#[test]
fn an_ambiguous_pairing_reports_with_counts_and_provenance() {
    let mut spec = spec_with(vec![]);
    spec.on_ambiguous = Severity::Error;
    for (from, to) in [
        (vec!["a1", "a2"], vec!["b"]),
        (vec!["a"], vec!["b1", "b2"]),
        (vec!["a1", "a2"], vec!["b1", "b2", "b3"]),
    ] {
        let err = classify_only(&spec, &from, &to).expect_err("should report");
        let msg = format!("{err}");
        assert!(msg.contains("test.jsonl:0"), "provenance: {msg}");
        assert!(
            msg.contains(&format!("{} pair(s)", from.len() * to.len())),
            "counts: {msg}"
        );
    }
    // Singleton x singleton is not ambiguous.
    assert!(classify_only(&spec, &["a"], &["b"]).is_ok());
}

#[test]
fn a_unique_pairing_under_warn_is_silent_and_still_emits() {
    let (mut facts, mut source_info) = fact_base(&[("a", 1), ("b", 1)]);
    let mut spec = spec_with(vec![pair(port(0, ""), port(0, ""), Direction::Both)]);
    spec.on_ambiguous = Severity::Warn;
    let report = run(&spec, &["a"], &["b"], &mut facts, &mut source_info);
    assert_eq!(report.bridges.len(), 1);
    assert_eq!(report.bridges[0].pairs, 1);
    assert_eq!(report.bridges[0].from_matched, 1);
    assert_eq!(report.bridges[0].to_matched, 1);
}

// ---------------------------------------------------------------------------
// Propagations
// ---------------------------------------------------------------------------

fn model_port(tag: FormalIndexTypeTag, index: Option<i16>) -> ModelPort {
    ModelPort {
        tag,
        index,
        path: facts::Path::empty(),
    }
}

/// `Argument(*)` expands over `compute_arg_arity`, which takes the max over actual call sites.
/// Running phase 2 after every import is what lets a function whose call sites span imports
/// expand fully.
#[test]
fn any_argument_expands_over_call_sites_from_every_import() {
    let (mut facts, mut source_info) = fact_base(&[("caller_a", 0), ("caller_b", 0), ("sprintf", 1)]);
    let sprintf = id_of(&source_info, "sprintf");
    // Import 1 calls sprintf with 2 arguments; import 2 calls it with 4.
    for (caller, argc) in [("caller_a", 2), ("caller_b", 4)] {
        let id = id_of(&source_info, caller);
        let site = source_info.add_insn_site(id);
        let site: PackedInsnSiteId = site.try_into().unwrap();
        facts.call.push((site, sprintf));
        for i in 0..argc {
            facts.actual_param.push((
                site,
                FormalIndex::new(i),
                FlowVertex(
                    FlowVariable::local(facts::Str::from(format!("x{i}").as_str())),
                    facts::Path::empty(),
                ),
            ));
        }
    }

    let mut matches = ProgramModelMatches::default();
    matches.propagations.push(PropagationMatch {
        function: facts::Str::from("sprintf"),
        dst: model_port(FormalIndexTypeTag::Index, Some(0)),
        src: model_port(FormalIndexTypeTag::AnyArgument, None),
    });
    codegen_model_matches(&matches, &[], &mut facts, &mut source_info).expect("codegen");

    let srcs: Vec<i16> = facts
        .summary
        .iter()
        .filter(|(f, ..)| *f == sprintf)
        .map(|(_, _, _, src, _)| **src)
        .collect();
    // 0..4 from the widest call site, minus `dst == src` (index 0).
    assert_eq!(srcs, vec![1, 2, 3], "got {srcs:?}");
}

#[test]
fn a_propagation_pushes_formals_for_both_ports_and_skips_dst_eq_src() {
    let (mut facts, mut source_info) = fact_base(&[("f", 0)]);
    let f = id_of(&source_info, "f");
    let mut matches = ProgramModelMatches::default();
    matches.propagations.push(PropagationMatch {
        function: facts::Str::from("f"),
        dst: model_port(FormalIndexTypeTag::Return, None),
        src: model_port(FormalIndexTypeTag::Index, Some(1)),
    });
    // A self-edge, which must produce formals but no summary row.
    matches.propagations.push(PropagationMatch {
        function: facts::Str::from("f"),
        dst: model_port(FormalIndexTypeTag::Index, Some(2)),
        src: model_port(FormalIndexTypeTag::Index, Some(2)),
    });
    codegen_model_matches(&matches, &[], &mut facts, &mut source_info).expect("codegen");

    assert_eq!(facts.summary.len(), 1);
    assert_eq!(formals_of(&facts, f), vec![RETURN_INDEX, 1, 2]);
}

/// Model paths must seed `model_paths` -- via `facts.summary` -- rather than `facts.paths`,
/// which is the program-path bucket. Folding them into the wrong bucket turns the one-level
/// model x program concatenation into a program x program self-join.
#[test]
fn propagation_paths_do_not_land_in_facts_paths() {
    let (mut facts, mut source_info) = fact_base(&[("f", 0)]);
    let mut matches = ProgramModelMatches::default();
    matches.propagations.push(PropagationMatch {
        function: facts::Str::from("f"),
        dst: ModelPort {
            tag: FormalIndexTypeTag::Return,
            index: None,
            path: crate::models::spec::parse_declared_access_path(".deref", 0).unwrap(),
        },
        src: model_port(FormalIndexTypeTag::Index, Some(0)),
    });
    codegen_model_matches(&matches, &[], &mut facts, &mut source_info).expect("codegen");
    assert_eq!(facts.summary.len(), 1);
    assert!(facts.paths.is_empty());
}

/// The declared-access-path registry is the one thing that *does* push `facts.paths` rows.
#[test]
fn declared_access_paths_reach_the_initial_indexer_paths() {
    let (mut facts, mut source_info) = fact_base(&[("f", 0)]);
    let mut matches = ProgramModelMatches::default();
    let p = crate::models::spec::parse_declared_access_path(".next.next.next", 0).unwrap();
    matches.access_paths.insert(p);
    let report = codegen_model_matches(&matches, &[], &mut facts, &mut source_info).expect("cg");
    assert_eq!(report.declared_paths, 1);
    assert_eq!(facts.paths, vec![(p,)]);
}

/// A model naming a function this project does not contain is not an error: a model file names
/// functions that may or may not be present.
#[test]
fn a_propagation_on_an_absent_function_is_skipped() {
    let (mut facts, mut source_info) = fact_base(&[("present", 0)]);
    let mut matches = ProgramModelMatches::default();
    matches.propagations.push(PropagationMatch {
        function: facts::Str::from("absent"),
        dst: model_port(FormalIndexTypeTag::Return, None),
        src: model_port(FormalIndexTypeTag::Index, Some(0)),
    });
    let report = codegen_model_matches(&matches, &[], &mut facts, &mut source_info).expect("cg");
    assert_eq!(report.summaries, 0);
    assert!(facts.summary.is_empty());
}

// ---------------------------------------------------------------------------
// Helpers for readable assertions
// ---------------------------------------------------------------------------

fn vertex_str(v: &FlowVertex) -> String {
    let FlowVertex(var, path) = v;
    format!("{}{}", var, path.to_dot_string())
}

fn local_name(v: FlowVariable) -> String {
    format!("{v}")
}
