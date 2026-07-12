//! Ghidra Pcode language frontend
//!
//! Converts Ghidra pcode facts into CTADL IR.

use std::ops::Deref;
use std::path::Path;

use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactKey, SourceInfoBuilder};
use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, ErrorContext};
use ctadl_ir::mir::call::{NativeFunction, NativeSignature, NativeSimpleName, VirtualMethodTable};
use ctadl_ir::*;

use pcode_reader::PcodeFactsReader;

mod ghidra;

/// This is hardcoded for now, but should be read from the facts
const WORD_SIZE: i64 = 8;

/// Import pcode facts from an artifact by running Ghidra and then converting the facts
pub fn import_pcode(import: &crate::project::ArtifactImport) -> Result<ProgramInfo, Error> {
    let path = &import.artifact_path;
    let import_path = &import.import_path;

    // Run Ghidra to generate facts
    ghidra::run_ghidra_export(path, import_path)?;

    let facts_dir = import_path.join("facts");

    // Persist Ghidra's image base on the import config so downstream consumers
    // (SARIF address mapping, regression line checks) can recover
    // section-relative offsets regardless of the base Ghidra chose.
    if let Some(image_base) = PcodeFactsReader::new(&facts_dir)
        .read_image_base()
        .map_err(|e| Error::PcodeFactRead(format!("Failed to read image base: {}", e)))?
    {
        let mut updated = import.clone();
        updated.image_base = Some(image_base);
        updated.save()?;
    }

    let mut ctx = Context::new();
    let mut builders = Builders::new();

    let key = ArtifactKey {
        path: path.to_string_lossy().to_string(),
        sub_artifact_id: 0,
        hash: Vec::new(),
        encoding: source_info::ArtifactEncoding::Binary,
    };

    ctx.process(&facts_dir, key, &mut builders)?;
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
            vmt: VirtualMethodTable::new_native(),
            source_info_builder: SourceInfoBuilder::new(artifact_metadata),
        }
    }
}

enum VnodeRep {
    Const(i64),
    Var(VariableRef),
    Offset(VariableRef, i64),
    /// A named stack-storage varnode at frame offset `F`, expressed as the canonical
    /// stack memory `__stack_top.[F].deref` so it unifies with stack-address derefs.
    StackSlot(i64),
    /// Storage for a global variable living at address `A`, expressed as the canonical
    /// global memory `$globals.[A].deref`.
    Global(i64),
}

#[derive(Debug)]
struct Context {
    // Function mapping: pcode function ID -> CTADL function index
    functions: BTreeMap<pcode_reader::HighFunc, FunctionIdx>,
    // Basic block mapping: (function ID, pcode block ID) -> CTADL basic block index
    basic_blocks: BTreeMap<(pcode_reader::HighFunc, pcode_reader::PcodeBlockBasic), BasicBlockIdx>,
    // Basic block facts for function lookup
    bb_facts: BTreeMap<pcode_reader::PcodeBlockBasic, pcode_reader::BBData>,
    // Mapping from instruction address to basic block
    address_to_bb: BTreeMap<i64, (pcode_reader::HighFunc, BasicBlockIdx)>,
    // Constant propagation results
    cp_results:
        BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::constant_propagation::SymbolicProp>,
    // Stack pointer register name
    sp_name: Option<String>,
    // Varnodes that Ghidra classifies as storage for a global variable
    global_vnodes: BTreeSet<pcode_reader::PcodeVarnode>,
    // Register facts needed for vnode conversion
    register_facts: Vec<pcode_reader::RegisterData>,
    // Current function being processed
    current_hfunc: Option<pcode_reader::HighFunc>,
    counter: i64,
}

impl Context {
    fn new() -> Self {
        Self {
            functions: Default::default(),
            basic_blocks: Default::default(),
            bb_facts: Default::default(),
            address_to_bb: Default::default(),
            cp_results: Default::default(),
            sp_name: None,
            global_vnodes: Default::default(),
            register_facts: Default::default(),
            current_hfunc: None,
            counter: 0,
        }
    }

    /// Appends a write of `src` to the access path `dest`: a plain assign for a bare variable,
    /// otherwise a store of the (non-empty) field path.
    fn push_assign_or_store(&mut self, stmts: &mut Vec<Statement>, dest: AccessPath, src: Exp) {
        stmts.push(Statement::new_kind(StatementKind::assign_or_store(
            dest, src,
        )));
    }


    fn process(
        &mut self,
        facts_dir: &Path,
        artifact_key: ArtifactKey,
        builders: &mut Builders,
    ) -> Result<(), Error> {
        // Use pcode-reader crate to read facts
        let reader = PcodeFactsReader::new(facts_dir);
        let mut pcode_facts = reader
            .read_all_facts()
            .map_err(|e| Error::PcodeFactRead(format!("Failed to read pcode facts: {}", e)))?;

        // Synthesize stack top varnode and add it to facts
        let stack_top_vn = pcode_reader::PcodeVarnode::from("__stack_top");
        let (sp_name, sp_size) = pcode_facts
            .register_facts
            .iter()
            .find(|r| r.is_stack_pointer && r.size == WORD_SIZE)
            .map(|r| (r.name.clone(), Some(r.size)))
            .unwrap_or_else(|| ("stack".to_string(), Some(8)));
        self.sp_name = Some(sp_name.clone());

        pcode_facts.vnode_facts.insert(
            stack_top_vn.clone(),
            pcode_reader::VnodeData {
                name: sp_name,
                size: sp_size,
                is_address: false,
                space: Some("register".to_string()),
                address: None,
                constant_offset: None,
            },
        );

        // Run and store constant propagation results
        self.cp_results =
            pcode_reader::constant_propagation::compute_constant_propagation(&pcode_facts);
        self.global_vnodes = pcode_facts
            .vnode_facts
            .keys()
            .filter(|vn| pcode_facts.is_global_vnode(vn))
            .cloned()
            .collect();
        self.register_facts = pcode_facts.register_facts.clone();

        // Store bb_facts for later use
        self.bb_facts = pcode_facts.bb_facts.clone();

        // Pre-pass: Create mapping from function names to HighFunc IDs for functions with pcode
        let name_to_func_map = self.create_name_to_func_mapping(&pcode_facts);

        // 1. Process functions first (metadata only)
        self.process_functions(&pcode_facts, &name_to_func_map, builders)?;

        // 2. Process basic blocks and map parameters (function by function)
        self.process_all_blocks(&pcode_facts, builders)?;

        // 3. Process pcode instructions
        self.process_pcode_instructions(&pcode_facts, artifact_key, builders)?;

        Ok(())
    }

