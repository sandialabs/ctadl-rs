//! C++ frontend unit tests (Milestones 1–2).
//!
//! Mirrors `tree_sitter/tests.rs`, but drives `parse_cpp_program` via
//! `program_from_cpp_string`. The Milestone 1 tests pin the seam (the FR-4 reference
//! program and the handful of constructs it needs). The Milestone 2 (`c-frontend-parity`)
//! tests below mirror the C frontend's control-flow, array, and struct-field tier tests
//! one-for-one and assert the **same** block/successor/dataflow shape the C frontend
//! produces — proving the C++ frontend lowers the shared C subset identically. Several of
//! them deliberately exercise the two grammar-shape divergences bridged by `cpp::CPP_HOOKS`:
//! `if`/`while`/`switch` conditions (tree-sitter-cpp's `condition_clause`) and array
//! subscripts (tree-sitter-cpp's `subscript_argument_list`).
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

// ---------------------------------------------------------------------------
// Milestone 2 — C-frontend parity. Each test below mirrors the same-named C test
// in `tests.rs`, asserting the C++ frontend produces the identical lowered shape.
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_simple_else() {
    // Parity with C `simple_else`: an `if/else` is a four-block diamond. The C++ `if`
    // condition is a `condition_clause` (vs C's `parenthesized_expression`); the
    // `condition_expr` hook unwraps it so the shared walker lowers it identically.
    let src = r"
        int simple_else() {
            int x = 55;
            int v_if;
            if(x){
                v_if = x;
            } else {
                int v_else = x;
            }
            return 0;
        }
    ";
    let (program, _dump) = program_from_cpp_string(src);
    check_assign_or_update(&program, "v_if", ["x"], Some(1));
    check_assign_or_update(&program, "v_else", ["x"], Some(3));
    check_successors(&program, 0, &[1, 3]);
    check_successors(&program, 1, &[2]);
    check_successors(&program, 3, &[2]);
    check_successors(&program, 2, &[]);
}

#[test_log::test]
fn cpp_simple_elif() {
    // Parity with C `simple_elif`: `else if` desugars to a nested `if`, two condition
    // blocks each branching to exactly their two arms. Exercises the `condition_clause`
    // hook on both the outer and the nested condition.
    let src = r"
        int simple_elif() {
            int x = 5;
            int v_if;
            int v_elif;
            int v_else;
            if(x){
                v_if = x;
            }
            else if(!z) {
                v_elif = x;
            } else {
                v_else = x;
            }
            return 0;
        }
    ";
    let (program, _dump) = program_from_cpp_string(src);
    check_assign_or_update(&program, "v_if", ["x"], Some(1));
    check_assign_or_update(&program, "v_elif", ["x"], Some(4));
    check_assign_or_update(&program, "v_else", ["x"], Some(6));
    check_successors(&program, 0, &[1, 3]);
    check_successors(&program, 1, &[2]);
    check_successors(&program, 3, &[4, 6]);
    check_successors(&program, 4, &[5]);
    check_successors(&program, 5, &[2]);
    check_successors(&program, 6, &[5]);
    check_successors(&program, 2, &[]);
}

#[test_log::test]
fn cpp_if_fallthrough_cfg() {
    // Parity with C `if_fallthrough_cfg`: an `if` with no early return falls through to a
    // shared continuation. Three blocks; the condition branches to body or continuation.
    let src = r"
        int f(int x, int y) {
            if(x) {
                x = x + 21;
            }
            return y;
        }";
    let prog = program_from_cpp_string(src).0;
    check_block_count(&prog, 3);
    check_successors(&prog, 0, &[1, 2]);
    check_successors(&prog, 1, &[2]);
    check_successors(&prog, 2, &[]);
}

#[test_log::test]
fn cpp_while_loop_cfg() {
    // Parity with C `while_loop_cfg`: the loop header branches to body or exit, the body
    // back-edges to the header. The `while` condition is a `condition_clause` in C++.
    let src = r"
        int f(Field my_parm, int parB) {
            int b = 2;
            int x = 5;
            while(my_parm->x = parB) {
                x = b;
            }
            int y = x;
            return y;
        }";
    let prog = program_from_cpp_string(src).0;
    check_block_count(&prog, 4);
    check_successors(&prog, 0, &[1]);
    check_successors(&prog, 1, &[2, 3]);
    check_successors(&prog, 3, &[1]);
    check_successors(&prog, 2, &[]);
    check_assign_or_update(&prog, "x", ["b"], Some(3));
    check_assign_or_update(&prog, "y", ["x"], Some(2));
}

#[test_log::test]
fn cpp_while_with_nested_if_cfg() {
    // Parity with C `while_with_nested_if_cfg`: a `while` body containing an `if`. The
    // if-join back-edges to the loop condition, never to the entry block. Exercises the
    // condition hook on both the `while` and the nested `if`.
    let src = r"
        int f(int y, int z) {
            int x = 5;
            while(x < 50) {
                x = z;
                if(y == z)
                    x = y;
                x = x + z;
            }
            return x;
        }";
    let prog = program_from_cpp_string(src).0;
    check_block_count(&prog, 6);
    check_successors(&prog, 0, &[1]);
    check_successors(&prog, 1, &[2, 3]);
    check_successors(&prog, 2, &[]);
    check_successors(&prog, 3, &[4, 5]);
    check_successors(&prog, 4, &[5]);
    check_successors(&prog, 5, &[1]);
}

