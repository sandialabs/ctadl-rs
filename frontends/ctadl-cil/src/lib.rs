/*! .NET (CIL) language front end.

Turns .NET assemblies into CTADL IR, through `cil-reader`'s dataflow layer
([`cil_reader::flow::MethodFlow`]). The reader describes what CIL does; this crate decides how
CTADL says it. It consumes only `MethodFlow` (blocks of `Effect<Var>`, `CallKind`, `Control`,
stack kinds) and never matches on `Code`.

Each step is a total mapping written down as a table in its file's module docs, and each
table is one `match` with no `_` arm, so a review compares a row to an arm:

| File           | Owns                                                              |
| -------------- | ----------------------------------------------------------------- |
| `lib.rs`       | [`import_path`], [`Context`], assembly selection                  |
| `names.rs`     | every spelling: classes, methods, descriptors, fields             |
| `hierarchy.rs` | VMT rows and override aliases                                     |
| `method.rs`    | `FunctionData` shape, variables, blocks, terminators              |
| `effects.rs`   | `Effect<Var>` to statements                                       |
| `ptr.rs`       | managed pointers                                                  |
| `calls.rs`     | calls, delegates, extern stubs                                    |
| `lines.rs`     | spans: source lines from the portable PDB, else IL bytes          |

The IR is naive: one obvious statement per effect, every copy kept, no merging of
temporaries. `ssa::run_pipeline_with_threads` in `ctadl index` does the cleanup (copy
coalescing, pruned SSA, copy propagation), and gives each definition of a depth-named stack
variable `s{d}` its own version, so "which object does this `stfld` write to" is answered by
copy propagation.

Every lowering choice the IR does not force is a policy, made in one place:

| Policy                         | Choice                                                     | Where         |
| ------------------------------ | ---------------------------------------------------------- | ------------- |
| Pointer lowering               | the traced place, else `PathSegment::Deref`; byref arguments copied in and out | `ptr.rs` |
| Array index                    | does not taint the element read (as Dex and JVM)           | `ptr.rs`, `effects.rs` |
| Branch conditions              | `Test` emits nothing                                       | `effects.rs`  |
| `box`                          | contents-preserving assign; the boxed type instantiated    | `effects.rs`  |
| Value types                    | lowered like objects, so two copies of a struct alias      | `effects.rs`  |
| Non-virtual or sealed `callvirt` | `DirectCall`                                             | `calls.rs`    |
| Generic instantiation          | erased: a class is its open definition                     | `names.rs`    |
| Assembly selection             | framework and BCL not imported by default ([`is_framework_assembly`]); their metadata always read | `lib.rs` |

Conventions (S19). A field is the symbol `<cls::name>` and an array element the field
`[]`, whatever the index; `length` is the array length. A class that owns a static field is a
global named by the class (`$[asm]Namespace.Type`), and
a static field is that global's field named by the field's name alone, `$[asm]Namespace.Type.f`;
a field whose owner does not decode is a field of the global `[?]?` named by its token. A managed
pointer to a static field resolves to the same path. The step from a `ref`, `out`, `in`, a
managed or an unmanaged pointer to the memory there is `PathSegment::Deref`, and only that.
A byref argument whose place is traced is copied into a cell (`cell.deref`) before the call
and copied back out after it. Arithmetic, bitwise operations, shifts, conversions and
comparisons supply values only (`value(v)`); the pointer base of `add`, `sub` and
`conv.i`/`conv.u` is a copy (`effects.rs`). An exception is the last result of every call and
the last value of every return: exceptions that may leave a method accumulate in its local
`excout`. Value types are lowered like objects, so two copies of a struct alias
(docs/limitations.md).

Like the Dex front end, a [`Context`] is made once, given the assemblies with
`Context::process`, and turned into one [`ProgramInfo`] by [`Context::finish`]: a
multi-assembly application has one hierarchy. Method bodies are lowered in parallel (see
`Context::process`). Referenced assemblies that are not imported (the framework, by default) are still read
for their metadata, from the input's directory and the framework's
([`ImportOptions::reference_dirs`]): base types' virtual methods, type forwarders, sealed and
virtual flags. Nothing of theirs is lowered.

An assembly's portable PDB (beside it, embedded, or in its own metadata) gives its instructions
source lines; see `lines.rs`.
*/

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cil_reader::Module;
use cil_reader::model::{AssemblyCache, DirectoryLoader, MethodDef, MethodSig, PdbSource, TypeDef};
use ctadl_import::error::{Error, ErrorContext};
use ctadl_ir::call::{
    CilClass, CilMethod, CilSignature, CilSimpleName, Dispatch, FunctionNames, VirtualMethodTable,
};
use ctadl_ir::*;
use hashbrown::HashMap;
use rayon::prelude::*;
use smallvec::SmallVec;
use source_info::{ArtifactKey, SourceInfoBuilder};

use lines::{LineStats, ModuleSpans};

mod calls;
mod effects;
pub mod hierarchy;
pub mod lines;
pub mod method;
pub mod names;
mod ptr;

