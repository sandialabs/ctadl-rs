/*! JVM (.jar and .class) language front end.

Takes an artifact and returns a [`ctadl_ir::ProgramInfo`], and does nothing else: it does not
touch the store, choose a front end, or run the engine. Reading `.class` and `.jar` files is
`jvm-reader`'s job. What lives here is the lowering to MIR.
*/

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use hashbrown::HashSet;
use hashbrown::hash_map::HashMap;
use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactKey, SourceInfoBuilder, SpanLen};

use ctadl_import::error::{Error, ErrorContext};
use ctadl_ir::mir::call::{
    CallObject, JavaClass, JavaDispatch, JavaMethod, JavaSignature, JavaSimpleName,
    VirtualMethodTable,
};
use ctadl_ir::*;

use jvm_reader::flow::{CallInfo, CallKind, ConstantValue, DataflowInfo, Location};
use jvm_reader::{ClassFileParser, JarFileParser};

const JVM_ACC_STATIC: u16 = 0x0008;
const JVM_ACC_NATIVE: u16 = 0x0100;
const JVM_ACC_INTERFACE: u16 = 0x0200;
const JVM_ACC_ABSTRACT: u16 = 0x0400;

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
        params.push(ParameterType::Shared);
    }
    for p in jvm_reader::descriptor_parameter_info(descriptor) {
        match p.kind {
            jvm_reader::MethodParameterKind::Primitive => params.push(ParameterType::SharedFields),
            jvm_reader::MethodParameterKind::Reference => params.push(ParameterType::Shared),
        }
    }
    params
}

#[cfg(test)]
mod tests;

pub fn import_jar(file: &Path) -> Result<ProgramInfo, Error> {
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
    /// VMT entries for the externs seen so far.
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
    /// The class whose methods are being lowered. Set once per class in [`Context::process`].
    enclosing: Option<EnclosingClass>,
}

/// What lowering an `invokespecial` needs to know about the class the call appears in.
#[derive(Debug, Default)]
struct EnclosingClass {
    name: Symbol,
    superclass: Option<Symbol>,
    /// The interfaces the class directly implements. An `X.super.m()` names one of these, and
    /// there lookup begins at the named interface rather than at the superclass.
    interfaces: Vec<Symbol>,
}

type DupSlotPair = (u32, u32);

impl Context {
    /// Where an `invokespecial` naming `cls.name` begins method lookup.
    ///
    /// Under `ACC_SUPER` a `super.m()` starts at the superclass of the class declaring the
    /// current method, not at the class the instruction names. Three cases start at the named
    /// class instead: `X.super.m()`, whose reference names an interface the enclosing class
    /// implements directly; a constructor, whose target class is exact; and a private call,
    /// which names the current class.
    ///
    /// `None` when the enclosing class is unknown or has no superclass, which leaves the site
    /// on the ordinary resolvent set.
    fn super_start(&self, cls: &str, name: &str) -> Option<Symbol> {
        let enclosing = self.enclosing.as_ref()?;
        if name == "<init>"
            || &*enclosing.name == cls
            || enclosing.interfaces.iter().any(|i| &**i == cls)
        {
            return Some(Symbol::from(cls));
        }
        enclosing.superclass.clone()
    }

    fn new() -> Self {
        Self {
            ext: Default::default(),
            call_result: None,
            catch_result: None,
            enclosing: None,
        }
    }

