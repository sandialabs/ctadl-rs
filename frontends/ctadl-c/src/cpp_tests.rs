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

use crate::test_utils::*;
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
    // Parity with C `subscript_access_paths`: a constant subscript is `*(f + N)`, so it lowers
    // to a numeric `Offset(N)` on the address (`.[N]`) plus the symbolic `deref` field naming
    // the memory there. In C++ the index lives under a `subscript_argument_list`; the
    // `subscript_index` hook recovers it so the lowered path matches the C frontend's exactly.
    let src = r"
        int brackets_simple(Donkey v, Burro* b, int x, int y) {
            int f = 1;
            x = f[3];
            f[4] = x;
        }";
    let prog = program_from_cpp_string(src).0;
    check_loads(&prog, "f.[3].deref"); // x = f[3]  (read lowers to a load)
    check_assign_or_update(&prog, "f.[4].deref", ["@p2"], None); // f[4] = x  (store)
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
    check_flow(&s, 2, "", 0, ".f2");
    check_flow(&s, 1, ".f2.f3.f4", 0, ".f2.nf1.y");
    check_flow(&s, 1, ".fa", 0, ".f5");
    check_flow(&s, 1, ".fb", 0, ".f5");
    check_flow(&s, 2, "", 0, ".f3");
    check_flow(&s, 3, "", 0, ".f3");
    check_flow(&s, 1, ".xyz", 0, ".f1");
    check_returns_param(&s, 0, ".f1");
    check_returns_param(&s, 1, ".xyz");
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
    check_func_loads(&prog, "Box::get", "@p0.v");
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
    check_func_loads(&prog, "S::get", "@p0.v");
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
    check_func_loads(&prog, "Box::get", "@p0.v");
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
    check_func_loads(&prog, "Box::get", "@p0.v");
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
fn cpp_destructor_definition_parses_and_is_lowered() {
    // Spec 016 FR-1: a destructor *definition* `~Box(){}` parses (no tree-sitter error) and is
    // lowered as the niladic `this`-method `Box::~Box` — an implicit `this` (`ByRef`) param 0 and
    // no other params. (Before spec 016 it was ingested-but-not-lowered; spec 014's `delete` was a
    // no-op. Now `delete p` invokes it, so it must be a real function.) The C++ path stays clean:
    // there is no free function named `Box` (the constructor is `Box::Box`) and the destructor's
    // IR name is the qualified `Box::~Box`, not a bare `~Box`.
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
    // Constructor + getter + destructor lowered as members. The destructor is a niladic
    // `this`-method: exactly one `ByRef` param (`this`), no others.
    check_func_params(
        &prog,
        "Box::Box",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_loads(&prog, "Box::get", "@p0.v");
    check_func_params(&prog, "Box::~Box", &[ParameterType::ByRef]);
    assert!(
        function_named(&prog, "Box").is_none(),
        "the inline constructor must not leak as a free function `Box`\n{prog}"
    );
    assert!(
        function_named(&prog, "~Box").is_none(),
        "the destructor's IR name is the qualified `Box::~Box`, not a bare `~Box`\n{prog}"
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
    check_func_loads(&prog, "Box::get", "@p0.v");
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

// ---- Spec 016: virtual destructors (`delete` invokes the destructor chain) -----------------

#[test_log::test]
fn cpp_virtual_destructor_recorded_and_this_param_shape() {
    // Spec 016 FR-1: `virtual ~Base(){ sink(v); }` lowers `Base::~Base` as a niladic `this`-method
    // — exactly one `ByRef` param (`this`), no others — and its body reads the member through
    // `this` (`@p0.v`) into the sink. The `virtual` keyword marks it for CHA, exercised by the
    // delete-dispatch test below.
    let src = r#"
        int source();
        void sink(int);
        struct Base {
            int v;
            void set(int x) { v = x; }
            virtual ~Base() { sink(v); }
        };
    "#;
    let prog = program_from_cpp_string(src).0;
    check_func_params(&prog, "Base::~Base", &[ParameterType::ByRef]);
    // The destructor body reads the member through `this` (arg-0 receiver) and sinks it; the
    // read is a load, so the sink's argument is the loaded temp rather than the path itself.
    check_func_loads(&prog, "Base::~Base", "@p0.v");
    check_has_direct_call(&prog, "Base::~Base", "sink");
}

#[test_log::test]
fn cpp_delete_virtual_destructor_is_multi_target_cha() {
    // Spec 016 FR-2: `Base* p = new Derived(); delete p;` with a `virtual ~Base` overridden by
    // `~Derived`. `delete p` invokes the destructor of `*p`; because the destructor is virtual it
    // is a CHA multi-target `DirectCall` over the static type `Base`'s subtree destructors
    // (`Base::~Base`, `Derived::~Derived`) — a sound superset of the single runtime chain — with
    // the heap object (aliased by `p`) as the arg-0 by-ref receiver. This is the soundness fix:
    // static dispatch would never run `~Derived`, missing the sink.
    let src = r#"
        int source();
        void sink(int);
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
    "#;
    let prog = program_from_cpp_string(src).0;
    let calls = direct_calls_in(&prog, "main");
    let dtor_edges: Vec<&Vec<String>> = calls
        .iter()
        .map(|(edges, _)| edges)
        .filter(|edges| edges.iter().any(|e| e.contains("::~")))
        .collect();
    assert_eq!(
        dtor_edges.len(),
        1,
        "expected exactly one delete/destructor call site\n{prog}"
    );
    let edges = dtor_edges[0];
    assert!(
        edges.iter().any(|e| e == "Base::~Base") && edges.iter().any(|e| e == "Derived::~Derived"),
        "virtual delete must dispatch to BOTH Base::~Base and Derived::~Derived (CHA); got {edges:?}\n{prog}"
    );
    // The delete's arg-0 receiver is the same heap object the setter wrote (the alias of `p`).
    let set_arg0 = calls
        .iter()
        .find(|(e, _)| e.iter().any(|c| c == "Base::set"))
        .and_then(|(_, a)| a.first())
        .cloned();
    let del_arg0 = calls
        .iter()
        .find(|(e, _)| e.iter().any(|c| c == "Base::~Base"))
        .and_then(|(_, a)| a.first())
        .cloned();
    assert!(
        set_arg0.is_some() && set_arg0 == del_arg0,
        "delete's receiver must be the same heap object as the setter's; \
         set={set_arg0:?} del={del_arg0:?}\n{prog}"
    );
}

#[test_log::test]
fn cpp_delete_exact_type_non_virtual_is_single_target() {
    // Spec 016 FR-2: an exact-type `Box* p = new Box(); delete p;` with a non-virtual `~Box`
    // invokes the single `Box::~Box` — no CHA, no subtree — with the heap object as arg-0.
    let src = r#"
        int source();
        void sink(int);
        struct Box {
            int v;
            void set(int x) { v = x; }
            ~Box() { sink(v); }
        };
        int main() {
            Box* p = new Box();
            p->set(source());
            delete p;
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let dtor_edges: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(e, _)| e)
        .filter(|e| e.iter().any(|x| x.contains("::~")))
        .collect();
    assert_eq!(
        dtor_edges.len(),
        1,
        "expected exactly one destructor call site\n{prog}"
    );
    assert_eq!(
        dtor_edges[0],
        vec!["Box::~Box".to_string()],
        "exact-type non-virtual delete is single-target Box::~Box\n{prog}"
    );
}

#[test_log::test]
fn cpp_delete_destructor_less_class_emits_no_call() {
    // Spec 016 FR-2 (negative): `delete p` on a class with **no** destructor emits no call edge —
    // unchanged from spec 014's no-op. The setter/getter dispatch survive; nothing named `::~`.
    let src = r#"
        int source();
        void sink(int);
        struct Plain {
            int v;
            void set(int x) { v = x; }
            int get() { return v; }
        };
        int main() {
            Plain* p = new Plain();
            p->set(source());
            sink(p->get());
            delete p;
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let callees: Vec<String> = direct_calls_in(&prog, "main")
        .into_iter()
        .flat_map(|(edges, _)| edges)
        .collect();
    assert!(
        callees.iter().any(|c| c == "Plain::set") && callees.iter().any(|c| c == "Plain::get"),
        "expected the setter/getter dispatch to survive; got {callees:?}\n{prog}"
    );
    assert!(
        !callees.iter().any(|c| c.contains("::~")),
        "a destructor-less delete must emit no destructor call; got {callees:?}\n{prog}"
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
    check_func_loads(&prog, "Counter::get", "$globals.Counter::total");
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
    check_func_loads(&prog, "Counter::get", "$globals.Counter::total");
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
    check_func_loads(&prog, "Counter::getb", "$globals.Counter::b");
}

// ---- Spec 017: scope-exit / RAII destructors -------------------------------
// A class-typed STACK (automatic) object's destructor runs at the closing `}` of its enclosing
// scope — no `delete`. The `scope_exit` hook emits a single-target exact-type destructor
// `DirectCall` (the object as the arg-0 by-ref receiver) at the scope's normal fall-through exit,
// in reverse construction order. These assert the emitted call shape directly; the end-to-end
// source→sink flow is in `taint_compare::tests::cpp_scope_exit_*`.

#[test_log::test]
fn cpp_scope_exit_destructor_emitted_with_object_receiver() {
    // FR-1: a stack `Widget w` in a bare inner block gets its destructor emitted at the block's
    // `}` — a `DirectCall Widget::~Widget(w)` after the setter call, with the OBJECT itself (`w`,
    // not `*p`) as the arg-0 by-ref receiver, so the destructor's `this.v` read lands on the
    // tainted member.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Widget {
            int v;
            void set(int x) { v = x; }
            ~Widget() { sink(v); }
        };
        int main() {
            { Widget w; w.set(source()); }
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    // Exactly one destructor call site, single-target exact-type `Widget::~Widget`.
    let dtor_edges: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(e, _)| e)
        .filter(|e| e.iter().any(|x| x.contains("::~")))
        .collect();
    assert_eq!(
        dtor_edges,
        vec![vec!["Widget::~Widget".to_string()]],
        "a stack Widget must get exactly one single-target scope-exit destructor\n{prog}"
    );
    // The receiver is the object `w` itself.
    check_direct_call_arg0(&prog, "main", "Widget::~Widget", "w");
}

#[test_log::test]
fn cpp_scope_exit_destructor_less_stack_object_emits_no_call() {
    // FR-1 (negative): a stack object of a class with NO destructor emits nothing at scope exit —
    // the destructor frame records it, but the `scope_exit` hook finds no destructor in the
    // hierarchy and emits no call (spec 014's no-op). The setter dispatch survives.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Plain {
            int v;
            void set(int x) { v = x; }
        };
        int main() {
            { Plain w; w.set(source()); }
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let callees: Vec<String> = direct_calls_in(&prog, "main")
        .into_iter()
        .flat_map(|(e, _)| e)
        .collect();
    assert!(
        callees.iter().any(|c| c == "Plain::set"),
        "the setter dispatch must survive; got {callees:?}\n{prog}"
    );
    assert!(
        !callees.iter().any(|c| c.contains("::~")),
        "a destructor-less stack object must emit no scope-exit destructor; got {callees:?}\n{prog}"
    );
}

#[test_log::test]
fn cpp_scope_exit_destructors_are_reverse_construction_order() {
    // FR-1: two stack objects `a` then `b` in one scope are destroyed in REVERSE construction
    // order (LIFO) — `B::~B(b)` before `A::~A(a)` — matching C++.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct A { int v; void set(int x) { v = x; } ~A() { sink(v); } };
        struct B { int v; void set(int x) { v = x; } ~B() { sink(v); } };
        int main() {
            { A a; B b; a.set(source()); b.set(source()); }
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let dtors: Vec<String> = direct_calls_in(&prog, "main")
        .into_iter()
        .flat_map(|(e, _)| e)
        .filter(|e| e.contains("::~"))
        .collect();
    assert_eq!(
        dtors,
        vec!["B::~B".to_string(), "A::~A".to_string()],
        "objects are destroyed in reverse construction order (b before a)\n{prog}"
    );
}

#[test_log::test]
fn cpp_scope_exit_derived_object_destroys_as_exact_declared_type() {
    // FR-2: a stack `Derived d` (base `~Base`) destroys as exactly `Derived` — a stack object's
    // dynamic type equals its declared type, so scope exit is the single-target exact-type
    // destructor (no CHA), followed by its base subobject's own single-target `~Base`, as C++
    // runs it (spec 030; it used to stop at `~Derived`).
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Base { int v; void set(int x) { v = x; } ~Base() {} };
        struct Derived : Base { ~Derived() { sink(v); } };
        int main() {
            { Derived d; d.set(source()); }
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let dtor_edges: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(e, _)| e)
        .filter(|e| e.iter().any(|x| x.contains("::~")))
        .collect();
    assert_eq!(
        dtor_edges,
        vec![
            vec!["Derived::~Derived".to_string()],
            vec!["Base::~Base".to_string()]
        ],
        "a stack Derived destroys as exactly Derived::~Derived, then its Base (single-target, \
         no CHA)\n{prog}"
    );
    check_direct_call_arg0(&prog, "main", "Derived::~Derived", "d");
}

#[test_log::test]
fn cpp_scope_exit_function_body_scope() {
    // FR-2: the function body is itself a scope — a `Widget w` local is destroyed at the function's
    // closing `}` (no explicit inner block). The destructor call is emitted in `use`, not `main`.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Widget { int v; void set(int x) { v = x; } ~Widget() { sink(v); } };
        void use(int x) { Widget w; w.set(x); }
        int main() { use(source()); return 0; }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "use", "Widget::~Widget", "w");
    // No destructor is emitted in `main` (it constructs no stack object).
    let main_dtors: Vec<String> = direct_calls_in(&prog, "main")
        .into_iter()
        .flat_map(|(e, _)| e)
        .filter(|e| e.contains("::~"))
        .collect();
    assert!(
        main_dtors.is_empty(),
        "main constructs no stack object, so it emits no destructor; got {main_dtors:?}\n{prog}"
    );
}

#[test_log::test]
fn cpp_heap_pointer_gets_no_scope_exit_destructor() {
    // FR-4: a heap object reached through a pointer (`Box* p = new Box()`) is NOT a stack object —
    // it must get no spurious scope-exit destructor. With a single explicit `delete p`, exactly ONE
    // destructor call is emitted (the delete's), not two. The pointer `p` is not recorded as a
    // value stack object.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Box { int v; void set(int x) { v = x; } ~Box() { sink(v); } };
        int main() {
            Box* p = new Box();
            p->set(source());
            delete p;
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let dtor_edges: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(e, _)| e)
        .filter(|e| e.iter().any(|x| x.contains("::~")))
        .collect();
    assert_eq!(
        dtor_edges,
        vec![vec!["Box::~Box".to_string()]],
        "a heap pointer's object is destroyed only by its explicit `delete` (one call), \
         with no spurious scope-exit destructor\n{prog}"
    );
}

// ---- Spec 021: early-exit destructors --------------------------------------
// A scope's destructors run on EVERY exit, not just the fall-through `}`: a `return` unwinds every
// frame of the function (innermost first), a `break`/`continue` every frame back to the loop body.
// The `scope_unwind` hook emits them into the diverging statement's own block, before the control
// transfer, so each CFG path carries exactly the destructors it runs. These assert the per-block
// call shape (`dtor_calls_per_block` keeps the grouping `direct_calls_in` flattens away); the
// end-to-end source→sink flow is in `ctadl-dynamic`'s `CPP_104..CPP_107`.

#[test_log::test]
fn cpp_early_return_unwinds_the_enclosing_scope() {
    // FR-1 (`CPP_104`): a `return` inside an `if` arm destroys the enclosing function scope's
    // `Widget w`. Both paths out of `use` — the early `return 1` and the trailing `return 0` — get
    // their own `Widget::~Widget(w)`, one call each, because emission is per CFG path.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Widget { int v; void set(int x) { v = x; } ~Widget() { sink(v); } };
        int use(int c) {
            Widget w;
            w.set(source());
            if (c) { return 1; }
            return 0;
        }
        int main() { return use(1); }
    "#;
    let prog = program_from_cpp_string(src).0;
    assert_eq!(
        nonempty_dtor_blocks(&prog, "use"),
        vec![
            vec!["Widget::~Widget".to_string()],
            vec!["Widget::~Widget".to_string()]
        ],
        "each of the two paths out of `use` destroys `w` exactly once\n{prog}"
    );
    check_direct_call_arg0(&prog, "use", "Widget::~Widget", "w");
}

#[test_log::test]
fn cpp_early_return_unwinds_nested_scopes_innermost_first() {
    // FR-1 (`CPP_105`): a `return` two scopes deep unwinds BOTH frames in one block, innermost
    // first — `Inner::~Inner(x)` then `Outer::~Outer(w)`. Distinct classes so the order is visible
    // in the callee names alone. Neither compound here falls through (the inner block ends in
    // `return 2`, and so does the function body), so spec 017's scope-exit emission contributes
    // nothing at all: every call below comes from unwinding, one full pair per path.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Outer { int v; void set(int x) { v = x; } ~Outer() { sink(v); } };
        struct Inner { int v; ~Inner() { sink(v); } };
        int use(int c) {
            Outer w;
            w.set(source());
            {
                Inner x;
                if (c) { return 1; }
                return 2;
            }
        }
        int main() { return use(1); }
    "#;
    let prog = program_from_cpp_string(src).0;
    assert_eq!(
        nonempty_dtor_blocks(&prog, "use"),
        vec![
            vec!["Inner::~Inner".to_string(), "Outer::~Outer".to_string()],
            vec!["Inner::~Inner".to_string(), "Outer::~Outer".to_string()],
        ],
        "each `return` unwinds the inner frame then the outer one, in that order, in its own \
         block\n{prog}"
    );
    check_direct_call_arg0(&prog, "use", "Inner::~Inner", "x");
    check_direct_call_arg0(&prog, "use", "Outer::~Outer", "w");
}

#[test_log::test]
fn cpp_bare_return_in_void_function_unwinds() {
    // FR-1: a valueless `return;` unwinds exactly like a value-returning one — the arity-filling
    // `implicit_return` terminator is still preceded by the scope's destructors.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Widget { int v; void set(int x) { v = x; } ~Widget() { sink(v); } };
        void use(int c) {
            Widget w;
            w.set(source());
            if (c) { return; }
            w.set(0);
        }
        int main() { use(1); return 0; }
    "#;
    let prog = program_from_cpp_string(src).0;
    assert_eq!(
        nonempty_dtor_blocks(&prog, "use"),
        vec![
            vec!["Widget::~Widget".to_string()],
            vec!["Widget::~Widget".to_string()]
        ],
        "a bare `return;` unwinds the function scope just like `return <value>;`\n{prog}"
    );
}

#[test_log::test]
fn cpp_break_unwinds_the_loop_body_frame() {
    // FR-2 (`CPP_106`): `break` as the LAST statement of a loop body — the body never falls
    // through, so spec 017's scope-exit emission produced nothing at all. The `break`'s own block
    // now carries exactly one `Widget::~Widget(w)`, and it is the only one in the function.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Widget { int v; void set(int x) { v = x; } ~Widget() { sink(v); } };
        int main() {
            for (int i = 0; i < 2; i++) {
                Widget w;
                w.set(source());
                break;
            }
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    assert_eq!(
        nonempty_dtor_blocks(&prog, "main"),
        vec![vec!["Widget::~Widget".to_string()]],
        "the `break` block destroys the loop body's `w` exactly once\n{prog}"
    );
    check_direct_call_arg0(&prog, "main", "Widget::~Widget", "w");
}

#[test_log::test]
fn cpp_continue_unwinds_only_the_loop_body_frame() {
    // FR-2: a `continue` in an `if` arm destroys the loop body's object on its own path (it used
    // to agree only by luck, through the body's fall-through emission two blocks later). One call
    // in the `continue` block, one on the body's own fall-through — never two on one path.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Widget { int v; void set(int x) { v = x; } ~Widget() { sink(v); } };
        int main() {
            for (int i = 0; i < 2; i++) {
                Widget w;
                w.set(source());
                if (i == 0) { continue; }
                w.set(0);
            }
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    assert_eq!(
        nonempty_dtor_blocks(&prog, "main"),
        vec![
            vec!["Widget::~Widget".to_string()],
            vec!["Widget::~Widget".to_string()]
        ],
        "the `continue` path and the body's fall-through each destroy `w` once\n{prog}"
    );
}

#[test_log::test]
fn cpp_break_does_not_unwind_frames_outside_the_loop() {
    // FR-2 (the bound): a `break` leaves the frames back to the loop body and NO further — the
    // `Outer o` declared outside the loop is still live afterwards, so the `break` block must not
    // destroy it. `o` is destroyed only by the function's own `return`.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Outer { int v; void set(int x) { v = x; } ~Outer() { sink(v); } };
        struct Inner { int v; ~Inner() { sink(v); } };
        int main() {
            Outer o;
            o.set(source());
            for (int i = 0; i < 2; i++) {
                Inner w;
                break;
            }
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    let blocks = nonempty_dtor_blocks(&prog, "main");
    assert!(
        blocks.contains(&vec!["Inner::~Inner".to_string()]),
        "the `break` block destroys only the loop body's `w`; got {blocks:?}\n{prog}"
    );
    assert!(
        !blocks
            .iter()
            .any(|b| b.len() > 1 || (b.contains(&"Outer::~Outer".to_string()) && b.len() != 1)),
        "no path destroys `o` alongside `w` — a `break` does not unwind past the loop; \
         got {blocks:?}\n{prog}"
    );
    assert_eq!(
        blocks
            .iter()
            .filter(|b| b.contains(&"Outer::~Outer".to_string()))
            .count(),
        1,
        "`o` is destroyed once, by the function's `return`; got {blocks:?}\n{prog}"
    );
}

#[test_log::test]
fn cpp_fall_through_scope_exit_is_not_double_unwound() {
    // FR-4: spec 017's shape is unchanged. The inner block falls through, so its frame is emitted
    // and popped there; the following `return 0` finds it gone and adds nothing — exactly one
    // destructor call in the whole function, in the inner block, not two.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Widget { int v; void set(int x) { v = x; } ~Widget() { sink(v); } };
        int main() {
            { Widget w; w.set(source()); }
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    assert_eq!(
        nonempty_dtor_blocks(&prog, "main"),
        vec![vec!["Widget::~Widget".to_string()]],
        "a fall-through scope exit still emits exactly one destructor — the `return` after it \
         must not emit a second\n{prog}"
    );
}

#[test_log::test]
fn cpp_labeled_null_statement_goto_target() {
    // The `CPP_32_label_empty_statement` shape: `goto done;` jumps over a kill into a label whose
    // body is the null statement (`done: ;`), then the value reaches the sink. tree-sitter-cpp
    // parses the bare `;` into exactly the same node shape as tree-sitter-c (an
    // `expression_statement` with no named child), so the shared core's no-op lowering covers both
    // grammars with no hook and no language branch -- this test pins that parity. CFG mirrors the C
    // frontend's `labeled_null_statement_goto_target`: the pre-created label block is 1, the
    // unreachable post-`goto` statements land in block 2, which still links into the label.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        int main() {
            int s = source();
            int r = s;
            goto done;
            r = 0;
        done:
            ;
            sink(r);
            return 0;
        }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_block_count(&prog, 3);
    check_successors(&prog, 0, &[1]); // goto jumps straight to the label block
    check_successors(&prog, 1, &[]); // label block runs the sink call and returns
    check_successors(&prog, 2, &[1]); // unreachable `r = 0;` falls through into the label
    check_assign_or_update(&prog, "r", ["s"], None);
    check_has_direct_call(&prog, "main", "sink");
}

#[test_log::test]
fn cpp_null_statement_is_noop() {
    // A bare `;` mid-body is a no-op under the C++ grammar too: the body stays a single
    // straight-line block and the surrounding assignment is unaffected.
    let src = r"
        int f(int a) {
            int r = a;
            ;
            return r;
        }";
    let prog = program_from_cpp_string(src).0;
    check_block_count(&prog, 1);
    check_assign_or_update(&prog, "r", ["@p0"], None);
}

// ---------------------------------------------------------------------------
// Aggregate (brace) initializers — spec 019. The desugaring is in the shared core
// (`collect_initializer_list`), keyed on the node kind `initializer_list`, which both
// grammars use — so these mirror the C tests and assert the C++ frontend produces the
// same element stores. The C++-specific concern is the boundary with construction:
// a brace on a class *with a constructor* must stay a `Box::Box` call (specs 006/010),
// which `cpp_class_brace_init_is_not_an_aggregate` pins.
// ---------------------------------------------------------------------------

#[test_log::test]
fn cpp_array_aggregate_initializer() {
    // Parity with C `array_aggregate_initializer` (the CPP_31 shape): `int a[2] = { x, 0 };`
    // desugars element-wise, so element 0 is `a.deref` (`Offset(0)` is elided) and element 1
    // `a.[1].deref`, and the tainted element reaches the return through `a[0]`.
    let src = r"
        int cpp_array_aggregate_initializer(int x) {
            int a[2] = { x, 0 };
            return a[0];
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_assign_or_update(&prog, "a.deref", ["@p0"], None);
    check_assign_or_update(&prog, "a.[1].deref", ["#0"], None);

    let summary = get_summary(prog).unwrap().0;
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn cpp_struct_aggregate_initializer_positional() {
    // Parity with C `struct_aggregate_initializer_positional` (the CPP_95 shape). C++ lets the
    // record type be named without the `struct` keyword, so both spellings are asserted: the
    // layout registry keys on the tag either way, and the tainted element lands on `p.x`.
    for decl in ["struct P p = { v, 0 };", "P p = { v, 0 };"] {
        let src = format!(
            r"
            struct P {{ int x; int y; }};
            int cpp_struct_aggregate_initializer_positional(int v) {{
                {decl}
                return p.x;
            }}
        "
        );
        let prog = program_from_cpp_string(&src).0;
        check_assign_or_update(&prog, "p.x", ["@p0"], None);
        check_assign_or_update(&prog, "p.y", ["#0"], None);

        let summary = get_summary(prog).unwrap().0;
        check_returns_param(&summary, 0, "");
    }
}

#[test_log::test]
fn cpp_struct_aggregate_initializer_distinct_member() {
    // Parity with C `struct_aggregate_initializer_distinct_member` (the CPP_96 negative):
    // `{ 0, v }` taints the SECOND member, so reading `p.x` must report no flow. An
    // over-approximating lowering (both elements written to `p` itself) would fail here.
    let src = r"
        struct P { int x; int y; };
        int cpp_struct_aggregate_initializer_distinct_member(int v) {
            P p = { 0, v };
            return p.x;
        }
    ";
    let prog = program_from_cpp_string(src).0;
    check_assign_or_update(&prog, "p.x", ["#0"], None);
    check_assign_or_update(&prog, "p.y", ["@p0"], None);

    let summary = get_summary(prog).unwrap().0;
    check_does_not_return_param(&summary, 0, "");
}

#[test_log::test]
fn cpp_funcptr_array_aggregate_initializer() {
    // Parity with C `funcptr_array_aggregate_initializer` (the CPP_97 shape): the brace form
    // must lower to exactly the statements the element-assignment form does, which is what makes
    // the indirect call through `fps[0]` resolve (the CPP_30 funcptr facts).
    let brace = r"
        int id(int p) { return p; }
        int via_brace(int x) {
            int (*fps[2])(int) = { id, id };
            return fps[0](x);
        }
    ";
    let elemwise = r"
        int id(int p) { return p; }
        int via_brace(int x) {
            int (*fps[2])(int);
            fps[0] = id;
            fps[1] = id;
            return fps[0](x);
        }
    ";
    let brace_prog = program_from_cpp_string(brace).0;
    let elemwise_prog = program_from_cpp_string(elemwise).0;
    assert_eq!(
        stmt_kinds_of(&brace_prog, "via_brace"),
        stmt_kinds_of(&elemwise_prog, "via_brace"),
        "brace-initialized funcptr array must lower like element assignment\nbrace:\n{brace_prog}\nelement-wise:\n{elemwise_prog}"
    );
}

#[test_log::test]
fn cpp_class_brace_init_is_not_an_aggregate() {
    // FR-5 boundary: `Box b{source()}` on a class *with a constructor* is construction, not
    // aggregate initialization — the `construct` hook claims the declaration before the
    // aggregate path sees it. So the brace form must lower to exactly what the paren form
    // does (a `Box::Box` call with `b` as the arg-0 receiver) and synthesize no positional
    // member store of its own; comparing the two lowerings pins both halves at once.
    let brace = r#"
        extern "C" int source();
        struct Box {
            int v;
            Box(int x) { v = x; }
        };
        int main() {
            Box b{source()};
            return 0;
        }
    "#;
    let paren = r#"
        extern "C" int source();
        struct Box {
            int v;
            Box(int x) { v = x; }
        };
        int main() {
            Box b(source());
            return 0;
        }
    "#;
    let brace_prog = program_from_cpp_string(brace).0;
    let paren_prog = program_from_cpp_string(paren).0;
    check_direct_call_arg0(&brace_prog, "main", "Box::Box", "b");
    assert_eq!(
        stmt_kinds_of(&brace_prog, "main"),
        stmt_kinds_of(&paren_prog, "main"),
        "a class brace-init must stay construction, not become an aggregate\nbrace:\n{brace_prog}\nparen:\n{paren_prog}"
    );
}

// ---- Pointer-returning definitions: spec 130 parity for the C++ discovery paths -------------

#[test_log::test]
fn cpp_pointer_returning_inline_method_is_discovered() {
    // An inline method whose return type is a pointer (`int *pick(int *p)`) wraps its
    // `function_declarator` in a `pointer_declarator`. Discovery used to demand the bare
    // `function_declarator` (or a `reference_declarator` around it) and silently dropped the
    // body -- spec 130's bug on the C++ side. It is now discovered, lowered with the implicit
    // `this`, and returns its pointer parameter (`ByVal`, like every pointer: a write through
    // it is a non-empty path and still flows out).
    let src = r"
        struct Box {
            int *pick(int *p) { return p; }
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::pick",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_return_arity(&prog, "Box::pick", 1);
    check_func_returns_path(&prog, "Box::pick", "@p1");
}

#[test_log::test]
fn cpp_void_pointer_returning_method_returns_a_value() {
    // `void *raw(...)` returns a value: the `void` is the pointee, not the function. The arity
    // used to be read off the `type:` field alone, which makes this a void method whose
    // `return p` does not verify (`InconsistentReturns`).
    let src = r"
        struct Box {
            void *raw(void *p) { return p; }
        };";
    let prog = program_from_cpp_string(src).0;
    check_return_arity(&prog, "Box::raw", 1);
    check_func_returns_path(&prog, "Box::raw", "@p1");
}

#[test_log::test]
fn cpp_pointer_returning_out_of_line_method_is_discovered() {
    // The out-of-line query used to enumerate the bare and the reference-wrapped shapes; a
    // `pointer_declarator` per `*` around the `function_declarator` matched neither, so the
    // body was dropped. The declarator is now captured whole and unwrapped.
    let src = r"
        struct Box {
            int v;
            int **pick(int **p);
        };
        int **Box::pick(int **p) { return p; }";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::pick",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_return_arity(&prog, "Box::pick", 1);
    check_func_returns_path(&prog, "Box::pick", "@p1");
}

#[test_log::test]
fn cpp_pointer_returning_namespaced_function_is_discovered() {
    // Same drop, third path: a namespaced free function returning a pointer.
    let src = r"
        namespace ns {
            char *dup(char *s) { return s; }
        }";
    let prog = program_from_cpp_string(src).0;
    check_func_params(&prog, "ns::dup", &[ParameterType::ByVal]);
    check_return_arity(&prog, "ns::dup", 1);
    check_func_returns_path(&prog, "ns::dup", "@p0");
}

#[test_log::test]
fn cpp_pointer_returning_methods_are_still_an_overload_set() {
    // `overload_base_name` unwraps the same way, so pointer-returning definitions count toward
    // an overload set and are lowered under their arity-mangled names.
    let src = r"
        struct Box {
            int *pick(int *p) { return p; }
            int *pick(int *p, int) { return p; }
        };";
    let prog = program_from_cpp_string(src).0;
    check_return_arity(&prog, "Box::pick#1", 1);
    check_return_arity(&prog, "Box::pick#2", 1);
    check_func_returns_path(&prog, "Box::pick#2", "@p1");
}

#[test_log::test]
fn cpp_call_to_a_pointer_returning_method_reaches_its_body() {
    // The shape corpus case CPP_98 pins end to end: a call to a pointer-returning method is a
    // direct call to the lowered body (not to an arity-0 stub `define_extern_functions` would
    // invent), so the pointer it returns carries the caller's taint back.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Box {
            int *pick(int *p) { return p; }
        };
        int main() {
            Box b;
            int arr[2] = {0, 0};
            arr[0] = source();
            int *q = b.pick(arr);
            sink(q[0]);
            return 0;
        }"#;
    let (prog, dump) = program_from_cpp_string(src);
    check_has_direct_call(&prog, "main", "Box::pick");
    check_return_arity(&prog, "Box::pick", 1);
    assert!(
        check_no_match(&dump, "define Box::pick(@p0[byref]) -> 0"),
        "`Box::pick` must not be an arity-0 stub\n{dump}"
    );
}

// ---------------------------------------------------------------------------------------
// Milestone 12 (spec 020): a class-typed *parameter* is a method-call receiver, exactly as a
// class-typed local is. The class is recorded where the parameter is bound (`collect_params`),
// through the same type-to-class resolution a local's declaration uses (`declared_class`), so
// every dispatch site treats a parameter and a local alike.
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn cpp_by_value_class_parameter_is_a_receiver() {
    // FR-1: `b.get()` on a by-value `Box` parameter is a direct call to `Box::get` with the
    // parameter's own access path (`@p0`) as the arg-0 receiver -- not a free call `get(b)` to
    // a stub `define_extern_functions` would invent.
    let src = r#"
        extern "C" void sink(int);
        struct Box { int v; int get() { return v; } };
        void take(Box b) { sink(b.get()); }"#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "take", "Box::get", ["@p0"]);
}

#[test_log::test]
fn cpp_reference_class_parameter_is_a_receiver() {
    // FR-1: a `Box&` parameter dispatches the same way, on the same `@p0`.
    let src = r#"
        extern "C" void sink(int);
        struct Box { int v; int get() { return v; } };
        void take(Box& b) { sink(b.get()); }"#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "take", "Box::get", ["@p0"]);
}

#[test_log::test]
fn cpp_pointer_class_parameter_is_a_receiver() {
    // FR-1: a `Box*` parameter dispatches through `p->get()`. The receiver is the *pointer*
    // identifier itself (spec 014's convention: the pointer stands for the object it aliases),
    // so `param_head`'s descent to the leaf identifier is what keys it -- `*p` would never
    // match the receiver name.
    let src = r#"
        extern "C" void sink(int);
        struct Box { int v; int get() { return v; } };
        void take(Box* p) { sink(p->get()); }"#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "take", "Box::get", ["@p0"]);
}

#[test_log::test]
fn cpp_method_object_parameter_dispatches_on_p1() {
    // FR-1: inside a *method*, the implicit `this` is `@p0`, so a class-typed parameter of that
    // method is `@p1` -- and `o.get()` must dispatch on the parameter, not on the receiver.
    let src = r"
        struct Box {
            int v;
            int get() { return v; }
            int diff(Box& o) { return v - o.get(); }
        };";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "Box::diff", "Box::get", ["@p1"]);
}

#[test_log::test]
fn cpp_class_parameter_modes_are_unchanged() {
    // FR-2 (the frontend's half): registering a parameter's class must not disturb its passing
    // mode. `Box b` and `Box* p` are `ByVal` and `Box& b` is `ByRef`. A member write is a
    // non-empty path, so it flows out of a `ByVal` formal too (`isout`); `ByRef` additionally
    // carries a write at the formal's root, which only a reference can make the caller see. What the analysis *below*
    // the frontend does with a by-value aggregate is its own model (see the CPP_102 case).
    let src = r"
        struct Box { int v; void set(int x) { v = x; } };
        void by_value(Box b) { b.set(1); }
        void by_ref(Box& b) { b.set(1); }
        void by_const_ref(const Box& b) { b.set(1); }
        void by_ptr(Box* p) { p->set(1); }";
    let prog = program_from_cpp_string(src).0;
    check_func_params(&prog, "by_value", &[ParameterType::ByVal]);
    check_func_params(&prog, "by_ref", &[ParameterType::ByRef]);
    check_func_params(&prog, "by_const_ref", &[ParameterType::ByVal]);
    check_func_params(&prog, "by_ptr", &[ParameterType::ByVal]);
}

#[test_log::test]
fn cpp_class_parameter_receiver_keeps_members_apart() {
    // The CPP_103 negative, at the IR level: a write through the parameter receiver lands on
    // one member's access path only. `setV` updates `@p0.v`; `getW` reads `@p0.w`. Registering
    // the parameter's class makes it a receiver -- it does not smear taint over the object.
    let src = r"
        struct Box {
            int v;
            int w;
            void setV(int x) { v = x; }
            int getW() { return w; }
        };
        void fill(Box& b, int x) { b.setV(x); }";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "fill", "Box::setV", ["@p0", "@p1"]);
    check_func_assign_or_update(&prog, "Box::setV", "@p0.v", ["@p1"]);
    check_func_loads(&prog, "Box::getW", "@p0.w");
}

#[test_log::test]
fn cpp_unknown_type_parameter_is_not_a_receiver() {
    // The registration is gated on the neutral `classes` map: a parameter whose type names no
    // known class (here `Opaque`, declared nowhere in the unit) records nothing, so `o.get()`
    // falls back to ordinary call lowering rather than inventing a dispatch.
    let src = r#"
        extern "C" void sink(int);
        struct Box { int v; int get() { return v; } };
        void take(Opaque o) { sink(o.get()); }"#;
    let prog = program_from_cpp_string(src).0;
    assert!(
        !direct_calls_in(&prog, "take")
            .iter()
            .any(|(callees, _)| callees.iter().any(|c| c == "Box::get")),
        "a parameter of an unknown type must not dispatch to a class method\n{prog}"
    );
}

#[test_log::test]
fn c_struct_parameter_is_untouched() {
    // Principle II / the neutrality guard: for C the `classes` map is empty, so no parameter is
    // ever registered and a `struct`-typed parameter lowers exactly as before -- a plain
    // by-value formal whose member read is a load off `@p0`, with no dispatch anywhere.
    let src = r"
        struct Box { int v; };
        int get(struct Box b) { return b.v; }";
    let prog = program_from_string(src).0;
    check_func_params(&prog, "get", &[ParameterType::ByVal]);
    check_func_loads(&prog, "get", "@p0.v");
    assert!(
        direct_calls_in(&prog, "get").is_empty(),
        "a C struct parameter must not dispatch anything\n{prog}"
    );
}

#[test_log::test]
fn cpp_copy_ctor_is_lowered_as_a_function_of_its_own() {
    // Spec 022 FR-1: `Box(int)` and `Box(const Box&)` are both arity 1. Keyed by arity alone
    // they shared the single name `Box::Box` -- one arity, so never mangled -- and the second
    // `lower_function` ran against the first's `FunctionIdx`, appending its parameters to it.
    // The copy constructor is now keyed `Box::Box#copy`, which is neither counted in
    // `Box::Box`'s arity set nor mangled by it, so each is a function with exactly its own
    // parameters and its own body.
    let src = r#"
        struct Box {
            int v;
            int w;
            Box(int x) : v(x), w(0) {}
            Box(const Box& o) : v(0), w(o.v) {}
        };"#;
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::Box",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_params(
        &prog,
        "Box::Box#copy",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    // Each body writes its own members from its own parameter: the int constructor takes the
    // value, the copy constructor reads the source object's member.
    check_func_assign_or_update(&prog, "Box::Box", "@p0.v", ["@p1"]);
    check_func_loads(&prog, "Box::Box#copy", "@p1.v");
    // Neither name is arity-mangled: the copy constructor's arity never joined `Box::Box`'s set,
    // so both remain single-arity names.
    assert!(
        function_named(&prog, "Box::Box#1").is_none(),
        "the copy constructor's arity must not make `Box::Box` an overload set\n{prog}"
    );
}

#[test_log::test]
fn cpp_copy_ctor_parameter_mode_follows_the_reference_kind() {
    // Spec 022 FR-1: recognizing the copy constructor changes only its *key*; its parameter
    // mode is the ordinary one `param_head` computes, so `const Box&` is `ByVal` (read-only)
    // and a non-const `Box&` is `ByRef` (writes reach the source object).
    let src = r"
        struct ConstRef { int v; ConstRef(int x) : v(x) {} ConstRef(const ConstRef& o) : v(o.v) {} };
        struct MutRef { int v; MutRef(int x) : v(x) {} MutRef(MutRef& o) : v(o.v) {} };";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "ConstRef::ConstRef#copy",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_params(
        &prog,
        "MutRef::MutRef#copy",
        &[ParameterType::ByRef, ParameterType::ByRef],
    );
}

#[test_log::test]
fn cpp_out_of_line_copy_ctor_is_discovered() {
    // Spec 022 FR-1: an out-of-line `Box::Box(const Box& o) : v(o.v) {}` takes the same distinct
    // key its inline spelling does, so a copy construction reaches its body -- and its
    // same-arity sibling `Box::Box(int)` keeps its own.
    let src = r#"
        extern "C" int source();
        struct Box {
            int v;
            int w;
            Box(int x);
            Box(const Box& o);
        };
        Box::Box(int x) : v(x), w(0) {}
        Box::Box(const Box& o) : v(0), w(o.v) {}
        int main() { Box a(source()); Box b(a); return 0; }"#;
    let prog = program_from_cpp_string(src).0;
    check_func_assign_or_update(&prog, "Box::Box", "@p0.v", ["@p1"]);
    check_func_loads(&prog, "Box::Box#copy", "@p1.v");
    check_direct_call_arg0(&prog, "main", "Box::Box#copy", "b");
}

#[test_log::test]
fn cpp_copy_construction_forms_call_the_copy_ctor() {
    // Spec 022 FR-2: all three spellings of a construction from a same-class object -- direct
    // `Box b(a)` (the most-vexing-parse `function_declarator`), copy `Box c = a` (an
    // `init_declarator` whose value is the bare identifier) and brace `Box d{a}` -- lower to
    // `DirectCall Box::Box#copy(&obj, a)`, the spec-006 construction edge with the copy key.
    let src = r#"
        extern "C" int source();
        struct Box {
            int v;
            int w;
            Box(int x) : v(x), w(0) {}
            Box(const Box& o) : v(0), w(o.v) {}
        };
        int main() {
            Box a(source());
            Box b(a);
            Box c = a;
            Box d{a};
            return 0;
        }"#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "main", "Box::Box#copy", ["b", "a"]);
    check_direct_call(&prog, "main", "Box::Box#copy", ["c", "a"]);
    check_direct_call(&prog, "main", "Box::Box#copy", ["d", "a"]);
    // The non-copy construction still resolves by arity to the other constructor.
    check_direct_call_arg0(&prog, "main", "Box::Box", "a");
}

#[test_log::test]
fn cpp_construction_without_a_user_copy_ctor_is_still_a_value_copy() {
    // Spec 022 FR-2: the copy edge is gated on the class declaring a copy constructor. Without
    // one, `Box b = a` keeps the value-copy lowering byte-for-byte -- which is exactly the
    // implicit member-wise copy -- and emits no constructor call at all (the CPP_95/`p1d`
    // model this must not disturb).
    let src = r#"
        extern "C" int source();
        struct Box { int v; void set(int x) { v = x; } };
        int main() {
            Box a;
            a.set(source());
            Box b = a;
            return 0;
        }"#;
    let prog = program_from_cpp_string(src).0;
    assert!(
        !direct_calls_in(&prog, "main")
            .iter()
            .any(|(callees, _)| callees.iter().any(|c| c.starts_with("Box::Box"))),
        "a class with no user copy constructor must emit no constructor call for `Box b = a`\n{prog}"
    );
}

#[test_log::test]
fn cpp_byvalue_class_argument_is_copy_constructed() {
    // Spec 022 FR-3: `take(Box b)` copy-constructs its parameter from the argument, so the call
    // site emits `Box::Box#copy(&<temp>, a)` and passes that fresh object -- what the callee
    // receives is the copy the constructor built, not `a`.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Box {
            int v;
            int w;
            Box(int x) : v(x), w(0) {}
            Box(const Box& o) : v(0), w(o.v) {}
            int getw() { return w; }
        };
        void take(Box b) { sink(b.getw()); }
        int main() { Box a(source()); take(a); return 0; }"#;
    let prog = program_from_cpp_string(src).0;
    let calls = direct_calls_in(&prog, "main");
    let take_arg0 = calls
        .iter()
        .find(|(callees, _)| callees.iter().any(|c| c == "take"))
        .and_then(|(_, args)| args.first().cloned())
        .unwrap_or_else(|| panic!("no call to `take` in `main`\n{prog}"));
    let locals = &function_named(&prog, "main").expect("main").locals;
    let a = exp_from_str("a", locals);
    assert_ne!(
        take_arg0, a,
        "a by-value class parameter must receive the copy, not the source object\n{prog}"
    );
    assert!(
        calls.iter().any(
            |(callees, args)| callees.iter().any(|c| c == "Box::Box#copy")
                && args.as_slice() == [take_arg0.clone(), a.clone()]
        ),
        "expected `Box::Box#copy(<take's argument>, a)` before the call\n{prog}"
    );
}

#[test_log::test]
fn cpp_reference_class_parameter_argument_is_not_copied() {
    // Spec 022 FR-3: only a **by-value** class parameter copy-constructs. A `Box&` parameter
    // names the caller's object, so the argument is passed through untouched and no copy
    // constructor runs -- the record is keyed on the declarator shape, not the type.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Box {
            int v;
            int w;
            Box(int x) : v(x), w(0) {}
            Box(const Box& o) : v(0), w(o.v) {}
            int getw() { return w; }
        };
        void take(Box& b) { sink(b.getw()); }
        int main() { Box a(source()); take(a); return 0; }"#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "main", "take", ["a"]);
    assert!(
        !direct_calls_in(&prog, "main")
            .iter()
            .any(|(callees, _)| callees.iter().any(|c| c == "Box::Box#copy")),
        "a by-reference class parameter must not copy-construct its argument\n{prog}"
    );
}

#[test_log::test]
fn cpp_byvalue_class_argument_without_a_copy_ctor_is_untouched() {
    // Spec 022 FR-3: with no user copy constructor the call is unchanged -- the argument is the
    // caller's object and the existing value-copy model stands (the CPP_99/CPP_102 behavior).
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Box { int v; void set(int x) { v = x; } int get() { return v; } };
        void take(Box b) { sink(b.get()); }
        int main() { Box a; a.set(source()); take(a); return 0; }"#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "main", "take", ["a"]);
}

#[test_log::test]
fn c_by_value_struct_argument_is_untouched() {
    // Principle II / the neutrality guard: for C the `classes` map is empty, so no parameter is
    // ever recorded as a by-value class object and the `copy_args` hook is a no-op -- a
    // by-value `struct` argument is passed exactly as before, with no constructor call anywhere.
    let src = r"
        struct Box { int v; };
        void take(struct Box b) { }
        int main() { struct Box a; take(a); return 0; }";
    let prog = program_from_string(src).0;
    check_direct_call(&prog, "main", "take", ["a"]);
    assert_eq!(
        direct_calls_in(&prog, "main").len(),
        1,
        "a C by-value struct argument must emit nothing but the call itself\n{prog}"
    );
}

// ---------------------------------------------------------------------------------------
// Spec 023 — operator overloading: an operator applied to a class object is a method call
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn cpp_operator_assign_is_discovered_with_this() {
    // FR-1: `Acc& operator=(int x)` is an ordinary instance method under a punctuation name.
    // Discovery used to require a `field_identifier` declarator and dropped the `operator_name`
    // one, so the body was never lowered at all. It now lowers as `Acc::operator=` with the
    // implicit `this` first (`[ByRef, ByVal]`, arity 1) and, since it is a reference-returning
    // `return *this`, returns `@p0`.
    let src = r#"
        struct Acc { int v; Acc& operator=(int x) { v = x; return *this; } };
    "#;
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Acc::operator=",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_returns_path(&prog, "Acc::operator=", "@p0");
    check_func_assign_or_update(&prog, "Acc::operator=", "@p0.v", ["@p1"]);
}

#[test_log::test]
fn cpp_operator_subscript_and_call_are_discovered() {
    // FR-1: the subscript and call operators are discovered under the same rule, each with the
    // implicit `this` — `Acc::operator[]` and `Acc::operator()`. The punctuation in an IR
    // function name is inert (nothing downstream parses it), which is what these names pin.
    let src = r#"
        struct Acc {
            int v;
            int operator[](int i) { return v + i; }
            int operator()(int y) { return v + y; }
        };
    "#;
    let prog = program_from_cpp_string(src).0;
    for name in ["Acc::operator[]", "Acc::operator()"] {
        check_func_params(&prog, name, &[ParameterType::ByRef, ParameterType::ByVal]);
        check_func_loads(&prog, name, "@p0.v");
    }
    // The punctuation survives into the IR dump intact: nothing downstream parses a function
    // name, so `Class::operator[]` is as good a name as `Class::at`.
    let dump = format!("{prog}");
    assert!(
        dump.contains("define Acc::operator[](") && dump.contains("define Acc::operator()("),
        "an operator method's name must round-trip through the IR dump\n{dump}"
    );
}

#[test_log::test]
fn cpp_out_of_line_operator_is_discovered() {
    // FR-1: an out-of-line `Acc& Acc::operator+=(int x) { … }` is named by a
    // `qualified_identifier` whose *name* is an `operator_name`; phase 1.5 used to accept only a
    // plain `identifier` there and skipped it. It now lowers under the same `Acc::operator+=`
    // string the in-class spelling would produce, so the call site below reaches it.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Acc { int v; Acc& operator+=(int x); int get() { return v; } };
        Acc& Acc::operator+=(int x) { v = v + x; return *this; }
        int main() { Acc a; a += source(); sink(a.get()); return 0; }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Acc::operator+=",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_direct_call_arg0(&prog, "main", "Acc::operator+=", "a");
}

#[test_log::test]
fn cpp_assignment_to_object_dispatches_to_operator() {
    // FR-2: `a = source()` on an object whose class declares `operator=` is a method call with
    // `a` as the arg-0 (by-ref) receiver — not the built-in value copy into `a`. The by-ref
    // receiver is what carries the body's `v = x` back into the caller's object.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Acc {
            int v;
            Acc() : v(0) {}
            Acc& operator=(int x) { v = x; return *this; }
            int get() { return v; }
        };
        int main() { Acc a; a = source(); sink(a.get()); return 0; }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "main", "Acc::operator=", "a");
    // The operator call *replaces* the store: nothing in `main` writes `a` any more — the object
    // is only ever written by the callee, through the by-ref `@p0`.
    let a = local_render(&prog, "main", "a");
    let dump = format!("{prog}");
    assert!(
        !dump.contains(&format!("assign {a} =")) && !dump.contains(&format!("store {a}")),
        "`a = source()` must dispatch to the operator instead of storing into `a`\n{dump}"
    );
}

#[test_log::test]
fn cpp_compound_assignment_to_object_takes_only_the_rhs() {
    // FR-2: `a += source()` dispatches by the assignment's own spelling, and the call takes
    // exactly ONE explicit argument. The built-in compound lowering reads the target's old value
    // as a second operand; `operator+=` reads it itself through `this`, so passing it again would
    // be a second argument the one-parameter method has no formal for.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Acc {
            int v;
            Acc() : v(0) {}
            Acc& operator+=(int x) { v = v + x; return *this; }
            int get() { return v; }
        };
        int main() { Acc a; a += source(); sink(a.get()); return 0; }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "main", "Acc::operator+=", "a");
    let args = call_args(&prog, "main", "Acc::operator+=");
    assert_eq!(
        args.len(),
        2,
        "`a += x` is `Acc::operator+=(&a, x)` — receiver plus one argument\n{prog}"
    );
}

#[test_log::test]
fn cpp_assignment_without_an_operator_keeps_the_value_copy() {
    // FR-2 (the negative that keeps the rest of the corpus still): a class that declares NO
    // `operator=` keeps the exact value-copy lowering — `a = b` writes `a` and emits no call.
    // The implicit copy assignment *is* that value copy.
    let src = r#"
        struct Acc { int v; };
        int main() { Acc a; Acc b; a = b; return 0; }
    "#;
    let prog = program_from_cpp_string(src).0;
    assert!(
        direct_calls_in(&prog, "main").is_empty(),
        "a class with no user `operator=` must emit no call for `a = b`\n{prog}"
    );
    check_writes_to(&prog, "a", 1);
}

#[test_log::test]
fn cpp_operator_assign_keeps_members_apart() {
    // FR-2: field sensitivity across the operator call (the CPP_116 negative). `operator=`
    // writes only `v`; a read of the distinct member `w` observes nothing, because the by-ref
    // receiver carries the body's store to `a.v` and to no other path.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Acc {
            int v;
            int w;
            Acc() : v(0), w(0) {}
            Acc& operator=(int x) { v = x; return *this; }
            int getw() { return w; }
        };
        int main() { Acc a; a = source(); sink(a.getw()); return 0; }
    "#;
    let (summary, si) = get_summary(program_from_cpp_string(src).0).unwrap();
    // The argument reaches `v` and only `v`: the receiver is by-ref, so the body's store lands
    // at `@p0.v`. A flow to `@p0.w` here would mean the call conflated the whole object.
    check_flow_in(&summary, &si, "Acc::operator=", 1, "", 0, ".v");
    check_no_flow_in(&summary, &si, "Acc::operator=", 1, "", 0, ".w");
}

#[test_log::test]
fn cpp_subscript_on_object_dispatches_to_operator() {
    // FR-3: `a[0]` on an object whose class declares `operator[]` is `a.operator[](0)`, with `a`
    // as the receiver — not a read of the synthetic element path `a.[0].deref`, which nothing
    // ever wrote.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Vec {
            int v;
            Vec() : v(0) {}
            void set(int x) { v = x; }
            int operator[](int i) { return v + i; }
        };
        int main() { Vec a; a.set(source()); sink(a[0]); return 0; }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "main", "Vec::operator[]", "a");
}

