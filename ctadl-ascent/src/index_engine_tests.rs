//! Tests of the index engine (`ctadl_index::index_engine`) that lower real programs through
//! `codegen` first. They live in this crate because `codegen` does.

use crate::codegen::{CallResolutionStrategy, codegen_program};
use crate::facts::{CallTargetObject, FlowVariable, FunctionId, IdMap, InsnId, Path};
use crate::index_engine::source_info::IndexSourceInfo;
use crate::index_engine::*;
use ctadl_ir::ProgramInfo;
use ctadl_ir::mir::builder::FunctionBuilder;
use ctadl_ir::mir::call::{CallEdges, CallObject, CallStyle};
use ctadl_ir::mir::{AccessPath, Exp, FunctionData, Functions, ParameterType, Program, ReturnType};
use std::num::NonZeroUsize;

/// Lowers one C translation unit. This function and [`index_program`] are test helpers
/// that belong to the engine. `ctadl-c` has almost the same helpers, but the engine keeps
/// its own thirty lines instead of reaching into a front end's private code. The crates
/// depend on each other in the other direction, and sharing them would need a
/// dev-dependency cycle in both directions.
fn program_from_string(src: &str) -> (ctadl_ir::Program, String) {
    let (program, failed, dump) =
        ctadl_c::parse_c_program(src).expect("Failed to parse C program.");
    assert!(!failed, "Input Program failed to parse without error");
    assert!(
        !dump.contains("<no terminator>"),
        "Parsed IR contains a block with no terminator:\n{dump}"
    );
    (program, dump)
}

/// Runs SSA, then code generation. Returns the facts the taint index runs on, and the
/// source info that names their functions.
fn index_program(program: ctadl_ir::Program) -> (IndexFacts, IndexSourceInfo) {
    let mut program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    program_info.program.verify().unwrap();
    ctadl_ir::ssa::transform_program(&mut program_info.program, true);
    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_program(
        program_info,
        &mut facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
        Default::default(),
        &Default::default(),
    );
    (facts, source_info)
}

/// A program that touches every part of the rules: direct calls, summaries, field-sensitive
/// flows, and enough indirect calls (function-pointer parameter, function pointer in a
/// struct field, one target reaching two critical sites) that the hybrid-inlining relations
/// and their decision sets all get rows.
const SRC: &str = r"
    struct ops { int (*f)(int); int tag; };
    int id(int p) { return p; }
    int twice(int p) { return p + p; }
    int apply(int (*f)(int), int x) { return f(x); }
    int through(struct ops *o, int x) { return o->f(x); }
    int wrap(int a, int b) {
        struct ops o;
        o.f = id;
        o.tag = a;
        int r = apply(twice, b);
        int s = through(&o, r);
        return s + apply(id, a) + o.tag;
    }";

/// The output relations of one run, each sorted, so two runs compare as sets.
#[allow(clippy::type_complexity)]
fn canonical(
    result: IndexResult,
) -> (
    Vec<FunctionSummary>,
    Vec<(FunctionId, FlowVariable, Path, FlowVariable, Path)>,
    Vec<(FunctionId, FlowVariable, Path, CallTargetObject)>,
    Vec<(FunctionId, InsnId, FunctionId)>,
    Vec<(Path,)>,
    Vec<(FunctionId,)>,
) {
    let IndexResult {
        mut summary,
        mut assign_like,
        mut call_target_assign_like,
        mut resolved_call,
        const_reaches: _,
        intent_pair: _,
        mut paths,
        mut external_function,
        global_flow: _,
        stats: _,
    } = result;
    summary.sort();
    assign_like.sort();
    call_target_assign_like.sort();
    resolved_call.sort();
    paths.sort();
    external_function.sort();
    (
        summary,
        assign_like,
        call_target_assign_like,
        resolved_call,
        paths,
        external_function,
    )
}

