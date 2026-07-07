//! In-process driver for the DFSan dynamic/static taint comparison harness.
//!
//! [`analyze_c_flows`] runs CTADL's full source→sink taint query on a single C
//! program string plus a model file (the same `.json` model format used by the
//! CLI, e.g. `tests/c/xfer.json`), and returns the set of source→sink flows the
//! static analysis reports. The dynamic side (LLVM DFSan) produces a comparable
//! set at runtime; the comparator diffs the two.
//!
//! This mirrors the index+query pipeline in [`crate::cli::query`] and
//! [`crate::codegen::flowy::check`], but runs entirely in memory (no project
//! store on disk) so a harness can evaluate many programs quickly.

use ctadl_ir::{ProgramInfo, ssa};

use crate::cli::build_query_endpoints;
use crate::codegen::models::codegen_summary;
use crate::codegen::{CallResolutionStrategy, codegen_program};
use crate::error::Error;
use crate::facts::TaintDirection;
use crate::index_engine::source_info::IndexSourceInfo;
use crate::index_engine::{IndexConfig, IndexFacts, taint_index_with_config};
use crate::languages::tree_sitter::{parse_c_program, parse_cpp_program};
use crate::models::{ModelsBatch, try_load_models};
use crate::query_engine::{QueryFacts, taint_analysis};

/// A single source→sink flow that CTADL reports statically: taint of `label`
/// reaches the sink vertex (`sink_function` + `sink_path`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StaticFlow {
    pub sink_function: String,
    /// Dotted access path at the sink vertex, e.g. "" or ".inner".
    pub sink_path: String,
    pub label: String,
}

/// Parse `src` as **C**, index it, and run the taint query using the source/sink model
/// at `model_path`. Returns the deduplicated, sorted set of reported flows.
pub fn analyze_c_flows(
    src: &str,
    model_path: impl AsRef<std::path::Path>,
) -> Result<Vec<StaticFlow>, Error> {
    // 1. Parse C to IR.
    let (program, has_error, _dump) = parse_c_program(src)?;
    analyze_parsed_flows(program, has_error, model_path)
}

/// Parse `src` as **C++**, index it, and run the taint query using the source/sink model
/// at `model_path`. The C++ counterpart of [`analyze_c_flows`]: it differs only in the
/// frontend ([`parse_cpp_program`]); the index/query pipeline below is language-agnostic
/// and shared via [`analyze_parsed_flows`].
pub fn analyze_cpp_flows(
    src: &str,
    model_path: impl AsRef<std::path::Path>,
) -> Result<Vec<StaticFlow>, Error> {
    // 1. Parse C++ to IR.
    let (program, has_error, _dump) = parse_cpp_program(src)?;
    analyze_parsed_flows(program, has_error, model_path)
}