#[test_log::test]
fn cpp_call_on_object_dispatches_to_call_operator() {
    // FR-3: `f(1)` where `f` names an OBJECT is `f.operator()(1)`. Before this slice it resolved
    // to a function named `f` that no unit defines, so `define_extern_functions` invented an
    // empty body for it — assert that stub is gone as well as that the method call is there.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Fn {
            int k;
            Fn() : k(0) {}
            void set(int x) { k = x; }
            int operator()(int y) { return k + y; }
        };
        int main() { Fn f; f.set(source()); sink(f(1)); return 0; }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "main", "Fn::operator()", "f");
    assert!(
        !direct_calls_in(&prog, "main")
            .iter()
            .any(|(callees, _)| callees.iter().any(|c| c == "f")),
        "`f(1)` on an object must not lower as a call to an extern stub named `f`\n{prog}"
    );
}

#[test_log::test]
fn cpp_call_to_a_real_function_is_unchanged() {
    // FR-3: the other side of the same question — a callee that is a function, not an object,
    // misses in `local_types` and keeps the ordinary call lowering. A local of a class that
    // *does* declare `operator()` is in scope here, so this pins that the hook keys on the
    // callee identifier and not merely on the presence of a call operator somewhere.
    let src = r#"
        struct Fn { int k; int operator()(int y) { return k + y; } };
        int g(int x) { return x; }
        int main() { Fn f; int r = g(1); return r; }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "main", "g", ["#1"]);
}