#[cfg(test)]
mod tests;

use names::{MethodKey, ModNames, Namer};

/// The environment variable that overrides where referenced assemblies' metadata is read
/// from: a path list, like `PATH`.
pub const REFERENCE_DIRS_ENV: &str = "CTADL_CIL_REFERENCE_DIRS";

/// The prefix of a delegate thunk's name (`calls.rs`).
pub const THUNK_PREFIX: &str = "ctadl$delegate$";

/// Settings for an import.
#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Import framework and BCL assemblies found in a directory too, rather than only
    /// reading their metadata. Off by default: the default models are to cover the framework,
    /// and a self-contained publish holds some 200 of them.
    pub include_framework: bool,
    /// Where referenced assemblies' metadata is read from, after the input's own directory.
    /// [`ImportOptions::default`] takes `$CTADL_CIL_REFERENCE_DIRS`, else the shared framework
    /// of the `dotnet` on `PATH`.
    pub reference_dirs: Vec<PathBuf>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            include_framework: false,
            reference_dirs: framework_reference_dirs(),
        }
    }
}

/// Imports a `.dll`/`.exe`, or every application assembly in a directory.
pub fn import_path<P: AsRef<Path>>(path: P) -> Result<ProgramInfo, Error> {
    import_path_with(path, &ImportOptions::default())
}

/// [`import_path`] with settings.
pub fn import_path_with<P: AsRef<Path>>(
    path: P,
    opts: &ImportOptions,
) -> Result<ProgramInfo, Error> {
    let path = path.as_ref();
    if path.is_dir() {
        import_directory(path, opts)
    } else {
        import_assembly(path, opts)
    }
}

/// Imports one assembly.
pub fn import_assembly(path: &Path, opts: &ImportOptions) -> Result<ProgramInfo, Error> {
    import_files(&[path.to_path_buf()], opts)
}

/// Imports the application assemblies of a directory (a published app): every CLI image in
/// it that is not part of the framework, unless [`ImportOptions::include_framework`].
pub fn import_directory(dir: &Path, opts: &ImportOptions) -> Result<ProgramInfo, Error> {
    let mut candidates = Vec::new();
    let entries =
        std::fs::read_dir(dir).err_context(|| format!("reading directory {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "dll" | "exe") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !opts.include_framework && is_framework_assembly(stem) {
            log::debug!("cil: skipping framework assembly {}", path.display());
            continue;
        }
        candidates.push(path);
    }
    let mut files: Vec<PathBuf> = candidates
        .into_par_iter()
        .filter(|path| is_cli_image(path))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(Error::NothingToImport {
            message: format!("no application assemblies in {}", dir.display()),
        });
    }
    import_files(&files, opts)
}

/// Imports these assemblies into one program.
pub fn import_files(files: &[PathBuf], opts: &ImportOptions) -> Result<ProgramInfo, Error> {
    Ok(import_files_with_stats(files, opts)?.0)
}

