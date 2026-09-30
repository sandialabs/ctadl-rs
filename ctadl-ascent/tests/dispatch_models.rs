//! `find: "dispatch"` end to end: from a model file to the facts a call site emits and the
//! flow the indexer derives from them.
//!
//! One fixture throughout. `LI;->m(Ljava/lang/Object;)Ljava/lang/Object;` has two implementers,
//! `LA;` and `LB;`, and one caller that hands its own parameter to the interface method and
//! returns the result. What each disposition does to that caller's summary is the whole test:
//! under a model the caller propagates without either implementation being in the graph, under
//! a skip it propagates nothing, and under CHA both implementations are edges.

use ctadl_ascent::codegen::{CallPolicy, CallResolutionStrategy, codegen_program};
use ctadl_ascent::facts as fx;
use ctadl_ascent::index_engine::source_info::IndexSourceInfo;
use ctadl_ascent::index_engine::{IndexFacts, taint_index};
use ctadl_ascent::models::{
    DispatchKeys, ImportScope, ProgramMatchIndex, ProgramModelMatches, try_load_models_from_values,
};
use ctadl_ir::index::idx::Idx;
use ctadl_ir::mir::builder::FunctionBuilder;
use ctadl_ir::mir::call::{
    CallStyle, JavaClass, JavaDispatch, JavaMethod, JavaSignature, JavaSimpleName,
    VirtualMethodTable,
};
use ctadl_ir::mir::{Exp, FunctionData, ParameterType, Program, ProgramInfo};

const IFACE: &str = "Ljava/util/Iterator;";
const NAME: &str = "next";
const DESC: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";
const CALLER: &str = "Lcom/example/Caller;->run(Ljava/lang/Object;)Ljava/lang/Object;";
const IMPL_A: &str = "LA;->next(Ljava/lang/Object;)Ljava/lang/Object;";
const IMPL_B: &str = "LB;->next(Ljava/lang/Object;)Ljava/lang/Object;";
/// What the ladder names the signature's summary carrier.
const SYNTHETIC: &str =
    "ctadl$dispatch$Ljava/util/Iterator;->next(Ljava/lang/Object;)Ljava/lang/Object;";

/// An implementation of the interface method that moves nothing of its own, so any flow the
/// caller shows came from the model rather than from a body.
fn implementation(cls: &str) -> FunctionData {
    let mut f = FunctionData {
        name: format!("{cls}->{NAME}{DESC}"),
        ..Default::default()
    };
    f.params.parameters.push(ParameterType::ByRef);
    f.params.parameters.push(ParameterType::ByRef);
    let mut fb = FunctionBuilder::new(&mut f);
    fb.set_return_arity(1);
    let body = fb.add_block();
    let mut b = fb.at_block(body);
    let nothing = b.new_local_var("nothing");
    b.create_assign(nothing.clone(), Vec::<Exp>::new());
    b.create_ret(vec![Exp::Variable(nothing)]);
    f
}

/// `run(arg)` calls the interface method with `arg` and returns what it gets back.
fn caller() -> FunctionData {
    let mut f = FunctionData {
        name: CALLER.to_string(),
        ..Default::default()
    };
    f.params.parameters.push(ParameterType::ByRef);
    f.params.parameters.push(ParameterType::ByRef);
    let mut fb = FunctionBuilder::new(&mut f);
    fb.set_return_arity(1);
    let body = fb.add_block();
    let mut b = fb.at_block(body);
    let recv = b.new_local_var("recv");
    b.create_assign(
        recv.clone(),
        vec![Exp::ObjectRef(ctadl_ir::mir::CallObject::JavaObject(
            JavaClass("LA;".into()),
        ))],
    );
    let arg = b.new_param_var(ctadl_ir::mir::ParameterIdx::new(1));
    let result = b.new_local_var("result");
    b.create_call(
        CallStyle::JavaCall {
            receiver: recv,
            cls: IFACE.into(),
            simple_name: NAME.into(),
            descriptor: DESC.into(),
            dispatch: JavaDispatch::Interface,
            super_start: None,
        },
        vec![result.clone()],
        vec![Exp::Variable(arg)],
    );
    b.create_ret(vec![Exp::Variable(result)]);
    f
}