#[test_log::test]
fn cpp_write_through_reference_returning_subscript() {
    // FR-4: `a[0] = source()` with `int& operator[](int)` runs the operator AND stores through
    // the reference it returned. Discovery records that the body returns a reference to the
    // member `v`, so the store lands on `a.v` — where `a.get()` reads it — instead of on the
    // built-in element path `a.[0].deref`, which nothing reads.
    let src = r#"
        extern "C" int source();
        extern "C" void sink(int);
        struct Vec {
            int v;
            Vec() : v(0) {}
            int& operator[](int i) { return v; }
            int get() { return v; }
        };
        int main() { Vec a; a[0] = source(); sink(a.get()); return 0; }
    "#;
    let prog = program_from_cpp_string(src).0;
    check_direct_call_arg0(&prog, "main", "Vec::operator[]", "a");
    // The store lands on the member the returned reference denotes, `a.v`, which is exactly the
    // path `Vec::get` loads — not on the built-in element path `a.[0].deref`.
    check_func_loads(&prog, "Vec::get", "@p0.v");
    let a = local_render(&prog, "main", "a");
    let dump = format!("{prog}");
    assert!(
        dump.contains(&format!("store {a}.v")),
        "the write through `a[0]` must land on the member the reference denotes\n{dump}"
    );
}