/// [`import_files`], with the import's counts.
pub fn import_files_with_stats(
    files: &[PathBuf],
    opts: &ImportOptions,
) -> Result<(ProgramInfo, Stats), Error> {
    let timer = PhaseTimer::start();
    // Collected whole and then checked in order, so that the error reported is the first file's.
    let datas = files
        .par_iter()
        .map(|f| std::fs::read(f).err_context(|| format!("reading {}", f.display())))
        .collect::<Vec<_>>()
        .into_iter()
        .collect::<Result<Vec<_>, Error>>()?;
    let modules = files
        .par_iter()
        .zip(&datas)
        .map(|(f, data)| Module::read(data).err_context(|| format!("reading {}", f.display())))
        .collect::<Vec<_>>()
        .into_iter()
        .collect::<Result<Vec<_>, Error>>()?;
    // Symbols are optional: one that does not read costs the assembly its lines, not the import.
    let pdb_sources: Vec<Option<PdbSource>> = files
        .par_iter()
        .zip(&modules)
        .map(|(f, module)| {
            PdbSource::locate(module, f).unwrap_or_else(|e| {
                log::warn!("cil: {}: symbols: {e}", f.display());
                None
            })
        })
        .collect();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for f in files {
        if let Some(d) = f.parent() {
            let d = if d.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                d.to_path_buf()
            };
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    dirs.extend(opts.reference_dirs.iter().cloned());
    let loader = DirectoryLoader::new(dirs);
    let cache = AssemblyCache::new(&loader);
    let namer = Namer::new(&cache);
    let assemblies: Vec<Assembly<'_, '_, '_, '_>> = files
        .par_iter()
        .zip(&modules)
        .zip(&pdb_sources)
        .enumerate()
        .map(|(i, ((f, module), source))| {
            let key = ArtifactKey {
                path: f.to_string_lossy().to_string(),
                sub_artifact_id: i.try_into().unwrap_or(u32::MAX),
                hash: Vec::new(),
                encoding: source_info::ArtifactEncoding::Binary,
            };
            let pdb = source.as_ref().and_then(|s| match s.read(module) {
                Ok(pdb) => Some(pdb),
                Err(e) => {
                    log::warn!("cil: {}: symbols ({}): {e}", f.display(), s.describe());
                    None
                }
            });
            Assembly {
                path: f,
                names: ModNames::new(module, &namer),
                spans: ModuleSpans::new(key, pdb),
                resolved: calls::Resolved::default(),
            }
        })
        .collect();
    timer.done("reading assemblies");
    let mut ctx = Context::new();
    ctx.process(&assemblies)?;
    for a in assemblies {
        ctx.out.stats.lines.add(a.spans.into_stats());
    }
    // The program owns everything it needs: the assemblies and what was read for them go before
    // it is finished.
    drop(namer);
    drop(cache);
    drop(pdb_sources);
    drop(modules);
    drop(datas);
    let timer = PhaseTimer::start();
    let finished = ctx.finish_with_stats();
    timer.done("finishing");
    finished
}

/// Logs how long each phase of an import took, under the `ctadl_cil::timing` target at debug
/// level.
struct PhaseTimer(std::cell::Cell<std::time::Instant>);

impl PhaseTimer {
    fn start() -> Self {
        Self(std::cell::Cell::new(std::time::Instant::now()))
    }

    fn done(&self, phase: &str) {
        let now = std::time::Instant::now();
        let took = now - self.0.replace(now);
        log::debug!(target: "ctadl_cil::timing", "{phase}: {took:.2?}");
    }
}

/// An assembly being imported.
struct Assembly<'n, 'm, 'l, 'p> {
    path: &'n Path,
    names: ModNames<'n, 'm, 'm, 'l>,
    spans: ModuleSpans<'p>,
    resolved: calls::Resolved,
}

/// What [`scan_type`] reads of a type: what the bookkeeping needs that takes naming or
/// decoding. Where reading fails, the error is kept for the bookkeeping to meet where it would
/// have read it.
struct TypeScan<'m> {
    td: TypeDef<'m>,
    cls: String,
    /// The classes of its base type and interfaces.
    parents: cil_reader::Result<Vec<String>>,
    aliases: hierarchy::Aliases,
    /// Each method, with its key if its name decodes, and its signature.
    methods: cil_reader::Result<Vec<ScannedMethod<'m>>>,
}

struct ScannedMethod<'m> {
    md: MethodDef<'m>,
    key: Option<MethodKey>,
    sig: cil_reader::Result<MethodSig>,
}

/// Reads a type for [`Context::collect_type`], on its own.
fn scan_type<'m>(names: &ModNames<'_, 'm, 'm, '_>, td: TypeDef<'m>) -> TypeScan<'m> {
    let parents = || -> cil_reader::Result<Vec<String>> {
        let mut parents = Vec::new();
        if let Some(base) = td.base_type()? {
            parents.extend(names.tdor_cls(&base));
        }
        for i in td.interfaces()? {
            if let Some(iface) = i.interface {
                parents.extend(names.tdor_cls(&iface));
            }
        }
        Ok(parents)
    };
    TypeScan {
        td,
        cls: names.typedef_cls(&td),
        parents: parents(),
        aliases: hierarchy::aliases(names.namer, names.module, td),
        methods: td.methods().map(|methods| {
            methods
                .into_iter()
                .map(|md| ScannedMethod {
                    md,
                    key: names.method_key(md.token()),
                    sig: md.signature(),
                })
                .collect()
        }),
    }
}

/// An override bridge's target, recorded as a callee before the job at `before_job` is placed:
/// where the bookkeeping met it.
struct PendingCallee {
    before_job: usize,
    key: MethodKey,
    params: Vec<ParameterType>,
}

/// A method body for [`lower_job`] to lower, from [`Context::collect_type`].
struct Job<'m> {
    /// The assembly, by position.
    assembly: usize,
    fidx: FunctionIdx,
    fid: String,
    md: MethodDef<'m>,
    sig: MethodSig,
}

/// What [`lower_job`] made of a [`Job`]: the function, with statements' span ids indexing
/// `spans` (1 + position) and globals indexing `globals`; and what lowering recorded.
struct Lowered {
    assembly: usize,
    fidx: FunctionIdx,
    body: FunctionData,
    spans: lines::SpanList,
    globals: ctadl_ir::cir::Globals,
    out: Collected,
}

/// Lowers a method on its own, for [`Context::place`] to put into the program.
fn lower_job(assembly: &Assembly<'_, '_, '_, '_>, job: &Job<'_>) -> Result<Lowered, Error> {
    let body = job
        .md
        .body()
        .err_context(|| format!("converting {}", assembly.path.display()))?;
    let mut fdat = FunctionData::default();
    method::shape(&mut fdat, job.fid.clone(), method::parameters(&job.sig));
    let mut lines = assembly
        .spans
        .method(job.md.token().rid(), body.code_file_offset());
    let mut globals = ctadl_ir::cir::Globals::default();
    let mut out = Collected::default();
    method::lower_body(
        &assembly.names,
        &job.md,
        &body,
        &job.sig,
        &mut fdat,
        &mut globals,
        &mut lines,
        &assembly.resolved,
        &mut out,
    );
    fdat.locals.drop_index();
    Ok(Lowered {
        assembly: job.assembly,
        fidx: job.fidx,
        body: fdat,
        spans: lines.finish(),
        globals,
        out,
    })
}

