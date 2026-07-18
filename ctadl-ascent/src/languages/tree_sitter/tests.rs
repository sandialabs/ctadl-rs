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
    // Reading the global `a` lowers to a load of `$globals.a` (into a temp that flows to `b`).
    check_loads(&prog, "$globals.a");
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
    // Operands are parameters (read as bare variables, no load temps) so the only temporaries
    // are the ones each binary operation allocates.
    let src = r"
        void fun(int n, int p, int r, int q, int a, int b, int m, int x){
            int z = n + p + r + q;   // <t0>, <t1>, <t2>
            int v = a + b;           // <t3>
            int w = m + x;           // <t4>
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
fn block_shadow_does_not_leak() {
    // A nested block re-declares `x` (`if(...) { int x = false_return; }`), shadowing the outer `x`.
    // That inner `x` is a distinct, block-scoped variable, so `return x` refers to the OUTER `x`
    // (= ac_return, param 1). The shadow must not escape its block: param 1 reaches the return, and
    // param 0 (false_return, assigned only to the inner shadow) does NOT. The param-0 absence is the
    // load-bearing assertion -- if the inner `x` were conflated with the outer one, false_return
    // would leak to the return.
    let src = r"
        int bar(int false_return, int ac_return) {
            int x = ac_return;
            if(x == 5) {
                int x = false_return;
            }
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 1, ""); // outer x (ac_return) is what gets returned
    check_does_not_return_param(&s, 0, ""); // the block-scoped shadow (false_return) must not leak
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
fn do_while_cfg() {
    // A `do { ... } while(...)` loop. The defining difference from `while` is that the body runs
    // *before* the condition: block 0 sets up and falls into the body (1); the body (1) falls into
    // the condition (2); the condition (2) either back-edges to the body (1) or exits to the
    // continuation (3); block 3 runs the post-loop code and returns. (Contrast while_loop_cfg, where
    // block 0 enters the condition first.) The body assignment lands in block 1.
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
    let prog = program_from_string(src).0;
    check_block_count(&prog, 4);
    check_successors(&prog, 0, &[1]); // setup falls into the body, not a condition
    check_successors(&prog, 1, &[2]); // body falls into the condition
    check_successors(&prog, 2, &[1, 3]); // condition: back-edge to body, or exit
    check_successors(&prog, 3, &[]); // continuation returns, terminal
    check_assign_or_update(&prog, "x", ["b"], Some(1)); // body statement
}

#[test_log::test]
fn do_while_body_flows() {
    // Taint traverses a do-while body. The body assigns `x = p` (param 0); after the loop, `return x`
    // carries param 0 to the return. Since a do-while runs its body at least once, the flow holds
    // regardless of the condition -- and would vanish if the body were dropped (x would stay the
    // constant 0).
    let src = r"
        int f(int p) {
            int x = 0;
            do {
                x = p;
            } while(x);
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

// The three tests below pin control-flow *combinations* (an if nested in a loop, two sequential ifs,
// an if followed by a loop). Each was historically a "no errant goto 0" dump check: the worry was a
// construct wiring a stray edge back into the entry block. Asserting the full successor graph is the
// structural form of that guarantee -- block 0 never appears as a successor -- and also pins the
// reconvergence/back-edge wiring these combinations introduce.

#[test_log::test]
fn while_with_nested_if_cfg() {
    // A `while` whose body contains an `if` and a trailing statement. CFG: setup (0) enters the
    // condition (1); the condition exits to the continuation (2, which returns) or the body (3); the
    // body branches on the inner if to its consequence (4) or straight to the if-join (5); the
    // consequence (4) falls into the join (5); the join (5) back-edges to the condition (1). The
    // back-edge targets the condition, never the entry block.
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
    let prog = program_from_string(src).0;
    check_block_count(&prog, 6);
    check_successors(&prog, 0, &[1]);
    check_successors(&prog, 1, &[2, 3]); // condition: exit or enter body
    check_successors(&prog, 2, &[]); // continuation returns, terminal
    check_successors(&prog, 3, &[4, 5]); // body: inner-if branch
    check_successors(&prog, 4, &[5]); // if-consequence joins
    check_successors(&prog, 5, &[1]); // if-join back-edges to the condition, not entry
}

#[test_log::test]
fn sequential_ifs_cfg() {
    // Two `if`s back-to-back with no return in between (the function falls off the end). CFG: two
    // diamonds chained -- the first if's condition (0) branches to its consequence (1) or its
    // continuation (2); that continuation doubles as the second if's condition, branching to the
    // second consequence (3) or the final continuation (4, terminal). Neither diamond branches back
    // to the entry. (An if *not* followed by a return was the original "goto 0" trigger.)
    let src = r"
        int f(int y, int z) {
            int x = 5;
            if(x == 3)
                x = z;
            if(y == z)
                x = y;
        }";
    let prog = program_from_string(src).0;
    check_block_count(&prog, 5);
    check_successors(&prog, 0, &[1, 2]); // first if
    check_successors(&prog, 1, &[2]);
    check_successors(&prog, 2, &[3, 4]); // second if (lives in the first if's continuation)
    check_successors(&prog, 3, &[4]);
    check_successors(&prog, 4, &[]); // falls off the end, terminal
}

#[test_log::test]
fn if_then_while_cfg() {
    // An unbraced `if` immediately followed by an unbraced `while`. CFG: the if's condition (0)
    // branches to its consequence (1) or continuation (2); the continuation (2) flows into the while
    // condition (3); the while condition exits to the continuation (4, which returns) or the body
    // (5); the body (5) back-edges to the while condition (3). The two constructs chain in sequence
    // with no edge back to the entry.
    let src = r"
        int f(int y, int z) {
            int x = 5;
            if(x == 3)
                x = z;
            while(x == 5)
                x = y;
            return x;
        }";
    let prog = program_from_string(src).0;
    check_block_count(&prog, 6);
    check_successors(&prog, 0, &[1, 2]); // if
    check_successors(&prog, 1, &[2]);
    check_successors(&prog, 2, &[3]); // if-continuation flows into the while condition
    check_successors(&prog, 3, &[4, 5]); // while condition: exit or body
    check_successors(&prog, 4, &[]); // continuation returns, terminal
    check_successors(&prog, 5, &[3]); // body back-edges to the while condition, not entry
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
    check_loads(&prog, "f.[3]"); // x = f[3]  (read lowers to a load of f.[3])
    check_assign_or_update(&prog, "f.[4]", ["@p2"], None); // f[4] = x  (store)
}

#[test_log::test]

fn array_declaration_element_flows_to_return() {
    // An explicit array *declaration* `int arr[3];` now ingests (previously
    // "ERR 78: Unsupported expression type: array_declarator"). Taint written to an
    // element flows back out when the same element is read: `b` (@p1) -> arr.[1] -> return.
    // (Subscript access itself already worked; this exercises the declaration arm.)
    let src = r"
        int f(int a, int b) {
            int arr[3];
            arr[1] = b;
            return arr[1];
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
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
fn unbraced_if_else_cfg() {
    // An `if/else` whose arms are *unbraced* single statements (`if(...) x = y; else x = z;`). The
    // unbraced else body must not be dropped -- it once was. Structurally that means the consequence
    // assignment lands in block 1 and the else assignment in block 3. The CFG is the same diamond as
    // simple_else (which covers the braced form): the condition (0) branches only to the two arms
    // [1,3], never straight to the join, and both arms rejoin at the terminal block 2.
    let src = r"
        int f(int y, int z) {
            int x = 1;
            if(x == 1)
                x = y;
            else
                x = z;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "x", ["@p0"], Some(1)); // if-consequence
    check_assign_or_update(&prog, "x", ["@p1"], Some(3)); // unbraced else -- must not be dropped
    check_successors(&prog, 0, &[1, 3]);
    check_successors(&prog, 1, &[2]);
    check_successors(&prog, 3, &[2]);
    check_successors(&prog, 2, &[]);
}

#[test_log::test]
fn unbraced_if_else_branches_flow() {
    // The dataflow view of the unbraced if/else. With a trailing `return x`, either arm can supply
    // x, so both params reach the return. param 1 (the unbraced else's `x = z`) is the load-bearing
    // assertion: if that body were dropped, z would never reach the return.
    let src = r"
        int f(int y, int z) {
            int x = 1;
            if(x == 1)
                x = y;
            else
                x = z;
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, ""); // if-consequence (y) reaches the return
    check_returns_param(&s, 1, ""); // unbraced else (z) reaches the return
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

// ---- switch / case (+ break, continue) -------------------------------------
// CTADL is path-insensitive, like its `if` lowering: it does not evaluate the
// scrutinee, so every `case`/`default` arm is treated as reachable. These tests
// assert the resulting (sound, over-approximate) param->return summary flows.

#[test_log::test]
fn switch_case_flows_to_return() {
    // Taint in a `case` arm reaches the return. `b` (@p1) is assigned to `x` in
    // `case 1`; after the switch merges, `x` carries @p1 into the return.
    let src = r"
        int f(int a, int b) {
            int x = 0;
            switch (a) {
                case 1:
                    x = b;
                    break;
                default:
                    x = 0;
                    break;
            }
            return x;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn switch_default_flows_to_return() {
    // The `default` arm is just a valueless `case_statement`. `b` (@p1) assigned in
    // `default` flows to the return.
    let src = r"
        int f(int a, int b) {
            int x = 0;
            switch (a) {
                case 1:
                    x = 0;
                    break;
                default:
                    x = b;
                    break;
            }
            return x;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn switch_fallthrough_flows_to_return() {
    // Fall-through across a `case` boundary (no `break` after `case 1`): `x = b`
    // in `case 1` flows into `y = x` in `case 2`, then to the return.
    let src = r"
        int f(int a, int b) {
            int x = 0;
            int y = 0;
            switch (a) {
                case 1:
                    x = b;
                case 2:
                    y = x;
                    break;
                default:
                    break;
            }
            return y;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn break_exits_loop_flows_to_return() {
    // `break` inside a loop must be ingested (it had no handler before switch
    // support landed). The taint assigned before the `break` still reaches the
    // return.
    let src = r"
        int f(int a, int b) {
            int x = 0;
            while (a) {
                x = b;
                break;
            }
            return x;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn continue_in_loop_flows_to_return() {
    // `continue` is likewise ingested; the body's taint assignment still flows.
    let src = r"
        int f(int a, int b) {
            int x = 0;
            while (a) {
                x = b;
                continue;
            }
            return x;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn goto_backward_loop_flows_to_return() {
    // A backward `goto` forms a loop. `x = b` (@p1) executes in the labeled block on
    // every iteration and flows into the return. Exercises label definition + a
    // backward jump (the label is seen before the `goto`).
    let src = r"
        int f(int a, int b) {
            int x = 0;
        loop:
            x = b;
            if (a) goto loop;
            return x;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn goto_forward_jump_flows_to_return() {
    // A forward `goto` (label defined *after* the jump, so it relies on the pre-scan)
    // skips a kill on the only reachable path: `x = b`, jump over `x = 0`, then return
    // x. The skipped block is unreachable, so @p1 still reaches the return.
    let src = r"
        int f(int a, int b) {
            int x = b;
            goto done;
            x = 0;
        done:
            return x;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

// --- F1: taint through indirect (function-pointer) calls ---------------------
// `check_returns_param` matches across all functions; routing the value through
// param 1 means a `return <- @p1` summary can only come from `wrap` (the callee
// `id`'s own summary is `return <- @p0`). So these assert that `wrap` carries @p1
// through the (in)direct call to its return.

#[test_log::test]
fn taint_flows_through_direct_call() {
    // Control (passed before the F1 fix): a DIRECT call carries taint param->return.
    let src = r"
        int id(int p) { return p; }
        int wrap(int a, int b) {
            return id(b);
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn taint_flows_through_indirect_call() {
    // F1: the same flow through an INDIRECT call via a local function pointer
    // initialized to `id`. Previously dropped (soundness gap); now resolved because
    // the RHS `id` lowers to a function-pointer object, emitting `func_ptr_assign`.
    let src = r"
        int id(int p) { return p; }
        int wrap(int a, int b) {
            int (*fp)(int) = id;
            return fp(b);
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn taint_flows_through_indirect_call_separate_assign() {
    // F1, separate-assignment form: `int (*fp)(int); fp = id; fp(b)`.
    let src = r"
        int id(int p) { return p; }
        int wrap(int a, int b) {
            int (*fp)(int);
            fp = id;
            return fp(b);
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn taint_flows_through_indirect_call_forward_decl() {
    // The referenced function is defined AFTER its use as a function pointer. Relies on
    // the function-name pre-pass in collect_functions so `later` is already known when
    // `wrap`'s body is lowered.
    let src = r"
        int wrap(int a, int b) {
            int (*fp)(int) = later;
            return fp(b);
        }
        int later(int p) { return p; }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
#[ignore = "F1 partial: indirect call through a function-pointer PARAMETER still drops \
            taint (needs interprocedural func-ptr-value propagation); un-ignore when resolved"]
fn taint_flows_through_funcptr_param() {
    // F1, harder form: the function pointer is a PARAMETER. `apply`'s `return f(x)`
    // carries @p1 (x) to the return only if the indirect call through formal `f`
    // resolves (interprocedurally, since `f` is bound to `id` at the call site).
    // The frontend fix alone does NOT resolve this (the local-fp forms above do).
    let src = r"
        int id(int p) { return p; }
        int apply(int (*f)(int), int x) { return f(x); }
        int wrap(int a, int b) {
            return apply(id, b);
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
#[ignore = "F1 partial: indirect call through a function-pointer STRUCT FIELD still drops \
            taint (needs field-sensitive func-ptr-value propagation); un-ignore when resolved"]
fn taint_flows_through_funcptr_in_struct() {
    // F1, hardest form: the function pointer lives in a STRUCT FIELD. `o.op(b)`
    // resolves only if field-sensitivity carries `o.op = id` to the indirect call.
    // The frontend fix alone does NOT resolve this (the local-fp forms above do).
    let src = r"
        int id(int p) { return p; }
        struct S { int (*op)(int); };
        int wrap(int a, int b) {
            struct S o;
            o.op = id;
            return o.op(b);
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn aggregate_initializer_list_lowers_to_element_stores() {
    // An aggregate brace initializer (`int a[2] = { s, 0 }`) lowers to per-element stores
    // into synthetic index fields `a.[i]` -- the same `.[N]` field shape a constant-index
    // subscript read resolves to (see `subscript_access_paths`), so taint deposited in the
    // initializer is observed at a later `a[0]` read. Previously the `initializer_list`
    // reached `flatten_expr`'s catch-all and failed ingestion (ERR 78).
    let src = r"
        int f() {
            int s = source();
            int a[2] = { s, 0 };
            return a[0];
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "a.[0]", ["s"], None); // a[0] <- s
    check_assign_or_update(&prog, "a.[1]", ["#0"], None); // a[1] <- 0
}

#[test_log::test]
fn nested_aggregate_initializer_lowers_recursively() {
    // A nested aggregate (`int m[2][2] = {{s,0},{0,0}}`) recurses, extending the base path by
    // the outer index so the tainted element lands at `m[0][0]`. Access paths are offset-only,
    // so a two-symbol write (`m.[0].[0]`) decomposes through an intermediate load: the outer
    // `[0]` is loaded (`t = load m.[0]`) and the inner tainted element is stored into it
    // (`store t.[0] := s`). Both halves are asserted below.
    let src = r"
        int f() {
            int s = source();
            int m[2][2] = { { s, 0 }, { 0, 0 } };
            return m[0][0];
        }";
    let prog = program_from_string(src).0;
    let dump = format!("{prog}");
    check_loads(&prog, "m.[0]"); // the outer index is loaded to address the inner element
    assert!(
        dump.contains(".[0] := %s"),
        "nested tainted element should store `%s` into a `.[0]` field:\n{dump}"
    );
}

#[test_log::test]
fn labeled_empty_statement_parses() {
    // A label on an empty statement (`done: ;`), e.g. a `goto` target that jumps over a
    // kill, must ingest cleanly: the empty `expression_statement` (just `;`) carries no
    // expression to lower. Previously the bare `;` reached `flatten_expr`'s catch-all and
    // failed ingestion (ERR 78). `program_from_string` asserts a clean parse with no
    // dangling (terminator-less) block; we also confirm the pre-goto `r = s` flow survives.
    let src = r"
        int f() {
            int s = source();
            int r = s;
            goto done;
            r = 0;
        done:
            ;
            return r;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "r", ["s"], None); // r = s, before the goto
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
    // init + the two increments each write x (the +/- 1 temp sources are left unpinned).
    check_writes_to(&prog, "x", 3);
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
    // The field increment routes through an `update` of @p0.x (not a plain assign); the new value is
    // an unpinnable flatten temp, so we assert only that exactly one such update exists.
    check_writes_to(&prog, "@p0.x", 1);
}

// --- F2: taint through MULTIPLE function-pointer stores into one aggregate -----------
//
// A single function-pointer store into an aggregate already resolves at the call site.
// The F2 gap is that a *second* store into the same aggregate (struct or array) creates
// a new SSA version of the receiver, and the stored target must propagate across that
// version to reach the indirect call. The taint-index transitive rule that performs the
// hop (`func_ptr_assign_like` over `assign_like`) gates on `paths(p_new)`, but program
// paths were seeded only from call *arguments* (`actual_param`) -- never from an indirect
// call's *receiver* path. So the binding never reached the call and taint was dropped.
//
// Fix (ctadl-ascent/src/index_engine/mod.rs): register indirect/virtual-call receiver
// paths as program paths --
//     program_paths(p) <-- indirect_call(_, _, _, p);
//     program_paths(p) <-- java_call(_, _, _, p, _, _);
//
// Each test routes param 1 (`b`) through `id` and back to the return; a `return <- @p1`
// summary can only come from `wrap` (the callee `id`'s own summary is `return <- @p0`),
// so these assert that `wrap` carries @p1 through the indirect call. Remove the two fix
// lines above and the two `*_multistore_flows` tests fail (taint dropped) while
// `funcptr_single_store_flows` still passes -- that contrast IS the bug.

#[test_log::test]
fn funcptr_single_store_flows() {
    // Control: ONE function pointer stored into a struct field, then called through it.
    // Resolves with the single-store handling alone -- no SSA version hop is needed, so
    // this passes with or without the F2 fix. Establishes the baseline for the contrast.
    let src = r"
        int id(int p) { return p; }
        struct Ops { int (*f)(int); };
        int wrap(int a, int b) {
            struct Ops o;
            o.f = id;
            return o.f(b);
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn funcptr_struct_multistore_flows() {
    // F2 (struct form): TWO function pointers stored into the same struct, then a call
    // through the first. The second store (`o.g = id`) makes a new SSA version of `o`;
    // the `o.f -> id` binding must propagate across it to the call `o.f(b)`. This was
    // dropped before the F2 index-engine fix even though `funcptr_single_store_flows` works.
    let src = r"
        int id(int p) { return p; }
        struct Ops { int (*f)(int); int (*g)(int); };
        int wrap(int a, int b) {
            struct Ops o;
            o.f = id;
            o.g = id;
            return o.f(b);
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
#[ignore = "needs the array_declarator frontend fix (auto_test commit d1ccd07): `int (*fps[2])(int)` \
            fails ingestion here with `ERR 78: Unsupported expression type: array_declarator`. The F2 \
            resolution itself is covered by funcptr_struct_multistore_flows; un-ignore once array \
            declarators are supported on this branch."]
fn funcptr_array_multistore_flows() {
    // F2 (array form): TWO function pointers stored into the same array, then a call
    // through element 0. The `fps[1] = id` store makes a new SSA version of `fps`; the
    // `fps[0] -> id` binding must propagate across it to the call `fps[0](b)`. Same root
    // cause as the struct form -- this is what the broadened DFSan generator first surfaced.
    let src = r"
        int id(int p) { return p; }
        int wrap(int a, int b) {
            int (*fps[2])(int);
            fps[0] = id;
            fps[1] = id;
            return fps[0](b);
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

// ============================================================================
// Imported from the `treesitter_c_testing` coverage waves: cross-function flow
// (recursion, call-depth, globals), field/struct precision, expression-level
// dataflow, and `#[ignore]`d aspirational specs for not-yet-lowered constructs.
// ============================================================================

#[test_log::test]
fn field_non_interference() {
    // Field sensitivity, stated as a negative: writing `x` into `s.a` and returning `s.b` must NOT
    // leak `x` to the return -- `s.a` and `s.b` are distinct field paths. The positive halves (x ->
    // s.a, s.b returned) hold too; the load-bearing assertion is that x does not reach the return.
    let src = r"
        int f(Donkey s, int x) {
            s.a = x;
            return s.b;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 1, "", 0, "a"); // x -> s.a
    check_returns_param(&s, 0, "b"); // s.b -> return
    check_no_flow(&s, 1, "", 0, "b"); // x does NOT bleed into s.b
    check_does_not_return_param(&s, 1, ""); // ...so x never reaches the return
}

#[test_log::test]
fn arrow_field_returns_param() {
    // Reading a field through an arrow (`return p->x;`) on a pointer parameter summarizes as the
    // field path @p0.x reaching the return. (The `(*p).x` spelling that *should* be equivalent is
    // not yet lowered -- see the ignored `deref_paren_field_equivalent`.)
    let src = r"
        int f(Field *p) {
            return p->x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "x");
}

#[test_log::test]
fn deref_paren_field_equivalent() {
    // `(*p).x` is the same field access as `p->x` (see `arrow_field_returns_param`), yielding
    // @p0.x -> return. The frontend used to panic on the parenthesized-deref-then-field shape;
    // `extract_field_expression` now peels parens/derefs so it resolves like `p->x`.
    let src = r"
        int f(Field *p) {
            return (*p).x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "x");
}

#[test_log::test]
fn ternary_both_arms_flow() {
    // A ternary `a ? b : c` can yield either arm, so both `b` and `c` flow to the result (here the
    // return). The condition `a` is a control dependence, not a data source. `flatten_expr` lowers
    // `conditional_expression` by blending both arms into a temp.
    let src = r"
        int f(int a, int b, int c) {
            return a ? b : c;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 1, ""); // b (consequent arm)
    check_returns_param(&s, 2, ""); // c (alternative arm)
}

#[test_log::test]
fn cast_passthrough() {
    // A cast is value-preserving for taint: `(long)a` still carries `a` to the return.
    // `flatten_expr` lowers `cast_expression` by passing the operand straight through.
    let src = r"
        int f(int a) {
            return (long)a;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn sizeof_does_not_evaluate() {
    // `sizeof(*p)` does not evaluate its operand -- it yields a compile-time size, never reading
    // through `p` -- so the parameter must NOT reach the return. `flatten_expr` lowers
    // `sizeof_expression` as a constant (the operand is never visited), keeping this a true negative.
    let src = r"
        int f(int *p) {
            return sizeof(*p);
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_does_not_return_param(&s, 0, "");
}

#[test_log::test]
fn whole_struct_copy_carries_field() {
    // A whole-struct assignment (`t = s`) copies field taint: a later `t.a` read still resolves back
    // to the source struct's field. So s.a (@p0.a) reaches the return through the copy.
    let src = r"
        int f(Donkey s) {
            Donkey t;
            t = s;
            return t.a;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "a");
}

#[test_log::test]
fn nested_field_depth_returns() {
    // A deep field read (`v.a.b.c`) preserves the full access path: the summary endpoint is the
    // three-deep field path on the formal, not a flattened or truncated one.
    let src = r"
        int f(Thing v) {
            return v.a.b.c;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "a.b.c");
}

#[test_log::test]
fn field_store_then_load_roundtrips() {
    // Storing into a field and reading it back round-trips taint: `v.inner.val = x` then `y =
    // v.inner.val` carries x to y, and the return. Exercises field-store/field-load on the same path
    // across statements.
    let src = r"
        int f(Box v, int x) {
            v.inner.val = x;
            int y = v.inner.val;
            return y;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 1, "");
}

#[test_log::test]
fn out_param_write_propagates() {
    // The canonical C out-parameter taint shape: `*out = src` should propagate src (@p1) into the
    // object reached through out (@p0). This is the highest-value pointer pattern for a taint tool.
    let src = r"
        void f(int *out, int src) {
            *out = src;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 1, "", 0, "");
}

#[test_log::test]
fn address_of_local_aliases() {
    // Taking a local's address and writing through it (`int *p = &x; *p = src;`) taints x, so a
    // later `return x` carries src. Exercises address-of plus write-through-alias -- here `x` is a
    // by-ref-able parameter, complementing the local-variable cases in
    // `addr_of_local_write_through_taints_pointee`. Resolved (F3): the frontend records `p = &x` and
    // resolves the same-block `*p` to its pointee, so the store lands on x.
    let src = r"
        int f(int x, int src) {
            int *p = &x;
            *p = src;
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 1, "");
}

#[test_log::test]
fn for_loop_body_flows() {
    // A `for` loop body that assigns the parameter into a local (`for(...) { x = src; }`) carries the
    // parameter to a later `return x`. `for` is otherwise parked (only an ignored experimental case);
    // this pins that its body dataflow actually lowers and flows src (@p0) -> return.
    let src = r"
        int f(int src) {
            int x = 0;
            for (int i = 0; i < 10; i++) {
                x = src;
            }
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn recursion_returns_param() {
    // A self-recursive function (`if (src) return f(src); return src;`) must reach a summary fixpoint
    // where the parameter flows to the return: the base case returns src directly, and the recursive
    // call returns f(src), which by the same summary is src. Pins that the indexer's fixpoint handles
    // direct recursion (single function, so plain check_returns_param suffices).
    let src = r"
        int f(int src) {
            if (src) return f(src);
            return src;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn struct_by_value_through_call() {
    // Field taint survives a struct passed BY VALUE through a call. `callee` returns `p.a`, so its
    // summary is @p0.a -> return; `caller` passes its struct `s` as that argument and returns the
    // result, so caller's summary is @p0.a -> return too. Uses the per-function helpers so each flow
    // is pinned to the correct function (plain summary_search would conflate the two).
    let src = r"
        int callee(Donkey p) {
            return p.a;
        }
        int caller(Donkey s) {
            return callee(s);
        }";
    let (s, si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param_in(&s, &si, "callee", 0, "a");
    check_returns_param_in(&s, &si, "caller", 0, "a");
}

#[test_log::test]
fn unary_ops_blend_through() {
    // Unary operators are value-preserving for taint: negation, bitwise-not, and logical-not all carry
    // their operand to the result. (`!x` arguably should not -- it yields 0/1 -- but the frontend
    // treats it as a blend like the others; we pin the actual behavior.) Each is its own parse so a
    // single operator failing is isolated in the assertion message.
    for op in ["-", "~", "!"] {
        let src = format!("int f(int x) {{ return {op}x; }}");
        let (s, _si) = get_summary(program_from_string(&src).0).unwrap();
        check_returns_param(&s, 0, "");
    }
}

#[test_log::test]
fn constant_index_field_precision() {
    // Constant subscripts are distinct field paths: writing `src` into `v.a[0]` must NOT leak to a read
    // of `v.a[1]`. Subscripts lower to `FieldAccess::Symbol("[N]")`, so `[0]` and `[1]` are different
    // segments -- the array-index analogue of `field_non_interference`. The load-bearing assertion is
    // that src (p1) does not reach the return through the distinct index.
    //
    // NB the escaped brackets in the path strings: the summary stores a subscript as the literal
    // `Symbol("[0]")`, but `facts::Path`'s parser reads an unescaped `[0]` as a real `Offset(0)` (the
    // Symbol-vs-Offset divergence noted in this dir's CLAUDE.md). Escaping (`\[0\]`) forces the parser
    // to emit a Symbol, matching what the frontend actually lowered.
    let src = r"
        int f(Thing v, int src) {
            v.a[0] = src;
            return v.a[1];
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 1, "", 0, r"a.\[0\]"); // src -> v.a[0]
    check_returns_param(&s, 0, r"a.\[1\]"); // v.a[1] -> return
    check_does_not_return_param(&s, 1, ""); // src (into a[0]) does NOT reach the a[1] return
}

#[test_log::test]
fn mutual_recursion_returns_param() {
    // Mutual recursion across a summary fixpoint: `f` calls `g`, `g` calls `f`, and EACH has a base
    // case returning its parameter directly (`return x` / `return y`). The base cases seed the fixpoint
    // with param->return for both functions; the recursive-call edges then propagate those summaries
    // around the f<->g cycle without losing them. Pinned per-function so the two aren't conflated.
    //
    // The base cases are load-bearing. Without them -- `int f(int x){ return g(x); } int g(int y){
    // return f(y); }` -- the program is non-terminating recursion that never returns, and the only
    // sound summary is the EMPTY one: there is no param->return path that doesn't pass through another
    // non-terminating call, so the least fixpoint is empty. That empty result is *correct*, not a
    // dropped flow; a meaningful mutual-recursion test must supply a terminating base case.
    let src = r"
        int g(int y);
        int f(int x) { if (x > 0) return g(x); return x; }
        int g(int y) { if (y > 0) return f(y); return y; }";
    let (s, si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param_in(&s, &si, "f", 0, "");
    check_returns_param_in(&s, &si, "g", 0, "");
}

#[test_log::test]
fn struct_by_value_non_interference_through_call() {
    // Field sensitivity survives a by-value struct through a call: `callee` reads only `p.a`, so a
    // caller that writes `src` into the *other* field `s.b` and returns `callee(s)` must NOT leak src
    // to its return. The non-interference complement of `struct_by_value_through_call`. Uses the
    // per-function negative so the absence is asserted on the caller's summary specifically.
    let src = r"
        int callee(Donkey p) {
            return p.a;
        }
        int caller(Donkey s, int src) {
            s.b = src;
            return callee(s);
        }";
    let (s, si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param_in(&s, &si, "caller", 0, "a"); // s.a still reaches caller's return
    check_does_not_return_param_in(&s, &si, "caller", 1, ""); // src (into s.b) does not
}

#[test_log::test]
fn for_init_clause_flows() {
    // The `for` *init* clause is a real assignment slot: `for (x = src; ...; ...)` with an empty body
    // still flows src into x, so a later `return x` carries it. Complements `for_loop_body_flows`
    // (which exercises the body); this pins the init clause specifically.
    let src = r"
        int f(int src) {
            int x = 0;
            for (x = src; x < 10; x++) {}
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn post_increment_value_is_operand() {
    // `int x = y++;` consumes the *value* of `y++` (post-increment yields the old y, then increments),
    // so y flows to x and on to the return. We only ever tested `++` as a standalone statement before;
    // this pins its value-as-subexpression behavior. (The frontend loses the pre/post distinction, but
    // either way the operand value reaches x -- that is what we assert.)
    let src = r"
        int f(int y) {
            int x = y++;
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn nonconstant_subscript_may_alias_constant() {
    // A non-constant subscript is sound only if it may-alias every concrete index: writing `a[n]` and
    // reading `a[0]` should carry taint, because `n` could be 0. The frontend instead lowers `a[n]` to
    // `Symbol("[_elem_]")` and `a[0]` to `Symbol("[0]")` and treats them as disjoint paths, so src does
    // NOT reach the return -- unsound. (The doc anticipated an array-blind *collapse*; the reality is a
    // separate `[_elem_]` slot that never merges with constant indices.) Asserting the intended flow
    // documents the gap. Contrast `constant_index_field_precision`, where keeping two *constant*
    // indices distinct is the correct, precise answer.
    let src = r"
        int f(int *a, int src, int n) {
            a[n] = src;
            return a[0];
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 1, "");
}

#[test_log::test]
#[ignore = "limitation: union member overlap is modeled only for variables declared with an explicit \
            `union_specifier` (see `union_member_write_aliases_other_member`, live). This uses a bare \
            `U u;` whose type is a `type_identifier` (typedef/undeclared union), which the collapse \
            does not recognize, so `.a`/`.b` stay disjoint. Un-ignore once typedef-union tracking lands."]
fn union_write_overlaps_other_field() {
    // A union aliases its fields: writing `u.a` and reading `u.b` should carry taint (shared storage).
    // The overlap model (F4) collapses union members to one field -- but only for a variable declared
    // with an explicit `union { .. }` type. Here `U` is a bare type name (typedef/undeclared), so the
    // frontend can't tell `u` is a union and keeps `.a`/`.b` disjoint; the flow is still dropped. This
    // documents the remaining typedef-union gap; the supported form is covered live elsewhere.
    let src = r"
        int f(int src) {
            U u;
            u.a = src;
            return u.b;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn vararg_call_carries_argument() {
    // A tainted value passed as a variadic argument (`printf("%d", src)`) must at least be *captured*
    // as an argument of the lowered call -- the prerequisite for any varargs taint modeling. printf is
    // an unresolved external (no summary), so this is a Category-A call-shape assertion: f emits a
    // direct call to printf, and src (@p0) appears among its arguments.
    let src = r#"
        int f(int src) {
            return printf("%d", src);
        }"#;
    let (prog, _dump) = program_from_string(src);
    check_has_direct_call(&prog, "f", "printf");
    let src_exp = exp_from_str("@p0");
    let carries_src = direct_calls_in(&prog, "f").iter().any(|(callees, args)| {
        callees.iter().any(|c| c == "printf") && args.iter().any(|a| *a == src_exp)
    });
    assert!(
        carries_src,
        "expected @p0 to appear as an argument of the printf call\n{prog}"
    );
}

#[test_log::test]
fn designated_initializer_flows() {
    // A designated initializer `Thing v = {.a = src}` taints field `a`, so `return v.a` carries src.
    // The designator gives the member name, so the store lands at `v.a` -- exactly where the `v.a`
    // read resolves. (`src` is a scalar param, so it reaches the return at path "" -- the field path
    // lives on the local `v`, not on the param.)
    let src = r"
        int f(int src) {
            Thing v = {.a = src};
            return v.a;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn global_flows_across_functions() {
    // Taint through a global: `set` writes its parameter into global `g`, `get` returns `g`. Each half
    // is a summary endpoint on the special global heap -- `set`: @p0 -> $globals.g, `get`:
    // $globals.g -> return -- so the two compose into a cross-function flow `set`'s param ~> `get`'s
    // return. Pins that global writes/reads are summarized (not dropped) and tied to the right function.
    let src = r"
        int g;
        void set(int src) { g = src; }
        int get() { return g; }";
    let (s, si) = get_summary(program_from_string(src).0).unwrap();
    check_param_into_global_in(&s, &si, "set", 0, "g");
    check_returns_global_in(&s, &si, "get", "g");
}

#[test_log::test]
fn transitive_call_depth_returns_param() {
    // Summary composition through three call hops: `a` calls `b` calls `c`, each returning its param.
    // The param->return summary must propagate all the way out, so every function reports @p0 -> return
    // (we only ever tested a single call hop before). Pinned per-function.
    let src = r"
        int c(int z) { return z; }
        int b(int y) { return c(y); }
        int a(int x) { return b(x); }";
    let (s, si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param_in(&s, &si, "c", 0, "");
    check_returns_param_in(&s, &si, "b", 0, "");
    check_returns_param_in(&s, &si, "a", 0, "");
}

#[test_log::test]
fn comma_operator_yields_rhs() {
    // The comma operator `(a, b)` evaluates `a`, discards its value, and yields `b`. So `b` (p1) reaches
    // the return and the discarded `a` (p0) does NOT -- a precise, correct result (not an over-approx
    // blend). Distinct from `comma_list_declarations`, which is comma-separated *declarations*.
    let src = r"
        int f(int a, int b) {
            return (a, b);
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 1, ""); // b is the value of the comma expression
    check_does_not_return_param(&s, 0, ""); // a is evaluated then discarded
}

#[test_log::test]
fn short_circuit_both_operands_flow() {
    // A short-circuit `a && b` yields a 0/1 value derived from both operands (b only when a is truthy).
    // The frontend treats it as a blend, so both p0 and p1 flow to the return -- the sound over-approx
    // (it does not try to model that `b` may go unevaluated). Pins the blend behavior.
    let src = r"
        int f(int a, int b) {
            return a && b;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
    check_returns_param(&s, 1, "");
}

#[test_log::test]
fn const_local_flows() {
    // A `const`-qualified local is an ordinary local for dataflow: `const int x = src;` then `return x`
    // carries src to the return. (Robustness: the qualifier must not change lowering.)
    let src = r"
        int f(int src) {
            const int x = src;
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn static_local_flows() {
    // A `static` local persists across calls, but within a single call `x = src; return x;` still flows
    // src to the return. We pin the intra-call flow (cross-call persistence is not modeled and is out
    // of scope for a per-function summary). Robustness: the `static` qualifier must not drop the flow.
    let src = r"
        int f(int src) {
            static int x;
            x = src;
            return x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn addr_of_local_write_through_taints_pointee() {
    // F3 (soundness): writing through a local's address must write the *pointee*, not just
    // the pointer. CTADL models pointers as value copies (`int *p = &x` -> `assign p = x`,
    // `*p = src` -> `assign p = src`), which is sound for reads but drops the write-back, so
    // `x` never becomes tainted. Resolving `*p` to its same-block address-of pointee makes
    // the store `*p = src` lower to `assign x = src` -- a real def of `x` -- so a later
    // `sink(x)` observes the taint (DFSan corpus case 34_addr_of_local_alias).
    let src = r"
        int f() {
            int x = 0;
            int src = source();
            int *p = &x;
            *p = src;
            return x;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "x", ["src"], None); // *p = src  ==>  x = src
}

#[test_log::test]
fn addr_of_local_read_through_resolves_pointee() {
    // Reading through the alias (`int *p = &x; int y = *p;`) resolves `*p` to `x`, so `y`
    // reads the current `x`. This was already sound under the value-copy model; the fix keeps
    // it working while making the read path consistent with the write path (both route the
    // dereference to the pointee).
    let src = r"
        int f() {
            int x = source();
            int *p = &x;
            int y = *p;
            return y;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "y", ["x"], None); // y = *p  ==>  y = x
}

#[test_log::test]
fn addr_of_alias_does_not_cross_basic_blocks() {
    // The must-points-to is confined to the straight-line block the `p = &x` binding was
    // recorded in. Once control flow intervenes, a later `*p` falls back to the value-copy
    // model (writing `p`, not the pointee) rather than unsoundly resolving a possibly-stale
    // alias across a branch. Here the store lands after an `if`, so it writes `p`, and the
    // only write to `x` is its initializer.
    let src = r"
        int f(int c) {
            int x = 0;
            int src = source();
            int *p = &x;
            if (c) { int z = 1; }
            *p = src;
            return x;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "p", ["src"], None); // fallback: *p = src  ==>  p = src
    assert_eq!(
        count_writes_to(&prog, "x"),
        1,
        "only `int x = 0` should write x; the post-if `*p = src` must not resolve to x across a block boundary"
    );
}

#[test_log::test]
fn union_member_write_aliases_other_member() {
    // F4 (soundness): a union's members share storage, so writing `u.a` is observable at a
    // read of `u.b`. CTADL is field-sensitive (correct for structs) and treated `.a`/`.b` as
    // disjoint, dropping the flow. Union members are now collapsed to a single synthetic field
    // (`$union`), so `u.a` and `u.b` share an access path: the write to `.a` lands on
    // `u.$union` and the read of `.b` resolves there too, carrying the taint.
    let src = r"
        union U { int a; int b; };
        int f(int src) {
            union U u;
            u.a = src;
            return u.b;
        }";
    let prog = program_from_string(src).0;
    // The write to member `.a` collapses onto the shared union field...
    check_assign_or_update(&prog, "u.$union", ["@p0"], None); // u.a = src  ==>  u.$union := @p0
    // ...and no distinct `.a` field survives (both members share `$union`).
    assert_eq!(
        count_writes_to(&prog, "u.a"),
        0,
        "union member `.a` must collapse to `$union`, not remain a distinct field"
    );
    // End to end: param 0 reaches the return through the aliased union member.
    let (s, _si) = get_summary(prog).unwrap();
    check_summary_count(&s, 1);
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn struct_members_stay_disjoint() {
    // Control for the union fix: a *struct*'s members are genuinely disjoint, so writing
    // `s.a` must NOT taint `s.b`. Structs are not collapsed (only `union_specifier`-typed
    // vars are), so field sensitivity is preserved and there is no param->return flow.
    let src = r"
        struct S { int a; int b; };
        int f(int src) {
            struct S s;
            s.a = src;
            return s.b;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "s.a", ["@p0"], None); // stays a distinct `.a` field
    let (s, _si) = get_summary(prog).unwrap();
    check_summary_count(&s, 0); // s.a = src does not reach `return s.b`
}

#[test_log::test]
fn char_literal_ingests_as_constant() {
    // A character literal (`'a'`) is a compile-time constant, lowered like a numeric literal (no
    // taint). Previously it hit `flatten_expr`'s catch-all and failed ingestion (ERR 78), so *any*
    // program containing a char literal could not be analyzed. `program_from_string` asserts a
    // clean parse; we also confirm the adjacent real dataflow (`r = s`) still lowers.
    let src = r"
        int f(int s) {
            char c = 'a';
            int r = s;
            return r;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "r", ["@p0"], None); // r = s (param 0); the char const carries no taint
}

#[test_log::test]
fn char_literal_in_expression_flows() {
    // A char literal inside a larger expression (`s + 'a'`) and as a comparison guard (`x == 'z'`)
    // must ingest too; taint still flows from the parameter to the return through the surrounding
    // dataflow, while the constants `'a'`/`'z'` contribute none.
    let src = r"
        int f(int s) {
            int x = s + 'a';
            if (x == 'z') { x = s; }
            return x;
        }";
    let (sm, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&sm, 0, ""); // param 0 (s) reaches the return
}