#[test_log::test]
fn cpp_subscript_on_an_array_is_unchanged() {
    // FR-3 / FR-6: an array base is not a class object, so the hook declines and the built-in
    // element lowering stands — exactly the C behavior, which the C++ frontend has matched since
    // Milestone 2. Asserted against the C frontend's own lowering of the same program.
    let src = r"
        int source();
        void sink(int);
        int main() {
            int a[3];
            a[0] = source();
            sink(a[0]);
            return 0;
        }";
    let (cpp_prog, cpp_dump) = program_from_cpp_string(src);
    let (_, c_dump) = program_from_string(src);
    assert_eq!(
        cpp_dump, c_dump,
        "an array subscript must lower identically under both frontends"
    );
    assert_eq!(
        direct_calls_in(&cpp_prog, "main").len(),
        2,
        "an array subscript emits no call of its own — only source and sink\n{cpp_prog}"
    );
}

#[test_log::test]
fn c_assignment_subscript_and_call_are_untouched() {
    // Principle II / FR-6: for C the `classes`/`local_types` maps are empty, so the
    // `operator_call` hook is the no-op returning `None` at every shape — a C assignment,
    // subscript and call are lowered exactly as they always were, with no method call anywhere.
    let src = r"
        struct S { int v; };
        int g(int x) { return x; }
        int main() {
            struct S s;
            int a[2];
            s.v = 1;
            a[0] = 2;
            int r = g(a[0]);
            return r;
        }";
    let prog = program_from_string(src).0;
    let calls = direct_calls_in(&prog, "main");
    assert_eq!(
        calls.len(),
        1,
        "the only call in a C program with no operators is `g`\n{prog}"
    );
    assert_eq!(calls[0].0, vec!["g".to_string()], "{prog}");
    // The subscript keeps the built-in element lowering: `a[0]` is a load of `a.deref`, not a
    // call, and the store to `s.v` is a store, not a dispatch.
    check_func_loads(&prog, "main", "a.deref");
    check_func_assign_or_update(&prog, "main", "s.v", ["#1"]);
}

