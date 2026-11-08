/*! Ghidra Pcode language front end.

Turns Ghidra pcode facts into CTADL IR. It also holds the `RegisterNatives` scanner, because the
two need each other.

# Conventions

How pcode follows the front-end conventions of S19 in `docs/ir.md`:

- There is no address-of. Address-taken storage lives behind `deref`. The frame is memory:
  frame slot `k` is `__stack_top.[k].deref`, and writing one is a store. `mem2reg` is the only
  thing that makes a slot flow-sensitive.
- An import declares one global, its image's memory, named by the import and Ghidra's address
  space: `$<import>::ram` (written `$ram` below). A global
  at address `A` is `$ram.[A].deref`, keyed on the address so that every function shares it,
  and named by the import so that two images' addresses are distinct cells. A pointer-typed
  constant stays an inert constant: there is no `&g`.
- A constant offset is an offset segment: `PTRSUB p, 8` is `p.[8]`. A non-constant offset is a
  copy of the base plus the value of the index: `q = PTRADD p, i, 4` is `q = p, value(i)`.
  Under `CTADL_EXP_INT_ARITH=merge`, a comparison mode, every operand is merged instead.
- Offsets have no width. `p.[0].deref` and `p.[4].deref` are distinct cells, and a 4-byte write
  that overlaps an 8-byte read is lost.
- A write to a frame address itself, `__stack_top.[k]`, is dropped, because every read of the
  varnode that holds the address resolves to the address too. See
  `Lowerer::push_assign_or_store`.

# Spans

A span's start is an offset from the image base, the address Ghidra loaded the image at
(`PROGRAM_IMAGE_BASE`), not the address itself: an instruction at Ghidra address `A` starts at
`A - image_base`. Source-info positions are `u32`, and a 64-bit image's addresses are not
(a Mach-O executable loads at `0x1_0000_0000`); its offsets are, for any image under 4 GiB. An
instruction whose offset does not fit gets no span, and the import warns. The memory global is
declared at offset 0, the image itself.

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
use ctadl_ir::cir::call::{FunctionNames, VirtualMethodTable};
use ctadl_ir::*;

use pcode_reader::PcodeFactsReader;
use rayon::prelude::*;

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

/// The span start of Ghidra address `address` in an image loaded at `image_base`: its offset
/// from the base (see "Spans" in the crate docs). `None` below the base or 4 GiB or more above it.
fn image_offset(address: i64, image_base: i64) -> Option<u32> {
    u32::try_from(address.checked_sub(image_base)?).ok()
}

/// The name of the memory global of the import `import_name`, `<import>::ram`: the import, then
/// Ghidra's address space.
pub fn memory_global_name(import_name: &str) -> String {
    format!("{import_name}::ram")
}

/// Import pcode facts from an artifact by running Ghidra and then converting the facts.
///
/// `jobs` caps Ghidra's threads; see [`ghidra::run_ghidra_export`].
pub fn import_pcode(
    import: &ctadl_import::project::ArtifactImport,
    jobs: usize,
) -> Result<ProgramInfo, Error> {
    // Run Ghidra to generate facts
    ghidra::run_ghidra_export(&import.artifact_path, &import.import_path(), jobs)?;
    lower_facts(import)
}

/// Everything [`import_pcode`] does after Ghidra has written the import's `facts/` directory, in
/// this process: compresses the fact tables, lowers them to a program, and scans for
/// `RegisterNatives` tables. It can be run again on an existing import's facts, to rerun or
/// profile the in-process work without Ghidra: compressing is idempotent.
pub fn lower_facts(import: &ctadl_import::project::ArtifactImport) -> Result<ProgramInfo, Error> {
    let facts_dir = import.import_path().join("facts");
    // Gzip the fact tables Ghidra just produced while they are read and lowered. The reader
    // reads the plain tables, so they are removed only once it is done.
    let (compressed, program_info) = rayon::join(
        || {
            let started = std::time::Instant::now();
            let compressed = ghidra::compress_facts_keeping_originals(&facts_dir);
            log::debug!(
                target: "ctadl_pcode::timing",
                "compress facts (alongside the phases below): {:.2?}",
                started.elapsed()
            );
            compressed
        },
        || lower_tables(import, &facts_dir),
    );
    let program_info = program_info?;
    ghidra::remove_compressed_originals(&compressed?)?;
    Ok(program_info)
}

/// Lowers the fact tables in `facts_dir` to the program of `import`, and scans for its
/// `RegisterNatives` tables.
fn lower_tables(
    import: &ctadl_import::project::ArtifactImport,
    facts_dir: &Path,
) -> Result<ProgramInfo, Error> {
    let timer = PhaseTimer::start();
    let path = &import.artifact_path;

    // Persist Ghidra's image base on the import config so downstream consumers
    // (SARIF address mapping, regression line checks) can recover
    // section-relative offsets regardless of the base Ghidra chose.
    let image_base = PcodeFactsReader::new(facts_dir)
        .read_image_base()
        .map_err(|e| Error::PcodeFactRead(format!("Failed to read image base: {}", e)))
        .err_context(|| format!("reading pcode facts in: {}", facts_dir.display()))?;
    if let Some(image_base) = image_base {
        let mut updated = import.clone();
        updated.image_base = Some(image_base);
        updated.save()?;
    }
    timer.done("image base");

    let mut ctx = Context::new();
    ctx.import_name = import.name.clone();
    let mut builders = ProgramInfoBuilder::new(VirtualMethodTable::new_native());

    let key = ArtifactKey {
        path: path.to_string_lossy().to_string(),
        sub_artifact_id: 0,
        hash: Vec::new(),
        encoding: source_info::ArtifactEncoding::Binary,
    };

    ctx.process(facts_dir, key, &mut builders, &timer)?;

    // Recover this library's `RegisterNatives` tables and write them beside the import's other
    // artifacts. Here, rather than earlier or later, because this is the first point at which
    // both halves of the address translation are known: Ghidra's image base was read above, and
    // `ctx.process` has just populated the entry-point map every recovered `fnPtr` is looked up
    // in. Scanning is unconditional and costs milliseconds; `--no-jni-registry` ignores the
    // result at index time, which is what makes a clean A/B possible without re-importing.
    jni_registry::scan_import(import, image_base, &ctx.entry_points)?;
    timer.done("jni registry scan");

    let program_info = ctx.finish(builders);
    timer.done("finish");
    program_info
}

/// Logs how long each phase of an import took, under the `ctadl_pcode::timing` target at debug
/// level.
struct PhaseTimer(std::cell::Cell<std::time::Instant>);

impl PhaseTimer {
    fn start() -> Self {
        Self(std::cell::Cell::new(std::time::Instant::now()))
    }

    fn done(&self, phase: &str) {
        let now = std::time::Instant::now();
        let took = now - self.0.replace(now);
        log::debug!(target: "ctadl_pcode::timing", "{phase}: {took:.2?}");
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
    /// global memory `$ram.[A].deref`.
    Global(i64),
}

#[derive(Debug)]
struct Context {
    /// Function mapping: pcode function ID -> CTADL function index
    functions: BTreeMap<pcode_reader::HighFunc, FunctionIdx>,
    /// Basic block mapping: (function ID, pcode block ID) -> CTADL basic block index
    basic_blocks: BTreeMap<(pcode_reader::HighFunc, pcode_reader::PcodeBlockBasic), BasicBlockIdx>,
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
    /// Logged at the end of the import; the sum of every [`Lowerer`]'s.
    binop_sources: BinopSources,
    /// Register facts needed for vnode conversion
    register_facts: Vec<pcode_reader::RegisterData>,
    /// Function entry address -> the fully-qualified IR name of the function there. Keyed by the
    /// address Ghidra reports, which is `image_base + ELF vaddr`. Built alongside `functions` in
    /// `process_functions`.
    entry_points: BTreeMap<i64, String>,
    /// The IR name of the function a call to an address calls: the first function, in id order,
    /// whose entry point or one of whose local entry points is that address. Built by
    /// `process_functions`.
    function_at: hashbrown::HashMap<i64, String>,
    /// The name of the import being lowered, which names its memory global.
    import_name: String,
    /// The import's memory global, `$<import>::ram`, declared by [`Self::process`].
    memory: Option<ctadl_ir::cir::GlobalIdx>,
    /// The address Ghidra loaded the image at, which spans are offsets from (see "Spans" in the
    /// crate docs); 0 when the facts do not say. Read by [`Self::process`].
    image_base: i64,
    /// Instructions with no span because their offset from the image base is not a `u32`.
    unlocated_instructions: usize,
}

impl Context {
    fn new() -> Self {
        Self {
            functions: Default::default(),
            basic_blocks: Default::default(),
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
            entry_points: Default::default(),
            function_at: Default::default(),
            import_name: "pcode".to_string(),
            memory: None,
            image_base: 0,
            unlocated_instructions: 0,
        }
    }

    fn process(
        &mut self,
        facts_dir: &Path,
        artifact_key: ArtifactKey,
        builders: &mut ProgramInfoBuilder,
        timer: &PhaseTimer,
    ) -> Result<(), Error> {
        let reader = PcodeFactsReader::new(facts_dir);
        self.image_base = reader
            .read_image_base()
            .map_err(|e| Error::PcodeFactRead(format!("Failed to read image base: {}", e)))
            .err_context(|| format!("reading pcode facts in: {}", facts_dir.display()))?
            .unwrap_or(0);
        let image_span = builders.source_info_builder.span_for(
            artifact_key.clone(),
            0,
            source_info::SpanLen::Empty,
        );
        self.memory = Some(
            builders
                .program
                .declare_global(memory_global_name(&self.import_name), Some(image_span)),
        );
        let mut pcode_facts = reader
            .read_all_facts()
            .map_err(|e| Error::PcodeFactRead(format!("Failed to read pcode facts: {}", e)))
            .err_context(|| format!("reading pcode facts in: {}", facts_dir.display()))?;
        timer.done("read facts");

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

        // Constant propagation, and the classes of varnodes lowering looks up, are independent.
        let int_arith = self.int_arith;
        let (cp_results, (global_vnodes, value_classes)) = rayon::join(
            || pcode_reader::constant_propagation::compute_constant_propagation(&pcode_facts),
            || {
                rayon::join(
                    || global_vnodes(&pcode_facts),
                    || (int_arith == IntArith::Value).then(|| ValueClasses::of(&pcode_facts)),
                )
            },
        );
        self.cp_results = cp_results;
        self.global_vnodes = global_vnodes;
        if let Some(classes) = value_classes {
            self.vnode_kinds = classes.vnode_kinds;
            self.addr_vnodes = classes.addr_vnodes;
            self.scaled_vnodes = classes.scaled_vnodes;
        }
        timer.done("constant propagation and vnode classes");
        self.register_facts = pcode_facts.register_facts.clone();

        // Pre-pass: Create mapping from function names to HighFunc IDs for functions with pcode
        let name_to_func_map = self.create_name_to_func_mapping(&pcode_facts);

        // 1. Process functions first (metadata only)
        self.process_functions(&pcode_facts, &name_to_func_map, builders)?;
        timer.done("functions");

        // 2. Process basic blocks and map parameters (function by function)
        self.process_all_blocks(&pcode_facts, builders)?;
        timer.done("blocks");

        // 3. Process pcode instructions
        self.process_pcode_instructions(&pcode_facts, artifact_key, builders)?;
        drop_in_background(pcode_facts);
        timer.done("instructions");
        if self.unlocated_instructions > 0 {
            log::warn!(
                "pcode: {}: {} instruction(s) more than 4 GiB from the image base {:#x}, or \
                 below it, have no source location",
                self.import_name,
                self.unlocated_instructions,
                self.image_base
            );
        }

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
                            .push(ctadl_ir::cir::ParameterType::Shared);
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

        for (func_id, func_data) in hfunc_facts {
            let Some(&func_idx) = self.functions.get(func_id) else {
                continue;
            };
            for address in func_data
                .entry_point
                .iter()
                .chain(&func_data.local_entry_points)
            {
                self.function_at
                    .entry(address.0)
                    .or_insert_with(|| builders.program[func_idx].name.clone());
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

        let mut basic_blocks = BTreeMap::new();
        let mut address_to_bb = BTreeMap::new();
        let lowerer = Lowerer::new(self);

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
                            Lowerer::stack_slot_path(addr.0, &mut func.locals)
                        } else {
                            // Other parameter (register, etc.) - bind to local variable
                            lowerer.get_lvalue(rep, &pcode_facts.vnode_facts, &mut func.locals)?
                        };
                        let mut stmts = Vec::new();
                        lowerer.push_assign_or_store(
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
                    basic_blocks.insert((hfunc_id.clone(), bb_id.clone()), bb_idx);
                    if entry_bb_idx.is_none() {
                        entry_bb_idx = Some(bb_idx);
                    }

                    // Map address to this block
                    let bb_data = &pcode_facts.bb_facts[&bb_id];
                    if let Some(first_inst_id) = &bb_data.first_inst
                        && let Some(pcode) = pcode_facts.pcode_facts.get(first_inst_id)
                        && let Some(address) = &pcode.target
                    {
                        address_to_bb.insert(address.0, (hfunc_id.clone(), bb_idx));
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

        self.basic_blocks = basic_blocks;
        self.address_to_bb = address_to_bb;
        Ok(())
    }

    /// Lowers every block's instructions and terminator, then puts them into the program.
    ///
    /// Blocks are lowered in parallel, one job per function (and one per block of no known
    /// function), and placed so that the program is the one lowering them one at a time in
    /// block-id order made: spans are interned and temporaries numbered in that order.
    fn process_pcode_instructions(
        &mut self,
        pcode_facts: &pcode_reader::PcodeFacts,
        artifact_key: ArtifactKey,
        builders: &mut ProgramInfoBuilder,
    ) -> Result<(), Error> {
        /// The blocks of one function, or one block of no known function, with the locals
        /// table they are lowered into.
        struct Job<'f> {
            func: Option<FunctionIdx>,
            blocks: Vec<(&'f pcode_reader::PcodeBlockBasic, &'f pcode_reader::BBData)>,
            return_arity: usize,
            locals: Locals,
        }

        /// A lowered [`Job`].
        struct Lowered<'f> {
            func: Option<FunctionIdx>,
            blocks: Vec<(&'f pcode_reader::PcodeBlockBasic, LoweredBlock)>,
            locals: Locals,
            temps: Vec<LocalIdx>,
        }

        let mut jobs: Vec<Job<'_>> = Vec::new();
        let mut job_of_func: hashbrown::HashMap<FunctionIdx, usize> = Default::default();
        // Each block's job and its place in the job, in block-id order.
        let mut order: Vec<(usize, usize)> = Vec::with_capacity(pcode_facts.bb_facts.len());
        for (bb_id, bb_data) in &pcode_facts.bb_facts {
            let func = self.functions.get(&bb_data.hfunc).copied();
            let job = match func {
                Some(func_idx) => *job_of_func.entry(func_idx).or_insert_with(|| {
                    let func = &mut builders.program[func_idx];
                    jobs.push(Job {
                        func: Some(func_idx),
                        blocks: Vec::new(),
                        return_arity: func.return_type.arity as usize,
                        locals: std::mem::take(&mut func.locals),
                    });
                    jobs.len() - 1
                }),
                None => {
                    jobs.push(Job {
                        func: None,
                        blocks: Vec::new(),
                        return_arity: 0,
                        locals: Locals::default(),
                    });
                    jobs.len() - 1
                }
            };
            order.push((job, jobs[job].blocks.len()));
            jobs[job].blocks.push((bb_id, bb_data));
        }

        let ctx = &*self;
        let lowered: Vec<Result<(Lowered<'_>, BinopSources, usize), Error>> = jobs
            .into_par_iter()
            .map(|job| {
                let mut lowerer = Lowerer::new(ctx);
                let mut locals = job.locals;
                let blocks = job
                    .blocks
                    .iter()
                    .map(|&(bb_id, bb_data)| {
                        let block = lowerer.lower_block(
                            bb_data,
                            pcode_facts,
                            job.return_arity,
                            &mut locals,
                        )?;
                        Ok((bb_id, block))
                    })
                    .collect::<Result<Vec<_>, Error>>()?;
                let Lowerer {
                    temps,
                    binop_sources,
                    unlocated_instructions,
                    ..
                } = lowerer;
                let lowered = Lowered {
                    func: job.func,
                    blocks,
                    locals,
                    temps,
                };
                Ok((lowered, binop_sources, unlocated_instructions))
            })
            .collect();
        let mut jobs = Vec::with_capacity(lowered.len());
        for result in lowered {
            let (lowered, binop_sources, unlocated_instructions) = result?;
            self.binop_sources.add(binop_sources);
            self.unlocated_instructions += unlocated_instructions;
            jobs.push(lowered);
        }

        // Number the temporaries and intern the spans in block-id order.
        let file = builders.source_info_builder.file_for(&artifact_key);
        let mut next_temp = 0;
        for (job, place) in order {
            let block = &mut jobs[job].blocks[place].1;
            block.first_temp = next_temp;
            next_temp += block.temps.len();
            block.span_ids = block
                .spans
                .iter()
                .map(|&offset| {
                    builders.source_info_builder.span_in_file(
                        file,
                        offset,
                        source_info::SpanLen::ByteLen(1),
                    )
                })
                .collect();
        }

        jobs.par_iter_mut().for_each(|job| {
            if job.func.is_some() {
                job.locals = number_temps(&job.locals, &job.temps, &job.blocks);
            }
            for (_, block) in &mut job.blocks {
                for stmt in &mut block.statements {
                    let span = &mut stmt.source_info.span_id;
                    if *span != source_info::NO_SPAN {
                        *span = block.span_ids[span.0 as usize - 1];
                    }
                }
            }
        });

        for job in jobs {
            let Some(func_idx) = job.func else {
                continue;
            };
            let func = &mut builders.program[func_idx];
            func.locals = job.locals;
            for (bb_id, block) in job.blocks {
                let hfunc = &pcode_facts.bb_facts[bb_id].hfunc;
                if let Some(&bb_idx) = self.basic_blocks.get(&(hfunc.clone(), bb_id.clone())) {
                    let bb = &mut func[bb_idx];
                    for stmt in block.statements {
                        bb.statements.push_back(stmt);
                    }
                    bb.terminator = Some(block.terminator);
                }
            }
        }

        Ok(())
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
        drop_in_background(self);
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

/// One block, lowered by [`Lowerer::lower_block`].
struct LoweredBlock {
    statements: Vec<Statement>,
    terminator: Terminator,
    /// The temporaries the block made, as positions in its lowerer's [`Lowerer::temps`].
    temps: std::ops::Range<usize>,
    /// The program-wide number of the block's first temporary.
    first_temp: usize,
    /// The offset of each span the block's statements have, in order. A statement's span id is
    /// its position here plus one until the spans are interned into `span_ids`.
    spans: Vec<u32>,
    span_ids: Vec<source_info::FileSpanId>,
}

/// `locals` with each of `temps`, the temporaries a [`Lowerer`] made, renamed to `temp_<n>`
/// for its program-wide number `n`, which `blocks` say.
fn number_temps(
    locals: &Locals,
    temps: &[LocalIdx],
    blocks: &[(&pcode_reader::PcodeBlockBasic, LoweredBlock)],
) -> Locals {
    let mut numbers = vec![0; temps.len()];
    for (_, block) in blocks {
        for (k, n) in block.temps.clone().enumerate() {
            numbers[n] = block.first_temp + k;
        }
    }
    // Every temporary is a new local, so they come in the order the table holds them.
    let mut temps = temps.iter().zip(numbers).peekable();
    let mut renamed = Locals::default();
    for (idx, _) in locals.iter_enumerated() {
        let new = match temps.next_if(|&(&temp, _)| temp == idx) {
            Some((_, n)) => renamed.get_or_intern(&format!("temp_{n}")),
            None => renamed.get_or_intern(locals.name(idx)),
        };
        debug_assert_eq!(new, idx);
    }
    renamed
}

/// Lowers instructions against a finished [`Context`], keeping what lowering changes, so that
/// functions can be lowered in parallel, each by its own.
struct Lowerer<'a> {
    ctx: &'a Context,
    /// The temporaries made so far, in order (see [`Self::create_temp`]).
    temps: Vec<LocalIdx>,
    binop_sources: BinopSources,
    /// Instructions with no span because their offset from the image base is not a `u32`.
    unlocated_instructions: usize,
}

impl Deref for Lowerer<'_> {
    type Target = Context;

    fn deref(&self) -> &Context {
        self.ctx
    }
}

impl<'a> Lowerer<'a> {
    fn new(ctx: &'a Context) -> Self {
        Self {
            ctx,
            temps: Vec::new(),
            binop_sources: BinopSources::default(),
            unlocated_instructions: 0,
        }
    }

    /// Appends a write of `src` to the access path `dest`: a plain assign for a bare variable, or a
    /// field store (with any needed loads emitted for intermediate dereferences) via
    /// [`cir::store_access_path`].
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
        &self,
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

    /// Lowers the instructions of the block `bb_data` of a function returning `return_arity`
    /// values, and its terminator.
    fn lower_block(
        &mut self,
        bb_data: &pcode_reader::BBData,
        pcode_facts: &pcode_reader::PcodeFacts,
        return_arity: usize,
        locals: &mut Locals,
    ) -> Result<LoweredBlock, Error> {
        let first_temp = self.temps.len();
        let mut statements = Vec::new();
        let mut spans = Vec::new();

        // Use the sorted instruction indices from BBData
        for (_, inst_id) in &bb_data.instruction_indices {
            if let Some(pcode) = pcode_facts.pcode_facts.get(inst_id) {
                for mut stmt in self.pcode_to_statement(pcode, &pcode_facts.vnode_facts, locals)? {
                    if let Some(addr) = &pcode.target {
                        match image_offset(addr.0, self.image_base) {
                            Some(offset) => {
                                spans.push(offset);
                                stmt.source_info =
                                    SourceInfo::new(source_info::FileSpanId(spans.len() as u32));
                            }
                            None => self.unlocated_instructions += 1,
                        }
                    }
                    statements.push(stmt);
                }
            }
        }

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
                        // must execute before the terminator, so append them to the end of
                        // this block's statements.
                        let arg = self.get_exp(
                            &mut statements,
                            &pcode.inputs[1],
                            &pcode_facts.vnode_facts,
                            locals,
                        )?;
                        args.push(arg);
                    }
                    // Ensure return arity matches
                    while args.len() < return_arity {
                        args.push(Exp::new_bytes(Vec::new()));
                    }
                    args.truncate(return_arity);
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
                        terminator = Some(Terminator::new_kind(TerminatorKind::Goto { targets }));
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
                        terminator = Some(Terminator::new_kind(TerminatorKind::Goto { targets }));
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
            while args.len() < return_arity {
                args.push(Exp::new_bytes(Vec::new()));
            }
            Terminator::new_kind(TerminatorKind::Return { args })
        });

        Ok(LoweredBlock {
            statements,
            terminator,
            temps: first_temp..self.temps.len(),
            first_temp: 0,
            spans,
            span_ids: Vec::new(),
        })
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
            "CALL" | "CALLIND" => self.handle_call_operation(pcode, vnode_facts, locals),
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
                self.handle_binop(pcode, vnode_facts, locals)
            }
            "PTRSUB" => self.handle_ptrsub(pcode, vnode_facts, locals),
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
    /// constant (S19): a copy of the base plus the value of the index, so `dest` reaches the
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
            if let Some(func_name) = self.function_at.get(&addr).cloned() {
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
    /// Global storage is canonicalized the same way, as `$ram.[A].deref` (a
    /// [`VnodeRep::Global`]). Globals are rooted at the import's memory global rather than at
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

    /// Builds the access path for the canonical global `$ram.[address].deref`.
    ///
    /// Every function names the one memory global, which is shared by every instance, so
    /// rooting globals here is what carries their taint between functions that never pass it
    /// directly.
    fn global_path(&self, address: i64) -> AccessPath {
        let memory = self
            .memory
            .expect("the memory global is declared before any instruction is lowered");
        AccessPath::new(
            VariableRef::global(memory),
            [PathSegment::offset(address), PathSegment::Deref],
        )
    }

    /// Resolve an offset expression using constant propagation results if available.
    /// If offset = x + c, returns an access path for x with the offset `[c]`.
    fn resolve_mem_exp(
        &self,
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
                self.resolve_call_target(target_vnode, vnode_facts)
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
            ctadl_ir::cir::call::CallStyle::FuncPtrCall {
                callee,
                signature: None,
            }
        } else {
            ctadl_ir::cir::call::CallStyle::DirectCall {
                call_edges: ctadl_ir::cir::call::CallEdges::Explicit(call_edges),
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

    /// Try to resolve call target by checking if the address matches any function entry points or local entry points
    fn resolve_call_target(
        &self,
        target_vnode: &pcode_reader::PcodeVarnode,
        vnode_facts: &BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::VnodeData>,
    ) -> ctadl_ir::ThinVec<String> {
        let address = if let Some(vnode_data) = vnode_facts.get(target_vnode) {
            vnode_data.address.as_ref().map(|addr| addr.0)
        } else {
            None
        };

        if let Some(addr) = address
            && let Some(name) = self.function_at.get(&addr).cloned()
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
                Exp::path(self.load_ap(stmts, self.global_path(address), locals))
            }
        };
        Ok(exp)
    }

    /// Reads the memory part of `ap` into a temporary with [`cir::read_into_temp`], appending
    /// the copy to `stmts`, and returns the temporary plus `ap`'s trailing offsets.
    fn load_ap(
        &mut self,
        stmts: &mut Vec<Statement>,
        ap: AccessPath,
        locals: &mut Locals,
    ) -> AccessPath {
        cir::read_into_temp(ap, stmts, || self.create_temp(locals))
    }

    fn get_lvalue(
        &self,
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
            VnodeRep::Global(address) => self.global_path(address),
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

    /// A new temporary. Temporaries are numbered across the whole program, which a lowerer of
    /// one function cannot know, so it gives each a name that no varnode has and records it;
    /// [`Context::process_pcode_instructions`] renames them once every block is lowered.
    fn create_temp(&mut self, locals: &mut Locals) -> VariableRef {
        let idx = locals.get_or_intern(&format!("\0temp{}", self.temps.len()));
        self.temps.push(idx);
        VariableRef::new_local_idx(idx)
    }
}

/// Drops `value` on the rayon pool, off the import's critical path: dropping the facts
/// releases millions of interned strings.
fn drop_in_background<T: Send + 'static>(value: T) {
    rayon::spawn(move || drop(value));
}

/// How [`Lowerer::handle_binop`] lowers integer arithmetic and comparisons, from
/// `CTADL_EXP_INT_ARITH`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IntArith {
    /// `value` (default): most operands supply only their value ([`Exp::Value`]), so a sum or
    /// a comparison is not an alias of the memory behind its operands. [`Lowerer::operand_flows`]
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

/// The number of each kind of source of arithmetic and comparisons in [`Lowerer::handle_binop`].
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
    fn add(&mut self, other: BinopSources) {
        self.variable += other.variable;
        self.value += other.value;
        self.other += other.other;
    }

    fn count(&mut self, exp: &Exp) {
        match exp {
            Exp::Path(ap) if ap.is_pathless() => self.variable += 1,
            Exp::Value(_) => self.value += 1,
            _ => self.other += 1,
        }
    }
}

/// The varnodes Ghidra classifies as storage for a global variable; see
/// [`pcode_reader::PcodeFacts::is_global_vnode`].
fn global_vnodes(facts: &pcode_reader::PcodeFacts) -> BTreeSet<pcode_reader::PcodeVarnode> {
    let globals: Vec<_> = facts
        .vnode_facts
        .par_iter()
        .filter(|(vn, _)| facts.is_global_vnode(vn))
        .map(|(vn, _)| vn.clone())
        .collect();
    globals.into_iter().collect()
}

/// What lowering under [`IntArith::Value`] needs to know about varnodes; the fields of
/// [`Context`] of the same names say what each is.
struct ValueClasses {
    vnode_kinds: BTreeMap<pcode_reader::PcodeVarnode, pcode_reader::TypeKind>,
    addr_vnodes: BTreeSet<pcode_reader::PcodeVarnode>,
    scaled_vnodes: BTreeSet<pcode_reader::PcodeVarnode>,
}

impl ValueClasses {
    fn of(facts: &pcode_reader::PcodeFacts) -> Self {
        let (vnode_kinds, (addr_vnodes, scaled_vnodes)) = rayon::join(
            || {
                let kinds: Vec<_> = facts
                    .vnode_facts
                    .par_iter()
                    .filter_map(|(vn, _)| Some((vn.clone(), facts.vnode_type_kind(vn)?)))
                    .collect();
                kinds.into_iter().collect()
            },
            || {
                rayon::join(
                    || address_vnodes(facts),
                    || {
                        facts
                            .pcode_facts
                            .values()
                            .filter(|op| matches!(&**op.mnemonic, "INT_MULT" | "INT_LEFT"))
                            .flat_map(|op| op.outputs.iter().cloned())
                            .collect()
                    },
                )
            },
        );
        Self {
            vnode_kinds,
            addr_vnodes,
            scaled_vnodes,
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
    let mut defs: hashbrown::HashMap<&pcode_reader::PcodeVarnode, &pcode_reader::PcodeData> =
        Default::default();
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
    let mut seen = hashbrown::HashSet::new();
    while let Some(vn) = work.pop() {
        if !seen.insert(vn) {
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
    seen.into_iter().cloned().collect()
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
        let mut lowerer = Lowerer::new(&ctx);
        let stmts = match mnemonic {
            "PTRADD" => lowerer.handle_ptradd(&pcode, &vnode_facts, &mut locals),
            "PTRSUB" => lowerer.handle_ptrsub(&pcode, &vnode_facts, &mut locals),
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

    /// A span starts at the offset from the image base, which a 64-bit image's addresses have
    /// even where the addresses are not `u32`s.
    #[test]
    fn a_span_starts_at_the_offset_from_the_image_base() {
        // A Mach-O executable.
        assert_eq!(image_offset(0x1_0000_1000, 0x1_0000_0000), Some(0x1000));
        // An ELF shared library at Ghidra's base.
        assert_eq!(image_offset(0x10_1234, 0x10_0000), Some(0x1234));
        assert_eq!(image_offset(0x1234, 0), Some(0x1234));
        assert_eq!(image_offset(0xf_ffff, 0x10_0000), None);
        assert_eq!(image_offset(0x1_0010_0000, 0x10_0000), None);
        assert_eq!(image_offset(i64::MIN, 1), None);
    }

    /// An image's memory global is declared at the image, offset 0 from its base.
    #[test]
    fn the_memory_global_is_declared_at_the_image() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Every table the reader reads, empty, and the image base.
        for name in [
            ".facts",
            "BB_FIRST.facts",
            "BB_FOUT.facts",
            "BB_HFUNC.facts",
            "BB_LAST.facts",
            "BB_OUT.facts",
            "BB_PCODE_INDEX.facts",
            "BB_START.facts",
            "BB_TOUT.facts",
            "HFUNC_EP.facts",
            "HFUNC_ISEXT.facts",
            "HFUNC_LOCAL_EP.facts",
            "HFUNC_NAME.facts",
            "HFUNC_PROTO.facts",
            "HVAR_CLASS.facts",
            "HVAR_NAME.facts",
            "HVAR_REPRESENTATIVE.facts",
            "HVAR_TYPE.facts",
            "PCODE_INDEX.facts",
            "PCODE_INPUT.facts",
            "PCODE_MNEMONIC.facts",
            "PCODE_OPCODE.facts",
            "PCODE_OUTPUT.facts",
            "PCODE_PARENT.facts",
            "PCODE_TARGET.facts",
            "PROGRAM_IMAGE_BASE.facts",
            "PROTO_IS_VARARG.facts",
            "PROTO_IS_VOID.facts",
            "PROTO_PARAMETER.facts",
            "PROTO_PARAMETER_COUNT.facts",
            "PROTO_RETTYPE.facts",
            "REGISTER_IS_SP.facts",
            "REGISTER_OFF_NAME.facts",
            "SYMBOL_HVAR.facts",
            "TYPE_BOOLEAN.facts",
            "TYPE_ENUM.facts",
            "TYPE_FLOAT.facts",
            "TYPE_INTEGER.facts",
            "TYPE_POINTER.facts",
            "VNODE_ADDRESS.facts",
            "VNODE_HVAR.facts",
            "VNODE_IS_ADDRESS.facts",
            "VNODE_NAME.facts",
            "VNODE_OFFSET_N.facts",
            "VNODE_SIZE.facts",
            "VNODE_SPACE.facts",
        ] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        std::fs::write(dir.path().join("PROGRAM_IMAGE_BASE.facts"), "1048576\n").unwrap();

        let mut ctx = Context::new();
        ctx.import_name = "lib.so".to_string();
        let mut builders = ProgramInfoBuilder::new(VirtualMethodTable::new_native());
        let key = ArtifactKey {
            path: "lib.so".to_string(),
            sub_artifact_id: 0,
            hash: Vec::new(),
            encoding: source_info::ArtifactEncoding::Binary,
        };
        ctx.process(dir.path(), key, &mut builders, &PhaseTimer::start())
            .expect("the facts lower");
        let info = ctx.finish(builders).expect("the program finishes");
        let memory = info
            .program
            .globals
            .lookup(&memory_global_name("lib.so"))
            .expect("the memory global");
        let span = info
            .program
            .globals
            .get(memory)
            .unwrap()
            .span
            .expect("a span");
        let (artifact, span) = info.source_info.resolve(span).expect("the span resolves");
        assert_eq!(artifact.canonical_path, "lib.so");
        assert_eq!(span.start, 0);
        assert_eq!(span.len, source_info::SpanLen::Empty);
    }

    /// An image's memory is one global named by the import and the address space, so a
    /// global at address `A` is `$<import>::ram.[A].deref` and two images' addresses are two
    /// cells.
    #[test]
    fn a_global_address_is_a_cell_of_the_import_memory() {
        let mut program = Program::default();
        let mut paths = Vec::new();
        for import in ["libA.so", "libB.so"] {
            let mut ctx = Context::new();
            ctx.import_name = import.to_string();
            ctx.memory = Some(program.declare_global(memory_global_name(import), None));
            paths.push(Lowerer::new(&ctx).global_path(4096));
        }
        assert_ne!(paths[0], paths[1]);
        let _names = program.global_names_guard();
        assert_eq!(paths[0].to_string(), r"$libA\.so::ram.[4096].deref");
        assert_eq!(paths[1].to_string(), r"$libB\.so::ram.[4096].deref");
    }
}