/// The two engines are one rule text under two macros, so they must agree on every
/// relation they return, and on the sizes of the internal ones they do not. Run the parallel
/// engine on more threads than the program has functions so the rules actually interleave.
#[test_log::test]
fn parallel_engine_matches_serial() {
    let (program, _) = program_from_string(SRC);
    let (facts, source_info) = index_program(program);
    let id_map = Some(&source_info.sites);

    let config = IndexConfig::default();
    let serial = taint_index_with_config(facts.clone(), config.clone(), id_map);
    let parallel = taint_index_with_config(
        facts,
        IndexConfig {
            parallelism: Parallelism::Threads(NonZeroUsize::new(8).unwrap()),
            ..config
        },
        id_map,
    );

    // The fixture only proves anything if the hybrid machinery actually ran.
    assert!(
        serial.stats.hybrid_resolvent > 0,
        "fixture derived no resolvents"
    );
    assert!(
        serial.stats.hybrid_context_assign > 0,
        "fixture derived no context_assign rows"
    );
    assert_same_index(serial, parallel);
}

/// The LSM storage holds the same relations as the tries, so the engine must derive the
/// same index. A tiny write buffer makes every index spill into several runs and merge them.
#[test_log::test]
fn lsm_storage_matches_memory() {
    let (program, _) = program_from_string(SRC);
    let (facts, source_info) = index_program(program);
    let id_map = Some(&source_info.sites);

    let config = IndexConfig::default();
    let memory = taint_index_with_config(facts.clone(), config.clone(), id_map);
    lsm::ascent_provider::set_memtable_limit(4);
    let on_lsm = taint_index_with_config(
        facts,
        IndexConfig {
            storage: Storage::Lsm,
            ..config
        },
        id_map,
    );
    lsm::ascent_provider::set_memtable_limit(lsm::ascent_provider::DEFAULT_MEMTABLE_LIMIT);
    assert_same_index(memory, on_lsm);
}

/// Asserts two runs of the same facts derived the same index.
fn assert_same_index(a: IndexResult, b: IndexResult) {
    // The internal relations first: a count that differs points straight at the rule group
    // that diverged, which the output diff below would only show as a symptom.
    let (s, p) = (&a.stats, &b.stats);
    assert_eq!(s.final_locals, p.final_locals, "locals");
    assert_eq!(
        s.reached_variables, p.reached_variables,
        "reached variables"
    );
    assert_eq!(
        s.hybrid_critical_summary, p.hybrid_critical_summary,
        "critical_summary"
    );
    assert_eq!(s.hybrid_resolvent, p.hybrid_resolvent, "resolvent");
    assert_eq!(
        s.hybrid_context_assign, p.hybrid_context_assign,
        "context_assign"
    );
    assert_eq!(
        s.hybrid_context_locals, p.hybrid_context_locals,
        "context_locals"
    );
    assert_eq!(
        s.hybrid_context_summary, p.hybrid_context_summary,
        "context_summary"
    );
    assert_eq!(canonical(a), canonical(b));
}

/// The three context joins are three ways to pair the same two relations, so they must
/// agree on every relation, internal and returned. Same fixture and same checks as the
/// engine test above.
#[test_log::test]
fn context_joins_agree() {
    let (program, _) = program_from_string(SRC);
    let (facts, source_info) = index_program(program);
    let id_map = Some(&source_info.sites);
    let run = |context_join| {
        taint_index_with_config(
            facts.clone(),
            IndexConfig {
                context_join,
                ..IndexConfig::default()
            },
            id_map,
        )
    };
    let scan = run(ContextJoin::Scan);
    let scan_stats = scan.stats.clone();
    let scan_rows = canonical(scan);
    for other in [ContextJoin::Sets, ContextJoin::Unfold] {
        let r = run(other);
        let (s, p) = (&scan_stats, &r.stats);
        assert_eq!(s.final_locals, p.final_locals, "{other}: locals");
        assert_eq!(s.hybrid_resolvent, p.hybrid_resolvent, "{other}: resolvent");
        assert_eq!(
            s.hybrid_context_assign, p.hybrid_context_assign,
            "{other}: context_assign"
        );
        assert_eq!(
            s.hybrid_context_locals, p.hybrid_context_locals,
            "{other}: context_locals"
        );
        assert_eq!(
            s.hybrid_context_summary, p.hybrid_context_summary,
            "{other}: context_summary"
        );
        assert!(
            s.hybrid_context_assign > 0,
            "fixture derived no context_assign rows"
        );
        assert_eq!(scan_rows, canonical(r), "{other}");
    }
}