/// Shared index+query tail for [`analyze_c_flows`] / [`analyze_cpp_flows`]: takes an
/// already-parsed [`Program`] (plus the frontend's tree-sitter error flag) and the model
/// path, and returns the reported source→sink flows. Everything below the frontend is
/// language-agnostic, so both entry points funnel through here.
fn analyze_parsed_flows(
    program: ctadl_ir::Program,
    has_error: bool,
    model_path: impl AsRef<std::path::Path>,
) -> Result<Vec<StaticFlow>, Error> {
    if has_error {
        return Err(Error::TreeSitterParse(
            "tree-sitter reported a parse error in the input program".to_owned(),
        ));
    }
    let mut program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    program_info.program.verify()?;

    // 2. Load the source/sink model against the program (needs program_info
    //    before codegen consumes it). Split summary (consumed by indexing) from
    //    endpoints (used to build the query).
    let ModelsBatch {
        summary, endpoint, ..
    } = try_load_models(&program_info, model_path.as_ref())?;

    // 3. Index: SSA → codegen facts → fold in model summaries → datalog index.
    ssa::transform_program(&mut program_info.program, true);
    let mut index_facts = IndexFacts::default();
    let mut source_info = IndexSourceInfo::default();
    codegen_program(
        program_info,
        &mut index_facts,
        &mut source_info,
        CallResolutionStrategy::Mixed,
    );
    codegen_summary(summary, &mut index_facts, &mut source_info);
    let index_result = taint_index_with_config(
        index_facts.clone(),
        IndexConfig::default(),
        Some(&source_info.sites),
    );

    // 4. Build query endpoints (sources + sinks) from the model.
    let (endpoints, model_formals) =
        build_query_endpoints(&endpoint, &index_facts, &source_info.sites);
    let mut formal_params = index_facts.formal_param.clone();
    formal_params.extend(model_formals);

    {
        let n_src = endpoints
            .iter()
            .filter(|(e,)| e.direction == TaintDirection::Forward)
            .count();
        let n_sink = endpoints
            .iter()
            .filter(|(e,)| e.direction == TaintDirection::Backward)
            .count();
        log::debug!(
            "taint_compare: built {} endpoints ({} sources, {} sinks); functions: {:?}",
            endpoints.len(),
            n_src,
            n_sink,
            source_info
                .sites
                .functions()
                .map(|(_, f)| f.0.to_string())
                .collect::<Vec<_>>(),
        );
    }

    // 5. Run the source/sink taint query.
    let query_facts = QueryFacts {
        formal_param: formal_params,
        actual_param: index_facts.actual_param,
        call: index_facts.call,
        assign: index_result.assign_like,
        paths: index_result.paths,
        endpoints: endpoints.clone(),
    };
    let query_result = taint_analysis(query_facts, Some(&source_info.sites));

    // 6. For each sink endpoint, a flow is present when forward taint carrying
    //    the matching label reached the sink's vertex (same predicate as
    //    flowy::query_check_endpoints).
    let mut flows = Vec::new();
    for (ep,) in &endpoints {
        if ep.direction != TaintDirection::Backward {
            continue;
        }
        let present = query_result.taint.iter().any(|r| {
            r.0 == ep.infunc
                && r.4.label == ep.label
                && r.4.direction == TaintDirection::Forward
                && r.2 == ep.vertex.0
                && r.3 == ep.vertex.1
        });
        if present {
            let sink_function = source_info
                .sites
                .get_function(ep.infunc)
                .map(|f| f.0.to_string())
                .unwrap_or_else(|| format!("<func#{}>", ep.infunc.id));
            flows.push(StaticFlow {
                sink_function,
                sink_path: ep.vertex.1.to_dot_string(),
                label: ep.label.0.to_string(),
            });
        }
    }
    flows.sort();
    flows.dedup();
    Ok(flows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_c_path(name: &str) -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests");
        p.push("c");
        p.push(name);
        p
    }

    /// M1 acceptance: a direct source() → sink() flow is reported when driven
    /// through the in-process pipeline.
    #[test_log::test]
    fn direct_flow_is_reported() {
        let src = r#"
            int source() { return 0; }
            void sink(int x) { return; }
            int main() {
                int s = source();
                sink(s);
                return 0;
            }
        "#;
        let flows = analyze_c_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("direct flows: {flows:?}");
        // The model sink `sink(Argument(0))` is anchored at the *call site* in `main`
        // (not the `sink` callee): model endpoints on formals fan out to their callers'
        // call-arg vertices so flows that differ by call site stay distinct (see
        // QueryEndpoint::anchored_at_callsites). Every flow analyze_c_flows returns is a
        // sink flow by construction, so we assert the source label reached one — the same
        // predicate the harness itself keys on (ctadl-dynamic compare_program) — rather
        // than the anchoring function's name.
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a source->sink flow (label Test), got: {flows:?}"
        );
    }

    /// Negative control: a program with a source and a sink but no data path
    /// between them must report no flow.
    #[test_log::test]
    fn no_flow_when_disconnected() {
        let src = r#"
            int source() { return 0; }
            void sink(int x) { return; }
            int main() {
                int s = source();
                int x = 0;
                sink(x);
                return 0;
            }
        "#;
        let flows = analyze_c_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("disconnected flows: {flows:?}");
        assert!(flows.is_empty(), "expected no flow, got: {flows:?}");
    }

    /// M1 (FR-2) acceptance: the same direct source() → sink() flow is reported when the
    /// program is driven through the **C++** frontend (`analyze_cpp_flows`). Same model,
    /// same pipeline, only the parser differs.
    #[test_log::test]
    fn cpp_direct_flow_is_reported() {
        let src = r#"
            int source() { return 0; }
            void sink(int x) { return; }
            int main() {
                int s = source();
                sink(s);
                return 0;
            }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp direct flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a C++ source->sink flow (label Test), got: {flows:?}"
        );
    }

    /// Mirrors exactly what the `ctadl-dynamic` harness feeds the static side for a C++
    /// case: the FR-4 program's `extern "C"` prototypes + body, concatenated with the
    /// `extern "C"` inert marker *definitions* (`static_markers_cpp.cpp`). This pins that
    /// `parse_cpp_program` ingests `extern "C"` function definitions (wrapped in a
    /// tree-sitter `linkage_specification`) and still resolves `main`'s calls to them, so
    /// the source→sink flow is reported through the unmangled markers.
    #[test_log::test]
    fn cpp_extern_c_markers_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            int main() {
                int s = source();
                sink(s);
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp extern-c flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a C++ source->sink flow through extern \"C\" markers, got: {flows:?}"
        );
    }

    /// Negative control for the C++ frontend: a source and a sink with no data path
    /// between them must report no flow (parity with `no_flow_when_disconnected`).
    #[test_log::test]
    fn cpp_no_flow_when_disconnected() {
        let src = r#"
            int source() { return 0; }
            void sink(int x) { return; }
            int main() {
                int s = source();
                int x = 0;
                sink(x);
                return 0;
            }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp disconnected flows: {flows:?}");
        assert!(flows.is_empty(), "expected no flow, got: {flows:?}");
    }

    /// M3 (spec 003) acceptance, end to end: taint flows IN through a setter method and OUT
    /// through a getter method of a `struct`, so the source→sink path runs through two member
    /// functions. The frontend models each as `Box::m(this: ByRef, …)`, and the existing
    /// by-ref/return propagation carries the member across — mirrors the CPP_36 dynamic case.
    #[test_log::test]
    fn cpp_method_flow_through_struct_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                void set(int x) { v = x; }
                int get() { return v; }
            };
            int main() {
                Box b;
                b.set(source());
                sink(b.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp method flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a C++ source->sink flow through instance methods, got: {flows:?}"
        );
    }

    /// Negative control for instance methods (field sensitivity): `source()` taints member
    /// `a` through one method, but the sink reads a *different* member `b` through another.
    /// The member modeling is field-sensitive, so no spurious `a`→`b` flow is reported —
    /// mirrors the CPP_37 dynamic case (`s=none d=none`).
    #[test_log::test]
    fn cpp_method_field_sensitive_no_cross_member_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Pair {
                int a;
                int b;
                void set_a(int x) { a = x; }
                int get_b() { return b; }
            };
            int main() {
                Pair p;
                p.set_a(source());
                sink(p.get_b());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp field-sensitive method flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "expected no cross-member flow (field sensitivity), got: {flows:?}"
        );
    }

    /// M3 (spec 004, FR-2) acceptance, end to end: a non-const `T&` parameter is a write-back
    /// `ByRef` out-param. `set_ref(int& out, int v){ out = v; }` writes the tainted second
    /// argument through the reference, tainting the caller's `x` — mirrors CPP_38.
    #[test_log::test]
    fn cpp_ref_out_param_write_back_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            void set_ref(int& out, int v) { out = v; }
            int main() {
                int x = 0;
                set_ref(x, source());
                sink(x);
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp ref out-param flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a write-back flow through a non-const T& param, got: {flows:?}"
        );
    }

    /// M3 (spec 004, FR-3) acceptance, end to end: a `const T&` parameter is inbound-only
    /// (`ByVal`). `read(const int& r){ return r; }` flows the tainted argument out through the
    /// return — mirrors CPP_39.
    #[test_log::test]
    fn cpp_const_ref_inbound_through_return_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            int read(const int& r) { return r; }
            int main() {
                int x = source();
                sink(read(x));
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp const-ref inbound flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected an inbound flow through a const T& param's return, got: {flows:?}"
        );
    }

    /// Negative control for `const T&` (spec 004, FR-3): a `const T&` parameter is read-only,
    /// NOT a write-back out-param, so passing a clean variable by const-reference leaves it
    /// clean even though the program produces taint elsewhere — mirrors CPP_40 (`s=none`).
    #[test_log::test]
    fn cpp_const_ref_no_write_back() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            int read(const int& r) { return r; }
            int main() {
                int t = source();
                int x = 0;
                int y = read(x);
                sink(x);
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp const-ref no-write-back flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "const T& must not write the caller's clean variable back, got: {flows:?}"
        );
    }

    /// M3 (spec 004, FR-4) acceptance, end to end: a reference local `int& r = x` aliases its
    /// referent, so reading `r` reads `x`'s taint — mirrors CPP_41.
    #[test_log::test]
    fn cpp_reference_local_alias_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            int main() {
                int x = source();
                int& r = x;
                sink(r);
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp reference-local alias flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected flow through a reference local aliasing a tainted variable, got: {flows:?}"
        );
    }

    /// M3 (spec 005, FR-1) acceptance, end to end: a method defined *out of line*
    /// (`void Box::set(int){…}` / `int Box::get(){…}`) is discovered and lowered with the same
    /// implicit `this` as an inline method, so the setter→getter source→sink path flows —
    /// mirrors CPP_42.
    #[test_log::test]
    fn cpp_out_of_line_method_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            class Box {
              public:
                int v;
                void set(int x);
                int get();
            };
            void Box::set(int x) { v = x; }
            int Box::get() { return v; }
            int main() {
                Box b;
                b.set(source());
                sink(b.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp out-of-line method flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through out-of-line method definitions, got: {flows:?}"
        );
    }

    /// M3 (spec 005, FR-2) acceptance, end to end: explicit `this->v` reads/writes resolve to
    /// the same `this.v` member as the unqualified `v`, so the setter→getter path flows —
    /// mirrors CPP_43.
    #[test_log::test]
    fn cpp_this_arrow_member_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                void set(int x) { this->v = x; }
                int get() { return this->v; }
            };
            int main() {
                Box b;
                b.set(source());
                sink(b.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp this-> member flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through explicit this->member accesses, got: {flows:?}"
        );
    }

    /// M3 (spec 005, FR-3) acceptance, end to end: a pointer receiver `p->set(…)` / `p->get()`
    /// (where `Box* p = &b`) dispatches to `Box::set`/`Box::get` with the pointed-to object as
    /// the arg-0 receiver, so the member write propagates back and flows out — mirrors CPP_44.
    #[test_log::test]
    fn cpp_pointer_receiver_method_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                void set(int x) { v = x; }
                int get() { return v; }
            };
            int main() {
                Box b;
                Box* p = &b;
                p->set(source());
                sink(p->get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp pointer-receiver method flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through a pointer receiver, got: {flows:?}"
        );
    }

    /// M3 (spec 005, FR-3) acceptance, end to end: a reference receiver `r.set(…)` / `r.get()`
    /// (where `Box& r = b`) dispatches to the referenced object — mirrors CPP_45 and reuses the
    /// spec-004 reference-local aliasing.
    #[test_log::test]
    fn cpp_reference_receiver_method_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                void set(int x) { v = x; }
                int get() { return v; }
            };
            int main() {
                Box b;
                Box& r = b;
                r.set(source());
                sink(r.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp reference-receiver method flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through a reference receiver, got: {flows:?}"
        );
    }

    /// M3 (spec 005, FR-3) negative control: a pointer receiver taints one object (`a` via
    /// `p = &a`), but the sink reads a *different*, untainted object (`b`). The method model is
    /// per-object (the receiver is arg-0 by-ref), so no spurious `a`→`b` cross-taint is
    /// reported even though real taint exists — mirrors CPP_46 (`s=none d=none`).
    #[test_log::test]
    fn cpp_distinct_receiver_object_has_no_cross_taint() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                void set(int x) { v = x; }
                int get() { return v; }
            };
            int main() {
                Box a;
                Box b;
                Box* p = &a;
                p->set(source());
                sink(b.v);
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp distinct-object flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "tainting a different object than the one sunk must not flow, got: {flows:?}"
        );
    }

    /// M3 (spec 006, FR-1/FR-3) acceptance, end to end: a constructor argument flows into a
    /// member through the constructor body, then out through a getter. `Box b(source())`
    /// lowers to `DirectCall Box::Box(&b, source())`, so the constructor's `v = x` write
    /// lands in `b.v` — mirrors the CPP_47 dynamic case (`s=flow d=flow`).
    #[test_log::test]
    fn cpp_ctor_param_to_member_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                Box(int x) { v = x; }
                int get() { return v; }
            };
            int main() {
                Box b(source());
                sink(b.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp ctor param-to-member flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a source->sink flow through a constructor, got: {flows:?}"
        );
    }

    /// M3 (spec 006, FR-2) acceptance, end to end: a member-initializer list `: v(x)` carries
    /// the constructor argument's taint into the member, identically to a body write — mirrors
    /// the CPP_48 dynamic case (`s=flow d=flow`).
    #[test_log::test]
    fn cpp_ctor_init_list_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                Box(int x) : v(x) {}
                int get() { return v; }
            };
            int main() {
                Box b(source());
                sink(b.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp ctor init-list flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a source->sink flow through a member-initializer list, got: {flows:?}"
        );
    }

    /// M3 (spec 006, FR-1) acceptance, end to end: an out-of-line constructor definition
    /// `Box::Box(int){…}` carries the argument's taint into the member just like an inline
    /// one — mirrors the CPP_49 dynamic case (`s=flow d=flow`).
    #[test_log::test]
    fn cpp_ctor_out_of_line_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                Box(int x);
                int get() { return v; }
            };
            Box::Box(int x) { v = x; }
            int main() {
                Box b(source());
                sink(b.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp ctor out-of-line flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a source->sink flow through an out-of-line constructor, got: {flows:?}"
        );
    }

    /// Negative control for constructors (field sensitivity): the constructor taints member
    /// `a` from its argument and sets `b` to a constant; the sink reads the distinct member
    /// `b`. Real taint enters the program (`bx.a`), but no `a`→`b` flow is reported — mirrors
    /// the CPP_50 dynamic case (`s=none d=none`).
    #[test_log::test]
    fn cpp_ctor_distinct_member_no_cross_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int a;
                int b;
                Box(int x) { a = x; b = 0; }
                int getb() { return b; }
            };
            int main() {
                Box bx(source());
                sink(bx.getb());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp ctor distinct-member flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "constructor taint to member `a` must not reach a distinct member `b`, got: {flows:?}"
        );
    }

    /// M3 (spec 007, FR-2) acceptance, end to end: fluent method chaining. `setV`/`setW`
    /// return `Box&` and `return *this`, so `b.setV(source()).setW(0)` dispatches both setters
    /// on the same object `b`; `setV` taints `b.v`, which the terminal `b.getV()` reads back —
    /// mirrors the CPP_51 dynamic case.
    #[test_log::test]
    fn cpp_chain_setters_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                int w;
                Box& setV(int x) { v = x; return *this; }
                Box& setW(int x) { w = x; return *this; }
                int getV() { return v; }
            };
            int main() {
                Box b;
                b.setV(source()).setW(0);
                sink(b.getV());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp chain-setters flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through a chained setter, got: {flows:?}"
        );
    }

    /// M3 (spec 007, FR-2) acceptance, end to end: a terminal getter on the chained object.
    /// `b.setV(source()).getV()` — `setV`'s result aliases `b`, so the terminal `.getV()`
    /// reads the member `setV` just tainted — mirrors the CPP_52 dynamic case.
    #[test_log::test]
    fn cpp_chain_terminal_getter_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                Box& setV(int x) { v = x; return *this; }
                int getV() { return v; }
            };
            int main() {
                Box b;
                sink(b.setV(source()).getV());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp chain terminal-getter flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through a chained terminal getter, got: {flows:?}"
        );
    }

    /// M3 (spec 007, FR-1) acceptance, end to end: a reference return bound to a reference
    /// local. `Box& r = b.setV(source())` aliases `r` to `b` (not the returned temporary), so
    /// `r.getV()` reads the member `setV` tainted — mirrors the CPP_53 dynamic case.
    #[test_log::test]
    fn cpp_ref_return_to_ref_local_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                Box& setV(int x) { v = x; return *this; }
                int getV() { return v; }
            };
            int main() {
                Box b;
                Box& r = b.setV(source());
                sink(r.getV());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp ref-return-to-ref-local flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through a reference-returning method bound to a ref local, got: {flows:?}"
        );
    }

    /// M3 (spec 007, FR-2) acceptance, end to end: a member reference getter read from behaves
    /// like a value getter. `void setV` taints `b.v`; `int& get(){ return v; }` flows it out.
    #[test_log::test]
    fn cpp_member_reference_getter_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                void setV(int x) { v = x; }
                int& get() { return v; }
            };
            int main() {
                Box b;
                b.setV(source());
                sink(b.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp member-ref-getter flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through a member-reference getter, got: {flows:?}"
        );
    }

    /// Negative control for chaining (field sensitivity through a chain): the chain taints
    /// member `v` (from `source`) and sets `w = 0` on the same object; the sink reads the
    /// distinct member `w`. A field-sensitive model must not leak `v`'s taint into `w` just
    /// because both writes hit the same chained object — mirrors the CPP_54 dynamic case
    /// (`s=none d=none`).
    #[test_log::test]
    fn cpp_chain_distinct_member_no_cross_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                int w;
                Box& setV(int x) { v = x; return *this; }
                Box& setW(int x) { w = x; return *this; }
                int getW() { return w; }
            };
            int main() {
                Box b;
                b.setV(source()).setW(0);
                sink(b.getW());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp chain distinct-member flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "a chain tainting member `v` must not reach a distinct member `w`, got: {flows:?}"
        );
    }

    /// M4 (spec 008, FR-1) acceptance, end to end: a free function defined in a named
    /// namespace is lowered under its qualified IR name (`ns::id`), and the qualified call
    /// `ns::id(t)` resolves to it, so taint flows through the namespaced function — mirrors
    /// the CPP_55 dynamic case.
    #[test_log::test]
    fn cpp_namespaced_free_function_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            namespace ns {
                int id(int x) { return x; }
            }
            int main() {
                int t = source();
                sink(ns::id(t));
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp namespaced free-function flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through a namespaced free function ns::id, got: {flows:?}"
        );
    }

    /// M4 (spec 008, FR-2) acceptance, end to end: a class defined in a named namespace is
    /// registered under its qualified name (`ns::Box`); `ns::Box b;` records the local's type
    /// as `ns::Box` so `b.set(…)`/`b.get()` dispatch to `ns::Box::set`/`ns::Box::get` and taint
    /// flows in through the setter and out through the getter — mirrors the CPP_56 dynamic case.
    #[test_log::test]
    fn cpp_namespaced_class_method_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            namespace ns {
                struct Box {
                    int v;
                    void set(int x) { v = x; }
                    int get() { return v; }
                };
            }
            int main() {
                ns::Box b;
                b.set(source());
                sink(b.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp namespaced class-method flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through a namespaced class's methods, got: {flows:?}"
        );
    }

    /// M4 (spec 008, FR-2) acceptance, end to end: construction of a namespaced class at a
    /// declaration (`ns::Box b(source())`) lowers to `DirectCall ns::Box::ns::Box(&b, source())`,
    /// so the constructor's member write lands in `b`; `sink(b.get())` reads it back out —
    /// mirrors the CPP_57 dynamic case.
    #[test_log::test]
    fn cpp_namespaced_construction_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            namespace ns {
                struct Box {
                    int v;
                    Box(int x) { v = x; }
                    int get() { return v; }
                };
            }
            int main() {
                ns::Box b(source());
                sink(b.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp namespaced construction flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through a namespaced constructor, got: {flows:?}"
        );
    }

    /// Negative control (spec 008, FR-3): a namespace declares two distinct free functions,
    /// `keep(x){return x;}` and `drop(x){return 0;}`. The call `ns::drop(source())` must resolve
    /// to `ns::drop` (which discards its argument), NOT to `ns::keep` — so no taint reaches the
    /// sink. This pins that a qualified call resolves *precisely* to the same-named definition
    /// (no cross-resolution to a sibling) — mirrors the CPP_58 dynamic case (`s=none d=none`).
    #[test_log::test]
    fn cpp_namespaced_distinct_function_no_cross_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            namespace ns {
                int keep(int x) { return x; }
                int drop(int x) { return 0; }
            }
            int main() {
                sink(ns::drop(source()));
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp namespaced distinct-function flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "ns::drop discards its argument and must not resolve to ns::keep, got: {flows:?}"
        );
    }

    /// M4 (spec 009, FR-1) acceptance, end to end: an overloaded free function resolves by
    /// arity. `id` has an arity-1 overload (returns its arg) and an arity-2 overload; the 1-arg
    /// call `id(source())` resolves to the arity-1 overload, so taint reaches the sink — mirrors
    /// the CPP_59 dynamic case (`s=flow d=flow`).
    #[test_log::test]
    fn cpp_overload_free_function_arity1_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            int id(int a) { return a; }
            int id(int a, int b) { return b; }
            int main() {
                sink(id(source()));
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp overload free arity-1 flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through the arity-1 overload id#1, got: {flows:?}"
        );
    }

    /// M4 (spec 009, FR-2) acceptance, end to end: an overloaded *method* resolves by arity.
    /// `Box::f` has an arity-1 overload (returns its arg) and an arity-2 one; `b.f(source())`
    /// dispatches to the arity-1 method, so taint reaches the sink — mirrors the CPP_60 case.
    #[test_log::test]
    fn cpp_overload_method_arity1_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                int f(int a) { return a; }
                int f(int a, int b) { return b; }
            };
            int main() {
                Box b;
                sink(b.f(source()));
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp overload method arity-1 flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through the arity-1 method overload Box::f#1, got: {flows:?}"
        );
    }

    /// M4 (spec 009, FR-1) acceptance, end to end: the arity-2 overload is selected and taint
    /// follows *its* body. `id(0, source())` resolves to the arity-2 overload (returns its 2nd
    /// argument), so source (the 2nd arg) reaches the sink — mirrors the CPP_61 case.
    #[test_log::test]
    fn cpp_overload_arity2_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            int id(int a) { return a; }
            int id(int a, int b) { return b; }
            int main() {
                sink(id(0, source()));
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp overload arity-2 flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through the arity-2 overload id#2 (returns its 2nd arg), got: {flows:?}"
        );
    }

    /// Negative control (spec 009, FR-3): precise selection. `g` has an arity-1 overload that
    /// flows its arg and an arity-2 overload that drops its args (returns 0). `g(source(), 0)`
    /// must resolve to the arity-2 overload (which drops), NEVER cross-resolving to the flowing
    /// arity-1 sibling — so no taint reaches the sink. A merge (or wrong pick) would leak taint;
    /// mirrors the CPP_62 dynamic case (`s=none d=none`).
    #[test_log::test]
    fn cpp_overload_selects_dropping_no_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            int g(int a) { return a; }
            int g(int a, int b) { return 0; }
            int main() {
                sink(g(source(), 0));
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp overload selects-dropping flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "g(source(), 0) must resolve to the dropping arity-2 overload g#2, not the flowing g#1, got: {flows:?}"
        );
    }

    /// Spec 011 (FR-2), end to end: an **inherited** setter and getter carry taint through a
    /// derived object. `Derived` adds nothing of its own; `set`/`get` are defined in `Base`, so
    /// `d.set(source())` must dispatch to `Base::set` (with `d` as the by-ref receiver, writing
    /// `d.v`) and `d.get()` to `Base::get` (reading `d.v` back). Before this slice neither
    /// inherited method dispatched. Mirrors the CPP_67 dynamic case.
    #[test_log::test]
    fn cpp_inherited_method_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Base {
                int v;
                void set(int x) { v = x; }
                int get() { return v; }
            };
            struct Derived : Base {};
            int main() {
                Derived d;
                d.set(source());
                sink(d.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp inherited-method flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through inherited Base::set/Base::get on a Derived object, got: {flows:?}"
        );
    }

    /// Spec 011 (FR-1), end to end: a **derived method** touches an **inherited data member**.
    /// `store` is defined in `Derived` but writes `v`, a member of `Base`; member flattening
    /// makes `v` resolve to `this.v` inside `Derived::store` (the base subobject shares the
    /// derived object's field-named path). The inherited `Base::get` reads it back, so
    /// `d.store(source()); sink(d.get())` flows. Mirrors the CPP_68 dynamic case.
    #[test_log::test]
    fn cpp_inherited_member_in_derived_method_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Base {
                int v;
                int get() { return v; }
            };
            struct Derived : Base {
                void store(int x) { v = x; }
            };
            int main() {
                Derived d;
                d.store(source());
                sink(d.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp inherited-member-in-derived-method flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow: Derived::store writes inherited member v, Base::get reads it, got: {flows:?}"
        );
    }

    /// Spec 011 (FR-3), negative control: **non-virtual override** picks the derived method by
    /// static type. `Derived` redefines `get` to return a constant, hiding `Base::get`. A
    /// `Derived` static-type receiver's `d.get()` must dispatch to `Derived::get` (checked
    /// first), which drops the taint `d.set(source())` wrote into `d.v` — so no flow. If the
    /// walk found `Base::get` instead, this would leak. Mirrors the CPP_69 dynamic case.
    #[test_log::test]
    fn cpp_inherited_override_static_dispatch_no_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Base {
                int v;
                void set(int x) { v = x; }
                int get() { return v; }
            };
            struct Derived : Base {
                int get() { return 0; }
            };
            int main() {
                Derived d;
                d.set(source());
                sink(d.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp inherited-override flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "Derived::get (override) must be selected by static type and drop the taint, got: {flows:?}"
        );
    }

    /// Spec 011 (FR-4), negative control: field sensitivity through inheritance. An inherited
    /// setter taints the **inherited** member `v`; the sink reads a **distinct derived** member
    /// `w` (set to a constant). `d.v` and `d.w` are separate field-named paths, so no taint
    /// crosses despite real taint entering `d.v`. Mirrors the CPP_70 dynamic case.
    #[test_log::test]
    fn cpp_inherited_distinct_member_no_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Base {
                int v;
                void setv(int x) { v = x; }
            };
            struct Derived : Base {
                int w;
                int getw() { return w; }
            };
            int main() {
                Derived d;
                d.w = 0;
                d.setv(source());
                sink(d.getw());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp inherited-distinct-member flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "inherited setter taints d.v; sink reads distinct derived member d.w — no flow, got: {flows:?}"
        );
    }

    /// Spec 013 (base references), end-to-end positive — reference twin of CPP_75. A `virtual`
    /// `get()` overridden in `Derived` to flow, called through a **base reference** `Base& r = d`
    /// (`r.get()`, dot not arrow). CHA over `Base`'s subtree includes `Derived::get`, so taint
    /// flows end to end — static dispatch on the reference's `Base` static type would miss it.
    /// This locks in behavior 012's machinery already provides for a reference receiver.
    #[test_log::test]
    fn cpp_virtual_dispatch_through_base_reference_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Base {
                int v;
                void set(int x) { v = x; }
                virtual int get() { return 0; }
            };
            struct Derived : Base {
                int get() override { return v; }
            };
            int main() {
                Derived d;
                Base& r = d;
                r.set(source());
                sink(r.get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp virtual-through-reference flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a virtual-dispatch flow through a base reference (r.get() -> Derived::get), got: {flows:?}"
        );
    }

    /// Spec 013 (base references), end-to-end negative — reference twin of CPP_78. A `virtual`
    /// setter taints member `v`; the sink reads a distinct member `w` (set to 0). Field
    /// sensitivity holds through a virtual call on a reference, so no taint crosses `v` -> `w`.
    #[test_log::test]
    fn cpp_virtual_dispatch_through_base_reference_distinct_member_no_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Base {
                int v;
                int w;
                virtual void setv(int x) { v = x; }
                int getw() { return w; }
            };
            struct Derived : Base {
                void setv(int x) override { v = x; }
            };
            int main() {
                Derived d;
                d.w = 0;
                Base& r = d;
                r.setv(source());
                sink(r.getw());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp virtual-through-reference distinct-member flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "virtual setter taints v; sink reads distinct member w through a reference — no flow, got: {flows:?}"
        );
    }

    /// Spec 014 (heap objects), end to end — mirrors the CPP_79 dynamic case. `new Box()`
    /// allocates a heap object with no constructor; the pointer `p` aliases it, so `p->set`
    /// taints the object's member and `p->get()` reads it back. `delete p` is a taint no-op.
    #[test_log::test]
    fn cpp_new_heap_method_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                void set(int x) { v = x; }
                int get() { return v; }
            };
            int main() {
                Box* p = new Box();
                p->set(source());
                sink(p->get());
                delete p;
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp new heap-method flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a source->sink flow through a heap object's setter/getter, got: {flows:?}"
        );
    }

    /// Spec 014 (heap objects), end to end — mirrors the CPP_80 dynamic case. `new Box(source())`
    /// runs the constructor on the synthetic heap object, so the argument's taint lands in the
    /// member; the pointer aliases the object and `p->get()` reads it back out.
    #[test_log::test]
    fn cpp_new_ctor_arg_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                Box(int x) { v = x; }
                int get() { return v; }
            };
            int main() {
                Box* p = new Box(source());
                sink(p->get());
                delete p;
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp new ctor-arg flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a source->sink flow through a heap constructor argument, got: {flows:?}"
        );
    }

    /// Spec 014 (heap objects, FR-2), end to end — mirrors the CPP_81 dynamic case. A `Derived`
    /// is heap-allocated through a `Base*` (`Base* p = new Derived()`), so `p->get()` is a
    /// VIRTUAL call: CHA over the declared static type `Base`'s subtree captures the overriding
    /// `Derived::get` (which flows), the sound target the runtime actually runs.
    #[test_log::test]
    fn cpp_new_virtual_dispatch_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Base {
                int v;
                void set(int x) { v = x; }
                virtual int get() { return 0; }
            };
            struct Derived : Base {
                int get() override { return v; }
            };
            int main() {
                Base* p = new Derived();
                p->set(source());
                sink(p->get());
                delete p;
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp new virtual-dispatch flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a virtual-dispatch flow through a heap object (p->get() -> Derived::get), got: {flows:?}"
        );
    }

    /// Spec 014 (heap objects), end-to-end negative — mirrors the CPP_82 dynamic case. The
    /// setter taints member `a` on a heap object; the sink reads a distinct member `b` (0).
    /// Field sensitivity holds through the heap alias, so no `a` -> `b` flow is reported.
    #[test_log::test]
    fn cpp_new_distinct_member_no_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int a;
                int b;
                void seta(int x) { a = x; }
                int getb() { return b; }
            };
            int main() {
                Box* p = new Box();
                p->b = 0;
                p->seta(source());
                sink(p->getb());
                delete p;
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp new distinct-member flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "heap setter taints member `a`; sink reads distinct member `b` — no flow, got: {flows:?}"
        );
    }

    /// Spec 014 (FR-3): a program containing `delete p;` parses and lowers without a
    /// frontend error, and `delete` moves no taint (its presence changes no flow).
    #[test_log::test]
    fn cpp_delete_is_taint_no_op() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Box {
                int v;
                void set(int x) { v = x; }
                int get() { return v; }
            };
            int main() {
                Box* p = new Box();
                p->set(source());
                int r = p->get();
                delete p;
                sink(r);
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        // The value was read into `r` before `delete p`, so the flow still holds; the point is
        // that `delete p;` lowered without error (a `frontend-error` would panic `analyze`).
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp delete no-op flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "delete must parse/lower as a taint no-op without disturbing the pre-delete flow, got: {flows:?}"
        );
    }

    /// Spec 016 (virtual destructors) acceptance, end to end — the soundness fix, mirrors the
    /// CPP_87 dynamic case. A `virtual ~Base` overridden by `~Derived(){ sink(v); }`; a `Derived`
    /// heap object through a `Base*` is tainted (`p->set(source())`) then `delete p`. `delete`
    /// runs the destructor, and since it is virtual the CHA target set includes `Derived::~Derived`
    /// — which sinks the tainted member. Before this slice `delete` was a no-op, so CTADL reported
    /// `s=none` while DFSan observed the flow (a soundness-disagree); now the flow is reported.
    #[test_log::test]
    fn cpp_virtual_destructor_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Base {
                int v;
                void set(int x) { v = x; }
                virtual ~Base() {}
            };
            struct Derived : Base {
                ~Derived() { sink(v); }
            };
            int main() {
                Base* p = new Derived();
                p->set(source());
                delete p;
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp virtual-destructor flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "delete of a Base* to a Derived must run ~Derived (CHA), sinking the tainted member — a flow, got: {flows:?}"
        );
    }

    /// Spec 016 (FR-3), end-to-end negative — mirrors the CPP_90 dynamic case. The destructor sinks
    /// a member that was never tainted (`w`, untouched), while a distinct member `v` is tainted.
    /// Field sensitivity holds through the destructor call: no `v` -> `w` flow, so no `source` ->
    /// `sink` flow — the CHA destructor edge invents no spurious taint.
    #[test_log::test]
    fn cpp_virtual_destructor_untainted_member_no_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Base {
                int v;
                int w;
                void set(int x) { v = x; }
                virtual ~Base() {}
            };
            struct Derived : Base {
                ~Derived() { sink(w); }
            };
            int main() {
                Base* p = new Derived();
                p->set(source());
                delete p;
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp virtual-destructor untainted-member flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "the destructor sinks an untainted member `w`; the tainted member is `v` — no flow, got: {flows:?}"
        );
    }

    /// M8 (spec 015, FR-1/FR-3) acceptance, end to end: a `static` data member is a class-scoped
    /// GLOBAL, not a per-object field. A static setter writes it and a static getter reads it
    /// across separate calls (no object), so the source→sink path runs through the global
    /// `Counter::total`. Modeling the member as a global and the static methods without an
    /// implicit `this` closes the prior soundness gap — mirrors the CPP_83 dynamic case.
    #[test_log::test]
    fn cpp_static_member_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Counter {
                static int total;
                static void add(int x) { total = x; }
                static int get() { return total; }
            };
            int Counter::total = 0;
            int main() {
                Counter::add(source());
                sink(Counter::get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp static member flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow through the static data member global Counter::total, got: {flows:?}"
        );
    }

    /// M8 (spec 015, FR-2/FR-3) acceptance, end to end: a `static` member function has NO implicit
    /// `this`, so `C::identity(source())` passes the source as the method's first (and only)
    /// parameter, which is returned straight to the sink — mirrors the CPP_84 dynamic case.
    #[test_log::test]
    fn cpp_static_method_arg_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct C {
                static int identity(int x) { return x; }
            };
            int main() {
                sink(C::identity(source()));
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp static method arg flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected the static method argument to reach the return (no receiver shift), got: {flows:?}"
        );
    }

    /// M8 (spec 015, FR-3) acceptance, end to end: a static data member is one shared global
    /// regardless of how it is accessed. A NON-static method (with an implicit `this`) writes it
    /// and a `static` getter reads it — both bind to the same global `Counter::total`, so the
    /// taint flows across the two methods — mirrors the CPP_85 dynamic case.
    #[test_log::test]
    fn cpp_static_member_cross_method_flow_is_reported() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Counter {
                static int total;
                void bump(int x) { total = x; }
                static int get() { return total; }
            };
            int Counter::total = 0;
            int main() {
                Counter c;
                c.bump(source());
                sink(Counter::get());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp static cross-method flows: {flows:?}");
        assert!(
            flows.iter().any(|f| f.label == "Test"),
            "expected a flow from the instance method's static-member write to the static getter's read, got: {flows:?}"
        );
    }

    /// Negative control for statics (field sensitivity among class-scoped globals): a static
    /// setter taints member `a`, but the sink reads a *distinct* static member `b` (always 0).
    /// Each static member is its own global (`Counter::a` vs `Counter::b`), so no spurious
    /// `a`→`b` flow is reported — mirrors the CPP_86 dynamic case (`s=none d=none`).
    #[test_log::test]
    fn cpp_static_distinct_member_no_flow() {
        let src = r#"
            extern "C" int source();
            extern "C" void sink(int);
            struct Counter {
                static int a;
                static int b;
                static void seta(int x) { a = x; }
                static int getb() { return b; }
            };
            int Counter::a = 0;
            int Counter::b = 0;
            int main() {
                Counter::seta(source());
                sink(Counter::getb());
                return 0;
            }
            extern "C" int source() { return 0; }
            extern "C" void sink(int x) { return; }
        "#;
        let flows = analyze_cpp_flows(src, test_c_path("markers.json")).expect("analyze");
        log::info!("cpp static distinct-member flows: {flows:?}");
        assert!(
            flows.is_empty(),
            "expected no cross-member flow among distinct statics (field sensitivity), got: {flows:?}"
        );
    }
}
