//! JVM (.jar and .class) language frontend
// Mostly copied from the dex language frontend

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use hashbrown::HashSet;
use hashbrown::hash_map::HashMap;
use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactKey, SourceInfoBuilder, SpanLen};

use crate::error::{Error, ErrorContext};
use ctadl_ir::mir::call::{
    CallObject, JavaClass, JavaMethod, JavaSignature, JavaSimpleName, VirtualMethodTable,
};
use ctadl_ir::*;

use jvm_reader::flow::{CallInfo, CallKind, ConstantValue, DataflowInfo, Location};
use jvm_reader::{ClassFileParser, JarFileParser};

const JVM_ACC_STATIC: u16 = 0x0008;
const JVM_ACC_NATIVE: u16 = 0x0100;

/// JVM internal names (`java/lang/Object`, `MyInterface`) and type descriptors
/// (`LMyInterface;`) to the `L...;` symbol form used in MIR and CHA.
fn jvm_class_symbol(name: &str) -> String {
    if name.starts_with('L') && name.ends_with(';') {
        name.to_string()
    } else {
        format!("L{};", name)
    }
}

fn jvm_descriptor_to_params(descriptor: &str, is_instance: bool) -> Vec<ParameterType> {
    let mut params = Vec::new();
    if is_instance {
        params.push(ParameterType::ByRef);
    }
    for p in jvm_reader::descriptor_parameter_info(descriptor) {
        match p.kind {
            jvm_reader::MethodParameterKind::Primitive => params.push(ParameterType::ByVal),
            jvm_reader::MethodParameterKind::Reference => params.push(ParameterType::ByRef),
        }
    }
    params
}

//#[cfg(test)]
//mod tests;

pub fn import_jar(file: &Path) -> Result<ProgramInfo, Error> {
    //let data = read_file_bytes(file)?;
    let parser =
        JarFileParser::open(file).err_context(|| format!("reading jar: {}", file.display()))?;
    let mut ctx = Context::new();
    let mut builders = Builders::new();

    log::info!(
        "{}: {} class file(s)",
        file.display(),
        parser.class_parsers().len()
    );
    for (sub_artifact_id, parser) in parser.class_parsers().iter().enumerate() {
        let key = ArtifactKey {
            path: file.to_string_lossy().to_string(),
            sub_artifact_id: sub_artifact_id.try_into().unwrap(),
            hash: Vec::new(),
            encoding: source_info::ArtifactEncoding::Binary,
        };
        ctx.process(parser, key, &mut builders)
            .err_context(|| format!("converting class in jar: {}", file.display()))?;
    }
    ctx.finish(builders)
}

pub fn import_class(file: &Path) -> Result<ProgramInfo, Error> {
    let data =
        read_file_bytes(file).err_context(|| format!("reading class file: {}", file.display()))?;
    let parser = ClassFileParser::parse(&data)
        .err_context(|| format!("parsing class file: {}", file.display()))?;
    let mut ctx = Context::new();
    let mut builders = Builders::new();
    let key = ArtifactKey {
        path: file.to_string_lossy().to_string(),
        sub_artifact_id: 0,
        hash: Vec::new(),
        encoding: source_info::ArtifactEncoding::Binary,
    };

    ctx.process(&parser, key, &mut builders)
        .err_context(|| format!("converting class file: {}", file.display()))?;
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
    call_result: Option<VariableRef>,
    catch_result: Option<VariableRef>,
}

type DupSlotPair = (u32, u32);