/// Offsets: arrays in a struct and pointer arithmetic, so paths end in offsets and the wild
/// splits, the trailing-offset splits and the offset adjustments of the candidate test all run.
const SRC_OFFSETS: &str = r"
    struct buf { int len; int data[4]; struct buf *next; };
    int get(struct buf *b, int i) { return b->data[1] + b->data[i]; }
    void put(struct buf *b, int v) { b->data[2] = v; b->next->data[1] = v; }
    int walk(struct buf *b, int v) {
        int *p = &b->data[0];
        p[3] = v;
        put(b, v);
        struct buf *c = b + 1;
        put(c, get(b, v));
        return get(c, 2) + *(p + 1);
    }";

/// A call target that crosses copies of the aggregates that hold it, so the call-target rules
/// test candidates that are not paths of the program.
const SRC_NESTED_TARGET: &str = r"
    struct ops { int (*f)(int); int tag; };
    struct box { struct ops o; struct ops *p; struct box *up; };
    int id(int x) { return x; }
    int call(struct box *b, int x) { return b->o.f(x) + b->p->f(x) + b->up->o.f(x); }
    int run(int x) {
        struct box b, c;
        b.o.f = id;
        c.o = b.o;
        c.p = &b.o;
        c.up = &b;
        b.up = &c;
        return call(&c, x) + call(&b, x);
    }";

/// The fixpoint interns no `Path`, `Str`, `CallTargetObject` or `CallDispatchKey`, on every
/// engine. The interners are process-wide and the other tests in this binary intern into them
/// from other threads, so the test re-runs itself as a child process that runs nothing else,
/// and the child does the checking.
#[test]
fn fixpoint_interns_nothing() {
    const CHILD_ENV: &str = "CTADL_TEST_FIXPOINT_INTERNS_NOTHING";
    if std::env::var_os(CHILD_ENV).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "index_engine_tests::fixpoint_interns_nothing",
                "--test-threads",
                "1",
            ])
            .env(CHILD_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success(), "the child failed");
        return;
    }
    for src in [SRC, SRC_OFFSETS, SRC_NESTED_TARGET] {
        let (program, _) = program_from_string(src);
        let (facts, source_info) = index_program(program);
        for config in [
            IndexConfig::default(),
            IndexConfig {
                parallelism: Parallelism::Threads(NonZeroUsize::new(8).unwrap()),
                ..IndexConfig::default()
            },
            IndexConfig {
                storage: Storage::Lsm,
                ..IndexConfig::default()
            },
        ] {
            let what = format!("{:?} {:?}", config.parallelism, config.storage);
            let result = taint_index_with_config(facts.clone(), config, Some(&source_info.sites));
            let profile = &result.stats.profile;
            assert!(profile.reached_fixpoint, "{what}");
            assert_eq!(profile.split_misses, 0, "{what}: split misses");
            let interned = &profile.memory.fixpoint_interned;
            assert_eq!(
                interned.keys().copied().collect::<Vec<_>>(),
                ["CallDispatchKey", "CallTargetObject", "Path", "Str"],
                "{what}"
            );
            assert!(
                interned.values().all(|&n| n == 0),
                "{what}: the fixpoint interned {interned:?}"
            );
        }
    }
}

/// The id codegen gave `name`. An exact lookup rather than a search: these programs are
/// built here, so the name in the fixture is the name in the id map.
fn func_id(id_map: &IdMap, name: &str) -> FunctionId {
    id_map
        .get_function_id(crate::facts::Function(name.into()))
        .unwrap_or_else(|| panic!("no function named {name:?}"))
}