    /// Create a mapping from function names to HighFunc IDs for functions that have pcode instructions.
    /// Ensures that each name is mapped to at most one HighFunc ID.
    fn create_name_to_func_mapping(
        &self,
        pcode_facts: &pcode_reader::PcodeFacts,
    ) -> BTreeMap<String, pcode_reader::HighFunc> {
        let mut name_to_funcs: BTreeMap<String, Vec<pcode_reader::HighFunc>> = BTreeMap::new();

        let bbs_with_instructions: BTreeSet<&pcode_reader::PcodeBlockBasic> = pcode_facts
            .pcode_facts
            .values()
            .filter_map(|pcode| pcode.bb_id.as_ref())
            .collect();

        // Find all functions that have basic blocks with pcode instructions
        for (bb_id, bb_data) in &pcode_facts.bb_facts {
            let has_instructions = bbs_with_instructions.contains(bb_id);

            if has_instructions
                && let Some(func_data) = pcode_facts.hfunc_facts.get(&bb_data.hfunc)
                && !func_data.is_external
            {
                let entry = name_to_funcs.entry(func_data.name.clone()).or_default();
                if !entry.contains(&bb_data.hfunc) {
                    entry.push(bb_data.hfunc.clone());
                }
            }
        }

        let mut name_to_func_map = BTreeMap::new();
        for (name, funcs) in name_to_funcs {
            // Only if there is exactly one function with this name that has PCODE,
            // we can potentially use the "nice" name.
            if funcs.len() == 1 {
                name_to_func_map.insert(name, funcs[0].clone());
            }
        }

        name_to_func_map
    }

    fn process_functions(
        &mut self,
        pcode_facts: &pcode_reader::PcodeFacts,
        name_to_func_map: &BTreeMap<String, pcode_reader::HighFunc>,
        builders: &mut Builders,
    ) -> Result<(), Error> {
        let hfunc_facts = &pcode_facts.hfunc_facts;
        let proto_facts = &pcode_facts.proto_facts;

        // Sort function IDs for deterministic naming
        let mut sorted_func_ids: Vec<_> = hfunc_facts.keys().collect();
        sorted_func_ids.sort();

        let mut used_names = BTreeSet::new();

        for func_id in sorted_func_ids {
            let func_data = &hfunc_facts[func_id];

            // Determine base function name
            let base_name = if name_to_func_map.get(&func_data.name) == Some(func_id) {
                // This is the unique function with this name that has pcode
                func_data.name.clone()
            } else {
                // Collision or no pcode, use unique name by including ID
                // Shorten the name if it already contains the ID to avoid redundancy
                if func_id.contains(&func_data.name) {
                    func_id.to_string()
                } else {
                    format!("{}_{}", func_data.name, func_id)
                }
            };

            // Ensure uniqueness in the IR
            let mut func_name = base_name.clone();
            let mut counter = 1;
            while used_names.contains(&func_name) {
                func_name = format!("{}_{}", base_name, counter);
                counter += 1;
            }
            used_names.insert(func_name.clone());

            // The simple (un-decorated) name and a best-effort type signature for
            // the native VMT, so a JSON model's exact `names` list can match by
            // simple name even though `func_name` (the fully-qualified id) may be
            // decorated, e.g. Ghidra's `<EXTERNAL>::system@00101008`. Strip leading
            // underscores Ghidra sometimes emits (Mach-O prefixes every C symbol
            // with `_`) so the bare libc name matches without listing `_`-variants.
            let stripped = func_data.name.trim_start_matches('_');
            let simple_name = if stripped.is_empty() {
                func_data.name.as_str()
            } else {
                stripped
            };
            let signature = Self::format_native_signature(func_data, proto_facts);
            let fq_name = func_name.clone();

            // Create a new function
            let func_idx = builders.program.new_function();
            let func = &mut builders.program[func_idx];
            func.set_name(func_name);

            // Set return type and parameters from prototype if available
            if let Some(proto_id) = &func_data.proto {
                if let Some(proto_data) = proto_facts.get(proto_id) {
                    // Set return type based on prototype
                    let arity = if proto_data.is_void { 0 } else { 1 };
                    func.set_return_type(ReturnType { arity });

                    // Set parameters
                    for _ in 0..proto_data.parameters.len() {
                        func.params
                            .parameters
                            .push(ctadl_ir::mir::ParameterType::ByRef);
                    }
                }
            } else {
                // No prototype available, use default
                func.set_return_type(ReturnType { arity: 0 });
            }

            // Register in the native VMT (after the `func` borrow ends) so the
            // model matcher can resolve simple names to this function's id.
            if let VirtualMethodTable::Native { methods } = &mut builders.vmt {
                methods.push((
                    NativeSimpleName(simple_name.into()),
                    NativeSignature(signature.as_str().into()),
                    NativeFunction(fq_name.as_str().into()),
                ));
            }

            // Store function mapping
            self.functions.insert(func_id.clone(), func_idx);
        }
        Ok(())
    }

    /// Best-effort C-style signature string for the native VMT, e.g. `int(_, _)`.
    /// Parameter datatypes aren't currently exported by the pcode frontend, so
    /// each parameter renders as `_`; the arity, varargs flag, and return type
    /// are faithful. Used for display/disambiguation, not for matching (which
    /// keys off the simple name).
    fn format_native_signature(
        func_data: &pcode_reader::HFuncData,
        proto_facts: &BTreeMap<pcode_reader::HighProto, pcode_reader::ProtoData>,
    ) -> String {
        let Some(proto) = func_data.proto.as_ref().and_then(|p| proto_facts.get(p)) else {
            return "()".to_string();
        };
        let mut params: Vec<&str> = vec!["_"; proto.parameters.len()];
        if proto.is_vararg {
            params.push("...");
        }
        let ret = proto
            .return_type
            .as_deref()
            .unwrap_or(if proto.is_void { "void" } else { "_" });
        format!("{ret}({})", params.join(", "))
    }

