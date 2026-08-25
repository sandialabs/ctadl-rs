/*!
Unit tests for the tree-sitter C frontend.

# How a subscript is spelled

An `Offset` segment is pointer arithmetic and only that; every access that reads or writes
memory ends in the symbolic field `deref`. A subscript is both: `a[N]` is `*(a + N)`, so it
lowers to two segments -- a `PathSegment::Offset(N)` on the *address*, written `.[N]` in the
test DSL, and the `deref` performed there. So `a[3]` is `a.[3].deref`, and index 0 carries no
offset segment at all: `a[0]` is `*a`, and `Offset(0)` is the identity on addresses. This is
the pcode frontend's spelling, so both C frontends name a memory access the same way.

Splitting the index from the dereference is what makes element paths *compose*. Offsets are
summed where two paths meet, so a callee that writes at `@p0.[1].deref` and a caller that
passes `&x[1]` (the address `x.[1]`) agree on `x.[2].deref`. The frontend used to emit one
opaque `Symbol("[N]")` instead; nothing relates `Symbol("[1]")` to `Symbol("[2]")`, so
element addresses never composed and `&a[i]` had no address to form.

A non-constant index (`a[n]`) has no offset to name, so it lowers to a bare `deref` with no
offset at all -- the very path `a[0]` produces, which is how the two may-alias.
See `nonconstant_subscript_may_alias_constant`.

One consequence remains unfixed and is pinned elsewhere: `nightly/tests/c/ptrarith.c`
(XFAIL as `C:ptrarith` in the xtask suite) needs the binary `+` in `*(p + 2)` to lower
to an offset address too, which it does not yet.
*/

use ctadl_ir::ParameterType::{ByRef, ByVal};
use ctadl_ir::{Exp, StatementKind, Variable};

use crate::languages::tree_sitter_c::test_utils::*;

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
                Variable::Local(idx) => {
                    let name = fun.locals.name(*idx);
                    name.starts_with("<t").then(|| name.to_string())
                }
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
    check_returns_param(&summary, 0, ".f1");
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
    check_returns_param(&summary, 0, ".unbraced");
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
    check_flow(&s, 2, "", 0, ".f2");
    check_flow(&s, 1, ".f2.f3.f4", 0, ".f2.nf1.y");
    // blended RHS feeding a field (temp-free)
    check_flow(&s, 1, ".fa", 0, ".f5");
    check_flow(&s, 1, ".fb", 0, ".f5");
    check_flow(&s, 2, "", 0, ".f3");
    check_flow(&s, 3, "", 0, ".f3");
    // value-field source + field-write-then-return
    check_flow(&s, 1, ".xyz", 0, ".f1");
    check_returns_param(&s, 0, ".f1"); // formal field returned
    check_returns_param(&s, 1, ".xyz"); // resolved b.xyz reaches return
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
    // A constant array subscript, read and written (`x = f[3];` and `f[4] = x;`). A subscript is
    // `*(f + N)`, so it lowers to two segments: the index is a numeric `Offset(N)` on the address
    // -- written `.[3]` -- and the memory it names is the symbolic field `deref`. The read is a
    // load of `f.[3].deref` and the write a store at `f.[4].deref`. (`int x` is @p2.)
    let src = r"
        int brackets_simple(Donkey v, Burro* b, int x, int y) {
            int f = 1;
            x = f[3];
            f[4] = x;
        }";
    let prog = program_from_string(src).0;
    check_loads(&prog, "f.[3].deref"); // x = f[3]  (read lowers to a load of f.[3].deref)
    check_assign_or_update(&prog, "f.[4].deref", ["@p2"], None); // f[4] = x  (store)
}

#[test_log::test]
fn address_of_element_forms_an_address() {
    // `&x[1]` is the *address* of an element, so it lowers to the access path `x.[1]` -- a base
    // variable plus pointer arithmetic -- and is passed as such. It must NOT lower to a load of
    // `x.[1].deref`: that hands the callee a copy of the element's value, and any write the callee
    // makes through the pointer is lost (the pointer identity is gone before access paths are
    // involved at all). The load-bearing assertions are that the argument carries the offset and
    // that the element is never read.
    let src = r"
        void transfer(int *a, int b);
        void f(int s) {
            int x[3];
            transfer(&x[1], s);
        }";
    let prog = program_from_string(src).0;
    let args = call_args(&prog, "f", "transfer");
    let Exp::AccessPath(addr) = &args[0] else {
        panic!(
            "`&x[1]` should be an address access path, got {:?}\n{prog}",
            args[0]
        )
    };
    assert_eq!(addr.variable_ref, local_ref(&prog, "f", "x"));
    let offsets: Vec<i64> = addr.path.iter().map(|f| f.offset().0).collect();
    assert_eq!(offsets, vec![1], "`&x[1]` is `x` at offset 1\n{prog}");
    assert!(
        !statements_of(&prog).any(|s| matches!(s.kind, StatementKind::Load { .. })),
        "taking an element's address must not load the element\n{prog}"
    );
}

#[test_log::test]
fn address_of_element_zero_is_the_base_address() {
    // `&a[0]` is `a` itself: index 0 contributes no offset (`Offset(0)` is the identity on
    // addresses), so the address is the bare base -- the same bare variable the pass-through
    // gives for `&a`. Pinned because the offset-eliding branch of `push_element` is the one that
    // has to agree with what a later `a[0]` read resolves to.
    let src = r"
        void take(int *p);
        void f() {
            int a[3];
            take(&a[0]);
        }";
    let prog = program_from_string(src).0;
    let args = call_args(&prog, "f", "take");
    assert_eq!(
        args[0],
        Exp::Variable(local_ref(&prog, "f", "a")),
        "`&a[0]` is the base address, with no offset\n{prog}"
    );
}

#[test_log::test]
fn address_of_element_composes_with_callee_index() {
    // The point of forming the address: element offsets compose across a call. `transfer` writes
    // its parameter's element 1 (`a[1] = b`, a store at `.[1].deref`); the caller passes `&x[1]`
    // (the address `x.[1]`), so the write lands on `x.[2].deref` -- offsets are summed where the
    // paths meet -- which is exactly where a `x[2]` read resolves. This is the shape
    // `test_cli_query_c_sources_and_sinks` runs end to end.
    let flows = r"
        void transfer(int *a, int b) { a[1] = b; }
        int f(int s) {
            int x[3];
            transfer(&x[1], s);
            return x[2];
        }";
    let (summary, si) = get_summary(program_from_string(flows).0).unwrap();
    check_returns_param_in(&summary, &si, "f", 0, "");

    // ...and the arithmetic is real arithmetic, not an array-blind collapse: the same call does
    // not taint the element it started from. `x[1]` is where the *address* points, but the callee
    // writes one element past it.
    let precise = r"
        void transfer(int *a, int b) { a[1] = b; }
        int f(int s) {
            int x[3];
            transfer(&x[1], s);
            return x[1];
        }";
    let (summary, si) = get_summary(program_from_string(precise).0).unwrap();
    check_does_not_return_param_in(&summary, &si, "f", 0, "");
}

