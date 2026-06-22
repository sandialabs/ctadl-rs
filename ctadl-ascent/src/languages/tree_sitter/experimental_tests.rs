use crate::languages::tree_sitter::test_utils::*;
use crate::languages::tree_sitter::testing_block_flow_ascii::*;

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
    assert!(check_match(&dump, "direct-call callback"));
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
fn if_no_scope() {
    let src = r#"
    int if_no_scope(Foobar *fb){
      if(fb->ct ==3)
        return fb->unbraced;
      return x; // this should grab a global
    }
    "#;
    let (_, dump) = program_from_string(src);

    dump_ir(&dump);
    assert!(check_match(&dump, "return @p0.unbraced"));
}

#[test_log::test]
fn empty_param_list() {
    let src = r"
            int empty_param_list() {
                int a = 5;
                int b;
                b = a;
                return b;
            }
        ";
    let (_, dump) = program_from_string(src);
    dump_ir(&dump);

    assert!(check_match(&dump, "assign %b = %a"), "FAIL: dump\n{dump}");
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
    let (_, dump) = program_from_string(src);
    dump_ir(&dump);
    assert!(check_match(&dump, "assign %b = $globals.a"));
    assert!(check_match(&dump, "assign %<t0> = %b"));
    assert!(check_match(&dump, "assign %c = %<t0>"));
}

#[test_log::test]
fn comma_list_declarations() {
    let src = r"
        int comma_sep_decl() {
        int a,b,c,d;
        int x =a, y=b, z=7;
        
        return x+y;        
}";
    let (program, dump) = program_from_string(src);

    dump_ir(&dump);
    check_assign_or_update(&program, "x", ["a"], None);
    check_assign_or_update(&program, "y", ["b"], None);
    assert!(check_match(&dump, "assign %z = <const: \"7\""));

    //assert!(check_match(&dump, "assign %b = @p0"));
    //assert!(check_match(&dump, "what(@p0[byval], @p1[byref])"));
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

// `simple_elif` promoted to tests.rs (structural CFG assertions via check_successors).

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
    dump_ir(&dump);
    assert!(check_match(&dump, "assign %b = @p0"));
    assert!(check_match(&dump, "what(@p0[byval], @p1[byref])"));
    let (summary, _source_info) = get_summary(program_info.program).unwrap();
    //log::info!("SUMMARY: {:?}", summary);
    //[(Function("parameter_what"), AuxParam(1), Path(""), Param(Index(0)), Path(""))]
    assert!(summary_returns_param(&summary, 0, ""));
}

#[test_log::test]
fn pointer_expression() {
    let src = r"
            int parameter_what(int *y) {
                
                int b = *y;
                return b;                
            }            
        ";
    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);
    assert!(check_match(&dump, "assign %b = @p0")); //?
    assert!(check_match(&dump, "what(@p0[byref])"))
}

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
    /*
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    let (summary, source_info) = get_summary(program_info.program).unwrap();
    */
}
#[test_log::test]
fn unbraced_if() {
    let src = r"
    // man I hope y is never 5!
            int unbraced_if(int y, int z) {
                int x = 5;
                if(x == 3)
                    x = z;
                return x;
            }            
        ";
    let (program, dump) = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    let (summary, _source_info) = get_summary(program_info.program).unwrap();
    assert!(summary_returns_param(&summary, 1, ""));
    dump_ir(&dump);
}

#[test_log::test]
fn if_in_while() {
    let src = r"
            int if_in_while(int y, int z) {
                int x = 5;
                while(x<50){
                  x = z;
                  if(y == z)
                    x = y;
                  x = x + z;
                 }                
                return x; //ah it's not that there are two if's, it's that the if isn't followed by a return to overwrite the gotos.
            }            
        ";
    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);
    assert!(check_no_match(&dump, "goto 0"), "contains errant goto 0")
}

#[test_log::test]
fn double_if() {
    let src = r"
            int double_if(int y, int z) {
                int x = 5;
                  if(x == 3)
                    x = z;
                  if(y == z)
                    x = y;                
                //return x; //ah it's not that there are two if's, it's that the if isn't followed by a return to overwrite the gotos.
            }            
        ";
    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);
    assert!(check_no_match(&dump, "goto 0"), "contains errant goto 0")
    /*
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    let (summary, source_info) = get_summary(program_info.program).unwrap();

    assert!(summary_returns_param(&summary, 0, ""));*/
}

#[test_log::test]
fn unbraced_if_while() {
    let src = r"
    // man I hope y is never 5!
            int unbraced_if_while(int y, int z) {
                int x = 5;
                if(x == 3)
                    x = z;
                while(x == 5)
                    x = y;
                return x;
            }
        ";
    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);
    assert!(check_no_match(&dump, "goto 0"), "contains errant goto 0")
}

#[test_log::test]
fn unbraced_while() {
    let src = r"
    // man I hope y is never 5!
            int unbraced_while(int y, int z) {
                int x = 5;
                  if(x == 3)
                    x = z;
                while(x == 5) 
                    x = y;
                return x;
            }            
        ";
    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);
    /*
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    let (summary, source_info) = get_summary(program_info.program).unwrap();

    assert!(summary_returns_param(&summary, 0, ""));*/
}