    fn process_all_blocks(
        &mut self,
        pcode_facts: &pcode_reader::PcodeFacts,
        builders: &mut Builders,
    ) -> Result<(), Error> {
        // Group basic blocks by function
        let mut func_to_bbs: BTreeMap<pcode_reader::HighFunc, Vec<pcode_reader::PcodeBlockBasic>> =
            BTreeMap::new();
        for (bb_id, bb_data) in &pcode_facts.bb_facts {
            func_to_bbs
                .entry(bb_data.hfunc.clone())
                .or_default()
                .push(bb_id.clone());
        }

        // Process each function's blocks
        let mut sorted_hfuncs: Vec<_> = self.functions.keys().cloned().collect();
        sorted_hfuncs.sort(); // Determinism

        for hfunc_id in sorted_hfuncs {
            let func_idx = self.functions[&hfunc_id];
            let func_data = &pcode_facts.hfunc_facts[&hfunc_id];
            let func = &mut builders.program[func_idx];

            let mut pre_entry_idx = None;

            // 1. Create pre-entry block for parameter mapping and SP initialization if needed
            // Only do this if the function actually has a body
            if !func_data.is_external
                && let Some(bb_ids) = func_to_bbs.get(&hfunc_id)
                && !bb_ids.is_empty()
                && let Some(proto_id) = &func_data.proto
                && let Some(proto_data) = pcode_facts.proto_facts.get(proto_id)
                && (!proto_data.parameters.is_empty() || self.sp_name.is_some())
            {
                let bb_idx = func.blocks.new_block();
                pre_entry_idx = Some(bb_idx);

                // Add parameter mapping statements
                for (i, param) in proto_data.parameters.iter().enumerate() {
                    if let Some(rep) = pcode_facts.get_symbol_representative(&param.symbol) {
                        let vnode_data = pcode_facts.vnode_facts.get(rep);
                        let kind = if let Some(data) = vnode_data
                            && data.space.as_deref() == Some("stack")
                            && let Some(addr) = &data.address
                        {
                            // Stack parameter - bind to __stack_top offset
                            StatementKind::store(
                                Self::stack_slot_path(addr.0),
                                VariableRef::new_parameter(ParameterIdx::new(i)).into(),
                            )
                        } else {
                            // Other parameter (register, etc.) - bind to local variable
                            StatementKind::assign_or_store(
                                self.get_lvalue(rep, &pcode_facts.vnode_facts)?,
                                VariableRef::new_parameter(ParameterIdx::new(i)).into(),
                            )
                        };
                        func.blocks.blocks_mut()[bb_idx].push_back(Statement::new_kind(kind));
                    } else {
                        log::warn!(
                            "No representative varnode found for parameter {} of function {}",
                            i,
                            hfunc_id
                        );
                    }
                }

                // Initialize stack pointer if known - must be done after parameter updates
                // so that SP points to the stack state including the parameters.
                if let Some(sp_name) = &self.sp_name {
                    let sp_var = VariableRef::new_local(sp_name.to_string());
                    let stack_top_var = VariableRef::new_local("__stack_top".to_string());
                    let stmt = Statement::new_kind(StatementKind::Assign {
                        dest: sp_var,
                        sources: smallvec![Exp::AccessPath(AccessPath::without_fields(
                            stack_top_var
                        ))],
                    });
                    func.blocks.blocks_mut()[bb_idx].push_back(stmt);
                }
            }

            // 2. Add function blocks
            let mut entry_bb_idx = None;
            if let Some(bb_ids) = func_to_bbs.get(&hfunc_id) {
                let mut sorted_bb_ids = bb_ids.clone();
                sorted_bb_ids.sort(); // Determinism

                // Identify entry block by address or use the first one as fallback
                let mut entry_bb_id = None;
                if let Some(ep) = &func_data.entry_point {
                    entry_bb_id = sorted_bb_ids
                        .iter()
                        .find(|&bb_id| {
                            if let Some(bb_data) = pcode_facts.bb_facts.get(bb_id) {
                                // Try BB_START first
                                if let Some(start_addr) = &bb_data.start_address
                                    && start_addr.0 == ep.0
                                {
                                    return true;
                                }
                                // Fallback to first instruction address
                                if let Some(first_inst_id) = &bb_data.first_inst
                                    && let Some(pcode) = pcode_facts.pcode_facts.get(first_inst_id)
                                    && let Some(addr) = &pcode.target
                                {
                                    addr.0 == ep.0
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        })
                        .cloned();
                }

                // If we found the entry block, move it to the front
                if let Some(id) = &entry_bb_id
                    && let Some(pos) = sorted_bb_ids.iter().position(|x| x == id)
                {
                    let ep_id = sorted_bb_ids.remove(pos);
                    sorted_bb_ids.insert(0, ep_id);
                }

                for bb_id in sorted_bb_ids {
                    let bb_idx = func.blocks.new_block();
                    self.basic_blocks
                        .insert((hfunc_id.clone(), bb_id.clone()), bb_idx);
                    if entry_bb_idx.is_none() {
                        entry_bb_idx = Some(bb_idx);
                    }

                    // Map address to this block
                    let bb_data = &pcode_facts.bb_facts[&bb_id];
                    if let Some(first_inst_id) = &bb_data.first_inst
                        && let Some(pcode) = pcode_facts.pcode_facts.get(first_inst_id)
                        && let Some(address) = &pcode.target
                    {
                        self.address_to_bb
                            .insert(address.0, (hfunc_id.clone(), bb_idx));
                    }
                }
            }

            // 3. Link pre-entry to entry
            if let Some(p_idx) = pre_entry_idx
                && let Some(e_idx) = entry_bb_idx
            {
                func.blocks.blocks_mut()[p_idx].terminator =
                    Some(Terminator::new_kind(TerminatorKind::Goto {
                        targets: smallvec![e_idx],
                    }));
            }
        }

        Ok(())
    }

    fn process_pcode_instructions(
        &mut self,
        pcode_facts: &pcode_reader::PcodeFacts,
        artifact_key: ArtifactKey,
        builders: &mut Builders,
    ) -> Result<(), Error> {
        // Create a mapping from basic blocks to their functions
        let mut bb_to_function: BTreeMap<pcode_reader::PcodeBlockBasic, FunctionIdx> =
            BTreeMap::new();
        for (bb_id, bb_data) in &self.bb_facts {
            if let Some(func_idx) = self.functions.get(&bb_data.hfunc) {
                bb_to_function.insert(bb_id.clone(), *func_idx);
            }
        }

        // Collect all basic block IDs and their data first to avoid borrow issues
        let mut bb_facts_vec: Vec<(pcode_reader::PcodeBlockBasic, pcode_reader::BBData)> = self
            .bb_facts
            .iter()
            .map(|(bb_id, bb_data)| (bb_id.clone(), bb_data.clone()))
            .collect();
        bb_facts_vec.sort_by_key(|(id, _)| id.clone()); // Determinism

        let mut bb_statements: BTreeMap<
            (pcode_reader::HighFunc, pcode_reader::PcodeBlockBasic),
            Vec<Statement>,
        > = BTreeMap::new();
        let mut bb_terminators: BTreeMap<
            (pcode_reader::HighFunc, pcode_reader::PcodeBlockBasic),
            Terminator,
        > = BTreeMap::new();

        for (bb_id, bb_data) in bb_facts_vec {
            self.current_hfunc = Some(bb_data.hfunc.clone());
            let mut statements = Vec::new();

            let return_arity = bb_to_function
                .get(&bb_id)
                .map(|fidx| builders.program[*fidx].return_type.arity)
                .unwrap_or(0);

            // Use the sorted instruction indices from BBData
            for (_, inst_id) in &bb_data.instruction_indices {
                if let Some(pcode) = pcode_facts.pcode_facts.get(inst_id) {
                    for mut stmt in self.pcode_to_statement(
                        pcode,
                        &pcode_facts.vnode_facts,
                        &pcode_facts.hfunc_facts,
                        &builders.program,
                    )? {
                        if let Some(addr) = &pcode.target {
                            stmt.source_info =
                                SourceInfo::new(builders.source_info_builder.span_for(
                                    artifact_key.clone(),
                                    addr.0 as u32,
                                    source_info::SpanLen::ByteLen(1),
                                ));
                        }
                        statements.push(stmt);
                    }
                }
            }

            bb_statements.insert((bb_data.hfunc.clone(), bb_id.clone()), statements);

            // Determine terminator
            let mut terminator = None;
            if let Some((_, last_inst_id)) = bb_data.instruction_indices.last()
                && let Some(pcode) = pcode_facts.pcode_facts.get(last_inst_id)
            {
                match pcode.mnemonic.as_ref() {
                    "RETURN" => {
                        let mut args = smallvec![];
                        if pcode.inputs.len() >= 2 {
                            args.push(self.get_exp(&pcode.inputs[1], &pcode_facts.vnode_facts)?);
                        }
                        // Ensure return arity matches
                        while args.len() < return_arity as usize {
                            args.push(Exp::new_bytes(Vec::new()));
                        }
                        args.truncate(return_arity as usize);
                        terminator = Some(Terminator::new_kind(TerminatorKind::Return { args }));
                    }
                    "BRANCH" => {
                        // For BRANCH, we prefer out_edges or tout_edges
                        let mut targets = smallvec![];
                        for out in bb_data.out_edges.iter().chain(bb_data.tout_edges.iter()) {
                            if let Some(target_bb) =
                                self.basic_blocks.get(&(bb_data.hfunc.clone(), out.clone()))
                                && !targets.contains(target_bb)
                            {
                                targets.push(*target_bb);
                            }
                        }
                        if targets.is_empty()
                            && let Some(target_bb) = self.resolve_branch_target(
                                &bb_data.hfunc,
                                pcode,
                                &pcode_facts.vnode_facts,
                            )
                        {
                            targets.push(target_bb);
                        }

                        if !targets.is_empty() {
                            terminator =
                                Some(Terminator::new_kind(TerminatorKind::Goto { targets }));
                        }
                    }
                    "CBRANCH" => {
                        let mut targets = smallvec![];

                        // CBRANCH typically has two targets: True and False.
                        // Ghidra provides these in tout_edges and fout_edges.
                        for tout in &bb_data.tout_edges {
                            if let Some(target_bb) = self
                                .basic_blocks
                                .get(&(bb_data.hfunc.clone(), tout.clone()))
                                && !targets.contains(target_bb)
                            {
                                targets.push(*target_bb);
                            }
                        }
                        for fout in &bb_data.fout_edges {
                            if let Some(target_bb) = self
                                .basic_blocks
                                .get(&(bb_data.hfunc.clone(), fout.clone()))
                                && !targets.contains(target_bb)
                            {
                                targets.push(*target_bb);
                            }
                        }

                        // Fallback to resolve_branch_target and out_edges if needed
                        if targets.len() < 2
                            && let Some(target_bb) = self.resolve_branch_target(
                                &bb_data.hfunc,
                                pcode,
                                &pcode_facts.vnode_facts,
                            )
                            && !targets.contains(&target_bb)
                        {
                            targets.push(target_bb);
                        }

                        if targets.len() < 2 {
                            for out in &bb_data.out_edges {
                                if let Some(target_bb) =
                                    self.basic_blocks.get(&(bb_data.hfunc.clone(), out.clone()))
                                    && !targets.contains(target_bb)
                                {
                                    targets.push(*target_bb);
                                }
                            }
                        }

                        if !targets.is_empty() {
                            terminator =
                                Some(Terminator::new_kind(TerminatorKind::Goto { targets }));
                        }
                    }
                    _ => {}
                }
            }

            // If no explicit terminator (e.g. normal fallthrough block), use edges from BB_OUT
            if terminator.is_none() && !bb_data.out_edges.is_empty() {
                let mut targets = smallvec![];
                for out in &bb_data.out_edges {
                    if let Some(target_bb) =
                        self.basic_blocks.get(&(bb_data.hfunc.clone(), out.clone()))
                    {
                        targets.push(*target_bb);
                    }
                }
                if !targets.is_empty() {
                    terminator = Some(Terminator::new_kind(TerminatorKind::Goto { targets }));
                }
            }

            // Default to return if still no terminator
            let terminator = terminator.unwrap_or_else(|| {
                let mut args = smallvec![];
                while args.len() < return_arity as usize {
                    args.push(Exp::new_bytes(Vec::new()));
                }
                Terminator::new_kind(TerminatorKind::Return { args })
            });
            bb_terminators.insert((bb_data.hfunc.clone(), bb_id), terminator);
        }

        // Now add statements and terminators to ALL basic blocks
        for ((hfunc_id, bb_id), bb_idx) in &self.basic_blocks {
            if let Some(func_idx) = self.functions.get(hfunc_id) {
                let func = &mut builders.program[*func_idx];
                let bb = &mut func[*bb_idx];

                // Add statements if any exist for this basic block
                if let Some(statements) = bb_statements.get(&(hfunc_id.clone(), bb_id.clone())) {
                    for stmt in statements {
                        bb.statements.push_back(stmt.clone());
                    }
                }

                // Add terminator
                if let Some(terminator) = bb_terminators.get(&(hfunc_id.clone(), bb_id.clone())) {
                    bb.terminator = Some(terminator.clone());
                }
            }
        }

        Ok(())
    }

    fn resolve_branch_target(
        &self,
        hfunc_id: &pcode_reader::HighFunc,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Option<BasicBlockIdx> {
        if pcode.inputs.is_empty() {
            return None;
        }

        // In High Pcode, branches usually target a constant representing the block index
        // or a direct address.
        let target_vn = &pcode.inputs[0];
        if let Some(vnode_data) = vnode_facts.get(target_vn)
            && let Some(address) = &vnode_data.address
        {
            // Check if it's a relative offset to a block ID
            if let Some((target_hfunc, bb_idx)) = self.address_to_bb.get(&address.0)
                && target_hfunc == hfunc_id
            {
                return Some(*bb_idx);
            }
        }

        // If it's a CBRANCH, the target is the second input
        if &**pcode.mnemonic == "CBRANCH" && pcode.inputs.len() >= 2 {
            let target_vn = &pcode.inputs[1];
            if let Some(vnode_data) = vnode_facts.get(target_vn)
                && let Some(address) = &vnode_data.address
                && let Some((target_hfunc, bb_idx)) = self.address_to_bb.get(&address.0)
                && target_hfunc == hfunc_id
            {
                return Some(*bb_idx);
            }
        }

        None
    }

    fn pcode_to_statement(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        hfunc_facts: &BTreeMap<pcode_reader::HighFunc, pcode_reader::HFuncData>,
        program: &Program,
    ) -> Result<Vec<Statement>, Error> {
        match &**pcode.mnemonic {
            "COPY" | "INDIRECT" | "CAST" | "TRUNC" | "INT_SEXT" | "INT_ZEXT" | "INT2FLOAT"
            | "INT_2COMP" | "INT_NEGATE" | "BOOL_NEGATE" | "FLOAT_NEG" | "FLOAT_ABS"
            | "FLOAT_SQRT" | "FLOAT_CEIL" | "FLOAT_FLOOR" | "FLOAT_ROUND" | "FLOAT2FLOAT"
            | "POPCOUNT" => {
                // Handle copy-like and unary operations as assignments
                self.handle_copy_operation(pcode, vnode_facts)
            }
            "LOAD" => {
                // Handle load operations
                self.handle_load_operation(pcode, vnode_facts)
            }
            "STORE" => {
                // Handle store operations
                self.handle_store_operation(pcode, vnode_facts)
            }
            "CALL" | "CALLIND" => {
                // Handle call operations
                self.handle_call_operation(pcode, vnode_facts, hfunc_facts, program)
            }
            "RETURN" | "BRANCH" | "CBRANCH" | "BRANCHIND" => {
                // Control flow is handled in process_pcode_instructions for terminators
                Ok(Vec::new())
            }
            "MULTIEQUAL" | "INT_ADD" | "INT_SUB" | "INT_MULT" | "INT_DIV" | "INT_SDIV"
            | "INT_REM" | "INT_SREM" | "INT_AND" | "INT_OR" | "INT_XOR" | "INT_LEFT"
            | "INT_RIGHT" | "INT_SRIGHT" | "INT_EQUAL" | "INT_NOTEQUAL" | "INT_LESS"
            | "INT_SLESS" | "INT_LESSEQUAL" | "INT_SLESSEQUAL" | "INT_CARRY" | "INT_SCARRY"
            | "INT_SBORROW" | "BOOL_AND" | "BOOL_OR" | "BOOL_XOR" | "FLOAT_ADD" | "FLOAT_SUB"
            | "FLOAT_MULT" | "FLOAT_DIV" | "FLOAT_EQUAL" | "FLOAT_NOTEQUAL" | "FLOAT_LESS"
            | "FLOAT_LESSEQUAL" | "FLOAT_NAN" | "PIECE" | "SUBPIECE" => {
                self.handle_binop(pcode, vnode_facts, program, hfunc_facts)
            }
            "PTRSUB" => self.handle_ptrsub(pcode, vnode_facts, program, hfunc_facts),
            "PTRADD" => self.handle_ptradd(pcode, vnode_facts),
            _ => {
                // For now, treat unknown operations as no-ops
                log::warn!("Unsupported pcode mnemonic: {}", pcode.mnemonic);
                Ok(vec![Statement::new_kind(StatementKind::Nop)])
            }
        }
    }

    fn handle_binop(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        _program: &Program,
        _hfunc_facts: &BTreeMap<pcode_reader::HighFunc, pcode_reader::HFuncData>,
    ) -> Result<Vec<Statement>, Error> {
        let inputs: Result<SmallVec<[Exp; 2]>, Error> = pcode
            .inputs
            .iter()
            .map(|vn| self.get_exp(vn, vnode_facts))
            .collect();
        let outputs: Result<SmallVec<[AccessPath; 1]>, Error> = pcode
            .outputs
            .iter()
            .map(|vn| self.get_lvalue(vn, vnode_facts))
            .collect();

        let inputs = inputs?;
        let outputs = outputs?;

        if outputs.is_empty() {
            return Ok([Statement::new_kind(StatementKind::Nop)]
                .into_iter()
                .collect());
        }

        let temp = self.create_temp();
        let mut stmts = vec![Statement::new_kind(StatementKind::assign(
            temp.clone(),
            inputs,
        ))];
        self.push_assign_or_store(
            &mut stmts,
            outputs[0].clone(),
            Exp::AccessPath(AccessPath::without_fields(temp)),
        );
        Ok(stmts)
    }

    fn handle_ptrsub(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        program: &Program,
        hfunc_facts: &BTreeMap<pcode_reader::HighFunc, pcode_reader::HFuncData>,
    ) -> Result<Vec<Statement>, Error> {
        let outputs: Result<SmallVec<[AccessPath; 1]>, Error> = pcode
            .outputs
            .iter()
            .map(|vn| self.get_lvalue(vn, vnode_facts))
            .collect();
        let outputs = outputs.err_context(|| format!("handling outputs: {:?}", pcode.outputs))?;

        if outputs.is_empty() {
            return Ok([Statement::new_kind(StatementKind::Nop)]
                .into_iter()
                .collect());
        }

        if let Some(prop) = self.cp_results.get(&pcode.outputs[0]).cloned()
            && let pcode_reader::constant_propagation::SymbolicProp::Value(None, addr) = prop
        {
            if let Some(func_name) = self.resolve_address_to_func_name(addr, hfunc_facts, program) {
                let mut stmts = Vec::new();
                self.push_assign_or_store(
                    &mut stmts,
                    outputs[0].clone(),
                    Exp::ObjectRef(CallObject::FunctionPtr(func_name.into())),
                );
                log::warn!("Found a function pointer, yay");
                return Ok(stmts);
            } else {
                let src = self.exp_from_const_value(&pcode.outputs[0], vnode_facts, addr);
                let mut stmts = Vec::new();
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src);
                return Ok(stmts);
            }
        }

        if pcode.inputs.len() < 2 {
            return Ok([Statement::new_kind(StatementKind::Nop)]
                .into_iter()
                .collect());
        }

        let base_vn = &pcode.inputs[0];
        let offset_vn = &pcode.inputs[1];
        let base_const = self.get_propagated_const_value(base_vn, vnode_facts);
        let offset_const = self.get_propagated_const_value(offset_vn, vnode_facts);

        match (base_const, offset_const) {
            (Some(_), Some(_)) => {
                // Handled elsewhere, at use sites of this instruction
                Ok(Vec::new())
            }
            (Some(c), None) => {
                let mut ap = self.get_lvalue(offset_vn, vnode_facts)?;
                ap.path.fields.push(FieldAccess::Offset(Offset(c)));
                let mut stmts = Vec::new();
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), Exp::AccessPath(ap));
                Ok(stmts)
            }
            (None, Some(c)) => {
                let mut ap = self.get_lvalue(base_vn, vnode_facts)?;
                ap.path.fields.push(FieldAccess::Offset(Offset(c)));
                let mut stmts = Vec::new();
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), Exp::AccessPath(ap));
                Ok(stmts)
            }
            (None, None) => {
                let base_exp = self.get_exp(base_vn, vnode_facts)?;
                let offset_exp = self.get_exp(offset_vn, vnode_facts)?;
                let temp = self.create_temp();
                let mut stmts = vec![Statement::new_kind(StatementKind::assign(
                    temp.clone(),
                    [base_exp, offset_exp],
                ))];
                self.push_assign_or_store(
                    &mut stmts,
                    outputs[0].clone(),
                    Exp::AccessPath(AccessPath::without_fields(temp)),
                );
                Ok(stmts)
            }
        }
    }

    fn handle_ptradd(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Result<Vec<Statement>, Error> {
        let outputs: Result<SmallVec<[AccessPath; 1]>, Error> = pcode
            .outputs
            .iter()
            .map(|vn| self.get_lvalue(vn, vnode_facts))
            .collect();
        let outputs = outputs?;

        if outputs.is_empty() || pcode.inputs.len() < 3 {
            return Ok([Statement::new_kind(StatementKind::Nop)]
                .into_iter()
                .collect());
        }

        if let Some(prop) = self.cp_results.get(&pcode.outputs[0]).cloned()
            && let pcode_reader::constant_propagation::SymbolicProp::Value(None, addr) = prop
        {
            let src = self.exp_from_const_value(&pcode.outputs[0], vnode_facts, addr);
            let mut stmts = Vec::new();
            self.push_assign_or_store(&mut stmts, outputs[0].clone(), src);
            return Ok(stmts);
        }

        let base_vn = &pcode.inputs[0];
        let index_vn = &pcode.inputs[1];
        let size_vn = &pcode.inputs[2];

        let base_const = self.get_propagated_const_value(base_vn, vnode_facts);
        let index_const = self.get_propagated_const_value(index_vn, vnode_facts);
        let size_const = self
            .get_propagated_const_value(size_vn, vnode_facts)
            .or_else(|| self.get_const_value(size_vn, vnode_facts));

        match (base_const, index_const) {
            (Some(_), Some(_)) => Ok(Vec::new()),
            (None, Some(idx_c)) => {
                let mut ap = self.get_lvalue(base_vn, vnode_facts)?;
                let s_val = size_const.unwrap_or(1);
                ap.path
                    .fields
                    .push(FieldAccess::Offset(Offset(idx_c * s_val)));
                let mut stmts = Vec::new();
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), Exp::AccessPath(ap));
                Ok(stmts)
            }
            (Some(base_c), None) if size_const == Some(1) => {
                let mut ap = self.get_lvalue(index_vn, vnode_facts)?;
                ap.path.fields.push(FieldAccess::Offset(Offset(base_c)));
                let mut stmts = Vec::new();
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), Exp::AccessPath(ap));
                Ok(stmts)
            }
            _ => {
                let base_exp = self.get_exp(base_vn, vnode_facts)?;
                let index_exp = self.get_exp(index_vn, vnode_facts)?;
                let size_exp = if let Some(s) = size_const {
                    self.exp_from_const_value(size_vn, vnode_facts, s)
                } else {
                    self.get_exp(size_vn, vnode_facts)?
                };

                let temp = self.create_temp();
                let mut stmts = vec![Statement::new_kind(StatementKind::assign(
                    temp.clone(),
                    [base_exp, index_exp, size_exp],
                ))];
                self.push_assign_or_store(
                    &mut stmts,
                    outputs[0].clone(),
                    Exp::AccessPath(AccessPath::without_fields(temp)),
                );
                Ok(stmts)
            }
        }
    }

    fn handle_copy_operation(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Result<Vec<Statement>, Error> {
        let (inputs, outputs) = (&pcode.inputs, &pcode.outputs);
        if !inputs.is_empty() && !outputs.is_empty() && inputs[0] != outputs[0] {
            let input_exp = self.get_exp(&inputs[0], vnode_facts)?;
            let output_var = self.get_lvalue(&outputs[0], vnode_facts)?;
            let mut stmts = Vec::new();
            self.push_assign_or_store(&mut stmts, output_var, input_exp);
            return Ok(stmts);
        }
        Ok(vec![Statement::new_kind(StatementKind::Nop)])
    }

    fn handle_load_operation(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Result<Vec<Statement>, Error> {
        let inputs = &pcode.inputs;
        let outputs = &pcode.outputs;
        if inputs.len() >= 2 && !outputs.is_empty() {
            // LOAD <space>, <offset> -> <dest>
            let mut offset_exp = self.resolve_mem_exp(&inputs[0], &inputs[1], vnode_facts)?;
            offset_exp
                .path
                .fields
                .push(FieldAccess::Symbol("deref".into()));
            let output_var = self.get_lvalue(&outputs[0], vnode_facts)?;

            let mut stmts = Vec::new();
            // Load through the address's (composed) field path.
            let dest = if output_var.path.is_empty() {
                output_var.variable_ref.clone()
            } else {
                self.create_temp()
            };
            stmts.push(Statement::new_kind(StatementKind::load(
                dest.clone(),
                offset_exp.variable_ref,
                offset_exp.path,
            )));
            if !output_var.path.is_empty() {
                self.push_assign_or_store(
                    &mut stmts,
                    output_var,
                    Exp::AccessPath(AccessPath::without_fields(dest)),
                );
            }
            return Ok(stmts);
        }
        Ok(vec![Statement::new_kind(StatementKind::Nop)])
    }

    fn handle_store_operation(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Result<Vec<Statement>, Error> {
        let (inputs, _) = (&pcode.inputs, &pcode.outputs);
        if inputs.len() >= 3 {
            // STORE <space>, <offset>, <value>
            let mut offset_exp = self.resolve_mem_exp(&inputs[0], &inputs[1], vnode_facts)?;
            offset_exp
                .path
                .fields
                .push(FieldAccess::Symbol("deref".into()));
            let value_exp = self.get_exp(&inputs[2], vnode_facts)?;

            // Store through the address's (composed) field path. Unlike the old Update
            // lowering, this does NOT redefine the address base variable.
            return Ok(vec![Statement::new_kind(StatementKind::store(
                offset_exp, value_exp,
            ))]);
        }
        log::warn!("STORE missing inputs");
        Ok(vec![Statement::new_kind(StatementKind::Nop)])
    }

    /// Converts varnode into our internal representation. Constant space varnodes map to a Const
    /// address. stack register maps to __stack_top with the appropriate offset. Other registers
    /// map to a Var using vnode_id. Varnodes not in the register and not in the unique spaces map
    /// to a Offset of the vnode_id and the constant offset. All other varnodes map to a Var of the
    /// vnode_id.
    ///
    /// Stack memory is canonicalized so that the frontend's two models of it agree: a value
    /// that constant-propagation resolves to a stack address `__stack_top + k` becomes
    /// `__stack_top.[k]`, and a named Ghidra stack slot at frame offset `F` becomes
    /// `__stack_top.[F].deref` (a [`VnodeRep::StackSlot`]). Both then share the `__stack_top`
    /// root used by LOAD/STORE address resolution (see [`Self::resolve_mem_exp`]), so taint
    /// written through a stack pointer meets taint read from the corresponding slot.
    ///
    /// Global storage is canonicalized the same way, as `$globals.[A].deref` (a
    /// [`VnodeRep::Global`]). Globals are rooted at the shared global heap rather than at
    /// the varnode, because Ghidra gives each referencing function its own varnode and its
    /// own high variable for one global; only the address is common to all of them.
    fn convert_vnode(
        &self,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        register_facts: &[pcode_reader::RegisterData],
    ) -> VnodeRep {
        let Some(vnode_data) = vnode_facts.get(vnode_id) else {
            panic!("no data for vnode");
        };
        let var = VariableRef::new_local(vnode_id.to_string());
        let space = vnode_data.space.as_deref();

        if space == Some("const") {
            if let Some(address) = &vnode_data.address {
                return VnodeRep::Const(address.0);
            }
            if let Some(offset) = vnode_data.constant_offset {
                return VnodeRep::Const(offset);
            }
        }

        if space == Some("register")
            && let Some(offset) = &vnode_data.constant_offset
            && register_facts
                .iter()
                .any(|reg| reg.is_stack_pointer && reg.offset == *offset)
        {
            let stack_top = VariableRef::new_local("__stack_top".to_string());
            return VnodeRep::Offset(stack_top, 0);
        }

        // A register/unique value that constant-propagation resolves to a stack address
        // `__stack_top + k` is the address of frame slot `k`.
        if matches!(space, Some("register") | Some("unique"))
            && let Some(pcode_reader::constant_propagation::SymbolicProp::Value(Some(base), k)) =
                self.cp_results.get(vnode_id)
            && base.deref().deref() == "__stack_top"
        {
            let stack_top = VariableRef::new_local("__stack_top".to_string());
            return VnodeRep::Offset(stack_top, *k);
        }

        // A named Ghidra stack slot denotes the memory at its frame offset; express it as a
        // deref of the stack address so it unifies with the address-expression form above.
        if space == Some("stack")
            && let Some(offset) = vnode_data.constant_offset
        {
            return VnodeRep::StackSlot(offset);
        }

        // Storage for a global denotes the memory at its address. Key it on the address,
        // which is what every function referencing the global agrees on; the varnode and
        // its high variable are per-function, so rooting the path at either of those would
        // give each function a private copy of the global.
        if self.global_vnodes.contains(vnode_id)
            && let Some(address) = vnode_data
                .address
                .as_ref()
                .map(|a| a.0)
                .or(vnode_data.constant_offset)
        {
            return VnodeRep::Global(address);
        }

        if space != Some("register")
            && space != Some("unique")
            && let Some(offset) = vnode_data.constant_offset
        {
            return VnodeRep::Offset(var, offset);
        }

        VnodeRep::Var(var)
    }

    /// Builds the access path for the canonical stack slot `__stack_top.[offset].deref`.
    fn stack_slot_path(offset: i64) -> AccessPath {
        let stack_top = VariableRef::new_local("__stack_top".to_string());
        let mut ap = AccessPath::without_fields(stack_top);
        ap.path.fields.push(FieldAccess::Offset(Offset(offset)));
        ap.path.fields.push(FieldAccess::Symbol("deref".into()));
        ap
    }

    /// Builds the access path for the canonical global `$globals.[address].deref`.
    ///
    /// [`Variable::GlobalHeap`] is threaded through every call, so rooting globals here
    /// is what carries their taint between functions that never pass it directly.
    fn global_path(address: i64) -> AccessPath {
        let mut ap = AccessPath::without_fields(VariableRef::new_global());
        ap.path.fields.push(FieldAccess::Offset(Offset(address)));
        ap.path.fields.push(FieldAccess::Symbol("deref".into()));
        ap
    }

    /// Resolve an offset expression using constant propagation results if available.
    /// If offset = x + c, returns an access path for x with [c] as a symbolic field.
    fn resolve_mem_exp(
        &mut self,
        _space_id: &pcode_reader::PcodeVarnode,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Result<AccessPath, Error> {
        if let Some(prop) = self.cp_results.get(vnode_id).cloned()
            && let pcode_reader::constant_propagation::SymbolicProp::Value(Some(base_vn), offset) =
                prop
        {
            let is_stack = base_vn.deref().deref() == "__stack_top";
            if is_stack {
                let var_ref = VariableRef::new_local("__stack_top".to_string());
                let mut ap = AccessPath::without_fields(var_ref);
                ap.path.fields.push(FieldAccess::Offset(Offset(offset)));
                return Ok(ap);
            } else if base_vn != *vnode_id {
                let mut ap = self.get_lvalue(&base_vn, vnode_facts)?;
                ap.path.fields.push(FieldAccess::Offset(Offset(offset)));
                return Ok(ap);
            }
        }

        self.get_lvalue(vnode_id, vnode_facts)
    }

    /// Op is "CALL" or "CALLIND"
    fn handle_call_operation(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        hfunc_facts: &BTreeMap<pcode_reader::HighFunc, pcode_reader::HFuncData>,
        program: &Program,
    ) -> Result<Vec<Statement>, Error> {
        // Check if we have inputs and the first input is a call target
        let outputs: Result<SmallVec<[AccessPath; 4]>, _> = pcode
            .outputs
            .iter()
            .map(|output_id| self.get_lvalue(output_id, vnode_facts))
            .collect();

        // Try to resolve call target if we have inputs
        let (call_edges, actual_args) = if !pcode.inputs.is_empty() {
            let target_vnode = &pcode.inputs[0];
            let edges = if let Some(vnode_data) = vnode_facts.get(target_vnode)
                && vnode_data.space.as_deref() == Some("ram")
            {
                self.resolve_call_target(target_vnode, vnode_facts, hfunc_facts, program)
            } else {
                ctadl_ir::thin_vec![]
            };
            let args = pcode.inputs[1..]
                .iter()
                .map(|input_id| {
                    self.get_exp(input_id, vnode_facts)
                        .unwrap_or_else(|_| Exp::new_str("unknown"))
                })
                .collect();
            (edges, args)
        } else {
            (ctadl_ir::thin_vec![], ctadl_ir::thin_vec![])
        };

        let style = if &**pcode.mnemonic == "CALLIND" && call_edges.is_empty() {
            let callee = if !pcode.inputs.is_empty() {
                let target_vnode = &pcode.inputs[0];
                let target_exp = self
                    .get_exp(target_vnode, vnode_facts)
                    .unwrap_or_else(|_| Exp::new_str("unknown"));
                match target_exp {
                    Exp::AccessPath(ap) => ap,
                    _ => AccessPath::without_fields(VariableRef::new_local(
                        "unknown_callee".to_string(),
                    )),
                }
            } else {
                AccessPath::without_fields(VariableRef::new_local("unknown_callee".to_string()))
            };
            ctadl_ir::mir::call::CallStyle::FuncPtrCall {
                callee,
                signature: None,
            }
        } else {
            ctadl_ir::mir::call::CallStyle::DirectCall {
                call_edges: ctadl_ir::mir::call::CallEdges::Explicit(call_edges),
            }
        };

        let mut stmts = Vec::new();
        let outputs = outputs.err_context(|| format!("handling call: {:?}", pcode))?;
        let temps: ctadl_ir::ThinVec<VariableRef> =
            (0..outputs.len()).map(|_| self.create_temp()).collect();
        let kind = StatementKind::CallAssign {
            style,
            rets: temps.clone(),
            args: actual_args,
        };
        stmts.push(Statement::new_kind(kind));
        // store temps into outputs
        for (o, t) in outputs.iter().zip(temps) {
            self.push_assign_or_store(
                &mut stmts,
                o.clone(),
                Exp::AccessPath(AccessPath::without_fields(t)),
            );
        }

        Ok(stmts)
    }

    fn resolve_address_to_func_name(
        &self,
        addr: i64,
        hfunc_facts: &BTreeMap<pcode_reader::HighFunc, pcode_reader::HFuncData>,
        program: &Program,
    ) -> Option<String> {
        for (func_id, func_data) in hfunc_facts {
            if let Some(entry_point) = &func_data.entry_point
                && entry_point.0 == addr
                && let Some(target_func_idx) = self.functions.get(func_id)
            {
                return Some(program[*target_func_idx].name.clone());
            }
            for local_ep in &func_data.local_entry_points {
                if local_ep.0 == addr
                    && let Some(target_func_idx) = self.functions.get(func_id)
                {
                    return Some(program[*target_func_idx].name.clone());
                }
            }
        }
        None
    }

    /// Try to resolve call target by checking if the address matches any function entry points or local entry points
    fn resolve_call_target(
        &self,
        target_vnode: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        hfunc_facts: &BTreeMap<pcode_reader::HighFunc, pcode_reader::HFuncData>,
        program: &Program,
    ) -> ctadl_ir::ThinVec<String> {
        let address = if let Some(vnode_data) = vnode_facts.get(target_vnode) {
            vnode_data.address.as_ref().map(|addr| addr.0)
        } else {
            None
        };

        if let Some(addr) = address
            && let Some(name) = self.resolve_address_to_func_name(addr, hfunc_facts, program)
        {
            return ctadl_ir::thin_vec![name];
        }

        ctadl_ir::thin_vec![]
    }

    fn get_exp(
        &mut self,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Result<Exp, Error> {
        let rep = self.convert_vnode(vnode_id, vnode_facts, &self.register_facts);
        let exp = match rep {
            VnodeRep::Const(value) => self.exp_from_const_value(vnode_id, vnode_facts, value),
            VnodeRep::Var(var) => Exp::AccessPath(AccessPath::without_fields(var)),
            VnodeRep::Offset(var, offset) => {
                let mut ap = AccessPath::without_fields(var);
                ap.path.fields.push(FieldAccess::Offset(Offset(offset)));
                Exp::AccessPath(ap)
            }
            VnodeRep::StackSlot(offset) => Exp::AccessPath(Self::stack_slot_path(offset)),
            VnodeRep::Global(address) => Exp::AccessPath(Self::global_path(address)),
        };
        Ok(exp)
    }

    fn get_lvalue(
        &mut self,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Result<AccessPath, Error> {
        let rep = self.convert_vnode(vnode_id, vnode_facts, &self.register_facts);
        let ap = match rep {
            VnodeRep::Const(value) => {
                return Err(Error::PcodeConversion(format!(
                    "constant varnode {vnode_id} cannot be used as an lvalue: {value}"
                )));
            }
            VnodeRep::Var(var) => AccessPath::without_fields(var),
            VnodeRep::Offset(var, offset) => {
                let mut ap = AccessPath::without_fields(var);
                ap.path.fields.push(FieldAccess::Offset(Offset(offset)));
                ap
            }
            VnodeRep::StackSlot(offset) => Self::stack_slot_path(offset),
            VnodeRep::Global(address) => Self::global_path(address),
        };
        Ok(ap)
    }

    fn get_const_value(
        &self,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Option<i64> {
        match self.convert_vnode(vnode_id, vnode_facts, &self.register_facts) {
            VnodeRep::Const(value) => Some(value),
            VnodeRep::Var(_)
            | VnodeRep::Offset(..)
            | VnodeRep::StackSlot(_)
            | VnodeRep::Global(_) => None,
        }
    }

    fn get_propagated_const_value(
        &self,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> Option<i64> {
        if let Some(pcode_reader::constant_propagation::SymbolicProp::Value(None, value)) =
            self.cp_results.get(vnode_id)
        {
            return Some(*value);
        }
        self.get_const_value(vnode_id, vnode_facts)
    }

    fn exp_from_const_value(
        &self,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        value: i64,
    ) -> Exp {
        let size = vnode_facts
            .get(vnode_id)
            .and_then(|vnode| vnode.size)
            .unwrap_or(8);
        let bytes = value.to_be_bytes();
        let bytes = match usize::try_from(size) {
            Ok(size) if size <= bytes.len() => bytes[bytes.len() - size..].to_vec(),
            _ => bytes.to_vec(),
        };
        Exp::new_bytes(bytes)
    }

    fn create_temp(&mut self) -> VariableRef {
        let n = self.counter;
        self.counter += 1;
        VariableRef::new_local(format!("temp_{}", n))
    }

    fn finish(self, builders: Builders) -> Result<ProgramInfo, Error> {
        log::trace!("final program: {}", builders.program);
        // Verify the program
        builders.program.verify()?;

        Ok(ProgramInfo {
            program: builders.program,
            vmt: builders.vmt,
            source_info: builders.source_info_builder.finish(),
        })
    }
}
