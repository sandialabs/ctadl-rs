//use crate::languages::tree_sitter;
use crate::languages::tree_sitter::test_utils::*;

#[test_log::test]
fn simple_function() {
    let src = r"
            void simple() {}
        ";
    let prog = program_from_string(src).0;
    assert!(check_block_count(&prog, 1));
}

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
    let (program, dump) = program_from_string(src);

    log::info!("{}", dump);
    assert!(check_assign(&program, "v_if", ["x"], Some(1)));
    assert!(check_assign(&program, "v_else", ["x"], Some(3)));
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
    let (prog, _dump) = program_from_string(src);
    assert!(check_block_count(&prog, 1));
    assert!(check_assign(&prog, "b", ["a"], None));
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
    assert!(check_block_count(&prog, 1));
    assert!(check_assign(&prog, "<t0>", ["a", "b"], None));
    assert!(check_assign(&prog, "c", ["<t0>"], None));
}

#[test_log::test]
fn empty_param_list() {
    let src = r"
            int complex_expressions_1() {
                int a = 5;
                int b;
                b = a;
                return b;
            }
        ";
    let dump = program_from_string(src).1;
    assert!(check_match(&dump, "assign %b = %a"));
}

#[test_log::test]
fn declare_assign() {
    let src = r"
            int complex_expressions_1() {
                int b = a; //capture assignment in a declaration
                int c = b + a; // complex assignment to declare
                return b;
            }
        ";
    let dump = program_from_string(src).1;
    assert!(check_match(&dump, "assign %b = $globals.a"));
    assert!(check_match(&dump, "assign %<t0> = %b"));
    assert!(check_match(&dump, "assign %c = %<t0>"));
}

#[test_log::test]
fn parameter_lists_janky() {
    let src = r"
             int parameter_what(int x, int *y) {
                
                int b = x;
                return b;                
            }            
        ";
    let dump = program_from_string(src).1;
    assert!(check_match(&dump, "assign %b = @p0"));
    assert!(check_match(&dump, "what(@p0[byval], @p1[byref])"));
}

