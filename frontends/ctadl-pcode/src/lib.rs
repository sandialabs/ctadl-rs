/*! Ghidra Pcode language front end.

Turns Ghidra pcode facts into CTADL IR. It also holds the `RegisterNatives` scanner, because the
two need each other.

# Conventions

How pcode follows the front-end conventions of S19 in `spec.md`:

- There is no address-of. Address-taken storage lives behind `deref`. The frame is memory:
  frame slot `k` is `__stack_top.[k].deref`, and writing one is a store. `mem2reg` is the only
  thing that makes a slot flow-sensitive.
- A global at address `A` is `$globals.[A].deref`, keyed on the address so that every function
  shares it.
- A constant offset is an offset segment: `PTRSUB p, 8` is `p.[8]`. A non-constant offset is a
  copy of the base plus the value of the index: `q = PTRADD p, i, 4` is `q = p, value(i)`.
  Under `CTADL_EXP_INT_ARITH=merge`, a comparison mode, every operand is merged instead.
- Offsets have no width. `p.[0].deref` and `p.[4].deref` are distinct cells, and a 4-byte write
  that overlaps an 8-byte read is lost.
- A write to a frame address itself, `__stack_top.[k]`, is dropped, because every read of the
  varnode that holds the address resolves to the address too. See
  `Context::push_assign_or_store`.

# Why the JNI registry lives here

[`import_pcode`] calls [`jni_registry::scan_import`], and [`jni_registry`] calls
[`ghidra::GhidraSource::detect`]. That is not an accident. The scan needs Ghidra's image base
and its map of entry points, and those exist only while `import_pcode` is running. To separate
them, the `detect` check would move up into `import_pcode`, which would pass an `is_binary:
bool` down, and the registry would become an ELF scanner that stands on its own. That is about
twenty lines of work, worth doing when something wants to read the registry without pcode.
Nothing does today: `ctadl_ascent::languages::jni` reads the file the scan wrote, not the
scanner itself.
*/

use std::ops::Deref;
use std::path::Path;

use smallvec::{SmallVec, smallvec};
use source_info::ArtifactKey;
use std::collections::{BTreeMap, BTreeSet};

use ctadl_import::error::{Error, ErrorContext};
use ctadl_ir::mir::call::{FunctionNames, VirtualMethodTable};
use ctadl_ir::*;

use pcode_reader::PcodeFactsReader;

pub mod ghidra;

// The `RegisterNatives` scanner. The module docs above say why it lives in this crate, and
// `jni_registry`'s own module docs say what it does. This is a plain `//` comment, not a `///`
// doc comment, on purpose. Adding a doc comment here to a module that already has its own
// `/*! ... */` docs makes rustdoc resolve the combined text in this file's scope, which breaks
// every link the module wrote to its own items.
pub mod jni_registry;

/// This is hardcoded for now, but should be read from the facts
const WORD_SIZE: i64 = 8;

/// True when Ghidra is available to lower a binary to pcode. See [`ghidra::available`].
pub fn ghidra_available() -> bool {
    ghidra::available()
}

