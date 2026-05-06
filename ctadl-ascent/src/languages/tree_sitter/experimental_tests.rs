use crate::languages::tree_sitter::tests::get_full_path;
use crate::languages::tree_sitter::tests::get_summary;
use crate::languages::tree_sitter::tests::program_from_file;
use crate::languages::tree_sitter::tests::program_from_string;
use crate::languages::tree_sitter::tests::summary_returns_param;

use ctadl_ir::ProgramInfo;

fn janky_expected(dump: &str, needle: &str) -> bool {
    let res = dump.contains(needle);
    if !res {
        log::error!("TEST FAIL: expected {needle}");
        log::error!("\t{dump}");
    }
    res
}
fn init() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug) // This forces it to Debug
        .is_test(true)
        .try_init();
}

#[test_log::test]
#[should_panic]
fn test_janky_assert() {
    assert!(janky_expected("return a", "return asdf%a"), "has return a");
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
    let dump = program_from_string(src).to_string();
    log::info!("{}", dump);
}

#[test_log::test]
#[ignore = "Aspriational, needs unbraced consequences. "]
fn if_no_scope() {
    let src = r#"

    int if_no_scope(Foobar *fb){
      if(fb->ct ==3)
        return fb->unbraced
      return x; // this should grab a global
    }

    "#;
    let dump = program_from_string(src).to_string();

    log::info!("{}", dump);
    assert!(janky_expected("return a", "return asdf%a"), "NOT COMPLETE");
}

#[test_log::test]
#[ignore = "Aspriational needs proper else, elseif handling"]
fn scopes_and_blocks() {
    let src = r#"
        void main(){
            //before a compound statement
            {
                int x = 5;
            }

            foo(5);
            foo(10);
            int y = 7;
            int z = foo(10);
            if(z == 10){
                int v = 33;
                return;
            }
            if(y == 7){
                printf("Here i am!\n");
            }
            else if (v == 7){
                callme(blondie);
            } else{
                printf("Final countdown!");
            }

            for(int y=0, z = foo();y<5;y++){
                printf("Do you see that?");
            }

            printf("more statements");
            {
                exogenou();
            }

            }
       }"#;
    let dump = program_from_string(src).to_string();

    log::info!("{}", dump);
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
    let dump = program_from_string(src).to_string();

    assert!(
        janky_expected(&dump, "assign %b = %a"),
        "FAIL: dump\n{dump}"
    );
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
    let dump = program_from_string(src).to_string();
    log::info!("{}", dump);
    assert!(janky_expected(&dump, "assign %b = $globals.a"));
    assert!(janky_expected(&dump, "assign %<t0> = %b"));
    assert!(janky_expected(&dump, "assign %c = %<t0>"));
}

#[test_log::test]
fn parameter_lists_janky() {
    let src = r"
             int parameter_what(int x, int *y) {
                
                int b = x;
                return b;                
            }            
        ";
    let dump = program_from_string(src).to_string();

    log::info!("{}", dump);
    assert!(janky_expected(&dump, "assign %b = @p0"));
    assert!(janky_expected(&dump, "what(@p0[byval], @p1[byref])"));
}

/*
int simple_else(int x, int *y, int z) {
                if(x){
                    y->v_if = x;
                } else if(z){
                    y->v_else_if = z;
                } else
                    y->v_else = 5;
                }
                return 0;
            }
             */

#[test_log::test]
fn simple_else() {
    let src = r"
             int simple_else(int x, int *y, int z) {
                if(x){
                    y->v_if = x;
                } else {
                    y->v_else = 5;
                }
                return 0;               
            }            
        ";
    let dump = program_from_string(src).to_string();

    log::info!("{}", dump);
    assert!(janky_expected(
        &dump,
        "begin block_1:\n@p1 = update (@p1.v_if := @p0"
    ));
    assert!(janky_expected(
        &dump,
        "begin block_3:\n@p1 = update (@p1.v_else := <const"
    ));
    //todo add block
}