/* ----------------------------------------------------------------------------------------
Spec 024 (`same-arity-overloads`): two definitions of one name at one arity are two
functions, and a call reaches the one(s) its arguments can mean.
---------------------------------------------------------------------------------------- */

#[test_log::test]
fn cpp_same_arity_ctors_are_two_functions() {
    // FR-1: `Box(int)` and `Box(Box*)` share a name AND an arity. Keyed by arity alone they got
    // the single name `Box::Box`, so the second `lower_function` ran against the first's
    // `FunctionIdx` and appended its parameters to it -- one chimera with `(this, x, this, o)`
    // and two concatenated bodies. Each definition now gets a signature-mangled key of its own
    // and exactly its own two parameters (`this` + one), and each body writes its own member.
    let src = r"
        struct Box {
            int v;
            int w;
            Box(int x) : v(x), w(0) {}
            Box(Box* o) : v(0), w(o->v) {}
        };";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::Box#1(int)",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    // A pointer parameter and a plain `int` are both `ByVal` (a write through the pointer is a
    // non-empty path and still flows out) -- the modes `param_head` computes; only the *key*
    // is new.
    check_func_params(
        &prog,
        "Box::Box#1(Box*)",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_assign_or_update(&prog, "Box::Box#1(int)", "@p0.v", ["@p1"]);
    check_func_loads(&prog, "Box::Box#1(Box*)", "@p1.v");
    // The merged chimera is gone: nothing is lowered under the bare name or the arity-only key.
    assert!(
        function_named(&prog, "Box::Box").is_none()
            && function_named(&prog, "Box::Box#1").is_none(),
        "neither definition may keep a key that the other could also claim\n{prog}"
    );
}

#[test_log::test]
fn cpp_same_arity_methods_and_free_functions_are_two_functions() {
    // FR-1, the method and free-function shapes: `set(int)`/`set(Box&)` on a class become
    // `Box::set#1(int)`/`Box::set#1(Box&)`, and free `f(int)`/`f(Box*)` become
    // `f#1(int)`/`f#1(Box*)`. A reference parameter and an int parameter are distinct keys
    // because their written types differ, which is the whole of the naming rule.
    let src = r"
        struct Box {
            int v;
            int w;
            void set(int x) { v = x; }
            void set(Box& o) { w = o.v; }
        };
        int f(int x) { return x + 1; }
        int f(Box* p) { return p->v; }";
    let prog = program_from_cpp_string(src).0;
    check_func_params(
        &prog,
        "Box::set#1(int)",
        &[ParameterType::ByRef, ParameterType::ByVal],
    );
    check_func_params(
        &prog,
        "Box::set#1(Box&)",
        &[ParameterType::ByRef, ParameterType::ByRef],
    );
    check_func_assign_or_update(&prog, "Box::set#1(int)", "@p0.v", ["@p1"]);
    check_func_loads(&prog, "Box::set#1(Box&)", "@p1.v");
    check_func_params(&prog, "f#1(int)", &[ParameterType::ByVal]);
    check_func_params(&prog, "f#1(Box*)", &[ParameterType::ByVal]);
    check_func_loads(&prog, "f#1(Box*)", "@p0.v");
}

#[test_log::test]
fn cpp_namespaced_same_arity_functions_are_two_functions() {
    // FR-1: a namespaced free function is keyed by its qualified name, so two same-arity
    // definitions there are `ns::f#1(int)` / `ns::f#1(Box*)` -- the qualification and the
    // signature mangling compose, exactly as the qualification and the arity mangling do.
    let src = r"
        struct Box { int v; };
        namespace ns {
            int f(int x) { return x + 1; }
            int f(Box* p) { return p->v; }
        }";
    let prog = program_from_cpp_string(src).0;
    check_func_params(&prog, "ns::f#1(int)", &[ParameterType::ByVal]);
    check_func_params(&prog, "ns::f#1(Box*)", &[ParameterType::ByVal]);
    check_func_loads(&prog, "ns::f#1(Box*)", "@p0.v");
}

#[test_log::test]
fn cpp_singleton_arity_groups_keep_their_keys() {
    // FR-1 / FR-5: the key changes ONLY where one arity holds several definitions. A name
    // overloaded across two arities with one definition each keeps the arity-only keys it has
    // had since spec 007/010 (`id#1`/`id#2`, `Box::Box#1`/`Box::Box#2`), and a name with a
    // single definition keeps its bare name -- so no existing edge is renamed.
    let src = r"
        struct Box {
            int v;
            Box(int x) : v(x) {}
            Box(int x, int y) : v(x + y) {}
            int get() { return v; }
        };
        int id(int x) { return x; }
        int id(int x, int y) { return y; }";
    let prog = program_from_cpp_string(src).0;
    for name in ["id#1", "id#2", "Box::Box#1", "Box::Box#2", "Box::get"] {
        assert!(
            function_named(&prog, name).is_some(),
            "a singleton arity group must keep the key {name:?}\n{prog}"
        );
    }
    // Nothing acquired a signature: a signature-mangled key always contains a `(`.
    assert!(
        prog.functions
            .functions
            .raw
            .iter()
            .all(|f| !f.name.contains('(')),
        "no name whose arity groups are all singletons may be signature-mangled\n{prog}"
    );
}

#[test_log::test]
fn cpp_ctor_call_resolves_by_argument_shape() {
    // FR-2, the constructor shape (dynamic case CPP_118/CPP_120): `Box b(&a)` passes a pointer
    // to a known `Box`, which only the `Box*` parameter accepts, so the construction is a
    // SINGLE edge to `Box::Box#1(Box*)`. The `int` candidate is not a target -- that is what
    // keeps the negative (`sink(b.get())`, where the pointer body writes the constant 0) from
    // reporting a flow.
    let src = r"
        struct Box {
            int v;
            int w;
            Box(int x) : v(x), w(0) {}
            Box(Box* o) : v(0), w(o->v) {}
        };
        int main() {
            Box a(1);
            Box b(&a);
            return 0;
        }";
    let prog = program_from_cpp_string(src).0;
    let ctor_calls: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(callees, _)| callees)
        .collect();
    assert_eq!(
        ctor_calls,
        vec![
            vec!["Box::Box#1(int)".to_string()],
            vec!["Box::Box#1(Box*)".to_string()],
        ],
        "each construction must reach exactly the one overload its argument can mean\n{prog}"
    );
    check_direct_call_arg0(&prog, "main", "Box::Box#1(Box*)", "b");
}

#[test_log::test]
fn cpp_method_call_resolves_by_argument_shape() {
    // FR-2, the method shape (dynamic case CPP_119) and its scalar mirror: `b.set(a)` passes a
    // known `Box` OBJECT, which only the `Box&` parameter accepts, while `b.set(source())`
    // passes a shape the frontend cannot name -- and the only candidate that accepts an
    // unclassifiable argument is the `int` one. Each is one target, not two.
    let src = r"
        int source();
        struct Box {
            int v;
            int w;
            void set(int x) { v = x; }
            void set(Box& o) { w = o.v; }
        };
        int main() {
            Box a;
            Box b;
            b.set(a);
            b.set(source());
            return 0;
        }";
    let prog = program_from_cpp_string(src).0;
    let targets: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(callees, _)| callees)
        .filter(|callees| callees.iter().any(|c| c.starts_with("Box::set")))
        .collect();
    assert_eq!(
        targets,
        vec![
            vec!["Box::set#1(Box&)".to_string()],
            vec!["Box::set#1(int)".to_string()],
        ],
        "an object argument selects the reference overload and a scalar one the int \
         overload\n{prog}"
    );
}