impl Context {
    fn new() -> Self {
        Self {
            ext: Default::default(),
            call_result: None,
            catch_result: None,
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
            let class_name = jvm_class_symbol(parser.class_name()?);
            log::trace!("Class: {}", class_name);
            // Populate class hierarchy information for the VMT.
            // Immediate superclass (if any) and immediate super‑interfaces.
            let superclass_opt = if class_def.super_class != 0 {
                parser
                    .get_class_name(class_def.super_class)
                    .ok()
                    .map(|name| JavaClass(jvm_class_symbol(name).into()))
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
                let iface = jvm_class_symbol(parser.get_class_name(*type_idx).ok().unwrap());
                iface_vec.push(JavaClass(iface.clone().into()));
                log::trace!("Interface: {}", iface);
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

                let is_instance = enc.access_flags & JVM_ACC_STATIC == 0;
                for p in jvm_descriptor_to_params(&java_sig, is_instance) {
                    fdat.params.push(p);
                }

                // All JVM functions model (normal_return, exception_return) like DEX.
                fdat.return_type = ReturnType { arity: 2 };

                if let VirtualMethodTable::Java { methods, .. } = &mut builders.vmt {
                    methods.push((
                        JavaClass(class_name.to_string().into()),
                        JavaSimpleName(method_name.clone().into()),
                        JavaSignature(java_sig.clone().into()),
                        JavaMethod(full_name.clone().into()),
                    ));
                }
                // Native methods are additionally listed in `natives`, the column the JNI
                // bridge reads. They are already in `methods` above -- this frontend walks
                // every declared method, code or not -- so the extra row is only there to
                // carry the staticness the bridge needs to know whether slot 0 is `this`.
                if enc.access_flags & JVM_ACC_NATIVE != 0
                    && let VirtualMethodTable::Java { natives, .. } = &mut builders.vmt
                {
                    natives.push((
                        JavaClass(class_name.to_string().into()),
                        JavaSimpleName(method_name.clone().into()),
                        JavaSignature(java_sig.clone().into()),
                        JavaMethod(full_name.clone().into()),
                        !is_instance,
                    ));
                }

                // ---------------------------------------------------------------------
                match &enc.code {
                    None => {
                        log::trace!("No code for function {}", method_name)
                    }
                    Some(code_attr) => {
                        log::trace!("Processing code for function {}", method_name);
                        let basic_blocks = parser
                            .basic_blocks_with_stack_slots(enc)?
                            .expect("Non-empty function");

                        let handler_pcs: HashSet<u32> = code_attr
                            .exception_table
                            .iter()
                            .map(|e| e.handler_pc as u32)
                            .collect();

                        let mut stack_aliases: HashMap<String, VariableRef> = HashMap::new();
                        let mut dup_slot_pairs: HashSet<DupSlotPair> = HashSet::new();
                        let mut last_aload_reg: Option<VariableRef> = None;

                        for bb in basic_blocks.clone().blocks() {
                            let mut bb_data = BasicBlockData::new(None);

                            // Exception handler entry: model MoveException (thrown value → stack slot).
                            if handler_pcs.contains(&bb.start_pc) {
                                let dest =
                                    VariableRef::new_local_idx(fdat.locals.get_or_intern("stack0"));
                                let except = Self::except(&mut fdat.locals);
                                bb_data.push_back(Statement::new_kind(StatementKind::Assign {
                                    dest,
                                    sources: smallvec![Exp::from(AccessPath::without_fields(
                                        except
                                    ),)],
                                }));
                            }

                            let block_instrs = bb.instructions(&basic_blocks);

                            // Add statements to the basic block
                            for (instr_idx, instr) in block_instrs.iter().enumerate() {
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
                                            .decode_call(parser, call_info, &mut fdat.locals)
                                            .expect("Call should be there");
                                        stmt.source_info = source_info;
                                        bb_data.push_back(stmt);
                                        if let Some(ret_loc) = call_info.return_value.as_ref() {
                                            let dest = self.convert_location_to_var_ref(
                                                ret_loc,
                                                &mut fdat.locals,
                                            );
                                            let ret = Self::ret(&mut fdat.locals);
                                            let mut assign =
                                                Statement::new_kind(StatementKind::Assign {
                                                    dest,
                                                    sources: smallvec![Exp::from(
                                                        AccessPath::without_fields(ret),
                                                    )],
                                                });
                                            assign.source_info = source_info;
                                            Self::note_assign_aliases(&assign, &mut stack_aliases);
                                            bb_data.push_back(assign);
                                        }
                                        if let Some(mut link) = Self::init_survivor_link(
                                            call_info,
                                            &dup_slot_pairs,
                                            &mut fdat.locals,
                                        ) {
                                            link.source_info = source_info;
                                            Self::note_assign_aliases(&link, &mut stack_aliases);
                                            bb_data.push_back(link);
                                        }
                                    }
                                }
                                let dup_stmts =
                                    if Self::is_stack_dup_opcode(instr.opcode) {
                                        Some(self.stack_dup_statements(
                                            &instr.dataflow,
                                            &mut fdat.locals,
                                        ))
                                    } else {
                                        None
                                    };
                                if let Some(stmts) = dup_stmts {
                                    Self::record_dup_slot_pairs(
                                        &mut dup_slot_pairs,
                                        &instr.dataflow,
                                    );
                                    for mut stmt in stmts {
                                        stmt.source_info = source_info;
                                        Self::note_assign_aliases(&stmt, &mut stack_aliases);
                                        bb_data.push_back(stmt);
                                    }
                                } else {
                                    for df in &instr.dataflow {
                                        for mut stmt in self.dataflow_to_statements(
                                            instr.opcode,
                                            df,
                                            &stack_aliases,
                                            last_aload_reg.as_ref(),
                                            block_instrs,
                                            instr_idx,
                                            &mut fdat.locals,
                                        ) {
                                            stmt.source_info = source_info;
                                            Self::note_assign_aliases(&stmt, &mut stack_aliases);
                                            if let StatementKind::Assign { dest, sources } =
                                                &stmt.kind
                                                && let [Exp::Variable(v)] = sources.as_slice()
                                                && Self::is_stack_var(dest)
                                                && Self::aload_source_var(v)
                                            {
                                                last_aload_reg = Some(v.clone());
                                            }
                                            bb_data.push_back(stmt);
                                        }
                                    }
                                }
                            }

                            let empty_exp = || Exp::new_bytes(Vec::new());
                            let term = match block_instrs.last() {
                                Some(instr) => match instr.opcode {
                                    0xbf => {
                                        let succs = bb
                                            .successors
                                            .iter()
                                            .map(|&b| BasicBlockIdx::new(b))
                                            .collect::<SmallVec<[BasicBlockIdx; 4]>>();
                                        if succs.is_empty() {
                                            let except = Self::except(&mut fdat.locals);
                                            TerminatorKind::Return {
                                                args: smallvec![
                                                    empty_exp(),
                                                    Exp::from(AccessPath::without_fields(except)),
                                                ],
                                            }
                                        } else {
                                            TerminatorKind::Goto { targets: succs }
                                        }
                                    }
                                    0xac..=0xb0 => TerminatorKind::Return {
                                        args: smallvec![
                                            self.convert_location_to_exp(
                                                &Location::StackSlot(0),
                                                &mut fdat.locals,
                                            ),
                                            empty_exp(),
                                        ],
                                    },
                                    0xb1 => {
                                        let e = empty_exp();
                                        TerminatorKind::Return {
                                            args: smallvec![e.clone(), e],
                                        }
                                    }
                                    _ => {
                                        if bb.successors.is_empty() {
                                            let e = empty_exp();
                                            TerminatorKind::Return {
                                                args: smallvec![e.clone(), e],
                                            }
                                        } else {
                                            TerminatorKind::Goto {
                                                targets: bb
                                                    .successors
                                                    .iter()
                                                    .map(|&b| BasicBlockIdx::new(b))
                                                    .collect::<SmallVec<[BasicBlockIdx; 4]>>(),
                                            }
                                        }
                                    }
                                },
                                None => {
                                    let e = empty_exp();
                                    TerminatorKind::Return {
                                        args: smallvec![e.clone(), e],
                                    }
                                }
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

    fn decode_call(
        &mut self,
        _parser: &ClassFileParser,
        call: &CallInfo,
        locals: &mut Locals,
    ) -> Option<Statement> {
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
                        let out_params = jvm_descriptor_to_params(descr, false);
                        // All functions return 2 values: (normal_return, exception_return)
                        self.ext.insert(
                            java_sig.clone(),
                            (
                                JavaClass(class_name.into()),
                                JavaSimpleName(method_name.clone().into()),
                                JavaSignature(descr.clone().into()),
                                JavaMethod(java_sig.clone().into()),
                                out_params,
                                ReturnType { arity: 2 },
                            ),
                        );
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit([java_sig].into_iter().collect()),
                        }
                    }
                    CallKind::Interface | CallKind::Special | CallKind::Virtual => {
                        let target = call.target.as_ref().expect("call target");
                        let class_name = "L".to_owned() + &target.class_name + ";";
                        let method_name = &target.method_name;
                        let descr = &target.descriptor;
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        let out_params = jvm_descriptor_to_params(descr, true);
                        self.ext.insert(
                            java_sig.clone(),
                            (
                                JavaClass(class_name.clone().into()),
                                JavaSimpleName(method_name.clone().into()),
                                JavaSignature(descr.clone().into()),
                                JavaMethod(java_sig.clone().into()),
                                out_params,
                                ReturnType { arity: 2 },
                            ),
                        );
                        CallStyle::DirectCall {
                            call_edges: CallEdges::Explicit([java_sig].into_iter().collect()),
                        }
                    }
                    CallKind::Static => {
                        let class_name =
                            "L".to_owned() + &call.target.as_ref().unwrap().class_name + ";";
                        let method_name = &call.target.as_ref().unwrap().method_name;
                        let descr = &call.target.as_ref().unwrap().descriptor;
                        let java_sig = class_name.to_owned() + "->" + method_name + descr;
                        let out_params = jvm_descriptor_to_params(descr, false);
                        // All functions return 2 values: (normal_return, exception_return)
                        self.ext.insert(
                            java_sig.clone(),
                            (
                                JavaClass(class_name.clone().into()),
                                JavaSimpleName(method_name.clone().into()),
                                JavaSignature(descr.clone().into()),
                                JavaMethod(java_sig.clone().into()),
                                out_params,
                                ReturnType { arity: 2 },
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
                        let out_params = jvm_descriptor_to_params(descr, true);
                        // All functions return 2 values: (normal_return, exception_return)
                        self.ext.insert(
                            java_sig.clone(),
                            (
                                JavaClass(class_name.clone().into()),
                                JavaSimpleName(method_name.clone().into()),
                                JavaSignature(descr.clone().into()),
                                JavaMethod(java_sig.clone().into()),
                                out_params,
                                ReturnType { arity: 2 },
                            ),
                        );
                        CallStyle::JavaCall {
                            receiver: self.convert_location_to_var_ref(recv, locals),
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
                        let out_params = jvm_descriptor_to_params(descr, true);
                        // All functions return 2 values: (normal_return, exception_return)
                        self.ext.insert(
                            java_sig.clone(),
                            (
                                JavaClass(class_name.clone().into()),
                                JavaSimpleName(method_name.clone().into()),
                                JavaSignature(descr.clone().into()),
                                JavaMethod(java_sig.clone().into()),
                                out_params,
                                ReturnType { arity: 2 },
                            ),
                        );
                        CallStyle::JavaCall {
                            receiver: self.convert_location_to_var_ref(recv, locals),
                            cls: class_name.clone().into(),
                            simple_name: method_name.clone().into(),
                            descriptor: descr.clone().into(),
                        }
                    }
                }
            }
        };

        let args: ctadl_ir::ThinVec<Exp> = call
            .arguments
            .iter()
            .map(|x| self.convert_location_to_exp(x, locals))
            .collect();

        let retval = Self::ret(locals);
        let throwval = Self::except(locals);
        self.call_result = call.return_value.as_ref().map(|_| retval.clone());
        self.catch_result = Some(throwval.clone());

        Some(Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: ctadl_ir::thin_vec![retval, throwval],
            args,
        }))
    }

    fn ret(locals: &mut Locals) -> VariableRef {
        VariableRef::new_local_idx(locals.get_or_intern("retval"))
    }

    fn except(locals: &mut Locals) -> VariableRef {
        VariableRef::new_local_idx(locals.get_or_intern("throwval"))
    }

    fn allocation_exp(class_name: &str) -> Exp {
        let jclass = if class_name.starts_with('L') || class_name.starts_with('[') {
            class_name.to_string()
        } else {
            format!("L{};", class_name)
        };
        Exp::new_object_ref(CallObject::JavaObject(JavaClass(jclass.into())))
    }

    fn jvm_field_symbol(f: &jvm_reader::flow::FieldRef) -> mir::FieldRef {
        let class = format!("L{};", f.class_name);
        mir::FieldRef::symbol(format!("<{}->{}:{}>", class, f.field_name, f.descriptor))
    }

    fn stack_exp(&self, loc: &Location, locals: &mut Locals) -> Exp {
        Exp::from(AccessPath::without_fields(
            self.convert_location_to_var_ref(loc, locals),
        ))
    }

    fn field_ref_in(locs: &[Location]) -> Option<&jvm_reader::flow::FieldRef> {
        locs.iter().find_map(|l| match l {
            Location::FieldRef(f) => Some(f),
            _ => None,
        })
    }

    fn stack_sources(locs: &[Location]) -> impl Iterator<Item = &Location> {
        locs.iter()
            .filter(|l| matches!(l, Location::StackSlot(_) | Location::StackInput(_)))
    }

    fn array_base(locs: &[Location]) -> Option<&Location> {
        locs.iter().find_map(|l| match l {
            Location::ArrayElement { base, .. } => Some(base.as_ref()),
            _ => None,
        })
    }

    fn is_register_var(v: &VariableRef) -> bool {
        v.to_string().starts_with("reg")
    }

    fn is_stack_var(v: &VariableRef) -> bool {
        v.to_string().starts_with("stack")
    }

    fn note_assign_aliases(stmt: &Statement, aliases: &mut HashMap<String, VariableRef>) {
        if let StatementKind::Assign { dest, sources } = &stmt.kind
            && let [src] = sources.as_slice()
        {
            if let Exp::Variable(v) = src {
                let base = aliases
                    .get(&v.to_string())
                    .cloned()
                    .unwrap_or_else(|| v.clone());
                if Self::is_stack_var(dest)
                    && (Self::is_register_var(v) || Self::aload_source_var(v))
                {
                    aliases.insert(dest.to_string(), v.clone());
                } else {
                    aliases.insert(dest.to_string(), base);
                }
                return;
            }
            if matches!(src, Exp::ObjectRef(_)) {
                aliases.insert(dest.to_string(), dest.clone());
            }
        }
    }

    fn aload_source_var(v: &VariableRef) -> bool {
        let s = v.to_string();
        s.starts_with("reg") || s.starts_with("@p")
    }

    fn stack_slot_alias(
        loc: &Location,
        aliases: &HashMap<String, VariableRef>,
        locals: &mut Locals,
    ) -> Option<VariableRef> {
        let stack_var = match loc {
            Location::StackSlot(n) => {
                VariableRef::new_local_idx(locals.get_or_intern(&format!("stack{n}")))
            }
            _ => return None,
        };
        aliases.get(&stack_var.to_string()).cloned()
    }

    fn aload_for_stack_slot(
        block_instrs: &[jvm_reader::flow::InstructionFlowInfo<'_>],
        current_idx: usize,
        object_loc: &Location,
        locals: &mut Locals,
    ) -> Option<VariableRef> {
        let target_slot = Self::stack_slot_index(object_loc)?;
        for inst in block_instrs[..current_idx].iter().rev() {
            if matches!(inst.opcode, 0x15..=0x2d) {
                for df in &inst.dataflow {
                    if Self::stack_slot_index(&df.destination) != Some(target_slot) {
                        continue;
                    }
                    for loc in &df.sources {
                        match loc {
                            Location::Register(n) => {
                                return Some(VariableRef::new_local_idx(
                                    locals.get_or_intern(&format!("reg{n}")),
                                ));
                            }
                            Location::Parameter(n) => {
                                return Some(VariableRef::new_parameter((*n).into()));
                            }
                            _ => {}
                        }
                    }
                }
            }
            if matches!(inst.opcode, 0xb4 | 0xb5) {
                break;
            }
        }
        None
    }

    fn resolve_object_var(
        &self,
        loc: &Location,
        aliases: &HashMap<String, VariableRef>,
        locals: &mut Locals,
    ) -> VariableRef {
        if let Some(base) = Self::stack_slot_alias(loc, aliases, locals) {
            return base;
        }
        let v = self.convert_location_to_var_ref(loc, locals);
        aliases.get(&v.to_string()).cloned().unwrap_or(v)
    }

    fn aload_register_before(
        block_instrs: &[jvm_reader::flow::InstructionFlowInfo<'_>],
        current_idx: usize,
        locals: &mut Locals,
    ) -> Option<VariableRef> {
        for inst in block_instrs[..current_idx].iter().rev() {
            if matches!(inst.opcode, 0x15..=0x2d) {
                for df in &inst.dataflow {
                    for loc in &df.sources {
                        match loc {
                            Location::Register(n) => {
                                return Some(VariableRef::new_local_idx(
                                    locals.get_or_intern(&format!("reg{n}")),
                                ));
                            }
                            Location::Parameter(n) => {
                                return Some(VariableRef::new_parameter((*n).into()));
                            }
                            _ => {}
                        }
                    }
                }
                return None;
            }
            if matches!(inst.opcode, 0xb4 | 0xb5) {
                break;
            }
        }
        None
    }

    fn field_object_base(
        &self,
        loc: &Location,
        aliases: &HashMap<String, VariableRef>,
        last_aload_reg: Option<&VariableRef>,
        block_instrs: &[jvm_reader::flow::InstructionFlowInfo<'_>],
        instr_idx: usize,
        locals: &mut Locals,
    ) -> VariableRef {
        // Prefer a stable register/parameter for this stack slot when it differs from the
        // most recent aload (e.g. aload_0; aload_1; putfield must use this, not the arg).
        if let Some(slot_base) = Self::stack_slot_alias(loc, aliases, locals)
            && !Self::is_stack_var(&slot_base)
        {
            let conflicts_with_last = last_aload_reg.is_some_and(|r| r != &slot_base);
            if conflicts_with_last {
                return slot_base;
            }
        }
        if let Some(reg) = last_aload_reg {
            return reg.clone();
        }
        if let Some(reg) = Self::aload_for_stack_slot(block_instrs, instr_idx, loc, locals) {
            return reg;
        }
        if let Some(reg) = Self::aload_register_before(block_instrs, instr_idx, locals) {
            return reg;
        }
        self.resolve_object_var(loc, aliases, locals)
    }

    /// Stack-manipulation `dup*` opcodes (JVMS 6.5).
    fn is_stack_dup_opcode(opcode: u8) -> bool {
        matches!(opcode, 0x59..=0x5e)
    }

    fn assign_var(dest: VariableRef, src: VariableRef) -> Statement {
        Statement::new_kind(StatementKind::Assign {
            dest,
            sources: smallvec![Exp::from(AccessPath::without_fields(src))],
        })
    }

    fn stack_slot_index(loc: &Location) -> Option<u32> {
        match loc {
            Location::StackSlot(n) => Some(*n),
            _ => None,
        }
    }

    fn record_dup_slot_pair(pairs: &mut HashSet<DupSlotPair>, a: u32, b: u32) {
        if a != b {
            pairs.insert((a.min(b), a.max(b)));
        }
    }

    fn record_dup_slot_pairs(pairs: &mut HashSet<DupSlotPair>, dataflow: &[DataflowInfo]) {
        let slots: SmallVec<[u32; 4]> = dataflow
            .iter()
            .filter_map(|df| Self::stack_slot_index(&df.destination))
            .collect();
        for i in 0..slots.len() {
            for j in (i + 1)..slots.len() {
                Self::record_dup_slot_pair(pairs, slots[i], slots[j]);
            }
        }
    }

    /// After `new; dup; …; invokespecial <init>`, the receiver slot is consumed but a
    /// duplicate reference remains on the stack below it. Link survivor ← receiver so
    /// constructor summaries on the receiver reach the value that `athrow` uses.
    ///
    /// Only emitted when the receiver and the slot immediately below it were outputs of
    /// the same `dup*` instruction (avoids merging unrelated stack values).
    fn init_survivor_link(
        call: &CallInfo,
        dup_slot_pairs: &HashSet<DupSlotPair>,
        locals: &mut Locals,
    ) -> Option<Statement> {
        let target = call.target.as_ref()?;
        if target.method_name != "<init>" {
            return None;
        }
        let receiver = call.receiver.as_ref()?;
        let Location::StackSlot(recv_idx) = receiver else {
            return None;
        };
        if *recv_idx == 0 {
            return None;
        }
        let survivor_idx = recv_idx - 1;
        let pair = (survivor_idx.min(*recv_idx), survivor_idx.max(*recv_idx));
        if !dup_slot_pairs.contains(&pair) {
            return None;
        }
        let survivor =
            VariableRef::new_local_idx(locals.get_or_intern(&format!("stack{survivor_idx}")));
        let recv_var =
            VariableRef::new_local_idx(locals.get_or_intern(&format!("stack{recv_idx}")));
        Some(Self::assign_var(survivor, recv_var))
    }

    /// Lower `dup` / `dup_x1` / … so every duplicate stack slot is flow-connected.
    fn stack_dup_statements(
        &self,
        dataflow: &[DataflowInfo],
        locals: &mut Locals,
    ) -> SmallVec<[Statement; 8]> {
        let mut stmts = SmallVec::new();
        let mut dests = SmallVec::<[VariableRef; 4]>::new();
        for df in dataflow {
            let mut sources = SmallVec::new();
            for source_loc in df.sources.iter() {
                sources.push(self.convert_location_to_exp(source_loc, locals));
            }
            let dest = self.convert_location_to_var_ref(&df.destination, locals);
            stmts.push(Statement::new_kind(StatementKind::Assign {
                dest: dest.clone(),
                sources,
            }));
            dests.push(dest);
        }
        for i in 0..dests.len() {
            for j in (i + 1)..dests.len() {
                stmts.push(Self::assign_var(dests[i].clone(), dests[j].clone()));
                stmts.push(Self::assign_var(dests[j].clone(), dests[i].clone()));
            }
        }
        stmts
    }

    fn dataflow_to_statements(
        &mut self,
        opcode: u8,
        data: &DataflowInfo,
        aliases: &HashMap<String, VariableRef>,
        last_aload_reg: Option<&VariableRef>,
        block_instrs: &[jvm_reader::flow::InstructionFlowInfo<'_>],
        instr_idx: usize,
        locals: &mut Locals,
    ) -> SmallVec<[Statement; 2]> {
        let dest_is_array = matches!(data.destination, Location::ArrayElement { .. });

        match opcode {
            // new / newarray / anewarray
            0xbb..=0xbd => {
                let class_name = data
                    .sources
                    .iter()
                    .find_map(|l| match l {
                        Location::Allocation(c) => Some(c.as_str()),
                        _ => None,
                    })
                    .expect("allocation type");
                smallvec![Statement::new_kind(StatementKind::Assign {
                    dest: self.convert_location_to_var_ref(&data.destination, locals),
                    sources: smallvec![Self::allocation_exp(class_name)],
                })]
            }
            // getstatic
            0xb2 => {
                let field = Self::field_ref_in(&data.sources).expect("getstatic field");
                smallvec![Statement::new_kind(StatementKind::load(
                    self.convert_location_to_var_ref(&data.destination, locals),
                    VariableRef::new_global(),
                    Self::jvm_field_symbol(field),
                ))]
            }
            // putstatic
            0xb3 => {
                let field = match &data.destination {
                    Location::FieldRef(f) => f,
                    _ => panic!("putstatic destination"),
                };
                let value = self.stack_exp(
                    Self::stack_sources(&data.sources)
                        .next()
                        .expect("putstatic value"),
                    locals,
                );
                smallvec![Statement::new_kind(StatementKind::store(
                    AccessPath::without_fields(VariableRef::new_global()),
                    Self::jvm_field_symbol(field),
                    value,
                ))]
            }
            // getfield
            0xb4 => {
                let field = Self::field_ref_in(&data.sources).expect("getfield field");
                let object = self.field_object_base(
                    Self::stack_sources(&data.sources)
                        .next()
                        .expect("getfield object"),
                    aliases,
                    last_aload_reg,
                    block_instrs,
                    instr_idx,
                    locals,
                );
                smallvec![Statement::new_kind(StatementKind::load(
                    self.convert_location_to_var_ref(&data.destination, locals),
                    object,
                    Self::jvm_field_symbol(field),
                ))]
            }
            // putfield
            0xb5 => {
                let field = match &data.destination {
                    Location::FieldRef(f) => f,
                    _ => panic!("putfield destination"),
                };
                let mut stacks = Self::stack_sources(&data.sources);
                let value = self.stack_exp(stacks.next().expect("putfield value"), locals);
                let object_slot = stacks.next().expect("putfield object");
                let object = self.field_object_base(
                    object_slot,
                    aliases,
                    last_aload_reg,
                    block_instrs,
                    instr_idx,
                    locals,
                );
                smallvec![Statement::new_kind(StatementKind::store(
                    AccessPath::without_fields(object),
                    Self::jvm_field_symbol(field),
                    value,
                ))]
            }
            // athrow
            0xbf => {
                let thrown = self.stack_exp(
                    Self::stack_sources(&data.sources)
                        .next()
                        .expect("athrow value"),
                    locals,
                );
                let except = Self::except(locals);
                smallvec![Statement::new_kind(StatementKind::Assign {
                    dest: except,
                    sources: smallvec![thrown],
                })]
            }
            // *aload
            0x2e..=0x35 if Self::array_base(&data.sources).is_some() => {
                let base = Self::array_base(&data.sources).expect("aload base");
                let object = self.field_object_base(
                    base,
                    aliases,
                    last_aload_reg,
                    block_instrs,
                    instr_idx,
                    locals,
                );
                smallvec![Statement::new_kind(StatementKind::load(
                    self.convert_location_to_var_ref(&data.destination, locals),
                    object,
                    mir::FieldRef::symbol("[]"),
                ))]
            }
            // *astore
            0x4f..=0x55 if dest_is_array => {
                let Location::ArrayElement { base, .. } = &data.destination else {
                    unreachable!();
                };
                let value = self.stack_exp(
                    Self::stack_sources(&data.sources)
                        .next()
                        .expect("astore value"),
                    locals,
                );
                let object = self.field_object_base(
                    base,
                    aliases,
                    last_aload_reg,
                    block_instrs,
                    instr_idx,
                    locals,
                );
                smallvec![Statement::new_kind(StatementKind::store(
                    AccessPath::without_fields(object),
                    mir::FieldRef::symbol("[]"),
                    value,
                ))]
            }
            _ => {
                let mut sources = SmallVec::new();
                for source_loc in data.sources.iter() {
                    sources.push(self.convert_location_to_exp(source_loc, locals));
                }
                smallvec![Statement::new_kind(StatementKind::Assign {
                    dest: self.convert_location_to_var_ref(&data.destination, locals),
                    sources,
                })]
            }
        }
    }

    fn convert_location_to_exp(&self, loc: &Location, locals: &mut Locals) -> Exp {
        match loc {
            Location::StackSlot(_) | Location::StackInput(_) => Exp::from(
                AccessPath::without_fields(self.convert_location_to_var_ref(loc, locals)),
            ),
            Location::Constant(ConstantValue::Integer(n)) => Exp::new_int(*n as i64),
            Location::Constant(ConstantValue::Long(n)) => Exp::new_int(*n),
            Location::Constant(ConstantValue::String(s)) => Exp::new_str(s),
            Location::Allocation(class_name) => Self::allocation_exp(class_name),
            // A field read is not expressible as an Exp; genuine field reads are lowered to
            // Load instructions in the getfield/getstatic/aload opcode arms. This &self helper
            // cannot emit a load, so a FieldRef reaching here (a rare generic-source fallback)
            // degrades to its base variable.
            Location::FieldRef(_) => Exp::from(AccessPath::without_fields(
                self.convert_location_to_var_ref(loc, locals),
            )),
            _ => Exp::from(AccessPath::without_fields(
                self.convert_location_to_var_ref(loc, locals),
            )),
        }
    }

    fn convert_location_to_var_ref(&self, loc: &Location, locals: &mut Locals) -> VariableRef {
        match loc {
            Location::StackSlot(n) => {
                VariableRef::new_local_idx(locals.get_or_intern(&format!("stack{}", n)))
            }
            Location::StackInput(_) | Location::StackOutput => {
                VariableRef::new_local_idx(locals.get_or_intern("Stack Local?"))
            }
            Location::Register(n) => {
                VariableRef::new_local_idx(locals.get_or_intern(&format!("reg{}", n)))
            }
            Location::Parameter(n) => VariableRef::new_parameter((*n).into()),
            Location::FieldRef(_) => {
                // Field accesses are lowered in `dataflow_to_statements`; this path is a fallback.
                VariableRef::new_local_idx(locals.get_or_intern("unknownFieldBase"))
            }
            Location::ArrayElement { base, .. } => self.convert_location_to_var_ref(base, locals),
            _ => VariableRef::new_local_idx(locals.get_or_intern("unknownLocationType")),
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