/// Rewrites a function [`lower_job`] lowered to the program's span ids and globals.
struct Resolve<'a> {
    spans: &'a [source_info::FileSpanId],
    globals: &'a [ctadl_ir::cir::GlobalIdx],
}

impl ctadl_ir::cir::visit::MutVisitor for Resolve<'_> {
    fn visit_statement(&mut self, statement: &mut Statement, location: ctadl_ir::cir::Location) {
        let span = &mut statement.source_info.span_id;
        if span.0 != 0 {
            *span = self.spans[span.0 as usize - 1];
        }
        self.super_statement(statement, location);
    }

    fn visit_variable_ref(&mut self, variable_ref: &mut VariableRef) {
        if let Variable::Global(g) = &mut variable_ref.variable {
            *g = self.globals[g.index()];
        }
    }
}

/// Whether the file is a PE image with CLI metadata, as opposed to a native one.
pub fn is_cli_image(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|data| Module::read(&data).is_ok())
}

/// Whether a directory holds an assembly with CLI metadata.
pub fn dir_has_cli_image(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let p = e.path();
        matches!(p.extension().and_then(|e| e.to_str()), Some("dll" | "exe")) && is_cli_image(&p)
    })
}

/// The assembly-selection policy: whether an assembly of this simple name belongs to
/// the framework or the BCL.
pub fn is_framework_assembly(name: &str) -> bool {
    const EXACT: [&str; 6] = [
        "mscorlib",
        "netstandard",
        "System",
        "WindowsBase",
        "Microsoft.VisualBasic",
        "Microsoft.CSharp",
    ];
    EXACT.contains(&name)
        || name.starts_with("System.")
        || name.starts_with("Microsoft.NETCore.")
        || name.starts_with("Microsoft.Win32.")
        || name.starts_with("Microsoft.Extensions.")
        || name.starts_with("Microsoft.AspNetCore.")
}

/// Where the framework's assemblies are: `$CTADL_CIL_REFERENCE_DIRS`, else the newest
/// `shared/Microsoft.NETCore.App/<version>` of the `dotnet` on `PATH` (or `$DOTNET_ROOT`).
pub fn framework_reference_dirs() -> Vec<PathBuf> {
    if let Some(dirs) = std::env::var_os(REFERENCE_DIRS_ENV) {
        return std::env::split_paths(&dirs).collect();
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(root) = std::env::var_os("DOTNET_ROOT") {
        roots.push(PathBuf::from(root));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let exe = dir.join("dotnet");
            let Ok(exe) = std::fs::canonicalize(&exe) else {
                continue;
            };
            if let Some(bin) = exe.parent() {
                roots.push(bin.to_path_buf());
                if let Some(prefix) = bin.parent() {
                    roots.push(prefix.join("share/dotnet"));
                }
            }
            break;
        }
    }
    for root in roots {
        let shared = root.join("shared/Microsoft.NETCore.App");
        let Ok(entries) = std::fs::read_dir(&shared) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        versions.sort_by_key(|p| version_key(p));
        if let Some(newest) = versions.pop() {
            return vec![newest];
        }
    }
    Vec::new()
}

fn version_key(p: &Path) -> Vec<u64> {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .split(['.', '-'])
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

/// What lowering records beyond each function's body: the callees it saw, the delegate
/// thunks it needs, and counts for the import summary.
#[derive(Debug, Default)]
pub struct Collected {
    /// Every callee, by function id, with the shape its stub gets if nothing defines it.
    pub ext: HashMap<String, (MethodKey, Vec<ParameterType>)>,
    /// Thunk function id -> the static method it calls.
    pub thunks: BTreeMap<String, String>,
    /// Override bridges to define, in the order their types were processed.
    pub bridges: Vec<Bridge>,
    pub stats: Stats,
}

/// An override bridge (see `hierarchy.rs`): a function under `key` that calls `target`.
#[derive(Debug)]
pub struct Bridge {
    pub key: MethodKey,
    /// The class declaring the base or interface method, which names the bridge when `key`'s
    /// own function id is taken.
    pub decl_cls: String,
    pub target: String,
    pub params: Vec<ParameterType>,
}

impl Collected {
    /// Records a callee; its function id.
    pub fn callee(&mut self, key: &MethodKey, params: Vec<ParameterType>) -> String {
        let fid = key.fid();
        self.ext
            .entry(fid.clone())
            .or_insert_with(|| (key.clone(), params));
        fid
    }

    /// The thunk a delegate bound to static method `target` points at.
    pub fn thunk(&mut self, target: &str) -> String {
        let fid = format!("{THUNK_PREFIX}{target}");
        self.thunks.insert(fid.clone(), target.to_string());
        fid
    }
}

/// Counts for the import summary.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub assemblies: usize,
    pub types: usize,
    pub methods: usize,
    pub bodies: usize,
    pub stubs: usize,
    pub bridges: usize,
    /// Bridges whose key is a different method the class declares, so whose row shares it.
    pub shared_key_bridges: usize,
    /// `Opaque` effects lowered conservatively.
    pub opaque_effects: usize,
    /// Addresses merged at a join, whose writes reach only their cell (`ptr.rs`).
    pub unresolved_address_writes: usize,
    /// Inputs the flow layer's `resolve_address` traces and the frontend's analysis does not
    /// (`ptr.rs`). Expected to be zero.
    pub address_disagreements: usize,
    /// Base types and interfaces whose metadata could not be read, so made no aliases.
    pub missing_metadata: Vec<String>,
    /// Source lines.
    pub lines: LineStats,
}