#[test_log::test]
fn do_while() {
    let src = r"
            int do_while() {
                int b = 2;
                int x = 5;
                do{
                    x = b;
                } while(b = b + x);
                int y = x;
                return y;
            }
        ";
    let (prog, dump) = program_from_string(src);
    dump_ir(&dump);
    check_block_count(&prog, 4);
    check_assign_or_update(&prog, "x", ["b"], Some(1));
    // do-while: body (block_1) -> condition (block_2); the condition loops back into
    // the body (back-edge) and exits to the continuation (block_3).
    assert!(
        janky_goto(&dump, 1, "2"),
        "body should fall into the condition"
    );
    assert!(
        janky_goto(&dump, 2, "3, 1"),
        "condition should exit to continuation and back-edge to body"
    );
    assert!(check_no_match(&dump, "goto 0"), "contains errant goto 0");
}

#[test_log::test]
fn extra_parens() {
    let src = r"
        int extra_parens(Field myParm){
        int z = 55;
        int x,y;
        int y;
            if((x = z)) {
                //empty
            }
            while((((y = z)))) {
                //empty
            }
            return 0;
        }
    ";
    let (program, dump) = program_from_string(src);
    dump_ir(&dump);
    check_assign_or_update(&program, "x", ["z"], None);
    check_assign_or_update(&program, "y", ["z"], None);
}

#[test_log::test]
fn plus_equals() {
    let src = r"
        int update_expression_2(){
            int x = 5;
            int y = 11;
            y = 99;
            y+=10;
            y+=x*2;
            return 0;
        }
    ";
    let (_prog, dump) = program_from_string(src);
    dump_ir(&dump);
}

#[test_log::test]
fn update_expression() {
    let src = r"
        int update_expression(Field myParm){
            int x = 5;
            int y = 11;

            x++;        
            y+=x*2;
            
            --x;
            myParm->x++;
            
            return myParm->x;
        }
    ";
    let (_prog, dump) = program_from_string(src);
    dump_ir(&dump);
    //assert(check_match(&dump,""))
    assert!(check_match(&dump, "@p0 = update (@p0.x :="));
}

#[test_log::test]
fn simple_while() {
    let src = r" 
            int simple_while(Field my_parm, int parB) {
                int b = 2;  // block 0
                int x = 5;
                while(my_parm->x = parB){ //block 1
                    x = b; //block 3
                }
                int y = x; //block 2
                return y;
            }            
        ";
    let (program, dump) = program_from_string(src);
    dump_ir(&dump);
    check_assign_or_update(&program, "x", ["b"], Some(3));
    check_assign_or_update(&program, "y", ["x"], Some(2));
    assert!(janky_goto(&dump, 1, "2, 3"));
    assert!(janky_goto(&dump, 3, "1")); //the condition goes to 3, not the body.*/
}

//this tests unbraced if/else consequents

#[test_log::test]
fn unbraced_if_else() {
    let src = r"
            int unbraced_if_else(int y, int z) {
                int x = 1;
                if(x == 1)
                    x = y;
                else
                x = z;
            }            
        ";
    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);
    // Both the if-consequence (x = y, @p0) and the unbraced else (x = z, @p1) must be
    // present; the unbraced else body used to be silently dropped.
    assert!(
        check_match(&dump, "assign %x = @p0"),
        "missing if-consequence"
    );
    assert!(
        check_match(&dump, "assign %x = @p1"),
        "unbraced else body was dropped"
    );
    assert!(check_no_match(&dump, "goto 0"), "contains errant goto 0");
    // The condition branches to consequence + else only, never directly to the join.
    assert!(
        janky_goto(&dump, 0, "1, 3"),
        "condition must branch to consequence and else, not the join"
    );
    assert!(
        janky_goto(&dump, 1, "2"),
        "consequence joins the continuation"
    );
    assert!(janky_goto(&dump, 3, "2"), "else joins the continuation");
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

    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);
    assert!(check_match(&dump, "%<t4>"));
    assert!(check_no_match(&dump, "%<t5>"));
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
    let (_, dump) = program_from_string(src);
    dump_ir(&dump);
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
    let (_, dump) = program_from_string(src);
    dump_ir(&dump);
    //let summary = get_summary(program);
    //log::info!("SUMMARY {:#?}", summary);
    assert!(check_match(&dump, "@p0 = update (@p0.f2 := @p2)"));

    assert!(check_match(
        &dump,
        "@p0 = update (@p0.f2.nf1.y := @p1.f2.f3.f4)"
    ));

    assert!(check_match(
        &dump,
        "@p0 = update (@p0.f2.nf1.y := @p1.f2.f3.f4)"
    ));

    assert!(check_match(&dump, "assign %<t1> = @p2, @p3"));

    assert!(check_match(&dump, "@p0 = update (@p0.f3 := %<t2>)"));

    assert!(check_match(&dump, "return @p0.f1"));
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
    let dump = program_from_string(src).1;
    dump_ir(&dump);
    assert!(check_match(&dump, "assign %b = %a"));
    assert!(check_match(&dump, "assign %b = <const: "));
    assert!(check_match(&dump, "assign %c = %<t1>"));
    assert!(check_match(&dump, "assign %<t0> = %a, %b"));
    assert!(check_match(&dump, "return <const: "));
    assert!(check_match(&dump, "assign %x = <const: "));
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

    let (_, dump) = program_from_string(src);
    dump_ir(&dump);

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
    let (_, dump) = program_from_string(src);
    dump_ir(&dump);

    assert!(check_match(&dump, "assign %a = <const:"));
    assert!(check_match(&dump, "assign %<t0> = %a, %x"));
    assert!(check_match(&dump, "return %<t0>"));
    assert!(check_match(&dump, "return %<t3>"));
}

