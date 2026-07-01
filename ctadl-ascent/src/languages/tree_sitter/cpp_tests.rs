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
