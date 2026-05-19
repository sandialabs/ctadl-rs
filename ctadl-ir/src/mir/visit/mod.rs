/*!
CTADL IR visitor.

The visitor in this module is modeled after the design in the Rust MIR. The visitors are generated
by a macro; there's an immutable and mutable version. The mutable version allows you to generate
AST code whereas the immutable version is just for walking and reading from the AST. Every
`visit_xxx` method has a `super_xxx` method that simple visits every sub-AST. The cases in the
`super_xxx` methods are written deliberately to use exhaustive pattern matching, rather than
structure field access, to maximize the opportunity for the compiler to throw errors if the AST
changes.
*/
use super::*;
use FieldAccess;

macro_rules! basic_blocks {
    ($blocks:ident, mut, true) => {
        $blocks.blocks_mut()
    };
    ($blocks:ident, mut, false) => {
        $blocks.blocks_mut_preserves_cfg()
    };
    ($blocks:ident,) => {
        $blocks
    };
}

macro_rules! basic_blocks_iter {
    ($blocks:ident, mut, $invalidate:tt) => {
        basic_blocks!($blocks, mut, $invalidate).iter_enumerated_mut()
    };
    ($blocks:ident,) => {
        basic_blocks!($blocks,).iter_enumerated()
    };
}

macro_rules! index_vec_iter {
    ($statements:ident, mut, $invalidate:tt) => {
        $statements.iter_enumerated_mut()
    };
    ($statements:ident,) => {
        $statements.iter_enumerated()
    };
}

macro_rules! field_accesses_iter {
    ($field_accesses:ident, mut) => {
        $field_accesses.fields.iter_mut()
    };
    ($field_accesses:ident,) => {
        $field_accesses.fields.iter()
    };
}