/// `fn <name>(p) { return p; }`, or `{ return 7; }` when `pass_through` is false.
///
/// The constant-returning form is the whole point of the empty-summary fixture below: a
/// formal that reaches no out-formal derives no `summary` row, so the call site that
/// resolves to it instantiates nothing.
fn leaf(name: &str, pass_through: bool) -> FunctionData {
    let mut f = FunctionData {
        name: name.to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };
    let mut fb = FunctionBuilder::new(&mut f);
    let p = fb.add_param(ParameterType::SharedFields);
    let body = fb.add_block();
    let mut b = fb.at_block(body);
    let ret: Exp = if pass_through {
        b.new_param_var(p).into()
    } else {
        b.new_int_exp(7)
    };
    b.create_ret(vec![ret]);
    f
}

/// `fn <name>(x) { fp = <target>; return fp(x); }`.
///
/// The target is established in the calling function itself, which is the arm the
/// local-dispatch bypass covers. The `ObjectRef` operand is what codegen turns into the
/// `call_target_assign` row that resolution follows to the call site; the `FuncPtrCall`
/// is what it turns into the `callee_info` row naming the site.
fn calls_stored_target(name: &str, target: &str) -> FunctionData {
    let mut f = FunctionData {
        name: name.to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };
    let mut fb = FunctionBuilder::new(&mut f);
    let x = fb.add_param(ParameterType::SharedFields);
    let body = fb.add_block();
    let mut b = fb.at_block(body);
    let (fp, r, x) = (
        b.new_local_var("fp"),
        b.new_local_var("r"),
        b.new_param_var(x),
    );
    b.create_assign(
        fp.clone(),
        vec![Exp::ObjectRef(CallObject::FunctionPtr(target.into()))],
    );
    b.create_call(
        CallStyle::FuncPtrCall {
            callee: AccessPath::from(fp),
            signature: None,
        },
        vec![r.clone()],
        vec![x.into()],
    );
    b.create_ret(vec![r.into()]);
    f
}

/// `fn <name>(f, x) { return f(x); }`.
///
/// Nothing here establishes the target: it has to arrive from a caller, which is the arm
/// the resolvent path covers. On its own this function is also the unresolvable fixture,
/// since a site whose formal nothing reaches resolves to nothing.
fn calls_formal_target(name: &str) -> FunctionData {
    let mut f = FunctionData {
        name: name.to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };
    let mut fb = FunctionBuilder::new(&mut f);
    let callee = fb.add_param(ParameterType::SharedFields);
    let x = fb.add_param(ParameterType::SharedFields);
    let body = fb.add_block();
    let mut b = fb.at_block(body);
    let (r, callee, x) = (
        b.new_local_var("r"),
        b.new_param_var(callee),
        b.new_param_var(x),
    );
    b.create_call(
        CallStyle::FuncPtrCall {
            callee: AccessPath::from(callee),
            signature: None,
        },
        vec![r.clone()],
        vec![x.into()],
    );
    b.create_ret(vec![r.into()]);
    f
}

/// `fn <name>(a) { return <callee>(<target>, a); }`: hands `target` to `callee`'s
/// function-pointer formal, which is what seeds the resolvent the formal arm needs.
fn passes_target(name: &str, callee: &str, target: &str) -> FunctionData {
    let mut f = FunctionData {
        name: name.to_string(),
        return_type: ReturnType { arity: 1 },
        ..Default::default()
    };
    let mut fb = FunctionBuilder::new(&mut f);
    let a = fb.add_param(ParameterType::SharedFields);
    let body = fb.add_block();
    let mut b = fb.at_block(body);
    let (r, a) = (b.new_local_var("r"), b.new_param_var(a));
    b.create_call(
        CallStyle::DirectCall {
            call_edges: CallEdges::Explicit(ctadl_ir::thin_vec![callee.to_string()]),
        },
        vec![r.clone()],
        vec![
            Exp::ObjectRef(CallObject::FunctionPtr(target.into())),
            a.into(),
        ],
    );
    b.create_ret(vec![r.into()]);
    f
}