fn program_info() -> ProgramInfo {
    let method = |cls: &str| {
        (
            JavaClass(cls.into()),
            JavaSimpleName(NAME.into()),
            JavaSignature(DESC.into()),
            JavaMethod(format!("{cls}->{NAME}{DESC}").as_str().into()),
        )
    };
    let mut program = Program::default();
    for f in [caller(), implementation("LA;"), implementation("LB;")] {
        let idx = program.new_function();
        program[idx] = f;
    }
    ProgramInfo {
        program,
        vmt: VirtualMethodTable::Java {
            methods: vec![method("LA;"), method("LB;")],
            hierarchy: [
                (
                    JavaClass("LA;".into()),
                    smallvec::smallvec![JavaClass(IFACE.into())],
                ),
                (
                    JavaClass("LB;".into()),
                    smallvec::smallvec![JavaClass(IFACE.into())],
                ),
            ]
            .into_iter()
            .collect(),
            interfaces: vec![JavaClass(IFACE.into())],
            abstract_methods: vec![(
                JavaClass(IFACE.into()),
                JavaSimpleName(NAME.into()),
                JavaSignature(DESC.into()),
            )],
            natives: Vec::new(),
        },
        ..Default::default()
    }
}

/// What one run of the pipeline produced, named so the assertions read as questions about the
/// call site rather than about table indices.
struct Indexed {
    facts: IndexFacts,
    source_info: IndexSourceInfo,
    report: ctadl_ascent::codegen::CodegenReport,
    summary: Vec<(
        fx::FunctionId,
        fx::FormalIndex,
        fx::Path,
        fx::FormalIndex,
        fx::Path,
    )>,
}

impl Indexed {
    fn id(&self, name: &str) -> Option<fx::FunctionId> {
        self.source_info
            .sites
            .get_function_id(fx::Function(name.into()))
    }

    /// The functions the call site has `call` rows to.
    fn callees(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .facts
            .call
            .iter()
            .map(|(_, callee)| {
                self.source_info
                    .sites
                    .get_function(*callee)
                    .expect("an interned callee")
                    .0
                    .to_string()
            })
            .collect();
        names.sort();
        names
    }

    /// Whether the caller's derived summary carries `arg -> return`, which is the flow the
    /// model is supposed to produce and the only flow this program can produce.
    fn caller_propagates(&self) -> bool {
        let Some(caller) = self.id(CALLER) else {
            return false;
        };
        self.summary.iter().any(|(f, dst, _, src, _)| {
            *f == caller && *dst == (-1i16).into() && *src == 1i16.into()
        })
    }
}

/// Runs the loader, phase 1 and phase 2 over the fixture with the given generators, plus any
/// endpoints the source/sink guard should see.
fn index_with(generators: Vec<serde_json::Value>, endpoint_functions: &[&str]) -> Indexed {
    let mut info = program_info();
    let mut matches = ProgramModelMatches::default();
    {
        let keys = DispatchKeys::from_program(&info.program);
        let match_index =
            ProgramMatchIndex::new_with_dispatch(&info, ImportScope::unknown(), Some(&keys));
        try_load_models_from_values(&match_index, generators.into_iter().map(Ok), &mut matches)
            .expect("loading models");
    }
    for function in endpoint_functions {
        matches.endpoints.push(endpoint(function));
    }
    ctadl_ir::ssa::run_pipeline(&mut info.program, ctadl_ir::ssa::Pipeline::index_default());

    let mut facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    let report = codegen_program(
        info,
        &mut facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
        CallPolicy {
            // Two targets, so without a model the site is comfortably under the threshold and
            // takes CHA. That is what makes every difference below attributable to the model.
            cha_threshold: 8,
            cha_threshold_interface: 8,
            ..CallPolicy::default()
        },
        &matches,
    );
    ctadl_ascent::codegen::model_matches::codegen_model_matches(
        &matches,
        &[],
        &mut facts,
        &mut source_info,
    )
    .expect("phase 2");
    let summary = taint_index(facts.clone()).summary;
    Indexed {
        facts,
        source_info,
        report,
        summary,
    }
}

/// A sink on the first argument of `function`, which is all the rung-1 guard looks at.
fn endpoint(function: &str) -> ctadl_ascent::models::EndpointMatch {
    ctadl_ascent::models::EndpointMatch {
        function: fx::Str::from(function),
        selector_ty: ctadl_ascent::models::FormalIndexTypeTag::Index,
        index: Some(1),
        path: fx::Path::empty(),
        label: fx::Str::from("test-sink"),
        direction: fx::TaintDirection::Backward,
        wildcard: true,
        saturating: false,
        in_function: None,
        callsite_scoped: false,
        local_index: None,
    }
}