/// Import pcode facts from an artifact by running Ghidra and then converting the facts.
///
/// `jobs` caps Ghidra's threads; see [`ghidra::run_ghidra_export`].
pub fn import_pcode(
    import: &ctadl_import::project::ArtifactImport,
    jobs: usize,
) -> Result<ProgramInfo, Error> {
    let path = &import.artifact_path;
    let import_path = import.import_path();

    // Run Ghidra to generate facts
    ghidra::run_ghidra_export(path, &import_path, jobs)?;

    let facts_dir = import_path.join("facts");

    // Gzip the fact tables Ghidra just produced; the reader below handles either variant.
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
    let mut builders = ProgramInfoBuilder::new(VirtualMethodTable::new_native());

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
    /// Function mapping: pcode function ID -> CTADL function index
    functions: BTreeMap<pcode_reader::HighFunc, FunctionIdx>,
    /// Basic block mapping: (function ID, pcode block ID) -> CTADL basic block index
    basic_blocks: BTreeMap<(pcode_reader::HighFunc, pcode_reader::PcodeBlockBasic), BasicBlockIdx>,
    /// Basic block facts for function lookup
    bb_facts: BTreeMap<pcode_reader::PcodeBlockBasic, pcode_reader::BBData>,
    /// Mapping from instruction address to basic block
    address_to_bb: BTreeMap<i64, (pcode_reader::HighFunc, BasicBlockIdx)>,
    /// Constant propagation results
    cp_results:
        BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::constant_propagation::SymbolicProp>,
    /// Stack pointer register name
    sp_name: Option<String>,
    /// Varnodes that Ghidra classifies as storage for a global variable
    global_vnodes: BTreeSet<pcode_reader::PcodeVarnode>,
    /// The kind of each varnode's Ghidra data type, where it is a pointer or a scalar.
    vnode_kinds: BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::TypeKind>,
    int_arith: IntArith,
    /// Under [`IntArith::Value`], the varnodes used as a memory address, computed by
    /// [`address_vnodes`].
    addr_vnodes: BTreeSet<pcode_reader::PcodeVarnode>,
    /// Under [`IntArith::Value`], the outputs of `INT_MULT` and `INT_LEFT`: values that look
    /// like a scaled index rather than a base when added to an address.
    scaled_vnodes: BTreeSet<pcode_reader::PcodeVarnode>,
    /// Logged at the end of the import.
    binop_sources: BinopSources,
    /// Register facts needed for vnode conversion
    register_facts: Vec<pcode_reader::RegisterData>,
    /// Current function being processed
    current_hfunc: Option<pcode_reader::HighFunc>,
    /// Function entry address -> the fully-qualified IR name of the function there. Keyed by the
    /// address Ghidra reports, which is `image_base + ELF vaddr`. Built alongside `functions` in
    /// `process_functions`.
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
            vnode_kinds: Default::default(),
            int_arith: IntArith::from_env(),
            addr_vnodes: Default::default(),
            scaled_vnodes: Default::default(),
            binop_sources: Default::default(),
            register_facts: Default::default(),
            current_hfunc: None,
            entry_points: Default::default(),
            counter: 0,
        }
    }

    /// Appends a write of `src` to the access path `dest`: a plain assign for a bare variable, or a
    /// field store (with any needed loads emitted for intermediate dereferences) via
    /// [`mir::store_access_path`].
    ///
    /// A store always writes a symbolic field, but a `dest` that resolves to an offset-only address
    /// (e.g. a stack slot `__stack_top.[k]` or a memory-space varnode) has no field — writing there
    /// means writing the memory *at* that address. That dereference is a pcode-level detail, so we
    /// synthesize the canonical `.deref` field here, making the write a `store ....deref := src`
    /// that aliases the `.deref` reads emitted for the same address (memory loads, stack slots).
    ///
    /// A write into the frame itself, `__stack_top.[k].deref := src`, is a store like any other.
    /// The frame is memory, so `mem2reg` is what makes a slot flow-sensitive.
    ///
    /// A `dest` that is a frame address, `__stack_top.[k]`, writes nothing. It comes from a
    /// register or temporary that holds the address (the stack pointer, or `&local` that constant
    /// propagation resolved), and every read of that varnode resolves to the address too
    /// ([`Self::convert_vnode`]). Writing it to the slot at that address would overwrite the slot.
    fn push_assign_or_store(
        &mut self,
        stmts: &mut Vec<Statement>,
        mut dest: AccessPath,
        src: Exp,
        locals: &mut Locals,
    ) {
        if !dest.segments.is_empty() && dest.segments.iter().all(PathSegment::is_offset) {
            if let Variable::Local(base) = dest.base.variable
                && locals.name(base) == ssa::STACK_TOP
            {
                return;
            }
            dest.push(PathSegment::Deref);
        }
        stmts.push(Statement::new_kind(StatementKind::assign(dest, [src])));
    }

    fn process(
        &mut self,
        facts_dir: &Path,
        artifact_key: ArtifactKey,
        builders: &mut ProgramInfoBuilder,
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
        if self.int_arith == IntArith::Value {
            self.vnode_kinds = pcode_facts
                .vnode_facts
                .keys()
                .filter_map(|vn| Some((vn.clone(), pcode_facts.vnode_type_kind(vn)?)))
                .collect();
            self.addr_vnodes = address_vnodes(&pcode_facts);
            self.scaled_vnodes = pcode_facts
                .pcode_facts
                .values()
                .filter(|op| matches!(&**op.mnemonic, "INT_MULT" | "INT_LEFT"))
                .flat_map(|op| op.outputs.iter().cloned())
                .collect();
        }
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
        builders: &mut ProgramInfoBuilder,
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
                            .push(ctadl_ir::mir::ParameterType::Shared);
                    }
                }
            } else {
                // No prototype available, use default
                func.set_return_type(ReturnType { arity: 0 });
            }

            // Name it in the VMT (after the `func` borrow ends), so a model can find it by its
            // simple name. `func_data.name` is Ghidra's own spelling, before the leading
            // underscores are stripped from `simple_name`.
            builders.vmt.names.push(
                FunctionNames::new(fq_name.as_str(), simple_name)
                    .with_signature(signature.as_str())
                    .with_other(qualified_name.as_str())
                    .with_other(func_data.name.as_str()),
            );

            // Store function mapping
            self.functions.insert(func_id.clone(), func_idx);
            // And, by entry address, the name `link` resolves to a `FunctionId`: `fq_name` is
            // exactly the IR name the name table entry above keys on, so a registry row naming
            // it names the same function the bridge would.
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
        builders: &mut ProgramInfoBuilder,
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
                        dest: sp_var.into(),
                        sources: smallvec![Exp::variable(stack_top_var)],
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
        builders: &mut ProgramInfoBuilder,
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
            "LOAD" => self.handle_load_operation(pcode, vnode_facts, locals),
            "STORE" => self.handle_store_operation(pcode, vnode_facts, locals),
            "CALL" | "CALLIND" => {
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
        let outputs: Result<SmallVec<[AccessPath; 1]>, Error> = pcode
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
        let flows = self.operand_flows(pcode, vnode_facts, locals);
        let mut inputs: SmallVec<[Exp; 2]> = SmallVec::new();
        for (i, vn) in pcode.inputs.iter().enumerate() {
            let exp = self.get_exp(&mut stmts, vn, vnode_facts, locals)?;
            let exp = match flows.get(i) {
                Some(OperandFlow::Value) => self.value_of(&mut stmts, exp, locals),
                _ => exp,
            };
            if is_compare(&pcode.mnemonic) || is_arith(&pcode.mnemonic) {
                self.binop_sources.count(&exp);
            }
            inputs.push(exp);
        }

        let temp = self.create_temp(locals);
        stmts.push(Statement::new_kind(StatementKind::assign(
            temp.clone(),
            inputs,
        )));
        self.push_assign_or_store(&mut stmts, outputs[0].clone(), Exp::variable(temp), locals);
        Ok(stmts)
    }

    /// `exp` as a source that supplies only its value: [`Exp::Value`] of a variable. A path
    /// with offsets, `x.[k]`, an address computed from `x`, is first copied to a temp, whose
    /// value is then the source. Constants are returned as they are.
    fn value_of(&mut self, stmts: &mut Vec<Statement>, exp: Exp, locals: &mut Locals) -> Exp {
        match exp {
            Exp::Path(ap) if ap.is_pathless() => Exp::Value(ap),
            exp @ Exp::Path(_) => {
                let t = self.create_temp(locals);
                stmts.push(Statement::new_kind(StatementKind::assign(t.clone(), [exp])));
                Exp::value(t)
            }
            exp => exp,
        }
    }

    /// Writes `dest = base + index` for a `PTRADD` or `PTRSUB` whose offset is not a
    /// constant (R16): a copy of the base plus the value of the index, so `dest` reaches the
    /// memory behind `base` and not the memory behind `index`. Under [`IntArith::Merge`],
    /// `rest` (a `PTRADD`'s element size) is merged in too and `index` is a plain copy, as
    /// for every other arithmetic op in that mode.
    fn push_offset_by_value(
        &mut self,
        stmts: &mut Vec<Statement>,
        dest: AccessPath,
        base: Exp,
        index: Exp,
        rest: Option<Exp>,
        locals: &mut Locals,
    ) {
        let mut sources: SmallVec<[Exp; 2]> = smallvec![base];
        match self.int_arith {
            IntArith::Value => sources.push(self.value_of(stmts, index, locals)),
            IntArith::Merge => {
                sources.push(index);
                sources.extend(rest);
            }
        }
        let temp = self.create_temp(locals);
        stmts.push(Statement::new_kind(StatementKind::assign(
            temp.clone(),
            sources,
        )));
        self.push_assign_or_store(stmts, dest, Exp::variable(temp), locals);
    }

    /// Under [`IntArith::Value`], how each operand of `pcode` flows into its result, by
    /// position. Empty, meaning every operand merges, under the other variants and for ops
    /// that are neither arithmetic nor comparisons.
    ///
    /// The base of an `INT_ADD` or `INT_SUB` whose result is used as an address (see
    /// [`base_operand`]) merges; every other operand of an arithmetic op or comparison supplies
    /// only its value. When an address op has no clear base, every operand merges.
    fn operand_flows(
        &self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Vec<OperandFlow> {
        let m: &str = &pcode.mnemonic;
        if self.int_arith != IntArith::Value || !(is_compare(m) || is_arith(m)) {
            return Vec::new();
        }
        let n = pcode.inputs.len();
        if matches!(m, "INT_ADD" | "INT_SUB") && self.addr_vnodes.contains(&pcode.outputs[0]) {
            let shapes: Vec<OperandShape> = pcode
                .inputs
                .iter()
                .map(|vn| OperandShape {
                    pointer: self.vnode_kinds.get(vn) == Some(&pcode_reader::TypeKind::Pointer),
                    scaled: self.scaled_vnodes.contains(vn)
                        || self.get_const_value(vn, vnode_facts, locals).is_some(),
                })
                .collect();
            return match base_operand(&shapes) {
                Some(base) => (0..n)
                    .map(|i| {
                        if i == base {
                            OperandFlow::Merge
                        } else {
                            OperandFlow::Value
                        }
                    })
                    .collect(),
                None => vec![OperandFlow::Merge; n],
            };
        }
        vec![OperandFlow::Value; n]
    }

    fn handle_ptrsub(
        &mut self,
        pcode: &pcode_reader::PcodeData,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        program: &Program,
        hfunc_facts: &BTreeMap<pcode_reader::HighFunc, pcode_reader::HFuncData>,
        locals: &mut Locals,
    ) -> Result<Vec<Statement>, Error> {
        let outputs: Result<SmallVec<[AccessPath; 1]>, Error> = pcode
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
            // A constant base is a constant offset from the other operand, the address.
            (Some(c), None) => {
                let mut ap = self.get_lvalue(offset_vn, vnode_facts, locals)?;
                ap.push(PathSegment::offset(c));
                let mut stmts = Vec::new();
                let src = Exp::path(self.load_ap(&mut stmts, ap, locals));
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
                Ok(stmts)
            }
            (None, Some(c)) => {
                let mut ap = self.get_lvalue(base_vn, vnode_facts, locals)?;
                ap.push(PathSegment::offset(c));
                let mut stmts = Vec::new();
                let src = Exp::path(self.load_ap(&mut stmts, ap, locals));
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
                Ok(stmts)
            }
            (None, None) => {
                let mut stmts = Vec::new();
                let base_exp = self.get_exp(&mut stmts, base_vn, vnode_facts, locals)?;
                let offset_exp = self.get_exp(&mut stmts, offset_vn, vnode_facts, locals)?;
                self.push_offset_by_value(
                    &mut stmts,
                    outputs[0].clone(),
                    base_exp,
                    offset_exp,
                    None,
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
        let outputs: Result<SmallVec<[AccessPath; 1]>, Error> = pcode
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
                // which is a crashed import, not a wrong offset.
                ap.push(PathSegment::offset(idx_c.wrapping_mul(s_val)));
                let mut stmts = Vec::new();
                let src = Exp::path(self.load_ap(&mut stmts, ap, locals));
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
                Ok(stmts)
            }
            // A constant base with a unit stride is a constant offset from the index, which
            // is the address: `i.[base_c]`.
            (Some(base_c), None) if size_const == Some(1) => {
                let mut ap = self.get_lvalue(index_vn, vnode_facts, locals)?;
                ap.push(PathSegment::offset(base_c));
                let mut stmts = Vec::new();
                let src = Exp::path(self.load_ap(&mut stmts, ap, locals));
                self.push_assign_or_store(&mut stmts, outputs[0].clone(), src, locals);
                Ok(stmts)
            }
            _ => {
                let mut stmts = Vec::new();
                let base_exp = self.get_exp(&mut stmts, base_vn, vnode_facts, locals)?;
                let index_exp = self.get_exp(&mut stmts, index_vn, vnode_facts, locals)?;
                let size_exp = match (self.int_arith, size_const) {
                    (IntArith::Value, _) => None,
                    (IntArith::Merge, Some(s)) => {
                        Some(self.exp_from_const_value(size_vn, vnode_facts, s))
                    }
                    (IntArith::Merge, None) => {
                        Some(self.get_exp(&mut stmts, size_vn, vnode_facts, locals)?)
                    }
                };
                self.push_offset_by_value(
                    &mut stmts,
                    outputs[0].clone(),
                    base_exp,
                    index_exp,
                    size_exp,
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
            let ap = self.resolve_mem_exp(&inputs[0], &inputs[1], vnode_facts, locals)?;
            let output_var = self.get_lvalue(&outputs[0], vnode_facts, locals)?;

            let mut stmts = Vec::new();
            // Materialize the address (loading through any intermediate derefs), then load the
            // value at that address's `deref` field.
            let addr = self.load_ap(&mut stmts, ap, locals);
            let dest = if output_var.is_pathless() {
                output_var.base.clone()
            } else {
                self.create_temp(locals)
            };
            stmts.push(Statement::new_kind(StatementKind::load(
                dest.clone(),
                addr,
                PathSegment::Deref,
            )));
            if !output_var.is_pathless() {
                self.push_assign_or_store(&mut stmts, output_var, Exp::variable(dest), locals);
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
        let inputs = &pcode.inputs;
        if inputs.len() >= 3 {
            // STORE <space>, <offset>, <value>
            let mut dest = self.resolve_mem_exp(&inputs[0], &inputs[1], vnode_facts, locals)?;
            // Store through the address's `deref` field.
            dest.push(PathSegment::Deref);
            let mut stmts = Vec::new();
            let value_exp = self.get_exp(&mut stmts, &inputs[2], vnode_facts, locals)?;

            // Store through the (composed) access path; its base variable is not redefined. Any
            // loads needed to materialize the stored value (above) or intermediate dereferences of
            // the address are emitted first.
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
        // its high variable are per-function, so rooting the access path at either of those would
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

    /// Builds the access path for the canonical stack slot `__stack_top.[offset].deref`.
    fn stack_slot_path(offset: i64, locals: &mut Locals) -> AccessPath {
        let stack_top = VariableRef::new_local_idx(locals.get_or_intern("__stack_top"));
        AccessPath::new(stack_top, [PathSegment::offset(offset), PathSegment::Deref])
    }

    /// Builds the access path for the canonical global `$globals.[address].deref`.
    ///
    /// [`Variable::GlobalHeap`] is threaded through every call, so rooting globals here
    /// is what carries their taint between functions that never pass it directly.
    fn global_path(address: i64) -> AccessPath {
        AccessPath::new(
            VariableRef::new_global(),
            [PathSegment::offset(address), PathSegment::Deref],
        )
    }

    /// Resolve an offset expression using constant propagation results if available.
    /// If offset = x + c, returns an access path for x with the offset `[c]`.
    fn resolve_mem_exp(
        &mut self,
        _space_id: &pcode_reader::PcodeVarnode,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Result<AccessPath, Error> {
        if let Some(prop) = self.cp_results.get(vnode_id).cloned()
            && let pcode_reader::constant_propagation::SymbolicProp::Value(Some(base_vn), offset) =
                prop
        {
            let is_stack = base_vn.deref().deref() == "__stack_top";
            if is_stack {
                let var_ref = VariableRef::new_local_idx(locals.get_or_intern("__stack_top"));
                return Ok(AccessPath::new(var_ref, [PathSegment::offset(offset)]));
            } else if base_vn != *vnode_id {
                let mut ap = self.get_lvalue(&base_vn, vnode_facts, locals)?;
                ap.push(PathSegment::offset(offset));
                return Ok(ap);
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
        let outputs: Result<SmallVec<[AccessPath; 4]>, _> = pcode
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
                    Err(_) => AccessPath::from(VariableRef::new_local_idx(
                        locals.get_or_intern("unknown_callee"),
                    )),
                };
                self.load_ap(&mut stmts, ap, locals)
            } else {
                AccessPath::from(VariableRef::new_local_idx(
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
            self.push_assign_or_store(&mut stmts, o.clone(), Exp::variable(t), locals);
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
            VnodeRep::Var(var) => Exp::variable(var),
            VnodeRep::Offset(var, offset) => {
                let ap = AccessPath::new(var, [PathSegment::offset(offset)]);
                Exp::path(self.load_ap(stmts, ap, locals))
            }
            VnodeRep::StackSlot(offset) => {
                let ap = Self::stack_slot_path(offset, locals);
                Exp::path(self.load_ap(stmts, ap, locals))
            }
            VnodeRep::Global(address) => {
                Exp::path(self.load_ap(stmts, Self::global_path(address), locals))
            }
        };
        Ok(exp)
    }

    /// Reads the memory part of `ap` into a temporary with [`mir::read_into_temp`], appending
    /// the copy to `stmts`, and returns the temporary plus `ap`'s trailing offsets.
    fn load_ap(
        &mut self,
        stmts: &mut Vec<Statement>,
        ap: AccessPath,
        locals: &mut Locals,
    ) -> AccessPath {
        mir::read_into_temp(ap, stmts, || self.create_temp(locals))
    }

    fn get_lvalue(
        &mut self,
        vnode_id: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
        locals: &mut Locals,
    ) -> Result<AccessPath, Error> {
        let rep = self.convert_vnode(vnode_id, vnode_facts, &self.register_facts, locals);
        let ap = match rep {
            VnodeRep::Const(value) => {
                return Err(Error::PcodeConversion(format!(
                    "constant varnode {vnode_id} cannot be used as an lvalue: {value}"
                )));
            }
            VnodeRep::Var(var) => AccessPath::from(var),
            VnodeRep::Offset(var, offset) => AccessPath::new(var, [PathSegment::offset(offset)]),
            VnodeRep::StackSlot(offset) => Self::stack_slot_path(offset, locals),
            VnodeRep::Global(address) => Self::global_path(address),
        };
        Ok(ap)
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

    fn finish(self, mut builders: ProgramInfoBuilder) -> Result<ProgramInfo, Error> {
        if self.int_arith != IntArith::Merge {
            let BinopSources {
                variable,
                value,
                other,
            } = self.binop_sources;
            log::info!(
                "pcode: int arith {:?}: arithmetic and comparison sources: \
                 {variable} variable, {value} value, {other} other",
                self.int_arith
            );
        }
        log::trace!("final program: {}", builders.program);
        // Verify the program
        builders.program.verify()?;
        builders.vmt.finish_names();

        Ok(ProgramInfo {
            program: builders.program,
            vmt: builders.vmt,
            source_info: builders.source_info_builder.finish(),
        })
    }
}

/// How [`Context::handle_binop`] lowers integer arithmetic and comparisons, from
/// `CTADL_EXP_INT_ARITH`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntArith {
    /// `value` (default): most operands supply only their value ([`Exp::Value`]), so a sum or
    /// a comparison is not an alias of the memory behind its operands. [`Context::operand_flows`]
    /// says which operands still merge.
    Value,
    /// `merge`: every operand flows into the result, so the result gets every path of every
    /// operand and an integer added to a pointer becomes a pointer too. For comparison runs.
    Merge,
}

impl IntArith {
    fn from_env() -> Self {
        match std::env::var("CTADL_EXP_INT_ARITH").as_deref() {
            Ok("value") | Err(_) => IntArith::Value,
            Ok("merge") => IntArith::Merge,
            Ok(other) => {
                panic!("CTADL_EXP_INT_ARITH: unknown value {other:?}, expected value or merge")
            }
        }
    }
}

/// Whether `mnemonic` is a comparison or a bool op.
fn is_compare(mnemonic: &str) -> bool {
    matches!(
        mnemonic,
        "INT_EQUAL"
            | "INT_NOTEQUAL"
            | "INT_LESS"
            | "INT_SLESS"
            | "INT_LESSEQUAL"
            | "INT_SLESSEQUAL"
            | "INT_CARRY"
            | "INT_SCARRY"
            | "INT_SBORROW"
            | "BOOL_AND"
            | "BOOL_OR"
            | "BOOL_XOR"
            | "FLOAT_EQUAL"
            | "FLOAT_NOTEQUAL"
            | "FLOAT_LESS"
            | "FLOAT_LESSEQUAL"
            | "FLOAT_NAN"
    )
}

/// Whether `mnemonic` is integer or float arithmetic.
fn is_arith(mnemonic: &str) -> bool {
    matches!(
        mnemonic,
        "INT_ADD"
            | "INT_SUB"
            | "INT_MULT"
            | "INT_DIV"
            | "INT_SDIV"
            | "INT_REM"
            | "INT_SREM"
            | "INT_AND"
            | "INT_OR"
            | "INT_XOR"
            | "INT_LEFT"
            | "INT_RIGHT"
            | "INT_SRIGHT"
            | "FLOAT_ADD"
            | "FLOAT_SUB"
            | "FLOAT_MULT"
            | "FLOAT_DIV"
    )
}

/// How one operand of a `handle_binop` op flows into its result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperandFlow {
    /// The operand is a source as itself ([`Exp::Variable`]): the result gets all its paths.
    Merge,
    /// Only the operand's value flows ([`Exp::Value`]).
    Value,
}

/// What [`base_operand`] knows about an operand of an address `INT_ADD` or `INT_SUB`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct OperandShape {
    /// Ghidra types the operand as a pointer.
    pointer: bool,
    /// The operand is a constant or the output of `INT_MULT` or `INT_LEFT`.
    scaled: bool,
}

/// Which operand of an `INT_ADD` or `INT_SUB` used as an address is the base, the one whose
/// memory the address reaches: the only pointer-typed operand, or else the only one that is not
/// a scaled index. `None` when neither rule picks exactly one.
fn base_operand(shapes: &[OperandShape]) -> Option<usize> {
    let only = |pred: fn(&OperandShape) -> bool| {
        let mut it = shapes.iter().enumerate().filter(|(_, s)| pred(s));
        match (it.next(), it.next()) {
            (Some((i, _)), None) => Some(i),
            _ => None,
        }
    };
    only(|s| s.pointer).or_else(|| only(|s| !s.scaled))
}

/// The number of each kind of source of arithmetic and comparisons in [`Context::handle_binop`].
#[derive(Clone, Copy, Debug, Default)]
struct BinopSources {
    /// Whole-variable sources.
    variable: usize,
    /// [`Exp::Value`] sources.
    value: usize,
    /// Every other source: constants, and address computations (an [`Exp::Path`] with
    /// offsets) that merge.
    other: usize,
}

impl BinopSources {
    fn count(&mut self, exp: &Exp) {
        match exp {
            Exp::Path(ap) if ap.is_pathless() => self.variable += 1,
            Exp::Value(_) => self.value += 1,
            _ => self.other += 1,
        }
    }
}

/// The varnodes used as a memory address: a `LOAD` or `STORE` address, a `PTRADD` or `PTRSUB`
/// base, and, backwards to a fixpoint, the inputs of a copy, cast, phi, `INDIRECT`, `INT_ADD` or
/// `INT_SUB` whose output is one. A call argument or a returned value is not an address use: a
/// value-only operand still carries its taint there.
/// Propagation stops at every other operator, so an index scaled by a multiply or shift is not
/// an address even when the scaled value is added to one.
fn address_vnodes(facts: &pcode_reader::PcodeFacts) -> BTreeSet<pcode_reader::PcodeVarnode> {
    let mut defs: BTreeMap<&pcode_reader::PcodeVarnode, &pcode_reader::PcodeData> = BTreeMap::new();
    let mut work: Vec<&pcode_reader::PcodeVarnode> = Vec::new();
    for op in facts.pcode_facts.values() {
        for out in &op.outputs {
            defs.insert(out, op);
        }
        match &**op.mnemonic {
            "LOAD" | "STORE" => work.extend(op.inputs.get(1)),
            "PTRADD" | "PTRSUB" => work.extend(op.inputs.first()),
            _ => {}
        }
    }
    let mut seen = BTreeSet::new();
    while let Some(vn) = work.pop() {
        if !seen.insert(vn.clone()) {
            continue;
        }
        if let Some(op) = defs.get(vn)
            && matches!(
                &**op.mnemonic,
                "COPY" | "CAST" | "MULTIEQUAL" | "INDIRECT" | "INT_ADD" | "INT_SUB"
            )
        {
            work.extend(op.inputs.iter());
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    const PTR: OperandShape = OperandShape {
        pointer: true,
        scaled: false,
    };
    const SCALED: OperandShape = OperandShape {
        pointer: false,
        scaled: true,
    };
    const PLAIN: OperandShape = OperandShape {
        pointer: false,
        scaled: false,
    };

    /// A register varnode `name`, or a constant varnode when `constant` is given.
    fn vnode(name: &str, constant: Option<i64>) -> pcode_reader::VnodeData {
        pcode_reader::VnodeData {
            name: name.to_string(),
            size: Some(8),
            is_address: false,
            space: Some(
                if constant.is_some() {
                    "const"
                } else {
                    "register"
                }
                .to_string(),
            ),
            address: None,
            constant_offset: constant,
        }
    }

    /// Lowers one `PTRADD` or `PTRSUB` over the register varnodes `p` (the base) and `i`, and
    /// the constant varnodes `four` and `eight`, whose output is the register `q`. `index`
    /// names the second input, and `PTRADD` gets `four` as its element size.
    fn lower_ptr_op(int_arith: IntArith, mnemonic: &str, index: &str) -> (Vec<Statement>, Locals) {
        let mut ctx = Context::new();
        ctx.int_arith = int_arith;
        let vnode_facts: BTreeMap<_, _> = [
            ("p", None),
            ("i", None),
            ("q", None),
            ("four", Some(4)),
            ("eight", Some(8)),
        ]
        .into_iter()
        .map(|(n, c)| (pcode_reader::PcodeVarnode::from(n), vnode(n, c)))
        .collect();
        let mut inputs: SmallVec<[pcode_reader::PcodeVarnode; 2]> =
            smallvec!["p".into(), index.into()];
        if mnemonic == "PTRADD" {
            inputs.push("four".into());
        }
        let pcode = pcode_reader::PcodeData {
            mnemonic: mnemonic.into(),
            opcode: None,
            inputs,
            outputs: smallvec!["q".into()],
            bb_id: None,
            index: 0,
            target: None,
        };
        let mut locals = Locals::default();
        let stmts = match mnemonic {
            "PTRADD" => ctx.handle_ptradd(&pcode, &vnode_facts, &mut locals),
            "PTRSUB" => ctx.handle_ptrsub(
                &pcode,
                &vnode_facts,
                &Program::default(),
                &BTreeMap::new(),
                &mut locals,
            ),
            _ => unreachable!(),
        }
        .unwrap();
        (stmts, locals)
    }

    /// The assignments in `stmts`, as `(dest, sources)`.
    fn assigns(stmts: &[Statement]) -> Vec<(AccessPath, Vec<Exp>)> {
        stmts
            .iter()
            .map(|st| match &st.kind {
                StatementKind::Assign { dest, sources } => (dest.clone(), sources.to_vec()),
                other => panic!("expected an assignment, got {other}"),
            })
            .collect()
    }

    fn var(locals: &mut Locals, name: &str) -> VariableRef {
        VariableRef::new_local_idx(locals.get_or_intern(name))
    }

    #[test]
    fn non_constant_offset_is_a_copy_of_the_base_plus_the_value_of_the_index() {
        for mnemonic in ["PTRADD", "PTRSUB"] {
            let (stmts, mut locals) = lower_ptr_op(IntArith::Value, mnemonic, "i");
            let [(t, t_srcs), (q, q_srcs)] = assigns(&stmts).try_into().unwrap();
            let (p, i) = (var(&mut locals, "p"), var(&mut locals, "i"));
            assert!(t.is_pathless(), "{mnemonic}: {t}");
            // The element size of a `PTRADD` carries nothing and is dropped.
            assert_eq!(t_srcs, [Exp::variable(p), Exp::value(i)], "{mnemonic}");
            assert_eq!(q, AccessPath::from(var(&mut locals, "q")), "{mnemonic}");
            assert_eq!(q_srcs, [Exp::Path(t)], "{mnemonic}");
        }
    }

    #[test]
    fn non_constant_offset_merges_every_operand_under_merge() {
        let (stmts, mut locals) = lower_ptr_op(IntArith::Merge, "PTRADD", "i");
        let [(_, t_srcs), _] = assigns(&stmts).try_into().unwrap();
        let (p, i) = (var(&mut locals, "p"), var(&mut locals, "i"));
        assert_eq!(t_srcs.len(), 3);
        assert_eq!(t_srcs[..2], [Exp::variable(p), Exp::variable(i)]);

        let (stmts, mut locals) = lower_ptr_op(IntArith::Merge, "PTRSUB", "i");
        let [(_, t_srcs), _] = assigns(&stmts).try_into().unwrap();
        let (p, i) = (var(&mut locals, "p"), var(&mut locals, "i"));
        assert_eq!(t_srcs, [Exp::variable(p), Exp::variable(i)]);
    }

    #[test]
    fn constant_offset_is_an_offset_of_the_base() {
        // `PTRADD p, 8, 4` is `p.[32]`; `PTRSUB p, 8` is `p.[8]`.
        for (mnemonic, k) in [("PTRADD", 32), ("PTRSUB", 8)] {
            for int_arith in [IntArith::Value, IntArith::Merge] {
                let (stmts, mut locals) = lower_ptr_op(int_arith, mnemonic, "eight");
                let [(q, q_srcs)] = assigns(&stmts).try_into().unwrap();
                let p = var(&mut locals, "p");
                assert_eq!(q, AccessPath::from(var(&mut locals, "q")));
                assert_eq!(
                    q_srcs,
                    [Exp::path(AccessPath::new(p, [PathSegment::offset(k)]))],
                    "{mnemonic} {int_arith:?}"
                );
            }
        }
    }

    #[test]
    fn base_is_the_pointer_typed_operand() {
        assert_eq!(base_operand(&[PLAIN, PTR]), Some(1));
        assert_eq!(base_operand(&[PTR, PLAIN]), Some(0));
        // The type decides before the shape does.
        let scaled_ptr = OperandShape {
            pointer: true,
            scaled: true,
        };
        assert_eq!(base_operand(&[scaled_ptr, PLAIN]), Some(0));
    }

    #[test]
    fn base_is_the_operand_that_is_not_a_scaled_index() {
        // `a + (i << 3)`, `p + 4`
        assert_eq!(base_operand(&[PLAIN, SCALED]), Some(0));
        assert_eq!(base_operand(&[SCALED, PLAIN]), Some(1));
    }

    #[test]
    fn no_base_when_neither_rule_picks_one() {
        assert_eq!(base_operand(&[PLAIN, PLAIN]), None);
        assert_eq!(base_operand(&[SCALED, SCALED]), None);
        // `p - q`, both pointers: neither is the base.
        assert_eq!(base_operand(&[PTR, PTR]), None);
        assert_eq!(base_operand(&[]), None);
    }
}
