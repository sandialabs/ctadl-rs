use crate::languages::tree_sitter_c::test_utils::*;
use crate::languages::tree_sitter_c::testing_block_flow_ascii::*;

use ctadl_ir::ProgramInfo;

#[test_log::test]
#[should_panic]
fn test_janky_assert() {
    assert!(check_match("return a", "return asdf%a"), "has return a");
}
#[test_log::test]
#[ignore = "aspirational"]
fn type_def_func_params() {
    let src = r#"
        typedef void (*HelloCallback)(char*);

        HelloCallback execute_callback(HelloCallback callback, char* name) {
            callback(name);
            
            // Returning the pointer is now as simple as returning an int
            return callback;
        }
            "#;
    let (_, dump) = program_from_string(src);
    dump_ir(&dump);
}

#[test_log::test]
fn func_ptr_simplest() {
    let src = r#"
                    #include <stdio.h>

                    int main() {
                        int (*X)(const char *, ...) = printf;
                        X("Hello, %s!\n", "world");
                        return 0;
                    }
                    "#;

    let (_, dump) = program_from_string(src);
    dump_ir(&dump);
    assert!(check_match(&dump, "funcptr-call"));
}

#[test_log::test]
fn func_params() {
    let src = r#"
    void function_param(void (*callback)(char*)) {
        return callback("I once had a dog name foo, he was a great boy");
    }

"#;

    let (_, dump) = program_from_string(src);
    dump_ir(&dump);
    // A call through a function-pointer PARAMETER is an INDIRECT call: the callee is
    // unknown at the call site, arriving via `callback` (@p0). (Previously asserted a
    // direct call — the buggy behavior from when collect_params dropped function-pointer
    // parameters, leaving `callback` unresolved.)
    assert!(check_match(&dump, "@p0 <indirect-call"));
}

#[test_log::test]
fn indirect_call1() {
    let src = r#"
    #include <stdio.h>

// 1. The callback now expects a string
void say_hello(char* name) {
    printf("Hello, %s! (from inside the callback)\n", name);
}

// 2. The wrapper now takes the function pointer AND the data to pass to it
void execute_callback(void (*callback)(char*), char* name_to_pass) {
    printf("Wrapper: Preparing to call the function...\n");

    // 3. Assigning the callback to a local variable
    // Note how the (char*) signature must match exactly
    void (*local_ptr)(char*) = callback;

    // 4. Execute the local pointer with the argument
    local_ptr(name_to_pass);
}

int main() {
    execute_callback(say_hello, "Gemini");
    return 0;
}
 
"#;

    let (_, dump) = program_from_string(src);
    dump_ir(&dump);
}

#[test_log::test]
fn shadowing() {
    let src = r#"
        int x;
        int z;
        void param_shadow_global(int x){
            x = 5;
        }

        void local_shadow_global(int y){
            z = 3;
            z->nn = 3;
            x = z.nn;
            x = 4; 
            y = x;
            int z = 10;
            int x; //assignments have double_declarators, declarations don't
            y = x;
            x = 7;
            
        }

        void local_shadow_param(int y){
            int y = 4;
}
        "#;
    let (_, dump) = program_from_string(src);
    dump_ir(&dump);
}

#[test_log::test]
fn simple_for() {
    let src = r"
             int simple_for(int x, int *y) {
                int x;
                for (int a = 0, x = 9; a < 10; a++,x++,y++){
                int b = a;    
                }
                return 0;
            }            
        ";
    let (_, dump) = program_from_string(src);

    dump_ir(&dump);
    //assert!(check_match(&dump, "assign %b = @p0"));
    //assert!(check_match(&dump, "what(@p0[byval], @p1[byref])"));
}
//todo:  comma operator
//

//this tests whether we die on this unchilded expression_statement
// while(y==5);  <--- just a semi colon. sounds like a good way to
// spin the processor ;)
#[test_log::test]
fn no_child_while() {
    let src = r"
    // man I hope y is never 5!
            int no_child_while(int y) {
                int x = 5;
                while(x == 5);                    
                return x;
            }            
        ";
    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);
}

#[test_log::test]
#[ignore = "Aspirational  3[f] is valid C"]
fn brackets_commutative() {
    let src = r"
            int field_access(Donkey v,  Burro* b, int x, int y){
                int x = 3[f];
        }
        ";
    let (_, dump) = program_from_string(src);
    dump_ir(&dump);
    //let summary = get_summary(program);
    //log::info!("SUMMARY {:#?}", summary);
    assert!(check_match(
        &dump,
        "TODO: we need to check whether the index/lhs are swapped"
    ));
}

