/*! Dex language frontend

Converts dex/apk into CTADL IR.
*/

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;
use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactKey, SourceInfoBuilder, SpanLen};
use streaming_iterator::StreamingIterator; // needed for DataFlow.dest.owned()

use crate::error::{Error, ErrorContext};
use ctadl_ir::mir::call::{
    CallObject, JavaClass, JavaMethod, JavaSignature, JavaSimpleName, VirtualMethodTable,
};
use ctadl_ir::*;
use dex_reader::basic_blocks::{basic_blocks, block_successors};
use dex_reader::error::DexError;
use dex_reader::instructions::{DataFlow, Instruction, Reg};
use dex_reader::parser::{DecodedCodeItem, decode_code_item};
use dex_reader::types::{ACC_NATIVE, ACC_STATIC, CodeItem, MethodId};
use dex_reader::{APKParser, DexParser};

#[cfg(test)]
mod tests;

pub fn import_apk<P: AsRef<Path>>(file: P) -> Result<ProgramInfo, Error> {
    let file = file.as_ref();
    let data = read_file_bytes(file)?;
    let parser = APKParser::new(&data)?;
    let mut ctx = Context::new();
    let mut builders = Builders::new();

    for (sub_artifact_id, (dex_file_name, parser)) in
        parser.dex_parsers_with_filenames().into_iter().enumerate()
    {
        // This will not refer to a real file path, but a path "inside" the apk. But this is OK,
        // since for ArtifactEncoding::Binary, format won't read the file on this path.
        let key = ArtifactKey {
            path: file.join(dex_file_name).to_string_lossy().to_string(),
            sub_artifact_id: sub_artifact_id.try_into().unwrap(),
            hash: Vec::new(),
            encoding: source_info::ArtifactEncoding::Binary,
        };
        ctx.process(&parser, key, &mut builders)?;
    }
    ctx.finish(builders)
}

pub fn import_dex<P: AsRef<Path>>(file: P) -> Result<ProgramInfo, Error> {
    let file = file.as_ref();
    let data = read_file_bytes(file)?;
    let parser = DexParser::new(&data)?;
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
    counter: Counter,
    call_result: Option<VariableRef>,
    catch_result: Option<VariableRef>,
    call_to: HashSet<MethodId>,
    defined: HashMap<String, FunctionIdx>,
    // vmt entries for externs so far
    ext: HashMap<String, (JavaClass, JavaSimpleName, JavaSignature, JavaMethod)>,
}

impl Context {
    /// The context manages translation from dex into CTADL IR.
    ///
    /// The expectation is to call `new`, then [`Context::process`] possibly many times, then
    /// [`Context::finish`] exactly once. The reason is that we have to handle both multiple dex
    /// files and open programs, programs that call functions for which we don't have a definition.
    ///
    /// During `process`, the context records methods that are "external," meaning we haven't so
    /// far seen a definition for them. We also stub them out in the program. Since we might see a
    /// definition for them during a later call to process, we also clear those entries when we
    /// process the defined methods. We record the VMT entry for such methods in the
    /// [`Context::ext`] member.
    ///
    /// During `finish`, any methods still-undefined are added to the VMT using what we recorded
    /// during processing.
    fn new() -> Self {
        Self {
            counter: Default::default(),
            call_result: Default::default(),
            catch_result: Default::default(),
            call_to: Default::default(),
            defined: Default::default(),
            ext: Default::default(),
        }
    }