/// Runs the index over an explicitly built program and hands back the result beside the
/// id map, which is what the `resolved_call` assertions need in order to name a callee.
fn index_functions(
    functions: impl IntoIterator<Item = FunctionData>,
) -> (IndexResult, IndexSourceInfo) {
    let program = Program::new(Functions::new(functions));
    let (facts, source_info) = index_program(program);
    let result = taint_index_with_config(facts, IndexConfig::default(), Some(&source_info.sites));
    (result, source_info)
}

/// `fn thrower(x) -> (normal, exception) { return ((), x); }` and
/// `fn catcher(a) { r, e = thrower(a); return (e, ()); }`: a value thrown out of a callee
/// reaches the caller's exception slot, and from there anything the caller does with it.
/// Java and CIL functions return arity 2, and the second slot is a formal like the first,
/// with a `formal_param` row of its own.
#[test]
fn exceptional_return_is_summarized() {
    let mut thrower = FunctionData {
        name: "thrower".to_string(),
        return_type: ReturnType { arity: 2 },
        ..Default::default()
    };
    {
        let mut fb = FunctionBuilder::new(&mut thrower);
        let x = fb.add_param(ParameterType::SharedFields);
        let body = fb.add_block();
        let mut b = fb.at_block(body);
        let x = b.new_param_var(x);
        b.create_ret(vec![Exp::new_bytes(Vec::new()), x.into()]);
    }
    let mut catcher = FunctionData {
        name: "catcher".to_string(),
        return_type: ReturnType { arity: 2 },
        ..Default::default()
    };
    {
        let mut fb = FunctionBuilder::new(&mut catcher);
        let a = fb.add_param(ParameterType::SharedFields);
        let body = fb.add_block();
        let mut b = fb.at_block(body);
        let (r, e, a) = (
            b.new_local_var("r"),
            b.new_local_var("e"),
            b.new_param_var(a),
        );
        b.create_call(
            CallStyle::DirectCall {
                call_edges: CallEdges::Explicit(ctadl_ir::thin_vec!["thrower".to_string()]),
            },
            vec![r, e.clone()],
            vec![a.into()],
        );
        b.create_ret(vec![e.into(), Exp::new_bytes(Vec::new())]);
    }
    let (result, si) = index_functions([thrower, catcher]);
    let (thrower, catcher) = (func_id(&si.sites, "thrower"), func_id(&si.sites, "catcher"));
    let has = |f: FunctionId, dst: i16, src: i16| {
        result.summary.iter().any(|(sf, d, dp, s, sp)| {
            *sf == f
                && d.as_formal().map(|i| *i) == Some(dst)
                && dp.is_empty()
                && s.as_formal().map(|i| *i) == Some(src)
                && sp.is_empty()
        })
    };
    assert!(has(thrower, -2, 0), "summary = {:?}", result.summary);
    assert!(has(catcher, -1, 0), "summary = {:?}", result.summary);
}

/// A site is resolved by either of two rules -- a target stored in the calling function,
/// or one arriving through a formal -- and `resolved_call` has an arm for each. One arm
/// alone would silently drop half the call graph.
#[test_log::test]
fn resolved_call_covers_both_dispatch_arms() {
    let (result, si) = index_functions([
        leaf("id", true),
        calls_stored_target("local_arm", "id"),
        calls_formal_target("formal_arm"),
        passes_target("drive", "formal_arm", "id"),
    ]);
    let id_map = &si.sites;
    let (id, local_arm, formal_arm) = (
        func_id(id_map, "id"),
        func_id(id_map, "local_arm"),
        func_id(id_map, "formal_arm"),
    );

    let resolved: Vec<(FunctionId, FunctionId)> = result
        .resolved_call
        .iter()
        .map(|(caller, _, target)| (*caller, *target))
        .collect();
    assert!(
        resolved.contains(&(local_arm, id)),
        "the locally stored target did not resolve; resolved_call = {:?}",
        result.resolved_call
    );
    assert!(
        resolved.contains(&(formal_arm, id)),
        "the target passed through a formal did not resolve; resolved_call = {:?}",
        result.resolved_call
    );
}

