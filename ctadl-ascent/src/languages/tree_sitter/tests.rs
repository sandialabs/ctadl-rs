use ctadl_ir::ParameterType::{ByRef, ByVal};
use ctadl_ir::{StatementKind, Variable};

use crate::languages::tree_sitter::test_utils::*;

#[test_log::test]
fn simple_function() {
    // An empty function body. Even with no statements, it lowers to exactly one basic block.
    let src = r"
            void simple() {}
        ";
    let prog = program_from_string(src).0;
    check_block_count(&prog, 1);
}

#[test_log::test]
fn simple_assign() {
    // Copying one variable into another (`int b = a;`). Lowers to a single `assign b = a`, and a
    // straight-line body is a single basic block.
    let src = r"
            int simple_assign() {
                int a = 5;
                int b = a;
                return b;
            }
        ";
    let prog = program_from_string(src).0;
    check_block_count(&prog, 1);
    check_assign_or_update(&prog, "b", ["a"], None);
}

#[test_log::test]
fn simple_assign_expr() {
    // A binary operator inside a declarator (`int c = a + b;`). An assign can't hold a compound
    // expression, so the add spills into a temporary: `<t0> = a, b` then `c = <t0>`.
    let src = r"
            int simple_assign_expr() {
                int a = 5;
                int b = 4;
                int c = a + b;
                return c;
            }
        ";
    let (prog, _dump) = program_from_string(src);
    check_block_count(&prog, 1);
    check_assign_or_update(&prog, "<t0>", ["a", "b"], None);
    check_assign_or_update(&prog, "c", ["<t0>"], None);
}

#[test_log::test]
fn simple_assign_global() {
    // Reading a name with no local declaration (`int b = a;`, where `a` was never declared). CTADL
    // resolves it to a global, so the assignment's source is `$globals.a`.
    let src = r"
            int simple_assign_global() {
                int b = a;
                return b;
            }
        ";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "b", ["$globals.a"], None);
}

#[test_log::test]
#[ignore = "assigning to an undeclared global as a target is WIP; un-ignore once supported and confirmed"]
fn simple_global_assign() {
    // Writing to a name with no local declaration (`a = b;`). This should resolve to a global store,
    // `$globals.a = b` -- but writing an undeclared global as the *target* isn't supported yet, so
    // the test is ignored (see the attribute).
    let src = r"
            int simple_global_assign() {
                int b;
                a = b;
                return a;
            }
        ";
    let (prog, dump) = program_from_string(src);
    log::info!("{}", dump);
    check_assign_or_update(&prog, "$globals.a", ["b"], None);
}

#[test_log::test]
fn basic_params() {
    // How parameters are passed: a plain `int x` is by-value, a pointer `int *y` is by-reference.
    let src = r"
            void basic_params(int x, int *y) {}
        ";
    let prog = program_from_string(src).0;
    check_params(&prog, &[ByVal, ByRef]);
}

