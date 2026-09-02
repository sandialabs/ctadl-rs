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
passes `&x[1]` (the address `x.[1]`) agree on `x.[2].deref`. A single opaque `Symbol("[N]")`
could not compose that way: nothing relates `Symbol("[1]")` to `Symbol("[2]")`.

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
fn simple_global_assign() {
    // Writing to a name with no local declaration (`a = b;`) resolves to a global store,
    // `$globals.a = b`.
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
    // Each operator that needs flattening gets its own temporary; check the allocator hands
    // out distinct, gap-free names <t0>..<t4> across the whole function, read off the IR.
    // Operands are parameters, so the only temps are the binary operations' own.
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
    // A nested block re-declares `x`, shadowing the outer one; the shadow must not escape
    // its block. Load-bearing: param 0 (assigned only to the inner shadow) must NOT reach
    // the return -- conflating the two would leak it.
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
    // Struct-field writes with plain, deep-load, and blended right-hand sides
    // (params: v=@p0, b=@p1, x=@p2, y=@p3); each summarizes as a flow into the formal's
    // field with no temporaries leaking out, and a just-written field returned shows both
    // the formal path and its resolved source.
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
    // A `do { ... } while(...)`: unlike `while`, the body runs *before* the condition, so
    // setup falls into the body, the body into the condition, and the condition back-edges
    // or exits.
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
// an if followed by a loop). Asserting the full successor graph pins the reconvergence/back-edge
// wiring these combinations introduce, and that block 0 never appears as a successor (no construct
// wires a stray edge back into the entry block).

#[test_log::test]
fn while_with_nested_if_cfg() {
    // A `while` whose body holds an `if` and a trailing statement: the if-join back-edges
    // to the loop condition, never the entry block.
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
    // Two `if`s back-to-back, function falls off the end: two chained diamonds, the first
    // continuation doubling as the second condition, and no edge back to the entry.
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
    // An unbraced `if` immediately followed by an unbraced `while`: the two constructs
    // chain in sequence with no edge back to the entry.
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
    // `&x[1]` is the *address* of an element -- the path `x.[1]` -- and must NOT lower to a
    // load of `x.[1].deref`: that would hand the callee a copy, and any write through the
    // pointer would be lost. Load-bearing: the argument carries the offset, and the element
    // is never read.
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
    // The point of forming the address: element offsets compose across a call. The callee
    // stores at `.[1].deref` through `&x[1]` (the address `x.[1]`), so the write lands on
    // `x.[2].deref` -- exactly where `x[2]` reads. (`test_cli_query_c_sources_and_sinks`
    // runs this shape end to end.)
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
    // read observes it.
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
    // `&s.f` has no address in this IR (a member's byte offset needs type information), so
    // address-of falls back to the value model and passes the member's *value*: a plain
    // pathless argument, and a callee's write through the pointer is lost.
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
    // An explicit array *declaration* `int arr[3];`. Taint written to an element flows back
    // out when the same element is read: `b` (@p1) -> arr.[1] -> return. (This exercises the
    // declaration arm; subscript access is covered elsewhere.)
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
    // `a[i].f` composes the subscript's `.[i]` segment with the field: one slot named by an
    // index *and* a field. The three fixtures pin precision in both dimensions: taint at
    // `a[1].y` is observed there but not at `a[0].y` (same field) or `a[1].x` (same
    // element).
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

#[test_log::test]
fn implicit_return() {
    // `foo` declares `int` and never returns, so `link_blocks` closes its body with a
    // synthesized return that must satisfy the declared arity. The other synthesis sites
    // and the precision pin live in the implicit-return section at the end of this file.
    let src = r"
            int foo() {
            //no explicit_return
            }
        ";
    get_summary(program_from_string(src).0).expect("Verify probably bonked");
}

//TODO_JDB:  I don't think i handled *(p+1) = f; or (p+1)->f()

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
    // An `if/else` with *unbraced* single-statement arms: the else body must not be
    // dropped, and the CFG is the same diamond as `simple_else` (the braced form).
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
    // `else if` desugars to a nested `if` in the outer else: two condition blocks, each
    // branching to exactly its own two arms, with no stray edge straight to the join.
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
    // The fixture has several functions, so it looks them up by name.
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
    // `break` inside a loop must ingest; the taint assigned before the `break` still
    // reaches the return.
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

// --- taint through indirect (function-pointer) calls -------------------------
// `check_returns_param` matches across all functions; routing the value through
// param 1 means a `return <- @p1` summary can only come from `wrap` (the callee
// `id`'s own summary is `return <- @p0`). So these assert that `wrap` carries @p1
// through the (in)direct call to its return.

#[test_log::test]
fn taint_flows_through_direct_call() {
    // Control: a DIRECT call carries taint param->return.
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
    // The same flow through an INDIRECT call via a local function pointer
    // initialized to `id`. Resolves because
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
    // Separate-assignment form: `int (*fp)(int); fp = id; fp(b)`.
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
    // the function-name pre-pass in `lower_units` so `later` is already known when
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
fn taint_flows_through_funcptr_param() {
    // Harder form: the function pointer is a PARAMETER. `apply`'s `return f(x)`
    // carries @p1 (x) to the return only if the indirect call through formal `f`
    // resolves (interprocedurally, since `f` is bound to `id` at the call site).
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
fn taint_flows_through_funcptr_in_struct() {
    // Hardest form: the function pointer lives in a STRUCT FIELD. `o.op(b)`
    // resolves only if field-sensitivity carries `o.op = id` to the indirect call.
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
    // An aggregate brace initializer lowers to per-element stores at successive element
    // addresses -- the same offset + `deref` shape a subscript read resolves to -- so
    // taint deposited in the initializer is observed at a later `a[0]` read.
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
    // A nested aggregate recurses, extending the base path by the outer index. A load/store
    // field is a single symbol, so a two-element write decomposes through an intermediate
    // load (`t = load m.deref`, then `store t.deref := s`); both halves are asserted, and
    // index 0 contributes no offset segment.
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
    // expression to lower. `program_from_string` asserts a clean parse with no
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
    // An AST shape the frontend does not lower (here `offsetof`) is a warning by default:
    // the expression becomes an opaque temp and the rest of the function still lowers, so
    // the param->return flow survives. Skips under CTADL_ERROR_ON_AST (the env var is
    // process-global and tests run in parallel).
    //
    // `offsetof` is only a stand-in for "some expression kind with no arm"; if it is
    // ever lowered, swap in another unhandled kind rather than deleting the test.
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
    // Stray `break`/`continue`/`goto`-to-nowhere are source problems, not frontend gaps.
    // Each recovers as a no-op that does not terminate the block, so following statements
    // still lower and the param->return flow survives. Skips under CTADL_ERROR_ON_AST.
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
    // The strict side: under CTADL_ERROR_ON_AST an unsupported expression is a hard error.
    // Strictness comes from the per-thread override (the env var is process-global and
    // would race the parallel harness). Same stand-in caveat as above.
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
    // Scope-semantics invariant: a bare `{ ... }` in statement position is pure scope --
    // it must neither terminate the enclosing block nor start a new one, and nothing after
    // it may be dropped. Strict mode, so any report fails the test. (The compound-bearing
    // follower is the harder case: `if_chain_then_bare_block_then_if_recovers`.)
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
fn if_chain_then_bare_block_then_if_recovers() {
    // THE regression pin for the bare-block wiring bug: an if/else-if/else chain, a bare
    // `{ }` block (the remnant of a compiled-out `#if`), then a trailing `if`. The
    // bare-block-then-IF pair is the trigger: only a compound-bearing follower asks for
    // the end-of-compound link that would install a bogus implicit `return` and orphan
    // what follows. Strict mode pins that the shape lowers with no gap at all.
    let _strict = super::force_error_on_ast();
    let src = r"
        void teardown(int exitcode) {
            int add_delay = 0;
            if (early) { log(1); }
            else if (authed) { log(2); }
            else { log(3); add_delay = 1; }
            { cleanup(); }
            if (key) { release(key); }
        }";
    let prog = super::parse_c_program(src)
        .expect("the chain + bare block + if shape must not gap in strict mode")
        .0;
    assert!(
        function_named(&prog, "teardown").is_some(),
        "program should still define teardown\n{prog}"
    );
    // The load-bearing half: the statements on either side of the bare block are still
    // reachable -- if the braces closed the shared block, the trailing `if` would be an
    // unreachable island and `release` dead code.
    check_has_direct_call(&prog, "teardown", "cleanup");
    check_has_direct_call(&prog, "teardown", "release");
    get_summary(prog).expect("CFG must verify and index");
}

#[test_log::test]
fn bare_block_then_if_reports_no_gap() {
    // Strict-mode statement that the bare-block shape is gap-free: a bare block does not
    // close the block it shares, so strict ingestion of a bare block followed by an `if`
    // must succeed, and the enclosing block must still branch into the `if`.
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
    // The dataflow half: `r = a` sits behind the `if` that follows the bare block, so if
    // the braces closed the enclosing block, the assignment would live in an unreachable
    // block and @p0 would never reach the return.
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
    // that asks for the end-of-compound link.
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
    // The bare-block arm threads the end *block* back but restores the caller's *scope*:
    // a name declared inside the braces must not be visible after them. `r` is shadowed
    // and assigned `a` inside; the later `return r` must resolve to the outer `r`, so `b`
    // reaches the return and `a` does not.
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
    // The divergence half: the bare block's *divergence* has to reach the enclosing
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
    // divergence signal the enclosing compound would keep filling the block the `return`
    // already terminated, leaving `h()` *after* the terminator in the entry block.
    check_block_count(&prog, 2);
    check_successors(&prog, 1, &[]);
    check_has_direct_call(&prog, "f", "h");
    get_summary(prog).expect("CFG must verify and index");
}

#[test_log::test]
fn goto_label_after_return_lowers() {
    // The goto-cleanup idiom: a label after a diverging statement, reached only through
    // its `goto` edge. The trailing siblings must still be walked -- otherwise the
    // pre-created `out:` block is never visited and `cleanup()` silently drops from the
    // IR. The walk continues in a fresh unreachable block, as after a `goto`; the shape
    // reports no gap in either mode.
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
    // The load-bearing assertion: the label body is actually in the IR -- if the trailing
    // siblings were not walked, `out:`'s block would be empty and this call absent.
    check_has_direct_call(&prog, "f", "cleanup");
    // End-to-end through verify() + SSA + codegen: the CFG satisfies the basic-block
    // contract with no tolerance on the ctadl-ir side.
    get_summary(prog).expect("CFG must verify and index");
}

#[test_log::test]
fn label_after_return_dataflow() {
    // The dataflow half: lowering the label body is only worth anything if taint flows
    // through it. `out:` writes `a` into global `g`, so the summary must carry
    // @p0 -> $globals.g; strict mode also pins that the shape reports NO frontend gap.
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
    // The degenerate case: plain unreachable statements after a `return`, no label. They
    // lower into a dead block that must still be terminated (`verify()` rejects a
    // terminator-less block regardless of reachability) without disturbing the reachable
    // part. Strict mode.
    //
    // `f` is `void` on purpose: the return-arity interplay of the synthesized terminator
    // is pinned by `duplicate_label_orphan_block_terminated` and the implicit-return
    // section, not here.
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
    // The goto-after-return shape does not reach the sweep (trailing siblings are
    // walked), so this points at the shape that does orphan a block: a duplicate
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
    // Duplicate label names: two blocks are pre-created, `label_blocks` keeps only the
    // second, and the first is orphaned -- unreachable AND unterminated, which `verify()`
    // rejects regardless of reachability, so the sweep must patch orphans too. `f`
    // returns `int` deliberately: the invented terminator has to satisfy the return arity
    // like any other, and `verify()` rejects it either way -- missing terminator before
    // the sweep, wrong arity after it.
    if std::env::var_os("CTADL_ERROR_ON_AST").is_some() {
        return;
    }
    let src = r"
        int f(void) {
            goto l;
        l:  a();
        l:  b();
        }";
    get_summary(program_from_string(src).0).expect("orphaned label block must get a terminator");
}

// ---------------------------------------------------------------------------------------
// Labels the walk cannot reach. `lower_function` pre-creates a block for every label
// `collect_labels` finds, so a forward `goto L` resolves; a block for a label the walk
// never enters is an empty orphan. Three ways that happens, each answered where it
// belongs: two by not collecting the label at all, one by not blaming the frontend for
// the parser's wreckage.
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn label_in_an_unevaluated_sizeof_operand_strands_no_block() {
    // `sizeof`'s operand is unevaluated -- `flatten_expr` never walks inside it, and there
    // is no `goto` into an unevaluated operand -- so a pre-created block for a label there
    // would only ever be an orphan, and a "statements dropped" report doubly wrong:
    // nothing runs there. Strict mode: the shape reports nothing at all, and it is the
    // only member of the class that can be pinned strictly (the other two carry an
    // independent report by design).
    let _strict = super::force_error_on_ast();
    let src = r"
        int g;
        int f(int a) {
            g = a;
            int n = sizeof(({ int t = a; lbl: t; }));
            return n;
        }";
    let prog = super::parse_c_program(src)
        .expect("an unevaluated operand's label must not gap")
        .0;
    // The real statements around it still lower: `a` reaches the global.
    let (summary, si) = get_summary(prog).unwrap();
    check_param_into_global_in(&summary, &si, "f", 0, ".g");
}

#[test_log::test]
fn a_nested_functions_label_is_not_the_enclosing_functions() {
    // A label's scope is the function containing it, and the definition query lowers a
    // nested definition as its own function with its own label block -- a second block in
    // the *enclosing* function would be an orphan reported against code that lowered fine.
    // (Real "nested" definitions are usually parse recovery re-parenting what follows.)
    let src = r"
        int outer(int a) {
            int inner(int b) {
                if (b) goto out;
                return 0;
            out:
                return b;
            }
            return a;
        }";
    let reports = reports_for(src);
    assert!(
        !reports
            .iter()
            .any(|(_, m)| m.contains("without a terminator")),
        "the enclosing function must not be charged for the nested one's label: {reports:?}"
    );
    // Exactly the one report this shape is *supposed* to draw: a GNU nested function is a
    // construct this frontend does not model (`nested_function_definition_is_still_a_frontend_gap`).
    assert_eq!(
        reports.len(),
        1,
        "expected only the nested-function gap: {reports:?}"
    );
    assert!(
        reports[0]
            .1
            .contains("Unsupported expression type: function_definition"),
        "unexpected report: {reports:?}"
    );

    // And the label's own statements really do lower -- in `inner`, the function they belong
    // to. `out: return b;` is the only path that returns the parameter.
    let prog = super::parse_c_program(src).expect("ingestion recovers").0;
    let (summary, si) = get_summary(prog).unwrap();
    check_returns_param_in(&summary, &si, "inner", 0, "");
}

#[test_log::test]
fn a_label_stranded_in_recovery_output_is_not_a_frontend_gap() {
    // The third way: a real label stranded in recovery output the walk skips by design. It
    // IS still collected -- a damaged body lowers plenty of good code whose `goto`s need
    // it (see `a_goto_still_resolves_in_a_damaged_body`) -- so what changes is
    // attribution: an unentered label block in a body the parser did not finish is the
    // source's problem, said once per function.
    let src = r"
        int g;
        void f(int a) {
            g = a;
            case 1 ... 3:
        out:
            g = a;
        }";
    let reports = reports_for(src);
    for (attribution, msg) in &reports {
        assert_ne!(
            *attribution, "frontend gap",
            "a label the recovery swallowed is not a frontend gap: {msg}"
        );
    }
    assert!(
        reports.iter().any(|(_, m)| m.contains("not analyzed")),
        "the loss must still be stated once, against the source: {reports:?}"
    );

    // Suppressing the blame must not suppress the analysis: the code that did parse still
    // lowers, so `a` still reaches the global.
    let prog = super::parse_c_program(src).expect("ingestion recovers").0;
    let (summary, si) = get_summary(prog).unwrap();
    check_param_into_global_in(&summary, &si, "f", 0, ".g");
}

#[test_log::test]
fn a_goto_still_resolves_in_a_damaged_body() {
    // The guard on the decision above: stop collecting labels in any damaged body and the
    // warning goes to zero -- by throwing away real dataflow. The `goto`/label pair shares
    // a body with an unparsable construct and must still work: `a` reaches `g` only
    // through `out:`.
    let src = r"
        int g;
        void f(int a) {
            switch (a) { case 1 ... 3: break; }
            if (a) goto out;
            return;
        out:
            g = a;
        }";
    let reports = reports_for(src);
    for (attribution, msg) in &reports {
        assert_ne!(
            *attribution, "frontend gap",
            "nothing here is the frontend's fault: {msg}"
        );
    }
    let prog = super::parse_c_program(src).expect("ingestion recovers").0;
    let (summary, si) = get_summary(prog).unwrap();
    check_param_into_global_in(&summary, &si, "f", 0, ".g");
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
    // `x++` and `--x` each lower to a writeback assignment to `x`; the `+/- 1` is a
    // constant, so the contract is structural: counting writes to `x` (init + two updates
    // = 3) guards that neither was dropped. (`++` and `--` lower identically.)
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

// --- taint through MULTIPLE function-pointer stores into one aggregate ---------------
//
// A second store into the same aggregate creates a new SSA version of the receiver, and
// the stored target must propagate across it to reach the indirect call: the transitive
// rule gates on `paths(p_new)`, so a call's *receiver* path must be registered as a
// program path (ctadl-ascent/src/index_engine/mod.rs):
//     program_paths(p) <-- callee_info(_, _, _, p, _);
//
// Each test routes param 1 through `id` and back; a `return <- @p1` summary can only come
// from `wrap`. Remove the rule above and the two `*_multistore_flows` tests fail while
// `funcptr_single_store_flows` still passes -- that contrast IS the bug guarded.

#[test_log::test]
fn funcptr_single_store_flows() {
    // Control: ONE function pointer stored into a struct field, then called through it.
    // Resolves with the single-store handling alone -- no SSA version hop is needed.
    // Establishes the baseline for the contrast.
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
    // Struct form: TWO function pointers stored into the same struct, then a call
    // through the first. The second store (`o.g = id`) makes a new SSA version of `o`;
    // the `o.f -> id` binding must propagate across it to the call `o.f(b)`.
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
fn funcptr_array_multistore_flows() {
    // Array form: TWO function pointers stored into the same array, then a call
    // through element 0. The `fps[1] = id` store makes a new SSA version of `fps`; the
    // `fps[0] -> id` binding must propagate across it to the call `fps[0](b)`. Same root
    // cause as the struct form.
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
// Cross-function flow (recursion, call-depth, globals), field/struct precision,
// expression-level dataflow, and `#[ignore]`d aspirational tests for constructs
// not yet lowered.
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
    // field path @p0.x reaching the return. (The equivalent `(*p).x` spelling is covered by
    // `deref_paren_field_equivalent`.)
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
    // @p0.x -> return. `flatten_lvalue` resolves the field's object (`(*p)`) recursively --
    // peeling the paren and deref -- so it resolves like `p->x`.
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
    // `consequence` field, so the arm must not be assumed present. Both the condition and
    // the alternative must reach the result.
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
    // The `field ?: fallback` idiom usually blends a field read with a
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
fn alignof_type_is_a_constant() {
    // `_Alignof`/`__alignof__` yields a compile-time alignment, so it lowers to the same thing a
    // numeric literal does: an `Exp::Str` of the node's own source text. It shares
    // `sizeof_expression`'s `flatten_expr` arm because the rule is identical (unevaluated operand,
    // constant result).
    let src = r"
        int f(void) {
            return __alignof__(long);
        }";
    let prog = program_from_string(src).0;
    check_returns_const(&prog, "f", "__alignof__(long)");
}

#[test_log::test]
fn alignof_does_not_carry_operand_taint() {
    // `__alignof__(a)` still does not *evaluate* `a`, so the parameter must NOT reach the
    // return -- the alignof twin of `sizeof_does_not_evaluate`. (`a` parses as a
    // `type_identifier`, and the arm never visits any child.)
    let src = r"
        int f(int a) {
            return __alignof__(a);
        }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_does_not_return_param(&s, 0, "");
}

#[test_log::test]
fn alignof_is_not_a_frontend_gap() {
    // Under `force_error_on_ast` any frontend gap becomes a hard error, so `program_from_string`
    // succeeding at all is the assertion that `alignof_expression` reports none. Both
    // spellings are pinned: the C11 keyword `_Alignof(T)` and the GNU
    // `__alignof__(...)`, over struct tags and pointer types alike.
    let _strict = super::force_error_on_ast();
    for (src, constant) in [
        (
            "int f(void) { return _Alignof(unsigned long long); }",
            "_Alignof(unsigned long long)",
        ),
        (
            "int f(void) { return __alignof__(void *); }",
            "__alignof__(void *)",
        ),
        (
            "struct pad { int x; }; int f(void) { return __alignof__(struct pad); }",
            "__alignof__(struct pad)",
        ),
    ] {
        let prog = program_from_string(src).0;
        check_returns_const(&prog, "f", constant);
    }
}

#[test_log::test]
fn alignof_of_an_expression_operand_is_a_grammar_limit() {
    // The one price of lowering `alignof_expression` to a constant: the grammar accepts
    // ONLY a `type_descriptor`, so `__alignof__(<expr>)` over anything it cannot swallow
    // is a parse error (`__alignof__(p->f)` recovers as a `field_expression` based on the
    // alignof node plus a stray `)`). The recovered tree is what is wrong, so the report
    // is charged to the source; strict mode still rejects it, which is what this asserts.
    // `program_from_string` cannot be used here: it asserts a clean parse.
    let src = r"
        struct holder { char ctx[1]; };
        unsigned f(struct holder *h) { return __alignof__(h->ctx); }";
    let (_prog, has_error, _markup) =
        super::parse_c_program(src).expect("non-strict ingestion recovers");
    assert!(
        has_error,
        "expected tree-sitter-c to reject an expression operand to __alignof__"
    );

    let _strict = super::force_error_on_ast();
    let err = super::parse_c_program(src).expect_err("strict mode must reject the recovered tree");
    let err = err.to_string();
    assert!(err.contains("not analyzed"), "unexpected error: {err}");
    assert!(
        !err.contains("not an lvalue"),
        "a store target inside a body that did not parse must not be charged to the \
         frontend: {err}"
    );
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
    // Taking a local's address and writing through it taints `x`, so `return x` carries
    // src -- here on a by-ref-able parameter, complementing
    // `addr_of_local_write_through_taints_pointee`. The same-block alias resolves `*p` to
    // its pointee, so the store lands on `x`.
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
    // parameter to a later `return x`: the body dataflow lowers and flows src (@p0) -> return.
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
    // Constant subscripts are distinct field paths: `src` into `v.a[0]` must NOT leak to a
    // read of `v.a[1]` -- the array-index analogue of `field_non_interference`. (`.[1]` in
    // the path strings is an *offset*; `.deref` names the memory at that address; index 0
    // contributes no offset segment at all.)
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
    // Mutual recursion across a summary fixpoint: `f` calls `g`, `g` calls `f`, EACH with
    // a base case returning its parameter, which seeds the fixpoint. The base cases are
    // load-bearing: without them the program never returns and the only sound summary is
    // the EMPTY one (correct, not a dropped flow) -- a meaningful mutual-recursion test
    // must supply a terminating base case. Pinned per-function so the two are not
    // conflated.
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
    // A prefix `++i` in the for-update clause reaches the `update_expression` arm directly
    // (unwrapped); the whole node routes to `flatten_update_expression`, which handles
    // prefix and postfix alike -- `child(0)` would be the `++` token itself. Structural
    // contract: `i` is written twice (init clause + `++i`).
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
    // The postfix twin of `for_update_prefix_increment_lowers`: `child.child(0)` of `i++`
    // would be the bare identifier `i`, a read that silently drops the increment. Counting
    // writes to `i` (init + increment = 2) is what catches that; a dump-string or warning
    // check would not.
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
    // `--i` is the same shape as `++i` (the operator token differs, the AST shape does not).
    // Under `force_error_on_ast` any frontend gap becomes a hard error, so
    // `program_from_string` succeeding at all is the assertion that the construct reports
    // none; the write count then pins the decrement itself.
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
    // so y flows to x and on to the return -- the value-as-subexpression behavior. (The frontend
    // loses the pre/post distinction, but either way the operand value reaches x.)
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
    // A non-constant subscript is sound only if it may-alias every concrete index: `a[n]`
    // and `a[0]` lower to the same bare-dereference path, so the write carries. Only index
    // 0 is covered -- `a[n] = src` is still not observed at `a[2]` (module doc). Contrast
    // `constant_index_field_precision`, where keeping two *constant* indices distinct is
    // the correct answer.
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
    // A union aliases its fields, but the collapse only recognizes a variable declared
    // with an explicit `union { .. }` type. Here `U` is a bare type name, so `.a`/`.b`
    // stay disjoint and the flow is dropped -- documenting the typedef-union gap; the
    // supported form is covered live elsewhere.
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
    // Soundness: writing through a local's address must write the *pointee*. The
    // value-copy model (`*p = src` -> `assign p = src`) is sound for reads but drops the
    // write-back; resolving `*p` to its same-block pointee lowers the store to
    // `assign x = src` -- a real def of `x` -- so a later `sink(x)` observes the taint.
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
    // reads the current `x` -- the read path is consistent with the write path (both route
    // the dereference to the pointee).
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
    // The must-points-to is confined to the block the binding was recorded in: once
    // control flow intervenes, `*p` falls back to the value-copy model rather than
    // unsoundly resolving a possibly-stale alias across a branch. The post-if store writes
    // `p`, and the only write to `x` is its initializer.
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
    // Soundness: a union's members share storage, so a write to `u.a` is observable at a
    // read of `u.b`. Union members collapse to the single synthetic field `$union`, so
    // both accesses share one path and the taint carries.
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
    // A character literal (`'a'`) is a compile-time constant, lowered like a numeric literal
    // (no taint). `program_from_string` asserts a
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
// A brace initializer's elements must land on the paths a later read resolves to: for a
// record, the *members* those positions name -- a write at `p.deref` is not observed at a
// read of `p.x`, so element numbering would silently drop the taint. Layouts come from the
// `struct_layouts` registry; nested braces recurse with the inner level's layout.

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
    // the elements take the element-numbering fallback.
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
// array forms take element numbering.

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
    // A literal in argument position: the call must receive *the object the literal was
    // materialized into*, not an unrelated opaque temp -- so find the store that put the
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
fn compound_literal_is_not_a_frontend_gap() {
    // Under `force_error_on_ast` any frontend gap becomes a hard error, so `program_from_string`
    // succeeding at all is the assertion that `compound_literal_expression` reports none.
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
    // parameter must reach the return through the asm.
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
    // A `"+"` constraint is one operand that is both read and written. The old value must be
    // read *before* the
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
    // Four outputs fed by one input. Every output operand is a write (the
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
    // Assembly with no operand lists at all. There is nothing to
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
fn asm_is_not_a_frontend_gap() {
    // Strict-mode pin for the whole class: under `force_error_on_ast` any frontend gap is a hard
    // error, so `program_from_string` succeeding at all is the assertion that none of these asm
    // shapes reports one. Covers the common forms -- `"+r"` read-modify-write, two outputs with
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
fn asm_goto_label_is_reachable() {
    // `err` is reachable *only* through the `asm goto`: it sits after a `return`, so nothing
    // falls into it. With the jump modeled (`link_asm_goto_labels`) the label block has a
    // predecessor and `a` reaches the return along that path; without it the label would be
    // dead IR and this flow absent.
    let src = r#"
        int f(int a) {
            int r = 0;
            __asm__ goto ("jmp %l0" : : "r"(a) : : err);
            return r;
        err:
            return a;
        }"#;
    let (prog, _dump) = program_from_string(src);
    // 0 = entry (holds the asm), 1 = the pre-created `err` block, 2 = the fall-through.
    check_successors(&prog, 0, &[1, 2]);
    let (s, _si) = get_summary(prog).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn asm_goto_also_falls_through() {
    // The other half of the branch: an `asm goto` may jump, but it may equally fall out the
    // bottom, so it must not be modeled as diverging the way a plain `goto` is. Here the only
    // path carrying `a` to the return is the fall-through (`r = a; return r;`) -- the label
    // returns a constant -- so terminating the block with just the label edges would lose it.
    let src = r#"
        int f(int a) {
            int r = 0;
            __asm__ goto ("" : : "r"(a) : : hit);
            r = a;
            return r;
        hit:
            return 0;
        }"#;
    let (prog, _dump) = program_from_string(src);
    // Block 0 holds the asm; 1 is the pre-created `hit` block, 2 the fall-through it opens.
    check_successors(&prog, 0, &[1, 2]);
    let (s, _si) = get_summary(prog).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn asm_goto_multiple_labels_all_link() {
    // The label list is a list: GNU allows any number of targets, and real code uses more
    // than one. Every one of them is an edge, so linking only the first
    // would leave the rest of the arms dead.
    let src = r#"
        int f(int a) {
            int r = 0;
            __asm__ goto ("" : : "r"(a) : : one, two);
            return r;
        one:
            return a;
        two:
            return a + 1;
        }"#;
    let (prog, _dump) = program_from_string(src);
    // 1 and 2 are the pre-created `one`/`two` blocks (pre-scan order), 3 the fall-through.
    check_successors(&prog, 0, &[1, 2, 3]);
    let (s, _si) = get_summary(prog).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn asm_goto_is_not_a_frontend_gap() {
    // Strict-mode pin: under `force_error_on_ast` a frontend gap is a hard error, so
    // `program_from_string` succeeding is the assertion that `asm goto` reports none.
    let _strict = super::force_error_on_ast();
    let src = r#"
        int f(int a) {
            __asm__ goto ("" : : "r"(a) : : hit);
            return 0;
        hit:
            return a;
        }"#;
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn statement_expression_value_flows() {
    // A GNU statement expression `({ ...; e; })` has the value of its last statement, and
    // that value must flow out of the braces.
    let src = r"
        int f(int a) { int r = ({ int t = a; t; }); return r; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn statement_expression_side_effect_is_observed() {
    // The statements *before* the value are the whole point of the construct -- macro
    // expansions do their work there. Here the write to the enclosing local `o`
    // happens inside the braces and the value (`1`) is discarded, so only the side effect can
    // carry the parameter to the return.
    let src = r"
        int f(int a) { int o = 0; int r = ({ o = a; 1; }); return o; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn nested_statement_expression_flows() {
    // Statement expressions nest -- real macro expansions put one inside another. The value
    // expression is lowered by an ordinary
    // `flatten_expr` call, so the arm must be re-entrant.
    let src = r"
        int f(int a) { return ({ int t = ({ int u = a; u; }); t; }); }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn statement_expression_body_block_threads_continuation() {
    // `do { } while (0)` inside the braces opens blocks of its own, so the body does not
    // end where it started; the end block must thread back to the caller or everything
    // after would strand behind the loop's terminator (the bare-block hazard).
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
fn statement_expression_is_not_a_frontend_gap() {
    // Strict-mode pin for the class: under `force_error_on_ast` any frontend gap is a hard
    // error, so `program_from_string` succeeding at all is the assertion that a statement
    // expression's `compound_statement` does not reach `flatten_expr`'s catch-all.
    let _strict = super::force_error_on_ast();
    let src = r"
        int f(int a) { int r = ({ int t = a; t; }); return r; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn statement_expression_in_store_position_writes_through() {
    // A macro expansion can put a statement expression in *store* position --
    // `({ ... })->field = v`, whose value is an interior address. So the braces have to
    // resolve as an *lvalue*, not merely as a value: an address carrying an offset segment
    // is not a bare variable, which is all the `flatten_lvalue` catch-all accepts. The write
    // must land exactly where the direct `(&a[1])->f = x` spelling puts it.
    let _strict = super::force_error_on_ast();
    let src = r"
        struct S { int f; };
        void f(struct S *a, int x) { ({ int t = 0; &a[1]; })->f = x; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 1, "", 0, ".[1].deref.f");
}

#[test_log::test]
fn generic_selection_blends_every_arm() {
    // `_Generic` selects on a type this frontend cannot compute, so every association's
    // value lowers and blends into one temp, like a ternary. Two shapes pin that: an arm
    // that is the parameter, and two arms naming *different* parameters where BOTH must
    // reach the return -- picking one arm would drop the other's flow.
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
    // Type-polymorphic macros put the real work *inside* the arms, so collapsing
    // the selection into a temp would erase those calls from the call graph entirely. Every
    // arm is lowered, so every arm's callee is a direct call of `f`.
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
    // The controlling expression is a selection dependence, not a data source, and must
    // not join the blend -- that keeps `a` out of the return -- but it is still *lowered*
    // for its side effects, since real code often mentions the object nowhere else.
    let src = r"
        int f(int a, int b) { return _Generic(a, default: b); }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 1, "");
    check_does_not_return_param(&s, 0, "");
}

#[test_log::test]
fn generic_selection_is_not_a_frontend_gap() {
    // The strict-mode pin: with an arm of its own, `_Generic` is not a gap at all, so
    // ingestion succeeds even under CTADL_ERROR_ON_AST.
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
    // `_Generic` also appears on the LEFT of an assignment (`_Generic(p, ...)->f |= v`).
    // There is no `flatten_lvalue` arm: the catch-all routes through `flatten_expr`, whose
    // blend temp IS an `Exp::Variable`, so the store composes back through the copy onto
    // the arm's own base. An lvalue arm would have to pick ONE arm's location and silently
    // drop the stores to the others.
    let _strict = super::force_error_on_ast();
    let src = r"
        struct S { int f; };
        void g(struct S *p, int x) { _Generic(p, struct S *: p, default: p)->f = x; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_flow(&s, 1, "", 0, ".f");
}

#[test_log::test]
fn if_arm_return_then_statement_strict() {
    // An if-arm that returns, followed by reachable code: the arm diverges, so the
    // end-of-compound link must be skipped (linking would push a continuation edge into
    // the Return-terminated block, a report strict mode promotes). The only unit pin of
    // the skip-link half; the fresh-block half is `label_after_return_dataflow`.
    let _strict = super::force_error_on_ast();
    let src = r"
        void g(void);
        void f(int a) {
            if (a) { return; }
            g();
        }";
    super::parse_c_program(src).expect("if-arm return + following statement must not gap");
}

// ---------------------------------------------------------------------------------------
// Parse-recovery debris. tree-sitter signals a syntax error in two ways, and
// only one of them is a node kind: an `ERROR` node over text it could not parse, and --
// once it resumes -- an ordinary well-formed subtree re-parented somewhere it does not
// belong. Both are facts about the analyzed source, so both belong to `malformed_source`,
// reported once per region; neither is a gap in this frontend.
// ---------------------------------------------------------------------------------------

/// The reports `parse_c_program` made while ingesting `src`, as `(attribution, message)`.
/// Uses `parse_c_program` directly because `program_from_string` asserts a clean parse and
/// every input here is deliberately unparsable.
fn reports_for(src: &str) -> Vec<(&'static str, String)> {
    let _ = super::take_reports();
    super::parse_c_program(src).expect("non-strict ingestion must recover");
    super::take_reports()
}

#[test_log::test]
fn parse_error_is_a_source_problem_not_a_frontend_gap() {
    // `case A ... B:` is a GNU case range, which tree-sitter-c 0.24.1 has no rule for. The
    // parse error is a fact
    // about the analyzed source, so the one report it draws is attributed to the source --
    // not to the frontend, which no change could make parse it.
    let src = r"
        int f(int c) { switch (c) { case 1 ... 3: return 1; default: return 0; } }
        struct S { int x; };
        int g(int a) { return a; }";
    let reports = reports_for(src);
    assert_eq!(
        reports.len(),
        1,
        "expected exactly one report for one parse error, got {reports:?}"
    );
    let (attribution, msg) = &reports[0];
    assert_eq!(*attribution, "source problem", "wrong attribution: {msg}");
    assert!(msg.contains("parse error"), "unexpected message: {msg}");
    assert!(
        msg.contains("ERROR: ... 3"),
        "the message must still quote the unparsable construct (downstream triage classifies \
         it from this tail): {msg}"
    );

    // The strict switch still fires -- `CTADL_ERROR_ON_AST` promotes source problems too
    // (`error_on_ast_promotes_source_problem`) -- but on the source, not on a frontend gap.
    let _strict = super::force_error_on_ast();
    let err = super::parse_c_program(src).expect_err("strict mode rejects the parse error");
    let err = err.to_string();
    assert!(err.contains("parse error"), "unexpected error: {err}");
    assert!(
        !err.contains("Unknown token") && !err.contains("Unsupported expression type"),
        "nothing on this path may call unexpected_ast any more: {err}"
    );
}

#[test_log::test]
fn declarations_after_a_parse_error_are_not_reported_as_expressions() {
    // The dominant recovery shape, and it is NOT an `ERROR` node's children:
    // tree-sitter recovers from the unparsable construct by re-parenting the declarations
    // that follow into the enclosing `compound_statement`, where they look exactly like
    // block-scope declarations. Reporting each as an unsupported expression would blame the
    // frontend once per re-parented node; instead the one parse error is reported once and
    // the wreckage around it is skipped.
    let src = r"
        int f(int c) {
            switch (c) { case 1 ... 3: return 1; default: return 0; }
            struct S { int x; };
            int g(int a) { return a; }
        }";
    let reports = reports_for(src);
    for (attribution, msg) in &reports {
        assert_ne!(
            *attribution, "frontend gap",
            "parse-recovery debris must not be charged to the frontend: {msg}"
        );
        assert!(
            !msg.contains("Unsupported expression type"),
            "unexpected gap report: {msg}"
        );
    }
    // Exactly the two tiers, once each -- not once per re-parented node. The construct that
    // could not be parsed, named; and the body that therefore holds recovery output, named.
    assert_eq!(reports.len(), 2, "expected the two tiers, got {reports:?}");
    assert!(
        reports[0].1.contains("ERROR: ... 3"),
        "first report must name the unparsable construct: {reports:?}"
    );
    assert!(
        reports[1].1.contains("function `f`") && reports[1].1.contains("not analyzed"),
        "second report must name the body that is not analyzed: {reports:?}"
    );

    // Strict mode is the second, independent pin: any surviving `unexpected_ast` call on
    // this path would be a hard error naming the node kind.
    let _strict = super::force_error_on_ast();
    let err = super::parse_c_program(src)
        .expect_err("strict mode still rejects the parse error itself")
        .to_string();
    assert!(
        !err.contains("function_definition") && !err.contains("struct_specifier"),
        "a re-parented declaration is still being reported as an expression: {err}"
    );
}

#[test_log::test]
fn declarations_after_a_parse_error_still_import() {
    // Suppressing the *warning* must not suppress the *code*. `lower_definitions` queries the
    // whole tree, so a function the recovery re-parented into another function's body is
    // still collected and lowered. Pinned so a future "skip the region" optimisation
    // cannot quietly drop them.
    let src = r"
        void f(int c) {
            switch (c) { case 1 ... 3: return; default: return; }
            int g(int a) { return a; }
        }";
    let (prog, has_error, _markup) = super::parse_c_program(src).expect("ingestion recovers");
    assert!(has_error, "the input is deliberately unparsable");
    assert!(
        function_named(&prog, "g").is_some(),
        "the re-parented definition of `g` must still be collected:\n{prog}"
    );
    let (summary, si) = get_summary(prog).unwrap();
    check_returns_param_in(&summary, &si, "g", 0, "");
}

#[test_log::test]
fn nested_function_definition_is_still_a_frontend_gap() {
    // The scoping pin: the suppression keys on the parse-recovery region, never on the
    // node kind. A GNU nested function in a body that parsed *cleanly* is a real construct
    // this frontend does not model, and it must keep saying so -- otherwise the
    // suppression would read as "function_definition in statement position is fine".
    let src = r"
        int f(int a) { int g(int x) { return x; } return g(a); }";
    let (_prog, has_error, _markup) = super::parse_c_program(src).expect("ingestion recovers");
    assert!(!has_error, "this input must parse cleanly");

    let _strict = super::force_error_on_ast();
    let err = super::parse_c_program(src)
        .expect_err("a nested function in clean source is still a gap")
        .to_string();
    assert!(
        err.contains("Unsupported expression type: function_definition"),
        "unexpected error: {err}"
    );
}

#[test_log::test]
fn parse_error_message_is_truncated() {
    // Preprocessed source puts a macro expansion on one line, so a raw `ERROR` node can
    // run to kilobytes, and an embedded newline would split one warning across log lines.
    // The quote is whitespace-collapsed and cut at `PARSE_ERROR_QUOTE_CHARS`, with the
    // elided count reported; the asserted bound leaves headroom for the message around the
    // quote. The fixture is a macro-built register-variable ladder: one line, over 200
    // characters.
    let src = r#"
        int f(int *p) {
            int r;
            register __typeof__( __builtin_choose_expr(sizeof(*(p))<=sizeof(char),(unsigned char)0,__builtin_choose_expr(sizeof(*(p))<=sizeof(short),(unsigned short)0,__builtin_choose_expr(sizeof(*(p))<=sizeof(int),(unsigned int)0,__builtin_choose_expr(sizeof(*(p))<=sizeof(long),(unsigned long)0,(unsigned long long)0))))) v asm("ax");
            r = v;
            return r;
        }"#;
    let reports = reports_for(src);
    assert_eq!(reports.len(), 1, "expected one report, got {reports:?}");
    let (_attribution, msg) = &reports[0];
    assert!(
        msg.chars().count() <= 280,
        "parse-error message is {} chars, must stay bounded: {msg}",
        msg.chars().count()
    );
    assert!(
        !msg.contains('\n'),
        "a warning must stay on one line: {msg}"
    );
    assert!(
        msg.contains("chars elided"),
        "a cut quote must say how much was cut: {msg}"
    );
}

// ---------------------------------------------------------------------------------------
// Rare declaration- and location-position constructs: GCC's explicit-register variable and
// a string literal used as an array.
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn register_asm_variable_declares_an_ordinary_local() {
    // GCC's explicit-register variable: `asm("eax")` says where `r` lives, not what it is.
    // The declaration is otherwise ordinary, so the variable must be usable. The grammar
    // is the subtlety: the `declarator` field covers a sequence, and tree-sitter
    // distributes a field over every element, so one declared name yields two `declarator`
    // children -- the second is the annotation, which must not be reported as a declarator
    // of an unexpected kind.
    let src = r#"
        int f(int a) {
            register int r asm("eax");
            r = a;
            return r;
        }"#;
    let _ = super::take_reports();
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
    // The report log is what actually pins this -- the real declarator lowers normally
    // either way; it is the annotation child that must stay silent. Checked here rather
    // than in a strict-mode twin because the realistic spelling below also fails to parse,
    // where strict mode would stop on the parse error first.
    assert_eq!(
        super::take_reports(),
        vec![],
        "an explicit-register declaration must draw no report at all"
    );

    // A realistic spelling: a register variable that takes its type from a `__typeof__`
    // and its register from a concatenated string.
    let realistic = r#"
        int g(int *ufd, int v) {
            register __typeof__(*(ufd)) __val_pu asm("%""rax");
            __val_pu = v;
            *ufd = __val_pu;
            return 0;
        }"#;
    let _ = super::take_reports();
    let _ = super::parse_c_program(realistic).expect("non-strict ingestion recovers");
    for (attribution, msg) in super::take_reports() {
        assert_ne!(
            attribution, "frontend gap",
            "this spelling must not be charged to the frontend: {msg}"
        );
    }
}

#[test_log::test]
fn register_asm_variable_at_file_scope_is_not_a_gap() {
    // A file-scope explicit-register variable never reaches `walk_declaration` (which only
    // walks function bodies), so it must stay silent -- a boundary pin;
    // `register_asm_variable_declares_an_ordinary_local` is where the class is actually
    // pinned.
    let _strict = super::force_error_on_ast();
    let src = r#"
        register unsigned long stack_ptr asm("rsp");
        int f(int a) { return a; }"#;
    super::parse_c_program(src)
        .expect("a GCC explicit-register variable must not be reported as a frontend gap");
}

#[test_log::test]
fn asm_label_on_a_declarator_carries_no_dataflow() {
    // The other shape the same syntax spells: an asm label renaming the emitted symbol. On
    // a function the asm sits *inside* the `function_declarator`; on an object it is a
    // second `declarator` child, like the register case. Neither is a value: `g` stays an
    // ordinary call and the annotation contributes no flow.
    let src = r#"
        extern int g(int x) asm("real_g");
        int f(int a) {
            extern int myvar asm("othervar");
            myvar = a;
            return g(myvar);
        }"#;
    let _ = super::take_reports();
    let (prog, has_error, _markup) =
        super::parse_c_program(src).expect("an asm label must not be reported as a frontend gap");
    assert!(!has_error, "this input must parse cleanly");
    assert_eq!(
        super::take_reports(),
        vec![],
        "an asm label must draw no report at all"
    );
    check_has_direct_call(&prog, "f", "g");
}

#[test_log::test]
fn string_literal_subscript_is_a_constant() {
    // A constant lookup table spelled as a subscript on a string literal: an object, so a
    // legitimate subscript base, but also a constant -- nothing to store into, nothing to
    // taint -- so the read lowers to a load of a location nothing else names. Strict mode
    // pins no gap; the summary pins that a tainted index does not taint the result.
    let _strict = super::force_error_on_ast();
    let src = r#"
        int f(int i) { return "\004\002\006\006"[i & 3]; }"#;
    let (prog, has_error, _markup) =
        super::parse_c_program(src).expect("a string-literal subscript must not be a frontend gap");
    assert!(!has_error, "this input must parse cleanly");
    let (s, _si) = get_summary(prog).unwrap();
    check_does_not_return_param(&s, 0, "");
}

#[test_log::test]
fn a_store_target_in_a_damaged_body_is_a_source_problem() {
    // A `typeof` of a cast is a grammar limit, and the recovery re-parents the rest of the
    // statement into `assignment_expression`s that appear nowhere in the source; charging
    // the frontend with "not an lvalue" would blame it for a store position nobody wrote.
    // This pins the store side asking `recovery_region` too. What survives is attribution,
    // not silence: the body is still named once. `3[a]` is the smallest store target that
    // reaches the catch-all from source that parses (see the clean-source twin below).
    let src = r"
        void f(int a, int b) {
            switch (a) { case 1 ... 3: break; }
            3[a] = b;
        }";
    let reports = reports_for(src);
    for (attribution, msg) in &reports {
        assert_ne!(
            *attribution, "frontend gap",
            "parse-recovery debris must not be charged to the frontend: {msg}"
        );
        assert!(
            !msg.contains("not an lvalue"),
            "unexpected gap report: {msg}"
        );
    }
    assert!(
        reports
            .iter()
            .any(|(_, msg)| msg.contains("function `f`") && msg.contains("not analyzed")),
        "the damaged body must still be named once: {reports:?}"
    );
}

#[test_log::test]
fn a_non_lvalue_store_in_clean_source_is_still_a_frontend_gap() {
    // The scoping pin: the suppression keys on the parse-recovery region, never on the
    // node kind. The same store with the parse error removed -- `3[a] = b` is legal C (a
    // commuted subscript), the frontend does not model a literal in base position, and in
    // a cleanly parsed body it must keep saying so.
    let src = r"
        void f(int *a, int b) {
            3[a] = b;
        }";
    let (_prog, has_error, _markup) = super::parse_c_program(src).expect("ingestion recovers");
    assert!(!has_error, "this input must parse cleanly");

    let _strict = super::force_error_on_ast();
    let err = super::parse_c_program(src)
        .expect_err("a non-location store target in clean source is still a gap")
        .to_string();
    assert!(
        err.contains("not an lvalue: number_literal"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------------------
// Names tree-sitter inserted. Where the grammar requires a name and the parse fails,
// tree-sitter INSERTS a zero-width `identifier`/`field_identifier`. The empty string is
// not a name -- it would mint `$globals.` with an empty first path segment, which
// `facts::Path` rejects, making the whole index unqueryable. A token nobody wrote names
// nothing: the body is named once as holding recovery output and the name lowers to a
// fresh temp.
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn inserted_identifier_does_not_become_an_empty_path_segment() {
    // `typeof` of a dereference is a tree-sitter-c 0.24.1 grammar limit (a shape
    // macro-generated accessors produce), and recovering from it leaves
    // the cast's `*` parsed as a multiplication whose right operand the parser has to
    // invent.
    let src = r"
        struct S { unsigned long f; };
        struct S fixed;
        unsigned long g(void) {
            return (unsigned long)((typeof(*(&(fixed.f))) *)((&(fixed.f))));
        }";
    let _ = super::take_reports();
    let (prog, has_error, _markup) =
        super::parse_c_program(src).expect("non-strict ingestion must recover");
    assert!(has_error, "this input is the parse error under test");

    let symbols = field_symbols(&prog);
    assert!(
        !symbols.iter().any(|s| s.is_empty()),
        "an inserted name must not become a path segment; symbols: {symbols:?}\n{prog}"
    );
    // The real global next to it still resolves, so the guard keys on the *name* being
    // empty and not on the body being damaged.
    assert!(
        symbols.contains(&"fixed"),
        "the named global in the same expression must still resolve: {symbols:?}\n{prog}"
    );

    let reports = super::take_reports();
    assert!(
        reports
            .iter()
            .all(|(attribution, _)| *attribution == "source problem"),
        "an inserted token is the source's parse error, not a frontend gap: {reports:?}"
    );
    assert!(
        reports
            .iter()
            .any(|(_, msg)| msg.contains("function `g`") && msg.contains("not analyzed")),
        "the damaged body must be named once: {reports:?}"
    );
}

#[test_log::test]
fn inserted_field_name_does_not_become_an_empty_path_segment() {
    // The other position the grammar requires a name in: the member of a `field_expression`.
    // Appending an empty segment here would be worse than useless -- the access would
    // silently alias the whole object -- so the object's effects stay lowered and the access
    // itself names a dead temp.
    let src = r"
        struct S { int f; };
        int h(struct S *p, int v) {
            p-> = v;
            return p->;
        }";
    let _ = super::take_reports();
    let (prog, has_error, _markup) =
        super::parse_c_program(src).expect("non-strict ingestion must recover");
    assert!(has_error, "this input is the parse error under test");

    let symbols = field_symbols(&prog);
    assert!(
        !symbols.iter().any(|s| s.is_empty()),
        "a missing member name must not become a path segment; symbols: {symbols:?}\n{prog}"
    );
    let reports = super::take_reports();
    assert!(
        reports
            .iter()
            .all(|(attribution, _)| *attribution == "source problem"),
        "a missing member name is the source's parse error, not a frontend gap: {reports:?}"
    );
}

#[test_log::test]
fn a_named_global_still_lowers_to_a_symbolic_field() {
    // The scoping pin: the guard keys on the name being empty, never on "identifier in a
    // damaged body". A global read in a cleanly parsed body must keep the field that names
    // it -- dropping every identifier in a damaged body, or renaming globals to temps,
    // would pass the two tests above and delete the analysis.
    let src = r"
        unsigned long named;
        unsigned long f(void) { return named; }";
    let (prog, dump) = program_from_string(src);
    assert!(
        field_symbols(&prog).contains(&"named"),
        "a global read must still load `$globals.named`\n{dump}"
    );
}

// ---------------------------------------------------------------------------------------
// Implicit returns and the return-arity contract.
//
// A non-`void` function has return arity 1 and `verify()` rejects a `Return` of any other
// arity, so the *empty* return is only well-formed in a `void` function. Three call sites
// synthesize one: `link_blocks` (falling off the end), `finalize_terminators` (patching an
// orphaned block), `walk_return` (a bare `return;`).
//
// Getting this wrong is silent: the C import path never calls `program.verify()`, and the
// post-SSA check passes because `complete()` has rewritten every return into a goto to one
// exit block. `get_summary` below is the only thing that asks -- and one bad function
// fails the whole program's `verify()`. `implicit_return` closes such a block with a local
// that is never written: the indeterminate value C specifies, satisfying the arity
// contract without becoming a conduit.
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn fall_off_end_of_nonvoid_verifies() {
    // The minimal case, and the `link_blocks` call site: no `goto`, no unreachable code, no
    // parse damage -- just a non-`void` function that runs off the end of its body, which is
    // `to_sv.continuation_blidx == None`. An empty synthesized return here would be
    // `InconsistentReturns { expected_arity: 1, actual_arity: 0 }`.
    let src = r"int f(int a) { int b = a; }";
    get_summary(program_from_string(src).0)
        .expect("a non-void function that falls off the end of its body must verify");
}

#[test_log::test]
fn bare_return_in_nonvoid_verifies() {
    // The `walk_return` call site. `return;` in a function declared `int` is legal C and is
    // the shape of half the error paths in any such function, so it has to produce a return
    // of the declared arity too -- while the value-carrying `return a;` on the other path
    // keeps the flow it always had.
    let src = r"int f(int a) { if (a) return; return a; }";
    let (summary, _) = get_summary(program_from_string(src).0)
        .expect("a bare `return;` in a non-void function must verify");
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn implicit_return_carries_no_taint() {
    // The precision pin: the local the synthesized return reads is never assigned, so the
    // parameter reaches `b` and stops. Returning something that *does* have a value (the
    // last temp, or the parameter) would pass the two tests above and invent a return flow
    // in every function that falls off the end.
    let src = r"int f(int a) { int b = a; }";
    let (summary, _) = get_summary(program_from_string(src).0).expect("must verify");
    check_does_not_return_param(&summary, 0, "");
}

#[test_log::test]
fn implicit_return_in_a_void_function_is_still_empty() {
    // The boundary pin: arity 0 means the empty return is the *correct* shape, and inventing
    // an argument there would break `verify()` in the opposite direction. `void` functions
    // that fall off the end are the overwhelming majority of implicit returns in real code,
    // so this is the case the synthesis must not touch.
    let src = r"void f(int a) { int b = a; }";
    let (prog, dump) = program_from_string(src);
    get_summary(prog).expect("a void function that falls off the end must still verify");
    assert!(
        check_no_match(&dump, "<implicit-return>"),
        "a void function needs no return value\n{dump}"
    );
}

// ---------------------------------------------------------------------------------------
// One name, several definitions.
//
// `import_c` lowers every unit into ONE program whose function table is one namespace, so
// definitions C keeps apart meet in it (a file's `static` helper, a header's `static
// inline` included many times). Keyed on the name alone they would merge into one chimera
// -- parameter lists concatenated, the last body's arity imposed on all -- silently.
//
// `plan_definitions` gives each definition an identity: textually identical copies are one
// function, lowered once; genuinely distinct ones keep the bare name for the first (extern
// declarations, cross-file calls, and taint models still resolve by it) and are named for
// their file (`g$util.c`) after that. A call resolves within its own file first, as C
// does.
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn colliding_definitions_of_one_name_verify() {
    // A name defined `static void` in one place and `static int` in another must not
    // merge: the merge would impose the `int` arity on the `void` body's returns
    // retroactively. Both definitions in ONE unit is not C, so the fixture draws a source
    // problem (asserted below) as well as being split -- and the split is what makes each
    // body's arity right and the IR verifiable.
    if std::env::var_os("CTADL_ERROR_ON_AST").is_some() {
        return;
    }
    let src = r"
        static void g(int a) { h(a); }
        static int g(int a) { return a; }";
    let (prog, _dump) = program_from_string(src);
    check_return_arity(&prog, "g", 0);
    check_return_arity(&prog, "g$1", 1);
    assert_eq!(
        function_named(&prog, "g")
            .expect("first definition keeps the name")
            .params
            .len(),
        1,
        "each definition keeps its OWN parameter list; the merge concatenated them"
    );
    get_summary(prog)
        .expect("two definitions sharing a name must not produce IR that fails to verify");
}

#[test_log::test]
fn two_definitions_in_one_translation_unit_are_a_source_problem() {
    // Whose fault it is: two files each defining their own `static g` are ordinary C and
    // draw nothing (next test); two definitions in one unit are the analyzed code's
    // problem, reported once -- naming the second body's identity and what a call here
    // reaches, because the answer is not "both".
    let reports = reports_for(
        r"
        static void g(int a) { h(a); }
        static int g(int a) { return a; }",
    );
    assert_eq!(reports.len(), 1, "expected exactly one report: {reports:?}");
    assert_eq!(reports[0].0, "source problem", "reports: {reports:?}");
    assert!(
        reports[0].1.contains("`g` is defined more than once") && reports[0].1.contains("`g$1`"),
        "report must name the second definition's identity: {reports:?}"
    );
}

#[test_log::test]
fn two_files_each_keep_their_own_static_helper() {
    // Two translation units, each with its own file-local `g`: two functions, each with its
    // own parameter list and its own return arity, and each file's call reaching its own.
    // A merged `g(@p0, @p1)` would hand `cb`'s argument to the other file's body.
    let (prog, _dump) = program_from_files(&[
        (
            "one.c",
            r"static void g(int a) { sink(a); }
              void ca(int x) { g(x); }",
        ),
        (
            "two.c",
            r"static int g(int a) { return a; }
              int cb(int x) { return g(x); }",
        ),
    ]);
    check_has_direct_call(&prog, "ca", "g");
    check_has_direct_call(&prog, "cb", "g$two.c");
    check_return_arity(&prog, "g", 0);
    check_return_arity(&prog, "g$two.c", 1);
    for name in ["g", "g$two.c"] {
        assert_eq!(
            function_named(&prog, name).expect(name).params.len(),
            1,
            "`{name}` takes one parameter; the merge concatenated both lists"
        );
    }
}

#[test_log::test]
fn taint_follows_the_definition_the_calling_file_holds() {
    // The dataflow the split buys, stated as a flow rather than a shape. Only two.c's
    // `g` returns what it was passed, so `cb` returns its parameter and `ca` does not. With
    // one merged `g` there would be one summary for both call sites, and the argument
    // `cb` passed would go to the parameter of the OTHER file's body.
    let (prog, _dump) = program_from_files(&[
        (
            "one.c",
            r"static int g(int a) { return 0; }
              int ca(int x) { return g(x); }",
        ),
        (
            "two.c",
            r"static int g(int a) { return a; }
              int cb(int x) { return g(x); }",
        ),
    ]);
    let (summary, source_info) = get_summary(prog).expect("must verify");
    check_returns_param_in(&summary, &source_info, "cb", 0, "");
    check_does_not_return_param_in(&summary, &source_info, "ca", 0, "");
}

#[test_log::test]
fn a_header_inline_included_twice_is_one_function() {
    // The header-inline case. Both files quote the same characters because both included
    // the same header, so they are not two functions -- they are one, seen twice, and lowering
    // it once is exact rather than an approximation: this frontend lowers text. The
    // parameter-list assertion is the guard: a merge would hold `h`'s parameter twice.
    let inline = r"static inline int h(int a) { return a; }";
    let (prog, dump) = program_from_files(&[
        (
            "a.c",
            &format!(
                "{inline}
int ua(int x) {{ return h(x); }}"
            ),
        ),
        (
            "b.c",
            &format!(
                "{inline}
int ub(int x) {{ return h(x); }}"
            ),
        ),
    ]);
    assert_eq!(
        function_named(&prog, "h")
            .expect("the one `h`")
            .params
            .len(),
        1,
        "one function, one parameter list\n{dump}"
    );
    assert!(
        function_named(&prog, "h$b.c").is_none(),
        "the second copy is the same function, not a second one\n{dump}"
    );
    check_has_direct_call(&prog, "ua", "h");
    check_has_direct_call(&prog, "ub", "h");
    let (summary, source_info) = get_summary(prog).expect("must verify");
    check_returns_param_in(&summary, &source_info, "ua", 0, "");
    check_returns_param_in(&summary, &source_info, "ub", 0, "");
}

#[test_log::test]
fn one_definition_and_an_undefined_callee_are_left_alone() {
    // The ordinary case must not move: one definition still owns its name, a cross-file
    // call still resolves directly, and an undefined name still gets its extern stub --
    // what every taint model matches on, so losing it would silently unhook every model in
    // the import.
    let (prog, _dump) = program_from_files(&[
        ("a.c", r"int g(int a) { return a; }"),
        ("b.c", r"int c(int x) { return g(x) + undefined_sink(x); }"),
    ]);
    check_has_direct_call(&prog, "c", "g");
    assert!(
        function_named(&prog, "g$b.c").is_none(),
        "a name only one file defines is not qualified"
    );
    let stub = function_named(&prog, "undefined_sink").expect("extern stub");
    assert!(stub.blocks.is_empty(), "an extern stub has no body");
    assert_eq!(stub.params.len(), 1, "sized from the call site");
}

#[test_log::test]
fn a_call_from_a_file_defining_no_such_name_takes_the_first_definition() {
    // C would refuse to link this: `c.c` calls a `g` that exists only as two file-local
    // definitions elsewhere, so there is no right answer -- and the front end must pick
    // one or drop the flow. It picks the first definition, the one an `extern` declaration
    // and every taint model resolve to. Recorded because it is a choice, not a
    // consequence.
    let (prog, _dump) = program_from_files(&[
        ("a.c", r"static int g(int a) { return a; }"),
        ("b.c", r"static int g(int a) { return 0; }"),
        ("c.c", r"int cc(int x) { return g(x); }"),
    ]);
    check_has_direct_call(&prog, "cc", "g");
}

#[test_log::test]
fn files_that_share_a_base_name_still_get_distinct_identities() {
    // The minted name is built from the base name because that is the readable half of a
    // path, but a name is resolved against the FULL path -- an import tree may hold any
    // number of `util.c`s, and a call in one of them means its own `g`. Three do here, so
    // the second and third also exercise the `#n` that keeps two minted names apart.
    let (prog, _dump) = program_from_files(&[
        (
            "a/util.c",
            r"static int g(int a) { return a; }
              int ca(int x) { return g(x); }",
        ),
        (
            "b/util.c",
            r"static int g(int a) { return 0; }
              int cb(int x) { return g(x); }",
        ),
        (
            "c/util.c",
            r"static int g(int a) { return 1; }
              int cc(int x) { return g(x); }",
        ),
    ]);
    check_has_direct_call(&prog, "ca", "g");
    check_has_direct_call(&prog, "cb", "g$util.c");
    check_has_direct_call(&prog, "cc", "g$util.c#2");
    let (summary, source_info) = get_summary(prog).expect("must verify");
    check_returns_param_in(&summary, &source_info, "ca", 0, "");
    check_does_not_return_param_in(&summary, &source_info, "cb", 0, "");
    check_does_not_return_param_in(&summary, &source_info, "cc", 0, "");
}

#[test_log::test]
fn a_function_pointer_reference_resolves_within_its_own_file() {
    // A name used as a VALUE resolves the same way a call does. `take(g)` in b.c binds b.c's
    // `g`, so indirect-call resolution follows the function pointer to the body that file
    // actually passed -- the same question `collect_call` answers, asked at the other place
    // a function name can appear.
    let (_prog, dump) = program_from_files(&[
        ("a.c", r"static void g(int a) { }"),
        (
            "b.c",
            r"static void g(int a) { sink(a); }
              void take(void (*fp)(int));
              void b(void) { take(g); }",
        ),
    ]);
    assert!(
        dump.contains("ptr<g$b.c>"),
        "the reference must name b.c's own `g`\n{dump}"
    );
}

// ---------------------------------------------------------------------------------------
// A cast in location position.
//
// A cast is value-preserving, so the location `(T *)e` names is the location `e` names --
// resolved through `flatten_lvalue`'s catch-all when `e` is an object. It is not a
// variable for the one thing a cast exists for here: naming an address that is no
// declared object. `(T *)K` lowers to an `Exp::Str`, and a constant address IS an lvalue
// -- two designations with the same `K` designate the SAME object -- so it gets
// `$globals.<address K>`; a fresh temp per site would make every reference to one
// hardware register a distinct object.
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn store_through_cast_of_a_constant_writes_through() {
    // The store lands on a location the rest of the program can name, instead of on a dead
    // temp. `<t0>` is the address `$globals.<address 0x1000>` loaded out of the globals object
    // -- the same two-statement shape `((struct S *)t->q)->f = x` lowers to, which is the point:
    // a constant address is just another way of spelling the pointer.
    let src = r"
        struct S { int f; };
        void e(int x) { ((struct S *)0x1000)->f = x; }";
    let (prog, _dump) = program_from_string(src);
    check_loads(&prog, "$globals.<address 0x1000>");
    check_assign_or_update(&prog, "<t0>.f", ["@p0"], None);
}

#[test_log::test]
fn a_store_through_a_literal_address_is_observed_at_a_read() {
    // Why the location is a global keyed on the constant: the two `0x2000` occurrences are
    // the same object, so the write reaches the read. A per-site temp would lose the flow
    // -- and so would the read-side pass-through, which is why the dereference of a
    // constant address resolves the same way on both sides.
    let src = r"int roundtrip(int x) {
                    *(volatile int *)0x2000 = x;
                    return *(volatile int *)0x2000;
                }";
    let (summary, _) = get_summary(program_from_string(src).0).expect("must verify");
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn cast_in_lvalue_position_is_not_a_frontend_gap() {
    // The macro-expanded spelling, in strict mode: `((struct box *)0)->v` inside
    // `__builtin_types_compatible_p(typeof(...), ...)`. `typeof` parses as a call, which
    // is what walks the argument and reaches the cast.
    let src = r#"
        struct box { int v; };
        int f(struct box *p) {
            return __builtin_types_compatible_p(typeof(*(p)),
                                                typeof(((struct box *)0)->v));
        }"#;
    let (_prog, has_error, _dump) = super::parse_c_program(src).expect("ingestion recovers");
    assert!(!has_error, "this input must parse cleanly");

    let _strict = super::force_error_on_ast();
    super::parse_c_program(src)
        .expect("a cast of a constant in location position is not a frontend gap");
}

#[test_log::test]
fn a_cast_of_an_object_in_lvalue_position_is_unchanged() {
    // The scoping pin: every shape whose cast operand is an OBJECT keeps the ordinary
    // lowering (those never go through the constant path), keeping the constant-address
    // arm strictly additional rather than a rewrite of the catch-all.
    let thru_param = r"
        struct S { int f; };
        void a(char *p, int x) { ((struct S *)p)->f = x; }";
    let (prog, _) = program_from_string(thru_param);
    check_assign_or_update(&prog, "@p0.f", ["@p1"], None);

    let thru_field = r"
        struct S { int f; };  struct T { char *q; };
        void b(struct T *t, int x) { ((struct S *)t->q)->f = x; }";
    let (prog, _) = program_from_string(thru_field);
    check_loads(&prog, "@p0.q");
    check_assign_or_update(&prog, "<t0>.f", ["@p1"], None);

    let deref_of_field = r"
        struct T { char *q; };
        void c(struct T *t, int x) { *(int *)(t->q) = x; }";
    let (prog, _) = program_from_string(deref_of_field);
    check_loads(&prog, "@p0.q");
    check_assign_or_update(&prog, "<t0>", ["@p1"], None);

    let thru_arithmetic = r"
        struct S { int f; };
        void d(char *p, int x) { ((struct S *)(p - 8))->f = x; }";
    let (prog, _) = program_from_string(thru_arithmetic);
    check_assign_or_update(&prog, "<t0>", ["@p0", "#8"], None);
    check_writes_to(&prog, "<t0>.f", 1);

    // and none of the four says anything about the frontend.
    let reports = reports_for(&format!(
        "{thru_param}\n{thru_field}\n{deref_of_field}\n{thru_arithmetic}"
    ));
    assert!(reports.is_empty(), "unexpected reports: {reports:?}");
}

#[test_log::test]
fn a_null_pointer_constant_is_still_a_constant_value() {
    // The other boundary, and why the constant-address reading never lives in
    // `flatten_expr`'s `cast_expression` arm: `(struct S *)0` in VALUE position is the
    // null pointer constant. Giving it a location would make every `p = NULL` a reference
    // to one shared object, aliasing every null-valued pointer with every other.
    let src = r"void n(void) { int *p; p = (int *)0; }";
    let (prog, dump) = program_from_string(src);
    check_assign_or_update(&prog, "p", ["#0"], None);
    assert!(
        check_no_match(&dump, "<address"),
        "a null pointer constant names no location\n{dump}"
    );
}

// ---------------------------------------------------------------------------------------
// A definition that returns a pointer.
//
// tree-sitter-c wraps the `function_declarator` in one `pointer_declarator` per `*`, so a
// query demanding a `function_declarator` directly would never match one: body never
// walked, parameters never bound, arity 0, no warning. A soundness hole, not just missing
// coverage: the NAME still reaches the function table (prototype, or extern stub at a
// call site), so a call would lower to an arity-0 stub and taint could not come back out.
// `function_definition_query` captures the declarator whole; `function_head` unwraps it.
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn pointer_returning_definition_is_collected() {
    // The minimal case. If the query missed it, `dup` would not be in the program at all --
    // so this fails on the very first assertion rather than on the arity.
    let src = r"char *dup(char *s) { return s; }";
    let (prog, dump) = program_from_string(src);
    assert!(
        function_named(&prog, "dup").is_some(),
        "a `char *` definition must be collected\n{dump}"
    );
    check_return_arity(&prog, "dup", 1);
    check_params(&prog, &[ByRef]);
    check_block_count(&prog, 1);
}

#[test_log::test]
fn taint_flows_through_a_pointer_returning_function() {
    // With a dropped body the summary of `dup` would say nothing and a caller's taint would
    // die at the call. With the body walked, parameter 0 reaches the return.
    let src = r"char *dup(char *s) { return s; }";
    let (summary, _) = get_summary(program_from_string(src).0).expect("must verify");
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn taint_returns_through_a_call_to_a_pointer_returning_function() {
    // The soundness statement in full, across a call: `use` passes its parameter to `dup` and
    // stores what comes back, so `use` returns its own parameter. If the definition were
    // missed, `dup` would be an arity-0 stub invented from the call site and this flow would
    // not exist.
    let src = r"
        char *dup(char *s) { return s; }
        char *use(char *p) { return dup(p); }";
    let (prog, dump) = program_from_string(src);
    check_has_direct_call(&prog, "use", "dup");
    check_return_arity(&prog, "dup", 1);
    assert!(
        check_no_match(&dump, "define dup(@p0[byref]) -> 0"),
        "`dup` must not be an arity-0 stub\n{dump}"
    );
    let (summary, info) = get_summary(prog).expect("must verify");
    check_returns_param_in(&summary, &info, "use", 0, "");
}

#[test_log::test]
fn double_pointer_returning_definition_is_collected() {
    // Pointer depth is not something the query enumerates: `char **` and `char ***` are two and
    // three nested `pointer_declarator`s, and `function_head` walks all of them.
    let two = r"char **argv_of(char **v) { return v; }";
    let (prog, dump) = program_from_string(two);
    assert!(
        function_named(&prog, "argv_of").is_some(),
        "a `char **` definition must be collected\n{dump}"
    );
    check_return_arity(&prog, "argv_of", 1);
    check_block_count(&prog, 1);

    let three = r"char ***deep(char ***v) { return v; }";
    let (prog, dump) = program_from_string(three);
    assert!(
        function_named(&prog, "deep").is_some(),
        "a `char ***` definition must be collected\n{dump}"
    );
    check_return_arity(&prog, "deep", 1);
}

#[test_log::test]
fn taint_returns_through_a_double_pointer_return() {
    // The flow through a `char **` return, routed through a struct member so the assertion
    // is about the return shape alone (parameter binding of nested declarators is pinned by
    // its own tests).
    let src = r"
        struct env { char **argv; };
        char **argv_of(struct env *e) { return e->argv; }";
    let (prog, dump) = program_from_string(src);
    assert!(
        function_named(&prog, "argv_of").is_some(),
        "a `char **` definition must be collected\n{dump}"
    );
    check_return_arity(&prog, "argv_of", 1);
    let (summary, _) = get_summary(prog).expect("must verify");
    check_returns_param(&summary, 0, ".argv");
}

#[test_log::test]
fn every_spelling_of_a_pointer_return_is_collected() {
    // Every spelling, with `one` and `two` bracketing the five pointer-returning ones as
    // controls. `void *` is the one that also needs the arity rule -- its `type:` capture
    // IS `void`, but the `void` describes the pointee, so the function returns a value.
    let src = r"
        struct S { int f; };
        int one(int a) { return a; }
        char *d1(char *s) { return s; }
        static char *d2(char *s) { return s; }
        char * d3(char *s) { return s; }
        struct S *d4(struct S *s) { return s; }
        void *d5(void *s) { return s; }
        int two(int a) { return a; }";
    let (prog, dump) = program_from_string(src);
    for name in ["one", "d1", "d2", "d3", "d4", "d5", "two"] {
        assert!(
            function_named(&prog, name).is_some(),
            "`{name}` must be collected\n{dump}"
        );
        check_return_arity(&prog, name, 1);
    }
    let (summary, info) = get_summary(prog).expect("must verify");
    for name in ["d1", "d2", "d3", "d4", "d5"] {
        check_returns_param_in(&summary, &info, name, 0, "");
    }
}

#[test_log::test]
fn a_void_definition_still_returns_nothing() {
    // The arity rule cuts both ways: `void f()` is still arity 0. Reading `returns_pointer`
    // before the `void` check must not turn every `void` function into one that returns.
    let src = r"void f(int a) { int b; b = a; }";
    let (prog, _dump) = program_from_string(src);
    check_return_arity(&prog, "f", 0);
}

#[test_log::test]
fn a_pointer_returning_prototype_is_still_not_a_definition() {
    // The boundary. A declaration is a `declaration` node, not a `function_definition`, so
    // widening the definition query must not start inventing bodies for prototypes: `strdup`
    // is known (a call resolves to it, via `define_extern_functions`) and empty.
    // (Through `program_from_files`, because it is the import path that runs
    // `define_extern_functions` and so the only one where a prototype gets a function at all.)
    let (prog, dump) = program_from_files(&[(
        "s.c",
        r"char *strdup(const char *);
          char *caller(char *p) { return strdup(p); }",
    )]);
    check_has_direct_call(&prog, "caller", "strdup");
    let stub = function_named(&prog, "strdup").expect("the extern pass names it");
    assert!(
        stub.blocks.is_empty(),
        "a prototype has no body to collect\n{dump}"
    );
    // and `caller`, which IS a definition, does have one.
    let defined = function_named(&prog, "caller").expect("a definition is collected");
    assert!(!defined.blocks.is_empty(), "caller has a body\n{dump}");
}

#[test_log::test]
fn a_parenthesized_declarator_declares_what_it_wraps() {
    // The other wrapper a naive query could not follow. A definition whose name is protected
    // by a function-like macro -- `#define f(x) (impl_f(x))` -- preprocesses to parentheses
    // around the whole function declarator:
    // `static unsigned (impl_f(unsigned char *p)) { ... }`.
    // `(f)(int)`, parentheses around just the name, is
    // the same idiom written on purpose. Both declare `f`, so both are collected.
    let whole = r"static unsigned (impl_f(unsigned char *msgp)) { return *msgp; }";
    let (prog, dump) = program_from_string(whole);
    assert!(
        function_named(&prog, "impl_f").is_some(),
        "parens around the declarator do not change what it declares\n{dump}"
    );
    check_return_arity(&prog, "impl_f", 1);

    let name_only = r"char *(strdup2)(char *s) { return s; }";
    let (prog, dump) = program_from_string(name_only);
    assert!(
        function_named(&prog, "strdup2").is_some(),
        "parens around the name do not change it either\n{dump}"
    );
    check_return_arity(&prog, "strdup2", 1);
    let (summary, _) = get_summary(prog).expect("must verify");
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn a_declarator_that_names_no_function_is_reported() {
    // The residual, said out loud. A function returning a function pointer
    // (`char *(*signal_handler(int))(int)`) parses as a `function_declarator` wrapping a
    // `parenthesized_declarator`, which names no single function this IR can give a return
    // type -- so it is still dropped, but as a `frontend gap` naming the declarator, not in
    // silence.
    let src = r"
        char *(*signal_handler(int sig))(int) { return 0; }
        int ok(int a) { return a; }";
    let reports = reports_for(src);
    assert!(
        reports.iter().any(|(who, msg)| *who == "frontend gap"
            && msg.contains("unsupported declarator in a function definition")),
        "the dropped definition must be reported: {reports:?}"
    );
    // Recovery is per definition: the next one is still collected.
    let (prog, dump) = program_from_string(src);
    assert!(
        function_named(&prog, "ok").is_some(),
        "recovery must not lose the rest of the file\n{dump}"
    );
}

#[test_log::test]
fn pointer_returning_definitions_of_one_name_keep_their_own_bodies() {
    // The class joins the definition-identity machinery for free, because `function_head`
    // feeds the same pre-pass: two files with their own `static char *fmt` are two
    // functions, not one, and the second is named for its file.
    let (prog, dump) = program_from_files(&[
        ("a.c", "static char *fmt(char *s) { return s; }"),
        (
            "b.c",
            "static char *fmt(char *s) { char *t; t = s; return t; }",
        ),
    ]);
    assert!(
        function_named(&prog, "fmt").is_some() && function_named(&prog, "fmt$b.c").is_some(),
        "two pointer-returning definitions of one name are two functions\n{dump}"
    );
    check_return_arity(&prog, "fmt", 1);
    check_return_arity(&prog, "fmt$b.c", 1);
}

#[test_log::test]
fn a_pointer_returning_definition_is_not_a_frontend_gap() {
    // Strict mode: the whole fixture must import with `CTADL_ERROR_ON_AST` set -- walking
    // these bodies introduces no gap.
    let src = r"
        struct S { int f; };
        char *d1(char *s) { return s; }
        char **d2(char **s) { return s; }
        void *d3(void *s) { return s; }
        struct S *d4(struct S *s) { return s; }";
    let _strict = super::force_error_on_ast();
    super::parse_c_program(src).expect("a pointer-returning definition is not a frontend gap");
}

/* A parameter's declarator nests, and its index is its position.

A one-level enumeration of declarator spellings would drop `char **v` -- and numbering
parameters by MATCH order would take every later parameter down a slot with it, while a
"matches anywhere in the subtree" rule would bind a function-pointer parameter's OWN
parameters as the enclosing function's. Both are index bugs, not just coverage bugs: a
taint model names `Argument(1)`. */

#[test_log::test]
fn a_double_pointer_parameter_is_bound() {
    // Dropping `v` would take `w` down to `@p0`; both must bind at their own index.
    let src = r"void f(char **v, char *w) { char **a; char *b; a = v; b = w; }";
    let (prog, dump) = program_from_string(src);
    check_params(&prog, &[ByRef, ByRef]);
    let (a, b) = (local_render(&prog, "f", "a"), local_render(&prog, "f", "b"));
    assert!(
        check_match(&dump, &format!("assign {a} = @p0")),
        "the double pointer is the FIRST parameter\n{dump}"
    );
    assert!(
        check_match(&dump, &format!("assign {b} = @p1")),
        "and the one after it keeps its own index\n{dump}"
    );
}

#[test_log::test]
fn main_binds_argc_and_argv_in_that_order() {
    // The canonical case: `main(int argc, char **argv)` binds both, in order. Depth 3 is
    // here too, because depth is not a list of cases.
    let src = r"int main(int argc, char **argv) { int n; char **a; n = argc; a = argv; }";
    let (prog, dump) = program_from_string(src);
    check_params(&prog, &[ByVal, ByRef]);
    let (n, a) = (
        local_render(&prog, "main", "n"),
        local_render(&prog, "main", "a"),
    );
    assert!(
        check_match(&dump, &format!("assign {n} = @p0"))
            && check_match(&dump, &format!("assign {a} = @p1")),
        "argc is @p0 and argv is @p1\n{dump}"
    );

    let deep = r"void g(char ***v, int n) { char ***a; int b; a = v; b = n; }";
    let (prog, dump) = program_from_string(deep);
    check_params(&prog, &[ByRef, ByVal]);
    let (a, b) = (local_render(&prog, "g", "a"), local_render(&prog, "g", "b"));
    assert!(
        check_match(&dump, &format!("assign {a} = @p0"))
            && check_match(&dump, &format!("assign {b} = @p1")),
        "`char ***` is one parameter at index 0\n{dump}"
    );
}

#[test_log::test]
fn taint_flows_through_a_double_pointer_parameter() {
    // The soundness statement: with `v` unbound, `return v` would read an implicit GLOBAL of
    // that name and the summary would be empty.
    let src = r"char **argv_of(char **v) { return v; }";
    let (prog, dump) = program_from_string(src);
    check_params(&prog, &[ByRef]);
    check_return_arity(&prog, "argv_of", 1);
    assert!(
        check_match(&dump, "return @p0"),
        "the return reads the parameter, not a global\n{dump}"
    );
    let (summary, _) = get_summary(prog).expect("must verify");
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn a_parameter_is_by_reference_at_every_depth() {
    // `ParameterType` is a property of the declarator's shape, not of how many layers the
    // query happened to spell: anything that dereferences to storage the caller can see is
    // `ByRef`, at any depth, and a plain value is `ByVal`.
    for (src, want) in [
        (r"void f(int a) { int x; x = a; }", ByVal),
        (r"void f(char *a) { char *x; x = a; }", ByRef),
        (r"void f(char **a) { char **x; x = a; }", ByRef),
        (r"void f(char ***a) { char ***x; x = a; }", ByRef),
        (r"void f(char *a[]) { char **x; x = a; }", ByRef),
        (r"void f(char a[10][20]) { char *x; x = a[0]; }", ByRef),
        (r"void f(struct S **a) { struct S **x; x = a; }", ByRef),
    ] {
        let (prog, dump) = program_from_string(src);
        let got = get_only_function(&prog)
            .expect("one function")
            .params
            .parameters
            .raw
            .as_slice();
        assert_eq!(got, &[want], "wrong parameter type for `{src}`\n{dump}");
    }
}

#[test_log::test]
fn a_function_pointer_parameter_is_one_parameter() {
    // The boundary in the other direction: `int (*cb)(int a, int b)` declares ONE
    // parameter, `cb` -- `a` and `b` belong to the type. Matching anywhere under the list
    // would give `g` four parameters and put `s` at index 3. A function pointer stays
    // `ByVal`: what it points at is code, not storage.
    let src = r"void g(int (*cb)(int a, int b), char *s) { char *t; t = s; }";
    let (prog, dump) = program_from_string(src);
    check_params(&prog, &[ByVal, ByRef]);
    let t = local_render(&prog, "g", "t");
    assert!(
        check_match(&dump, &format!("assign {t} = @p1")),
        "`s` is the second parameter, not the fourth\n{dump}"
    );

    // A function-TYPED parameter (`int cb(int)`, which adjusts to a pointer) is the same one
    // parameter, and so is a pointer to a function returning a pointer.
    let typed = r"void h(int cb(int a), char *s) { char *t; t = s; }";
    let (prog, dump) = program_from_string(typed);
    check_params(&prog, &[ByVal, ByRef]);
    let t = local_render(&prog, "h", "t");
    assert!(
        check_match(&dump, &format!("assign {t} = @p1")),
        "a function-typed parameter is one parameter\n{dump}"
    );
}

#[test_log::test]
fn a_nameless_parameter_holds_its_slot_without_binding_a_name() {
    // The other half of "the index is the position": an abstract declarator names nothing, so
    // nothing in the body can read it -- but it still occupies a place in the C parameter
    // list, and `Argument(1)` means the second one. The slot is reserved; no name is bound.
    let src = r"void k(char **, int n) { int x; x = n; }";
    let (prog, dump) = program_from_string(src);
    check_params(&prog, &[ByRef, ByVal]);
    let x = local_render(&prog, "k", "x");
    assert!(
        check_match(&dump, &format!("assign {x} = @p1")),
        "`n` is the second parameter\n{dump}"
    );
}

#[test_log::test]
fn a_void_parameter_list_is_still_empty() {
    // The boundary a "reserve a slot for a nameless parameter" rule would break, and it is
    // everywhere: the `void` in `f(void)` IS the empty list -- a `parameter_declaration`
    // with a type and NO declarator, which reserves nothing. An abstract declarator
    // (`f(char **)`) is the one that does.
    let (prog, _dump) = program_from_string(r"void f(void) { int x; x = 1; }");
    check_params(&prog, &[]);
    let (prog, _dump) = program_from_string(r"void f() { int x; x = 1; }");
    check_params(&prog, &[]);
}

#[test_log::test]
#[ignore = "aspirational: C23's nameless `f(int)` is a parameter, and real definitions do not write one"]
fn a_nameless_value_parameter_would_hold_its_slot_too() {
    // What the no-declarator rule gives up. C23 lets a definition leave a parameter
    // unnamed, so `void f(int, int n)` has two -- but that shape is spelled exactly like
    // `f(void)` and like an unparsable macro-built formal's leavings. Between inventing a
    // parameter for every one of those and dropping this one, practice decides: real
    // definitions do not write it.
    let (prog, _dump) = program_from_string(r"void f(int, int n) { int x; x = n; }");
    check_params(&prog, &[ByVal, ByVal]);
}

#[test_log::test]
fn a_variadic_marker_is_not_a_parameter() {
    // `...` is a `variadic_parameter` node, not a `parameter_declaration`; clang does not
    // count it as a formal either.
    let src = r"void logmsg(const char *fmt, int level, ...) { int x; x = level; }";
    let (prog, dump) = program_from_string(src);
    check_params(&prog, &[ByRef, ByVal]);
    let x = local_render(&prog, "logmsg", "x");
    assert!(
        check_match(&dump, &format!("assign {x} = @p1")),
        "`level` is the second parameter and `...` is not a third\n{dump}"
    );
}

#[test_log::test]
fn a_prototype_declares_no_parameters() {
    // The boundary pin: `collect_params` runs over a DEFINITION's parameter list only (a
    // prototype is a `declaration` node). `takes` is known only because a call resolves to
    // it, and its arity comes from the call site, not from the three declared formals.
    let (prog, dump) = program_from_files(&[(
        "s.c",
        r"void takes(char **, int, int);
          void caller(char **p) { takes(p); }",
    )]);
    check_has_direct_call(&prog, "caller", "takes");
    let stub = function_named(&prog, "takes").expect("the extern pass names it");
    assert!(
        stub.blocks.is_empty() && stub.params.parameters.raw.len() == 1,
        "a prototype's parameter list is not collected\n{dump}"
    );
}

#[test_log::test]
fn a_double_pointer_parameter_is_not_a_frontend_gap() {
    // Strict mode: the whole fixture imports with `CTADL_ERROR_ON_AST` set -- binding these
    // parameters introduces no gap.
    let src = r"
        struct env { char **argv; };
        int main(int argc, char **argv) { char **a; a = argv; return argc; }
        void each(struct env *e, char ***out, int (*cb)(int a, int b), ...) { *out = e->argv; }";
    let _strict = super::force_error_on_ast();
    super::parse_c_program(src).expect("a double-pointer parameter is not a frontend gap");
}

// ---------------------------------------------------------------------------------------------
// `(T)(x)` is a cast, and tree-sitter can only read it as a call.
// ---------------------------------------------------------------------------------------------

#[test_log::test]
fn a_typedef_cast_is_a_value_conversion_not_a_call() {
    // The class: `(( width_t)(x))` is a cast with the operand parenthesized, as
    // macro-generated conversion helpers spell them. tree-sitter reads `(A)(B)` as a call;
    // a call to `( width_t)` would give the taint an empty-bodied stub to disappear into.
    // As a conversion it is value-preserving: `x` reaches the result.
    let src = r"
        typedef unsigned short width_t;
        int convert(int x) {
            int v;
            v = (( width_t)(x));
            return v;
        }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "convert").is_empty(),
        "a cast is not a call\n{dump}"
    );
    let summary = get_summary(prog).unwrap().0;
    check_returns_param(&summary, 0, "");
}

#[test_log::test]
fn a_cast_shaped_call_invents_no_function() {
    // The other half of the hazard: a call to a name nothing defines makes
    // `define_extern_functions` invent it, so the IR would carry non-identifier names
    // (`( width_t)`). Through `program_from_files`, the path that creates the stubs.
    let (prog, dump) = program_from_files(&[(
        "convert.c",
        r"typedef unsigned short width_t;
          void store(int *v, int x) { *v = (( width_t)(x)); }",
    )]);
    assert!(
        function_named(&prog, "( width_t)").is_none(),
        "no function is invented for the cast\n{dump}"
    );
    let odd: Vec<&str> = prog
        .functions
        .functions
        .raw
        .iter()
        .map(|f| f.name.as_str())
        .filter(|name| !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
        .collect();
    assert!(
        odd.is_empty(),
        "invented non-identifier names: {odd:?}\n{dump}"
    );
}

#[test_log::test]
fn a_type_name_from_a_system_header_still_reads_as_a_cast() {
    // `type_names` is filled from every `type_identifier`, not from `typedef` declarations
    // alone: preprocessed input often leaves system headers unexpanded, so real code casts
    // to `u_char` without the unit declaring it. What the unit does hold is uses --
    // `u_char *p` in a parameter list -- which parse as `type_identifier`s.
    let src = r"
        int convert(u_char *p, int x) {
            int v;
            v = (u_char)(x);
            return v;
        }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "convert").is_empty(),
        "a cast to a type the unit only uses is still a cast\n{dump}"
    );
    let summary = get_summary(prog).unwrap().0;
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn a_cast_over_a_comma_expression_yields_its_last_operand() {
    // What `(T)(a, b)` means, and the reason the operand list is walked rather than required
    // to hold exactly one node: tree-sitter cannot tell a cast over a comma expression from a
    // two-argument call any more than it can tell the one-operand case. C evaluates both and
    // the value is the last, so `b` reaches the return and `a` does not.
    let src = r"
        typedef int T;
        int pick(int a, int b) {
            int r;
            r = (T)(a, b);
            return r;
        }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "pick").is_empty(),
        "a cast over a comma expression is not a call\n{dump}"
    );
    let summary = get_summary(prog).unwrap().0;
    check_returns_param(&summary, 1, "");
    check_does_not_return_param(&summary, 0, "");
}

#[test_log::test]
fn a_parenthesized_function_name_is_still_a_direct_call() {
    // The boundary that costs a call edge rather than a conversion: macro libraries expand
    // comparison hooks to `comp = (cmp_fn)(elm, parent);`. A callee named by its source
    // text would be a direct call to `(cmp_fn)` -- a function invented on the spot, the
    // edge to the real one lost.
    let src = r"
        int cmp_fn(int a, int b) { return a; }
        int find(int a, int b) {
            int c;
            c = (cmp_fn)(a, b);
            return c;
        }";
    let (prog, dump) = program_from_string(src);
    check_has_direct_call(&prog, "find", "cmp_fn");
    assert!(
        function_named(&prog, "(cmp_fn)").is_none(),
        "the parentheses are not part of the name\n{dump}"
    );
}

#[test_log::test]
fn a_call_through_a_parenthesized_function_pointer_stays_indirect() {
    // The boundary the disambiguation exists for: `(fp)(1)` is spelled exactly like a cast and
    // is not one. Both legacy spellings -- with and without the dereference -- stay indirect,
    // because `fp` is a variable in scope and a variable in scope is never a type name here.
    let src = r"
        void call_both(void) {
            void (*fp)(int);
            (*fp)(1);
            (fp)(1);
        }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "call_both").is_empty(),
        "neither spelling is a direct call\n{dump}"
    );
    assert_eq!(
        dump.matches("funcptr-call").count(),
        2,
        "both spellings are indirect calls\n{dump}"
    );
}

#[test_log::test]
fn a_plain_direct_call_is_unchanged() {
    // The third boundary: peeling parentheses off a callee must not disturb the
    // callee that never had any.
    let src = r"
        void g(int x);
        void caller(int a) { g(a); }";
    let (prog, dump) = program_from_string(src);
    check_direct_call(&prog, "caller", "g", ["@p0"]);
    assert!(
        check_no_match(&dump, "funcptr-call"),
        "still direct\n{dump}"
    );
}

#[test_log::test]
fn a_local_that_shadows_a_type_name_is_called_not_cast() {
    // `(A)(B)` where `A` is a *variable*, spelled so that the type-name evidence points the
    // wrong way: C lets a block-scope declaration shadow a typedef, so `fp_t` is a type at
    // file scope and a function pointer inside `dispatch`. The scope wins -- the answer is a
    // call -- and it wins by construction, because the shadowing declaration is the one the
    // scope tree resolves.
    let src = r"
        typedef int fp_t;
        void dispatch(int x) {
            void (*fp_t)(int);
            (fp_t)(x);
        }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "dispatch").is_empty(),
        "the local shadows the typedef\n{dump}"
    );
    assert!(
        check_match(&dump, "funcptr-call"),
        "a shadowed type name is a call through the variable\n{dump}"
    );
}

#[test_log::test]
fn a_record_tag_is_not_a_type_name() {
    // Why `type_names` records every `type_identifier` EXCEPT a record tag. `struct stat` is a
    // type; the bare name `stat` is not one, it is the function. Recording the tag would make
    // `(stat)(path, buf)` -- the same redundant-parentheses spelling as `(cmp_fn)` -- read as
    // a cast to `stat`, which silently deletes the call and yields its last argument instead.
    let src = r"
        struct stat { int mode; };
        int stat(char *path, struct stat *buf);
        int probe(char *path, struct stat *buf) { return (stat)(path, buf); }";
    let (prog, dump) = program_from_string(src);
    check_has_direct_call(&prog, "probe", "stat");
    assert!(
        function_named(&prog, "(stat)").is_none(),
        "the parentheses are not part of the name\n{dump}"
    );
}

#[test_log::test]
fn a_global_function_pointer_callee_is_not_a_cast() {
    // The other half of "`A` is a variable, not a type": a file-scope function pointer is
    // not in the scope tree, so what saves this one is that `hook` is not a type name
    // anywhere in the unit. It lands as a direct call to `hook` -- a name, the same one
    // the unparenthesized spelling produces; that is deliberate, see
    // `a_bare_global_callee_is_still_a_name`.
    let src = r"
        void (*hook)(int);
        void fire(int x) { (hook)(x); }";
    let (prog, dump) = program_from_string(src);
    check_has_direct_call(&prog, "fire", "hook");
    assert!(
        function_named(&prog, "(hook)").is_none(),
        "the parentheses are not part of the name\n{dump}"
    );
}

#[test_log::test]
fn a_cast_shaped_call_is_not_a_frontend_gap() {
    // Strict mode: the whole fixture imports with `CTADL_ERROR_ON_AST` set -- reading a cast
    // as a cast introduces no gap of its own.
    let src = r"
        typedef unsigned short width_t;
        int cmp_fn(int a, int b) { return a; }
        int convert(int x, int y) {
            void (*fp)(int);
            (fp)(1);
            (*fp)(2);
            return (( width_t)(x)) + (cmp_fn)(x, y);
        }";
    let _strict = super::force_error_on_ast();
    super::parse_c_program(src).expect("a cast written as a call is not a frontend gap");
}

#[test_log::test]
fn a_statement_expression_callee_is_an_indirect_call() {
    // A macro can expand to a GNU statement expression in callee position,
    // `({ ...; (&fp_object); })(args)`, which is a
    // `parenthesized_expression` wrapping a `compound_statement` -- the cast shape's grammar,
    // and not a cast. Those parentheses are part of the construct, so `unparenthesized_callee`
    // does not peel them; naming the callee by its own source text would invent a function
    // per call site. The value it yields is a function
    // pointer, so the answer is an indirect call through it.
    let src = r"
        void target(int x);
        void fire(int x) { ({ target; })(x); }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "fire").is_empty(),
        "a statement expression names no function\n{dump}"
    );
    assert!(
        function_named(&prog, "({ target; })").is_none(),
        "no function is invented for the braces' source text\n{dump}"
    );
    check_loads(&prog, "$globals.target");
    let callee = local_render(&prog, "fire", "<t1>");
    assert!(
        check_match(&dump, &format!("funcptr-call {callee} ")),
        "the callee is the statement expression's loaded value\n{dump}"
    );
}

// ---------------------------------------------------------------------------------------
// The cell `cast_shaped_call` cannot decide.
//
// `(name)(x)` is a cast if `name` is a type and a call if not, and only the unit's own
// evidence can say which. Two shapes carry none -- `(T)()`, a cast of nothing, and
// `(zzz)(x)` with no evidence either way -- and each draws one report while the lowering
// stays a call. A prototype is positive evidence for a call (`(free)(p)`, the
// macro-suppression idiom) and draws nothing.
// ---------------------------------------------------------------------------------------

#[test_log::test]
fn a_cast_shaped_call_with_no_evidence_is_reported_and_stays_a_call() {
    let src = r"
        int g(int x) { return (zzz)(x); }";
    // Non-strict: one frontend-gap report naming the construct, and the call is still made.
    let reports = reports_for(src);
    assert_eq!(
        reports.len(),
        1,
        "one report for one undecidable callee, got {reports:?}"
    );
    let (attribution, msg) = &reports[0];
    assert_eq!(*attribution, "frontend gap", "wrong attribution: {msg}");
    assert!(
        msg.contains("(zzz)(x)") && msg.contains("cast"),
        "unexpected message: {msg}"
    );
    let (prog, dump) = program_from_string(src);
    let calls = direct_calls_in(&prog, "g");
    assert_eq!(
        calls.len(),
        1,
        "the shape is lowered as a call, as before\n{dump}"
    );
    assert_eq!(calls[0].0, vec!["zzz".to_string()], "{dump}");
    // Strict: the same report is a hard error.
    let _strict = super::force_error_on_ast();
    assert!(
        super::parse_c_program(src).is_err(),
        "strict mode must refuse a callee it cannot tell from a cast"
    );
}

#[test_log::test]
fn a_cast_of_nothing_is_a_source_problem_and_stays_a_call() {
    let src = r"
        typedef unsigned short T;
        int f(void) { return (T)(); }";
    // Non-strict: one source-problem report -- `T` is a type here, and a cast needs an
    // operand -- and the lowering is the call it always was.
    let reports = reports_for(src);
    assert_eq!(
        reports.len(),
        1,
        "one report for one cast of nothing, got {reports:?}"
    );
    let (attribution, msg) = &reports[0];
    assert_eq!(*attribution, "source problem", "wrong attribution: {msg}");
    assert!(
        msg.contains("(T)()") && msg.contains("casts nothing"),
        "unexpected message: {msg}"
    );
    let (prog, dump) = program_from_string(src);
    let calls = direct_calls_in(&prog, "f");
    assert_eq!(calls.len(), 1, "still a call\n{dump}");
    assert_eq!(calls[0].0, vec!["T".to_string()], "{dump}");
    // Strict: a hard error.
    let _strict = super::force_error_on_ast();
    assert!(
        super::parse_c_program(src).is_err(),
        "strict mode must refuse a cast of nothing"
    );
}

#[test_log::test]
fn a_prototype_makes_a_parenthesized_call_a_call() {
    // The one legitimate occupant of the no-evidence cell, and why a prototype counts as
    // evidence: `(free)(p)` is how C code calls a function whose name is also a function-like
    // macro. The unit defines neither, but it declares both -- one of them pointer-returning,
    // so `function_head`'s unwrapping is exercised too -- and that is enough: no report even
    // in strict mode, and both are direct calls to the declared names.
    let src = r"
        void free(void *p);
        void *xmalloc(int n);
        void *f(void *p, int n) { (free)(p); return (xmalloc)(n); }";
    let _strict = super::force_error_on_ast();
    let (prog, _, dump) =
        super::parse_c_program(src).expect("a prototype is evidence enough: no report");
    let callees: Vec<String> = direct_calls_in(&prog, "f")
        .into_iter()
        .flat_map(|(edges, _)| edges)
        .collect();
    assert_eq!(
        callees,
        vec!["free".to_string(), "xmalloc".to_string()],
        "{dump}"
    );
}

// ---------------------------------------------------------------------------------------
// A function pointer in a field of a file-scope object.
//
// `cfg.handler()` is a call THROUGH a pointer `cfg` holds. A direct call to a function
// literally named `cfg.handler` would fail twice: the edge would point at a function that
// does not exist, and the indirect call the program makes would not be in the IR. The
// subtlety is the BASE: storage class says nothing -- the PATH does. `$globals.hook` IS
// the object `hook` and names it; `$globals.cfg.handler` is a location *inside* `cfg`,
// reached by a load, exactly like the local `s.f`.

#[test_log::test]
fn a_call_through_a_field_of_a_global_is_indirect() {
    // `.field` on a file-scope object. The field is loaded out of the global and called
    // through.
    let src = r"
        struct ops { void (*f)(int); };
        struct ops g;
        void fire(int x) { g.f(x); }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "fire").is_empty(),
        "a field is not a name\n{dump}"
    );
    assert!(
        function_named(&prog, "g.f").is_none(),
        "no function is invented for the callee's source text\n{dump}"
    );
    check_loads(&prog, "$globals.g");
    check_loads(&prog, "<t1>.f");
    let callee = local_render(&prog, "fire", "<t2>");
    assert!(
        check_match(&dump, &format!("funcptr-call {callee} ")),
        "the callee is the loaded field\n{dump}"
    );
}

#[test_log::test]
fn a_call_through_a_field_of_a_global_pointer_is_indirect() {
    // `->field` on a file-scope pointer. Same two loads: `->` is a field access here.
    let src = r"
        struct ops { void (*f)(int); };
        struct ops *gp;
        void fire(int x) { gp->f(x); }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "fire").is_empty(),
        "a field is not a name\n{dump}"
    );
    assert!(
        function_named(&prog, "gp->f").is_none(),
        "no function is invented for the callee's source text\n{dump}"
    );
    check_loads(&prog, "$globals.gp");
    check_loads(&prog, "<t1>.f");
    let callee = local_render(&prog, "fire", "<t2>");
    assert!(
        check_match(&dump, &format!("funcptr-call {callee} ")),
        "the callee is the loaded field\n{dump}"
    );
}

#[test_log::test]
fn a_call_through_a_field_of_a_global_array_element_is_indirect() {
    // `array[i].field` -- a global dispatch table. A non-constant index carries no offset
    // segment
    // (see the module header), so the element is the `deref` performed at the array's address
    // and the field is loaded from what that yields.
    let src = r"
        struct ops { void (*f)(int); };
        struct ops ga[4];
        void fire(int i, int x) { ga[i].f(x); }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "fire").is_empty(),
        "an element's field is not a name\n{dump}"
    );
    assert!(
        function_named(&prog, "ga[i].f").is_none(),
        "no function is invented for the callee's source text\n{dump}"
    );
    check_loads(&prog, "$globals.ga");
    check_loads(&prog, "<t1>.deref");
    check_loads(&prog, "<t2>.f");
    let callee = local_render(&prog, "fire", "<t3>");
    assert!(
        check_match(&dump, &format!("funcptr-call {callee} ")),
        "the callee is the loaded field\n{dump}"
    );
}

#[test_log::test]
fn a_call_through_a_field_of_a_local_or_parameter_is_unchanged() {
    // The boundary: the same three shapes with a LOCAL or PARAMETER base lower as indirect
    // calls by the
    // same rule -- the callee is a location, not a name -- never by the base's storage
    // class.
    let src = r"
        struct ops { void (*f)(int); };
        void via_local(int x) { struct ops s; s.f(x); }
        void via_param(struct ops *p, int x) { p->f(x); }
        void via_param_elem(struct ops *a, int i, int x) { a[i].f(x); }";
    let (prog, dump) = program_from_string(src);
    for f in ["via_local", "via_param", "via_param_elem"] {
        assert!(
            direct_calls_in(&prog, f).is_empty(),
            "{f}: still indirect\n{dump}"
        );
    }
    assert_eq!(
        dump.matches("funcptr-call").count(),
        3,
        "one indirect call each\n{dump}"
    );
}

#[test_log::test]
fn a_bare_global_callee_is_still_a_name() {
    // The other side, and why the rule is the PATH: a plain `f(1)` and a call through a
    // global function POINTER are both spelled as a bare identifier, and the frontend
    // cannot tell them apart (`plain` is only declared). Both keep resolving by name, so a
    // taint model naming `hook` still matches.
    let src = r"
        void (*hook)(int);
        void plain(int x);
        void fire(int x) { hook(x); plain(x); }";
    let (prog, dump) = program_from_string(src);
    check_has_direct_call(&prog, "fire", "hook");
    check_has_direct_call(&prog, "fire", "plain");
    assert!(
        check_no_match(&dump, "funcptr-call"),
        "a bare global name is still a direct call\n{dump}"
    );
}

#[test_log::test]
fn a_call_through_a_field_of_a_global_is_not_a_frontend_gap() {
    // Strict mode. An invented function is not a warning, so silence proves nothing; this
    // pins that reading the callee as a location introduces no gap of its own.
    let src = r"
        struct ops { void (*f)(int); };
        struct ops g;
        struct ops *gp;
        struct ops ga[4];
        void fire(int i, int x) { g.f(x); gp->f(x); ga[i].f(x); }";
    let _strict = super::force_error_on_ast();
    super::parse_c_program(src).expect("a call through a global's field is not a frontend gap");
}

#[test_log::test]
fn taint_crosses_a_call_through_a_field_of_a_global() {
    // End to end: the argument reaches `id`'s parameter and comes back,
    // through the binding `g.f = id` -- the dispatch-table shape. With an invented,
    // empty-bodied `g.f` there would be no indirect call to resolve at all, and an
    // empty body returns nothing, so the taint that went in would not come out.
    let src = r"
        int id(int p) { return p; }
        struct Ops { int (*f)(int); };
        struct Ops g;
        int wrap(int a, int b) {
            g.f = id;
            return g.f(b);
        }";
    let (summary, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&summary, 1, "");
}

#[test_log::test]
fn an_argument_of_a_call_through_a_global_field_reaches_the_callee_parameter() {
    // The same crossing from the argument side: `wrap` summarizes @p1 -> `$globals.taken`
    // only if the indirect call resolves to `store_it`. The `check_no_flow_in` makes it a
    // measurement: @p0 is never passed anywhere, so an over-approximation letting any
    // argument reach any callee would show up here.
    let src = r"
        int taken;
        void store_it(int p) { taken = p; }
        struct Ops { void (*f)(int); };
        struct Ops g;
        void wrap(int a, int b) {
            g.f = store_it;
            g.f(b);
        }";
    let (summary, si) = get_summary(program_from_string(src).0).unwrap();
    check_param_into_global_in(&summary, &si, "wrap", 1, ".taken");
    check_no_flow_in(
        &summary,
        &si,
        "wrap",
        0,
        "",
        crate::codegen::GLOBALS_INDEX,
        ".taken",
    );
}

// ---------------------------------------------------------------------------------------
// A GNU statement expression in callee position.
//
// Patchable-call macros expand to `({ ...; (&trampoline_fp); })(...)`: a statement
// expression whose value is a trampoline's address, immediately called. Naming the callee
// by its source text would invent a function per call site (the expansion embeds a unique
// counter) and lose the indirect call. The resolved access path cannot decide this shape
// -- the value node resolves to a bare global path, exactly what a name resolves to -- so
// the construct in callee position tells them apart: `is_statement_expression`.

#[test_log::test]
fn a_static_call_style_expansion_is_an_indirect_call() {
    // The expansion, spelled out: a `static` addressable alias declared for its side effect,
    // an empty statement from the `;;`, and the trampoline's address as the value. Nothing here
    // is a name in callee position, so nothing may be invented from the braces' text.
    let src = r"
        void __real_fn(void);
        void __tramp_fn(void);
        void fire(void) {
            ({ static void *__unique_id_addressable_134 =
                   (void *)&__real_fn;; (&__tramp_fn); })();
        }";
    let (prog, dump) = program_from_string(src);
    assert!(
        direct_calls_in(&prog, "fire").is_empty(),
        "the braces name no function\n{dump}"
    );
    assert!(
        prog.functions
            .functions
            .raw
            .iter()
            .all(|f| !f.name.starts_with("({")),
        "no function is invented for the braces' source text\n{dump}"
    );
    check_loads(&prog, "$globals.__tramp_fn");
    assert!(check_match(&dump, "funcptr-call"), "{dump}");
}

#[test_log::test]
fn a_statement_expression_callee_lowers_the_braces_effects() {
    // The statements before the value are not scenery -- the whole construct exists for them.
    // They run whether or not the call resolves, so the write to `o` inside the braces must be
    // real IR: only it can carry the parameter to the return here.
    let src = r"
        void (*hook)(int);
        int f(int a) { int o = 0; ({ o = a; hook; })(1); return o; }";
    let (s, _si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param(&s, 0, "");
}

#[test_log::test]
fn taint_crosses_a_statement_expression_callee() {
    // End to end: the statement expression yields `g`, `g` is bound to `id`,
    // so the argument reaches `id`'s parameter and comes back. An invented, empty-bodied
    // function named `({ g; })` would return nothing.
    let src = r"
        int id(int p) { return p; }
        int (*g)(int);
        int wrap(int b) {
            g = id;
            return ({ g; })(b);
        }";
    let (summary, si) = get_summary(program_from_string(src).0).unwrap();
    check_returns_param_in(&summary, &si, "wrap", 0, "");
}

#[test_log::test]
fn an_argument_of_a_statement_expression_callee_reaches_the_callee_parameter() {
    // The same crossing from the argument side, with the `check_no_flow_in` control that makes it
    // a measurement: `a` is passed to nothing, so an over-approximation letting any argument reach
    // any callee would show up as a second flow into `$globals.taken`.
    let src = r"
        int taken;
        void store_it(int p) { taken = p; }
        void (*g)(int);
        void wrap(int a, int b) {
            g = store_it;
            ({ g; })(b);
        }";
    let (summary, si) = get_summary(program_from_string(src).0).unwrap();
    check_param_into_global_in(&summary, &si, "wrap", 1, ".taken");
    check_no_flow_in(
        &summary,
        &si,
        "wrap",
        0,
        "",
        crate::codegen::GLOBALS_INDEX,
        ".taken",
    );
}

#[test_log::test]
fn a_static_call_trampoline_carries_no_flow_within_one_unit() {
    // What the analysis can NOT do, stated rather than implied: a real trampoline is
    // patched in by assembly, so no unit assigns the pointer and there is nothing to
    // resolve. The call is still in the IR as an indirect call through
    // `$globals.__tramp_f` -- what a whole-program run needs, and strictly more than an
    // edge to an invented empty function -- but within the unit no taint crosses it.
    let src = r"
        int taken;
        void __tramp_f(int);
        void fire(int x) { ({ (&__tramp_f); })(x); }";
    let (summary, si) = get_summary(program_from_string(src).0).unwrap();
    check_no_flow_in(
        &summary,
        &si,
        "fire",
        0,
        "",
        crate::codegen::GLOBALS_INDEX,
        ".taken",
    );
}

#[test_log::test]
fn a_statement_expression_callee_is_not_a_frontend_gap() {
    // Strict mode. An invented function is not a warning, so silence proves nothing; this
    // pins that reading the callee as a value introduces no gap of its own, including for
    // the `void`-valued braces that yield no callee at all.
    let src = r"
        void (*hook)(int);
        void fire(int x) { ({ hook; })(x); ({ do { } while (0); })(x); }";
    let _strict = super::force_error_on_ast();
    super::parse_c_program(src).expect("a statement-expression callee is not a frontend gap");
}

#[test_log::test]
fn the_other_statement_expression_positions_and_the_call_shapes_are_unchanged() {
    // The boundary, in one fixture. A statement expression in VALUE position and in
    // LVALUE position must not move; a genuine direct call and
    // a cast written with parentheses must not be dragged onto the indirect path by a
    // rule that keys on the callee's construct. Exactly one call here is indirect.
    let src = r"
        typedef unsigned short width_t;
        struct S { int f; };
        void (*hook)(int);
        int plain(int x);
        int f(struct S *a, int x) {
            int v = ({ int t = x; t; });     // value position: unchanged
            ({ int t = 0; &a[1]; })->f = v;  // lvalue position: unchanged
            ({ hook; })(v);                  // callee position: the case under test
            return plain((width_t)(x));      // a direct call and a cast: unchanged
        }";
    let (prog, dump) = program_from_string(src);
    let direct: Vec<String> = direct_calls_in(&prog, "f")
        .into_iter()
        .flat_map(|(names, _)| names)
        .collect();
    assert_eq!(direct, vec!["plain".to_string()], "{dump}");
    assert_eq!(
        dump.matches("funcptr-call").count(),
        1,
        "only the callee-position statement expression is indirect\n{dump}"
    );
    let (s, si) = get_summary(prog).unwrap();
    check_flow_in(&s, &si, "f", 1, "", 0, ".[1].deref.f");
}