// ---------------------------------------------------------------------------------------------
// Globals (G2, G5, G6 of `docs/ir.md`). A C global
// `g` is the global `$g`, whose storage is `$g.deref`.
// ---------------------------------------------------------------------------------------------

mod globals {
    use super::*;
    use crate::facts::{GLOBAL_FRAME, GlobalId, Root};

    fn index_c(src: &str) -> (IndexFacts, IndexSourceInfo, IndexResult) {
        let (facts, source_info) = index_program(program_from_string(src).0);
        let result = taint_index(facts.clone());
        (facts, source_info, result)
    }

    fn deref() -> Path {
        Path::parse(".deref").unwrap()
    }

    fn global(name: &str) -> GlobalId {
        GlobalId::new(name)
    }

    /// The call sites of `f` that the index resolved, whatever the target.
    fn resolved_in(result: &IndexResult, f: FunctionId) -> Vec<FunctionId> {
        result
            .resolved_call
            .iter()
            .filter(|(caller, ..)| *caller == f)
            .map(|(.., target)| *target)
            .collect()
    }

    /// A flow between two global paths is no function's summary row, however many
    /// functions call the one that makes it; it is an edge of the global frame, and is in
    /// `global_flow`.
    #[test]
    fn a_flow_between_two_globals_is_a_frame_edge_not_a_summary() {
        let (_, si, result) = index_c(
            r"
            int a; int b;
            void copy(void) { b = a; }
            void c1(void) { copy(); }
            void c2(void) { c1(); copy(); }",
        );
        assert!(
            !result
                .summary
                .iter()
                .any(|(_, d, _, s, _)| d.is_global() && s.is_global()),
            "no summary row has two global roots: {:#?}",
            result.summary
        );
        let edge = (global("b"), deref(), global("a"), deref());
        assert_eq!(result.global_flow, vec![edge], "{:#?}", result.global_flow);
        assert!(result.assign_like.contains(&(
            GLOBAL_FRAME,
            FlowVariable::global(edge.0),
            edge.1,
            FlowVariable::global(edge.2),
            edge.3
        )));
        // And nothing is passed for a global at a call (G5): no function has a summary at all.
        for f in ["copy", "c1", "c2"] {
            let f = func_id(&si.sites, f);
            assert!(
                result.summary.iter().all(|r| r.0 != f),
                "{:#?}",
                result.summary
            );
        }
    }

    /// A flow from a formal into a global, and from a global to the return, is a
    /// summary row rooted at the global, and a caller passing its own formal inherits the
    /// first.
    #[test]
    fn a_flow_between_a_formal_and_a_global_is_a_summary_rooted_at_the_global() {
        let (_, si, result) = index_c(
            r"
            int g;
            void put(int x) { g = x; }
            int get(void) { return g; }
            void outer(int y) { put(y); }",
        );
        let g = Root::global(global("g"));
        let (put, get, outer) = (
            func_id(&si.sites, "put"),
            func_id(&si.sites, "get"),
            func_id(&si.sites, "outer"),
        );
        let s = &result.summary;
        assert!(
            s.contains(&(put, g, deref(), 0i16.into(), Path::empty())),
            "{s:#?}"
        );
        assert!(
            s.contains(&(get, (-1i16).into(), Path::empty(), g, deref())),
            "{s:#?}"
        );
        assert!(
            s.contains(&(outer, g, deref(), 0i16.into(), Path::empty())),
            "{s:#?}"
        );
    }

    /// G6: a call target stored at a global path in one function resolves an indirect call
    /// through that path in a function no one calls, unconditionally; and the same when the
    /// target crosses a program-level edge first.
    #[test]
    fn a_target_stored_to_a_global_resolves_a_call_in_a_function_with_no_caller() {
        let (_, si, result) = index_c(
            r"
            void target(int x) {}
            void (*g)(int);
            void (*h)(int);
            void install(void) { g = target; }
            void move(void) { h = g; }
            void fire_g(int x) { g(x); }
            void fire_h(int x) { h(x); }",
        );
        let target = func_id(&si.sites, "target");
        for fire in ["fire_g", "fire_h"] {
            assert_eq!(
                resolved_in(&result, func_id(&si.sites, fire)),
                vec![target],
                "{fire}: {:#?}",
                result.resolved_call
            );
        }
    }