fn generator(model: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "find": "dispatch",
        "where": [{"constraint": "signature_match", "names": [NAME], "parents": [IFACE]}],
        "model": model,
    })
}

/// The control: no dispatch model, so the site keeps both CHA edges and neither body moves
/// anything, so the caller propagates nothing.
#[test]
fn without_a_model_the_site_takes_cha() {
    let indexed = index_with(Vec::new(), &[]);
    assert_eq!(
        indexed.callees(),
        vec![IMPL_A.to_string(), IMPL_B.to_string()]
    );
    assert!(indexed.id(SYNTHETIC).is_none(), "nothing was synthesized");
    assert!(!indexed.caller_propagates());
    let buckets = indexed.report.buckets[JavaDispatch::Interface.index()];
    assert_eq!((buckets.java_sites, buckets.cha), (1, 1));
}

/// A model replaces the target set: one edge to the synthetic function, no CHA rows, and the
/// summary it carries is what the caller propagates.
#[test]
fn a_model_replaces_the_targets_and_carries_the_flow() {
    let indexed = index_with(
        vec![generator(serde_json::json!({
            "propagation": [{"input": "Argument(1)", "output": "Return"}]
        }))],
        &[],
    );
    assert_eq!(indexed.callees(), vec![SYNTHETIC.to_string()]);
    let synthetic = indexed.id(SYNTHETIC).expect("the synthetic function");
    assert!(
        indexed.facts.summary.iter().any(|(f, ..)| *f == synthetic),
        "the synthetic function carries the model's summary"
    );
    assert!(
        indexed.facts.external_function.contains(&(synthetic,)),
        "it has no body"
    );
    assert!(
        indexed.caller_propagates(),
        "taint flows through the model, with neither implementation in the graph"
    );
    let buckets = indexed.report.buckets[JavaDispatch::Interface.index()];
    assert_eq!(
        (buckets.java_sites, buckets.modelled, buckets.cha),
        (1, 1, 0)
    );
}

/// An empty propagation list discards the target set: the site has no callee and nothing flows.
#[test]
fn a_skip_leaves_the_site_with_no_callee() {
    let indexed = index_with(vec![generator(serde_json::json!({"propagation": []}))], &[]);
    assert!(indexed.callees().is_empty());
    assert!(indexed.id(SYNTHETIC).is_none(), "nothing was synthesized");
    assert!(!indexed.caller_propagates());
    let buckets = indexed.report.buckets[JavaDispatch::Interface.index()];
    assert_eq!((buckets.java_sites, buckets.skipped), (1, 1));
}

/// `resolve: inline` keeps the bodies reachable: no `call` rows, a `callee_info` row for the
/// engine to resolve from the receiver's allocation.
#[test]
fn inline_defers_the_site_to_hybrid_inlining() {
    let indexed = index_with(
        vec![generator(serde_json::json!({"resolve": "inline"}))],
        &[],
    );
    assert!(indexed.callees().is_empty());
    assert_eq!(indexed.facts.callee_info.len(), 1);
    let buckets = indexed.report.buckets[JavaDispatch::Interface.index()];
    assert_eq!(
        (
            buckets.java_sites,
            buckets.inlined,
            buckets.inlined_by_model
        ),
        (1, 1, 1)
    );
    assert!(
        !indexed.facts.callee_resolvents.is_empty(),
        "a deferred signature keeps the rows the engine resolves it with"
    );
}

/// A sink inside one of the targets refuses the model, so those bodies stay in the graph.
#[test]
fn a_sink_in_the_target_set_refuses_the_model() {
    let indexed = index_with(
        vec![generator(serde_json::json!({
            "propagation": [{"input": "Argument(1)", "output": "Return"}]
        }))],
        &[IMPL_B],
    );
    assert_eq!(
        indexed.callees(),
        vec![IMPL_A.to_string(), IMPL_B.to_string()]
    );
    assert_eq!(
        indexed
            .report
            .refused
            .get("Ljava/util/Iterator;->next(Ljava/lang/Object;)Ljava/lang/Object;")
            .map(String::as_str),
        Some(IMPL_B),
        "the refusal names the endpoint that caused it"
    );
}

/// `callee_resolvents` is what a deferred site is resolved through, so a run that defers
/// nothing needs none of it.
#[test]
fn nothing_deferred_means_no_resolvent_rows() {
    let indexed = index_with(Vec::new(), &[]);
    assert!(indexed.facts.callee_resolvents.is_empty());
}
