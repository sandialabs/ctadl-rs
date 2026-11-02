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
| Pointer lowering               | the traced place, else `DEREF_FIELD`; byref arguments copied in and out | `ptr.rs` |
| Array index                    | does not taint the element read (as Dex and JVM)           | `ptr.rs`, `effects.rs` |
| Branch conditions              | `Test` emits nothing                                       | `effects.rs`  |
| `box`                          | contents-preserving assign; the boxed type instantiated    | `effects.rs`  |
| Value types                    | lowered like objects, so two copies of a struct alias      | `effects.rs`  |
| Non-virtual or sealed `callvirt` | `DirectCall`                                             | `calls.rs`    |
| Generic instantiation          | erased: a class is its open definition                     | `names.rs`    |
| Assembly selection             | framework and BCL not imported by default ([`is_framework_assembly`]); their metadata always read | `lib.rs` |

Like the Dex front end, a [`Context`] is made once, given each assembly with
[`Context::process`], and turned into one [`ProgramInfo`] by [`Context::finish`]: a
multi-assembly application has one hierarchy. Referenced assemblies that are not imported
(the framework, by default) are still read for their metadata, from the input's directory
and the framework's ([`ImportOptions::reference_dirs`]): base types' virtual methods, type
forwarders, sealed and virtual flags. Nothing of theirs is lowered.

An assembly's portable PDB (beside it, embedded, or in its own metadata) gives its instructions
source lines; see `lines.rs`.
*/

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cil_reader::Module;
use cil_reader::model::{AssemblyCache, DirectoryLoader, PdbSource, TypeDef};
use ctadl_import::error::{Error, ErrorContext};
use ctadl_ir::call::{
    CilClass, CilMethod, CilSignature, CilSimpleName, Dispatch, FunctionNames, VirtualMethodTable,
};
use ctadl_ir::*;
use hashbrown::HashMap;
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
    let mut files = Vec::new();
    let entries =
        std::fs::read_dir(dir).err_context(|| format!("reading directory {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "dll" | "exe") || !is_cli_image(&path) {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !opts.include_framework && is_framework_assembly(stem) {
            log::debug!("cil: skipping framework assembly {}", path.display());
            continue;
        }
        files.push(path);
    }
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
    let mut datas = Vec::with_capacity(files.len());
    for f in files {
        datas.push(std::fs::read(f).err_context(|| format!("reading {}", f.display()))?);
    }
    let mut modules = Vec::with_capacity(files.len());
    for (f, data) in files.iter().zip(&datas) {
        modules.push(Module::read(data).err_context(|| format!("reading {}", f.display()))?);
    }
    // Symbols are optional: one that does not read costs the assembly its lines, not the import.
    let pdb_sources: Vec<Option<PdbSource>> = files
        .iter()
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
    let mut ctx = Context::new();
    for (i, ((f, module), source)) in files.iter().zip(&modules).zip(&pdb_sources).enumerate() {
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
        let mut spans = ModuleSpans::new(key, pdb);
        ctx.process(module, &namer, &mut spans)
            .err_context(|| format!("converting {}", f.display()))?;
        ctx.out.stats.lines.add(spans.stats);
    }
    ctx.finish_with_stats()
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
    pub ext: BTreeMap<String, (MethodKey, Vec<ParameterType>)>,
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
        }
    }

    /// Translates one assembly into the program, its spans from `spans`.
    pub fn process<'m>(
        &mut self,
        module: &'m Module<'m>,
        namer: &Namer<'m, '_>,
        spans: &mut ModuleSpans<'_>,
    ) -> Result<(), Error> {
        let names = ModNames::new(module, namer);
        self.out.stats.assemblies += 1;
        log::debug!("cil: processing assembly {}", names.asm);
        for td in module.all_types() {
            self.process_type(&names, namer, module, td, spans)?;
        }
        Ok(())
    }

    fn process_type<'m>(
        &mut self,
        names: &ModNames<'_, 'm, 'm, '_>,
        namer: &Namer<'m, '_>,
        module: &'m Module<'m>,
        td: TypeDef<'m>,
        spans: &mut ModuleSpans<'_>,
    ) -> Result<(), Error> {
        self.out.stats.types += 1;
        let cls = names.typedef_cls(&td);
        let mut parents = Vec::new();
        if let Some(base) = td.base_type()? {
            parents.extend(names.tdor_cls(&base));
        }
        for i in td.interfaces()? {
            if let Some(iface) = i.interface {
                parents.extend(names.tdor_cls(&iface));
            }
        }
        let entry = self.table.hierarchy.entry(cls.clone()).or_default();
        for p in parents {
            if !entry.contains(&p) {
                entry.push(p);
            }
        }
        if td.is_interface()? {
            self.table.interfaces.push(cls.clone());
        }

        let aliases = hierarchy::aliases(namer, module, td);
        self.out
            .stats
            .missing_metadata
            .extend(aliases.missing_metadata);
        for alias in aliases.rows {
            // CHA reaches an implementation under its own key, inherited or not.
            if (&alias.simple, &alias.desc) == (&alias.target.simple, &alias.target.desc) {
                continue;
            }
            let target = self.out.callee(&alias.target, alias.params.clone());
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

        for md in td.methods()? {
            let Some(mkey) = names.method_key(md.token()) else {
                log::warn!("cil: {cls}: a method whose name does not decode");
                continue;
            };
            let fid = mkey.fid();
            if self.defined.contains_key(&fid) {
                log::debug!("cil: {fid} is defined twice; keeping the first");
                continue;
            }
            self.table.names.push(names_of(&fid, &mkey));
            let sig = md.signature()?;
            let fidx = self.program.new_function();
            self.defined.insert(fid.clone(), fidx);
            self.out.stats.methods += 1;
            let fdat = &mut self.program[fidx];
            method::shape(fdat, fid.clone(), method::parameters(&sig));
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
                let body = md.body()?;
                self.out.stats.bodies += 1;
                let mut lines = spans.method(md.token().rid(), body.code_file_offset());
                method::lower_body(
                    names,
                    &md,
                    &body,
                    &sig,
                    fdat,
                    &mut lines,
                    &mut self.spans,
                    &mut self.out,
                );
            }
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
        let ext = std::mem::take(&mut self.out.ext);
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

        let program = self.program;
        program.verify()?;
        let Table {
            names: function_names,
            mut methods,
            hierarchy,
            mut interfaces,
            mut abstract_methods,
            mut pinvokes,
        } = self.table;
        methods.sort();
        methods.dedup();
        interfaces.sort();
        interfaces.dedup();
        abstract_methods.sort();
        abstract_methods.dedup();
        pinvokes.sort();
        pinvokes.dedup();
        let dispatch = Dispatch::Cil {
            methods: methods
                .into_iter()
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
                .into_iter()
                .map(|c| CilClass(c.as_str().into()))
                .collect(),
            abstract_methods: abstract_methods
                .into_iter()
                .map(|(c, s, d)| {
                    (
                        CilClass(c.as_str().into()),
                        CilSimpleName(s.as_str().into()),
                        CilSignature(d.as_str().into()),
                    )
                })
                .collect(),
            pinvokes: pinvokes
                .into_iter()
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