    fn process(
        &mut self,
        parser: &ClassFileParser,
        artifact_key: ArtifactKey,
        builders: &mut Builders,
    ) -> Result<(), Error> {
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
            if let Some(jc) = superclass_opt.clone() {
                iface_vec.push(jc)
            };
            let mut direct_interfaces = Vec::new();
            for type_idx in &class_def.interfaces {
                let iface = jvm_class_symbol(parser.get_class_name(*type_idx).ok().unwrap());
                iface_vec.push(JavaClass(iface.clone().into()));
                direct_interfaces.push(Symbol::from(iface.as_str()));
                log::trace!("Interface: {}", iface);
            }

            if let VirtualMethodTable::Java {
                hierarchy,
                interfaces,
                ..
            } = &mut builders.vmt
            {
                hierarchy.insert(JavaClass(class_name.to_string().into()), iface_vec);
                if class_def.access_flags & JVM_ACC_INTERFACE != 0 {
                    interfaces.push(JavaClass(class_name.to_string().into()));
                }
            }
            self.enclosing = Some(EnclosingClass {
                name: Symbol::from(class_name.as_str()),
                superclass: superclass_opt.map(|c| c.0),
                interfaces: direct_interfaces,
            });
            for enc in parser.methods() {
                let sig = parser.method_signature(enc)?;
                let method_name = parser.method_name(enc)?;
                let fidx = builders.program.new_function();
                let fdat = &mut builders.program[fidx];

                // ---------------------------------------------------------------------
                // Collect Java virtual method information for the VMT. `java_sig` is the
                // descriptor alone, e.g. "(I)I"; `full_name` is `class->name(descriptor)`.
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
                if enc.access_flags & JVM_ACC_ABSTRACT != 0
                    && let VirtualMethodTable::Java {
                        abstract_methods, ..
                    } = &mut builders.vmt
                {
                    abstract_methods.push((
                        JavaClass(class_name.to_string().into()),
                        JavaSimpleName(method_name.clone().into()),
                        JavaSignature(java_sig.clone().into()),
                    ));
                }
                // Native methods are also listed in `natives`. They are already in `methods`
                // above: this frontend walks every declared method, code or not.
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
                        // A method jvm-reader cannot decode keeps its declaration, so calls
                        // to it still resolve, but gets no body. Failing here would fail the
                        // import of every other method in the class or JAR.
                        let basic_blocks = match parser.basic_blocks_with_stack_slots(enc) {
                            Ok(blocks) => blocks.expect("Non-empty function"),
                            Err(e) => {
                                log::warn!("{full_name}: importing without a body: {e}");
                                continue;
                            }
                        };

                        let handler_pcs: HashSet<u32> = code_attr
                            .exception_table
                            .iter()
                            .map(|e| e.handler_pc as u32)
                            .collect();

                        let mut dup_slot_pairs: HashSet<DupSlotPair> = HashSet::new();

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
                            for instr in block_instrs.iter() {
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
                                        // A long or double result takes two slots, and each
                                        // carries the value, as a `lload` writes both.
                                        for ret_loc in Self::call_result_slots(call_info) {
                                            let dest = self.convert_location_to_var_ref(
                                                &ret_loc,
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
                                            bb_data.push_back(assign);
                                        }
                                        if let Some(mut link) = Self::init_survivor_link(
                                            call_info,
                                            &dup_slot_pairs,
                                            &mut fdat.locals,
                                        ) {
                                            link.source_info = source_info;
                                            bb_data.push_back(link);
                                        }
                                    }
                                }
                                let dup_stmts = if Self::is_stack_shuffle_opcode(instr.opcode) {
                                    Some(self.stack_shuffle_statements(
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
                                        bb_data.push_back(stmt);
                                    }
                                } else {
                                    for df in &instr.dataflow {
                                        for mut stmt in self.dataflow_to_statements(
                                            instr.opcode,
                                            df,
                                            &mut fdat.locals,
                                        ) {
                                            stmt.source_info = source_info;
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
                                                instr
                                                    .return_value
                                                    .as_ref()
                                                    .expect("a value return names its value"),
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
                    log::trace!("adding external method: {}", entry.3);
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
                    log::trace!("skipping defined method: {}", entry.3);
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
                        // I don't know why dynamic calls with no receiver exist, but apparently makeConcatWithConstants does this
                        let class_name = "?unknown";
                        let method_name = call.dynamic_name.as_ref().unwrap();
                        let descr = call.dynamic_type.as_ref().unwrap();
                        let java_sig = "L".to_owned() + class_name + ";->" + method_name + descr;
                        let out_params = jvm_descriptor_to_params(descr, false);
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
                            // `invokedynamic` has no dispatch instruction to read: where it
                            // goes is decided by a bootstrap method at run time. Recorded as
                            // unknown rather than guessed at.
                            dispatch: JavaDispatch::Unknown,
                            super_start: None,
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
                            dispatch: match call.call_kind {
                                CallKind::Interface => JavaDispatch::Interface,
                                // `invokespecial` with a receiver: a `super.m()`, a private
                                // method, or a constructor. All three are fixed at the named
                                // class rather than found from the receiver.
                                CallKind::Special => JavaDispatch::Super,
                                CallKind::Virtual => JavaDispatch::Virtual,
                                // Unreachable: `Dynamic` is the arm above and `Static` has no
                                // receiver. Recorded honestly rather than asserted, since a
                                // new `CallKind` should not make this arm lie.
                                CallKind::Dynamic | CallKind::Static => JavaDispatch::Unknown,
                            },
                            super_start: match call.call_kind {
                                CallKind::Special => self.super_start(&class_name, method_name),
                                _ => None,
                            },
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

    /// The stack slots a call's result lands in: none for `void`, the lower
    /// and upper slot for a long or double.
    fn call_result_slots(call: &CallInfo) -> Vec<Location> {
        let Some(Location::StackSlot(n)) = call.return_value else {
            return call.return_value.iter().cloned().collect();
        };
        let descriptor = match call.call_kind {
            CallKind::Dynamic => call.dynamic_type.as_deref(),
            _ => call.target.as_ref().map(|t| t.descriptor.as_str()),
        };
        let slots = descriptor.map_or(1, jvm_reader::flow::descriptor_return_slot_count);
        (0..slots.max(1) as u32)
            .map(|i| Location::StackSlot(n + i))
            .collect()
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

    /// Stack-manipulation opcodes: `dup*` and `swap` (JVMS 6.5).
    fn is_stack_shuffle_opcode(opcode: u8) -> bool {
        matches!(opcode, 0x59..=0x5f)
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

    /// Record each pair of outputs that are copies of one value: they have the
    /// same sources.
    fn record_dup_slot_pairs(pairs: &mut HashSet<DupSlotPair>, dataflow: &[DataflowInfo]) {
        for (i, a) in dataflow.iter().enumerate() {
            for b in &dataflow[i + 1..] {
                if a.sources != b.sources {
                    continue;
                }
                if let (Some(x), Some(y)) = (
                    Self::stack_slot_index(&a.destination),
                    Self::stack_slot_index(&b.destination),
                ) {
                    Self::record_dup_slot_pair(pairs, x, y);
                }
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

    /// Lower `dup*` and `swap`. Every source is read before the instruction, but
    /// the outputs land on the input slots (`dup_x1` puts `v1` where `v2` was), so
    /// a source slot that an output overwrites is first copied to a temporary.
    fn stack_shuffle_statements(
        &self,
        dataflow: &[DataflowInfo],
        locals: &mut Locals,
    ) -> SmallVec<[Statement; 8]> {
        let moves: SmallVec<[&DataflowInfo; 6]> = dataflow
            .iter()
            .filter(|df| df.sources.as_slice() != std::slice::from_ref(&df.destination))
            .collect();
        let overwritten: SmallVec<[u32; 6]> = moves
            .iter()
            .filter_map(|df| Self::stack_slot_index(&df.destination))
            .collect();
        let mut stmts = SmallVec::new();
        let mut saved = SmallVec::<[u32; 6]>::new();
        for df in &moves {
            for n in df.sources.iter().filter_map(Self::stack_slot_index) {
                if overwritten.contains(&n) && !saved.contains(&n) {
                    saved.push(n);
                    stmts.push(Self::assign_var(
                        Self::shuffle_temp(n, locals),
                        self.convert_location_to_var_ref(&Location::StackSlot(n), locals),
                    ));
                }
            }
        }
        for df in moves {
            let sources = df
                .sources
                .iter()
                .map(|loc| match Self::stack_slot_index(loc) {
                    Some(n) if saved.contains(&n) => {
                        Exp::from(AccessPath::without_fields(Self::shuffle_temp(n, locals)))
                    }
                    _ => self.convert_location_to_exp(loc, locals),
                })
                .collect();
            stmts.push(Statement::new_kind(StatementKind::Assign {
                dest: self.convert_location_to_var_ref(&df.destination, locals),
                sources,
            }));
        }
        stmts
    }

    /// The temporary [`Self::stack_shuffle_statements`] saves stack slot `n` in.
    fn shuffle_temp(n: u32, locals: &mut Locals) -> VariableRef {
        VariableRef::new_local_idx(locals.get_or_intern(&format!("shuffle{n}")))
    }

    fn dataflow_to_statements(
        &mut self,
        opcode: u8,
        data: &DataflowInfo,
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
                let object = self.convert_location_to_var_ref(
                    Self::stack_sources(&data.sources)
                        .next()
                        .expect("getfield object"),
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
                // The value is on top (one slot, or two for long/double), the object below it.
                let mut stacks = Self::stack_sources(&data.sources);
                let value = self.stack_exp(stacks.next().expect("putfield value"), locals);
                let object_slot = stacks.last().expect("putfield object");
                let object = self.convert_location_to_var_ref(object_slot, locals);
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
                let object = self.convert_location_to_var_ref(base, locals);
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
                let object = self.convert_location_to_var_ref(base, locals);
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
            // As ctadl-dex models const-method-handle and const-method-type.
            Location::Constant(ConstantValue::MethodHandle { .. }) => {
                Self::allocation_exp("java/lang/invoke/MethodHandle")
            }
            Location::Constant(ConstantValue::MethodType(_)) => {
                Self::allocation_exp("java/lang/invoke/MethodType")
            }
            Location::Constant(ConstantValue::Dynamic { descriptor, .. })
                if descriptor.starts_with('L') || descriptor.starts_with('[') =>
            {
                Self::allocation_exp(descriptor)
            }
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
