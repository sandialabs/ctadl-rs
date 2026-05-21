//! JVM (.jar and .class) language frontend
// Mostly copied from the dex language frontend

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use hashbrown::hash_map::HashMap;
use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactKey, SourceInfoBuilder, SpanLen};

use crate::error::Error;
use ctadl_ir::mir::call::{
    JavaClass, JavaMethod, JavaSignature, JavaSimpleName, VirtualMethodTable,
};
use ctadl_ir::*;

use jvm_reader::flow::{CallInfo, CallKind, ConstantValue, DataflowInfo, Location};
use jvm_reader::{ClassFileParser, JarFileParser};

//#[cfg(test)]
//mod tests;

pub fn import_jar(file: &Path) -> Result<ProgramInfo, Error> {
    //let data = read_file_bytes(file)?;
    let parser = JarFileParser::open(file)?;
    let mut ctx = Context::new();
    let mut builders = Builders::new();

    for (sub_artifact_id, parser) in parser.class_parsers().iter().enumerate() {
        let key = ArtifactKey {
            path: file.to_string_lossy().to_string(),
            sub_artifact_id: sub_artifact_id.try_into().unwrap(),
            hash: Vec::new(),
            encoding: source_info::ArtifactEncoding::Binary,
        };
        ctx.process(parser, key, &mut builders)?;
    }
    ctx.finish(builders)
}

pub fn import_class(file: &Path) -> Result<ProgramInfo, Error> {
    let data = read_file_bytes(file)?;
    let parser = ClassFileParser::parse(&data)?;
    let mut ctx = Context::new();
    let mut builders = Builders::new();
    let key = ArtifactKey {
        path: file.to_string_lossy().to_string(),
        sub_artifact_id: 0,
        hash: Vec::new(),
        encoding: source_info::ArtifactEncoding::Binary,
    };

    ctx.process(&parser, key, &mut builders)?;
    ctx.finish(builders)
}

/// Builders for program and metadata
struct Builders {
    program: Program,
    vmt: VirtualMethodTable,
    source_info_builder: SourceInfoBuilder,
}

impl Builders {
    fn new() -> Self {
        let artifact_metadata = source_info::ArtifactMetadata::new();
        Self {
            program: Program::default(),
            vmt: VirtualMethodTable::new_java(),
            source_info_builder: SourceInfoBuilder::new(artifact_metadata),
        }
    }
}

#[derive(Debug)]
struct Context {
    // vmt entries for externs so far
    ext: HashMap<
        String,
        (
            JavaClass,
            JavaSimpleName,
            JavaSignature,
            JavaMethod,
            Vec<ParameterType>,
            ReturnType,
        ),
    >,
}

impl Context {
    fn new() -> Self {
        Self {
            ext: Default::default(),
        }
    }