#[test_log::test]
fn cpp_pointer_shaped_arguments_resolve_to_the_pointer_overload() {
    // FR-2: every spelling of "a pointer to a known class" resolves the same way -- `&a`, a
    // `Box*` local, a `Box*` parameter, and `new Box`. The free-function shape (dynamic probe
    // `p6d`) proves the same resolution runs on the ordinary call path, not just at a
    // construction.
    let src = r"
        struct Box { int v; };
        int f(int x) { return x + 1; }
        int f(Box* p) { return p->v; }
        int via_param(Box* q) { return f(q); }
        int main() {
            Box a;
            Box* p = &a;
            int r = f(&a) + f(p) + f(new Box) + via_param(p);
            return r;
        }";
    let prog = program_from_cpp_string(src).0;
    for (caller, n) in [("main", 3), ("via_param", 1)] {
        let to_ptr = direct_calls_in(&prog, caller)
            .iter()
            .filter(|(callees, _)| callees == &vec!["f#1(Box*)".to_string()])
            .count();
        assert_eq!(
            to_ptr, n,
            "every pointer-shaped argument in {caller:?} must select `f#1(Box*)`\n{prog}"
        );
    }
    assert!(
        !direct_calls_in(&prog, "main")
            .iter()
            .any(|(callees, _)| callees.iter().any(|c| c == "f#1(int)")),
        "no pointer-shaped argument may cross-resolve to the int overload\n{prog}"
    );
}

#[test_log::test]
fn cpp_object_shaped_arguments_see_through_a_dereference() {
    // FR-2: `*p` through a `Box*` is the OBJECT the pointer names, so it selects the `Box&`
    // overload -- while `p` itself selects the `Box*` one. The two shapes are read off the same
    // neutral maps, so one spelling cannot drift from the other.
    let src = r"
        struct Box {
            int v;
            int w;
            void set(Box* o) { v = o->v; }
            void set(Box& o) { w = o.v; }
        };
        int main() {
            Box a;
            Box* p = &a;
            Box b;
            b.set(p);
            b.set(*p);
            return 0;
        }";
    let prog = program_from_cpp_string(src).0;
    let targets: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .iter()
        .map(|(callees, _)| callees.clone())
        .collect();
    assert_eq!(
        targets,
        vec![
            vec!["Box::set#1(Box*)".to_string()],
            vec!["Box::set#1(Box&)".to_string()],
        ],
        "a pointer selects the pointer overload and its dereference the reference one\n{prog}"
    );
}

#[test_log::test]
fn cpp_ambiguous_scalar_overloads_are_one_call_with_two_edges() {
    // FR-3 (dynamic case CPP_122): `Box(int)` and `Box(long)` differ only in a scalar type, and
    // this slice does not type scalar expressions -- so `Box b(source())` could mean either.
    // The construction is ONE call statement whose target list holds BOTH keys (the multi-target
    // `DirectCall` a virtual call already emits), which is a sound superset of the one C++
    // selects, rather than a guess at whichever body happened to be defined first.
    let src = r"
        int source();
        struct Box {
            int v;
            int w;
            Box(int x) : v(x), w(0) {}
            Box(long x) : v(0), w((int)x) {}
        };
        int main() {
            Box b(source());
            return 0;
        }";
    let prog = program_from_cpp_string(src).0;
    let ctor: Vec<Vec<String>> = direct_calls_in(&prog, "main")
        .into_iter()
        .map(|(callees, _)| callees)
        .filter(|callees| callees.iter().any(|c| c.starts_with("Box::Box")))
        .collect();
    assert_eq!(
        ctor.len(),
        1,
        "an ambiguous construction is one call, not one per candidate\n{prog}"
    );
    assert_eq!(
        ctor[0],
        vec![
            "Box::Box#1(int)".to_string(),
            "Box::Box#1(long)".to_string()
        ],
        "both candidates must be edges of that one call\n{prog}"
    );
    // Both bodies exist as functions of their own, so each edge reaches a real body.
    check_func_assign_or_update(&prog, "Box::Box#1(int)", "@p0.v", ["@p1"]);
    check_func_assign_or_update(&prog, "Box::Box#1(long)", "@p0.w", ["@p1"]);
}

#[test_log::test]
fn cpp_copy_ctor_still_wins_over_a_same_arity_sibling() {
    // FR-2 (dynamic case CPP_121): the copy constructor's key (`Box::Box#copy`, spec 022) is not
    // a member of the arity group at all, so `Box b(a)` must still reach it while `Box c(&a)`
    // resolves by shape to `Box::Box#1(Box*)` -- three one-argument constructors, three
    // functions, three distinct edges.
    let src = r"
        struct Box {
            int v;
            int w;
            Box(int x) : v(x), w(0) {}
            Box(Box* o) : v(0), w(o->v) {}
            Box(const Box& o) : v(0), w(o.v) {}
        };
        int main() {
            Box a(1);
            Box b(a);
            Box c(&a);
            return 0;
        }";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "main", "Box::Box#1(int)", ["a", "#1"]);
    check_direct_call_arg0(&prog, "main", "Box::Box#copy", "b");
    check_direct_call_arg0(&prog, "main", "Box::Box#1(Box*)", "c");
    // The copy constructor keeps its own parameters and its own body, and is keyed apart from
    // the arity group rather than joining it.
    check_func_loads(&prog, "Box::Box#copy", "@p1.v");
    assert!(
        function_named(&prog, "Box::Box#1(constBox&)").is_none(),
        "the copy constructor must not also be signature-mangled into the arity group\n{prog}"
    );
}

#[test_log::test]
fn cpp_byvalue_class_parameter_is_copied_on_a_multi_target_call() {
    // FR-3, second half: the by-value-class-argument record (spec 022 FR-3) is consulted PER
    // target, so a parameter that is by value on a candidate of a multi-target call still gets
    // its copy. Here `take(int)` and `take(Box)` are one arity and the argument is an object, so
    // the `Box` candidate is selected and its by-value parameter copy-constructs -- and the copy
    // is emitted exactly once.
    let src = r"
        struct Box {
            int v;
            Box(int x) : v(x) {}
            Box(const Box& o) : v(o.v) {}
        };
        int take(int x) { return x; }
        int take(Box b) { return b.v; }
        int main() {
            Box a(1);
            int r = take(a);
            return r;
        }";
    let prog = program_from_cpp_string(src).0;
    let copies = direct_calls_in(&prog, "main")
        .iter()
        .filter(|(callees, _)| callees.iter().any(|c| c == "Box::Box#copy"))
        .count();
    assert_eq!(
        copies, 1,
        "a by-value class argument gets exactly one copy, however many targets the call \
         has\n{prog}"
    );
    check_has_direct_call(&prog, "main", "take#1(Box)");
}

#[test_log::test]
fn c_same_named_prototypes_and_definitions_are_unchanged() {
    // FR-5 / Principle II: for C the `overloads` map stays empty, so the mangler is the identity
    // and `overload_targets` returns the single bare name -- a C program with repeated
    // prototypes of one function has exactly the one function and the one edge it always had,
    // and no name acquires an arity or a signature.
    let src = r"
        int f(int x);
        int f(int x);
        int f(int x) { return x + 1; }
        int main() {
            int r = f(1);
            return r;
        }";
    let prog = program_from_string(src).0;
    check_func_params(&prog, "f", &[ParameterType::ByVal]);
    let calls = direct_calls_in(&prog, "main");
    assert_eq!(calls.len(), 1, "{prog}");
    assert_eq!(calls[0].0, vec!["f".to_string()], "{prog}");
    assert!(
        prog.functions
            .functions
            .raw
            .iter()
            .all(|fun| !fun.name.contains('#') && !fun.name.contains('(')),
        "no C name may be arity- or signature-mangled\n{prog}"
    );
}

#[test_log::test]
fn cpp_derived_object_matches_a_base_parameter() {
    // FR-2, the derived-for-base half of the shape rule: a `Derived` object can mean a `Base&`
    // parameter, so `take(d)` selects the reference overload even though the argument's class is
    // not literally `Base` -- the match walks the neutral base chain (`class_is_a`), which is
    // plain equality for every base-less class and for all of C.
    let src = r"
        struct Base { int v; };
        struct Derived : Base { int w; };
        int take(int x) { return x; }
        int take(Base& b) { return b.v; }
        int main() {
            Derived d;
            int r = take(d);
            return r;
        }";
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "main", "take#1(Base&)");
    assert!(
        !direct_calls_in(&prog, "main")
            .iter()
            .any(|(callees, _)| callees.iter().any(|c| c == "take#1(int)")),
        "a derived object must not cross-resolve to the int overload\n{prog}"
    );
}

// ---- Spec 025: named and functional casts ---------------------------------------------------

#[test_log::test]
fn cpp_named_casts_are_their_operand() {
    // FR-1: `static_cast<T>(e)` and kin parse as a call whose callee is a `template_function`;
    // each is the value of its operand, so the parameter reaches the return through every one,
    // and no call to a function named after the cast is emitted.
    let src = r"
        long a(int p) { return static_cast<long>(p); }
        long b(int p) { return reinterpret_cast<long>(p); }
        int c(const int p) { return const_cast<int>(p); }
        int d(int p) { return dynamic_cast<int>(p); }";
    let prog = program_from_cpp_string(src).0;
    for name in ["a", "b", "c", "d"] {
        assert!(
            direct_calls_in(&prog, name).is_empty(),
            "a named cast is not a call in `{name}`\n{prog}"
        );
    }
    let (s, si) = get_summary(prog).unwrap();
    for name in ["a", "b", "c", "d"] {
        check_returns_param_in(&s, &si, name, 0, "");
    }
}

#[test_log::test]
fn cpp_functional_casts_are_their_operand() {
    // FR-2: `int(e)` (a `primitive_type` callee), `long(e)` / `unsigned(e)` (the grammar reads a
    // lone size keyword as an `identifier`) and `wide_t(e)` for a typedef name are conversions.
    let src = r"
        typedef long wide_t;
        int a(int p) { return int(p); }
        long b(int p) { return long(p); }
        unsigned c(int p) { return unsigned(p); }
        wide_t d(int p) { return wide_t(p); }";
    let prog = program_from_cpp_string(src).0;
    for name in ["a", "b", "c", "d"] {
        assert!(
            direct_calls_in(&prog, name).is_empty(),
            "a functional cast is not a call in `{name}`\n{prog}"
        );
    }
    let (s, si) = get_summary(prog).unwrap();
    for name in ["a", "b", "c", "d"] {
        check_returns_param_in(&s, &si, name, 0, "");
    }
}

#[test_log::test]
fn cpp_cast_in_store_position_names_the_operand() {
    // FR-1 on the location side: `*static_cast<int*>(p) = v` stores through `p`, exactly as
    // `*p = v` does.
    let src = r"
        void put(int* p, int v) { *static_cast<int*>(p) = v; }";
    let prog = program_from_cpp_string(src).0;
    check_func_assign_or_update(&prog, "put", "@p0.deref", ["@p1"]);
}

#[test_log::test]
fn cpp_call_to_a_function_is_not_a_cast() {
    // The boundary: a callee naming a function is still a call, even one whose name a typedef
    // elsewhere could have claimed.
    let src = r"
        int conv(int x) { return x; }
        int f(int p) { return conv(p); }";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "f", "conv", ["@p0"]);
}

// ---- Spec 026: range-based for ----------------------------------------------------------------