/*

#[test_log::test]
fn parameter_lists_query() {
    let src = r"
            int parameter_what(int x, int *y) {
                int b = x;
                return b;
            }            
        ";
    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    log::info!("{}", dump);
    assert!(check_match(&dump, "assign %b = @p0"));
    assert!(check_match(&dump, "what(@p0[byval], @p1[byref])"));
    let (summary, source_info) = get_summary(program_info).unwrap();
    //    log::info!("SUMMARY: {:?}", summary);
    //[(Function("parameter_what"), AuxParam(1), Path(""), Param(Index(0)), Path(""))]
    assert!(summary_count(&summary, 1));
    assert!(summary_search(&summary, 0, "", -1, ""));
    assert!(summary_returns_param(
        &summary,
        &source_info,
        "parameter_what",
        0
    ));
}

#[test_log::test]
fn pointer_expression() {
    let src = r"
            int parameter_what(int *y) {
                
                int b = *y;
                return b;                
            }            
        ";
    let (_, dump) = program_from_string(src);
    log::info!("{}", dump);
    assert!(check_match(&dump, "assign %b = @p0")); //?
    assert!(check_match(&dump, "what(@p0[byref])"))
}

#[test_log::test]
fn ascending_temps_per_function() {
    let src = r"
        int counter_resets() {
            int a = x + y +z;
        }
        int a(){
        
        int z = n + p + r + q;
        int v = a + b;
        int n = m + x;
        
        return 0;
        }   
        
        ";

    let (_, dump) = program_from_string(src);
    log::info!("{}", dump);
    assert!(check_match(&dump, "%<t4>"));
    assert!(!check_match(&dump, "%<t5>"));
}

#[test_log::test]
#[ignore = "Aspirational  3[f] is valid C"]
fn brackets_commutative() {
    let src = r"
            int field_access(Donkey v,  Burro* b, int x, int y){
                int x = 3[f];
            }   
        }
        ";
    let (_program, dump) = program_from_string(src);
    log::info!("{}", dump);
    //let summary = get_summary(program);
    //log::info!("SUMMARY {:#?}", summary);
    assert!(check_match(
        &dump,
        "TODO: we need to check whether the index/lhs are swapped"
    ));
}

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

#[test_log::test]
fn brackets_simple() {
    let src = r#"
            int brackets_simple(Donkey v,  Burro* b, int x, int y){
                int f = 1;
                x = 5;
                x = f[3];
                f[4] = x;
                f.y->yah[n] = 5;
                f->p[4] = m[5] + v.n[4];                            
                return 1;
            }   
    
        "#;
    let (_program, dump) = program_from_string(src);
    log::info!("{}", dump);
    //let summary = get_summary(program);
    //log::info!("SUMMARY {:#?}", summary);
    assert!(check_match(&dump, "%f.[3]"));
    assert!(check_match(&dump, "%f.[4]"));
}

#[test_log::test]
fn field_access_values() {
    let src = r"
            int field_access(Donkey v,  Burro* b, int x, int y){
                
               v.f2 = x;
               v.f2.nf1.y = b->f2.f3->f4; //access b, with path f2,f3,f4
               v.f5 = b->fa + b->fb;
               v.f3 = x + y + z;
               v.f1 = b.xyz;
               return v.f1;

            }   
        ";
    let (_program, dump) = program_from_string(src);
    assert!(janky_expected(&dump, "@p0 = update (@p0.f2 := @p2)"));

    assert!(check_match(
        &dump,
        "@p0 = update (@p0.f2.nf1.y := @p1.f2.f3.f4)"
    ));

    assert!(check_match(
        &dump,
        "@p0 = update (@p0.f2.nf1.y := @p1.f2.f3.f4)"
    ));

    assert!(janky_expected(&dump, "assign %<t1> = @p2, @p3"));
    assert!(janky_expected(&dump, "@p0 = update (@p0.f3 := %<t2>)"));
    assert!(janky_expected(&dump, "return @p0.f1"));
}

#[test_log::test]
fn literals_in_expressions() {
    let src = r"
            int literals_1() {
                int a;
                int b = a;
                b = 5;
                int c = a + b + 17;
                return (14); // what to do this with this?
            }
            int literals_2() {
                int x = 17;
                return (x + 25);
            }
        ";
    let (_program, dump) = program_from_string(src);
    assert!(janky_expected(&dump, "assign %b = %a"));
    assert!(janky_expected(&dump, "assign %b = <const: "));
    assert!(janky_expected(&dump, "assign %c = %<t1>"));
    assert!(janky_expected(&dump, "assign %<t0> = %a, %b"));
    assert!(janky_expected(&dump, "return <const: "));
    assert!(janky_expected(&dump, "assign %x = <const: "));
}

#[test_log::test]
fn complex_expressions() {
    // let _ = env_logger::builder().is_test(true).try_init();
    let src = r"
            int complex_expressions_1(int p) {
                int a = 1;
                int b = a;
                int c = 3;
                int d = 4;
                int e = 5;
                b = a + b + c + (d + e); 
                return b;
            }
        ";

    let (_program, dump) = program_from_string(src);
    log::info!("{}", dump);

    assert!(check_match(&dump, "assign %<t0> = %a, %b"));
    assert!(check_match(&dump, "assign %<t1> = %<t0>, %c"));
    assert!(check_match(&dump, "assign %b = %<t3>"));
    assert!(check_match(&dump, "assign %c = <const:"));
}

#[test_log::test]
fn compound_return() {
    let src = r"
           int compound_return_1(){
             int a = 1;
             int x = 9;
             return (a+x);
            }

            //technically you always had to implement temporaries.
           int compound_return_long(){
            int a;
            int b;            
            int d;
            int e;
            return a + b + 55 + d + e;
            }
        ";
    let (_program, dump) = program_from_string(src);
    assert!(janky_expected(&dump, "assign %a = <const:"));
    assert!(janky_expected(&dump, "assign %<t0> = %a, %x"));
    assert!(janky_expected(&dump, "return %<t0>"));
    assert!(janky_expected(&dump, "return %<t3>"));
}

#[test_log::test]
fn return_arity() {
    let src = r"
          // TREE-SITTER DOESN'T SUPPORT implicit int return
          //  implicit_int(){return 1;}
            int explicit(){return 0;}
            void none(){return;}
            void really_void(void){return;}
        ";
    let (_, dump) = program_from_string(src);

    //assert!(check_match(&dump, "define implicit_int() -> 1"));
    assert!(check_match(&dump, "define explicit() -> 1"));
    assert!(check_match(&dump, "define none() -> 0"));
    assert!(check_match(&dump, "define really_void() -> 0"));
}

#[test_log::test]
fn params_and_simple_assign_in_example_2() {
    init();
    let fp =
        get_full_path("example2.c").expect("Test Sources are expected in .../tests/c/<filename>");
    let program = program_from_file(fp).expect("example2.c failed to parse");
    let dump = program.to_string();
    log::info!("dump: {dump}");

    assert!(check_match(&dump, "return %a"), "has return a");
    assert!(
        check_match(&dump, "assign %a = @p1"),
        "has the simplest assign, a=b"
    );
    assert!(
        check_match(&dump, "assign %a = $globals.d"),
        "has 2nd simple a=d"
    );
}

#[test_log::test]
fn passthrough_assignment() {
    let src = r"
        int passthrough_assigment() {
            int a;
            int b = a = 5;            
            int c = a + b;
            return c;
        }
        ";
    let (_, dump) = program_from_string(src);
    assert!(janky_expected(&dump, "assign %a = <const"));
    assert!(janky_expected(&dump, "assign %b = %a"));
    assert!(janky_expected(&dump, "assign %<t0> = %a, %b"));
    assert!(
        check_match(&dump, "assign %c = %<t0>"),
        "Expected to see c receive the blend"
    );
}

#[test_log::test]
fn pointer_decl() {
    let src = r"
        int pointer_decl(Foobar *fb){
            Foobar *r = fb;
            return r;
        }   
        ";
    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    log::info!("{}", dump);
    //hmm how @njsalim: how do I test this assignment?
    // assert!(check_assign(&prog, "r", ["@p0"])); <-- fails
    assert!(check_match(&dump, "assign %r = @p0"));
    let (summary, source_info) = get_summary(program_info).unwrap();
    assert!(summary_returns_param(
        &summary,
        &source_info,
        "pointer_decl",
        0
    ));
}

#[test_log::test]

fn ignore_scoped_condition() {
    let src = r"
        int scoped_condition(int a, int b){
            //allegedly legal in C23, we walk past the ERROR from treesitter
            if (int x = 7; x > 7){
                fb->in_consequence =x;            
              return a;            
            }   
        return b;
        ";
    let program = program_from_string_no_check(src);
    let dump = program.to_string();
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    log::info!("{}", dump);
    let (summary, source_info) = get_summary(program_info).unwrap();
    assert!(summary_returns_param(
        &summary,
        &source_info,
        "scoped_condition",
        1
    ));
}

#[test_log::test]
fn compound_declaration_with_fields() {
    let src = r"
        int passthrough_assigment_with_fields(Donkey *v) {
            int a = b = v->f1 + v->f3;
            int x = v->f4 = v->f5 + b;
        }
        ";
    let (_, dump) = program_from_string(src);
    assert!(janky_expected(&dump, "assign %<t0> = @p0.f1, @p0.f3"));
    assert!(janky_expected(&dump, "assign %<t1> = @p0.f5, $globals.b"));
    assert!(janky_expected(&dump, "@p0 = update (@p0.f4 := %<t1>)"));
}

#[test_log::test]
fn param_by_reference() {
    let src = r"
        int param_by_reference(Rando x, Rando *y) {

            Rando a = x;
            Rando b = *y;
            
            return a.field + b.field;
        }
        ";
    let (_, dump) = program_from_string(src);
    assert!(janky_expected(&dump, "assign %b = @p1"));
}

#[test_log::test]
fn simplest_calls() {
    let src = r"

        int tgt(Rando x){
            return x.f1;
    }
        int top(Rando y){
            int v = tgt(y);
            return v;
    }
";

    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);
    assert!(check_match(&dump, "direct-call tgt"));
}
#[test_log::test]
fn params_into_calls() {
    let src = r"
        int foo(Rando x){
            return x;
        }
        int foo2(int z){
            return  z *z;
        }
        int bar(int y){
            int x;
            foo(x = y);
            foo(y);
            foo(y + y);
            return y;
        }
        ";
    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);
    assert!(
        check_match(&dump, "assign %x = @p0"),
        "picked up assign in parameter list"
    );
    assert!(
        check_match(&dump, "%<t0> = direct-call foo(%x)"),
        "picked up assign in parameter list"
    );
    assert!(check_match(&dump, "direct-call foo(@p0)"));
    assert!(check_match(&dump, "assign %<t3> = @p0, @p0"));
    assert!(check_match(&dump, "%<t2> = direct-call foo(%<t3>)"));
    //TOOD_JDB: do summary queries, not these janks
    //assert!(check_match(&dump, "TODO: write param queries");
}

#[test_log::test]
fn call_not_assign() {
    let src = r"
        int foo(Rando x){
            return x;
        }
        int baz(Rando m){
        return m+ m;
        }
        int bar(Rando y){
            foo(baz(y)); 
            return y;
        }
        ";
    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);

    assert!(check_match(&dump, "direct-call foo"));
}

#[test_log::test]
#[ignore = "aspirational"]
fn shadow_block() {
    let src = r"
        int bar(int false_return, int ac_return){
            int x = ac_return;          
            
            if(x == 6){                
                int x = false_return;                
            }
            return x;
        }
        ";
    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    log::info!("{}", dump);

    let (summary, source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);
    assert!(summary_returns_param(&summary, &source_info, "bar", 1));
}

#[test_log::test]
fn scopes_arent_blocks() {
    let src = r"
        int bar(){
        {
            int x;
        }
        return 0;
        }";
    let (program, _) = program_from_string(src);
    assert!(check_block_count(&program, 1));
}

#[test_log::test]
#[ignore = "aspirational_indirect"]
fn indirect_call_1() {
    let src = r#"
        #include <stdio.h>

        // Two target functions with the same signature
        int add(int a, int b) { return a + b; }
        int sub(int a, int b) { return a - b; }

        int doit(int a) {
            // 1. Declare a function pointer
            int (*op_func)(int, int);

            // 2. Assign the pointer (could be based on user input, making it tainted!)
            op_func = add; 

            // 3. The Indirect Call
            int result = op_func(a, 3);
            
            // 4. Legacy syntax
             result = (*op_func)(result, b);
            
            printf("Result: %d\n", result);
            return 0;
        }"#;

    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);

    assert!(check_match(&dump, "indirect-call"));
    //assert!(summary_returns_param(&summary, &source_info, "bar", 0));
}

#[test_log::test]
#[ignore = "issue #40"]
fn block_without_return() {
    let src = r"
        void bar(){
            int x = 5;            
        }
        ";
    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);

    assert!(check_match(&dump, "assign %x"));
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
    log::info!("{}", dump);
    assert!(check_match(&dump, "exceptions not implemented"));
}

 */