    /// Labels come down by the paths a function reads. A function that reads
    /// `$t.deref.a` gets the label stored there; one that reads only `$t.deref.b` gets nothing;
    /// one that reads the whole cell gets the label at its extension and resolves a call
    /// through a copy of it.
    #[test]
    fn labels_come_down_at_the_paths_a_function_reads() {
        let (_, si, result) = index_c(
            r"
            void target(int x) {}
            struct T { void (*a)(int); void (*b)(int); };
            struct T t;
            void install(void) { t.a = target; }
            void fire_a(int x) { t.a(x); }
            void fire_b(int x) { t.b(x); }
            void fire_copy(int x) { struct T l = t; l.a(x); }",
        );
        let target = func_id(&si.sites, "target");
        let fire_b = func_id(&si.sites, "fire_b");
        assert_eq!(
            resolved_in(&result, func_id(&si.sites, "fire_a")),
            vec![target]
        );
        assert_eq!(resolved_in(&result, fire_b), vec![]);
        assert!(
            result
                .call_target_assign_like
                .iter()
                .all(|(f, ..)| *f != fire_b),
            "a function that reads only `$t.deref.b` holds no label: {:#?}",
            result.call_target_assign_like
        );
        assert_eq!(
            resolved_in(&result, func_id(&si.sites, "fire_copy")),
            vec![target],
            "{:#?}",
            result.call_target_assign_like
        );
    }

    /// A flow between two globals that holds only under a decision is a conditional
    /// summary row of the function that makes it, applied by rule 3.2 at the caller whose call
    /// establishes the decision, where it becomes a frame edge. Without that caller there is no
    /// frame edge.
    #[test]
    fn a_conditional_flow_between_globals_lands_in_the_frame_at_its_establishing_caller() {
        let lib = r"
            int a; int b;
            void set(int *dst, int v) { *dst = v; }
            void run(void (*f)(int *, int)) { f(&b, a); }";
        let edge = (global("b"), deref(), global("a"), deref());

        let (_, _, alone) = index_c(lib);
        assert!(
            !alone.global_flow.contains(&edge),
            "{:#?}",
            alone.global_flow
        );

        let (_, _, called) = index_c(&format!("{lib}\nvoid main1(void) {{ run(set); }}"));
        assert!(
            called.global_flow.contains(&edge),
            "{:#?}",
            called.global_flow
        );
        assert!(
            !called
                .summary
                .iter()
                .any(|(_, d, _, s, _)| d.is_global() && s.is_global()),
            "{:#?}",
            called.summary
        );
    }

    /// A store through a whole copy of a global, `l = &g; l->f = v`, reaches the global's
    /// path, as a store through a copy of a formal does.
    #[test]
    fn a_store_through_a_copy_of_a_global_reaches_the_global() {
        let (_, si, result) = index_c(
            r"
            struct S { int f; };
            struct S g;
            void f(int v) { struct S *l = &g; l->f = v; }",
        );
        let f = func_id(&si.sites, "f");
        let gf = Path::parse(".deref.f").unwrap();
        assert!(
            result.summary.contains(&(
                f,
                Root::global(global("g")),
                gf,
                0i16.into(),
                Path::empty()
            )),
            "{:#?}",
            result.summary
        );
    }

