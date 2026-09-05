/*! Reads and writes a [`ProgramInfo`] in an import directory.

The writer ([`save_program_info`]) and the readers ([`load_import`], [`open_import`]) are all
public, so no other crate has to work out the store layout and the bitcode filenames for itself.
This matters because code that reads the files directly cannot tell when
[`IMPORT_FORMAT_VERSION`](crate::project::IMPORT_FORMAT_VERSION) has changed. It either decodes
nonsense or fails with a `bitcode::Error` that explains nothing. [`open_import`] always goes
through [`ArtifactImport::load`], so an out-of-date store fails with
[`Error::IncompatibleImport`], which names the artifact to import again.
*/

use ctadl_ir::graph::is_connected;
use ctadl_ir::{ProgramInfo, encode, ssa};

use crate::error::{Error, ErrorContext};
use crate::project::{ArtifactImport, IMPORT_CONFIG_FILE, StorePaths};

/// Says whether [`load_import`] should read an import's source-info database.
///
/// That database is stored as parquet, and it is by far the largest part of an import
/// directory. Code that only wants the IR, such as a fact generator, a pass, or another
/// analysis, never looks at it. `ctadl index` reads it on its own, one import at a time, at the
/// point where it needs source spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceInfoMode {
    /// Leave [`ProgramInfo::source_info`] empty.
    #[default]
    Skip,
    /// Also read the `source-info/` directory next to the IR.
    Read,
}

/// Writes a [`ProgramInfo`] into the directory for `import`. That means three things: the
/// program, the virtual method table, and the source info. [`crate::project`] describes the
/// layout they are written in.
pub fn save_program_info(
    mut program_info: ProgramInfo,
    import: &ArtifactImport,
) -> Result<(), Error> {
    let path = &import.program_path();
    let obj = std::mem::take(&mut program_info.program);
    for f in obj.functions.iter() {
        if f.blocks.is_empty() {
            continue;
        }
        // Disassembled binaries often have functions with blocks that cannot be reached from
        // the entry block. Ghidra leaves these behind when it recovers the control-flow graph.
        // This is not an import error. Indexing drops unreachable blocks before the SSA and
        // dominator pass; see `--prune-unreachable-cfg-nodes`, which is on by default. So log
        // it here, but do not reject the function.
        if !is_connected(&f.blocks) {
            log::debug!("function has blocks unreachable from entry: {}", f.name);
        }
    }
    let data = encode::encode_program(&obj).map_err(Error::Bitcode)?;
    std::fs::write(path, data)
        .map_err(Error::Io)
        .err_context(|| format!("writing program: {}", path.display()))?;
    log::debug!("wrote {}", path.display());

    let path = &import.vmt_path();
    let obj = std::mem::take(&mut program_info.vmt);
    let data = encode::encode_vmt(&obj).map_err(Error::Bitcode)?;
    std::fs::write(path, data)
        .map_err(Error::Io)
        .err_context(|| format!("writing vmt: {}", path.display()))?;
    log::debug!("wrote {}", path.display());

    let path = import.source_info_dir();
    let obj = std::mem::take(&mut program_info.source_info);
    std::fs::create_dir_all(&path)
        .err_context(|| format!("creating source info dir: {}", path.display()))?;
    source_info::write_parquet_source_info(&path, &obj)
        .err_context(|| format!("writing source info: {}", path.display()))?;
    Ok(())
}

/// Reads the [`ProgramInfo`] out of the directory of an artifact that was already imported.
///
/// The IR comes back exactly as the front end wrote it, with no pass run over it. A caller that
/// is going to generate facts wants [`open_import`] instead, which is this function plus
/// [`ssa::run_pipeline`].
pub fn load_import(import: &ArtifactImport, src: SourceInfoMode) -> Result<ProgramInfo, Error> {
    let path = &import.program_path();
    log::debug!("reading {}", path.display());
    let data =
        std::fs::read(path).err_context(|| format!("reading program: {}", path.display()))?;
    let program = encode::decode_program(&data)
        .err_context(|| format!("decoding program: {}", path.display()))?;

    let path = &import.vmt_path();
    log::debug!("reading {}", path.display());
    let data = std::fs::read(path).err_context(|| format!("reading vmt: {}", path.display()))?;
    let vmt =
        encode::decode_vmt(&data).err_context(|| format!("decoding vmt: {}", path.display()))?;

    let source_info = match src {
        SourceInfoMode::Skip => Default::default(),
        SourceInfoMode::Read => {
            let path = import.source_info_dir();
            log::debug!("reading {}", path.display());
            source_info::read_parquet_source_info(&path)
                .err_context(|| format!("reading source info: {}", path.display()))?
        }
    };

    Ok(ProgramInfo {
        program,
        vmt,
        source_info,
    })
}

/// Turns an import in the store into preprocessed IR, in one call.
///
/// `name_or_dir` is either the name of an import, looked up under the store's `imports/`
/// directory, or a path to an import directory. Accepting a path is what lets a caller read a
/// store it did not create. `pipeline` says which IR-to-IR passes to run.
/// [`Pipeline::index_default`](ctadl_ir::ssa::Pipeline::index_default) is the set `ctadl index`
/// runs, so asking for anything else is a deliberate choice.
///
/// Source info is not read. To get it, call [`load_import`] with [`SourceInfoMode::Read`].
///
/// # Errors
///
/// Returns [`Error::IncompatibleImport`] if the import was written by a build with a different
/// [`IMPORT_FORMAT_VERSION`](crate::project::IMPORT_FORMAT_VERSION). Code that reads
/// `ir-program.bitcode` directly cannot make that check, which is why this function exists.
pub fn open_import(
    name_or_dir: &str,
    pipeline: ssa::Pipeline,
) -> Result<ProgramInfo, Error> {
    let import = resolve_import(name_or_dir)?;
    let mut program_info = load_import(&import, SourceInfoMode::Skip)?;
    ssa::run_pipeline(&mut program_info.program, pipeline);
    Ok(program_info)
}

/// Loads and version-checks an import config, given either the name of an import or a path. The
/// path may point at an import directory or straight at its `import_config.json`.
///
/// If the argument works as both a path and a name, the path wins. A caller who typed a path
/// meant the store that path is in, not the one this process is using.
pub fn resolve_import(name_or_dir: &str) -> Result<ArtifactImport, Error> {
    let as_path = std::path::Path::new(name_or_dir);
    if as_path.is_file() {
        return ArtifactImport::load(as_path)
            .err_context(|| format!("reading import config '{name_or_dir}'"));
    }
    let config = as_path.join(IMPORT_CONFIG_FILE);
    if config.is_file() {
        return ArtifactImport::load(&config)
            .err_context(|| format!("reading import at '{name_or_dir}'"));
    }
    // It is not a directory we can read, so treat it as the name of an import in this
    // process's store. Name the store root in the error, because an import that is not found is
    // almost always a sign that `--store` is wrong.
    if !StorePaths::import_path().join(name_or_dir).is_dir() {
        return Err(Error::Path {
            message: format!(
                "no import '{name_or_dir}': not a directory, and no such import under '{}'",
                StorePaths::import_path().display()
            ),
        });
    }
    ArtifactImport::load_by_name(name_or_dir)
}