/// The VMT columns, gathered as `process` runs.
#[derive(Default)]
struct Table {
    names: Vec<FunctionNames>,
    methods: Vec<(String, String, String, String)>,
    hierarchy: HashMap<String, Vec<String>>,
    interfaces: Vec<String>,
    abstract_methods: Vec<(String, String, String)>,
    pinvokes: Vec<(String, String, String, String, String, String)>,
}

/// What the table says about the function `fid`, whose key is `key`.
fn names_of(fid: &str, key: &MethodKey) -> FunctionNames {
    FunctionNames::new(fid, key.simple.as_str())
        .with_parent(key.cls.as_str())
        .with_signature(key.desc.as_str())
}

/// The translation of a set of assemblies into one program (see the crate docs).
pub struct Context {
    program: Program,
    table: Table,
    spans: SourceInfoBuilder,
    defined: HashMap<String, FunctionIdx>,
    out: Collected,
    /// The program's globals: one per class that owns a static field. Moved into the program when
    /// it is finished.
    globals: ctadl_ir::cir::Globals,
    /// Each class the assemblies define, as its global is named, with the assembly it is in and
    /// the file offset and size of its `TypeDef` row there: where the global is declared.
    /// Located when the program is finished, once every class has been lowered.
    type_defs: Vec<(String, ArtifactKey, u32, u32)>,
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    pub fn new() -> Self {
        Context {
            program: Program::default(),
            table: Table::default(),
            spans: SourceInfoBuilder::new(source_info::ArtifactMetadata::new()),
            defined: HashMap::new(),
            out: Collected::default(),
            globals: Default::default(),
            type_defs: Vec::new(),
        }
    }