#[test_log::test]
fn cpp_do_while_cfg() {
    // Parity with C `do_while_cfg`: the body runs before the condition. A do-while's
    // condition is *not* a `condition_clause` (it is parenthesized like C), so the C hook
    // applies — this test guards that the C++ frontend still lowers it correctly.
    let src = r"
        int f() {
            int b = 2;
            int x = 5;
            do {
                x = b;
            } while(b = b + x);
            int y = x;
            return y;
        }";
    let prog = program_from_cpp_string(src).0;
    check_block_count(&prog, 4);
    check_successors(&prog, 0, &[1]);
    check_successors(&prog, 1, &[2]);
    check_successors(&prog, 2, &[1, 3]);
    check_successors(&prog, 3, &[]);
    check_assign_or_update(&prog, "x", ["b"], Some(1));
}

#[test_log::test]
fn cpp_do_while_body_flows() {
    // Parity with C `do_while_body_flows`: a do-while runs its body at least once, so the
    // body's `x = p` carries param 0 to the `return x` regardless of the condition.
    let src = r"
        int f(int p) {
            int x = 0;
            do {
                x = p;
            } while(x);
            return x;
        }";
    let (s, _si) = get_summary(program_from_cpp_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn cpp_for_loop_flows() {
    // A `for` loop whose body assigns a param to a local that is then returned. The for
    // condition is a bare expression (not a `condition_clause`), so the C hook applies;
    // this pins that the C++ frontend lowers the for-loop body so taint survives it.
    let src = r"
        int f(int p) {
            int x = 0;
            for (int i = 0; i < 1; i = i + 1) {
                x = p;
            }
            return x;
        }";
    let (s, _si) = get_summary(program_from_cpp_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn cpp_switch_case_flows() {
    // A `switch` whose scrutinee is a `condition_clause` in C++. Taint assigned in a case
    // arm (`x = p`) reaches the `return x` (the switch is lowered path-insensitively, so
    // every arm is reachable). Exercises the `condition_expr` hook in `walk_switch`.
    let src = r"
        int f(int p) {
            int x = 0;
            switch (p) {
                case 1:
                    x = p;
                    break;
                default:
                    x = 0;
            }
            return x;
        }";
    let (s, _si) = get_summary(program_from_cpp_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn cpp_subscript_access_paths() {
    // Parity with C `subscript_access_paths`: a constant array subscript read and written
    // becomes a `.[N]` access-path segment. In C++ the index lives under a
    // `subscript_argument_list`; the `subscript_index` hook recovers it so the lowered
    // access path matches the C frontend's exactly.
    let src = r"
        int brackets_simple(Donkey v, Burro* b, int x, int y) {
            int f = 1;
            x = f[3];
            f[4] = x;
        }";
    let prog = program_from_cpp_string(src).0;
    check_assign_or_update(&prog, "@p2", ["f.[3]"], None);
    check_assign_or_update(&prog, "f.[4]", ["@p2"], None);
}

#[test_log::test]
fn cpp_field_write_flows() {
    // Parity with C `field_write_flows`: writes into struct fields (direct, deep-path, and
    // blended right-hand sides) summarize as flows into the formal's field with no leaked
    // temporaries. Struct field access (`field_expression`) is identical across the two
    // grammars, so this needs no hook — it confirms the shared lowering is reused as-is.
    let src = r"
        int field_access(Donkey v, Burro* b, int x, int y) {
            v.f2 = x;
            v.f2.nf1.y = b->f2.f3->f4;
            v.f5 = b->fa + b->fb;
            v.f3 = x + y + z;
            v.f1 = b.xyz;
            return v.f1;
        }";
    let (s, _si) = get_summary(program_from_cpp_string(src).0).unwrap();
    check_flow(&s, 2, "", 0, "f2");
    check_flow(&s, 1, "f2.f3.f4", 0, "f2.nf1.y");
    check_flow(&s, 1, "fa", 0, "f5");
    check_flow(&s, 1, "fb", 0, "f5");
    check_flow(&s, 2, "", 0, "f3");
    check_flow(&s, 3, "", 0, "f3");
    check_flow(&s, 1, "xyz", 0, "f1");
    check_returns_param(&s, 0, "f1");
    check_returns_param(&s, 1, "xyz");
}

#[test_log::test]
fn cpp_field_assignment_is_update() {
    // Parity with C `field_assignment_is_update`: storing into a struct field lowers to a
    // functional `update` on the base value (`@p0` updated at `.f2`), not a plain assign.
    let src = r"
        int f(Donkey v, int x) {
            v.f2 = x;
        }";
    let prog = program_from_cpp_string(src).0;
    check_assign_or_update(&prog, "@p0.f2", ["@p1"], None);
}
