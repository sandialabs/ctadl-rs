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
use ctadl_ir::ParameterType;

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

// ---------------------------------------------------------------------------
// Milestone 3 (spec 003) — C++ instance methods. These assert the *frontend's* lowering:
// method discovery (the implicit `this` shape), member resolution (`this.<member>`), and
// `recv.method(…)` dispatch. The end-to-end source→sink flow through methods is asserted in
// `taint_compare::tests::cpp_method_flow_through_struct_is_reported` and validated against
// DFSan by the CPP_34/35/36 dynamic cases.
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_method_discovery_this_param_shape() {
    // FR-1: each inline member function lowers to a `Class::method` function whose parameter 0
    // is an implicit `this` (`ByRef`), with the declared params following. A `void` setter is
    // arity 0; a value-returning getter is arity 1.
    let src = r"
        struct Box {
            int v;
            void set(int x) { v = x; }
            int get() { return v; }
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::set",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_return_arity(&prog, "Box::set", 0);
    check_func_params(&prog, "Box::get", &[ParameterType::ByRef]);
    check_return_arity(&prog, "Box::get", 1);
}

#[test_log::test]
fn cpp_method_member_resolves_to_this() {
    // FR-2: inside a method body an unqualified data-member name resolves to `this.<member>`,
    // i.e. an access path rooted at the implicit `this` (parameter 0). The setter's `v = x`
    // becomes an update of `@p0.v`; the getter's `return v` returns `@p0.v`.
    let src = r"
        struct Box {
            int v;
            void set(int x) { v = x; }
            int get() { return v; }
        };";
    let prog = program_from_cpp_string(src).0;
    // `v = x` => update `this.v` (@p0.v) from the declared param `x` (@p1).
    check_func_assign_or_update(&prog, "Box::set", "@p0.v", ["@p1"]);
    // `return v` => return `this.v` (@p0.v).
    check_func_returns_path(&prog, "Box::get", "@p0.v");
}

#[test_log::test]
fn cpp_method_member_shadowed_by_param() {
    // FR-2 (shadowing): a parameter named like a member shadows it — `v` here is the param, so
    // `v = x` is a plain local/param assign, NOT a write to `this.v`. Guards that member
    // resolution defers to in-scope locals/params (it only fires when the name is unbound).
    let src = r"
        struct Box {
            int v;
            void set(int v) { v = v; }
        };";
    let prog = program_from_cpp_string(src).0;
    // The param `v` is `@p1` (`this` is `@p0`), so `v = v` is the self-assign `@p1 := @p1`.
    // Member resolution does not fire (the name is bound), so no `@p0.v` update is produced.
    check_func_assign_or_update(&prog, "Box::set", "@p1", ["@p1"]);
}

#[test_log::test]
fn cpp_method_call_is_direct_with_receiver_arg0() {
    // FR-3: `recv.method(args)` lowers to a DIRECT call to `Class::method` with `recv` prepended
    // as the arg-0 receiver (not an indirect call through a nonexistent field-pointer). The
    // getter result feeds the sink; the setter receives the source's value as arg 1.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    // Both calls resolve to the qualified method as direct calls.
    check_has_direct_call(&prog, "main", "Box::set");
    check_has_direct_call(&prog, "main", "Box::get");
    // The receiver `b` is arg 0 of each (set's arg 1 is the incidental source() temp).
    check_direct_call_arg0(&prog, "main", "Box::set", "b");
    check_direct_call(&prog, "main", "Box::get", ["b"]);
}

// ---------------------------------------------------------------------------
// Milestone 3 (spec 004) — C++ lvalue references (`T&`) and `const`.
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_ref_param_is_byref_write_back() {
    // A non-const reference parameter `int& out` is a write-back `ByRef` formal (param 0),
    // exactly like a pointer out-param; the trailing value param `v` is `ByVal` (param 1).
    // The body `out = v` lowers to an assignment of the formal `@p0` from `@p1`, so the
    // existing out-param propagation carries `v`'s taint back to the caller's argument.
    let src = r"
        void set_ref(int& out, int v) {
            out = v;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "set_ref",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_assign_or_update(&prog, "set_ref", "@p0", ["@p1"]);
}

#[test_log::test]
fn cpp_const_ref_param_is_byval_inbound() {
    // A `const int& r` parameter is read-only: model it `ByVal` (inbound only, no write-back),
    // and the referent's value flows out through the return (`return r` => `return @p0`).
    let src = r"
        int read(const int& r) {
            return r;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_func_params(&prog, "read", &[ParameterType::ByVal]);
    check_func_returns_path(&prog, "read", "@p0");
}

#[test_log::test]
fn cpp_const_is_stripped_from_value_declaration() {
    // `const` is a type qualifier that never blocks taint: a `const T x = …` local must lower
    // identically to its non-const form. We assert the two lowerings are byte-for-byte equal.
    let const_src = r"
        int source();
        void sink(int);
        int main() {
            const int x = source();
            sink(x);
            return 0;
        }
    ";
    let plain_src = r"
        int source();
        void sink(int);
        int main() {
            int x = source();
            sink(x);
            return 0;
        }
    ";
    let const_dump = program_from_cpp_string(const_src).1;
    let plain_dump = program_from_cpp_string(plain_src).1;
    assert_eq!(
        const_dump, plain_dump,
        "`const int x` must lower identically to `int x`"
    );
}

#[test_log::test]
fn cpp_const_member_function_parses_and_lowers() {
    // A `const` member function (`int get() const`) parses without error and is discovered
    // as a method: the trailing `const` qualifier on the function declarator is inert for
    // flow, so `get` still lowers to `S::get(this: ByRef)` returning the member `this.v`.
    let src = r"
        struct S {
            int v;
            int get() const { return v; }
        };
    ";
    let prog = program_from_cpp_string(src).0;
    check_func_params(&prog, "S::get", &[ParameterType::ByRef]);
    check_func_returns_path(&prog, "S::get", "@p0.v");
}

#[test_log::test]
fn cpp_reference_local_aliases_referent() {
    // A reference local `int& r = x` aliases `x` rather than copying it, so a use of `r`
    // resolves to `x`'s access path: `sink(r)` lowers to a direct call to `sink` with the
    // argument `x` (not a separate local `r`).
    let src = r"
        int source();
        void sink(int);
        int main() {
            int x = source();
            int& r = x;
            sink(r);
            return 0;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "main", "sink", ["x"]);
}

// ---------------------------------------------------------------------------
// Milestone 3 (spec 005) — methods slice 2: out-of-line definitions, explicit `this->`,
// and pointer/reference receivers. These assert the *frontend's* lowering; the end-to-end
// source→sink flows are asserted in `taint_compare::tests::cpp_{out_of_line,this_arrow,
// pointer_receiver,reference_receiver}_*` and validated against DFSan by CPP_42..CPP_46.
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_out_of_line_method_discovered_and_lowered() {
    // FR-1: a method defined out of line (`void Box::set(int){…}`, declarator is a
    // `qualified_identifier`) is discovered and lowered with the same implicit `this` (`ByRef`,
    // param 0) and `this.<member>` resolution as an inline method — the body's `v = x` becomes
    // an update of `@p0.v`, and the prototype-only class still resolves `b.set`/`b.get` calls.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::set",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_return_arity(&prog, "Box::set", 0);
    check_func_assign_or_update(&prog, "Box::set", "@p0.v", ["@p1"]);
    check_func_returns_path(&prog, "Box::get", "@p0.v");
    // The calls in `main` dispatch to the qualified methods even though the bodies are
    // out of line (discovery registered them before `main` was lowered).
    check_direct_call_arg0(&prog, "main", "Box::set", "b");
    check_direct_call(&prog, "main", "Box::get", ["b"]);
}

#[test_log::test]
fn cpp_this_arrow_resolves_to_member() {
    // FR-2: an explicit `this->v` resolves to the same `this.v` (`@p0.v`) access path as the
    // unqualified member `v` — `this->v = x` is an update of `@p0.v`; `return this->v` returns
    // `@p0.v`.
    let src = r"
        struct Box {
            int v;
            void set(int x) { this->v = x; }
            int get() { return this->v; }
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_assign_or_update(&prog, "Box::set", "@p0.v", ["@p1"]);
    check_func_returns_path(&prog, "Box::get", "@p0.v");
}

#[test_log::test]
fn cpp_pointer_receiver_dispatches_to_method() {
    // FR-3: `p->set(args)` / `p->get()` (where `Box* p = &b`) dispatch as DIRECT calls to
    // `Box::set`/`Box::get` with the pointer `p` as the arg-0 receiver — by-ref param 0 carries
    // the member write back to `p`, and `p->get()` reads it out.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "main", "Box::set");
    check_direct_call_arg0(&prog, "main", "Box::set", "p");
    check_direct_call(&prog, "main", "Box::get", ["p"]);
}

#[test_log::test]
fn cpp_reference_receiver_dispatches_to_referent() {
    // FR-3: `r.set(args)` / `r.get()` (where `Box& r = b`) dispatch to `Box::set`/`Box::get`
    // with the *referent* `b` as the arg-0 receiver (the reference local aliases `b`, reusing
    // spec 004), not a separate local `r`.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "main", "Box::set");
    check_direct_call_arg0(&prog, "main", "Box::set", "b");
    check_direct_call(&prog, "main", "Box::get", ["b"]);
}

// ---------------------------------------------------------------------------
// Milestone 3 (spec 006) — C++ constructors. A constructor is modeled as the function
// `Class::Class` with an implicit `this` (`ByRef`) param 0; construction at a declaration
// is a `DirectCall Class::Class` with the new object as the arg-0 receiver. These assert
// the *frontend's* lowering; the end-to-end source→sink flows are asserted in
// `taint_compare::tests::cpp_ctor_*` and validated against DFSan by CPP_47..CPP_50.
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_ctor_discovery_this_param_shape() {
    // FR-1: an inline constructor `Box(int x){…}` lowers to the function `Box::Box` whose
    // parameter 0 is an implicit `this` (`ByRef`) with the declared params following; it
    // returns no value (arity 0). The body's `v = x` writes the member through `this`
    // (`@p0.v := @p1`), exactly like a setter.
    let src = r"
        struct Box {
            int v;
            Box(int x) { v = x; }
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::Box",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_return_arity(&prog, "Box::Box", 0);
    check_func_assign_or_update(&prog, "Box::Box", "@p0.v", ["@p1"]);
}

#[test_log::test]
fn cpp_ctor_init_list_writes_member() {
    // FR-2: a member-initializer list `Box(int x) : v(x) {}` lowers each `member(expr)` as a
    // write `this.member = expr` (`@p0.v := @p1`) before the body — identical to writing
    // `v = x` in the body.
    let src = r"
        struct Box {
            int v;
            Box(int x) : v(x) {}
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::Box",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_assign_or_update(&prog, "Box::Box", "@p0.v", ["@p1"]);
}

#[test_log::test]
fn cpp_ctor_init_list_target_is_member_under_param_shadowing() {
    // FR-2 (shadowing): in `Box(int v) : v(v) {}` the initializer's *left* side is always the
    // member `this.v` (`@p0.v`), even though the param `v` shadows the member name on the
    // right (the init expression resolves to the param `@p1`). Guards that the init-list LHS
    // is built directly as `this.<member>`, not via shadowing-aware name resolution.
    let src = r"
        struct Box {
            int v;
            Box(int v) : v(v) {}
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_assign_or_update(&prog, "Box::Box", "@p0.v", ["@p1"]);
}

#[test_log::test]
fn cpp_ctor_out_of_line_discovered_and_lowered() {
    // FR-1: an out-of-line constructor `Box::Box(int x){…}` (declarator is a
    // `qualified_identifier` whose scope and name are both the class) is discovered and
    // lowered with the same implicit `this` (`ByRef`, param 0) and `this.<member>`
    // resolution as an inline one — its body's `v = x` becomes an update of `@p0.v`.
    let src = r"
        struct Box {
            int v;
            Box(int x);
        };
        Box::Box(int x) { v = x; }";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::Box",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_return_arity(&prog, "Box::Box", 0);
    check_func_assign_or_update(&prog, "Box::Box", "@p0.v", ["@p1"]);
}

#[test_log::test]
fn cpp_construction_direct_calls_ctor_with_receiver_arg0() {
    // FR-3: `Box b(source())` (which tree-sitter parses as a function declaration — the most
    // vexing parse) lowers to a DIRECT call to `Box::Box` with the new object `b` prepended
    // as the arg-0 receiver; the constructor's `this.v` write thus lands in `b`. The
    // argument `source()` is reconstructed into a direct call to `source`. `b.get()` then
    // dispatches with `b` as its receiver.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "main", "Box::Box");
    check_direct_call_arg0(&prog, "main", "Box::Box", "b");
    // The most-vexing-parse argument `source()` was reconstructed as a real call.
    check_has_direct_call(&prog, "main", "source");
    // The constructed object is usable as a method receiver afterwards.
    check_direct_call(&prog, "main", "Box::get", ["b"]);
}

#[test_log::test]
fn cpp_construction_three_syntaxes_all_call_ctor() {
    // FR-3: direct `Box b1(arg)`, copy `Box b2 = Box(arg)`, and brace `Box b3{arg}` all lower
    // to a `DirectCall Box::Box` with the respective object as the arg-0 receiver.
    let src = r#"
        extern "C" int source();
        struct Box {
            int v;
            Box(int x) { v = x; }
        };
        int main() {
            Box b1(source());
            Box b2 = Box(source());
            Box b3{source()};
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "main", "Box::Box", "b1");
    check_direct_call_arg0(&prog, "main", "Box::Box", "b2");
    check_direct_call_arg0(&prog, "main", "Box::Box", "b3");
}

#[test_log::test]
fn cpp_destructor_definition_parses_and_is_not_lowered() {
    // FR-1: a destructor *definition* `~Box(){}` must parse (no tree-sitter error) and must
    // not be mis-lowered as a function — only the constructor and methods are lowered. The
    // C++ path stays clean: there is no free function named `Box` (the constructor is
    // `Box::Box`) and no `~Box`.
    let src = r"
        struct Box {
            int v;
            Box(int x) { v = x; }
            ~Box() {}
            int get() { return v; }
        };";
    let (prog, has_error, _dump) = super::parse_cpp_program(src).expect("parse C++");
    assert!(
        !has_error,
        "destructor definition should parse without error"
    );
    // Constructor + getter lowered as members; the destructor is ingested but not lowered.
    check_func_params(
        &prog,
        "Box::Box",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_returns_path(&prog, "Box::get", "@p0.v");
    assert!(
        function_named(&prog, "Box").is_none(),
        "the inline constructor must not leak as a free function `Box`\n{prog}"
    );
    assert!(
        function_named(&prog, "~Box").is_none(),
        "the destructor must not be lowered as a function\n{prog}"
    );
}

// ---------------------------------------------------------------------------
// Milestone 3 (spec 007) — reference-returning methods (`return *this`) and method
// chaining. These assert the *frontend's* lowering; the end-to-end source→sink flows are
// asserted in `taint_compare::tests::cpp_chain_*` and validated against DFSan by
// CPP_51..CPP_54.
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_ref_return_this_method_is_discovered_and_returns_receiver() {
    // FR-1: a reference-returning method (`Box& setV(int){…}`, whose `function_declarator` is
    // wrapped in a `reference_declarator`) is still discovered and lowered with the implicit
    // `this` (`ByRef`, param 0). Its `return *this` returns the receiver object `@p0` (not a
    // copy), which is what makes the call result an alias to arg 0.
    let src = r"
        struct Box {
            int v;
            Box& setV(int x) { v = x; return *this; }
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::setV",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    // A reference return is a value (arity 1) return of the receiver object.
    check_return_arity(&prog, "Box::setV", 1);
    check_func_assign_or_update(&prog, "Box::setV", "@p0.v", ["@p1"]);
    check_func_returns_path(&prog, "Box::setV", "@p0");
}

#[test_log::test]
fn cpp_method_chaining_dispatches_on_same_object() {
    // FR-2: `b.setV(source()).setW(0)` — because `setV` returns `*this` (an alias to the
    // receiver), the chained `.setW(0)` dispatches on the SAME object `b`, not on a copy of
    // the returned temporary. Both setters therefore have `b` as their arg-0 receiver, so the
    // by-ref writeback carries both member writes back into `b`.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "main", "Box::setV");
    check_has_direct_call(&prog, "main", "Box::setW");
    // The critical assertion: the chained setter dispatches on `b` (true aliasing), not a temp.
    check_direct_call_arg0(&prog, "main", "Box::setV", "b");
    check_direct_call_arg0(&prog, "main", "Box::setW", "b");
    check_direct_call(&prog, "main", "Box::getV", ["b"]);
}

#[test_log::test]
fn cpp_chain_terminal_getter_dispatches_on_object() {
    // FR-2: `b.setV(source()).getV()` — the terminal getter dispatches on the object `setV`
    // returned (`b`), so it reads the member `setV` just wrote.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "main", "Box::setV", "b");
    check_direct_call(&prog, "main", "Box::getV", ["b"]);
}

#[test_log::test]
fn cpp_ref_return_bound_to_ref_local_aliases_object() {
    // FR-1/FR-2: `Box& r = b.setV(source())` binds `r` to the object `setV` returned (`b`),
    // not to the returned temporary — so `r.getV()` dispatches on `b` (arg 0 is `b`).
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
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "main", "Box::setV", "b");
    check_direct_call(&prog, "main", "Box::getV", ["b"]);
}

#[test_log::test]
fn cpp_member_reference_getter_returns_member() {
    // FR-1: a method returning a member *reference* (`int& get(){ return v; }`) read from
    // behaves like a value getter — it is discovered (the `reference_declarator` return is
    // unwrapped) and its `return v` returns `this.v` (`@p0.v`), NOT the receiver, so it is not
    // marked `returns_self`.
    let src = r"
        struct Box {
            int v;
            int& get() { return v; }
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_params(&prog, "Box::get", &[ParameterType::ByRef]);
    check_return_arity(&prog, "Box::get", 1);
    check_func_returns_path(&prog, "Box::get", "@p0.v");
}

#[test_log::test]
fn cpp_out_of_line_ref_return_method_is_discovered() {
    // FR-1: an out-of-line reference-returning method (`Box& Box::setV(int){…}`, declarator is
    // a `reference_declarator` wrapping a `qualified_identifier` function declarator) is
    // discovered and lowered with the implicit `this`, and its `return *this` is a receiver
    // return that lets `b.setV(source()).getV()` chain on the object.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        class Box {
          public:
            int v;
            Box& setV(int x);
            int getV();
        };
        Box& Box::setV(int x) { v = x; return *this; }
        int Box::getV() { return v; }
        int main() {
            Box b;
            sink(b.setV(source()).getV());
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::setV",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_returns_path(&prog, "Box::setV", "@p0");
    // Chaining works even though the setter body is out of line.
    check_direct_call_arg0(&prog, "main", "Box::setV", "b");
    check_direct_call(&prog, "main", "Box::getV", ["b"]);
}

// ---- Milestone 4 (spec 008): namespaces ----

#[test_log::test]
fn cpp_namespaced_free_function_qualified_name() {
    // FR-1: a free function in a named namespace is lowered under its *qualified* IR name
    // (`ns::id`), and a qualified call `ns::id(t)` emits a direct call to that name. The bare
    // name `id` must NOT also be registered as a global function (no duplicate registration).
    let src = r"
        namespace ns {
            int id(int x) { return x; }
        }
        int main() {
            int t = 0;
            int r = ns::id(t);
            return r;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    // The namespaced function exists under its qualified name...
    check_return_arity(&prog, "ns::id", 1);
    check_func_returns_path(&prog, "ns::id", "@p0");
    // ...and NOT under its bare name.
    assert!(
        function_named(&prog, "id").is_none(),
        "bare `id` must not be a resolvable global function\n{prog}"
    );
    // The qualified call resolves to the qualified definition.
    check_has_direct_call(&prog, "main", "ns::id");
}

#[test_log::test]
fn cpp_namespaced_method_dispatch() {
    // FR-2: a class in a named namespace registers under `ns::Box`; a `ns::Box b;` local
    // dispatches `b.set(…)`/`b.get()` to the qualified method names `ns::Box::set`/`ns::Box::get`
    // with `b` as the arg-0 receiver. The methods are lowered under their qualified names.
    let src = r"
        namespace ns {
            struct Box {
                int v;
                void set(int x) { v = x; }
                int get() { return v; }
            };
        }
        int main() {
            ns::Box b;
            b.set(0);
            int g = b.get();
            return g;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    // Methods lowered under qualified names, each with an implicit `this` (ByRef) at param 0.
    check_func_params(
        &prog,
        "ns::Box::set",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_params(&prog, "ns::Box::get", &[ParameterType::ByRef]);
    // Dispatch on the namespaced-class local passes `b` as arg 0.
    check_direct_call_arg0(&prog, "main", "ns::Box::set", "b");
    check_direct_call(&prog, "main", "ns::Box::get", ["b"]);
}

#[test_log::test]
fn cpp_namespaced_construction_dispatch() {
    // FR-2: constructing a namespaced-class local (`ns::Box b(y)`) lowers to a direct call to
    // the qualified constructor `ns::Box::ns::Box` with `b` as the arg-0 receiver, and a later
    // `b.get()` dispatches to `ns::Box::get`.
    let src = r"
        namespace ns {
            struct Box {
                int v;
                Box(int x) { v = x; }
                int get() { return v; }
            };
        }
        int main() {
            int y = 0;
            ns::Box b(y);
            int g = b.get();
            return g;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    // The constructor is `ns::Box::ns::Box(this: ByRef, x: ByVal)`.
    check_func_params(
        &prog,
        "ns::Box::ns::Box",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_direct_call_arg0(&prog, "main", "ns::Box::ns::Box", "b");
    check_direct_call(&prog, "main", "ns::Box::get", ["b"]);
}

#[test_log::test]
fn cpp_namespaced_distinct_functions_resolve_precisely() {
    // FR-3 negative: two distinct namespaced free functions are each lowered under their own
    // qualified name; `ns::drop(t)` resolves to `ns::drop` (which discards its arg), NOT to the
    // similarly-scoped `ns::keep`. Pins that qualified calls do not cross-resolve to a sibling.
    let src = r"
        namespace ns {
            int keep(int x) { return x; }
            int drop(int x) { return 0; }
        }
        int main() {
            int t = 0;
            return ns::drop(t);
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_func_returns_path(&prog, "ns::keep", "@p0");
    check_returns_const(&prog, "ns::drop", "0");
    // The call resolves to `ns::drop`, and never to `ns::keep`.
    check_has_direct_call(&prog, "main", "ns::drop");
    let calls = direct_calls_in(&prog, "main");
    assert!(
        !calls
            .iter()
            .any(|(callees, _)| callees.iter().any(|c| c == "ns::keep")),
        "ns::drop(t) must not resolve to ns::keep\n{prog}"
    );
}

// ---- Milestone 4 (spec 009): overloading (by arity) ----
// The end-to-end source→sink flows for these are asserted in `taint_compare::tests::
// cpp_overload_*` and validated against DFSan by CPP_59..CPP_62.

#[test_log::test]
fn cpp_overloaded_free_functions_lowered_distinctly_and_resolved_by_arity() {
    // FR-1: `id` has definitions of two distinct arities, so each is lowered under an
    // arity-mangled IR name (`id#1`, `id#2`) as a DISTINCT function — neither clobbers the
    // other — and the bare name `id` is never registered. A call resolves to the overload whose
    // parameter count equals its explicit-argument count: `id(x)` → `id#1` (returns its one
    // arg, `@p0`), `id(x, y)` → `id#2` (returns its 2nd, `@p1`).
    let src = r"
        int id(int a) { return a; }
        int id(int a, int b) { return b; }
        int main() {
            int x = 0;
            int y = 0;
            int r1 = id(x);
            int r2 = id(x, y);
            return r1 + r2;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    // Two distinct overloads, each with its own body; neither merged into the other.
    check_return_arity(&prog, "id#1", 1);
    check_return_arity(&prog, "id#2", 1);
    check_func_returns_path(&prog, "id#1", "@p0");
    check_func_returns_path(&prog, "id#2", "@p1");
    // The overloaded name is NOT also registered bare.
    assert!(
        function_named(&prog, "id").is_none(),
        "overloaded `id` must not be registered under its bare name\n{prog}"
    );
    // Calls resolve by explicit-argument count.
    check_direct_call(&prog, "main", "id#1", ["x"]);
    check_direct_call(&prog, "main", "id#2", ["x", "y"]);
}

#[test_log::test]
fn cpp_overloaded_methods_resolved_by_arity() {
    // FR-2: a class with same-name methods of two arities lowers `Box::f#1`/`Box::f#2` (each
    // with an implicit `this` ByRef at param 0); `b.f(x)` dispatches to the arity-1 method,
    // `b.f(x, y)` to the arity-2 one, with `b` as the arg-0 receiver either way. The bare
    // qualified name `Box::f` is never registered.
    let src = r"
        struct Box {
            int v;
            int f(int a) { return a; }
            int f(int a, int b) { return b; }
        };
        int main() {
            Box b;
            int x = 0;
            int y = 0;
            int r1 = b.f(x);
            int r2 = b.f(x, y);
            return r1 + r2;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::f#1",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_params(
        &prog,
        "Box::f#2",
        &[
            ParameterType::ByRef,
            ParameterType::ByVal,
            ParameterType::ByVal,
        ],
    );
    // `this` is param 0, so the 1st explicit arg is `@p1`, the 2nd `@p2`.
    check_func_returns_path(&prog, "Box::f#1", "@p1");
    check_func_returns_path(&prog, "Box::f#2", "@p2");
    // Dispatch resolves by explicit-argument count; `b` is arg 0 in both.
    check_direct_call_arg0(&prog, "main", "Box::f#1", "b");
    check_direct_call_arg0(&prog, "main", "Box::f#2", "b");
    assert!(
        function_named(&prog, "Box::f").is_none(),
        "overloaded method `Box::f` must not be registered under its bare qualified name\n{prog}"
    );
}

#[test_log::test]
fn cpp_overload_selects_dropping_overload_no_cross_resolution() {
    // FR-3 (the negative): `g(x, y)` (2 explicit args) resolves to the arity-2 overload `g#2`,
    // which discards its args (returns the constant 0) — and NEVER cross-resolves to the
    // flowing arity-1 sibling `g#1` (returns its arg). This is the precise-selection guard: a
    // merge, or a wrong pick of `g#1`, would leak taint (`s=flow` where DFSan sees none).
    let src = r"
        int g(int a) { return a; }
        int g(int a, int b) { return 0; }
        int main() {
            int x = 0;
            int y = 0;
            return g(x, y);
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_func_returns_path(&prog, "g#1", "@p0");
    check_returns_const(&prog, "g#2", "0");
    check_has_direct_call(&prog, "main", "g#2");
    let calls = direct_calls_in(&prog, "main");
    assert!(
        !calls
            .iter()
            .any(|(callees, _)| callees.iter().any(|c| c == "g#1")),
        "g(x, y) must resolve to g#2, never cross-resolve to the flowing g#1\n{prog}"
    );
}

#[test_log::test]
fn cpp_non_overloaded_name_stays_bare() {
    // FR-1 (regression guard): a name with a SINGLE definition is not overloaded, so it keeps
    // its bare IR name (no `#arity` suffix) and its call resolves bare — proving the mangler is
    // the identity for non-overloaded names (hence for all of C, whose `overloads` map is empty).
    let src = r"
        int id(int a) { return a; }
        int main() {
            int x = 0;
            return id(x);
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_return_arity(&prog, "id", 1);
    check_func_returns_path(&prog, "id", "@p0");
    assert!(
        function_named(&prog, "id#1").is_none(),
        "a non-overloaded name must not be arity-mangled\n{prog}"
    );
    check_direct_call(&prog, "main", "id", ["x"]);
}

#[test_log::test]
fn cpp_ctor_overloads_lowered_distinctly_and_resolved_by_arity() {
    // Spec 010 FR-1/FR-2: a class with constructors of two distinct arities lowers each under
    // an arity-mangled ctor name (`Box::Box#1`, `Box::Box#2`) as a DISTINCT function — neither
    // clobbers the other, and the bare `Box::Box` is never registered. A construction resolves
    // to the ctor whose parameter count equals its explicit-argument count: `Box b(source())`
    // → `Box::Box#1` (writes `v` from its one arg), `Box c(0, source())` → `Box::Box#2` (writes
    // `v` from its 2nd arg). The mixed-literal direct form `Box c(0, source())` parses as an
    // `init_declarator` with an `argument_list` value (not the most-vexing-parse), exercising
    // that construction path too.
    let src = r#"
        extern "C" int source();
        struct Box {
            int v;
            Box(int x) { v = x; }
            Box(int x, int y) { v = y; }
            int get() { return v; }
        };
        int main() {
            Box b(source());
            Box c(0, source());
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    // Two distinct ctor overloads, each with implicit `this` (ByRef) at param 0.
    check_func_params(
        &prog,
        "Box::Box#1",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_params(
        &prog,
        "Box::Box#2",
        &[
            ParameterType::ByRef,
            ParameterType::ByVal,
            ParameterType::ByVal,
        ],
    );
    // Each writes the member from a different parameter (its body is not clobbered).
    check_func_assign_or_update(&prog, "Box::Box#1", "@p0.v", ["@p1"]);
    check_func_assign_or_update(&prog, "Box::Box#2", "@p0.v", ["@p2"]);
    // The overloaded ctor name is NOT also registered bare.
    assert!(
        function_named(&prog, "Box::Box").is_none(),
        "overloaded ctor `Box::Box` must not be registered under its bare name\n{prog}"
    );
    // Constructions resolve by explicit-argument count; the object is arg 0 either way.
    check_direct_call_arg0(&prog, "main", "Box::Box#1", "b");
    check_direct_call_arg0(&prog, "main", "Box::Box#2", "c");
}

#[test_log::test]
fn cpp_ctor_default_and_param_overload_resolves() {
    // Spec 010 FR-2: a default ctor (arity 0) and a parameterized ctor (arity 1) form an
    // overload set of two arities, so each is mangled (`Box::Box#0`, `Box::Box#1`) and both
    // are lowered (the default is not clobbered). `Box b(source())` has one explicit argument,
    // so it resolves to `Box::Box#1`, whose body writes the member from that argument.
    let src = r#"
        extern "C" int source();
        struct Box {
            int v;
            Box() { v = 0; }
            Box(int x) { v = x; }
            int get() { return v; }
        };
        int main() {
            Box b(source());
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    assert!(
        function_named(&prog, "Box::Box#0").is_some(),
        "the default constructor must be lowered as `Box::Box#0`\n{prog}"
    );
    check_func_params(
        &prog,
        "Box::Box#1",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_assign_or_update(&prog, "Box::Box#1", "@p0.v", ["@p1"]);
    // Construction with one explicit argument resolves to the arity-1 ctor.
    check_direct_call_arg0(&prog, "main", "Box::Box#1", "b");
    assert!(
        function_named(&prog, "Box::Box").is_none(),
        "overloaded ctor `Box::Box` must not be registered under its bare name\n{prog}"
    );
}

#[test_log::test]
fn cpp_ctor_overload_selects_dropping_no_cross_resolution() {
    // Spec 010 FR-3 (the negative): `Box(int x){v=x;}` (arity 1, flows) and
    // `Box(int x,int y){v=0;}` (arity 2, DROPS). The construction `Box d(source(), 0)` has two
    // explicit arguments, so it must resolve to the arity-2 ctor `Box::Box#2` (which discards
    // its args, writing the constant 0) and NEVER cross-resolve to the flowing arity-1 sibling
    // `Box::Box#1`. A merge — or a wrong pick of `#1` — would leak `source` into `d.v`
    // (`s=flow` where DFSan sees none). This is the precise-selection discriminator.
    let src = r#"
        extern "C" int source();
        struct Box {
            int v;
            Box(int x) { v = x; }
            Box(int x, int y) { v = 0; }
            int get() { return v; }
        };
        int main() {
            Box d(source(), 0);
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_func_assign_or_update(&prog, "Box::Box#1", "@p0.v", ["@p1"]);
    check_func_assign_or_update(&prog, "Box::Box#2", "@p0.v", ["#0"]);
    check_direct_call_arg0(&prog, "main", "Box::Box#2", "d");
    let calls = direct_calls_in(&prog, "main");
    assert!(
        !calls
            .iter()
            .any(|(callees, _)| callees.iter().any(|c| c == "Box::Box#1")),
        "Box d(source(), 0) must resolve to Box::Box#2, never cross-resolve to Box::Box#1\n{prog}"
    );
}

#[test_log::test]
fn cpp_single_ctor_stays_bare() {
    // Spec 010 FR-1 (regression guard): a class with a SINGLE constructor is not an overload
    // set, so the ctor keeps its bare IR name `Box::Box` (no `#arity` suffix) and the
    // construction edge is bare — byte-for-byte identical to spec 006. This proves the mangler
    // is the identity for a single-ctor class (so every existing 006/008 construction is
    // unchanged).
    let src = r#"
        extern "C" int source();
        struct Box {
            int v;
            Box(int x) { v = x; }
            int get() { return v; }
        };
        int main() {
            Box b(source());
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::Box",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    assert!(
        function_named(&prog, "Box::Box#1").is_none(),
        "a single-ctor class must not arity-mangle its constructor\n{prog}"
    );
    check_direct_call_arg0(&prog, "main", "Box::Box", "b");
}

// ---------------------------------------------------------------------------
// Milestone 6 (spec 011) — C++ single (non-virtual) inheritance.
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_inherited_method_dispatches_to_base() {
    // FR-2: a method defined only in `Base` dispatches on a `Derived` receiver to the base that
    // owns it (`Base::set`/`Base::get`), with the derived object `d` as the arg-0 by-ref
    // receiver — so the base's `this.v` write/read lands in `d`. Before this slice neither
    // inherited call dispatched (dispatch checked only the receiver's own `methods`).
    let src = r#"
        struct Base {
            int v;
            void set(int x) { v = x; }
            int get() { return v; }
        };
        struct Derived : Base {};
        int main() {
            Derived d;
            d.set(0);
            int y = d.get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    // The edge names the *defining* base class, not the receiver's static class.
    check_has_direct_call(&prog, "main", "Base::set");
    check_has_direct_call(&prog, "main", "Base::get");
    check_direct_call_arg0(&prog, "main", "Base::set", "d");
    check_direct_call(&prog, "main", "Base::get", ["d"]);
}

#[test_log::test]
fn cpp_inherited_member_resolves_in_derived_method() {
    // FR-1: an inherited data member used unqualified inside a *derived* method resolves to
    // `this.<member>` (`@p0.v`) — member flattening unions `Base`'s members into `Derived`'s, so
    // `v` in `Derived::store` is recognized as a member (the base subobject shares the field-named
    // path). Guards the resolution `build_access_path` performs off `Derived`'s flattened members.
    let src = r"
        struct Base { int v; };
        struct Derived : Base {
            void store(int x) { v = x; }
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_assign_or_update(&prog, "Derived::store", "@p0.v", ["@p1"]);
}

#[test_log::test]
fn cpp_non_virtual_override_dispatches_to_derived() {
    // FR-3: when both `Base` and `Derived` define `get`, a `Derived` static-type receiver
    // dispatches to `Derived::get` (the walk checks the receiver's own class first) — non-virtual
    // override by static type. The inherited-only `set` still dispatches to `Base::set`.
    let src = r#"
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
            d.set(0);
            int y = d.get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    // `get` is overridden: the derived method wins for a Derived receiver.
    check_has_direct_call(&prog, "main", "Derived::get");
    assert!(
        !direct_calls_in(&prog, "main")
            .iter()
            .any(|(edges, _)| edges.iter().any(|e| e == "Base::get")),
        "a Derived static-type receiver must NOT dispatch d.get() to Base::get\n{prog}"
    );
    // `set`, defined only in Base, still dispatches to the base.
    check_has_direct_call(&prog, "main", "Base::set");
    check_direct_call_arg0(&prog, "main", "Derived::get", "d");
}

#[test_log::test]
fn cpp_two_level_inheritance_walks_transitively() {
    // FR-2 (transitive): a two-level chain `A <- B <- C`. `set`/`get` are defined in `A`; a `C`
    // receiver walks C -> B -> A to dispatch each to `A::set`/`A::get`. Also exercises transitive
    // member flattening (C's members include A's `v`).
    let src = r#"
        struct A {
            int v;
            void set(int x) { v = x; }
            int get() { return v; }
        };
        struct B : A {};
        struct C : B {};
        int main() {
            C c;
            c.set(0);
            int y = c.get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "main", "A::set");
    check_has_direct_call(&prog, "main", "A::get");
    check_direct_call_arg0(&prog, "main", "A::set", "c");
    check_direct_call(&prog, "main", "A::get", ["c"]);
}

#[test_log::test]
fn cpp_base_less_class_dispatch_unchanged() {
    // FR-5 regression guard: a base-less class keeps an empty `bases`, so dispatch is a plain
    // own-class lookup — `b.set(...)`/`b.get()` dispatch to `Box::set`/`Box::get` exactly as
    // before this slice (the base-chain walk is a no-op when `bases` is empty).
    let src = r#"
        struct Box {
            int v;
            void set(int x) { v = x; }
            int get() { return v; }
        };
        int main() {
            Box b;
            b.set(0);
            int y = b.get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "main", "Box::set", "b");
    check_direct_call(&prog, "main", "Box::get", ["b"]);
}

// ---------------------------------------------------------------------------
// Milestone 6 (spec 012) — C++ virtual dispatch via class-hierarchy analysis (CHA).
// A virtual method called through a base pointer/reference lowers to a *multi-target*
// `DirectCall` listing every override in the static type's subclass subtree; a non-virtual
// method stays single-target static dispatch (spec 011).
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_virtual_call_through_base_pointer_is_multi_target() {
    // FR-2: `virtual int get()` overridden in `Derived`, called through `Base* p = &d`. The
    // edge lists BOTH overrides in the subtree (`Base::get`, `Derived::get`) — CHA, a sound
    // superset of the single dynamic target — with the pointer receiver `p` as arg 0. Static
    // dispatch would name only `Base::get` (unsound, since `p` points at a `Derived`).
    let src = r#"
        int source();
        void sink(int);
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
            Base* p = &d;
            p->set(source());
            int y = p->get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    // Both overrides are targets of the single virtual call.
    let get_edges: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(edges, _)| edges)
        .filter(|edges| edges.iter().any(|e| e.ends_with("::get")))
        .collect();
    assert_eq!(
        get_edges.len(),
        1,
        "expected exactly one get() call site\n{prog}"
    );
    let edges = &get_edges[0];
    assert!(
        edges.iter().any(|e| e == "Base::get") && edges.iter().any(|e| e == "Derived::get"),
        "virtual get() must dispatch to BOTH Base::get and Derived::get (CHA); got {edges:?}\n{prog}"
    );
    // The pointer receiver `p` is the arg-0 by-ref receiver of the virtual call.
    check_direct_call_arg0(&prog, "main", "Base::get", "p");
    check_direct_call_arg0(&prog, "main", "Derived::get", "p");
}

#[test_log::test]
fn cpp_virtual_call_through_base_reference_is_multi_target() {
    // Spec 013 (base references): reference twin of
    // `cpp_virtual_call_through_base_pointer_is_multi_target`. `virtual int get()` overridden in
    // `Derived`, called through `Base& r = d` (`r.get()`, dot not arrow). The edge lists BOTH
    // overrides in the subtree (`Base::get`, `Derived::get`) — CHA. 012's machinery resolves a
    // reference receiver the same as a pointer receiver; this locks that in with a regression test.
    let src = r#"
        int source();
        void sink(int);
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
            int y = r.get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let get_edges: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(edges, _)| edges)
        .filter(|edges| edges.iter().any(|e| e.ends_with("::get")))
        .collect();
    assert_eq!(
        get_edges.len(),
        1,
        "expected exactly one get() call site\n{prog}"
    );
    let edges = &get_edges[0];
    assert!(
        edges.iter().any(|e| e == "Base::get") && edges.iter().any(|e| e == "Derived::get"),
        "virtual get() through a base reference must dispatch to BOTH Base::get and Derived::get (CHA); got {edges:?}\n{prog}"
    );
}

#[test_log::test]
fn cpp_base_pointer_static_type_is_declared_class() {
    // FR-2: a base pointer's static type is its **declared** class (`Base`), not the derived
    // type of its initializer (`&d`). So dispatch resolves against `Base`'s hierarchy — the
    // virtual `get()` is virtual on `Base`, hence multi-target. (If the static type were
    // `Derived`, the CHA root would be `Derived` and `Base::get` would be dropped.)
    let src = r#"
        struct Base {
            int v;
            virtual int get() { return 0; }
        };
        struct Derived : Base {
            int get() override { return v; }
        };
        int main() {
            Derived d;
            Base* p = &d;
            int y = p->get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    // Rooting CHA at the declared `Base` includes the base's own `get` in the target set.
    check_has_direct_call(&prog, "main", "Base::get");
    check_has_direct_call(&prog, "main", "Derived::get");
}

#[test_log::test]
fn cpp_non_virtual_through_base_pointer_stays_single_target() {
    // FR-3: a **non-virtual** method called through a base pointer stays single-target static
    // dispatch (spec 011) — `p->set(...)` resolves to just `Base::set`, no CHA expansion, even
    // though `Base` also has a virtual method. Only `virtual` methods go multi-target.
    let src = r#"
        int source();
        void sink(int);
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
            Base* p = &d;
            p->set(source());
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let set_edges: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(edges, _)| edges)
        .filter(|edges| edges.iter().any(|e| e.ends_with("::set")))
        .collect();
    assert_eq!(set_edges, vec![vec!["Base::set".to_string()]], "{prog}");
}

#[test_log::test]
fn cpp_virtual_recorded_and_inherited_override_is_virtual() {
    // FR-1: both a `virtual`-keyword base method and an `override` derived method are recorded
    // virtual — so the override dispatches by CHA even though it lacks the `virtual` keyword.
    // We assert the derived override, called on a `Derived*`, still lists both subtree targets
    // (virtualness is inherited from the base; the `override` also marks it directly).
    let src = r#"
        struct Base {
            int v;
            virtual int get() { return v; }
        };
        struct Derived : Base {
            int get() override { return 0; }
        };
        int main() {
            Derived d;
            Derived* p = &d;
            int y = p->get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    // Rooted at `Derived` (the pointer's static type): the subtree is just `Derived`, but the
    // static-type resolution walks up to nothing new — CHA root `Derived` defines `get`, so the
    // single subtree target is `Derived::get`. Virtualness is what makes it a CHA edge at all.
    check_has_direct_call(&prog, "main", "Derived::get");
    // `Derived` has no subclasses, so `Base::get` is NOT a target of a `Derived*` receiver
    // (CHA descends the subtree, it does not ascend).
    assert!(
        !direct_calls_in(&prog, "main")
            .iter()
            .any(|(edges, _)| edges.iter().any(|e| e == "Base::get")),
        "a Derived* virtual call descends its subtree, not up to Base::get\n{prog}"
    );
}

#[test_log::test]
fn cpp_virtual_multi_level_gathers_whole_subtree() {
    // FR-2 (transitive CHA): a three-level chain `A <- B <- C`, `get` virtual in `A` and
    // overridden in both `B` and `C`. A call through `A* p` gathers the whole subtree —
    // `A::get`, `B::get`, `C::get` — a sound superset of whichever the dynamic type selects.
    let src = r#"
        struct A {
            int v;
            virtual int get() { return 0; }
        };
        struct B : A {
            int get() override { return v; }
        };
        struct C : B {
            int get() override { return v; }
        };
        int main() {
            C c;
            A* p = &c;
            int y = p->get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let get_edges: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(edges, _)| edges)
        .filter(|edges| edges.iter().any(|e| e.ends_with("::get")))
        .collect();
    assert_eq!(get_edges.len(), 1, "one get() call site\n{prog}");
    let edges = &get_edges[0];
    for want in ["A::get", "B::get", "C::get"] {
        assert!(
            edges.iter().any(|e| e == want),
            "CHA subtree must include {want}; got {edges:?}\n{prog}"
        );
    }
}

#[test_log::test]
fn cpp_non_polymorphic_class_dispatch_unchanged() {
    // FR-5 regression guard: a class with no `virtual` method keeps an empty `virtual_methods`
    // set, so even an inherited-method call through a base pointer stays single-target static
    // dispatch — byte-for-byte the spec-011 behavior (the CHA path is a no-op when nothing is
    // virtual).
    let src = r#"
        struct Base {
            int v;
            void set(int x) { v = x; }
            int get() { return v; }
        };
        struct Derived : Base {};
        int main() {
            Derived d;
            Base* p = &d;
            p->set(0);
            int y = p->get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    // Non-virtual: single-target static dispatch to the defining base, receiver `p`.
    let calls = direct_calls_in(&prog, "main");
    for method in ["set", "get"] {
        let edges: Vec<Vec<String>> = calls
            .iter()
            .map(|(edges, _)| edges.clone())
            .filter(|edges| edges.iter().any(|e| e.ends_with(&format!("::{method}"))))
            .collect();
        assert_eq!(
            edges,
            vec![vec![format!("Base::{method}")]],
            "non-virtual {method} must stay single-target Base::{method}\n{prog}"
        );
    }
}

// ---------------------------------------------------------------------------
// Spec 014 — `new` / `delete` heap objects. `new Type(args)` allocates a synthetic heap
// object, runs its constructor, and binds the pointer as an alias so `p->m()` (including
// virtual, via CHA) operates on it; `delete p` parses and moves no taint.
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_new_runs_constructor_on_heap_object() {
    // FR-1: `new Box(source())` emits the constructor `DirectCall Box::Box` on the synthetic
    // heap object (arg 0 the object, arg 1 the ctor argument), and the pointer `p` aliases that
    // object so `p->get()` dispatches to `Box::get` on it.
    let src = r#"
        int source();
        void sink(int);
        struct Box {
            int v;
            Box(int x) { v = x; }
            int get() { return v; }
        };
        int main() {
            Box* p = new Box(source());
            int y = p->get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    // The constructor call exists on the heap object (2 args: the object + the ctor arg).
    let ctor_calls: Vec<usize> = direct_calls_in(&prog, "main")
        .into_iter()
        .filter(|(edges, _)| edges.iter().any(|e| e == "Box::Box"))
        .map(|(_, args)| args.len())
        .collect();
    assert_eq!(
        ctor_calls,
        vec![2],
        "expected one Box::Box ctor call with 2 args (object + ctor arg)\n{prog}"
    );
    // The getter dispatches to Box::get (the pointer aliases the heap object).
    check_has_direct_call(&prog, "main", "Box::get");
}

#[test_log::test]
fn cpp_new_heap_virtual_call_is_multi_target() {
    // FR-2: a `Derived` heap-allocated through `Base* p = new Derived()` makes `p->get()` a
    // VIRTUAL call. CHA over the declared static type `Base`'s subtree lists BOTH overrides
    // (`Base::get`, `Derived::get`) — the pointer's static type, not the allocated `Derived`,
    // roots the dispatch — a sound superset of the single dynamic target.
    let src = r#"
        int source();
        void sink(int);
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
            int y = p->get();
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let get_edges: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(edges, _)| edges)
        .filter(|edges| edges.iter().any(|e| e.ends_with("::get")))
        .collect();
    assert_eq!(
        get_edges.len(),
        1,
        "expected exactly one get() call site\n{prog}"
    );
    let edges = &get_edges[0];
    assert!(
        edges.iter().any(|e| e == "Base::get") && edges.iter().any(|e| e == "Derived::get"),
        "heap virtual get() must dispatch to BOTH Base::get and Derived::get (CHA); got {edges:?}\n{prog}"
    );
}

#[test_log::test]
fn cpp_delete_parses_and_moves_no_taint() {
    // FR-3: `delete p;` parses without error and lowers to nothing that moves taint — it emits
    // no `DirectCall` of its own, leaving only the constructor and method dispatch.
    let src = r#"
        int source();
        void sink(int);
        struct Box {
            int v;
            void set(int x) { v = x; }
            int get() { return v; }
        };
        int main() {
            Box* p = new Box();
            p->set(source());
            int y = p->get();
            delete p;
            return 0;
        }
    "#;
    let (prog, has_error, _dump) = super::parse_cpp_program(src).expect("parse C++");
    assert!(
        !has_error,
        "a program with `delete p;` must parse without error"
    );
    // `delete` introduces no call edge; only the setter and getter dispatch remain.
    let callees: Vec<String> = direct_calls_in(&prog, "main")
        .into_iter()
        .flat_map(|(edges, _)| edges)
        .collect();
    assert!(
        callees.iter().any(|c| c == "Box::set") && callees.iter().any(|c| c == "Box::get"),
        "expected the setter/getter dispatch to survive alongside delete; got {callees:?}\n{prog}"
    );
    assert!(
        !callees.iter().any(|c| c.contains("delete")),
        "delete must not emit a call edge; got {callees:?}\n{prog}"
    );
}

// ---- Spec 015: static methods & static data members ------------------------

#[test_log::test]
fn cpp_static_data_member_is_class_scoped_global() {
    // A `static` data member is a single class-scoped GLOBAL, not a per-object field: an
    // unqualified reference inside the class's methods binds to the global `Counter::total`
    // (`$globals.Counter::total`), so a write through one static method is read by another.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    // The static member write/read both bind to the class-scoped global, not `this.total`.
    check_func_assign_or_update(&prog, "Counter::add", "$globals.Counter::total", ["@p0"]);
    check_func_returns_path(&prog, "Counter::get", "$globals.Counter::total");
    // Both static methods are invoked as receiver-less qualified calls.
    check_has_direct_call(&prog, "main", "Counter::add");
    check_has_direct_call(&prog, "main", "Counter::get");
}

#[test_log::test]
fn cpp_static_method_has_no_implicit_this() {
    // A `static` member function is lowered WITHOUT the implicit `this`: its parameters are
    // exactly the declared ones (`x` at index 0, ByVal — not ByRef `this` then `x` at 1), and
    // the argument reaches the return with no receiver shift.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    check_func_params(&prog, "C::identity", &[ParameterType::ByVal]);
    check_func_returns_path(&prog, "C::identity", "@p0");
    check_has_direct_call(&prog, "main", "C::identity");
}

#[test_log::test]
fn cpp_static_member_shared_across_instance_and_static_method() {
    // A static member is one shared global regardless of access: a NON-static method keeps its
    // implicit `this` (ByRef at 0, `x` at 1) but its write to the static member still binds to
    // the class-scoped global `Counter::total` — the same key the static getter reads — so the
    // two methods share the location.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    // The non-static method has an implicit `this` (so `x` is @p1), yet the static member is the
    // global, not `this.total`.
    check_func_params(
        &prog,
        "Counter::bump",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_assign_or_update(&prog, "Counter::bump", "$globals.Counter::total", ["@p1"]);
    // The static getter has no `this` and reads the same global.
    check_func_params(&prog, "Counter::get", &[]);
    check_func_returns_path(&prog, "Counter::get", "$globals.Counter::total");
}

#[test_log::test]
fn cpp_distinct_static_members_are_distinct_globals() {
    // Field sensitivity among statics: two distinct static members are two distinct globals
    // (`Counter::a` vs `Counter::b`), so a write to `a` does not reach a read of `b`.
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
    "#;
    let prog = program_from_cpp_string(src).0;
    // The setter writes the global `Counter::a`; the getter returns the *distinct* global
    // `Counter::b`. Two distinct global keys ⇒ no `a`→`b` flow (the end-to-end negative in
    // `taint_compare::tests::cpp_static_distinct_member_no_flow` confirms the absent flow).
    check_func_assign_or_update(&prog, "Counter::seta", "$globals.Counter::a", ["@p0"]);
    check_func_returns_path(&prog, "Counter::getb", "$globals.Counter::b");
}
