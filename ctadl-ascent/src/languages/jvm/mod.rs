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
    ext: HashMap<String, (JavaClass, JavaSimpleName, JavaSignature, JavaMethod)>,
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
            let class_name = parser.class_name()?;
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
                fdat.name = sig.clone();
                //self.ext.remove(&sig);

                // ---------------------------------------------------------------------
                // Collect Java virtual method information for the VMT.
                // Insert entry into the virtual method table stored in the context.
                // Compute a JavaSignature that contains only the parameter types and return type.
                // The full method signature (`sig`) includes the enclosing class; we want the proto
                // pretty‑signature, e.g. "(I)I". Use the parser's `proto_signature` helper.
                let java_sig = parser.method_proto(enc)?;

                if let VirtualMethodTable::Java { methods, .. } = &mut builders.vmt {
                    methods.push((
                        JavaClass(class_name.to_string().into()),
                        JavaSimpleName(method_name.clone().into()),
                        JavaSignature(java_sig.clone().into()),
                        JavaMethod(sig.clone().into()),
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
                                match &instr.dataflow {
                                    None => {}
                                    Some(data_info) => {
                                        let mut stmt = self
                                            .dataflow_to_assign(parser, data_info)
                                            .expect("Dataflow should be there");
                                        stmt.source_info = source_info;
                                        bb_data.push_back(stmt);
                                    }
                                }
                            }

                            // TODO: Add correct terminator (successors) to the basic block
                            // return? successors? no successors?
                            let term = match bb.successors.is_empty() {
                                // returns are treated as empty successors, no fallthrough / no branch targets
                                true => TerminatorKind::Return {
                                    args: SmallVec::new(),
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
        let program = builders.program;
        log::trace!("program: {program}");
        // Verify the generated program.
        program.verify()?;
        for (_sig, entry) in self.ext.drain() {
            if let VirtualMethodTable::Java { methods, .. } = &mut builders.vmt {
                methods.push(entry);
            }
        }
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
        // TODO: some of these are probably direct calls, right? We should use a direct call style instead?
        // Get call target
        let style = match &call.receiver {
            None => CallStyle::Unknown,
            Some(recv) => {
                match call.call_kind {
                    // Java invokedynamic calls have a bootstrap method index and dynamic name/type
                    // I'm not entirely sure what these are supposed to look like
                    CallKind::Dynamic => CallStyle::JavaCall {
                        receiver: self.convert_location_to_var_ref(recv),
                        cls: call.target.as_ref().unwrap().class_name.clone().into(),
                        simple_name: call.target.as_ref().unwrap().method_name.clone().into(),
                        descriptor: call
                            .dynamic_name_and_type
                            .as_ref()
                            .unwrap()
                            .to_string()
                            .clone()
                            .into(),
                    },
                    // other calls have a class name, method name, and descriptor
                    _ => CallStyle::JavaCall {
                        receiver: self.convert_location_to_var_ref(recv),
                        cls: call.target.as_ref().unwrap().class_name.clone().into(),
                        simple_name: call.target.as_ref().unwrap().method_name.clone().into(),
                        descriptor: call.target.as_ref().unwrap().descriptor.clone().into(),
                    },
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
        match data.destinations.as_slice() {
            [] => None,
            [x] => Some(Statement::new_kind(StatementKind::Assign {
                dest: self.convert_location_to_var_ref(x),
                sources,
            })),
            [_x, _y, ..] => {
                log::trace!("Multiple destinations in dataflow, skipping");
                None
            }
        }
    }

    fn convert_location_to_exp(&mut self, loc: &Location) -> Exp {
        match loc {
            Location::StackSlot(_) | Location::StackInput(_) => Exp::new_access_path(
                AccessPath::without_fields(self.convert_location_to_var_ref(loc)),
            ),
            Location::Constant(ConstantValue::Integer(n)) => {
                Exp::new_bytes(n.to_be_bytes().to_vec())
            }
            /*Location::FieldRef(f) => {
                Exp::new_access_path(AccessPath::new(, f.field_name))
            },*/
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
            Location::Parameter(n) => VariableRef::new_local(format!("param{}", n)),
            // TODO: this needs to become an acess path
            Location::FieldRef(f) => {
                VariableRef::new_local(format!("field{}.{}", f.class_name, f.field_name))
            }
            // TODO: not sure what is going on with this one, why is there no base/index?
            Location::ArrayElement => VariableRef::new_local("arrayElement".to_string()),
            _ => VariableRef::new_local("Unknown".to_string()),
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
