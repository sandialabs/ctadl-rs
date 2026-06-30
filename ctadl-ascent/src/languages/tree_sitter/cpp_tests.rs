//! C++ frontend unit tests (Milestone 1).
//!
//! Mirrors `tree_sitter/tests.rs`, but drives `parse_cpp_program` via
//! `program_from_cpp_string`. Coverage is deliberately tiny — the FR-4 reference program
//! and the handful of constructs it needs (value-returning function, local declaration
//! with a call initializer, call statement, `return`). Every other C++ construct is
//! expected to error for now; that is the Milestone 2 backlog, not a defect.
//!
//! The end-to-end static source→sink flow (through the markers model) is asserted in
//! `taint_compare::tests::cpp_direct_flow_is_reported`; here we assert the *frontend's*
//! structural and dataflow output directly.

use crate::languages::tree_sitter::test_utils::*;

#[test_log::test]
fn cpp_simple_function() {
    // The smallest possible C++ function. Proves the seam: parse_cpp_program drives the
    // shared lowering, which produces exactly one basic block for an empty body (parity
    // with the C frontend's `simple_function`).
    let src = r"
        void simple() {}
    ";
    let prog = program_from_cpp_string(src).0;
    check_block_count(&prog, 1);
}

#[test_log::test]
fn cpp_returns_param() {
    // A C++ transfer function: its parameter flows straight to the return. Asserting the
    // param→return summary proves the C++ frontend's lowering feeds the language-agnostic
    // dataflow pipeline correctly (parity with the C frontend's param-return tests).
    let src = r"
        int xfer(int x) {
            return x;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_return_arity(&prog, "xfer", 1);
    let (summary, _si) = get_summary(prog).unwrap();
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn cpp_direct_assign_first_program() {
    // The FR-4 reference program: source()'s return is assigned to a local, then passed
    // straight to sink(). source/sink are prototypes (the shims supply bodies), so only
    // `main` is a definition. We assert the lowered shape — `main` directly calls both
    // source and sink, and the value read by sink is the local `s` that received source's
    // return — which is exactly the source→sink data path the dynamic case observes.
    let src = r"
        int source();
        void sink(int);
        int main() {
            int s = source();
            sink(s);
            return 0;
        }
    ";
    let (prog, dump) = program_from_cpp_string(src);
    log::info!("FR-4 C++ IR:\n{dump}");
    // Only `main` is defined; the prototypes are not function definitions.
    check_function_count(&prog, 1);
    check_has_direct_call(&prog, "main", "source");
    check_direct_call(&prog, "main", "sink", ["s"]);
    // The program indexes cleanly through the shared pipeline (verify + SSA + codegen).
    get_summary(prog).expect("FR-4 program indexes without error");
}
