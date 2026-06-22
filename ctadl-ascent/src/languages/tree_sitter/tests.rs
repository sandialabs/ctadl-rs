use ctadl_ir::ParameterType::{ByRef, ByVal};
use ctadl_ir::{StatementKind, Variable};

use crate::languages::tree_sitter::test_utils::*;

#[test_log::test]
fn simple_function() {
    let src = r"
            void simple() {}
        ";
    let prog = program_from_string(src).0;
    check_block_count(&prog, 1);
}

#[test_log::test]
fn simple_assign() {
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
    let src = r"
            void basic_params(int x, int *y) {}
        ";
    let prog = program_from_string(src).0;
    check_params(&prog, &[ByVal, ByRef]);
}

#[test_log::test]
fn basic_param_flow() {
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
    // Each binary sub-expression gets a fresh temporary; within one function the TempAllocator must
    // hand out distinct, ascending names with no reuse. This is inherently a naming/allocation
    // property (temporaries are identified by the `<t...>` convention), but we assert it on the IR
    // rather than substring-matching the dump.
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
    // A bare lexical scope `{ ... }` is scoping, not control flow: it must NOT become its own
    // basic block. The function should have exactly one block.
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
    // `b = a;` as a standalone statement (NOT a declarator initializer) lowers to a plain assign.
    // Exercises the expression-statement path, distinct from `simple_assign`'s declarator path.
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
    // Comma-separated declarators each lower to their own assignment.
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
    // Redundant parens around a condition are peeled, and an assignment used as a condition still
    // lowers normally: `if((x = z))` / `while((((y = z))))`.
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
    // A direct call's argument flows through the callee and back: `tgt` returns `x.f1`, and `top`
    // returns the result of `tgt(y)`, so param 0's `.f1` field reaches the return. Asserting the
    // flow (not the `direct-call tgt` rendering) proves the call resolved AND data flows through it.
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
    // A value assigned inside an UNBRACED `if` consequent still reaches the return: `x = z` (z is
    // param 1) under `if(x == 3)`, then `return x` => param 1 flows to the return.
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
    // An unbraced `if` whose consequent returns a field of a by-ref param: `return fb->unbraced`
    // => param 0's `.unbraced` field flows to the return. (The other path returns a global.)
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
    // Field writes are summarized as effects on the formal (no temp names needed), including
    // blended RHSs. `return v.f1` (set from `b.xyz`) is captured both as the formal field-return
    // and the resolved source. Params: Donkey v = @p0, Burro* b = @p1, int x = @p2, int y = @p3.
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
    // `v.f = x` lowers to a functional `update` on the base (not a plain assign). Syntactic claim
    // the summary can't see; one representative case is enough.
    let src = r"
        int f(Donkey v, int x) {
            v.f2 = x;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "@p0.f2", ["@p1"], None);
}

#[test_log::test]
fn chained_assignment() {
    // `b = a = 5;` — the inner assignment's value propagates outward, so `b` receives `a`.
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
    // A numeric literal lowers to a constant source (`Exp::Str` of the source text), both as a
    // statement assignment (`a = 5`) and a declarator initializer (`int x = 17`). Literals buried
    // in a blend, or returned, are out of scope (see CLAUDE.md / constants notes).
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
    // An `if` with no early return: block 0 (condition) branches to the consequent (1) or the
    // fallthrough (2); the consequent falls through to 2; block 2 returns (terminal).
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
    // `if` whose consequent returns: block 0 branches to 1 or 2; both are terminal returns.
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
    // The dataflow facet of the same shape: `return x` on one path and `return y` on the other
    // means BOTH params reach the return.
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
    // While loop block structure: 0 enters the header (1); the condition (1) branches to the body
    // (3) or the exit (2); the body branches back to the header (1, the back-edge); 2 returns.
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
    // A constant array subscript becomes an `Offset` segment on the access path, both as an rvalue
    // (`f[3]`) and an lvalue (`f[4] = ...`, which lowers to an `update`). `int x` is @p2.
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
    // `v->f4 = v->f5 + b` with `b = v->f1 + v->f3`: the blended RHS (direct f5, plus f1/f3 routed
    // through b) all flow into the field update @p0.f4. v = @p0.
    // `f` is declared `void`: it returns nothing, so falling off the end is a consistent
    // (arity-0) implicit return. The test exercises field-update dataflow on the param, not the
    // return value.
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
    // Every operand of a nested/parenthesized sum reaches the result: `a + b + c + (d + e)` => all
    // five params flow to the return. (Covers flattening completeness, which `unique_temps` does
    // not — that only checks temp allocation.)
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
    // A blended expression used directly as a return value preserves all operand flows:
    // `return a + x` => both params reach the return.
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
    // `else if` desugars into a nested if inside the outer else: the outer condition (block 0)
    // branches to its consequence (block 1) and the else-branch (block 3); block 3 is itself the
    // inner condition, branching to the elif consequence (block 4) and the final else (block 6).
    // Each arm flows to its own continuation and the arms rejoin at the return (block 2).
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
    // A function's return arity comes from its declared return type: a value-returning `int`
    // function is arity 1, a `void` function is arity 0. (tree-sitter doesn't support implicit-int
    // returns, so every function here has an explicit signature -- see issue #54 for the
    // implicit-int aspirational case.) This fixture has several functions, so it exercises
    // `function_named` rather than `get_only_function`.
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
    // `return (14)` lowers to a Return terminator carrying the literal as a constant
    // (Exp::Str of the literal's source text), not an access path. The parens are just grouping.
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
    // A direct call carries its arguments as access paths: `foo(y)` passes param 0 directly. The
    // call result is discarded, so there is no summary flow -- this is purely call-site IR shape.
    // (`foo(y + y)` flattens its argument into a temp; we deliberately do not assert that temp
    // name, which would freeze TempAllocator numbering -- flattening is covered by the blend tests.)
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
    // A nested call `foo(baz(y))` lowers to two direct calls, not an assignment: `bar` directly
    // calls both `baz` and `foo`. The results are discarded, so this is verifiable only as
    // call-site shape, not as a summary flow.
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
    // An assignment in argument position is lowered as a real assignment before the call:
    // `bar(x = y)` emits `assign %x = @p0`. (Recursive self-call keeps this a single function so
    // check_assign_or_update applies.)
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