    /// Translates the assemblies into the program, in order.
    ///
    /// In three passes. The first reads each type on its own, on the current rayon pool: its
    /// name, override aliases and method keys, the costly part of the bookkeeping. The second
    /// goes through the types in order: the hierarchy, the VMT rows, a function for each method,
    /// and a [`Job`] for each body. The third lowers the bodies on the rayon pool, each on its
    /// own, and places them into the program in order ([`Context::place`]), so that the program
    /// does not depend on how many threads lowered it.
    fn process<'m>(&mut self, assemblies: &[Assembly<'_, 'm, '_, '_>]) -> Result<(), Error> {
        let timer = PhaseTimer::start();
        let types: Vec<(usize, TypeDef<'m>)> = assemblies
            .iter()
            .enumerate()
            .flat_map(|(a, asm)| asm.names.module.all_types().map(move |td| (a, td)))
            .collect();
        let scans: Vec<TypeScan<'m>> = types
            .into_par_iter()
            .map(|(a, td)| scan_type(&assemblies[a].names, td))
            .collect();
        timer.done("reading types");
        let mut scans = scans.into_iter();
        let mut jobs = Vec::new();
        let mut callees = Vec::new();
        let mut collected = Ok(());
        'assemblies: for (a, asm) in assemblies.iter().enumerate() {
            self.out.stats.assemblies += 1;
            log::debug!("cil: processing assembly {}", asm.names.asm);
            for scan in scans.by_ref().take(asm.names.module.all_types().len()) {
                if let Err(e) = self.collect_type(a, asm, scan, &mut jobs, &mut callees) {
                    collected = Err(e).err_context(|| format!("converting {}", asm.path.display()));
                    break 'assemblies;
                }
            }
        }
        timer.done("bookkeeping");
        // The bodies before a failing type are converted first: one of them may fail before it.
        self.lower_and_place(assemblies, jobs, callees)?;
        timer.done("lowering bodies");
        collected
    }

    /// The bookkeeping for one type: what is not a method body.
    fn collect_type<'m>(
        &mut self,
        a: usize,
        asm: &Assembly<'_, 'm, '_, '_>,
        scan: TypeScan<'m>,
        jobs: &mut Vec<Job<'m>>,
        callees: &mut Vec<PendingCallee>,
    ) -> Result<(), Error> {
        let TypeScan {
            td,
            cls,
            parents,
            aliases,
            methods,
        } = scan;
        let module = asm.names.module;
        self.out.stats.types += 1;
        if let Some((offset, size)) = module
            .metadata()
            .row_file_span(cil_reader::raw::Table::TypeDef, td.rid())
            && let Ok(offset) = u32::try_from(offset)
        {
            self.type_defs
                .push((cls.clone(), asm.spans.key.clone(), offset, size));
        }
        let parents = parents?;
        let entry = self.table.hierarchy.entry(cls.clone()).or_default();
        for p in parents {
            if !entry.contains(&p) {
                entry.push(p);
            }
        }
        if td.is_interface()? {
            self.table.interfaces.push(cls.clone());
        }

        self.out
            .stats
            .missing_metadata
            .extend(aliases.missing_metadata);
        for alias in aliases.rows {
            // CHA reaches an implementation under its own key, inherited or not.
            if (&alias.simple, &alias.desc) == (&alias.target.simple, &alias.target.desc) {
                continue;
            }
            let target = alias.target.fid();
            callees.push(PendingCallee {
                before_job: jobs.len(),
                key: alias.target,
                params: alias.params.clone(),
            });
            self.out.bridges.push(Bridge {
                key: MethodKey {
                    cls: cls.clone(),
                    simple: alias.simple,
                    desc: alias.desc,
                },
                decl_cls: alias.decl_cls,
                target,
                params: alias.params,
            });
        }

        for ScannedMethod { md, key: mkey, sig } in methods? {
            let Some(mkey) = mkey else {
                log::warn!("cil: {cls}: a method whose name does not decode");
                continue;
            };
            let fid = mkey.fid();
            if self.defined.contains_key(&fid) {
                log::debug!("cil: {fid} is defined twice; keeping the first");
                continue;
            }
            self.table.names.push(names_of(&fid, &mkey));
            let sig = sig?;
            let fidx = self.program.new_function();
            self.defined.insert(fid.clone(), fidx);
            self.out.stats.methods += 1;
            method::shape(
                &mut self.program[fidx],
                fid.clone(),
                method::parameters(&sig),
            );
            if md.is_abstract()? {
                self.table.abstract_methods.push((
                    cls.clone(),
                    mkey.simple.clone(),
                    mkey.desc.clone(),
                ));
                continue;
            }
            self.table.methods.push((
                cls.clone(),
                mkey.simple.clone(),
                mkey.desc.clone(),
                fid.clone(),
            ));
            if let Some(p) = md.pinvoke_info()? {
                self.table.pinvokes.push((
                    cls.clone(),
                    mkey.simple.clone(),
                    mkey.desc.clone(),
                    fid.clone(),
                    p.module.into_owned(),
                    p.entry_point.into_owned(),
                ));
            }
            if md.has_body()? && md.rva()? != 0 {
                self.out.stats.bodies += 1;
                jobs.push(Job {
                    assembly: a,
                    fidx,
                    fid,
                    md,
                    sig,
                });
            }
        }
        Ok(())
    }

    /// Lowers `jobs` on the current rayon pool and places them into the program in order, with
    /// the override bridges' targets among their callees where the bookkeeping met them.
    ///
    /// The jobs go in batches, so that only one batch of lowered bodies waits to be placed at a
    /// time, and so that the next batch is lowered while this one is placed.
    fn lower_and_place<'m>(
        &mut self,
        assemblies: &[Assembly<'_, 'm, '_, '_>],
        jobs: Vec<Job<'m>>,
        callees: Vec<PendingCallee>,
    ) -> Result<(), Error> {
        /// Bodies per batch: enough to keep every thread busy across uneven method sizes, few
        /// enough that a batch of lowered bodies is a small part of the program.
        const BATCH: usize = 4096;

        // A batch's jobs are dropped once it is lowered.
        let lower = |batch: Vec<Job<'m>>| -> Option<Vec<Result<Lowered, Error>>> {
            (!batch.is_empty()).then(|| {
                batch
                    .par_iter()
                    .map(|job| lower_job(&assemblies[job.assembly], job))
                    .collect()
            })
        };
        let mut callees = callees.into_iter().peekable();
        let mut placed_jobs = 0;
        let mut placing = std::time::Duration::ZERO;
        let mut jobs = jobs.into_iter();
        let mut take_batch = || jobs.by_ref().take(BATCH).collect::<Vec<_>>();
        let mut current = lower(take_batch());
        while let Some(results) = current.take() {
            let next_batch = take_batch();
            let mut placed = Ok(());
            let started = std::time::Instant::now();
            rayon::scope(|scope| {
                scope.spawn(|_| current = lower(next_batch));
                placed = self.place(assemblies, results, &mut placed_jobs, &mut callees);
                placing += started.elapsed();
            });
            placed?;
        }
        log::debug!(target: "ctadl_cil::timing", "placing bodies: {placing:.2?}");
        for c in callees {
            self.out.callee(&c.key, c.params);
        }
        Ok(())
    }

    /// Puts a batch of lowered bodies into the program, in order: records their callees,
    /// interns their spans and declares their globals in the order lowering them in place
    /// would have, then rewrites their statements to the program's ids (in parallel: each body
    /// on its own), then moves them into their functions.
    fn place(
        &mut self,
        assemblies: &[Assembly<'_, '_, '_, '_>],
        batch: Vec<Result<Lowered, Error>>,
        placed_jobs: &mut usize,
        callees: &mut std::iter::Peekable<std::vec::IntoIter<PendingCallee>>,
    ) -> Result<(), Error> {
        struct Placed {
            fidx: FunctionIdx,
            body: FunctionData,
            spans: Vec<source_info::FileSpanId>,
            globals: Vec<ctadl_ir::cir::GlobalIdx>,
        }

        let mut placed = Vec::with_capacity(batch.len());
        for lowered in batch {
            while let Some(c) = callees.next_if(|c| c.before_job <= *placed_jobs) {
                self.out.callee(&c.key, c.params);
            }
            *placed_jobs += 1;
            let Lowered {
                assembly,
                fidx,
                body,
                spans,
                globals,
                out,
            } = lowered?;
            // The first callee recorded for a function id gives its stub's shape.
            for (fid, callee) in out.ext {
                self.out.ext.entry(fid).or_insert(callee);
            }
            self.out.thunks.extend(out.thunks);
            let (stats, s) = (&mut self.out.stats, out.stats);
            stats.opaque_effects += s.opaque_effects;
            stats.unresolved_address_writes += s.unresolved_address_writes;
            stats.address_disagreements += s.address_disagreements;
            let spans = spans.intern(&mut self.spans, &assemblies[assembly].spans.key);
            let globals = globals
                .iter_enumerated()
                .map(|(_, g)| self.globals.declare(&*g.name, None))
                .collect();
            placed.push(Placed {
                fidx,
                body,
                spans,
                globals,
            });
        }

        placed.par_iter_mut().for_each(|p| {
            let mut resolve = Resolve {
                spans: &p.spans,
                globals: &p.globals,
            };
            ctadl_ir::cir::visit::MutVisitor::visit_function_data(
                &mut resolve,
                p.fidx,
                &mut p.body,
            );
        });

        for p in placed {
            self.program[p.fidx] = p.body;
        }
        Ok(())
    }

    /// Defines the override bridges, stubs every callee nothing defined, adds the delegate
    /// thunks, and builds the table.
    pub fn finish(self) -> Result<ProgramInfo, Error> {
        Ok(self.finish_with_stats()?.0)
    }

    /// [`Context::finish`], with the import's counts.
    pub fn finish_with_stats(mut self) -> Result<(ProgramInfo, Stats), Error> {
        // Before the stubs: a call naming a bridge's key calls the bridge.
        for bridge in std::mem::take(&mut self.out.bridges) {
            let mut fid = bridge.key.fid();
            if self.defined.contains_key(&fid) {
                fid = format!("{fid}#{}", bridge.decl_cls);
                if self.defined.contains_key(&fid) {
                    log::debug!("cil: bridge {fid} is defined twice; keeping the first");
                    continue;
                }
                self.out.stats.shared_key_bridges += 1;
            }
            let fidx = self.program.new_function();
            self.defined.insert(fid.clone(), fidx);
            let arity = bridge.params.len();
            method::shape(&mut self.program[fidx], fid.clone(), bridge.params);
            method::bridge_body(&mut self.program[fidx], &bridge.target, arity);
            self.table.names.push(names_of(&fid, &bridge.key));
            let MethodKey { cls, simple, desc } = bridge.key;
            self.table.methods.push((cls, simple, desc, fid));
            self.out.stats.bridges += 1;
        }
        let mut ext: Vec<_> = std::mem::take(&mut self.out.ext).into_iter().collect();
        ext.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        for (fid, (key, params)) in &ext {
            if self.defined.contains_key(fid) {
                continue;
            }
            let fidx = self.program.new_function();
            self.defined.insert(fid.clone(), fidx);
            method::shape(&mut self.program[fidx], fid.clone(), params.clone());
            self.table.names.push(names_of(fid, key));
            self.table.methods.push((
                key.cls.clone(),
                key.simple.clone(),
                key.desc.clone(),
                fid.clone(),
            ));
            self.out.stats.stubs += 1;
        }
        let thunks = std::mem::take(&mut self.out.thunks);
        for (thunk, target) in thunks {
            let Some(&tidx) = self.defined.get(&target) else {
                continue;
            };
            let target_params: Vec<ParameterType> =
                self.program[tidx].params.iter_params().copied().collect();
            let fidx = self.program.new_function();
            let fdat = &mut self.program[fidx];
            let mut params = vec![ParameterType::Shared];
            params.extend(target_params.iter().copied());
            // A thunk is the frontend's own; no source names it.
            self.table
                .names
                .push(FunctionNames::new(thunk.as_str(), thunk.as_str()));
            method::shape(fdat, thunk, params);
            method::thunk_body(fdat, &target, target_params.len());
        }

        let mut program = self.program;
        program.globals = self.globals;
        // A class's global is declared at its `TypeDef` row, if the program uses it.
        for (cls, key, offset, size) in self.type_defs {
            if program.globals.lookup(&cls).is_some() {
                let span = self
                    .spans
                    .span_for(key, offset, source_info::SpanLen::ByteLen(size));
                program.globals.declare(&cls, Some(span));
            }
        }
        program.verify()?;
        let Table {
            names: mut function_names,
            mut methods,
            hierarchy,
            mut interfaces,
            mut abstract_methods,
            mut pinvokes,
        } = self.table;
        methods.par_sort_unstable();
        methods.dedup();
        interfaces.par_sort_unstable();
        interfaces.dedup();
        abstract_methods.par_sort_unstable();
        abstract_methods.dedup();
        pinvokes.par_sort_unstable();
        pinvokes.dedup();
        let dispatch = Dispatch::Cil {
            methods: methods
                .into_par_iter()
                .map(|(c, s, d, f)| {
                    (
                        CilClass(c.as_str().into()),
                        CilSimpleName(s.as_str().into()),
                        CilSignature(d.as_str().into()),
                        CilMethod(f.as_str().into()),
                    )
                })
                .collect(),
            hierarchy: hierarchy
                .into_iter()
                .map(|(c, ps)| {
                    (
                        CilClass(c.as_str().into()),
                        ps.iter()
                            .map(|p| CilClass(p.as_str().into()))
                            .collect::<SmallVec<[CilClass; 2]>>(),
                    )
                })
                .collect(),
            interfaces: interfaces
                .into_par_iter()
                .map(|c| CilClass(c.as_str().into()))
                .collect(),
            abstract_methods: abstract_methods
                .into_par_iter()
                .map(|(c, s, d)| {
                    (
                        CilClass(c.as_str().into()),
                        CilSimpleName(s.as_str().into()),
                        CilSignature(d.as_str().into()),
                    )
                })
                .collect(),
            pinvokes: pinvokes
                .into_par_iter()
                .map(|(c, s, d, f, m, e)| {
                    (
                        CilClass(c.as_str().into()),
                        CilSimpleName(s.as_str().into()),
                        CilSignature(d.as_str().into()),
                        CilMethod(f.as_str().into()),
                        m.as_str().into(),
                        e.as_str().into(),
                    )
                })
                .collect(),
        };
        // Sorted as `finish_names` sorts them, stably, but in parallel: that sort then finds them
        // in order.
        function_names.par_sort_by(|a, b| a.fq.cmp(&b.fq));
        let mut vmt = VirtualMethodTable {
            names: function_names,
            dispatch,
        };
        vmt.finish_names();
        let s = &self.out.stats;
        log::info!(
            "cil: {} assembl{}, {} types, {} methods ({} bodies), {} extern stubs, {} override \
             bridges ({} sharing a key)",
            s.assemblies,
            if s.assemblies == 1 { "y" } else { "ies" },
            s.types,
            s.methods,
            s.bodies,
            s.stubs,
            s.bridges,
            s.shared_key_bridges
        );
        if s.unresolved_address_writes > 0 {
            log::info!(
                "cil: unresolved address writes: {} (writes through a merged address reach \
                 only its cell)",
                s.unresolved_address_writes
            );
        }
        if s.opaque_effects > 0 {
            log::info!("cil: opaque effects: {}", s.opaque_effects);
        }
        if s.address_disagreements > 0 {
            log::warn!(
                "cil: {} input(s) the flow layer traces to a place and the frontend does not",
                s.address_disagreements
            );
        }
        if s.lines.pdbs > 0 {
            let mut used = s.lines.documents.clone();
            used.sort();
            used.dedup();
            log::info!(
                "cil: source lines: {} of {} bodies, from {} document(s) of {} PDB(s)",
                s.lines.methods_with_lines,
                s.bodies,
                used.len(),
                s.lines.pdbs
            );
            let mut unusable = s.lines.unusable_documents.clone();
            unusable.sort();
            unusable.dedup();
            for (path, why) in &unusable {
                log::warn!("cil: no source lines from {path}: {why}");
            }
        } else if s.bodies > 0 {
            log::info!("cil: no portable PDB, so no source lines: spans are IL bytes");
        }
        if !s.missing_metadata.is_empty() {
            let mut missing = s.missing_metadata.clone();
            missing.sort();
            missing.dedup();
            log::info!(
                "cil: no metadata for {} base type(s) or interface(s), so no override aliases \
                 through them (first: {})",
                missing.len(),
                missing[0]
            );
        }
        let stats = self.out.stats;
        Ok((
            ProgramInfo {
                program,
                vmt,
                source_info: self.spans.finish(),
            },
            stats,
        ))
    }
}
