/*! Ghidra Pcode language front end.

Converts Ghidra pcode facts into CTADL IR, and -- because the two are one cycle -- carries the
`RegisterNatives` scanner with it.

# Why the JNI registry lives here

[`import_pcode`] calls [`jni_registry::scan_import`], and [`jni_registry`] calls
[`ghidra::GhidraSource::detect`]. That is not incidental: the scan needs Ghidra's image base and
entry-point map, which only exist mid-`import_pcode`. Splitting them would mean hoisting the
`detect` guard into `import_pcode` and passing an `is_binary: bool` down, so the registry became
a standalone ELF scanner -- about twenty lines, worth doing the day something wants to read the
registry without pcode. Nothing does: `ctadl_ascent::languages::jni` reads the *file* the scan
wrote, not the scanner.
*/

use std::ops::Deref;
use std::path::Path;

use smallvec::{SmallVec, smallvec};
use source_info::{ArtifactKey, SourceInfoBuilder};
use std::collections::{BTreeMap, BTreeSet};

use ctadl_import::error::{Error, ErrorContext};
use ctadl_ir::mir::call::{
    NativeFunction, NativeQualifiedName, NativeSignature, NativeSimpleName, VirtualMethodTable,
};
use ctadl_ir::*;

use pcode_reader::PcodeFactsReader;

// `pub(crate)` for `GhidraSource::detect`, which the JNI registry scan uses to tell an artifact
// that is a binary on disk from a `ghidra://` URL or a `.gpr` project.
pub mod ghidra;

// The `RegisterNatives` scanner. See the module docs above for why it ships in this crate,
// and `jni_registry`'s own module doc for what it does. Kept as a `//` comment rather than a
// `///` one: an outer doc on a module that already has an inner `/*! ... */` makes rustdoc
// resolve the whole merged doc in *this* scope, which breaks every link the module wrote about
// its own items.
pub mod jni_registry;

/// This is hardcoded for now, but should be read from the facts
const WORD_SIZE: i64 = 8;

/// True when Ghidra is available to lower a binary to pcode. See [`ghidra::available`].
pub fn ghidra_available() -> bool {
    ghidra::available()
}