#[test_log::test]
fn basic_param_flow() {
    // Returning a parameter unchanged (`return x;`). The function summary holds a single flow:
    // param 0 reaches the return.
    let src = r"
            int basic_param_flow(int x) {
                return x;
            }
        ";
    let prog = program_from_string(src).0;
    check_params(&prog, &[ByVal]);

    let summary = get_summary(prog).unwrap().0;
    check_summary_count(&summary, 1);
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn param_flows_through_local() {
    // Returning a parameter after bouncing it through a local (`int b = x; return b;`). The local
    // copy is invisible to the summary -- it still reports param 0 reaching the return.
    let src = r"
            int param_flows_through_local(int x) {
                int b = x;
                return b;
            }
        ";
    let prog = program_from_string(src).0;
    check_params(&prog, &[ByVal]);
    check_assign_or_update(&prog, "b", ["@p0"], None);

    let summary = get_summary(prog).unwrap().0;
    check_summary_count(&summary, 1);
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn return_from_pointer() {
    // Returning a dereferenced pointer parameter (`return *y;`). CTADL doesn't distinguish `*y` from
    // `y`, so the summary is identical to returning the param directly: param 0 reaches the return.
    let src = r"
            int return_from_pointer(int *y) {
                return *y;
            }
        ";
    let prog = program_from_string(src).0;
    check_params(&prog, &[ByRef]);

    let summary = get_summary(prog).unwrap().0;
    check_summary_count(&summary, 1);
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn return_from_pointer_through_local() {
    // Returning a dereferenced pointer parameter through a local (`int b = *y; return b;`). The
    // pointer deref and the local copy are both transparent -- param 0 still reaches the return.
    let src = r"
            int return_from_pointer_through_local(int *y) {
                int b = *y;
                return b;
            }
        ";
    let prog = program_from_string(src).0;
    check_params(&prog, &[ByRef]);
    check_assign_or_update(&prog, "b", ["@p0"], None);

    let summary = get_summary(prog).unwrap().0;
    check_summary_count(&summary, 1);
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn unique_temps() {
    // Several binary expressions in one function. Each operator that needs flattening gets its own
    // temporary; this checks the allocator hands out distinct, gap-free names <t0>..<t4> across the
    // whole function (no reuse, no extras). That's a property of how temporaries are numbered, so we
    // read the names off the IR rather than substring-matching the dump.
    let src = r"
        void fun(){
            int z = n + p + r + q;   // <t0>, <t1>, <t2>
            int v = a + b;           // <t3>
            int n = m + x;           // <t4>
        }
    ";
    let prog = program_from_string(src).0;
    let fun = get_only_function(&prog).expect("expected exactly one function");

    // Collect the names of every temporary that appears as an assignment destination.
    let temp_dests: Vec<String> = fun
        .blocks
        .iter()
        .flat_map(|b| b.statements.iter())
        .filter_map(|stmt| match &stmt.kind {
            StatementKind::Assign { dest, .. } => match dest.variable.as_ref() {
                Variable::Local(name) if name.starts_with("<t") => Some(name.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();

    // No temporary is assigned more than once...
    let distinct: std::collections::BTreeSet<String> = temp_dests.iter().cloned().collect();
    assert_eq!(
        temp_dests.len(),
        distinct.len(),
        "a temporary was assigned more than once: {temp_dests:?}"
    );

    // ...and exactly <t0>..<t4> were allocated (ascending, no gaps, no extras).
    let expected: std::collections::BTreeSet<String> = (0..5).map(|i| format!("<t{i}>")).collect();
    assert_eq!(
        distinct, expected,
        "expected exactly <t0>..<t4> as temporaries"
    );
}

#[test_log::test]
fn scopes_arent_blocks() {
    // A bare `{ ... }` block with no control flow. It only introduces a lexical scope, so it does
    // *not* start a new basic block -- the function stays at one block.
    let src = r"
        int bar() {
            {
                int x;
            }
            return 0;
        }";
    let prog = program_from_string(src).0;
    check_block_count(&prog, 1);
}

#[test_log::test]
fn assignment_statement() {
    // Assignment as a standalone statement (`b = a;`), not a declaration initializer. It lowers to
    // the same `assign b = a`, but through the expression-statement path rather than the declarator
    // path that `simple_assign` covers.
    let src = r"
        int f() {
            int a = 5;
            int b;
            b = a;
            return b;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "b", ["a"], None);
}

#[test_log::test]
fn comma_list_declarations() {
    // Several initialized declarators on one line (`int x = a, y = b, z = 7;`). Each becomes its own
    // `assign` (and `z` takes the literal 7 as a constant source).
    let src = r"
        int comma_sep_decl() {
            int a, b, c, d;
            int x = a, y = b, z = 7;
            return x + y;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "x", ["a"], None);
    check_assign_or_update(&prog, "y", ["b"], None);
    check_assign_or_update(&prog, "z", ["#7"], None); // z = 7 (literal)
}

#[test_log::test]
fn extra_parens() {
    // Redundant parentheses around a condition that is itself an assignment: `if((x = z))` and
    // `while((((y = z))))`. The extra parens are peeled and the embedded assignment lowers normally
    // to `assign x = z` / `assign y = z`.
    let src = r"
        int extra_parens() {
            int z = 55;
            int x, y;
            if((x = z)) {
            }
            while((((y = z)))) {
            }
            return 0;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "x", ["z"], None);
    check_assign_or_update(&prog, "y", ["z"], None);
}

#[test_log::test]
fn call_arg_flows_through_return() {
    // A function whose return value is the result of calling another (`top` returns `tgt(y)`, and
    // `tgt` returns `x.f1`). End to end, param 0's `.f1` field reaches `top`'s return. Asserting this
    // flow -- rather than that a `direct-call tgt` was emitted -- proves both that the call resolved
    // and that data passes through it.
    let src = r"
        int tgt(Rando x) {
            return x.f1;
        }
        int top(Rando y) {
            int v = tgt(y);
            return v;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 0, "f1");
}

#[test_log::test]
fn unbraced_if_branch_flows_to_return() {
    // An `if` with an unbraced single-statement body (`if(x == 3) x = z;`). The assignment in the
    // braceless body is not dropped: `z` (param 1) flows through `x` to the return.
    let src = r"
        int f(int y, int z) {
            int x = 5;
            if(x == 3)
                x = z;
            return x;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn unbraced_if_returns_param_field() {
    // An unbraced `if` whose body is a `return` of a pointer field (`if(...) return fb->unbraced;`).
    // The summary reports param 0's `.unbraced` field reaching the return. (The fall-through path
    // returns a global, which is not a param flow.)
    let src = r"
        int f(Foobar *fb) {
            if(fb->ct == 3)
                return fb->unbraced;
            return x;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 0, "unbraced");
}

#[test_log::test]
fn field_write_flows() {
    // Writing to struct fields, with right-hand sides ranging from a plain param to a deep
    // pointer-load to a sum (params: v=@p0, b=@p1, x=@p2, y=@p3). CTADL summarizes each as a flow
    // into the formal's field, with no temporaries leaking out: x -> v.f2, the deep load
    // b->f2.f3->f4 -> v.f2.nf1.y, and each operand of b->fa + b->fb -> v.f5. Returning a just-written
    // field (`return v.f1`) shows up twice: as @p0.f1 -> return and as its resolved original source
    // b.xyz -> return.
    let src = r"
        int field_access(Donkey v, Burro* b, int x, int y) {
            v.f2 = x;
            v.f2.nf1.y = b->f2.f3->f4;
            v.f5 = b->fa + b->fb;
            v.f3 = x + y + z;
            v.f1 = b.xyz;
            return v.f1;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    // direct + deep-path field writes
    check_flow(&s, 2, "", 0, "f2");
    check_flow(&s, 1, "f2.f3.f4", 0, "f2.nf1.y");
    // blended RHS feeding a field (temp-free)
    check_flow(&s, 1, "fa", 0, "f5");
    check_flow(&s, 1, "fb", 0, "f5");
    check_flow(&s, 2, "", 0, "f3");
    check_flow(&s, 3, "", 0, "f3");
    // value-field source + field-write-then-return
    check_flow(&s, 1, "xyz", 0, "f1");
    check_returns_param(&s, 0, "f1"); // formal field returned
    check_returns_param(&s, 1, "xyz"); // resolved b.xyz reaches return
}

#[test_log::test]
fn field_assignment_is_update() {
    // Storing into a struct field (`v.f2 = x;`). In the IR this is a functional `update` on the base
    // value (`@p0` updated at `.f2`), not a plain `assign`. The summary can't see this distinction,
    // so it's checked on the IR; one representative case is enough.
    let src = r"
        int f(Donkey v, int x) {
            v.f2 = x;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "@p0.f2", ["@p1"], None);
}

#[test_log::test]
fn chained_assignment() {
    // A chained assignment (`int b = a = 5;`). The inner `a = 5` runs first and its value propagates
    // outward, so `b` is assigned from `a`: IR is `assign a = 5` then `assign b = a`.
    let src = r"
        int f() {
            int a;
            int b = a = 5;
            int c = a + b;
            return c;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "a", ["#5"], None); // inner `a = 5` (literal)
    check_assign_or_update(&prog, "b", ["a"], None);
}

#[test_log::test]
fn literal_assignments() {
    // Numeric literals as assignment sources, both as a statement (`a = 5;`) and a declarator
    // (`int x = 17;`). A literal lowers to a constant source -- the literal's source text, not an
    // access path. (Literals buried inside a sum, or returned, are covered elsewhere.)
    let src = r"
        int f() {
            int a;
            a = 5;
            int x = 17;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "a", ["#5"], None);
    check_assign_or_update(&prog, "x", ["#17"], None);
}

#[test_log::test]
fn if_fallthrough_cfg() {
    // An `if` with no early return -- control falls out of the body and continues. CFG: block 0 (the
    // condition) branches to the body (1) or the continuation (2); the body falls through to 2; and
    // block 2 returns, so it is terminal.
    let src = r"
        int f(int x, int y) {
            if(x) {
                x = x + 21;
            }
            return y;
        }";
    let prog = program_from_string(src).0;
    check_block_count(&prog, 3);
    check_successors(&prog, 0, &[1, 2]);
    check_successors(&prog, 1, &[2]);
    check_successors(&prog, 2, &[]);
}

#[test_log::test]
fn if_return_in_consequent_cfg() {
    // An `if` whose body returns (`if(x) return x; return y;`). CFG: block 0 branches to the body (1)
    // or the fall-through (2), and both are terminal returns -- nothing rejoins.
    let src = r"
        int f(int x, int y) {
            if(x) {
                return x;
            }
            return y;
        }";
    let prog = program_from_string(src).0;
    check_block_count(&prog, 3);
    check_successors(&prog, 0, &[1, 2]);
    check_successors(&prog, 1, &[]);
    check_successors(&prog, 2, &[]);
}

#[test_log::test]
fn if_both_branches_can_return_params() {
    // The dataflow view of the same shape (`if(x) return x; return y;`): there are two possible
    // return paths, so the summary reports *both* params 0 and 1 reaching the return.
    let src = r"
        int f(int x, int y) {
            if(x) {
                return x;
            }
            return y;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
    check_returns_param(&s, 1, "");
}

#[test_log::test]
fn while_loop_cfg() {
    // A `while` loop's block structure. CFG: block 0 sets up and enters the header (1); the
    // header/condition (1) branches to the body (3) or the exit (2); the body loops back to the
    // header (1, the back-edge); block 2 runs the post-loop code and returns. Also checks the body
    // assignment lands in block 3 and the post-loop one in block 2.
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
    let prog = program_from_string(src).0;
    check_block_count(&prog, 4);
    check_successors(&prog, 0, &[1]);
    check_successors(&prog, 1, &[2, 3]);
    check_successors(&prog, 3, &[1]);
    check_successors(&prog, 2, &[]);
    // body and post-loop assignments land in the right blocks
    check_assign_or_update(&prog, "x", ["b"], Some(3));
    check_assign_or_update(&prog, "y", ["x"], Some(2));
}

#[test_log::test]
fn subscript_access_paths() {
    // A constant array subscript, read and written (`x = f[3];` and `f[4] = x;`). The subscript
    // becomes a `.[N]` segment on the access path (a symbol segment, not a numeric offset): the read
    // is `assign @p2 = f.[3]`, and the write lowers to an `update` of `f` at `.[4]`. (`int x` is @p2.)
    let src = r"
        int brackets_simple(Donkey v, Burro* b, int x, int y) {
            int f = 1;
            x = f[3];
            f[4] = x;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "@p2", ["f.[3]"], None); // x = f[3]
    check_assign_or_update(&prog, "f.[4]", ["@p2"], None); // f[4] = x  (update)
}

#[test_log::test]
fn field_blend_into_field_update() {
    // A field store whose right-hand side is a sum mixing a direct field load with a value routed
    // through a local: `v->f4 = v->f5 + b`, where `b = v->f1 + v->f3`. All three source fields flow
    // into the updated field: f5 directly, f1 and f3 via `b`, all into @p0.f4. `f` is `void`, so it
    // legitimately has no return -- this test is about the field-update flow, not a return value.
    let src = r"
        void f(Donkey *v) {
            int a = b = v->f1 + v->f3;
            int x = v->f4 = v->f5 + b;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 0, "f5", 0, "f4"); // direct
    check_flow(&s, 0, "f1", 0, "f4"); // via b
    check_flow(&s, 0, "f3", 0, "f4"); // via b
}

#[test_log::test]
fn nested_blend_operands_flow() {
    // A nested/parenthesized sum (`a + b + c + (d + e)`). Every operand survives flattening into
    // temporaries: all five params flow to the return. (unique_temps only checks the temps are
    // allocated; this checks none of the operands gets lost on the way.)
    let src = r"
        int f(int a, int b, int c, int d, int e) {
            int x = a + b + c + (d + e);
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    for p in 0..5 {
        check_returns_param(&s, p, "");
    }
}

#[test_log::test]
fn returned_blend_operands_flow() {
    // A sum used directly as the return value (`return a + x;`). Both operands flow to the return --
    // flattening a blended expression in return position keeps every source.
    let src = r"
        int g(int a, int x) {
            return a + x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
    check_returns_param(&s, 1, "");
}

/*

#[test_log::test]
#[ignore = "Issue 54: Implicit return"]
fn implicit_return() {
    let src = r"
            int foo() {
            //no explicit_return
            }
        ";
    let (program, _dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    let (_, _) = get_summary(program_info).expect("Verify probably bonked");
}
//TODO_JDB:  I don't think i handled *(p+1) = f; or (p+1)->f()

 */

#[test_log::test]
fn simple_else() {
    // A plain `if/else`. The CFG is a four-block diamond: the condition (0) branches to the if-body
    // (1) or the else (3), each arm assigns its own variable, and both rejoin at the terminal
    // return block (2).
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
    let (program, _dump) = program_from_string(src);

    // The assignments land in the if-consequence (block 1) and the else (block 3).
    check_assign_or_update(&program, "v_if", ["x"], Some(1));
    check_assign_or_update(&program, "v_else", ["x"], Some(3));

    // CFG shape: the condition (block 0) branches to the consequence and the else only
    // (never directly to the join); both rejoin at the return block, which is terminal.
    check_successors(&program, 0, &[1, 3]);
    check_successors(&program, 1, &[2]);
    check_successors(&program, 3, &[2]);
    check_successors(&program, 2, &[]);
}

#[test_log::test]
fn simple_elif() {
    // C `if / else if / else`. Tree-sitter has no "elif", so `else if` desugars to a nested `if` in
    // the outer else. In the IR that is two condition blocks, each branching to exactly its two arms:
    // block 0 -> [1,3] (if-body / else-branch) and block 3 -> [4,6] (elif-body / final else); all
    // arms reconverge at the terminal return block (2). What this pins down: each condition branches
    // only to its own two arms, with no stray edge jumping straight to the join.
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
    let (program, _dump) = program_from_string(src);

    check_assign_or_update(&program, "v_if", ["x"], Some(1));
    check_assign_or_update(&program, "v_elif", ["x"], Some(4));
    check_assign_or_update(&program, "v_else", ["x"], Some(6));

    // No condition block over-approximates to the join: each branches only to its two arms.
    check_successors(&program, 0, &[1, 3]); // outer condition -> if-consequence, else-branch
    check_successors(&program, 1, &[2]); // if-consequence -> join
    check_successors(&program, 3, &[4, 6]); // inner (elif) condition -> elif-consequence, else
    check_successors(&program, 4, &[5]); // elif-consequence -> its continuation
    check_successors(&program, 5, &[2]); // continuation -> join
    check_successors(&program, 6, &[5]); // final else -> continuation
    check_successors(&program, 2, &[]); // join: returns, terminal
}

#[test_log::test]
fn return_arity() {
    // Where a function's return arity comes from: its declared return type. An `int` function is
    // arity 1, a `void` function arity 0 (whether it `return;`s or just falls off the end).
    // tree-sitter can't parse implicit-int returns, so every function here is explicitly typed (see
    // issue #54). The fixture has several functions, so it looks them up by name.
    let src = r"
            int explicit(){return 0;}
            void none(){return;}
            void really_void(void){return;}
        ";
    let prog = program_from_string(src).0;
    check_return_arity(&prog, "explicit", 1);
    check_return_arity(&prog, "none", 0);
    check_return_arity(&prog, "really_void", 0);
}

#[test_log::test]
fn return_constant() {
    // Returning a parenthesized literal (`return (14);`). The parens are just grouping; the return
    // carries the literal as a constant (its source text "14"), not a variable or access path.
    let src = r"
            int return_constant() {
                return (14);
            }
        ";
    let prog = program_from_string(src).0;
    check_returns_const(&prog, "return_constant", "14");
}

#[test_log::test]
fn params_into_calls() {
    // Passing an argument to a call (`foo(y)`). It lowers to a direct call whose argument is the
    // access path for param 0. The result is unused, so there is no summary flow -- this is purely
    // about the call-site IR. (`foo(y + y)` flattens its argument into a temp first; we don't assert
    // the temp name, which would only pin down allocator numbering -- flattening is covered elsewhere.)
    let src = r"
        int foo(Rando x){
            return x;
        }
        int bar(int y){
            foo(y);
            foo(y + y);
            return y;
        }
        ";
    let prog = program_from_string(src).0;
    check_direct_call(&prog, "bar", "foo", ["@p0"]);
}

#[test_log::test]
fn call_not_assign() {
    // A call used as another call's argument (`foo(baz(y))`). It lowers to two direct calls, not an
    // assignment: `bar` directly calls `baz` (with param 0) and `foo` (whose argument is baz's result
    // temp). Both results are discarded, so this is only visible as call-site IR shape, not a flow.
    let src = r"
        int foo(Rando x){
            return x;
        }
        int baz(Rando m){
            return m + m;
        }
        int bar(Rando y){
            foo(baz(y));
            return y;
        }
        ";
    let prog = program_from_string(src).0;
    check_direct_call(&prog, "bar", "baz", ["@p0"]); // inner call gets the param directly
    check_has_direct_call(&prog, "bar", "foo"); // outer call resolves (its arg is baz's ret temp)
}

#[test_log::test]
fn assign_in_call_arg() {
    // An assignment sitting in an argument position (`bar(x = y)`). The assignment is lowered as a
    // real statement before the call: `assign x = y` (param 0). (`bar` calls itself so the program
    // stays a single function, which is what check_assign_or_update needs.)
    let src = r"
        int bar(int y){
            int x;
            bar(x = y);
            return y;
        }
        ";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "x", ["@p0"], None);
}

#[test_log::test]
fn compound_assign_accumulates() {
    // Compound assignment (`y += b`) is an accumulate, not an overwrite: it lowers to `y = b + y`,
    // keeping the prior value of `y` *and* mixing in the new one. With `y` seeded from param 0 and
    // `+=` adding param 1, both params reach the return. param 1 is the load-bearing assertion --
    // if `+=` were dropped (or lowered as a plain `y = b`), one of these two flows would vanish.
    let src = r"
        int f(int a, int b) {
            int y = a;
            y += b;
            return y;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, ""); // old value of y (param 0) survives the +=
    check_returns_param(&s, 1, ""); // += mixes param 1 in
}

#[test_log::test]
fn increment_decrement_reassign_local() {
    // `x++` and `--x` on a local each lower to a writeback assignment to `x` (`x = x +/- 1`). The
    // `+/- 1` is a constant, so there is no dataflow to assert -- the contract is purely structural:
    // each increment/decrement re-assigns the variable. Counting the assignments whose destination
    // is `x` (init + two updates = 3) guards that neither was dropped, without pinning the flatten
    // temp names. (`++` and `--` lower identically; the operator distinction is not preserved.)
    let src = r"
        int f(int a) {
            int x = a;
            x++;
            --x;
            return x;
        }";
    let prog = program_from_string(src).0;
    let fun = get_only_function(&prog).expect("expected exactly one function");
    let assigns_to_x = fun
        .blocks
        .iter()
        .flat_map(|b| b.statements.iter())
        .filter(|stmt| {
            matches!(&stmt.kind,
                StatementKind::Assign { dest, .. }
                    if matches!(dest.variable.as_ref(), Variable::Local(n) if n == "x"))
        })
        .count();
    assert_eq!(
        assigns_to_x, 3,
        "expected `int x = a`, `x++`, `--x` to each write x (3 assigns)\n{prog}"
    );
}

#[test_log::test]
fn field_increment_is_update() {
    // Incrementing through a field (`p->x++`) routes through the functional `update` path on the
    // formal, exactly like a field store does (see `field_assignment_is_update`) -- it is not a plain
    // assign. The new value is a flatten temp (`@p0.x + 1`), so we assert only that an `update` of
    // `@p0.x` exists, leaving the temp source unpinned.
    let src = r"
        void f(Field *p) {
            p->x++;
        }";
    let prog = program_from_string(src).0;
    let fun = get_only_function(&prog).expect("expected exactly one function");
    let updates_p0_x = fun
        .blocks
        .iter()
        .flat_map(|b| b.statements.iter())
        .any(|stmt| {
            matches!(&stmt.kind,
            StatementKind::Update { dest, .. }
                if matches!(dest.0.variable.as_ref(), Variable::Param(_))
                    && dest.1.fields.iter().map(|f| f.to_string()).eq(["x".to_string()]))
        });
    assert!(
        updates_p0_x,
        "expected `p->x++` to lower to an update of @p0.x\n{prog}"
    );
}