    fn process(
        &mut self,
        parser: &ClassFileParser,
        artifact_key: ArtifactKey,
        builders: &mut Builders,
    ) -> Result<(), Error> {
        // Iterate over all classes (no artificial limit).
        for class_def in parser.classes() {
            let class_name = "L".to_owned() + parser.class_name()? + ";";
            log::trace!("Class: {}", class_name);
            // Populate class hierarchy information for the VMT.
            // Immediate superclass (if any) and immediate super‑interfaces.
            let superclass_opt = if class_def.super_class != 0 {
                parser
                    .get_class_name(class_def.super_class)
                    .ok()
                    .map(|arg0: &str| JavaClass(arg0.to_string().into()))
            } else {
                None
            };
            log::trace!(
                "Superclass: {}",
                match superclass_opt {
                    Some(JavaClass(ref s)) => s.to_string(),
                    None => "None".to_string(),
                }
            );

            let mut iface_vec = SmallVec::new();
            if let Some(jc) = superclass_opt {
                iface_vec.push(jc)
            };
            for type_idx in &class_def.interfaces {
                iface_vec.push(JavaClass(
                    parser
                        .get_class_name(*type_idx)
                        .ok()
                        .unwrap()
                        .to_string()
                        .into(),
                ));
                log::trace!(
                    "Interface: {}",
                    parser.get_class_name(*type_idx).ok().unwrap()
                );
            }

            if let VirtualMethodTable::Java { hierarchy, .. } = &mut builders.vmt {
                hierarchy.insert(JavaClass(class_name.to_string().into()), iface_vec);
            }
            for enc in parser.methods() {
                let sig = parser.method_signature(enc)?;
                let method_name = parser.method_name(enc)?;
                let fidx = builders.program.new_function();
                // Reset temporaries per function.
                let fdat = &mut builders.program[fidx];

                // ---------------------------------------------------------------------
                // Collect Java virtual method information for the VMT.
                // Insert entry into the virtual method table stored in the context.
                // Compute a JavaSignature that contains only the parameter types and return type.
                // The full method signature (`sig`) includes the enclosing class; we want the proto
                // pretty‑signature, e.g. "(I)I". Use the parser's `proto_signature` helper.
                let java_sig = parser.method_proto(enc)?;
                let full_name: String = class_name.to_owned() + "->" + &sig;
                fdat.name = full_name.clone();

                let params = jvm_reader::descriptor_parameter_info(&java_sig);
                for p in params {
                    match p.kind {
                        jvm_reader::MethodParameterKind::Primitive => {
                            fdat.params.push(ParameterType::ByVal)
                        }
                        jvm_reader::MethodParameterKind::Reference => {
                            fdat.params.push(ParameterType::ByRef)
                        }
                    };
                }

                // TODO: might need this to be always arity 2 (ret val + exception)
                // regardless of whether void or not; this won't mess up our stack accounting
                // because we already finished that phase
                let return_arity = match jvm_reader::descriptor_returns_value(&java_sig) {
                    true => 1,
                    false => 0,
                };
                fdat.return_type = ReturnType {
                    arity: return_arity,
                };

                if let VirtualMethodTable::Java { methods, .. } = &mut builders.vmt {
                    methods.push((
                        JavaClass(class_name.to_string().into()),
                        JavaSimpleName(method_name.clone().into()),
                        JavaSignature(java_sig.clone().into()),
                        JavaMethod(full_name.clone().into()),
                    ));
                }

                // ---------------------------------------------------------------------
                match enc.code {
                    None => {
                        log::trace!("No code for function {}", method_name)
                    }
                    Some(_) => {
                        log::trace!("Processing code for function {}", method_name);
                        //let basic_blocks = compute_basic_blocks_for_method(class_def, enc)?;
                        let basic_blocks = parser
                            .basic_blocks_with_stack_slots(enc)?
                            .expect("Non-empty function");

                        for bb in basic_blocks.clone().blocks() {
                            let mut bb_data = BasicBlockData::new(None);

                            // Add statements to the basic block
                            for instr in bb.instructions(&basic_blocks) {
                                let source_info =
                                    SourceInfo::new(builders.source_info_builder.span_for(
                                        artifact_key.clone(),
                                        instr.file_byte_offset,
                                        SpanLen::ByteLen(instr.byte_length),
                                    ));
                                match &instr.call {
                                    None => {}
                                    Some(call_info) => {
                                        let mut stmt = self
                                            .decode_call(parser, call_info)
                                            .expect("Call should be there");
                                        stmt.source_info = source_info;
                                        bb_data.push_back(stmt);
                                    }
                                }
                                for df in &instr.dataflow {
                                    let mut stmt = self
                                        .dataflow_to_assign(parser, df)
                                        .expect("Dataflow should be there");
                                    stmt.source_info = source_info;
                                    bb_data.push_back(stmt);
                                }
                            }

                            // TODO: Add correct terminator (successors) to the basic block
                            // return? successors? no successors?
                            let term = match bb.successors.is_empty() {
                                // returns are treated as empty successors, no fallthrough / no branch targets
                                true => TerminatorKind::Return {
                                    args: match return_arity {
                                        1 => smallvec![
                                            self.convert_location_to_exp(&Location::StackSlot(0))
                                        ],
                                        _ => SmallVec::new(),
                                    },
                                },
                                // any other control flows will be present here
                                false => TerminatorKind::Goto {
                                    targets: bb
                                        .successors
                                        .iter()
                                        .map(|&b| BasicBlockIdx::new(b))
                                        .collect::<SmallVec<[BasicBlockIdx; 4]>>(),
                                },
                            };
                            bb_data.terminator = Some(Terminator::new_kind(term));
                            fdat.blocks.blocks_mut().push(bb_data);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn finish(&mut self, mut builders: Builders) -> Result<ProgramInfo, Error> {
        let mut program = builders.program;
        for (_sig, entry) in self.ext.drain() {
            if let VirtualMethodTable::Java { methods, .. } = &mut builders.vmt {
                if !methods
                    .iter()
                    .any(|(_class_name, _simple_name, _signature, defined_method)| {
                        &entry.3 == defined_method
                    })
                {
                    log::trace!("adding external method: {}", &entry.3);
                    methods.push((
                        entry.0.clone(),
                        entry.1.clone(),
                        entry.2.clone(),
                        entry.3.clone(),
                    ));

                    // Add empty definition
                    let fidx = program.new_function();
                    let fdat = &mut program.functions[fidx];
                    let JavaMethod(name) = entry.3.clone();
                    fdat.name = name.to_string();
                    for p in entry.4 {
                        fdat.params.push(p);
                    }
                    fdat.return_type = entry.5;
                } else {
                    log::trace!("skipping defined method: {}", &entry.3);
                }
            }
        }

        log::trace!("program: {program}");
        // Verify the generated program.
        program.verify()?;

        let source_info = builders.source_info_builder.finish();
        log::trace!("source_info: {source_info}");
        let vmt = builders.vmt;
        Ok(ProgramInfo {
            program,
            vmt,
            source_info,
        })
    }

    fn decode_call(&mut self, _parser: &ClassFileParser, call: &CallInfo) -> Option<Statement> {
        // Get call target
        let style = match &call.receiver {
            None => {
                match call.call_kind {
                    CallKind::Dynamic => {
                        // I don't know why dynamic calls with no reciever exist, but apparently makeConcatWithContants does this
                        let class_name = "?unknown";
                        let method_name = call.dynamic_name.as_ref().unwrap();
                        let descr = call.dynamic_type.as_ref().unwrap();
                        let java_sig = "L".to_owned() + class_name + ";->" + method_name + descr;
                        let in_params = jvm_reader::descriptor_parameter_info(descr);
                        let mut out_params = Vec::new();
                        for p in in_params {
                            match p.kind {
                                jvm_reader::MethodParameterKind::Primitive => {
                                    out_params.push(ParameterType::ByVal)
                                }
                                jvm_reader::MethodParameterKind::Reference => {
                                    out_params.push(ParameterType::ByRef)
                                }
                            };
                        }
                        // All functions return 2 values: (normal_return, exception_return)
                        let return_arity = match jvm_reader::descriptor_returns_value(descr) {
                            true => 1,
                            false => 0,
                        };
                        self.ext.insert(
                            java_sig.clone(),
                            (
                                JavaClass(class_name.into()),
                                JavaSimpleName(method_name.clone().into()),
                                JavaSignature(descr.clone().into()),
                                JavaMethod(java_sig.clone().into()),
                                out_params,
                                ReturnType {
                                    arity: return_arity,
                                },
                            ),
                        );
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit([java_sig].into_iter().collect()),
                        }
                    }
                    CallKind::Interface => CallStyle::Unknown,
                    CallKind::Special => CallStyle::Unknown,
                    CallKind::Virtual => CallStyle::Unknown,
                    CallKind::Static => {
                        let class_name =
                            "L".to_owned() + &call.target.as_ref().unwrap().class_name + ";";
                        let method_name = &call.target.as_ref().unwrap().method_name;
                        let descr = &call.target.as_ref().unwrap().descriptor;
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        let in_params = jvm_reader::descriptor_parameter_info(descr);
                        let mut out_params = Vec::new();
                        for p in in_params {
                            match p.kind {
                                jvm_reader::MethodParameterKind::Primitive => {
                                    out_params.push(ParameterType::ByVal)
                                }
                                jvm_reader::MethodParameterKind::Reference => {
                                    out_params.push(ParameterType::ByRef)
                                }
                            };
                        }
                        // All functions return 2 values: (normal_return, exception_return)
                        let return_arity = match jvm_reader::descriptor_returns_value(descr) {
                            true => 1,
                            false => 0,
                        };
                        self.ext.insert(
                            java_sig.clone(),
                            (
                                JavaClass(class_name.clone().into()),
                                JavaSimpleName(method_name.clone().into()),
                                JavaSignature(descr.clone().into()),
                                JavaMethod(java_sig.clone().into()),
                                out_params,
                                ReturnType {
                                    arity: return_arity,
                                },
                            ),
                        );
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit([java_sig].into_iter().collect()),
                        }
                    }
                }
            }
            Some(recv) => {
                match call.call_kind {
                    // Java invokedynamic calls have a bootstrap method index and dynamic name/type
                    // I'm not entirely sure what these are supposed to look like
                    CallKind::Dynamic => {
                        let class_name =
                            "L".to_owned() + &call.target.as_ref().unwrap().class_name + ";";
                        let method_name = &call.target.as_ref().unwrap().method_name;
                        let descr = call.dynamic_type.as_ref().unwrap();
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        let in_params = jvm_reader::descriptor_parameter_info(descr);
                        let mut out_params = Vec::new();
                        for p in in_params {
                            match p.kind {
                                jvm_reader::MethodParameterKind::Primitive => {
                                    out_params.push(ParameterType::ByVal)
                                }
                                jvm_reader::MethodParameterKind::Reference => {
                                    out_params.push(ParameterType::ByRef)
                                }
                            };
                        }
                        // All functions return 2 values: (normal_return, exception_return)
                        let return_arity = match jvm_reader::descriptor_returns_value(descr) {
                            true => 1,
                            false => 0,
                        };
                        self.ext.insert(
                            java_sig.clone(),
                            (
                                JavaClass(class_name.clone().into()),
                                JavaSimpleName(method_name.clone().into()),
                                JavaSignature(descr.clone().into()),
                                JavaMethod(java_sig.clone().into()),
                                out_params,
                                ReturnType {
                                    arity: return_arity,
                                },
                            ),
                        );
                        CallStyle::JavaCall {
                            receiver: self.convert_location_to_var_ref(recv),
                            cls: class_name.clone().into(),
                            simple_name: method_name.clone().into(),
                            descriptor: descr.clone().into(),
                        }
                    }
                    // other calls have a class name, method name, and descriptor
                    _ => {
                        let class_name =
                            "L".to_owned() + &call.target.as_ref().unwrap().class_name + ";";
                        let method_name = &call.target.as_ref().unwrap().method_name;
                        let descr = &call.target.as_ref().unwrap().descriptor;
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        let in_params = jvm_reader::descriptor_parameter_info(descr);
                        let mut out_params = Vec::new();
                        for p in in_params {
                            match p.kind {
                                jvm_reader::MethodParameterKind::Primitive => {
                                    out_params.push(ParameterType::ByVal)
                                }
                                jvm_reader::MethodParameterKind::Reference => {
                                    out_params.push(ParameterType::ByRef)
                                }
                            };
                        }
                        // All functions return 2 values: (normal_return, exception_return)
                        let return_arity = match jvm_reader::descriptor_returns_value(descr) {
                            true => 1,
                            false => 0,
                        };
                        self.ext.insert(
                            java_sig.clone(),
                            (
                                JavaClass(class_name.clone().into()),
                                JavaSimpleName(method_name.clone().into()),
                                JavaSignature(descr.clone().into()),
                                JavaMethod(java_sig.clone().into()),
                                out_params,
                                ReturnType {
                                    arity: return_arity,
                                },
                            ),
                        );
                        CallStyle::JavaCall {
                            receiver: self.convert_location_to_var_ref(recv),
                            cls: class_name.clone().into(),
                            simple_name: method_name.clone().into(),
                            descriptor: descr.clone().into(),
                        }
                    }
                }
            }
        };

        let args: SmallVec<[Exp; 4]> = call
            .arguments
            .iter()
            .map(|x| self.convert_location_to_exp(x))
            .collect();
        // Get return value
        // JVM returns onto the stack (gross)
        // Do void functions still return something on the stack ?
        let call_result = match &call.return_value {
            None => smallvec![],
            Some(loc) => smallvec![self.convert_location_to_var_ref(loc)],
        };

        Some(Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: call_result,
            args,
        }))
    }

    fn dataflow_to_assign(
        &mut self,
        _parser: &ClassFileParser,
        data: &DataflowInfo,
    ) -> Option<Statement> {
        let mut sources = SmallVec::new();
        for source_loc in data.sources.iter() {
            sources.push(self.convert_location_to_exp(source_loc));
        }
        Some(Statement::new_kind(StatementKind::Assign {
            dest: self.convert_location_to_var_ref(&data.destination),
            sources,
        }))
    }

    fn convert_location_to_exp(&mut self, loc: &Location) -> Exp {
        match loc {
            Location::StackSlot(_) | Location::StackInput(_) => Exp::new_access_path(
                AccessPath::without_fields(self.convert_location_to_var_ref(loc)),
            ),
            Location::Constant(ConstantValue::Integer(n)) => {
                Exp::new_bytes(n.to_be_bytes().to_vec())
            }
            Location::Constant(ConstantValue::String(s)) => Exp::new_str(s),
            Location::FieldRef(f) => Exp::new_access_path(AccessPath::new(
                self.convert_location_to_var_ref(loc),
                [mir::FieldAccess::Symbol(f.field_name.clone().into())],
            )),
            _ => Exp::new_access_path(AccessPath::without_fields(
                self.convert_location_to_var_ref(loc),
            )),
        }
    }

    fn convert_location_to_var_ref(&mut self, loc: &Location) -> VariableRef {
        match loc {
            Location::StackSlot(n) => VariableRef::new_local(format!("stack{}", n)),
            Location::StackInput(_) | Location::StackOutput => {
                VariableRef::new_local("Stack Local?".to_string())
            }
            Location::Register(n) => VariableRef::new_local(format!("reg{}", n)),
            Location::Parameter(n) => VariableRef::new_parameter((*n).into()),
            // Just the var ref part - field will be put in later
            Location::FieldRef(f) => VariableRef::new_local(f.class_name.to_string()),
            // TODO: not sure what is going on with this one, why is there no base/index?
            Location::ArrayElement { base, offset } => match (base.as_ref(), offset.as_ref()) {
                (Location::StackSlot(n), Location::StackSlot(m)) => {
                    VariableRef::new_local(format!("stack{}[stack{}]", n, m))
                }
                _ => VariableRef::new_local("unknownArrayOp".to_string()),
            },
            _ => VariableRef::new_local("unknownLocationType".to_string()),
        }
    }
}

fn read_file_bytes<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    let path = path.as_ref();
    let mut f = File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}