#[test_log::test]
fn cpp_range_for_body_is_lowered() {
    // FR-1: the loop and its body lower; a `for_range_loop` used to fall into the statement
    // walker's expression catch-all, which dropped the body with it.
    let src = r"
        int f(int p) {
            int a[2] = {1, 2};
            int r = 0;
            for (int x : a) { r = p; }
            return r;
        }";
    let (s, _si) = get_summary(program_from_cpp_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn cpp_range_for_value_variable_reads_an_element() {
    // FR-2: a by-value variable is a copy of the element location a non-constant subscript
    // names (`a.deref`), loaded in the loop header.
    let src = r"
        int f(int* a) {
            int r = 0;
            for (int x : a) r = x;
            return r;
        }";
    let prog = program_from_cpp_string(src).0;
    check_func_loads(&prog, "f", "@p0.deref");
    let (s, _si) = get_summary(prog).unwrap();
    check_returns_param(&s, 0, ".deref");
}

#[test_log::test]
fn cpp_range_for_reference_variable_writes_the_range() {
    // FR-3: a reference variable aliases the element, so a store through it is a store to the
    // range's memory, and nothing is copied into a local of that name.
    let src = r"
        void fill(int* a, int v) {
            for (int& y : a) y = v;
        }";
    let prog = program_from_cpp_string(src).0;
    check_func_assign_or_update(&prog, "fill", "@p0.deref", ["@p1"]);
}

#[test_log::test]
fn cpp_range_for_braced_list_blends_every_value() {
    // FR-4: a braced range has no memory; the variable takes every listed value.
    let src = r"
        int f(int p, int q) {
            int s = 0;
            for (int x : {p, 1, q}) s = x;
            return s;
        }";
    let (s, _si) = get_summary(program_from_cpp_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
    check_returns_param(&s, 1, "");
}

#[test_log::test]
fn cpp_range_for_variable_is_scoped_to_the_loop() {
    // The loop variable shadows an outer `x` only inside the loop: the outer one is returned.
    let src = r"
        int f(int p, int* a) {
            int x = p;
            for (int x : a) { x = 0; }
            return x;
        }";
    let (s, _si) = get_summary(program_from_cpp_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

// ---- Spec 027: temporaries --------------------------------------------------------------------

#[test_log::test]
fn cpp_temporary_is_constructed_and_passed() {
    // FR-1: `B(p)` on a class constructs a fresh object through `B::B` (never a call to a
    // function `B`), and the object is the value passed on.
    let src = r"
        struct B { int v; B(int x) : v(x) {} };
        int take(const B& b) { return b.v; }
        int f(int p) { return take(B(p)); }";
    let prog = program_from_cpp_string(src).0;
    let calls = direct_calls_in(&prog, "f");
    assert!(
        calls
            .iter()
            .any(|(callees, _)| callees == &["B::B".to_string()]),
        "the temporary runs its constructor\n{prog}"
    );
    assert!(
        !calls
            .iter()
            .any(|(callees, _)| callees == &["B".to_string()]),
        "no call to a function named after the class\n{prog}"
    );
    let (s, si) = get_summary(prog).unwrap();
    check_returns_param_in(&s, &si, "f", 0, "");
}

#[test_log::test]
fn cpp_temporary_receiver_dispatches() {
    // FR-2: a method called on a temporary dispatches with the constructed object as receiver.
    let src = r"
        struct B { int v; B(int x) : v(x) {} int get() { return v; } };
        int f(int p) { return B(p).get(); }";
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "f", "B::get");
    let (s, si) = get_summary(prog).unwrap();
    check_returns_param_in(&s, &si, "f", 0, "");
}

#[test_log::test]
fn cpp_brace_temporary_runs_the_constructor() {
    // FR-3: `B{p}` on a class with a constructor is a construction; `P{p, 0}` on a
    // constructor-less class is aggregate initialization, member by member.
    let src = r"
        struct B { int v; B(int x) : v(x) {} int get() { return v; } };
        struct P { int a; int b; };
        int f(int p) { return B{p}.get(); }
        int g(int p) { P q = P{p, 0}; return q.a; }
        int h(int p) { P q = P{0, p}; return q.a; }";
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "f", "B::B");
    let (s, si) = get_summary(prog).unwrap();
    check_returns_param_in(&s, &si, "f", 0, "");
    check_returns_param_in(&s, &si, "g", 0, "");
    check_does_not_return_param_in(&s, &si, "h", 0, "");
}

#[test_log::test]
fn cpp_temporary_destructor_runs_at_scope_exit() {
    // FR-4: the temporary joins the enclosing scope's destructor frame.
    let src = r"
        struct G { int v; G(int x) : v(x) {} ~G() { v = 0; } };
        int f(int p) { int r = G(p).v; return r; }";
    let prog = program_from_cpp_string(src).0;
    assert!(
        dtor_calls_per_block(&prog, "f")
            .iter()
            .flatten()
            .any(|c| c == "G::~G"),
        "the temporary is destroyed\n{prog}"
    );
}

// ---- Spec 028: default construction and default member initializers --------------------------

#[test_log::test]
fn cpp_default_declaration_runs_the_default_ctor() {
    // FR-1: `B b;` constructs `b` through the niladic constructor, with `b` as the receiver.
    let src = r"
        struct B { int v; B() : v(0) {} };
        int main() { B b; return b.v; }";
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "main", "B::B");
}

#[test_log::test]
fn cpp_ctor_applies_default_member_initializers() {
    // FR-2: a constructor stores the default of every member its list leaves out, and not of
    // one it names.
    let src = r"
        struct B { int v = 1; int w = 2; B(int x) : w(x) {} };";
    let prog = program_from_cpp_string(src).0;
    check_func_assign_or_update(&prog, "B::B", "@p0.v", ["#1"]);
    check_func_assign_or_update(&prog, "B::B", "@p0.w", ["@p1"]);
    assert_eq!(
        function_named(&prog, "B::B")
            .map(|f| format!("{f}").matches("@p0.w").count())
            .unwrap_or_default(),
        1,
        "w is initialized once, by the list\n{prog}"
    );
}

#[test_log::test]
fn cpp_implicit_default_ctor_is_synthesized_for_default_member_initializers() {
    // FR-3: a constructor-less class with defaults gets `B::B(this)` storing them; a class with
    // neither gets nothing.
    let src = r"
        struct B { int v = 7; int w{8}; };
        struct Plain { int v; };
        int main() { B b; Plain p; return b.v + p.v; }";
    let prog = program_from_cpp_string(src).0;
    check_func_params(&prog, "B::B", &[ParameterType::ByRef]);
    check_func_assign_or_update(&prog, "B::B", "@p0.v", ["#7"]);
    check_func_assign_or_update(&prog, "B::B", "@p0.w", ["#8"]);
    check_has_direct_call(&prog, "main", "B::B");
    assert!(
        function_named(&prog, "Plain::Plain").is_none(),
        "no constructor is invented for a class without defaults\n{prog}"
    );
}

// ---- Spec 029: subobject construction ---------------------------------------------------------

#[test_log::test]
fn cpp_member_object_initializer_runs_its_ctor() {
    // FR-2: `: in(x)` on a member object constructs it through `In::In` on the value loaded
    // from `this.in`, then stores that value back (copy-in/copy-out), so the member the
    // constructor wrote is `this.in.v`. `main` reads `o.in.v` so that path is materialized.
    let src = r"
        struct In { int v; In(int x) : v(x) {} };
        struct Out { In in; int w; Out(int x) : in(x), w(0) {} };
        int main() { Out o(1); return o.in.v; }";
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "Out::Out", "In::In");
    let (s, si) = get_summary(prog).unwrap();
    check_flow_in(&s, &si, "Out::Out", 1, "", 0, ".in.v");
}

#[test_log::test]
fn cpp_base_initializer_runs_the_base_ctor_on_this() {
    // FR-1: `: Base(x)` constructs the base subobject on the object itself (`@p0`), and a base
    // the list leaves out is default-constructed.
    let src = r"
        struct Base { int v; Base(int x) : v(x) {} };
        struct Other { int u; Other() : u(0) {} };
        struct D : Base, Other { D(int x) : Base(x) {} };";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "D::D", "Base::Base", ["@p0", "@p1"]);
    check_direct_call(&prog, "D::D", "Other::Other", ["@p0"]);
    let (s, si) = get_summary(prog).unwrap();
    check_flow_in(&s, &si, "D::D", 1, "", 0, ".v");
}

#[test_log::test]
fn cpp_implicit_ctor_constructs_bases_and_member_objects() {
    // FR-3: a constructor-less class whose base or member object has a constructor gets the
    // implicit default constructor, which runs both; a member of a constructor-less class is
    // left alone.
    let src = r"
        struct Base { int v; Base() : v(1) {} };
        struct In { int w; In() : w(2) {} };
        struct Plain { int p; };
        struct D : Base { In in; Plain pl; };
        int main() { D d; return d.v; }";
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "D::D", "Base::Base");
    check_has_direct_call(&prog, "D::D", "In::In");
    check_has_direct_call(&prog, "main", "D::D");
    assert!(
        function_named(&prog, "Plain::Plain").is_none(),
        "no constructor is invented for a plain member\n{prog}"
    );
}

// ---- Spec 030: member-object receivers and the destruction chain ------------------------------

#[test_log::test]
fn cpp_member_object_receivers_dispatch() {
    // FR-1: `in.set(x)` inside a method, `this->in.set(x)` and `o.in.set(x)` dispatch to the
    // member's class, on the value loaded from the member, and store it back after the call.
    let src = r"
        struct In { int v; void set(int x) { v = x; } };
        struct Out {
            In in;
            void put(int x) { in.set(x); }
            void put2(int x) { this->in.set(x); }
        };
        int main() { Out o; o.in.set(1); return 0; }";
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "Out::put", "In::set");
    check_has_direct_call(&prog, "Out::put2", "In::set");
    check_has_direct_call(&prog, "main", "In::set");
    for name in ["Out::put", "Out::put2"] {
        assert_eq!(
            count_writes_in(&prog, name, "@p0.in"),
            1,
            "the member is written back once in {name}\n{prog}"
        );
    }
}

#[test_log::test]
fn cpp_local_shadowing_a_member_object_is_not_a_receiver() {
    // A local that shadows the member is not the member: no dispatch on `in`.
    let src = r"
        struct In { int v; void set(int x) { v = x; } };
        struct Out { In in; void put(int x) { int in = x; in = in + 1; } };";
    let prog = program_from_cpp_string(src).0;
    assert!(
        direct_calls_in(&prog, "Out::put").is_empty(),
        "a shadowing scalar local is not a member-object receiver\n{prog}"
    );
}

#[test_log::test]
fn cpp_scope_exit_runs_the_destruction_chain() {
    // FR-2: destroying a `D : Base` with a member object runs `~D`, then the member's `~In`,
    // then `~Base` -- each once, in that order.
    let src = r"
        struct In { int v; ~In() { v = 0; } };
        struct Base { int b; ~Base() { b = 0; } };
        struct D : Base { In in; ~D() { b = 1; } };
        int main() { { D d; } return 0; }";
    let prog = program_from_cpp_string(src).0;
    let calls: Vec<String> = dtor_calls_per_block(&prog, "main")
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(
        calls,
        vec!["D::~D", "In::~In", "Base::~Base"],
        "destruction order\n{prog}"
    );
}

#[test_log::test]
fn cpp_delete_runs_the_destruction_chain() {
    // FR-2 for a non-virtual `delete`: the static type's own destructor, members, then bases.
    let src = r"
        struct Base { int b; ~Base() { b = 0; } };
        struct D : Base { ~D() { b = 1; } };
        int main() { D* p = new D(); delete p; return 0; }";
    let prog = program_from_cpp_string(src).0;
    let calls: Vec<String> = dtor_calls_per_block(&prog, "main")
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(calls, vec!["D::~D", "Base::~Base"], "{prog}");
}

// ---- Spec 031: member operators ---------------------------------------------------------------

#[test_log::test]
fn cpp_binary_and_unary_operators_dispatch_to_members() {
    // FR-1: `a + b`, `-a` and `*a` on objects call the class's member operators with the
    // object as receiver; a class without the operator keeps the built-in lowering.
    let src = r"
        struct S { int v; int operator+(int x) { return v + x; } int operator-() { return v; }
                   int operator*() { return v; } };
        struct P { int v; };
        int f(int y) { S a; P b; int r = a + y; int s = -a; int t = *a; return r + s + t + b.v; }";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "f", "S::operator+", ["a", "@p0"]);
    check_direct_call(&prog, "f", "S::operator-", ["a"]);
    check_direct_call(&prog, "f", "S::operator*", ["a"]);
    assert_eq!(direct_calls_in(&prog, "f").len(), 3, "{prog}");
}

#[test_log::test]
fn cpp_increment_operators_pass_the_postfix_dummy() {
    // FR-2: `++c` is `operator++()`, `c++` is `operator++(0)`; with both defined they resolve
    // by arity.
    let src = r"
        struct C { int v; C& operator++() { v = v + 1; return *this; }
                   C operator++(int) { C old = *this; v = v + 1; return old; } };
        void f() { C c; ++c; c++; }";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "f", "C::operator++#0", ["c"]);
    check_direct_call(&prog, "f", "C::operator++#1", ["c", "#0"]);
}

#[test_log::test]
fn cpp_pointer_arithmetic_is_not_an_operator_call() {
    // A pointer to a class is not an object: `p + 1` stays pointer arithmetic even though the
    // class declares `operator+`.
    let src = r"
        struct V { int v; int operator+(int x) { return x; } };
        V* f(V* p) { return p + 1; }";
    let prog = program_from_cpp_string(src).0;
    assert!(direct_calls_in(&prog, "f").is_empty(), "{prog}");
}

#[test_log::test]
fn cpp_operator_chain_dispatches_on_the_returned_receiver() {
    // FR-3: `s << 1 << 2` -- the outer `<<` dispatches on the object the inner one returned.
    let src = r"
        struct S { int v; S& operator<<(int x) { v = x; return *this; } };
        void f() { S s; s << 1 << 2; }";
    let prog = program_from_cpp_string(src).0;
    let calls = direct_calls_in(&prog, "f");
    assert_eq!(
        calls
            .iter()
            .filter(|(c, _)| c == &["S::operator<<".to_string()])
            .count(),
        2,
        "{prog}"
    );
}

// ---- Spec 032: free operators and operator-> --------------------------------------------------

#[test_log::test]
fn cpp_free_operator_is_lowered_and_called() {
    // FR-1/FR-2: a free `operator+` is lowered under its spelling, and `a + b` on objects calls
    // it with both operands; scalars keep the built-in `+`.
    let src = r"
        struct S { int v; };
        int operator+(const S& a, const S& b) { return a.v + b.v; }
        int f(int x) { S a; S b; return (a + b) + (x + 1); }";
    let prog = program_from_cpp_string(src).0;
    assert!(function_named(&prog, "operator+").is_some(), "{prog}");
    check_direct_call(&prog, "f", "operator+", ["a", "b"]);
    assert_eq!(direct_calls_in(&prog, "f").len(), 1, "{prog}");
}