#[test_log::test]
fn params_and_simple_assign_in_example_2() {
    init_test_logging();
    let src = r#"
        int d;
        int foo(int c, int b) {
          int a;
          a = b;
          a = d;
          return a;
        }
        "#;
    let (program, dump) = program_from_string(src);
    dump_ir(&dump);

    // Locals render by index (`%L{idx}`); resolve `a`'s index by name so the assertions stay
    // readable rather than hard-coding `%L0`.
    let a = local_render(&program, "foo", "a");
    assert!(check_match(&dump, &format!("return {a}")), "has return a");
    assert!(
        check_match(&dump, &format!("assign {a} = @p1")),
        "has the simplest assign, a=b"
    );
    assert!(
        // Reading the global `d` lowers to a load of `$globals.d` (into a temp that flows to a).
        check_match(&dump, "load $globals.d"),
        "has 2nd simple a=d"
    );
}


// `simplest_calls` promoted to tests.rs as `call_arg_flows_through_return` (Category B: the call's
// argument flows through the callee and back to the return -- subsumes the `direct-call tgt` check).
// `params_into_calls` and `call_not_assign` promoted to tests.rs (structural call-site assertions
// via direct_calls_in / check_direct_call / check_has_direct_call).

// Dump-based CFG-edge matcher. Its callers were promoted to tests.rs (now using check_successors),
// leaving it currently unused -- but kept here as scratch scaffolding for the partner's exploration
// lane, alongside janky_return.
#[allow(dead_code)]
fn janky_goto(dump: &str, from_block: usize, to_block: &str) -> bool {
    check_match(
        dump,
        format!("goto {}\nend block_{}", to_block, from_block).as_str(),
    )
}

// Companion to `janky_goto` for return terminators. Currently unused (its callers were promoted to
// tests.rs) but kept here as scratch scaffolding for the partner's exploration lane.
#[allow(dead_code)]
fn janky_return(dump: &str, from_block: usize, ret_val: &str) -> bool {
    check_match(
        dump,
        format!("return {}\nend block_{}", ret_val, from_block).as_str(),
    )
}

#[test_log::test]
fn indirect_call_1() {
    let src = r#"
        #include <stdio.h>

        // Two target functions with the same signature
        int add(int a, int b) { return a + b; }
        int sub(int a, int b) { return a - b; }

        int doit(int a) {
            // 1. Declare a function pointer and point it at a target
            //    (could be based on user input, making it tainted!)
            int (*op_func)(int, int) = add;

            // 2. The Indirect Call
            int result = op_func(a, 3);

            // 3. Legacy syntax
            result = (*op_func)(result, a);
            
            printf("Result: %d\n", result);
            return 0;
        }"#;

    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    dump_ir(&dump);

    let (summary, _source_info) = get_summary(program_info.program).unwrap();
    log::info!("{:?}", summary);

    assert!(check_match(&dump, "indirect-call"));
    //assert!(summary_returns_param(&summary, 0, ""));
}

#[test_log::test]
fn block_without_return() {
    let src = r"
        void bar(){
            int x = 5;            
        }
        ";
    let (program, dump) = program_from_string(src);
    // Resolve `x`'s interned index by name before `program` is consumed below, so the assertion
    // reads in terms of the source name rather than a hard-coded `%L0`.
    let x = local_render(&program, "bar", "x");
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    dump_ir(&dump);

    let (summary, _source_info) = get_summary(program_info.program).unwrap();
    log::info!("{:?}", summary);

    assert!(check_match(&dump, &format!("assign {x}")));
}

//msvc has an extension for try/catch, tree-sitter
#[test_log::test]
#[ignore = "aspirational MSVC extension try catch"]
fn try_catch() {
    let src = r#"
    #include <stdio.h>
       
        __try {
        // Guarded code
        int* ptr = NULL;
        *ptr = 42; // This would normally crash the program (Access Violation)
        } 
        __except(EXCEPTION_EXECUTE_HANDLER) {
            // This code runs if an exception occurs above
            printf("Caught a memory fault!\n");
        }
    }
"#;
    let (_program, dump) = program_from_string(src);

    dump_ir(&dump);
    assert!(check_match(&dump, "exceptions not implemented"));
}