#[test_log::test]
fn store_through_element_address_alias_flows() {
    // An element address bound to a pointer and written through it: `p = &x[1]` records the
    // pointee `x.[1]` in the must-points-to map, so `*p = s` is a store at `x.[1].deref` -- the
    // `deref` field terminates an address that is otherwise offsets-only -- and a later `x[1]`
    // read observes it. Before `&x[1]` formed an address, `p` held a *copy* of the element and
    // the write was dropped.
    let src = r"
        int f(int s) {
            int x[3];
            int *p = &x[1];
            *p = s;
            return x[1];
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn address_of_struct_member_keeps_value_model() {
    // `&s.f` has no address in this IR: naming it would need `f`'s byte offset, which the
    // frontend cannot compute without type information (it names members symbolically instead).
    // So address-of falls back to the historical value model and passes the member's *value*.
    // Pinned as the documented limitation next to `address_of_element_forms_an_address`: the
    // argument is a plain (pathless) value, and a callee's write through that pointer is lost.
    let src = r"
        void take(int *p);
        void f(Thing s) {
            take(&s.f);
        }";
    let prog = program_from_string(src).0;
    let args = call_args(&prog, "f", "take");
    assert!(
        matches!(&args[0], Exp::Variable(_)),
        "`&s.f` has no address spelling, so it must stay a loaded value, got {:?}\n{prog}",
        args[0]
    );
    // ...and that value comes from a load of the member. (`check_loads` wants a single-function
    // program; a call site always has at least two, so scan `f`'s statements directly.)
    let f = function_named(&prog, "f").expect("f is defined");
    assert!(
        f.blocks
            .iter()
            .flat_map(|b| b.statements.iter())
            .any(|s| matches!(&s.kind, StatementKind::Load { field, .. } if field.as_str() == "f")),
        "expected a load of the member `f`\n{prog}"
    );
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
fn array_of_struct_field_is_index_and_field_sensitive() {
    // A field access whose object is itself an array element (`a[i].f`) used to fail ingestion
    // ("ERR 78: Unsupported object in field access: subscript_expression"). It now composes the
    // subscript's `.[i]` segment with the field, so `a[1].y` is the access path `a.[1].y` on both
    // sides of an assignment -- a single slot named by an index *and* a field offset.
    //
    // The three fixtures pin that the composed path is precise in both dimensions: taint written
    // to `a[1].y` is observed at a read of `a[1].y`, but not at a read that shares only the field
    // (`a[0].y`) or only the element (`a[1].x`). This is the frontend-unit complement to the
    // `arrayofstruct` regression case, whose `unexpected_lines` make the same precision claim.
    let same = r"
        struct pt { int x; int y; };
        int f(int b) {
            struct pt a[3];
            a[1].y = b;
            return a[1].y;
        }";
    let (s, _si) = get_summary(program_from_string(same).0).unwrap();
    check_returns_param(&s, 0, ""); // same element, same field -> flows

    let other_field = r"
        struct pt { int x; int y; };
        int f(int b) {
            struct pt a[3];
            a[1].y = b;
            return a[1].x;
        }";
    let (s, _si) = get_summary(program_from_string(other_field).0).unwrap();
    check_does_not_return_param(&s, 0, ""); // same element, other field -> no flow

    let other_index = r"
        struct pt { int x; int y; };
        int f(int b) {
            struct pt a[3];
            a[1].y = b;
            return a[0].y;
        }";
    let (s, _si) = get_summary(program_from_string(other_index).0).unwrap();
    check_does_not_return_param(&s, 0, ""); // other element, same field -> no flow
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
    check_flow(&s, 0, ".f5", 0, ".f4"); // direct
    check_flow(&s, 0, ".f1", 0, ".f4"); // via b
    check_flow(&s, 0, ".f3", 0, ".f4"); // via b
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
    // An aggregate brace initializer (`int a[2] = { s, 0 }`) lowers to per-element stores at
    // successive element addresses -- the same offset + `deref` shape a constant-index
    // subscript resolves to (see `subscript_access_paths`), so taint deposited in the
    // initializer is observed at a later `a[0]` read. Element 0 carries no offset segment
    // (`a[0]` is `*a`), element 1 carries `.[1]`. Previously the `initializer_list` reached
    // `flatten_expr`'s catch-all and failed ingestion (ERR 78).
    let src = r"
        int f() {
            int s = source();
            int a[2] = { s, 0 };
            return a[0];
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "a.deref", ["s"], None); // a[0] <- s
    check_assign_or_update(&prog, "a.[1].deref", ["#0"], None); // a[1] <- 0
}

#[test_log::test]
fn nested_aggregate_initializer_lowers_recursively() {
    // A nested aggregate (`int m[2][2] = {{s,0},{0,0}}`) recurses, extending the base path by
    // the outer index so the tainted element lands at `m[0][0]`. A load/store field is a single
    // symbol, so a two-element write (`m.deref.deref`) decomposes through an intermediate load:
    // the outer element is loaded (`t = load m.deref`) and the inner tainted element is stored
    // into it (`store t.deref := s`). Both halves are asserted below; index 0 contributes no
    // offset segment.
    let src = r"
        int f() {
            int s = source();
            int m[2][2] = { { s, 0 }, { 0, 0 } };
            return m[0][0];
        }";
    let prog = program_from_string(src).0;
    let dump = format!("{prog}");
    check_loads(&prog, "m.deref"); // the outer element is loaded to address the inner one
    // The dump renders locals as `%L{idx}`, so resolve `s` to its interned rendering.
    let s = local_render(&prog, "f", "s");
    assert!(
        dump.contains(&format!(".deref := {s}")),
        "nested tainted element should store `{s}` (= `s`) into a `.deref` field:\n{dump}"
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
fn unsupported_expression_warns_and_recovers() {
    // An AST shape the frontend does not lower (here `offsetof`, which reaches
    // `flatten_expr`'s catch-all, ERR 78) is a warning by default, not an ingestion
    // error: the expression becomes an opaque temp via `unexpected_ast` and the rest
    // of the function still lowers, so `f`'s param->return flow survives. Setting
    // CTADL_ERROR_ON_AST restores the hard error; that side isn't exercised here
    // because the env var is process-global and tests run in parallel -- instead the
    // test skips when the var is set, so a strict-mode environment doesn't fail it.
    //
    // `offsetof` is only a stand-in for "some expression kind with no arm"; if a later
    // spec lowers it, swap in another unhandled kind rather than deleting the test.
    // (It used to be `asm("nop")` and then `_Generic`, both of which now lower -- see
    // `flatten_gnu_asm` and `flatten_expr`'s `generic_expression` arm.)
    if std::env::var_os("CTADL_ERROR_ON_AST").is_some() {
        return;
    }
    let src = r#"
        struct S { int m; };
        int f(int a) {
            offsetof(struct S, m);
            return a;
        }"#;
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn stray_break_continue_goto_warn_and_recover() {
    // `break`/`continue` outside any loop/switch and `goto` to an undefined label are
    // problems in the analyzed source ("source problem" warnings), not frontend gaps.
    // Each recovers as a no-op -- none terminates the block -- so the statements after
    // them still lower and `f`'s param->return flow survives. Skips under
    // CTADL_ERROR_ON_AST, which restores the hard error for all three.
    if std::env::var_os("CTADL_ERROR_ON_AST").is_some() {
        return;
    }
    let src = r"
        int f(int a) {
            break;
            continue;
            goto nowhere;
            return a;
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn error_on_ast_promotes_frontend_gap() {
    // The strict side of the switch: under CTADL_ERROR_ON_AST an unsupported
    // expression is a hard ingestion error again, exactly as before the warning
    // demotion. Strictness comes from the per-thread test override, not the env var,
    // which is process-global and would race the parallel test harness. Same stand-in
    // caveat as `unsupported_expression_warns_and_recovers`.
    let _strict = super::force_error_on_ast();
    let src = r#"
        struct S { int m; };
        int f(int a) {
            offsetof(struct S, m);
            return a;
        }"#;
    let err = super::parse_c_program(src).expect_err("strict mode must reject the frontend gap");
    assert!(
        err.to_string().contains("Unsupported expression type"),
        "unexpected error: {err}"
    );
}

#[test_log::test]
fn error_on_ast_promotes_source_problem() {
    // Same strict switch for the source-problem flavor: a stray `break` fails
    // ingestion under CTADL_ERROR_ON_AST.
    let _strict = super::force_error_on_ast();
    let src = r"
        int f(int a) {
            break;
            return a;
        }";
    let err = super::parse_c_program(src).expect_err("strict mode must reject the stray break");
    assert!(
        err.to_string().contains("`break` outside"),
        "unexpected error: {err}"
    );
}

#[test_log::test]
fn bare_block_then_statement_recovers() {
    // Scope-semantics invariant, NOT a regression pin for the bare-block wiring bug: a
    // bare compound block `{ ... }` written as a statement is pure scope, so the whole
    // body is one straight line -- the braces must neither terminate the enclosing basic
    // block nor start a new one, and nothing written after them may be dropped. Runs
    // under `force_error_on_ast`, so any recoverable report fails the test.
    //
    // What review of the spec-033 rewrite established empirically: this shape, with
    // *plain-statement* followers, never triggered the historic wiring bug -- this exact
    // test also passes on the pre-fix front end. The old comment's "followed by any
    // further statement" was wrong: the bug fired only when the follower was a
    // compound-BEARING statement (an `if`, as in dropbear), because only those asked for
    // the end-of-compound link that installed the bogus implicit `return`. The
    // behavioral pin that fails pre-fix and passes post-fix is
    // `svr_dropbear_exit_shape_recovers` below. (The `_recovers` in this test's name is
    // historical; nothing is recovered from any more.)
    let _strict = super::force_error_on_ast();
    let src = r"
        void h(void); void k(void); void m(void);
        void f(void) {
            { h(); }
            k();
            { m(); }
        }";
    let prog = super::parse_c_program(src)
        .expect("a bare block followed by statements must not gap in strict mode")
        .0;
    check_block_count(&prog, 1);
    check_successors(&prog, 0, &[]);
    // Nothing was dropped on the floor: every call is still in the (single, reachable)
    // block, in particular the ones written after the bare block.
    check_has_direct_call(&prog, "f", "h");
    check_has_direct_call(&prog, "f", "k");
    check_has_direct_call(&prog, "f", "m");
}

#[test_log::test]
fn svr_dropbear_exit_shape_recovers() {
    // THE regression pin for the bare-block wiring bug (spec 033): on the pre-fix front
    // end this fails in strict mode with `continuation edge into a block that already
    // returns, dropped: BasicBlockIdx(2) -> BasicBlockIdx(7)`; post-fix it lowers with
    // no gap at all, which is what `force_error_on_ast` asserts here.
    //
    // The reduced shape of dropbear's `svr_dropbear_exit`, the function this bug was
    // found on: an if / else-if / else chain, then a bare `{ }` block (the remnant of a
    // compiled-out `#if DROPBEAR_VFORK` that had guarded a lone `{ session_cleanup(); }`),
    // then a trailing `if` (`if (svr_opts.hostkey)`). The bare-block-then-IF pair is the
    // trigger -- not, despite first appearances, the returning arms of the chain, and not
    // bare-block-then-plain-statement either (see `bare_block_then_statement_recovers`):
    // only a compound-bearing follower asked for the end-of-compound link that installed
    // the bogus implicit `return`. With the compound arm no longer asking for that link,
    // the shape lowers cleanly.
    let _strict = super::force_error_on_ast();
    let src = r"
        void svr_dropbear_exit(int exitcode) {
            int add_delay = 0;
            if (early) { log(1); }
            else if (authed) { log(2); }
            else { log(3); add_delay = 1; }
            { session_cleanup(); }
            if (hostkey) { free_key(hostkey); }
        }";
    let prog = super::parse_c_program(src)
        .expect("the svr_dropbear_exit shape must not gap in strict mode")
        .0;
    assert!(
        function_named(&prog, "svr_dropbear_exit").is_some(),
        "program should still define svr_dropbear_exit\n{prog}"
    );
    // The load-bearing half: the statements on either side of the bare block are still
    // reachable. Before the fix `session_cleanup()`'s block carried an implicit `return`
    // and the trailing `if` was an unreachable island, so `free_key` was dead code.
    check_has_direct_call(&prog, "svr_dropbear_exit", "session_cleanup");
    check_has_direct_call(&prog, "svr_dropbear_exit", "free_key");
    get_summary(prog).expect("CFG must verify and index");
}

#[test_log::test]
fn error_on_ast_promotes_bare_block_edge() {
    // Historical name, inverted assertion: this used to pin that CTADL_ERROR_ON_AST
    // *promoted* the recovered `continuation edge into a block that already returns` back
    // to a hard ingestion error. There is no longer an edge to drop -- the bare block does
    // not close the block it shares -- so the strict mode this test guards is now the
    // strongest available statement that the gap is gone: strict ingestion of the very
    // source that used to fail must succeed. Kept under its original name so the fix's
    // before/after is traceable to the report it came from.
    let _strict = super::force_error_on_ast();
    let src = r"
        void f(void) {
            { h(); }
            if (c) { k(); }
        }";
    // `program_from_string` panics if ingestion errors, which under `force_error_on_ast`
    // is exactly "some frontend gap was reported".
    let prog = program_from_string(src).0;
    check_successors(&prog, 0, &[1, 2]);
}

#[test_log::test]
fn bare_block_then_if_keeps_branch_edge() {
    // The wiring fix, stated as a CFG assertion: a bare `{ ... }` shares the enclosing
    // basic block, so when the walk leaves it the enclosing block must still be open and
    // must branch into the following `if` -- not carry an implicit `return` that orphans
    // everything after the braces.
    let src = r"
        void h(void); void k(int); int c;
        void f(int a) {
            { h(); }
            if (c) { k(a); }
        }";
    let prog = program_from_string(src).0;
    check_successors(&prog, 0, &[1, 2]);
    // ...and the then-arm is genuinely on the graph, not a dead island.
    check_has_direct_call(&prog, "f", "k");
}

#[test_log::test]
fn bare_block_then_if_preserves_flow() {
    // The dataflow half: the CFG damage cost real taint. `r = a` sits behind the `if`
    // that followed the bare block, so before the fix the assignment lived in an
    // unreachable block and @p0 never reached the return.
    let src = r"
        int f(int a) {
            int r = 0;
            { r = 0; }
            if (a) { r = a; }
            return r;
        }";
    let prog = program_from_string(src).0;
    let (s, _si) = get_summary(prog).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn bare_block_then_while_keeps_branch_edge() {
    // Same shape with a `while` instead of an `if` -- the other block-creating statement
    // that triggered the dropped continuation edge in the corpora.
    let src = r"
        void h(void); void k(int); int c;
        void f(int a) {
            { h(); }
            while (c) { k(a); }
        }";
    let prog = program_from_string(src).0;
    // The entry block falls through into the loop condition (block 1) rather than
    // returning; the condition then branches to the continuation (2) and the body (3).
    check_successors(&prog, 0, &[1]);
    check_successors(&prog, 1, &[2, 3]);
    check_has_direct_call(&prog, "f", "k");
}

#[test_log::test]
fn bare_block_shadow_does_not_leak() {
    // The bare-block arm threads the *block* the inner walk ended in back to the caller
    // but deliberately restores the caller's *scope*: a bare `{ ... }` is a lexical scope,
    // so a name declared inside it must not be visible after the closing brace. `r` is
    // shadowed inside the braces and assigned `a` there; the `return r` afterwards must
    // resolve to the outer `r` (= `b`), so `b` reaches the return and `a` does not.
    // (`block_shadow_does_not_leak` covers the same rule for an `if` body, which takes a
    // different path -- `walk_if`, not the compound arm.)
    let src = r"
        int f(int a, int b) {
            int r = b;
            { int r = a; }
            return r;
        }";
    let prog = program_from_string(src).0;
    let (s, _si) = get_summary(prog).unwrap();
    check_returns_param(&s, 1, ""); // outer r = b reaches the return
    check_does_not_return_param(&s, 0, ""); // the block-scoped shadow (a) must not leak
}

#[test_log::test]
fn bare_block_with_return_diverges() {
    // The secondary defect: the bare block's *divergence* has to reach the enclosing
    // compound too. `{ return; }` terminates the shared block, so `h()` after it is
    // unreachable and must lower into its own dead block -- not get appended after the
    // `return` in the block that already terminated. Strict mode pins that neither the
    // dropped-edge gap nor an unterminated-block gap is reported.
    let _strict = super::force_error_on_ast();
    let src = r"
        void h(void);
        void f(void) {
            { return; }
            h();
        }";
    let prog = super::parse_c_program(src)
        .expect("a diverging bare block must not gap in strict mode")
        .0;
    check_successors(&prog, 0, &[]);
    // Two blocks, not one: `h()` must land in its own unreachable block. Without the
    // divergence signal the enclosing compound kept filling the block the `return`
    // already terminated, so `h()` sat *after* the terminator in the entry block.
    check_block_count(&prog, 2);
    check_successors(&prog, 1, &[]);
    check_has_direct_call(&prog, "f", "h");
    get_summary(prog).expect("CFG must verify and index");
}

#[test_log::test]
fn goto_label_after_return_lowers() {
    // The goto-cleanup idiom: a label sitting *after* a diverging statement, reached
    // only through its `goto` edge. `walk_compound_statement` stops falling through at
    // the `return`, but the trailing siblings still have to be walked -- otherwise the
    // pre-created `out:` block is never visited, `finalize_terminators` patches it with
    // an implicit empty `return`, and `cleanup()` is silently dropped from the IR. The
    // walk continues in a fresh unreachable block (as it already did after a `goto`),
    // so the label lowers normally. No CTADL_ERROR_ON_AST guard: this shape no longer
    // reports a gap in either mode.
    let src = r"
        void f(void) {
            if (c) goto out;
            return;
        out:
            cleanup();
        }";
    // program_from_string asserts no `<no terminator>` block survives.
    let prog = program_from_string(src).0;
    assert!(
        function_named(&prog, "f").is_some(),
        "program should define f\n{prog}"
    );
    // The load-bearing assertion: the label body is actually in the IR. Under the old
    // drop-and-patch behavior `out:`'s block was empty and this call was absent.
    check_has_direct_call(&prog, "f", "cleanup");
    // End-to-end through verify() + SSA + codegen: the CFG satisfies the basic-block
    // contract with no tolerance on the ctadl-ir side.
    get_summary(prog).expect("CFG must verify and index");
}

#[test_log::test]
fn label_after_return_dataflow() {
    // The dataflow half of `goto_label_after_return_lowers`: lowering the label body is
    // only worth anything if taint actually flows through it. `out:` writes parameter
    // `a` into global `g`, so the summary must carry @p0 -> $globals.g. Runs under
    // `force_error_on_ast`, which turns any residual recoverable report into a hard
    // error -- so this also pins that the goto-cleanup shape reports NO frontend gap in
    // strict mode.
    let _strict = super::force_error_on_ast();
    let src = r"
        int g;
        void f(int a) {
            if (c) goto out;
            return;
        out:
            g = a;
        }";
    let prog = super::parse_c_program(src)
        .expect("goto-cleanup must not gap in strict mode")
        .0;
    let (s, si) = get_summary(prog).unwrap();
    check_param_into_global_in(&s, &si, "f", 0, ".g");
}

#[test_log::test]
fn statements_after_return_still_import() {
    // The degenerate case of the same continuation: plain unreachable statements after a
    // `return`, with no label to jump back in. They lower into a dead block that nothing
    // branches to, which must still be terminated (`verify()` rejects a terminator-less
    // block regardless of reachability) and must not disturb the reachable part of the
    // function. Strict mode, so an unterminated leftover would be a hard error rather
    // than a warning.
    //
    // `f` is `void` on purpose. The implicit return that closes an unreachable trailing
    // block is the empty one (`return;`), and in a *non-void* function that trips
    // `verify()`'s `InconsistentReturns` (arity 1 vs 0) -- but that is the pre-existing
    // fall-off-the-end-of-a-non-void-function gap, reproducible on plain
    // `int f(int a) { int b = a; }` with or without this change, and out of scope here.
    let _strict = super::force_error_on_ast();
    let src = r"
        int g;
        void f(int a) {
            g = a;
            return;
            cleanup();
        }";
    let prog = super::parse_c_program(src)
        .expect("unreachable trailing code must not gap")
        .0;
    // The reachable write survives untouched, and the dead `cleanup()` block is
    // terminated well enough for verify()/SSA/codegen to run end to end.
    let (s, si) = get_summary(prog).unwrap();
    check_param_into_global_in(&s, &si, "f", 0, ".g");
}

#[test_log::test]
fn error_on_ast_promotes_unterminated_block() {
    // Strict side of the `finalize_terminators` sweep: under CTADL_ERROR_ON_AST a block
    // the walk never terminated (its statements were dropped) is a hard ingestion error.
    // The goto-after-return shape no longer reaches the sweep now that trailing siblings
    // are walked, so this points at the shape that still orphans a block: a duplicate
    // label, whose first pre-created block `label_blocks` drops on the floor (see
    // `duplicate_label_orphan_block_terminated` for the non-strict side).
    let _strict = super::force_error_on_ast();
    let src = r"
        void f(void) {
            goto l;
        l:  a();
        l:  b();
        }";
    let err =
        super::parse_c_program(src).expect_err("strict mode must reject the orphaned label block");
    assert!(
        err.to_string().contains("without a terminator"),
        "unexpected error: {err}"
    );
}

#[test_log::test]
fn duplicate_label_orphan_block_terminated() {
    // Duplicate label names: `collect_labels` pre-creates two blocks, `label_blocks`
    // keeps only the second, and the first is orphaned -- unreachable AND
    // unterminated. `is_connected` never visits it, but `verify()` rejects any
    // block without a terminator regardless of reachability, so the sweep must
    // patch orphans too.
    if std::env::var_os("CTADL_ERROR_ON_AST").is_some() {
        return;
    }
    let src = r"
        void f(void) {
            goto l;
        l:  a();
        l:  b();
        }";
    get_summary(program_from_string(src).0).expect("orphaned label block must get a terminator");
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
// hop (`call_target_assign_like` over `assign_like`) gates on `paths(p_new)`, but program
// paths were seeded only from call *arguments* (`actual_param`) -- never from an indirect
// call's *receiver* path. So the binding never reached the call and taint was dropped.
//
// Fix (ctadl-ascent/src/index_engine/mod.rs): register indirect/virtual-call receiver
// paths as program paths -- a single context-agnostic rule over the unified call-site
// relation (formerly two rules over `indirect_call` and `java_call`) --
//     program_paths(p) <-- callee_info(_, _, _, p, _);
//
// Each test routes param 1 (`b`) through `id` and back to the return; a `return <- @p1`
// summary can only come from `wrap` (the callee `id`'s own summary is `return <- @p0`),
// so these assert that `wrap` carries @p1 through the indirect call. Remove the fix
// line above and the two `*_multistore_flows` tests fail (taint dropped) while
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
    check_flow(&s, 1, "", 0, ".a"); // x -> s.a
    check_returns_param(&s, 0, ".b"); // s.b -> return
    check_no_flow(&s, 1, "", 0, ".b"); // x does NOT bleed into s.b
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
    check_returns_param(&s, 0, ".x");
}

#[test_log::test]
fn deref_paren_field_equivalent() {
    // `(*p).x` is the same field access as `p->x` (see `arrow_field_returns_param`), yielding
    // @p0.x -> return. The frontend used to panic on the parenthesized-deref-then-field shape;
    // `flatten_lvalue` now resolves the field's object (`(*p)`) recursively -- peeling the paren
    // and deref -- so it resolves like `p->x`.
    let src = r"
        int f(Field *p) {
            return (*p).x;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, ".x");
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
fn elvis_ternary_both_arms_flow() {
    // GNU's `a ?: c` omits the consequence: the value is `a` itself when `a` is truthy,
    // otherwise `c`. tree-sitter parses this as a `conditional_expression` with NO
    // `consequence` field -- the shape that used to panic `flatten_expr` with
    // "conditional_expression always has a consequence" (the Linux kernel uses `?:`
    // heavily, e.g. `sk->sk_bound_dev_if ?: dev->ifindex`). Both the condition and the
    // alternative must reach the result.
    let src = r"
        int f(int a, int c) {
            return a ?: c;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, ""); // a is the value when truthy -- a data source here
    check_returns_param(&s, 1, ""); // c (alternative arm)
}

#[test_log::test]
fn elvis_ternary_is_not_a_frontend_gap() {
    // Strict-mode pin: `a ?: c` must lower cleanly, not merely recover. Under
    // CTADL_ERROR_ON_AST any `unexpected_ast` report becomes a hard error, so this
    // failing would mean the elvis shape had regressed to the catch-all.
    let _strict = super::force_error_on_ast();
    let src = r"
        int f(int a, int c) {
            return a ?: c;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
    check_returns_param(&s, 1, "");
}

#[test_log::test]
fn elvis_ternary_propagates_struct_field() {
    // The kernel's `container ?: fallback` idiom usually blends a field read with a
    // fallback. Reusing the condition's already-lowered value (rather than re-lowering
    // it) keeps the field path intact, so `p->x` still reaches the result.
    let src = r"
        typedef struct Field { int x; } Field;
        int f(Field *p, int c) {
            return p->x ?: c;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, ".x");
    check_returns_param(&s, 1, "");
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
    check_returns_param(&s, 0, ".a");
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
    check_returns_param(&s, 0, ".a.b.c");
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
    check_returns_param_in(&s, &si, "callee", 0, ".a");
    check_returns_param_in(&s, &si, "caller", 0, ".a");
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
    // of `v.a[1]`. A subscript lowers to a numeric `Offset(N)` on the address plus the `deref` it
    // performs there, so `[0]` and `[1]` differ in that offset -- the array-index analogue of
    // `field_non_interference`. The load-bearing assertion is that src (p1) does not reach the return
    // through the distinct index.
    //
    // NB `.[1]` in the path strings is an *offset* (pointer arithmetic); `.deref` is the symbolic
    // field naming the memory at that address. Index 0 contributes no offset segment at all --
    // `a[0]` is `*a`, and `Offset(0)` is the identity on addresses.
    let src = r"
        int f(Thing v, int src) {
            v.a[0] = src;
            return v.a[1];
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 1, "", 0, ".a.deref"); // src -> v.a[0]
    check_returns_param(&s, 0, ".a.[1].deref"); // v.a[1] -> return
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
    check_returns_param_in(&s, &si, "caller", 0, ".a"); // s.a still reaches caller's return
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
fn for_update_prefix_increment_lowers() {
    // A prefix `++i` in the for-update clause used to reach `walk_statement`'s
    // `update_expression` arm, which lowered `child.child(0)` -- for a *prefix* update that
    // child is the `++` operator token, so it fell into `flatten_expr`'s catch-all and logged
    // `frontend gap: ERR 78: Unsupported expression type: ++`. Routing the whole
    // `update_expression` node through `flatten_expr` reaches `flatten_update_expression`,
    // which reads the `argument` field and so handles prefix and postfix alike.
    // Structural contract: `i` is written twice -- once by the init clause, once by `++i`.
    let src = r"
        int f(int n) {
            int x = 0;
            for (int i = 0; i < n; ++i) {
                x = n;
            }
            return x;
        }";
    let prog = program_from_string(src).0;
    check_writes_to(&prog, "i", 2);
}

#[test_log::test]
fn for_update_postfix_increment_lowers() {
    // The postfix twin of `for_update_prefix_increment_lowers`. Postfix never warned -- it was
    // worse than that: `child.child(0)` of `i++` is the bare identifier `i`, so lowering it
    // produced a read and the increment was *silently dropped*. Counting writes to `i`
    // (init + increment = 2) is what catches that; a dump-string or warning check would not.
    let src = r"
        int f(int n) {
            int x = 0;
            for (int i = 0; i < n; i++) {
                x = n;
            }
            return x;
        }";
    let prog = program_from_string(src).0;
    check_writes_to(&prog, "i", 2);
}

#[test_log::test]
fn for_update_prefix_decrement_lowers() {
    // `--i` is the same gap as `++i` (the operator token differs, the AST shape does not), and it
    // is the form the openssh corpus hits alongside `++`. Under `force_error_on_ast` any frontend
    // gap becomes a hard error, so `program_from_string` succeeding at all is the assertion that
    // the construct no longer reports one; the write count then pins the increment itself.
    let _strict = super::force_error_on_ast();
    let src = r"
        int f(int n) {
            int x = 0;
            for (int i = n; i > 0; --i) {
                x = n;
            }
            return x;
        }";
    let prog = program_from_string(src).0;
    check_writes_to(&prog, "i", 2);
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
    // reading `a[0]` carries taint, because `n` could be 0. The frontend gets that by lowering both to
    // the same path -- the bare dereference `a.deref`, since neither has an offset to name -- rather
    // than by asking the path matcher to relate two spellings. Only index 0 is covered: `a[n] = src`
    // is still not observed at `a[2]`, the remaining half of the F5 gap. Contrast
    // `constant_index_field_precision`, where keeping two *constant* indices distinct is the correct,
    // precise answer.
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
    // `@p0` is a parameter reference, so it resolves without consulting any local-name table.
    let src_exp = exp_from_str("@p0", &ctadl_ir::Locals::default());
    let carries_src = direct_calls_in(&prog, "f")
        .iter()
        .any(|(callees, args)| callees.iter().any(|c| c == "printf") && args.contains(&src_exp));
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
    check_param_into_global_in(&s, &si, "set", 0, ".g");
    check_returns_global_in(&s, &si, "get", ".g");
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

/// `import_c` registers called-but-undefined functions as empty-body externs (so taint
/// models can match `source`/`sink` by name) and attaches source-info spans to the IR it
/// lowers. This tests both behaviors on the import path, which -- unlike
/// `parse_c_program` used elsewhere in this file -- performs them.
#[test_log::test]
fn import_c_registers_externs_and_spans() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("t.c");
    std::fs::write(
        &file,
        "int source();\nvoid sink(int);\nint f() { int s = source(); sink(s); return s; }\n",
    )
    .unwrap();

    let info = super::import_c(&file).unwrap();

    // `source` and `sink` are only declared, so they must appear as empty-body externs.
    let find = |name: &str| info.program.functions.iter().find(|func| func.name == name);
    for name in ["source", "sink"] {
        let func = find(name).unwrap_or_else(|| panic!("extern function `{name}` not registered"));
        assert!(
            func.blocks.is_empty(),
            "extern `{name}` should have no body"
        );
    }
    // The defined function keeps its body.
    assert!(!find("f").unwrap().blocks.is_empty());

    // At least one lowered statement carries a real source span (not the NO_SPAN default).
    let has_span = info.program.functions.iter().any(|func| {
        func.blocks
            .iter()
            .flat_map(|b| b.statements.iter())
            .any(|s| s.source_info.span_id != source_info::NO_SPAN)
    });
    assert!(has_span, "no statement carried a source-info span");
}

/// End-to-end check of the `Variable(name)` source/sink selector: Stage 1 resolves the local
/// name to a base `LocalIdx`, and Stage 2 (`build_query_endpoints`) seeds exactly ONE versioned
/// vertex — the lowest *existing* SSA version — against a real index. `buf` is defined twice, so
/// it has multiple SSA versions; the test pins that we seed one (the lowest), not one per version.
#[test_log::test]
fn variable_port_selects_lowest_ssa_version() {
    use crate::facts::{Function, TaintDirection};
    use crate::models::ProgramModelMatches;
    use crate::models::json::ModelGeneratorIngest;
    use crate::models::{ImportScope, ProgramMatchIndex};
    use crate::query_engine::build_query_endpoints;
    use ctadl_ir::ProgramInfo;
    use serde_json::json;

    // `buf` is assigned twice → two SSA versions (`%L{idx}_1`, `%L{idx}_2`).
    let src = r"
        int MySource();
        int Other();
        void MySink(int x);
        void f() {
            int buf = MySource();
            MySink(buf);
            buf = Other();
            MySink(buf);
        }";

    // Stage 1 runs on the pre-SSA program (local names → base LocalIdx).
    let ingest_prog = program_from_string(src).0;
    // `%L{idx}` for `buf`; strip the `%L` to get the numeric base index the selector resolves to.
    let render = local_render(&ingest_prog, "f", "buf");
    let idx: u32 = render.strip_prefix("%L").unwrap().parse().unwrap();
    let program_info = ProgramInfo {
        program: ingest_prog,
        ..Default::default()
    };
    let mut matches = ProgramModelMatches::default();
    {
        let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
        let mut ingest = ModelGeneratorIngest::new(&match_index, &mut matches);
        let generator = json!({
            "find": "methods",
            "where": [{"constraint": "name", "pattern": "^f$"}],
            "model": {"sources": [{"kind": "K", "port": "Variable(buf)"}]}
        });
        ingest.encode_models(vec![generator]).unwrap();
    }
    // Stage 1 recorded exactly one endpoint row, tagged `Local`, carrying the base index.
    let rows = &matches.endpoints;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].selector_ty,
        crate::models::FormalIndexTypeTag::Local
    );
    assert_eq!(rows[0].local_index, Some(idx));

    // Index the same source and run Stage 2.
    let (facts, source_info, assign_like) = index_program(program_from_string(src).0);
    let f_id = source_info
        .sites
        .get_function_id(Function("f".into()))
        .expect("function f indexed");

    // The lowest existing SSA version of `buf` among the real graph vertices — the vertex we
    // expect Stage 2 to seed. Computed from the index (not hard-coded) so the test tracks SSA
    // numbering, while still proving "lowest existing version" (version 0 is typically dead).
    let prefix = format!("%L{idx}_");
    let expected_version = assign_like
        .iter()
        .flat_map(|(func, v1, _, v2, _)| [(*func, *v1), (*func, *v2)])
        .filter(|(func, _)| *func == f_id)
        .filter_map(|(_, v)| {
            v.as_local()?
                .as_str()
                .strip_prefix(&prefix)?
                .parse::<u32>()
                .ok()
        })
        .min()
        .expect("buf has at least one versioned vertex");
    // `buf` really does have more than one version, so "pick one" is a meaningful claim.
    let distinct_versions = assign_like
        .iter()
        .flat_map(|(func, v1, _, v2, _)| [(*func, *v1), (*func, *v2)])
        .filter(|(func, _)| *func == f_id)
        .filter_map(|(_, v)| {
            v.as_local()?
                .as_str()
                .strip_prefix(&prefix)?
                .parse::<u32>()
                .ok()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        distinct_versions.len() >= 2,
        "fixture should give buf multiple SSA versions, got {distinct_versions:?}"
    );

    let crate::query_engine::BuiltEndpoints {
        endpoints: eps,
        formals,
        ..
    } = build_query_endpoints(&matches.endpoints, &facts, &source_info.sites, &assign_like);

    // Exactly one endpoint (not one per version), anchored in f, forward, and — since a local is
    // not a formal — no formal registered.
    assert_eq!(eps.len(), 1, "expected exactly one seeded vertex");
    let (ep,) = &eps[0];
    assert_eq!(ep.infunc, f_id);
    assert_eq!(ep.direction, TaintDirection::Forward);
    assert_eq!(ep.call_site, None);
    assert!(formals.is_empty(), "a local selector registers no formal");
    let seeded = ep.vertex.0.as_local().expect("seeded a local vertex");
    assert_eq!(seeded.as_str(), format!("%L{idx}_{expected_version}"));
}

/// `Variable(name)` is resolved to a `LocalIdx` *per matched function*, not once for the
/// generator: one generator matching several functions must record each function's own index for
/// the same name. `g2` declares two locals ahead of `buf`, so `buf` lands on a different index
/// there than in `g1` — a resolution hoisted out of the per-function loop would give both rows the
/// same index and fail here. `g3` has no `buf` at all: that function is skipped with a warning
/// (the other matches still emit) rather than failing the whole model.
#[test_log::test]
fn variable_port_resolves_per_matched_function() {
    use crate::models::ProgramModelMatches;
    use crate::models::json::ModelGeneratorIngest;
    use crate::models::{ImportScope, ProgramMatchIndex};
    use ctadl_ir::ProgramInfo;
    use serde_json::json;

    let src = r"
        int MySource();
        void MySink(int x);
        void g1() {
            int buf = MySource();
            MySink(buf);
        }
        void g2() {
            int pad1 = MySource();
            int pad2 = MySource();
            int buf = MySource();
            MySink(pad1);
            MySink(pad2);
            MySink(buf);
        }
        void g3() {
            int other = MySource();
            MySink(other);
        }";

    let prog = program_from_string(src).0;
    // Expected base index per function, read from each function's own locals table.
    let want = |func: &str| -> u32 {
        local_render(&prog, func, "buf")
            .strip_prefix("%L")
            .unwrap()
            .parse()
            .unwrap()
    };
    let (g1_idx, g2_idx) = (want("g1"), want("g2"));
    assert_ne!(
        g1_idx, g2_idx,
        "fixture must give buf different indices in g1/g2 for this test to mean anything"
    );

    let program_info = ProgramInfo {
        program: prog,
        ..Default::default()
    };
    let mut matches = ProgramModelMatches::default();
    {
        let match_index = ProgramMatchIndex::new(&program_info, ImportScope::unknown());
        let mut ingest = ModelGeneratorIngest::new(&match_index, &mut matches);
        let generator = json!({
            "find": "methods",
            "where": [{"constraint": "name", "pattern": "^g[0-9]$"}],
            "model": {"sources": [{"kind": "K", "port": "Variable(buf)"}]}
        });
        // g3 lacking `buf` is a skip, not an error.
        ingest.encode_models(vec![generator]).unwrap();
    }

    let rows: std::collections::BTreeMap<&str, Option<u32>> = matches
        .endpoints
        .iter()
        .map(|r| {
            assert_eq!(r.selector_ty, crate::models::FormalIndexTypeTag::Local);
            (r.function.as_str(), r.local_index)
        })
        .collect();
    assert_eq!(
        rows,
        std::collections::BTreeMap::from([("g1", Some(g1_idx)), ("g2", Some(g2_idx))]),
        "expected one row per matched function that has `buf`, each with that function's own index"
    );
}

// --- Positional record initializers ------------------------------------------------------
//
// A brace initializer's elements must land on the paths a later read resolves to. For a record
// that means the *members* those positions name, not array element slots: a write at `p.deref`
// is not observed at a read of `p.x`, so numbering a record's elements silently drops the taint
// rather than over-approximating it. The layout comes from the `struct_layouts` registry, and
// nested braces recurse with the layout of whatever the inner level is (a member's own record
// type, or an array's element type).

#[test_log::test]
fn struct_positional_initializer_maps_onto_members() {
    // `struct P p = { v, 0 }` writes `p.x` and `p.y` -- the same paths `p.x = v; p.y = 0;`
    // produce -- so the tainted element reaches the return through `p.x`.
    let src = r"
        struct P { int x; int y; };
        int f(int v) {
            struct P p = { v, 0 };
            return p.x;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "p.x", ["@p0"], None);
    check_assign_or_update(&prog, "p.y", ["#0"], None);

    let summary = get_summary(prog).unwrap().0;
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn struct_nested_in_struct_initializer_maps_onto_members() {
    // A brace nested at a record member's position is that member's own record, so it recurses
    // with the member's layout: `{ { v, 0 }, 0 }` writes `r.q.a`, which `r.q.a` reads.
    let src = r"
        struct Q { int a; int b; };
        struct R { struct Q q; int z; };
        int f(int v) {
            struct R r = { { v, 0 }, 0 };
            return r.q.a;
        }";
    let prog = program_from_string(src).0;
    let summary = get_summary(prog).unwrap().0;
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn array_of_structs_initializer_maps_onto_members() {
    // An array's own elements keep the element numbering, but the brace one level down is a
    // record, so it maps onto members: `qs[0].a` reads what `{ { v, 0 }, ... }` wrote.
    let src = r"
        struct Q { int a; int b; };
        int f(int v) {
            struct Q qs[2] = { { v, 0 }, { 0, 0 } };
            return qs[0].a;
        }";
    let prog = program_from_string(src).0;
    let summary = get_summary(prog).unwrap().0;
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn typedef_struct_initializer_maps_onto_members() {
    // A typedef'd (otherwise anonymous) record is registered under the typedef name, so a
    // declaration naming it that way finds the same layout.
    let src = r"
        typedef struct { int x; int y; } P;
        int f(int v) {
            P p = { v, 0 };
            return p.x;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "p.x", ["@p0"], None);

    let summary = get_summary(prog).unwrap().0;
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn pointer_member_is_not_treated_as_an_inline_record() {
    // A *pointer* member is not stored inline, so a brace at its position is not that record's
    // body and must not be mapped onto its members. The element keeps the positional fallback;
    // what matters is that no wrong member path is written and lowering does not recurse
    // forever on a self-referential type.
    let src = r"
        struct Q { int a; int b; };
        struct S { struct Q *q; int z; };
        struct N { int v; struct N *next; };
        int f(int v) {
            struct S s = { 0, v };
            struct N n = { v, 0 };
            return s.z;
        }";
    let prog = program_from_string(src).0;
    // The scalar members still map by name...
    check_assign_or_update(&prog, "s.z", ["@p0"], None);
    check_assign_or_update(&prog, "n.v", ["@p0"], None);
    // ...and `s.z` carries the taint to the return.
    let summary = get_summary(prog).unwrap().0;
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn unknown_record_type_falls_back_to_positional_elements() {
    // A record defined in another translation unit has no layout here. That must not error:
    // the elements take the pre-existing element numbering, which is what this frontend did
    // for every record before layouts existed.
    let src = r"
        int f(int v) {
            struct Elsewhere e = { v, 0 };
            return v;
        }";
    let prog = program_from_string(src).0;
    check_assign_or_update(&prog, "e.deref", ["@p0"], None);
}

// --- C99 compound literals ------------------------------------------------------------
//
// `(T){ ... }` is an unnamed object of type `T` initialized by the brace, and the expression's
// value is that object. The frontend materializes a temp for the object and runs the *same*
// brace lowering a declaration's initializer gets, so designators land on `T`'s members and
// array forms take element numbering. Before this the literal hit `flatten_expr`'s catch-all
// (ERR 78) and every value inside the braces was dropped -- the largest gap class in the
// openssh/dropbear corpora (497 + 308 sites).

#[test_log::test]
fn compound_literal_designated_member_flows() {
    // `(struct pair){ .start = src }` must write the *member* `start` of the object it
    // materializes, so the later `p.start` read resolves to it and the param reaches the return.
    let src = r"
        struct pair { int start; int end; };
        int f(int src) {
            struct pair p = (struct pair){ .start = src, .end = 0 };
            return p.start;
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn compound_literal_argument_carries_value() {
    // A literal in argument position is the corpus shape (`f(blocks, ((Range){ .start = s }))`).
    // The call must receive *the object the literal was materialized into*, not the unrelated
    // opaque temp the catch-all recovery used to substitute -- so find the store that put the
    // param at `.start` and require the argument to be that same base variable.
    let src = r"
        struct pair { int start; int end; };
        int use(struct pair p);
        int f(int a) { return use((struct pair){ .start = a, .end = 0 }); }";
    let prog = program_from_string(src).0;
    check_has_direct_call(&prog, "f", "use");

    let param = exp_from_str("@p0", &ctadl_ir::Locals::default());
    let object = statements_of(&prog)
        .find_map(|stmt| match &stmt.kind {
            StatementKind::Store { dest, field, value }
                if field.as_str() == "start" && *value == param =>
            {
                Some(dest.variable_ref.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a store of @p0 at `.start`\n{prog}"));
    let args = direct_calls_in(&prog, "f")
        .into_iter()
        .find(|(callees, _)| callees.iter().any(|c| c == "use"))
        .map(|(_, args)| args)
        .expect("checked above that the call exists");
    assert_eq!(
        args.as_slice(),
        [Exp::Variable(object)],
        "expected the call's argument to be the literal's own object\n{prog}"
    );
}

#[test_log::test]
fn compound_literal_is_no_longer_a_frontend_gap() {
    // Under `force_error_on_ast` any frontend gap becomes a hard error, so `program_from_string`
    // succeeding at all is the assertion that `compound_literal_expression` no longer reports
    // one (ERR 78: Unsupported expression type). This is the spec's minimal reproducer.
    let _strict = super::force_error_on_ast();
    let src = r"
        struct pair { int start; int end; };
        int use(struct pair p);
        int f(int a, int b) { return use((struct pair){ .start = a, .end = b }); }";
    let prog = program_from_string(src).0;
    check_has_direct_call(&prog, "f", "use");
}

#[test_log::test]
fn compound_literal_array_elements_flow() {
    // The array form `(int[]){ a, 0 }` has no members to name, so its elements take the element
    // numbering an array initializer gets: element 0 at `.deref`, element 1 at `.[1].deref`. The
    // rank comes from the type descriptor's *abstract* array declarator -- the unnamed spelling
    // of `int a[]` -- which is why `array_declarator_rank` counts the `abstract_*` kinds too.
    let src = r"
        int use(int *p);
        int f(int a) { return use((int[]){ a, 0 }); }";
    let prog = program_from_string(src).0;
    let object = match call_args(&prog, "f", "use").as_slice() {
        [Exp::Variable(v)] => v.clone(),
        args => panic!("expected `use` to take just the literal's object, got {args:?}\n{prog}"),
    };
    // Recover the object's name so the element stores can be spelled in the path DSL. It is the
    // frontend's temp for the unnamed literal, and which `<tN>` that is depends on how many temps
    // the surrounding expression allocated first -- so ask the program, don't assume a number.
    let idx = object
        .variable
        .local()
        .unwrap_or_else(|| panic!("the literal's object must be a local\n{prog}"));
    let obj = function_named(&prog, "f")
        .expect("checked above that the call exists")
        .locals
        .iter_enumerated()
        .find_map(|(i, decl)| (i == idx).then(|| decl.name.as_str().to_owned()))
        .unwrap_or_else(|| panic!("the argument's local is not in the locals table\n{prog}"));
    check_assign_or_update(&prog, &format!("{obj}.deref"), ["@p0"], None);
    check_assign_or_update(&prog, &format!("{obj}.[1].deref"), ["#0"], None);
}

#[test_log::test]
fn asm_input_flows_to_output() {
    // Inline assembly is a black box, so it is modeled as an operand transfer: any input operand
    // may reach any output operand. Here `a` is the only input and `y` the only output, so the
    // parameter must reach the return through the asm. Before this, `gnu_asm_expression` hit
    // `flatten_expr`'s catch-all and the whole transfer was dropped -- taint laundered through
    // any `__asm__` vanished.
    let src = r#"
        int f(int a) {
            int y = 0;
            __asm__ ("nop" : "=r"(y) : "r"(a) : "cc");
            return y;
        }"#;
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn asm_readwrite_operand_keeps_identity_flow() {
    // A `"+"` constraint is one operand that is both read and written (openssh's
    // `crypto_int16_negative_mask` and its 77 siblings). The old value must be read *before* the
    // write, so `x -> x` survives; treating `"+r"` as write-only would kill the taint on `x`
    // instead of passing it through.
    let src = r#"
        int f(int a) {
            int x = a;
            __asm__ ("sarw $15,%0" : "+r"(x) : : "cc");
            return x;
        }"#;
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn asm_multiple_outputs_all_written() {
    // nginx's `ngx_cpuid`: four outputs fed by one input. Every output operand is a write (the
    // asm defines it), and each receives the blended inputs, so `i` reaches all four. Pins both
    // halves: one write apiece, and the transfer from the single input.
    let src = r#"
        int f(int i) {
            int eax, ebx, ecx, edx;
            __asm__ ( "cpuid" : "=a"(eax), "=b"(ebx), "=c"(ecx), "=d"(edx) : "a"(i) );
            return eax + ebx + ecx + edx;
        }"#;
    let (prog, _dump) = program_from_string(src);
    for out in ["eax", "ebx", "ecx", "edx"] {
        check_writes_to(&prog, out, 1);
    }
    let (s, _si) = get_summary(prog).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn asm_without_operands_lowers() {
    // nginx's `ngx_cpu_pause()`: assembly with no operand lists at all. There is nothing to
    // transfer, so the only requirement is that it is not a gap and does not disturb the
    // surrounding function -- the flow across it still lowers.
    let src = r#"
        int f(int a) {
            int x = a;
            __asm__ ("pause");
            return x;
        }"#;
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn asm_is_no_longer_a_frontend_gap() {
    // Strict-mode pin for the whole class: under `force_error_on_ast` any frontend gap is a hard
    // error, so `program_from_string` succeeding at all is the assertion that none of these asm
    // shapes reports one. Covers the corpus forms -- `"+r"` read-modify-write, two outputs with
    // two inputs, a `__volatile__` qualifier with only clobbers, and the bare no-operand form.
    let _strict = super::force_error_on_ast();
    let src = r#"
        int f(int a, int b) {
            int x = a;
            int y = 0;
            int z = 0, q = 0;
            __asm__ ("nop" : "=r"(y) : "r"(a) : "cc");
            __asm__ ("sarw $15,%0" : "+r"(x) : : "cc");
            __asm__ ("xorw %0,%0\n cmovew %1,%0" : "=&r"(z), "=&r"(q) : "r"(a), "r"(b) : "cc");
            __asm__ __volatile__ ("mfence" ::: "memory");
            __asm__ ("pause");
            return x + y + z + q;
        }"#;
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
    check_returns_param(&s, 1, "");
}

#[test_log::test]
fn asm_output_into_struct_field_is_stored() {
    // An output operand need not be a bare local: `"=m"(p->f)` is a *store* into a field path.
    // That is the case the single-operand blend temp exists for -- a store lowers exactly one
    // value (`add_assign_to_program`), so handing the write two operands would silently drop
    // the second and lose half the transfer.
    let src = r#"
        struct S { int f; int g; };
        void f(int a, struct S *p) {
            __asm__ ("nop" : "=m"(p->f) : "r"(a) : "memory");
        }"#;
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 0, "", 1, ".f");
}

#[test_log::test]
#[ignore = "limitation: `asm goto` transfers control to its label list, which needs CFG edges out \
            of an expression -- `flatten_expr` returns a value and cannot build them. The operands \
            still lower (the data model above applies unchanged); only the jumps are missing, and \
            the construct keeps reporting a frontend gap. No corpus (dropbear/openssh/nginx) uses \
            it. Un-ignore once asm statements can add successors."]
fn asm_goto_is_a_known_limitation() {
    // Aspirational: the `err` label is reachable only through the `asm goto`, so with real CFG
    // edges `a` would reach the return along that path. Today the jump is invisible, the label
    // block has no predecessor carrying `a`, and the flow is absent.
    let src = r#"
        int f(int a) {
            int r = 0;
            __asm__ goto ("jmp %l0" : : "r"(a) : : err);
            return r;
        err:
            return a;
        }"#;
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn statement_expression_value_flows() {
    // A GNU statement expression `({ ...; e; })` has the value of its last statement. The
    // catch-all recovery used to substitute a temp nothing wrote, so `r` was born opaque and
    // the parameter never reached the return.
    let src = r"
        int f(int a) { int r = ({ int t = a; t; }); return r; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn statement_expression_side_effect_is_observed() {
    // The statements *before* the value are the whole point of the construct -- the kernel's
    // `READ_ONCE`/`container_of` do their work there. Here the write to the enclosing local `o`
    // happens inside the braces and the value (`1`) is discarded, so only the side effect can
    // carry the parameter to the return.
    let src = r"
        int f(int a) { int o = 0; int r = ({ o = a; 1; }); return o; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn nested_statement_expression_flows() {
    // Statement expressions nest -- the kernel's RCU accessors put one inside another (see the
    // `expand_files` shape in spec 061). The value expression is lowered by an ordinary
    // `flatten_expr` call, so the arm must be re-entrant.
    let src = r"
        int f(int a) { return ({ int t = ({ int u = a; u; }); t; }); }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn statement_expression_body_block_threads_continuation() {
    // `do { } while (0)` inside the braces opens basic blocks of its own, so the body does not
    // end in the block it started in. That end block has to be threaded back to the caller:
    // lowering the rest of the enclosing statement -- and every statement after it -- into the
    // stale block would strand them behind the loop's terminator, exactly the breakage spec 033
    // fixed for bare blocks. This is the `READ_ONCE` shape the kernel uses everywhere.
    let src = r"
        int f(int a) { int r = ({ do { } while (0); a; }); int o = r; return o; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn void_statement_expression_is_not_a_gap() {
    // `({ do { } while (0); })` -- a statement expression whose last statement is not an
    // expression statement, so it has no value. That is well-defined C, not a gap: it must lower
    // silently, which under `force_error_on_ast` is what `program_from_string` succeeding proves.
    let _strict = super::force_error_on_ast();
    let src = r"
        int f(int a) { ({ do { } while (0); }); return a; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn statement_expression_is_no_longer_a_frontend_gap() {
    // Strict-mode pin for the class: under `force_error_on_ast` any frontend gap is a hard error,
    // so `program_from_string` succeeding at all is the assertion that `compound_statement` no
    // longer reaches `flatten_expr`'s catch-all (ERR 78: Unsupported expression type). This is
    // spec 061's minimal reproducer -- 27,062 occurrences in the kernel census.
    let _strict = super::force_error_on_ast();
    let src = r"
        int f(int a) { int r = ({ int t = a; t; }); return r; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn statement_expression_in_store_position_writes_through() {
    // The kernel's list/RCU accessors put a statement expression in *store* position --
    // `container_of(entry, struct T, member)->field = v`, whose value is an interior address
    // computed from the entry pointer. So the braces have to resolve as an *lvalue*, not merely
    // as a value: an address carrying an offset segment is not a bare variable, and the
    // `flatten_lvalue` catch-all (which accepts only one) reported `not an lvalue:
    // compound_statement` and dropped the store onto a dead temp. The write must land exactly
    // where the direct `(&a[1])->f = x` spelling puts it.
    let _strict = super::force_error_on_ast();
    let src = r"
        struct S { int f; };
        void f(struct S *a, int x) { ({ int t = 0; &a[1]; })->f = x; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 1, "", 0, ".[1].deref.f");
}

#[test_log::test]
fn generic_selection_blends_every_arm() {
    // `_Generic` selects on the *type* of its controlling expression, which this frontend has
    // no way to compute -- so, exactly like a ternary, every association's value is lowered and
    // blended into one temp and any of them may be the result. Two shapes pin that: an arm that
    // is the parameter (the parameter reaches the return through it), and two arms naming
    // *different* parameters, where BOTH have to reach the return -- picking one arm would drop
    // the other's flow. Before this the whole selection was an opaque temp and neither flowed.
    let src = r"
        int f(int a) { return _Generic(a, int: a, default: 0); }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");

    let src = r"
        int f(int a, int b) { return _Generic(a, char: a, default: b); }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
    check_returns_param(&s, 1, "");
}

#[test_log::test]
fn generic_selection_arm_calls_are_in_the_call_graph() {
    // The kernel's type-polymorphic macros put the real work *inside* the arms -- the
    // `__seqprop_*` family, `container_of`, `min`/`max` all dispatch this way -- so collapsing
    // the selection into a temp erased those calls from the call graph entirely. Every arm is
    // lowered, so every arm's callee is a direct call of `f`.
    let src = r"
        int pick_int(int);
        long pick_long(long);
        int f(int a) { return _Generic(a, int: pick_int(a), default: pick_long(a)); }";
    let prog = program_from_string(src).0;
    check_has_direct_call(&prog, "f", "pick_int");
    check_has_direct_call(&prog, "f", "pick_long");
}

#[test_log::test]
fn generic_selection_controlling_expression_is_not_the_value() {
    // C does not evaluate the controlling expression -- `_Generic` inspects its type -- so it is
    // a selection dependence, not a data source, and must not join the blend. That is the
    // ternary condition's treatment, and it is what keeps `a` out of the return here. It is
    // still *lowered*, for its side effects and because the kernel's
    // `_Generic(*(&sl->seqcount), ...)` mentions the object nowhere else.
    let src = r"
        int f(int a, int b) { return _Generic(a, default: b); }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 1, "");
    check_does_not_return_param(&s, 0, "");
}

#[test_log::test]
fn generic_selection_is_no_longer_a_frontend_gap() {
    // The strict-mode pin: `_Generic` used to reach `flatten_expr`'s catch-all (ERR 78), the
    // 4th-largest gap class in the kernel census at 1,589 occurrences across all 30 TUs. With an
    // arm of its own it is not a gap at all, so ingestion succeeds even under CTADL_ERROR_ON_AST.
    let _strict = super::force_error_on_ast();
    let src = r"
        int pick_int(int);
        long pick_long(long);
        int f(int a) { return _Generic(a, int: pick_int(a), default: pick_long(a)); }";
    let prog = program_from_string(src).0;
    check_has_direct_call(&prog, "f", "pick_int");
}

#[test_log::test]
fn generic_selection_in_store_position_writes_through() {
    // `_Generic` also appears on the LEFT of an assignment: the kernel's `INET_ECN_xmit` writes
    // `_Generic(sk, const typeof(*sk) *: container_of(...), default: container_of(...))->tos |=
    // ...`. There is no `flatten_lvalue` arm for it -- the catch-all there routes through
    // `flatten_expr`, and the blend temp this arm yields IS an `Exp::Variable`, so it is accepted
    // without a warning and the store composes back through the copy onto the arm's own base.
    // Pinned here because the alternative -- an lvalue arm -- would have to pick ONE arm's
    // location and silently drop the stores to the others.
    let _strict = super::force_error_on_ast();
    let src = r"
        struct S { int f; };
        void g(struct S *p, int x) { _Generic(p, struct S *: p, default: p)->f = x; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 1, "", 0, ".f");
}

#[test_log::test]
fn if_arm_return_then_statement_strict() {
    // An if-arm that returns, followed by reachable code. The arm's compound diverges,
    // so `walk_compound_statement` must skip the end-of-compound link; linking anyway
    // would push a continuation edge into the Return-terminated arm block and raise the
    // recoverable report, which strict mode promotes to a hard error. This is the only
    // unit-level pin of the skip-link-on-divergence half of the `diverged` logic -- the
    // fresh-block half is pinned by `label_after_return_dataflow`.
    let _strict = super::force_error_on_ast();
    let src = r"
        void g(void);
        void f(int a) {
            if (a) { return; }
            g();
        }";
    super::parse_c_program(src).expect("if-arm return + following statement must not gap");
}