    /// G3: two imports that declare one name declare one global, the same vertex in both,
    /// with a `globals` row per import; a name only one declares is its alone.
    #[test]
    fn two_imports_declaring_one_name_share_one_global() {
        let mut facts = IndexFacts::default();
        let mut si = IndexSourceInfo::default();
        for (name, src) in [
            (
                "one",
                "int shared; int mine; void put(int x) { shared = x; mine = x; }",
            ),
            ("two", "int shared; int get(void) { return shared; }"),
        ] {
            si.begin_import(name);
            let mut program_info = ProgramInfo {
                program: program_from_string(src).0,
                ..Default::default()
            };
            ctadl_ir::ssa::transform_program(&mut program_info.program, true);
            codegen_program(
                program_info,
                &mut facts,
                &mut si,
                CallResolutionStrategy::Mixed,
                Default::default(),
                &Default::default(),
            );
        }
        let mut rows: Vec<_> = facts
            .globals
            .iter()
            .map(|(g, i, _)| {
                (
                    g.name().to_string(),
                    si.import_name(*i).unwrap().to_string(),
                )
            })
            .collect();
        rows.sort();
        let row = |g: &str, i: &str| (g.to_string(), i.to_string());
        assert_eq!(
            rows,
            vec![
                row("mine", "one"),
                row("shared", "one"),
                row("shared", "two")
            ]
        );
        let (per_import, shared) = facts.global_declarations();
        assert_eq!(per_import.values().copied().collect::<Vec<_>>(), vec![2, 1]);
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].0, global("shared"));

        // The flow in through one import's function and out through the other's is one
        // global's: `put`'s row and `get`'s row name the same root.
        let result = taint_index(facts);
        let g = Root::global(global("shared"));
        let put = func_id(&si.sites, "put");
        let get = func_id(&si.sites, "get");
        assert!(
            result
                .summary
                .contains(&(put, g, deref(), 0i16.into(), Path::empty()))
        );
        assert!(
            result
                .summary
                .contains(&(get, (-1i16).into(), Path::empty(), g, deref()))
        );
    }

    /// A library indexed alone contributes its summaries and its program-level flows to a
    /// consumer, by name: a flow in through one library function, held in a library global,
    /// moved to another by a third, and out through a fourth, is found in the consumer.
    #[test]
    fn a_summary_projects_global_flows_cross_into_its_consumer() {
        let (_, lib_si, lib) = index_c(
            r"
            int g; int h;
            void put(int x) { g = x; }
            void mv(void) { h = g; }
            int get(void) { return h; }",
        );
        assert_eq!(
            lib.global_flow,
            vec![(global("h"), deref(), global("g"), deref())]
        );

        let (mut facts, si) = index_program(
            program_from_string(
                r"
                void put(int);
                int get(void);
                int main1(int x) { put(x); return get(); }",
            )
            .0,
        );
        for (f, d, dp, s, sp) in &lib.summary {
            let name = lib_si.sites.get_function(*f).unwrap().clone();
            if let Some(f) = si.sites.get_function_id(name) {
                facts.summary.push((f, *d, *dp, *s, *sp));
            }
        }
        facts.global_flow = lib.global_flow.clone();
        let result = taint_index(facts.clone());
        let main1 = func_id(&si.sites, "main1");
        let endpoint = |var: FlowVariable, direction| crate::query_engine::QueryEndpoint {
            infunc: main1,
            vertex: crate::facts::FlowVertex::new(var, Path::empty()),
            label: crate::facts::Label("L".into()),
            direction,
            call_site: None,
            saturating: false,
        };
        let query = crate::query_engine::QueryFacts {
            formal_param: facts.formal_param,
            actual_param: facts.actual_param,
            call: facts.call,
            assign: result.assign_like,
            paths: result.paths,
            external_function: result.external_function,
            endpoints: vec![
                (endpoint(
                    FlowVariable::formal_index(0i16.into()),
                    crate::facts::TaintDirection::Forward,
                ),),
                (endpoint(
                    FlowVariable::formal_index((-1i16).into()),
                    crate::facts::TaintDirection::Backward,
                ),),
            ],
        };
        let found = crate::query_engine::taint_analysis(query, Some(&si.sites));
        assert!(
            found
                .taint_edge
                .iter()
                .any(|e| e.0 == crate::facts::FlowEdge::Global),
            "the flow goes through the frame: {:#?}",
            found.taint_edge
        );
        assert!(
            found.taint.iter().any(|r| r.0 == main1
                && r.2 == FlowVariable::formal_index((-1i16).into())
                && r.4.direction == crate::facts::TaintDirection::Forward),
            "the return of `main1` is reached from its parameter"
        );
    }
}