macro_rules! make_ast_visitor {
    ($visitor_trait_name:ident, $($mutability:ident)?) => {
        pub trait $visitor_trait_name {
            fn visit_program(&mut self, program: &$($mutability)? Program) {
                self.super_program(program);
            }
            fn visit_functions(&mut self, functions: &$($mutability)? Functions) {
                self.super_functions(functions);
            }
            fn visit_function_data(&mut self, idx: FunctionIdx, function_data: &$($mutability)? FunctionData) {
                self.super_function_data(idx, function_data);
            }
            fn visit_basic_blocks(&mut self, idx: FunctionIdx, basic_blocks: &$($mutability)? BasicBlocks) {
                self.super_basic_blocks(idx, basic_blocks);
            }
            fn visit_basic_block_data(&mut self, function: FunctionIdx, block: BasicBlockIdx, basic_block_data: &$($mutability)? BasicBlockData) {
                self.super_basic_block_data(function, block, basic_block_data);
            }
            fn visit_statement(&mut self, statement: &$($mutability)? Statement, location: Location) {
                self.super_statement(statement, location);
            }
            fn visit_statement_kind(&mut self, statement_kind: &$($mutability)? StatementKind, location: Location) {
                self.super_statement_kind(statement_kind, location);
            }
            fn visit_terminator(&mut self, terminator: &$($mutability)? Terminator, location: Location) {
                self.super_terminator(terminator, location);
            }
            fn visit_terminator_kind(&mut self, kind: &$($mutability)? TerminatorKind, location: Location) {
                self.super_terminator_kind(kind, location);
            }
            fn visit_source_info(&mut self, source_info: &$($mutability)? SourceInfo, location: Location) {
                self.super_source_info(source_info, location);
            }
            fn visit_exp(&mut self, exp: &$($mutability)? Exp) {
                self.super_exp(exp);
            }
            fn visit_variable_ref(&mut self, variable_ref: &$($mutability)? VariableRef) {
                self.super_variable_ref(variable_ref);
            }
            fn visit_access_path(&mut self, access_path: &$($mutability)? AccessPath) {
                self.super_access_path(access_path);
            }
            fn visit_field_accesses(&mut self, field_accesses: &$($mutability)? FieldAccesses) {
                self.super_field_accesses(field_accesses);
            }
            fn visit_field_access(&mut self, field_access: &$($mutability)? FieldAccess) {
                self.super_field_access(field_access);
            }
            fn visit_call_style(&mut self, call_style: &$($mutability)? CallStyle) {
                self.super_call_style(call_style);
            }
            fn visit_call_edges(&mut self, call_edges: &$($mutability)? CallEdges) {
                self.super_call_edges(call_edges);
            }
            fn visit_params(&mut self, params: &$($mutability)? Params) {
                self.super_params(params);
            }
            fn visit_parameter_type(&mut self, ty: &$($mutability)? ParameterType) {
                self.super_parameter_type(ty);
            }
            fn visit_return_type(&mut self, return_type: &$($mutability)? ReturnType) {
                self.super_return_type(return_type);
            }

            // The `super_xxx` methods define default behavior and aren't meant to be overriden.
            fn super_program(&mut self, program: &$($mutability)? Program) {
                let Program { functions } = program;
                self.visit_functions(functions);
            }

            fn super_functions(&mut self, functions: &$($mutability)? Functions) {
                let Functions { functions } = functions;
                for (fidx, data) in index_vec_iter!(functions, $($mutability, true)?) {
                    self.visit_function_data(fidx, data);
                }
            }

            fn super_function_data(&mut self, idx: FunctionIdx, data: &$($mutability)? FunctionData) {
                let FunctionData { name: _, params, return_type, blocks } = data;
                self.visit_params(params);
                self.visit_return_type(return_type);
                self.visit_basic_blocks(idx, blocks);
            }

            fn super_basic_blocks(&mut self, idx: FunctionIdx, basic_blocks: &$($mutability)? BasicBlocks) {
                for (bb, basic_block) in basic_blocks_iter!(basic_blocks, $($mutability, true)?) {
                    self.visit_basic_block_data(idx, bb, basic_block);
                }
            }

            fn super_basic_block_data(&mut self, function: FunctionIdx, block: BasicBlockIdx, data: &$($mutability)? BasicBlockData) {
                let BasicBlockData { statements, terminator } = data;
                for (idx, statement) in index_vec_iter!(statements, $($mutability, true)?) {
                    self.visit_statement(statement, Location { function, block, statement_index: idx.index() });
                }
                if let Some(terminator) = terminator {
                  self.visit_terminator(terminator, Location { function, block, statement_index: statements.len() });
                }
            }

            fn super_source_info(&mut self, _source_info: &$($mutability)? SourceInfo, _location: Location) {
                // Nothing to do
            }

            fn super_statement(&mut self, statement: &$($mutability)? Statement, location: Location) {
                let Statement { source_info, kind } = statement;
                self.visit_source_info(source_info, location);
                self.visit_statement_kind(kind, location);
            }

            fn super_statement_kind(&mut self, statement_kind: &$($mutability)? StatementKind, _location: Location) {
                use StatementKind::*;
                match statement_kind {
                    Assign { dest, sources } => {
                        for src in sources {
                            self.visit_exp(src);
                        }
                        self.visit_variable_ref(dest);
                    }
                    Load { dest, source, field } => {
                        self.visit_variable_ref(dest);
                        self.visit_variable_ref(source);
                        self.visit_field_access(field);
                    }
                    Store { dest, field, value } => {
                        self.visit_variable_ref(dest);
                        self.visit_field_access(field);
                        self.visit_variable_ref(value);
                    }
                    CallAssign { rets, args, style } => {
                        self.visit_call_style(style);
                        for a in args {
                            self.visit_exp(a);
                        }
                        for r in rets {
                            self.visit_variable_ref(r);
                        }
                    }
                    Phi { dest, operands } => {
                        self.visit_variable_ref(dest);
                        for (_, op) in operands {
                            self.visit_variable_ref(op);
                        }
                    }
                    ParamFlow { params, global } => {
                        for op in params {
                            self.visit_variable_ref(op);
                        }
                        self.visit_variable_ref(global);
                    }
                    Nop => (),
                }
            }

            fn super_call_style(&mut self, call_style: &$($mutability)? CallStyle) {
                match call_style {
                    CallStyle::DirectCall { call_edges } => self.visit_call_edges(call_edges),
                    CallStyle::FuncPtrCall { callee, signature: _ } => {
                        self.visit_access_path(callee);
                    }
                    CallStyle::JavaCall { receiver, cls: _, simple_name: _, descriptor: _ } => {
                        self.visit_variable_ref(receiver)
                    },
                    CallStyle::Unknown => (),
                }
            }

            fn super_terminator(&mut self, terminator: &$($mutability)? Terminator, location: Location) {
                let Terminator { source_info, kind } = terminator;
                self.visit_source_info(source_info, location);
                self.visit_terminator_kind(kind, location);
            }

            fn super_terminator_kind(&mut self, terminator: &$($mutability)? TerminatorKind, _location: Location) {
                use TerminatorKind::*;
                match terminator {
                    Return { args } => {
                        for arg in args {
                            self.visit_exp(arg);
                        }
                    },
                    Goto { targets: _ } => (),
                }
                // Nothing else to do.
            }

            fn super_exp(&mut self, exp: &$($mutability)? Exp) {
                match exp {
                    Exp::AccessPath(access_path) => {
                        self.visit_access_path(access_path);
                    }
                    Exp::Bytes(_) => {}
                    Exp::Str(_) => {}
                    Exp::ObjectRef(_) => {}
                }
            }

            fn super_variable_ref(&mut self, _variable_ref: &$($mutability)? VariableRef) {
                // Can't mut visit variable, so don't
            }

            fn super_access_path(&mut self, access_path: &$($mutability)? AccessPath) {
                let AccessPath { variable_ref, path } = access_path;
                self.visit_variable_ref(variable_ref);
                self.visit_field_accesses(path);
            }

            fn super_field_accesses(&mut self, field_accesses: &$($mutability)? FieldAccesses) {
                // Traverse each field access in the sequence
                for field_access in field_accesses_iter!(field_accesses, $($mutability)?) {
                    self.visit_field_access(field_access);
                }
            }

            fn super_field_access(&mut self, field_access: &$($mutability)? FieldAccess) {
                match field_access {
                    FieldAccess::Symbol(_) => {
                        // No additional traversal needed for Symbol
                    }
                    FieldAccess::Offset(_) => {
                        // No additional traversal needed for Offset
                    }
                }
            }

            fn super_call_edges(&mut self, _call_edges: &$($mutability)? CallEdges) {
                // No additional traversal needed for CallEdges
            }

            fn super_params(&mut self, params: &$($mutability)? Params) {
                let Params { parameters } = params;
                for param in parameters {
                    self.visit_parameter_type(param);
                }
            }

            fn super_parameter_type(&mut self, _ty: &$($mutability)? ParameterType) {
                // No additional traversal needed for ParameterType
            }

            fn super_return_type(&mut self, return_type: &$($mutability)? ReturnType) {
                let ReturnType { arity: _ } = return_type;
                // Nothing else to do
            }
        }
    };
}

make_ast_visitor!(Visitor,);
make_ast_visitor!(MutVisitor, mut);