    /// Process one dex file into the program. Stubs any methods that are called but not defined.
    ///
    /// The implementation is gnarly and could be cleaned up.
    fn process(
        &mut self,
        parser: &DexParser<'_>,
        artifact_key: ArtifactKey,
        builders: &mut Builders,
    ) -> Result<(), Error> {
        // Iterate over all classes.
        for class_def in parser.classes() {
            let class_name = parser.class_name(class_def)?;
            log::trace!("Class: {}", class_name);
            // Populate class hierarchy information for the VMT.
            // Immediate superclass (if any) and immediate super‑interfaces.
            let superclass_opt = if class_def.superclass_idx != 0 {
                parser
                    .get_type(class_def.superclass_idx as usize)
                    .and_then(|t| parser.type_descriptor(t).ok())
                    .map(|s| JavaClass(s.into()))
            } else {
                None
            };
            let iface_type_list = parser.class_interfaces(class_def)?;
            let mut iface_vec = Vec::new();
            for type_idx in iface_type_list.types {
                if let Some(t) = parser.get_type(type_idx as usize)
                    && let Ok(desc) = parser.type_descriptor(t)
                {
                    iface_vec.push(JavaClass(desc.into()));
                }
            }
            if let VirtualMethodTable::Java { hierarchy, .. } = &mut builders.vmt {
                let parents = hierarchy.entry(JavaClass(class_name.into())).or_default();
                for sup in superclass_opt.into_iter().chain(iface_vec) {
                    parents.push(sup);
                }
            }
            let class_data = parser.class_data(class_def)?;
            for enc in class_data
                .direct_methods
                .iter()
                .chain(class_data.virtual_methods.iter())
            {
                let mi = parser.get_method(enc.method_idx as usize).unwrap();
                // fully qualified signature with class name, simple name, and descriptor
                let sig = parser.method_signature(mi)?;
                // we have the definition
                self.ext.remove(&sig);
                // Reset temporaries per function.
                self.counter.reset();
                // We may have encountered this method as an "extern" in a previous dex file - if
                // so, we're parsing the definition, so add to the existing FunctionData.
                let fdat = if let Some(fidx) = self.defined.get(&sig) {
                    &mut builders.program[*fidx]
                } else {
                    let fidx = builders.program.new_function();
                    self.defined.insert(sig.clone(), fidx);
                    &mut builders.program[fidx]
                };
                fdat.name = sig.clone();
                // Handle instructions

                // Parse the instruction stream for the method.
                if let Some(code) = parser.method_code(enc)? {
                    // ---------------------------------------------------------------------
                    // Collect Java virtual method information for the VMT.
                    // The method triple has the class name, method simple name, and descriptor
                    let (class_name, method_name, method_descr) = parser.method_triple(mi)?;

                    if let VirtualMethodTable::Java { methods, .. } = &mut builders.vmt {
                        methods.push((
                            JavaClass(class_name.into()),
                            JavaSimpleName(method_name.into()),
                            JavaSignature(method_descr.into()),
                            JavaMethod(sig.into()),
                        ));
                    }
                    // ---------------------------------------------------------------------
                    // Handle parameter types
                    let param_types = parser.method_parameters(mi)?.types;
                    // Add "this" parameter as param 0
                    if !ACC_STATIC.is_set_in(enc.access_flags)
                        && !ACC_NATIVE.is_set_in(enc.access_flags)
                    {
                        fdat.params.push(ParameterType::ByRef);
                    }
                    for param_idx in param_types.into_iter() {
                        let ty = parser.get_type(param_idx.into()).unwrap();
                        let descr = parser.get_string(ty.descriptor_idx.try_into().unwrap())?;
                        // double-wide arguments take two registers
                        let reg_count = if descr.starts_with("J") || descr.starts_with("D") {
                            2
                        } else {
                            1
                        };
                        for _ in 0..reg_count {
                            fdat.params.push(if !descr.starts_with("L") {
                                ParameterType::ByVal
                            } else {
                                ParameterType::ByRef
                            });
                        }
                    }

                    let items = decode_code_item(&code);
                    // Compute Dex basic blocks.
                    let bb_vec = basic_blocks(&code, &items);

                    // Compute successors for each basic block using dex-reader.
                    let block_successors_vec = block_successors(&code, &items);

                    // Ensure MIR has a block for each Dex basic block.
                    for _ in &bb_vec {
                        fdat.blocks.blocks_mut().push(BasicBlockData::new(None));
                    }

                    let mut offset_to_bb: HashMap<usize, BasicBlockIdx> = HashMap::new();
                    for (i, bb) in bb_vec.iter().enumerate() {
                        let offset = items[bb.start].offset();
                        offset_to_bb.insert(offset, BasicBlockIdx::new(i));
                    }

                    // Parse exception handlers and map them to basic blocks
                    let exception_handlers =
                        parse_exception_handlers(&code, &bb_vec, &offset_to_bb);

                    // Track which blocks contain calls and need exception handling
                    let mut call_blocks = HashSet::new();

                    // All Java functions return 2 values: (normal_return, exception_return)
                    fdat.return_type = ReturnType { arity: 2 };

                    // Populate each MIR block.
                    for (i, bb) in bb_vec.iter().enumerate() {
                        let block_idx = BasicBlockIdx::new(i);
                        let range = &items[bb.start..bb.end];
                        for dci in range {
                            if let DecodedCodeItem::Instruction { inst, .. } = dci {
                                let item_offset = code.absolute_offset(dci);
                                let source_info =
                                    SourceInfo::new(builders.source_info_builder.span_for(
                                        artifact_key.clone(),
                                        item_offset.try_into().unwrap(),
                                        SpanLen::ByteLen(2),
                                    ));
                                if let Some(mut stmt) = self.decode_call(parser, &code, inst) {
                                    stmt.source_info = source_info;
                                    fdat.blocks[block_idx].push_back(stmt);
                                    // Mark this block as containing a call
                                    call_blocks.insert(block_idx);
                                } else {
                                    for mut stmt in self.dataflow_to_assign(parser, &code, inst)? {
                                        stmt.source_info = source_info;
                                        fdat.blocks[block_idx].push_back(stmt);
                                    }
                                }
                            }
                        }

                        // Determine the terminator for this block using a helper that re‑uses `control_flow_targets` logic.
                        let term = if let Some(last) = range.last() {
                            match last {
                                DecodedCodeItem::Instruction { inst, .. } => {
                                    // Return instructions end the function.
                                    match inst {
                                        // Throw instruction - jump to handlers if protected, else return exception
                                        Instruction::Throw(f) => {
                                            let succ_usizes = &block_successors_vec[i];
                                            let succs = succ_usizes
                                                .iter()
                                                .map(|&b| BasicBlockIdx::new(b))
                                                .collect::<SmallVec<[BasicBlockIdx; 4]>>();

                                            if succs.is_empty() {
                                                let throw_exp = Exp::new_access_path(
                                                    AccessPath::without_fields(reg_to_var(
                                                        &code, f.a,
                                                    )),
                                                );
                                                let empty_exp = Exp::new_bytes(Vec::new());
                                                TerminatorKind::Return {
                                                    args: smallvec![empty_exp, throw_exp],
                                                }
                                            } else {
                                                TerminatorKind::Goto { targets: succs }
                                            }
                                        }
                                        // Return with a value (register)
                                        Instruction::Return(reg)
                                        | Instruction::ReturnWide(reg)
                                        | Instruction::ReturnObject(reg) => {
                                            let ret_exp =
                                                Exp::new_access_path(AccessPath::without_fields(
                                                    reg_to_var(&code, reg.a),
                                                ));
                                            let empty_exp = Exp::new_bytes(Vec::new());
                                            TerminatorKind::Return {
                                                args: smallvec![ret_exp, empty_exp],
                                            }
                                        }
                                        // Void return
                                        Instruction::ReturnVoid(_) => {
                                            let empty_exp = Exp::new_bytes(Vec::new());
                                            TerminatorKind::Return {
                                                args: smallvec![empty_exp.clone(), empty_exp],
                                            }
                                        }
                                        // Compute successors for this block using the precalculated vector.
                                        _ => {
                                            let succ_usizes = &block_successors_vec[i];
                                            let mut succs = succ_usizes
                                                .iter()
                                                .map(|&b| BasicBlockIdx::new(b))
                                                .collect::<SmallVec<[BasicBlockIdx; 4]>>();

                                            // If this block contains a call and there are exception handlers,
                                            // add exception handlers as additional targets
                                            if call_blocks.contains(&block_idx)
                                                && !exception_handlers.is_empty()
                                            {
                                                // Add all exception handler blocks as potential targets
                                                for &handler_block in &exception_handlers {
                                                    if !succs.contains(&handler_block) {
                                                        succs.push(handler_block);
                                                    }
                                                }
                                            }

                                            if succs.is_empty() {
                                                // No successors - this block should return
                                                let empty_exp = Exp::new_bytes(Vec::new());
                                                TerminatorKind::Return {
                                                    args: smallvec![empty_exp.clone(), empty_exp],
                                                }
                                            } else {
                                                TerminatorKind::Goto { targets: succs }
                                            }
                                        }
                                    }
                                }
                                _ => {
                                    // No instruction in block - create a return
                                    let empty_exp = Exp::new_bytes(Vec::new());
                                    TerminatorKind::Return {
                                        args: smallvec![empty_exp.clone(), empty_exp],
                                    }
                                }
                            }
                        } else {
                            // Empty block – create a return.
                            let empty_exp = Exp::new_bytes(Vec::new());
                            TerminatorKind::Return {
                                args: smallvec![empty_exp.clone(), empty_exp],
                            }
                        };
                        fdat.blocks[block_idx].terminator = Some(Terminator::new_kind(term));
                    }
                } else {
                    // Method without code – create a single empty block that returns.
                    fdat.return_type = ReturnType { arity: 2 };
                    let empty_exp = Exp::new_bytes(Vec::new());
                    fdat.blocks
                        .blocks_mut()
                        .push(BasicBlockData::new(Some(Terminator::new_kind(
                            TerminatorKind::Return {
                                args: smallvec![empty_exp.clone(), empty_exp],
                            },
                        ))));
                }
            }
        }

        // define extern funcs and parameters/return value
        {
            let funcs: HashSet<String> = builders
                .program
                .functions
                .iter()
                .map(|f| f.name.clone())
                .collect();
            let mut call_to: Vec<_> = self.call_to.iter().cloned().collect();
            // Clear so that multiple calls to 'process' don't work on old data
            self.call_to.clear();
            call_to.sort();
            for extern_id in call_to.drain(..) {
                let full_sig = parser
                    .method_signature(&extern_id)
                    .err_context(|| format!("method_signature: {extern_id:?}"))?;
                if funcs.contains(&full_sig) {
                    continue;
                }
                if self.defined.contains_key(&full_sig) {
                    continue;
                }
                let fidx = builders.program.new_function();
                self.defined.insert(full_sig.clone(), fidx);
                let fdat = &mut builders.program.functions[fidx];
                let java_sig = parser.method_signature(&extern_id)?;
                fdat.name = java_sig.clone();
                for ty in parser.method_parameters(&extern_id)?.types {
                    let ty = parser.get_type(ty.into()).unwrap();
                    let descr = parser.get_string(ty.descriptor_idx.try_into().unwrap())?;
                    fdat.params.push(if !descr.starts_with("L") {
                        ParameterType::ByVal
                    } else {
                        ParameterType::ByRef
                    });
                }
                // All functions return 2 values: (normal_return, exception_return)
                fdat.return_type = ReturnType { arity: 2 };
                let (class_name, method_name, descr) = parser
                    .method_triple(&extern_id)
                    .err_context(|| format!("method_triple: {extern_id:?}"))?;
                self.ext.insert(
                    java_sig.clone(),
                    (
                        JavaClass(class_name.into()),
                        JavaSimpleName(method_name.into()),
                        JavaSignature(descr.into()),
                        JavaMethod(java_sig.into()),
                    ),
                );
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

    /// Decode a call instruction into a `StatementKind::CallAssign`.
    /// Returns `None` for non‑call instructions.
    /// The return value is a single element which can be used to process subsequent 'move-result'
    /// type instructions. It is stored into `self.call_result`
    fn decode_call(
        &mut self,
        parser: &DexParser<'_>,
        code: &CodeItem,
        inst: &Instruction,
    ) -> Option<Statement> {
        // If this instruction is not a call, bail out.
        let args_regs = inst.call_args()?; // returns &[Reg]

        // Resolve the method index from the specific invoke variant.
        let (method_idx, is_static) = match inst {
            Instruction::InvokeVirtual(fmt) => (fmt.idx, false),
            Instruction::InvokeSuper(fmt) => (fmt.idx, false),
            Instruction::InvokeDirect(fmt) => (fmt.idx, true),
            Instruction::InvokeStatic(fmt) => (fmt.idx, true),
            Instruction::InvokeInterface(fmt) => (fmt.idx, false),
            Instruction::InvokeVirtualRange(fmt) => (fmt.idx, false),
            Instruction::InvokeSuperRange(fmt) => (fmt.idx, false),
            Instruction::InvokeDirectRange(fmt) => (fmt.idx, true),
            Instruction::InvokeStaticRange(fmt) => (fmt.idx, true),
            Instruction::InvokeInterfaceRange(fmt) => (fmt.idx, false),
            _ => return None,
        };

        // Resolve the callee name (human‑readable signature).
        let (cls, simple_name, descriptor) = {
            let mi = parser.get_method(method_idx.0 as usize)?;
            self.call_to.insert(*mi);
            parser.method_triple(mi).ok()?
        };
        let method_id = {
            let mi = parser.get_method(method_idx.0 as usize)?;
            parser.method_signature(mi).ok()?
        };

        // Convert argument registers into IR expressions.
        let args: SmallVec<[Exp; 4]> = args_regs
            .iter()
            .map(|reg| AccessPath::without_fields(reg_to_var(code, *reg)).into())
            .collect();
        let style = if is_static {
            CallStyle::DirectCall {
                call_edges: CallEdges::Explicit([method_id].into_iter().collect()),
            }
        } else {
            CallStyle::JavaCall {
                receiver: args[0].access_path().unwrap().variable_ref.clone(),
                cls: cls.into(),
                simple_name: simple_name.into(),
                descriptor: descriptor.into(),
            }
        };

        // Dex returns into a special register, so just create a temporary.
        let retval = Context::ret();
        let throwval = Context::except();
        self.call_result = Some(retval.clone());
        self.catch_result = Some(throwval.clone());
        Some(Statement::new_kind(StatementKind::CallAssign {
            style,
            rets: smallvec![retval, throwval],
            args: if is_static {
                args
            } else {
                args.into_iter().skip(1).collect()
            },
        }))
    }

    fn dataflow_to_assign(
        &mut self,
        parser: &DexParser<'_>,
        code_item: &CodeItem,
        inst: &Instruction,
    ) -> Result<Vec<Statement>, DexError> {
        let DataFlow {
            source,
            dest,
            ret: _,
        } = inst.data_flow();
        let mut stmts: Vec<Statement> = Vec::new();

        // We have to iterate over dest multiple times, sadly, so we need to store the dest items.
        let dest: SmallVec<[Reg; 4]> = dest.owned().collect();

        if let Some(const_exp) = match inst {
            // 8‑bit signed constant
            Instruction::Const4(f) => {
                let value = f.lit;
                Some(Exp::new_bytes(value.to_be_bytes().to_vec()))
            }
            // 16‑bit signed constant
            Instruction::Const16(f) => {
                let value = f.lit;
                Some(Exp::new_bytes(value.to_be_bytes().to_vec()))
            }
            // 32‑bit signed constant
            Instruction::Const(f) => {
                let value = f.lit;
                Some(Exp::new_bytes(value.to_be_bytes().to_vec()))
            }
            // high 16 bits of a 32‑bit constant
            Instruction::ConstHigh16(f) => {
                let value = ((f.lit as i64) << 16) & 0xFFFF_FFFF;
                Some(Exp::new_bytes(value.to_be_bytes().to_vec()))
            }
            // 16‑bit wide constant (lower half)
            Instruction::ConstWide16(f) => {
                let value = f.lit;
                Some(Exp::new_bytes(value.to_be_bytes().to_vec()))
            }
            // 32‑bit wide constant (lower half)
            Instruction::ConstWide32(f) => {
                let value = f.lit;
                Some(Exp::new_bytes(value.to_be_bytes().to_vec()))
            }
            // full 64‑bit constant
            Instruction::ConstWide(f) => {
                let value = f.lit;
                Some(Exp::new_bytes(value.to_be_bytes().to_vec()))
            }
            // high 16 bits of a 64‑bit constant
            Instruction::ConstWideHigh16(f) => {
                let value = (f.lit as i64) << 48;
                Some(Exp::new_bytes(value.to_be_bytes().to_vec()))
            }
            // String constant (regular)
            Instruction::ConstString(f) => {
                if let Ok(s) = parser.constant_pool().strings.get(f.idx.0 as usize) {
                    Some(Exp::new_str(&s))
                } else {
                    None
                }
            }
            Instruction::ConstStringJumbo(f) => {
                if let Ok(s) = parser.constant_pool().strings.get(f.idx.0 as usize) {
                    Some(Exp::new_str(&s))
                } else {
                    None
                }
            }
            // Class constant – resolved to its descriptor string
            Instruction::ConstClass(f) => {
                if let Some(tid) = parser.constant_pool().type_ids.get(f.idx.0 as usize)
                    && let Ok(desc) = tid.descriptor(&parser.constant_pool().strings)
                {
                    Some(Exp::new_str(&desc))
                } else {
                    None
                }
            }
            // NewInstance – resolved to a JavaObject reference
            Instruction::NewInstance(f) => {
                if let Some(tid) = parser.constant_pool().type_ids.get(f.idx.0 as usize)
                    && let Ok(desc) = tid.descriptor(&parser.constant_pool().strings)
                {
                    Some(Exp::new_object_ref(CallObject::JavaObject(JavaClass(
                        desc.into(),
                    ))))
                } else {
                    None
                }
            }
            _ => None,
        } {
            for d in dest {
                stmts.push(Statement::new_kind(StatementKind::assign(
                    reg_to_var(code_item, d),
                    [const_exp.clone()],
                )));
            }
            return Ok(stmts);
        }

        match inst {
            Instruction::ArrayLength(f) => {
                let src = Exp::AccessPath(AccessPath {
                    variable_ref: reg_to_var(code_item, f.b),
                    path: ["length"].into_iter().collect(),
                });
                let dest = reg_to_var(code_item, f.a);
                let sources = smallvec![src];
                stmts.push(Statement::new_kind(StatementKind::Assign { dest, sources }));
                return Ok(stmts);
            }
            Instruction::SPut(f)
            | Instruction::SPutObject(f)
            | Instruction::SPutByte(f)
            | Instruction::SPutChar(f)
            | Instruction::SPutBoolean(f)
            | Instruction::SPutShort(f)
            | Instruction::SPutWide(f) => {
                let fld = parser.get_field(f.idx.0 as usize).unwrap();
                let name = format!("<{}>", fld.pretty_name(parser.constant_pool())?);
                let temp_var = self.counter.temp();
                // flow sources to temp
                stmts.push(Statement::new_kind(StatementKind::assign(
                    temp_var.clone(),
                    source.cloned().map(|r| reg_to_var(code_item, r).into()),
                )));
                // flow temp into field update
                stmts.push(Statement::new_kind(StatementKind::update(
                    AccessPath::new(
                        VariableRef::new_global(),
                        [mir::FieldAccess::Symbol(name.into())],
                    ),
                    temp_var.into(),
                )));
                return Ok(stmts);
            }
            Instruction::SGet(f)
            | Instruction::SGetObject(f)
            | Instruction::SGetByte(f)
            | Instruction::SGetChar(f)
            | Instruction::SGetBoolean(f)
            | Instruction::SGetShort(f)
            | Instruction::SGetWide(f) => {
                let fld = parser.get_field(f.idx.0 as usize).unwrap();
                let name = format!("<{}>", fld.pretty_name(parser.constant_pool())?);
                for dest in dest.iter().cloned().map(|d| reg_to_var(code_item, d)) {
                    let source = AccessPath::new(
                        VariableRef::new_global(),
                        [mir::FieldAccess::Symbol(name.clone().into())],
                    )
                    .into();
                    stmts.push(Statement::new_kind(StatementKind::assign(dest, [source])));
                }
                return Ok(stmts);
            }
            Instruction::IPut(f)
            | Instruction::IPutBoolean(f)
            | Instruction::IPutByte(f)
            | Instruction::IPutChar(f)
            | Instruction::IPutShort(f)
            | Instruction::IPutObject(f)
            | Instruction::IPutWide(f) => {
                let temp_var = self.counter.temp();
                // flow sources to temp
                stmts.push(Statement::new_kind(StatementKind::assign(
                    temp_var.clone(),
                    source
                        .cloned()
                        .filter(|r| *r != f.b)
                        .map(|r| reg_to_var(code_item, r).into()),
                )));
                let object = reg_to_var(code_item, f.b);
                let fld = parser.get_field(f.idx.0 as usize).unwrap();
                let name = format!("<{}>", fld.pretty_name(parser.constant_pool())?);
                // flow temp into field update
                stmts.push(Statement::new_kind(StatementKind::update(
                    AccessPath::new(object, [mir::FieldAccess::Symbol(name.into())]),
                    temp_var.into(),
                )));
                return Ok(stmts);
            }
            Instruction::IGet(f)
            | Instruction::IGetBoolean(f)
            | Instruction::IGetByte(f)
            | Instruction::IGetChar(f)
            | Instruction::IGetShort(f)
            | Instruction::IGetObject(f)
            | Instruction::IGetWide(f) => {
                let object = reg_to_var(code_item, f.b);
                let fld = parser.get_field(f.idx.0 as usize).unwrap();
                let name = format!("<{}>", fld.pretty_name(parser.constant_pool())?);
                for dest in dest.iter().cloned().map(|d| reg_to_var(code_item, d)) {
                    let source = AccessPath::new(
                        object.clone(),
                        [mir::FieldAccess::Symbol(name.clone().into())],
                    )
                    .into();
                    stmts.push(Statement::new_kind(StatementKind::assign(dest, [source])));
                }
                return Ok(stmts);
            }
            Instruction::AGet(f)
            | Instruction::AGetBoolean(f)
            | Instruction::AGetByte(f)
            | Instruction::AGetChar(f)
            | Instruction::AGetShort(f)
            | Instruction::AGetObject(f)
            | Instruction::AGetWide(f) => {
                let array_var = reg_to_var(code_item, f.b);
                for d in dest.iter().cloned() {
                    let dest_var = reg_to_var(code_item, d);
                    let source =
                        AccessPath::new(array_var.clone(), [mir::FieldAccess::Symbol("[]".into())])
                            .into();
                    stmts.push(Statement::new_kind(StatementKind::assign(
                        dest_var,
                        [source],
                    )));
                }
                return Ok(stmts);
            }
            Instruction::APut(f)
            | Instruction::APutBoolean(f)
            | Instruction::APutByte(f)
            | Instruction::APutChar(f)
            | Instruction::APutShort(f)
            | Instruction::APutObject(f)
            | Instruction::APutWide(f) => {
                let temp_var = self.counter.temp();
                stmts.push(Statement::new_kind(StatementKind::assign(
                    temp_var.clone(),
                    source
                        .cloned()
                        .filter(|r| *r != f.b && *r != f.c)
                        .map(|r| reg_to_var(code_item, r).into()),
                )));
                let array_var = reg_to_var(code_item, f.b);
                let dest_path = AccessPath::new(array_var, [mir::FieldAccess::Symbol("[]".into())]);
                stmts.push(Statement::new_kind(StatementKind::update(
                    dest_path,
                    temp_var.into(),
                )));
                return Ok(stmts);
            }
            Instruction::Throw(f) => {
                let src_var = reg_to_var(code_item, f.a);
                stmts.push(Statement::new_kind(StatementKind::assign(
                    Context::except(),
                    [src_var.into()],
                )));
                return Ok(stmts);
            }
            _ => {
                // Flows all the sources to each dest
                let sources: Vec<_> = source.copied().collect();
                if !sources.is_empty() {
                    for dest in dest.iter() {
                        let dst_var = reg_to_var(code_item, *dest);
                        stmts.push(Statement::new_kind(StatementKind::assign(
                            dst_var,
                            sources.iter().map(|s| reg_to_var(code_item, *s).into()),
                        )));
                    }
                }
            }
        }

        // Handles moving results of calls into registers
        let is_move_result = inst.is_move_result();
        let is_move_exception = matches!(inst, Instruction::MoveException(_));

        let call_res = std::mem::take(&mut self.call_result);
        let catch_res = std::mem::take(&mut self.catch_result);

        if is_move_result {
            let result = call_res.unwrap_or_else(Context::ret);
            for d_reg in dest.iter().cloned() {
                let src_exp = result.clone().into();
                let dst_var = reg_to_var(code_item, d_reg);
                stmts.push(Statement::new_kind(StatementKind::assign(
                    dst_var,
                    [src_exp],
                )));
            }
        }

        if is_move_exception {
            let result = catch_res.unwrap_or_else(Context::except);
            for d_reg in dest.iter().cloned() {
                let src_exp = result.clone().into();
                let dst_var = reg_to_var(code_item, d_reg);
                stmts.push(Statement::new_kind(StatementKind::assign(
                    dst_var,
                    [src_exp],
                )));
            }
        }

        Ok(stmts)
    }

    /// Returns 'ret' temporary for move-result
    fn ret() -> VariableRef {
        VariableRef::new_local("retval".to_string())
    }

    fn except() -> VariableRef {
        VariableRef::new_local("throwval".to_string())
    }
}

fn reg_to_var(code_item: &CodeItem, reg: Reg) -> VariableRef {
    if let Some(pidx) = code_item.reg_to_p(reg.0.try_into().expect("reg too big")) {
        VariableRef::new_parameter(pidx.into())
    } else {
        VariableRef::new_local(format!("v{}", reg.0))
    }
}

/// Parse exception handlers from Dex code and map them to basic blocks
fn parse_exception_handlers(
    code: &CodeItem,
    _bb_vec: &[dex_reader::basic_blocks::BasicBlock],
    offset_to_bb: &HashMap<usize, BasicBlockIdx>,
) -> Vec<BasicBlockIdx> {
    let mut handlers = Vec::new();

    if let Some(handler_list) = &code.handlers {
        // Process each try-catch block
        for try_item in &code.tries {
            if let Some(handler) = handler_list.get_by_off(try_item.handler_off) {
                // Add all handler addresses for this try block
                for pair in &handler.pairs {
                    if let Some(&handler_block) = offset_to_bb.get(&(pair.addr as usize))
                        && !handlers.contains(&handler_block)
                    {
                        handlers.push(handler_block);
                    }
                }
                // Add catch-all handler if present
                if let Some(catch_all_addr) = handler.catch_all_addr
                    && let Some(&handler_block) = offset_to_bb.get(&(catch_all_addr as usize))
                    && !handlers.contains(&handler_block)
                {
                    handlers.push(handler_block);
                }
            }
        }
    }

    handlers
}

fn read_file_bytes<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    let path = path.as_ref();
    let mut f = File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

#[derive(Debug, Clone, Default)]
struct Counter {
    value: u32,
}

impl Counter {
    #[inline]
    fn reset(&mut self) {
        self.value = Default::default()
    }

    #[inline]
    fn next(&mut self) -> u32 {
        let v = self.value;
        self.value += 1u32;
        v
    }

    /// Make a new temporary with 't' prefix
    fn temp(&mut self) -> VariableRef {
        let i = self.next();
        VariableRef::new_local(format!("t{i}"))
    }
}