// `return_arity` promoted to tests.rs (structural check_return_arity / function_named).

#[test_log::test]
fn params_and_simple_assign_in_example_2() {
    init_test_logging();
    let fp =
        get_full_path("example2.c").expect("Test Sources are expected in .../tests/c/<filename>");
    let program = program_from_file(fp).expect("example2.c Program parsed");
    let dump = program.to_string();
    dump_ir(&dump);

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
    let dump = program_from_string(src).1;
    dump_ir(&dump);
    assert!(check_match(&dump, "assign %a = <const"));
    assert!(check_match(&dump, "assign %b = %a"));
    assert!(check_match(&dump, "assign %<t0> = %a, %b"));
    assert!(
        check_match(&dump, "assign %c = %<t0>"),
        "Expected to see c receive the blend"
    );
}

#[test_log::test]
fn compound_declaration_with_fields() {
    let src = r"
        int passthrough_assigment_with_fields(Donkey *v) {
            int a = b = v->f1 + v->f3;
            int x = v->f4 = v->f5 + b;
        }
        ";
    let dump = program_from_string(src).1;
    dump_ir(&dump);
    assert!(check_match(&dump, "assign %<t0> = @p0.f1, @p0.f3"));
    assert!(check_match(&dump, "assign %<t1> = @p0.f5, $globals.b"));
    assert!(check_match(&dump, "@p0 = update (@p0.f4 := %<t1>)"));
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
    dump_ir(&dump);

    assert!(check_match(&dump, "assign %b = @p1"));
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
    dump_ir(&dump);
    let (summary, _source_info) = get_summary(program_info.program).unwrap();
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

    dump_ir(&dump);

    let (summary, _source_info) = get_summary(program_info.program).unwrap();
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
    dump_ir(&dump);

    let (summary, _source_info) = get_summary(program_info.program).unwrap();
    log::info!("{:?}", summary);

    assert!(check_match(&dump, "direct-call foo"));
}

fn janky_goto(dump: &str, from_block: usize, to_block: &str) -> bool {
    check_match(
        dump,
        format!("goto {}\nend block_{}", to_block, from_block).as_str(),
    )
}
fn janky_return(dump: &str, from_block: usize, ret_val: &str) -> bool {
    check_match(
        dump,
        format!("return {}\nend block_{}", ret_val, from_block).as_str(),
    )
}

#[test_log::test]
fn simplest_if_no_return() {
    let src = r"
            int simplest_if_no_return(int x, int y) {
            //block 0
                if(x){
                //block 1
                    x = x + 21;                    
                }  
                //block 2
                return y;
            }
        ";
    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);
    assert!(janky_goto(&dump, 0, "1, 2"));
    assert!(janky_goto(&dump, 1, "2"));
    assert!(janky_return(&dump, 2, "@p1")); //returns
}

#[test_log::test]
fn simplest_if_with_return() {
    let src = r"
            int simplest_if_with_return(int x, int y) {
            //block 0
                if(x){
                //block 1
                    return x;
                }  
                //block 2
                return y;
            }
        ";
    let (_program, dump) = program_from_string(src);
    dump_ir(&dump);

    /*
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    dump_ir(&dump);
    let (summary, source_info) = get_summary(program_info.program).unwrap();
    assert!(summary_returns_param(&summary, 0, ""));
    assert!(summary_returns_param(&summary, 1, ""));
    */
    assert!(janky_goto(&dump, 0, "1, 2"));
    assert!(janky_return(&dump, 1, "@p0")); //returns
    assert!(janky_return(&dump, 2, "@p1")); //returns
}

#[test_log::test]
fn shadow_block() {
    let src = r"
        int bar(int false_return, int ac_return){
            int x = ac_return;          
            
            if(x == 5){                
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
    dump_ir(&dump);

    let (summary, _source_info) = get_summary(program_info.program).unwrap();
    log::info!("{:?}", summary);
    assert!(summary_returns_param(&summary, 1, ""));
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
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    dump_ir(&dump);

    let (summary, _source_info) = get_summary(program_info.program).unwrap();
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

    dump_ir(&dump);
    assert!(check_match(&dump, "exceptions not implemented"));
}