/// Import pcode facts from an artifact by running Ghidra and then converting the facts
pub fn import_pcode(import: &ctadl_import::project::ArtifactImport) -> Result<ProgramInfo, Error> {
    let path = &import.artifact_path;
    let import_path = import.import_path();

    // Run Ghidra to generate facts
    ghidra::run_ghidra_export(path, &import_path)?;

    let facts_dir = import_path.join("facts");

    // Gzip the (large) fact tables Ghidra just produced so they take less room in
    // the store. This also runs on the `CTADL_REUSE_FACTS` reuse path; it only
    // touches `*.facts` and skips `*.facts.gz`, so it is idempotent. The reader
    // below transparently handles either variant.
    ghidra::compress_facts_dir(&facts_dir)?;

    // Persist Ghidra's image base on the import config so downstream consumers
    // (SARIF address mapping, regression line checks) can recover
    // section-relative offsets regardless of the base Ghidra chose.
    let image_base = PcodeFactsReader::new(&facts_dir)
        .read_image_base()
        .map_err(|e| Error::PcodeFactRead(format!("Failed to read image base: {}", e)))
        .err_context(|| format!("reading pcode facts in: {}", facts_dir.display()))?;
    if let Some(image_base) = image_base {
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

    // Recover this library's `RegisterNatives` tables and write them beside the import's other
    // artifacts. Here, rather than earlier or later, because this is the first point at which
    // both halves of the address translation are known: Ghidra's image base was read above, and
    // `ctx.process` has just populated the entry-point map every recovered `fnPtr` is looked up
    // in. Scanning is unconditional and costs milliseconds; `--no-jni-registry` ignores the
    // result at index time, which is what makes a clean A/B possible without re-importing.
    jni_registry::scan_import(import, image_base, &ctx.entry_points)?;

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

/// A (possibly mixed) memory-address expression under construction: a base variable plus a
/// sequence of pointer-arithmetic offsets and symbolic dereferences. Access paths in the IR are
/// offset-only and fields are single symbols, so a mixed address is threaded here and lowered to
/// [`StatementKind::Load`]/[`StatementKind::Store`] via [`mir::load_access_path`] /
/// [`mir::store_access_path`].
#[derive(Clone, Debug)]
struct Addr {
    base: VariableRef,
    segments: ThinVec<PathSegment>,
}

impl Addr {
    fn new(base: VariableRef) -> Self {
        Self {
            base,
            segments: ThinVec::new(),
        }
    }

    /// True when this is a bare variable (no offset/deref).
    fn is_pathless(&self) -> bool {
        self.segments.is_empty()
    }

    fn push_offset(&mut self, offset: i64) {
        self.segments.push(PathSegment::offset(offset));
    }

    fn push_deref(&mut self) {
        self.segments.push(PathSegment::symbol("deref"));
    }
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
    // Function entry address -> the fully-qualified IR name of the function there. Keyed by the
    // address Ghidra reports, which is `image_base + ELF vaddr`. Built alongside `functions` in
    // `process_functions`; the JNI registry scan is its only consumer, resolving each recovered
    // `fnPtr` to something `link` can name.
    entry_points: BTreeMap<i64, String>,
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
            entry_points: Default::default(),
            counter: 0,
        }
    }

    /// Appends a write of `src` to the location `dest`: a plain assign for a bare variable, or a
    /// field store (with any needed loads emitted for intermediate dereferences) via
    /// [`mir::store_access_path`].
    ///
    /// A store always writes a symbolic field, but a `dest` that resolves to an offset-only address
    /// (e.g. a stack slot `__stack_top.[k]` or a memory-space varnode) has no field — writing there
    /// means writing the memory *at* that address. That dereference is a pcode-level detail, so we
    /// synthesize the canonical `.deref` field here, making the write a `store ....deref := src`
    /// that aliases the `.deref` reads emitted for the same address (memory loads, stack slots).
    fn push_assign_or_store(
        &mut self,
        stmts: &mut Vec<Statement>,
        mut dest: Addr,
        src: Exp,
        locals: &mut Locals,
    ) {
        if !dest.segments.is_empty() && dest.segments.iter().all(PathSegment::is_offset) {
            dest.push_deref();
        }
        mir::store_access_path(dest.base, dest.segments, src, stmts, || {
            self.create_temp(locals)
        });
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
            .map_err(|e| Error::PcodeFactRead(format!("Failed to read pcode facts: {}", e)))
            .err_context(|| format!("reading pcode facts in: {}", facts_dir.display()))?;

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
            // The namespace-qualified name, e.g. `Foo::bar`. Ghidra's exporter builds
            // `func_id` as `getName(true) + "@" + entryPoint` (ExportPcode.java), so the
            // qualification is recoverable only from the id: `func_data.name` is the bare
            // name and `func_name` above drops the namespace whenever the bare name happens
            // to be globally unique. Compute it here, outside that branch, so the value
            // never depends on which naming branch ran. Split on the LAST `@`: entry points
            // contain none, so `<EXTERNAL>::system@EXTERNAL:00000007` splits correctly.
            // Re-attach `simple_name` rather than the id's own tail so a namespace-less
            // function's qualified name equals its simple name even on Mach-O, where
            // `simple_name` has stripped a leading `_`.
            let signature = Self::format_native_signature(func_data, proto_facts);
            let func_id_str: &str = func_id;
            let qualified_raw = func_id_str
                .rsplit_once('@')
                .map_or(func_id_str, |(qualified, _entry_point)| qualified);
            let qualified_name = match qualified_raw.rfind("::") {
                Some(sep) => format!("{}::{}", &qualified_raw[..sep], simple_name),
                None => simple_name.to_string(),
            };
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
                    NativeQualifiedName(qualified_name.as_str().into()),
                ));
            }

            // Store function mapping
            self.functions.insert(func_id.clone(), func_idx);
            // And, by entry address, the name `link` resolves to a `FunctionId`: `fq_name` is
            // exactly the string that went into the `NativeFunction` VMT column above, so a
            // registry row naming it names the same function the bridge would.
            if let Some(entry_point) = &func_data.entry_point {
                self.entry_points.insert(entry_point.0, fq_name);
            }
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
                        let dest = if let Some(data) = vnode_data
                            && data.space.as_deref() == Some("stack")
                            && let Some(addr) = &data.address
                        {
                            // Stack parameter - bind to the canonical stack slot
                            // `__stack_top.[offset].deref`.
                            Self::stack_slot_path(addr.0, &mut func.locals)
                        } else {
                            // Other parameter (register, etc.) - bind to local variable
                            self.get_lvalue(rep, &pcode_facts.vnode_facts, &mut func.locals)?
                        };
                        let mut stmts = Vec::new();
                        self.push_assign_or_store(
                            &mut stmts,
                            dest,
                            VariableRef::new_parameter(ParameterIdx::new(i)).into(),
                            &mut func.locals,
                        );
                        for s in stmts {
                            func.blocks.blocks_mut()[bb_idx].push_back(s);
                        }
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
                    let sp_var = VariableRef::new_local_idx(func.locals.get_or_intern(sp_name));
                    let stack_top_var =
                        VariableRef::new_local_idx(func.locals.get_or_intern("__stack_top"));
                    let stmt = Statement::new_kind(StatementKind::Assign {
                        dest: sp_var,
                        sources: smallvec![Exp::Variable(stack_top_var)],
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

            let func_idx_opt = bb_to_function.get(&bb_id).copied();
            let return_arity = func_idx_opt
                .map(|fidx| builders.program[fidx].return_type.arity)
                .unwrap_or(0);

            // Interning locals requires `&mut` to the function's table while `&builders.program`
            // is held immutably (to resolve call targets), so take the table out for the duration
            // of statement generation and put it back at the end of the iteration.
            let mut locals = func_idx_opt
                .map(|fidx| std::mem::take(&mut builders.program[fidx].locals))
                .unwrap_or_default();

            // Use the sorted instruction indices from BBData
            for (_, inst_id) in &bb_data.instruction_indices {
                if let Some(pcode) = pcode_facts.pcode_facts.get(inst_id) {
                    for mut stmt in self.pcode_to_statement(
                        pcode,
                        &pcode_facts.vnode_facts,
                        &pcode_facts.hfunc_facts,
                        &builders.program,
                        &mut locals,
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
                            // The return value may be a field read, which lowers to loads. They
                            // must execute before the (separately stored) terminator, so append
                            // them to the end of this block's already-inserted statements.
                            let mut ret_stmts = Vec::new();
                            let arg = self.get_exp(
                                &mut ret_stmts,
                                &pcode.inputs[1],
                                &pcode_facts.vnode_facts,
                                &mut locals,
                            )?;
                            if !ret_stmts.is_empty()
                                && let Some(block_stmts) =
                                    bb_statements.get_mut(&(bb_data.hfunc.clone(), bb_id.clone()))
                            {
                                block_stmts.extend(ret_stmts);
                            }
                            args.push(arg);
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

            // Restore the function's locals table now that generation for this block is done.
            if let Some(fidx) = func_idx_opt {
                builders.program[fidx].locals = locals;
            }
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
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, Error> {
        match &**pcode.mnemonic {
            "COPY" | "INDIRECT" | "CAST" | "TRUNC" | "INT_SEXT" | "INT_ZEXT" | "INT2FLOAT"
            | "INT_2COMP" | "INT_NEGATE" | "BOOL_NEGATE" | "FLOAT_NEG" | "FLOAT_ABS"
            | "FLOAT_SQRT" | "FLOAT_CEIL" | "FLOAT_FLOOR" | "FLOAT_ROUND" | "FLOAT2FLOAT"
            | "POPCOUNT" => {
                // Handle copy-like and unary operations as assignments
                self.handle_copy_operation(pcode, vnode_facts, locals)
            }
            "LOAD" => {
                // Handle load operations
                self.handle_load_operation(pcode, vnode_facts, locals)
            }
            "STORE" => {
                // Handle store operations
                self.handle_store_operation(pcode, vnode_facts, locals)
            }
            "CALL" | "CALLIND" => {
                // Handle call operations
                self.handle_call_operation(pcode, vnode_facts, hfunc_facts, program, locals)
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
                self.handle_binop(pcode, vnode_facts, program, hfunc_facts, locals)
            }
            "PTRSUB" => self.handle_ptrsub(pcode, vnode_facts, program, hfunc_facts, locals),
            "PTRADD" => self.handle_ptradd(pcode, vnode_facts, locals),
            _ => {
                // For now, treat unknown operations as no-ops
                log_once::warn_once!("Unsupported pcode mnemonic: {}", pcode.mnemonic);
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
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, Error> {
        let outputs: Result<SmallVec<[Addr; 1]>, Error> = pcode
            .outputs
            .iter()
            .map(|vn| self.get_lvalue(vn, vnode_facts, locals))
            .collect();
        let outputs = outputs?;

        if outputs.is_empty() {
            return Ok([Statement::new_kind(StatementKind::Nop)]
                .into_iter()
                .collect());
        }

        let mut stmts = Vec::new();
        let inputs: SmallVec<[Exp; 2]> = pcode
            .inputs
            .iter()
            .map(|vn| self.get_exp(&mut stmts, vn, vnode_facts, locals))
            .collect::<Result<_, _>>()?;

        let temp = self.create_temp(locals);
        stmts.push(Statement::new_kind(StatementKind::assign(
            temp.clone(),
            inputs,
        )));
        self.push_assign_or_store(&mut stmts, outputs[0].clone(), Exp::Variable(temp), locals);
        Ok(stmts)
    }

    fn handle_ptrsub(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        program: &Program,
        hfunc_facts: &BTreeMap<pcode_reader::HighFunc, pcode_reader::HFuncData>,
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, Error> {
        let outputs: Result<SmallVec<[Addr; 1]>, Error> = pcode
            .outputs
            .iter()
            .map(|vn| self.get_lvalue(vn, vnode_facts, locals))
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
                    locals,
                );
                log::debug!("Found a function pointer, yay");
                return Ok(stmts);
            } else {
                let src = self.exp_from_const_value(&pcode.outputs[0], vnode_facts, addr);
                let mut stmts = Vec::new();
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
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
        let base_const = self.get_propagated_const_value(base_vn, vnode_facts, locals);
        let offset_const = self.get_propagated_const_value(offset_vn, vnode_facts, locals);

        match (base_const, offset_const) {
            (Some(_), Some(_)) => {
                // Handled elsewhere, at use sites of this instruction
                Ok(Vec::new())
            }
            (Some(c), None) => {
                let mut ap = self.get_lvalue(offset_vn, vnode_facts, locals)?;
                ap.push_offset(c);
                let mut stmts = Vec::new();
                let src = Exp::access_path(self.load_ap(&mut stmts, ap, locals));
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
                Ok(stmts)
            }
            (None, Some(c)) => {
                let mut ap = self.get_lvalue(base_vn, vnode_facts, locals)?;
                ap.push_offset(c);
                let mut stmts = Vec::new();
                let src = Exp::access_path(self.load_ap(&mut stmts, ap, locals));
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
                Ok(stmts)
            }
            (None, None) => {
                let mut stmts = Vec::new();
                let base_exp = self.get_exp(&mut stmts, base_vn, vnode_facts, locals)?;
                let offset_exp = self.get_exp(&mut stmts, offset_vn, vnode_facts, locals)?;
                let temp = self.create_temp(locals);
                stmts.push(Statement::new_kind(StatementKind::assign(
                    temp.clone(),
                    [base_exp, offset_exp],
                )));
                self.push_assign_or_store(
                    &mut stmts,
                    outputs[0].clone(),
                    Exp::Variable(temp),
                    locals,
                );
                Ok(stmts)
            }
        }
    }

    fn handle_ptradd(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, Error> {
        let outputs: Result<SmallVec<[Addr; 1]>, Error> = pcode
            .outputs
            .iter()
            .map(|vn| self.get_lvalue(vn, vnode_facts, locals))
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
            self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
            return Ok(stmts);
        }

        let base_vn = &pcode.inputs[0];
        let index_vn = &pcode.inputs[1];
        let size_vn = &pcode.inputs[2];

        let base_const = self.get_propagated_const_value(base_vn, vnode_facts, locals);
        let index_const = self.get_propagated_const_value(index_vn, vnode_facts, locals);
        let size_const = self
            .get_propagated_const_value(size_vn, vnode_facts, locals)
            .or_else(|| self.get_const_value(size_vn, vnode_facts, locals));

        match (base_const, index_const) {
            (Some(_), Some(_)) => Ok(Vec::new()),
            (None, Some(idx_c)) => {
                let mut ap = self.get_lvalue(base_vn, vnode_facts, locals)?;
                let s_val = size_const.unwrap_or(1);
                // PTRADD is `base + index * size` in the pointer's own width, so the
                // product wraps on the machine and has to wrap here. Constant
                // propagation happily hands back a huge index (a folded pointer, a
                // sentinel like -1), and `*` panics on overflow in a debug build --
                // which is a crashed import, not a wrong offset. libtmessages.49.so out
                // of Telegram's arm64 split APK does exactly this.
                ap.push_offset(idx_c.wrapping_mul(s_val));
                let mut stmts = Vec::new();
                let src = Exp::access_path(self.load_ap(&mut stmts, ap, locals));
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
                Ok(stmts)
            }
            (Some(base_c), None) if size_const == Some(1) => {
                let mut ap = self.get_lvalue(index_vn, vnode_facts, locals)?;
                ap.push_offset(base_c);
                let mut stmts = Vec::new();
                let src = Exp::access_path(self.load_ap(&mut stmts, ap, locals));
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
                Ok(stmts)
            }
            _ => {
                let mut stmts = Vec::new();
                let base_exp = self.get_exp(&mut stmts, base_vn, vnode_facts, locals)?;
                let index_exp = self.get_exp(&mut stmts, index_vn, vnode_facts, locals)?;
                let size_exp = if let Some(s) = size_const {
                    self.exp_from_const_value(size_vn, vnode_facts, s)
                } else {
                    self.get_exp(&mut stmts, size_vn, vnode_facts, locals)?
                };

                let temp = self.create_temp(locals);
                stmts.push(Statement::new_kind(StatementKind::assign(
                    temp.clone(),
                    [base_exp, index_exp, size_exp],
                )));
                self.push_assign_or_store(
                    &mut stmts,
                    outputs[0].clone(),
                    Exp::Variable(temp),
                    locals,
                );
                Ok(stmts)
            }
        }
    }

    fn handle_copy_operation(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, Error> {
        let (inputs, outputs) = (&pcode.inputs, &pcode.outputs);
        if !inputs.is_empty() && !outputs.is_empty() && inputs[0] != outputs[0] {
            let mut stmts = Vec::new();
            let input_exp = self.get_exp(&mut stmts, &inputs[0], vnode_facts, locals)?;
            let output_var = self.get_lvalue(&outputs[0], vnode_facts, locals)?;
            self.push_assign_or_store(&mut stmts, output_var, input_exp, locals);
            return Ok(stmts);
        }
        Ok(vec![Statement::new_kind(StatementKind::Nop)])
    }

    fn handle_load_operation(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, Error> {
        let inputs = &pcode.inputs;
        let outputs = &pcode.outputs;
        if inputs.len() >= 2 && !outputs.is_empty() {
            // LOAD <space>, <offset> -> <dest>
            let addr = self.resolve_mem_exp(&inputs[0], &inputs[1], vnode_facts, locals)?;
            let output_var = self.get_lvalue(&outputs[0], vnode_facts, locals)?;

            let mut stmts = Vec::new();
            // Materialize the address (loading through any intermediate derefs), then load the
            // value at that address's `deref` field.
            let addr = self.load_ap(&mut stmts, addr, locals);
            let dest = if output_var.is_pathless() {
                output_var.base.clone()
            } else {
                self.create_temp(locals)
            };
            stmts.push(Statement::new_kind(StatementKind::load(
                dest.clone(),
                addr,
                FieldRef::symbol("deref"),
            )));
            if !output_var.is_pathless() {
                self.push_assign_or_store(&mut stmts, output_var, Exp::Variable(dest), locals);
            }
            return Ok(stmts);
        }
        Ok(vec![Statement::new_kind(StatementKind::Nop)])
    }

    fn handle_store_operation(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, Error> {
        let (inputs, _) = (&pcode.inputs, &pcode.outputs);
        if inputs.len() >= 3 {
            // STORE <space>, <offset>, <value>
            let mut dest = self.resolve_mem_exp(&inputs[0], &inputs[1], vnode_facts, locals)?;
            // Store through the address's `deref` field.
            dest.push_deref();
            let mut stmts = Vec::new();
            let value_exp = self.get_exp(&mut stmts, &inputs[2], vnode_facts, locals)?;

            // Store through the address's (composed) field path. Unlike the old Update lowering,
            // this does NOT redefine the address base variable. Any loads needed to materialize
            // the stored value (above) or intermediate dereferences of the address are emitted
            // first.
            self.push_assign_or_store(&mut stmts, dest, value_exp, locals);
            return Ok(stmts);
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
        locals: &mut Locals,
    ) -> VnodeRep {
        let Some(vnode_data) = vnode_facts.get(vnode_id) else {
            panic!("no data for vnode");
        };
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
            let stack_top = VariableRef::new_local_idx(locals.get_or_intern("__stack_top"));
            return VnodeRep::Offset(stack_top, 0);
        }

        // A register/unique value that constant-propagation resolves to a stack address
        // `__stack_top + k` is the address of frame slot `k`.
        if matches!(space, Some("register") | Some("unique"))
            && let Some(pcode_reader::constant_propagation::SymbolicProp::Value(Some(base), k)) =
                self.cp_results.get(vnode_id)
            && base.deref().deref() == "__stack_top"
        {
            let stack_top = VariableRef::new_local_idx(locals.get_or_intern("__stack_top"));
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
            let var = VariableRef::new_local_idx(locals.get_or_intern(&vnode_id.to_string()));
            return VnodeRep::Offset(var, offset);
        }

        let var = VariableRef::new_local_idx(locals.get_or_intern(&vnode_id.to_string()));
        VnodeRep::Var(var)
    }

    /// Builds the address for the canonical stack slot `__stack_top.[offset].deref`.
    fn stack_slot_path(offset: i64, locals: &mut Locals) -> Addr {
        let stack_top = VariableRef::new_local_idx(locals.get_or_intern("__stack_top"));
        let mut addr = Addr::new(stack_top);
        addr.push_offset(offset);
        addr.push_deref();
        addr
    }

    /// Builds the access path for the canonical global `$globals.[address].deref`.
    ///
    /// [`Variable::GlobalHeap`] is threaded through every call, so rooting globals here
    /// is what carries their taint between functions that never pass it directly.
    fn global_path(address: i64) -> Addr {
        let mut addr = Addr::new(VariableRef::new_global());
        addr.push_offset(address);
        addr.push_deref();
        addr
    }

    /// Resolve an offset expression using constant propagation results if available.
    /// If offset = x + c, returns an access path for x with [c] as a symbolic field.
    fn resolve_mem_exp(
        &mut self,
        _space_id: &pcode_reader::PcodeVarnode,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Result<Addr, Error> {
        if let Some(prop) = self.cp_results.get(vnode_id).cloned()
            && let pcode_reader::constant_propagation::SymbolicProp::Value(Some(base_vn), offset) =
                prop
        {
            let is_stack = base_vn.deref().deref() == "__stack_top";
            if is_stack {
                let var_ref = VariableRef::new_local_idx(locals.get_or_intern("__stack_top"));
                let mut addr = Addr::new(var_ref);
                addr.push_offset(offset);
                return Ok(addr);
            } else if base_vn != *vnode_id {
                let mut addr = self.get_lvalue(&base_vn, vnode_facts, locals)?;
                addr.push_offset(offset);
                return Ok(addr);
            }
        }

        self.get_lvalue(vnode_id, vnode_facts, locals)
    }

    /// Op is "CALL" or "CALLIND"
    fn handle_call_operation(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        hfunc_facts: &BTreeMap<pcode_reader::HighFunc, pcode_reader::HFuncData>,
        program: &Program,
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, Error> {
        // Check if we have inputs and the first input is a call target
        let outputs: Result<SmallVec<[Addr; 4]>, _> = pcode
            .outputs
            .iter()
            .map(|output_id| self.get_lvalue(output_id, vnode_facts, locals))
            .collect();

        // Loads needed to materialize the call arguments are emitted before the call.
        let mut stmts = Vec::new();

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
                    self.get_exp(&mut stmts, input_id, vnode_facts, locals)
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
                // The callee is a call-target address. Offsets stay on the access path (pointer
                // arithmetic), but any dereference (e.g. a function pointer read from a stack
                // slot) is lowered to a load, leaving an offset-only callee address.
                let ap = match self.get_lvalue(target_vnode, vnode_facts, locals) {
                    Ok(ap) => ap,
                    Err(_) => Addr::new(VariableRef::new_local_idx(
                        locals.get_or_intern("unknown_callee"),
                    )),
                };
                self.load_ap(&mut stmts, ap, locals)
            } else {
                AccessPath::without_fields(VariableRef::new_local_idx(
                    locals.get_or_intern("unknown_callee"),
                ))
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

        let outputs = outputs.err_context(|| format!("handling call: {:?}", pcode))?;
        let temps: ctadl_ir::ThinVec<VariableRef> = (0..outputs.len())
            .map(|_| self.create_temp(locals))
            .collect();
        let kind = StatementKind::CallAssign {
            style,
            rets: temps.clone(),
            args: actual_args,
        };
        stmts.push(Statement::new_kind(kind));
        // store temps into outputs
        for (o, t) in outputs.iter().zip(temps) {
            self.push_assign_or_store(&mut stmts, o.clone(), Exp::Variable(t), locals);
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

    /// Reads a varnode as an expression. A field read is not expressible as an [`Exp`], so any
    /// access path with fields is lowered into a sequence of [`StatementKind::Load`]s (appended to
    /// `stmts`) and the resulting temporary is returned as an [`Exp::Variable`].
    fn get_exp(
        &mut self,
        stmts: &mut Vec<Statement>,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Result<Exp, Error> {
        let rep = self.convert_vnode(vnode_id, vnode_facts, &self.register_facts, locals);
        let exp = match rep {
            VnodeRep::Const(value) => self.exp_from_const_value(vnode_id, vnode_facts, value),
            VnodeRep::Var(var) => Exp::Variable(var),
            VnodeRep::Offset(var, offset) => {
                let mut addr = Addr::new(var);
                addr.push_offset(offset);
                Exp::access_path(self.load_ap(stmts, addr, locals))
            }
            VnodeRep::StackSlot(offset) => {
                let addr = Self::stack_slot_path(offset, locals);
                Exp::access_path(self.load_ap(stmts, addr, locals))
            }
            VnodeRep::Global(address) => {
                Exp::access_path(self.load_ap(stmts, Self::global_path(address), locals))
            }
        };
        Ok(exp)
    }

    /// Lowers the field reads (symbolic derefs) of `addr` into loads (see
    /// [`mir::load_access_path`]), appending them to `stmts` and returning the residual *address*
    /// — the base variable plus any trailing offset arithmetic — as an offset-only access path.
    /// Offsets emit no load; a pathless or offset-only `addr` is returned unchanged.
    fn load_ap(
        &mut self,
        stmts: &mut Vec<Statement>,
        addr: Addr,
        locals: &mut Locals,
    ) -> AccessPath {
        mir::load_access_path(addr.base, addr.segments, stmts, || self.create_temp(locals))
    }

    fn get_lvalue(
        &mut self,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Result<Addr, Error> {
        let rep = self.convert_vnode(vnode_id, vnode_facts, &self.register_facts, locals);
        let addr = match rep {
            VnodeRep::Const(value) => {
                return Err(Error::PcodeConversion(format!(
                    "constant varnode {vnode_id} cannot be used as an lvalue: {value}"
                )));
            }
            VnodeRep::Var(var) => Addr::new(var),
            VnodeRep::Offset(var, offset) => {
                let mut addr = Addr::new(var);
                addr.push_offset(offset);
                addr
            }
            VnodeRep::StackSlot(offset) => Self::stack_slot_path(offset, locals),
            VnodeRep::Global(address) => Self::global_path(address),
        };
        Ok(addr)
    }

    fn get_const_value(
        &self,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Option<i64> {
        match self.convert_vnode(vnode_id, vnode_facts, &self.register_facts, locals) {
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
        locals: &mut Locals,
    ) -> Option<i64> {
        if let Some(pcode_reader::constant_propagation::SymbolicProp::Value(None, value)) =
            self.cp_results.get(vnode_id)
        {
            return Some(*value);
        }
        self.get_const_value(vnode_id, vnode_facts, locals)
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

    fn create_temp(&mut self, locals: &mut Locals) -> VariableRef {
        let n = self.counter;
        self.counter += 1;
        VariableRef::new_local_idx(locals.get_or_intern(&format!("temp_{}", n)))
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