#[test_log::test]
fn cpp_free_operators_resolve_by_operand_class() {
    // FR-2: same-arity free operators for two classes are two functions, and each use reaches
    // the one its operands can mean.
    let src = r"
        struct A { int v; }; struct B { int v; };
        int operator+(const A& x, const A& y) { return x.v; }
        int operator+(const B& x, const B& y) { return y.v; }
        int f() { A a; B b; return (a + a) + (b + b); }";
    let prog = program_from_cpp_string(src).0;
    let calls = direct_calls_in(&prog, "f");
    assert_eq!(calls.len(), 2, "{prog}");
    assert!(
        calls
            .iter()
            .all(|(targets, _)| targets.len() == 1 && targets[0].starts_with("operator+#2(")),
        "each use resolves to one signature-keyed operator\n{prog}"
    );
    assert_ne!(calls[0].0, calls[1].0, "{prog}");
}

#[test_log::test]
fn cpp_overloaded_arrow_reads_through_the_returned_pointer() {
    // FR-3: `w->v` on an object whose class declares `operator->` loads `v` from what
    // `w.operator->()` returns.
    let src = r"
        struct In { int v; };
        struct P { In* p; In* operator->() { return p; } };
        int f() { P w; return w->v; }";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "f", "P::operator->", ["w"]);
}

// ---- Spec 033: templates (type-erased) -------------------------------------------------------

#[test_log::test]
fn cpp_class_template_instances_are_the_template() {
    // FR-1: `Box<int> b` is an object of `Box`, its methods (inline and out-of-line, written
    // `Box<T>::get`) are `Box::…`, and `new Box<int>()` / `Box<int>(x)` construct a `Box`.
    let src = r"
        template <class T> struct Box { T v; Box(T x) : v(x) {} void set(T x) { v = x; } T get(); };
        template <class T> T Box<T>::get() { return v; }
        int f(int p) {
            Box<int> b(0); b.set(p);
            Box<int>* q = new Box<int>(p);
            return b.get() + q->get() + Box<int>(p).get();
        }";
    let prog = program_from_cpp_string(src).0;
    assert!(function_named(&prog, "Box::get").is_some(), "{prog}");
    check_has_direct_call(&prog, "f", "Box::set");
    check_has_direct_call(&prog, "f", "Box::get");
    assert_eq!(
        direct_calls_in(&prog, "f")
            .iter()
            .filter(|(c, _)| c == &["Box::Box".to_string()])
            .count(),
        3,
        "declaration, new and temporary each construct a Box\n{prog}"
    );
}

#[test_log::test]
fn cpp_explicit_template_arguments_name_the_template() {
    // FR-2: `twice<int>(x)` calls `twice`, and `M<int>::id(x)` calls `M::id` -- direct calls,
    // where `twice<int>` used to be an indirect call through a temp.
    let src = r"
        template <typename T> T twice(T x) { return x + x; }
        template <class T> struct M { static T id(T x) { return x; } };
        int f(int p) { return twice<int>(p) + M<int>::id(p); }";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "f", "twice", ["@p0"]);
    check_direct_call(&prog, "f", "M::id", ["@p0"]);
}

#[test_log::test]
fn cpp_member_template_is_a_method() {
    // FR-3: a member template inside a class body is lowered as a method (its name parses as an
    // `identifier` there).
    let src = r"
        struct S { int v; template <class T> void set(T x) { v = x; } };
        void f(int p) { S s; s.set(p); }";
    let prog = program_from_cpp_string(src).0;
    check_func_assign_or_update(&prog, "S::set", "@p0.v", ["@p1"]);
    check_has_direct_call(&prog, "f", "S::set");
}

#[test_log::test]
fn cpp_type_aliases_name_their_class() {
    // FR-4: `using IntBox = Box<int>;` and `typedef Box<int> B2;` declare objects of `Box`.
    let src = r"
        template <class T> struct Box { T v; void set(T x) { v = x; } };
        using IntBox = Box<int>;
        typedef Box<int> B2;
        void f(int p) { IntBox a; B2 b; a.set(p); b.set(p); }";
    let prog = program_from_cpp_string(src).0;
    assert_eq!(
        direct_calls_in(&prog, "f")
            .iter()
            .filter(|(c, _)| c == &["Box::set".to_string()])
            .count(),
        2,
        "{prog}"
    );
}

// ---- Spec 034: lambdas -------------------------------------------------------------------------

#[test_log::test]
fn cpp_lambda_is_a_function_and_its_closure_a_pointer_to_it() {
    // FR-1: the lambda's body is the function `lambda@<row>:<col>`, and the closure is a
    // function pointer, so `f(x)` is an indirect call.
    let src = r"
        int f(int p) {
            auto g = [](int x) { return x + 1; };
            return g(p);
        }";
    let prog = program_from_cpp_string(src).0;
    let lambda = prog
        .functions
        .functions
        .raw
        .iter()
        .find(|fun| fun.name.starts_with("lambda@"))
        .unwrap_or_else(|| panic!("no lambda function\n{prog}"));
    assert_eq!(lambda.params.len(), 1, "{prog}");
    let text = format!("{}", function_named(&prog, "f").unwrap());
    assert!(text.contains("funcptr-call"), "{prog}");
}

#[test_log::test]
fn cpp_lambda_captures_share_global_slots() {
    // FR-2/FR-3: a by-reference capture makes the enclosing function's `y` a global slot the
    // lambda's function names too; a by-value capture is copied into the lambda's own slot.
    let src = r"
        int f(int p) {
            int y = 0;
            int x = p;
            auto g = [&y, x]() { y = x; };
            g();
            return y;
        }";
    let prog = program_from_cpp_string(src).0;
    let f = format!("{}", function_named(&prog, "f").unwrap());
    assert!(
        f.contains("<ref ") && f.contains(" y>"),
        "y lives in a ref slot\n{prog}"
    );
    assert!(
        f.contains("<val ") && f.contains(" x>"),
        "x is copied to a val slot\n{prog}"
    );
    let lambda = prog
        .functions
        .functions
        .raw
        .iter()
        .find(|fun| fun.name.starts_with("lambda@"))
        .map(|fun| format!("{fun}"))
        .unwrap_or_default();
    assert!(
        lambda.contains("<ref ") && lambda.contains("<val "),
        "the body names both slots\n{prog}"
    );
}

#[test_log::test]
fn cpp_lambda_implicit_capture_excludes_its_own_names() {
    // FR-3: under `[&]`, a name the lambda declares itself is not a capture.
    let src = r"
        void f() {
            int y = 0;
            auto g = [&]() { int y = 1; y = y + 1; };
            g();
        }";
    let prog = program_from_cpp_string(src).0;
    let f = format!("{}", function_named(&prog, "f").unwrap());
    assert!(!f.contains("<ref "), "the outer y is not captured\n{prog}");
}

// ---- Spec 035: implicit member calls -----------------------------------------------------------

#[test_log::test]
fn cpp_unqualified_method_call_dispatches_on_this() {
    // FR-1: `get()` inside a method is `this->get()`; a virtual one dispatches by CHA.
    let src = r"
        struct Base { int v; virtual int get() { return v; } int twice() { return get() + get(); }
                      virtual ~Base() {} };
        struct D : Base { int get() { return 0; } };";
    let prog = program_from_cpp_string(src).0;
    let calls = direct_calls_in(&prog, "Base::twice");
    assert_eq!(calls.len(), 2, "{prog}");
    for (targets, args) in calls {
        assert!(
            targets.contains(&"Base::get".to_string()) && targets.contains(&"D::get".to_string()),
            "CHA over the subtree\n{prog}"
        );
        assert_eq!(args.len(), 1, "the receiver is this\n{prog}");
    }
}

#[test_log::test]
fn cpp_unqualified_static_member_call_is_qualified() {
    // FR-2: a static member function called unqualified is `Class::m(args)`, with no receiver.
    let src = r"
        struct S { int v; static int id(int x) { return x; } void set(int x) { v = id(x); } };";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "S::set", "S::id", ["@p1"]);
}

#[test_log::test]
fn cpp_local_shadowing_a_method_is_not_a_member_call() {
    // A local named like a method shadows it: `get()` on a local functor calls its
    // `operator()`, not `S::get`.
    let src = r"
        struct F { int operator()() { return 0; } };
        struct S { int get() { return 1; } int f() { F get; return get(); } };";
    let prog = program_from_cpp_string(src).0;
    let calls = direct_calls_in(&prog, "S::f");
    assert!(
        !calls.iter().any(|(t, _)| t.contains(&"S::get".to_string())),
        "{prog}"
    );
    assert!(
        calls
            .iter()
            .any(|(t, _)| t.contains(&"F::operator()".to_string())),
        "{prog}"
    );
}

// ---- Spec 036: reference results ---------------------------------------------------------------

#[test_log::test]
fn cpp_assignment_through_a_reference_getter_stores_the_member() {
    // FR-1: `b.ref() = x`, where `ref` returns the member `v` by reference, stores to `b.v`;
    // `int& r = b.ref()` binds `r` to it.
    let src = r"
        struct B { int v; int& ref() { return v; } };
        void f(B& b, int x) { b.ref() = x; }
        void g(B& b, int x) { int& r = b.ref(); r = x; }";
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "f", "B::ref");
    check_func_assign_or_update(&prog, "f", "@p0.v", ["@p1"]);
    check_func_assign_or_update(&prog, "g", "@p0.v", ["@p1"]);
}

#[test_log::test]
fn cpp_returned_member_object_is_a_receiver() {
    // FR-2: `o.inner().set(x)`, where `inner` returns the member object `in` by reference,
    // dispatches `In::set` on `o.in` and writes it back.
    let src = r"
        struct In { int v; void set(int x) { v = x; } };
        struct Out { In in; In& inner() { return in; } };
        void f(Out& o, int x) { o.inner().set(x); }";
    let prog = program_from_cpp_string(src).0;
    check_has_direct_call(&prog, "f", "In::set");
    assert_eq!(count_writes_in(&prog, "f", "@p0.in"), 1, "{prog}");
}

// ---- Spec 037: name lookup ---------------------------------------------------------------------

#[test_log::test]
fn cpp_unqualified_call_finds_the_enclosing_namespace() {
    // FR-1: `id(x)` inside `ns::wrap` is `ns::id`, which hides a global `id`; a method of a
    // namespaced class finds it too.
    let src = r"
        int id(int x) { return 0; }
        namespace ns {
            int id(int x) { return x; }
            int wrap(int x) { return id(x); }
            struct S { int v; void set(int x) { v = id(x); } };
        }";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "ns::wrap", "ns::id", ["@p0"]);
    check_direct_call(&prog, "ns::S::set", "ns::id", ["@p1"]);
}

#[test_log::test]
fn cpp_using_makes_a_namespace_member_visible() {
    // FR-2: `using namespace ns;` and `using ns::id;` resolve unqualified functions and
    // classes.
    let src = r"
        namespace ns { int id(int x) { return x; } struct B { int v; int get() { return v; } }; }
        namespace m { int twice(int x) { return x + x; } }
        using namespace ns;
        using m::twice;
        int f(int p) { B b; b.v = p; return id(p) + twice(p) + b.get(); }";
    let prog = program_from_cpp_string(src).0;
    check_direct_call(&prog, "f", "ns::id", ["@p0"]);
    check_direct_call(&prog, "f", "m::twice", ["@p0"]);
    check_has_direct_call(&prog, "f", "ns::B::get");
}

#[test_log::test]
fn cpp_nested_class_is_named_qualified_and_out_of_line() {
    // FR-3: `Outer::Inner i;` is an `Inner`, and `int Outer::Inner::get()` is its method.
    let src = r"
        struct Outer { struct Inner { int v; int get(); }; };
        int Outer::Inner::get() { return v; }
        int f() { Outer::Inner i; return i.get(); }";
    let prog = program_from_cpp_string(src).0;
    assert!(function_named(&prog, "Inner::get").is_some(), "{prog}");
    check_has_direct_call(&prog, "f", "Inner::get");
}

// ---- Spec 038: arrays of objects ---------------------------------------------------------------

#[test_log::test]
fn cpp_object_array_elements_are_constructed() {
    // FR-1: `B arr[2];` constructs each element through `B::B`.
    let src = r"
        struct B { int v; B() : v(0) {} };
        int main() { B arr[2]; return arr[0].v; }";
    let prog = program_from_cpp_string(src).0;
    assert_eq!(
        direct_calls_in(&prog, "main")
            .iter()
            .filter(|(t, _)| t == &["B::B".to_string()])
            .count(),
        2,
        "{prog}"
    );
}

#[test_log::test]
fn cpp_object_array_element_is_a_receiver() {
    // FR-2: `arr[1].set(x)` dispatches `B::set` on the element and writes it back; a class
    // pointer declared without an initializer is a receiver too.
    let src = r"
        struct B { int v; void set(int x) { v = x; } };
        void f(int x) { B arr[2]; arr[1].set(x); B* p; p = &arr[0]; p->set(x); }";
    let prog = program_from_cpp_string(src).0;
    assert_eq!(
        direct_calls_in(&prog, "f")
            .iter()
            .filter(|(t, _)| t == &["B::set".to_string()])
            .count(),
        2,
        "{prog}"
    );
}