#[test_log::test]
fn parameter_lists_query() {
    let src = r"
            int parameter_what(int x, int *y) {
                int b = x;
                return b;                
            }            
        ";
    let program = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    let dump = program_info.program.to_string();
    log::info!("{}", dump);
    assert!(janky_expected(&dump, "assign %b = @p0"));
    assert!(janky_expected(&dump, "what(@p0[byval], @p1[byref])"));
    let (summary, source_info) = get_summary(program_info).unwrap();
    //log::info!("SUMMARY: {:?}", summary);
    //[(Function("parameter_what"), AuxParam(1), Path(""), Param(Index(0)), Path(""))]
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
    let dump = program_from_string(src).to_string();
    log::info!("{}", dump);
    assert!(janky_expected(&dump, "assign %b = @p0")); //?
    assert!(janky_expected(&dump, "what(@p0[byref])"))
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

    let dump = program_from_string(src).to_string();
    log::info!("{}", dump);
    assert!(janky_expected(&dump, "%<t4>"));
    assert!(!janky_expected(&dump, "%<t5>"));
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
    let dump = program_from_string(src).to_string();
    log::info!("{}", dump);
    //let summary = get_summary(program);
    //log::info!("SUMMARY {:#?}", summary);
    assert!(janky_expected(
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
    let dump = program_from_string(src).to_string();
    log::info!("{}", dump);
    //let summary = get_summary(program);
    //log::info!("SUMMARY {:#?}", summary);
    assert!(janky_expected(&dump, "%f.[3]"));
    assert!(janky_expected(&dump, "%f.[4]"));
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
    let dump = program_from_string(src).to_string();
    log::info!("{}", dump);
    //let summary = get_summary(program);
    //log::info!("SUMMARY {:#?}", summary);
    assert!(janky_expected(&dump, "@p0 = update (@p0.f2 := @p2)"));

    assert!(janky_expected(
        &dump,
        "@p0 = update (@p0.f2.nf1.y := @p1.f2.f3.f4)"
    ));

    assert!(janky_expected(
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
    let dump = program_from_string(src).to_string();
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

    let dump = program_from_string(src).to_string();
    log::info!("{}", dump);

    assert!(janky_expected(&dump, "assign %<t0> = %a, %b"));
    assert!(janky_expected(&dump, "assign %<t1> = %<t0>, %c"));
    assert!(janky_expected(&dump, "assign %b = %<t3>"));
    assert!(janky_expected(&dump, "assign %c = <const:"));
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
    let dump = program_from_string(src).to_string();

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
    let dump = program_from_string(src).to_string();

    //assert!(janky_expected(&dump, "define implicit_int() -> 1"));
    assert!(janky_expected(&dump, "define explicit() -> 1"));
    assert!(janky_expected(&dump, "define none() -> 0"));
    assert!(janky_expected(&dump, "define really_void() -> 0"));
}

#[test_log::test]
fn params_and_simple_assign_in_example_2() {
    init();
    let fp =
        get_full_path("example2.c").expect("Test Sources are expected in .../tests/c/<filename>");
    let program = program_from_file(fp).expect("example2.c Program parsed");
    let dump = program.to_string();
    log::info!("dump: {dump}");

    assert!(janky_expected(&dump, "return %a"), "has return a");
    assert!(
        janky_expected(&dump, "assign %a = @p1"),
        "has the simplest assign, a=b"
    );
    assert!(
        janky_expected(&dump, "assign %a = $globals.d"),
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
    let dump = program_from_string(src).to_string();
    assert!(janky_expected(&dump, "assign %a = <const"));
    assert!(janky_expected(&dump, "assign %b = %a"));
    assert!(janky_expected(&dump, "assign %<t0> = %a, %b"));
    assert!(
        janky_expected(&dump, "assign %c = %<t0>"),
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
    let dump = program_from_string(src).to_string();
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
    let dump = program_from_string(src).to_string();

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

    let program = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    let dump = program_info.program.to_string();
    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);
    assert!(janky_expected(&dump, "direct-call tgt"));
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
    let program = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    let dump = program_info.program.to_string();
    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);
    assert!(
        janky_expected(&dump, "assign %x = @p0"),
        "picked up assign in parameter list"
    );
    assert!(
        janky_expected(&dump, "%<t0> = direct-call foo(%x)"),
        "picked up assign in parameter list"
    );
    assert!(janky_expected(&dump, "direct-call foo(@p0)"));
    assert!(janky_expected(&dump, "assign %<t3> = @p0, @p0"));
    assert!(janky_expected(&dump, "%<t2> = direct-call foo(%<t3>)"));
    //TOOD_JDB: do summary queries, not these janks
    //assert!(janky_expected(&dump, "TODO: write param queries");
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
    let program = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    let dump = program_info.program.to_string();
    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);

    assert!(janky_expected(&dump, "direct-call foo"));
}

fn janky_goto(dump: &str, from_block: usize, to_block: &str) -> bool {
    janky_expected(
        dump,
        format!("goto {}\nend block_{}", to_block, from_block).as_str(),
    )
}
fn janky_return(dump: &str, from_block: usize, ret_val: &str) -> bool {
    janky_expected(
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
    let program = program_from_string(src); //.expect("always");
    let dump = program.to_string();
    assert!(janky_goto(&dump.as_str(), 0, "1, 2"));
    assert!(janky_goto(&dump.as_str(), 1, "2"));
    assert!(janky_return(&dump.as_str(), 2, "@p1")); //returns
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
    let program = program_from_string(src); //.expect("always");
    let dump = program.to_string();
    /*
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };

    log::info!("{}", dump);
    let (summary, source_info) = get_summary(program_info).unwrap();
    assert!(summary_returns_param(
        &summary,
        &source_info,
        "simplest_if",
        0
    ));
    assert!(summary_returns_param(
        &summary,
        &source_info,
        "simplest_if",
        1
    ));
    */
    assert!(janky_goto(&dump.as_str(), 0, "1, 2"));
    assert!(janky_return(&dump.as_str(), 1, "@p0")); //returns
    assert!(janky_return(&dump.as_str(), 2, "@p1")); //returns
}

#[test_log::test]
#[ignore = "aspirational"]
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
    let program = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    let dump = program_info.program.to_string();
    log::info!("{}", dump);

    let (summary, source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);
    assert!(summary_returns_param(&summary, &source_info, "bar", 1));
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

    let program = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    let dump = program_info.program.to_string();
    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);

    assert!(janky_expected(&dump, "indirect-call"));
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
    let program = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    let dump = program_info.program.to_string();
    log::info!("{}", dump);

    let (summary, _source_info) = get_summary(program_info).unwrap();
    log::info!("{:?}", summary);

    assert!(janky_expected(&dump, "assign %x"));
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
    let program = program_from_string(src);
    let program_info = ProgramInfo {
        program,
        ..Default::default()
    };
    let dump = program_info.program.to_string();
    log::info!("{}", dump);
    assert!(janky_expected(&dump, "exceptions not implemented"));
}
