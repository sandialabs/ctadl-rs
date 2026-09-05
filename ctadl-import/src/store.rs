/*! `ProgramInfo` in and out of an import directory.

The asymmetry this module exists to close: `ctadl` has always exported the writer
([`save_program_info`]) and hidden the reader, so every downstream consumer of an import
re-implemented the store layout and the bitcode filenames by hand -- and therefore never noticed
an [`IMPORT_FORMAT_VERSION`](crate::project::IMPORT_FORMAT_VERSION) bump, decoding garbage or
failing with a `bitcode::Error` that named no cause. [`open_import`] is the reader, and it goes
through [`ArtifactImport::load`], never around it, so a stale store fails as
[`Error::IncompatibleImport`] naming the artifact to re-import.
*/

use ctadl_ir::graph::is_connected;
use ctadl_ir::{ProgramInfo, encode, ssa};

use crate::error::{Error, ErrorContext};
use crate::project::{ArtifactImport, IMPORT_CONFIG_FILE, StorePaths};

/// Whether [`load_import`] reads an import's source-info database.
///
/// It is parquet on disk and by far the largest thing in an import directory, and a consumer
/// that only wants the IR -- a fact generator, a pass, a downstream analysis -- never touches
/// it. `ctadl index` reads it separately, per import, at the point it needs spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceInfoMode {
    /// Leave [`ProgramInfo::source_info`] empty.
    #[default]
    Skip,
    /// Read `source-info/` beside the IR.
    Read,
}

/// Writes a [`ProgramInfo`] into `import`'s directory: the program, the VMT, and the source
/// info, in the layout [`crate::project`] documents.
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
        // Real disassembled binaries routinely contain functions with blocks
        // that are unreachable from entry (Ghidra CFG recovery artifacts). This
        // is not an import error: indexing prunes unreachable blocks before the
        // SSA/dominator pass (see `--prune-unreachable-cfg-nodes`, on by
        // default), so record but don't reject them here.
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

/// Reads the [`ProgramInfo`] out of an already-imported artifact's directory.
///
/// The IR is returned exactly as the front end produced it: no pass has run. Callers that go on
/// to generate facts want [`open_import`], which is this plus
/// [`ssa::run_pipeline`](ctadl_ir::ssa::run_pipeline).
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

/// The one-liner: an import in the store, named or pointed at, to preprocessed IR.
///
/// `name_or_dir` is either an import name (resolved under the store's `imports/`) or a path to
/// an import directory, which is what makes this usable against a store the caller did not
/// create. `pipeline` is the IR-to-IR preprocessing to run;
/// [`Pipeline::index_default`](ctadl_ir::ssa::Pipeline::index_default) is what `ctadl index`
/// runs, and passing anything else is a deliberate choice rather than an accident of which
/// four calls the caller happened to copy.
///
/// Source info is skipped; use [`load_import`] with [`SourceInfoMode::Read`] to get it.
///
/// # Errors
///
/// [`Error::IncompatibleImport`] if the import was written by a build with a different
/// [`IMPORT_FORMAT_VERSION`](crate::project::IMPORT_FORMAT_VERSION) -- the check that a
/// hand-rolled reader of `ir-program.bitcode` cannot perform, and the reason this function
/// exists.
pub fn open_import(
    name_or_dir: &str,
    pipeline: ssa::Pipeline,
) -> Result<ProgramInfo, Error> {
    let import = resolve_import(name_or_dir)?;
    let mut program_info = load_import(&import, SourceInfoMode::Skip)?;
    ssa::run_pipeline(&mut program_info.program, pipeline);
    Ok(program_info)
}

/// An import name, or a path to an import directory (or to its `import_config.json`), to the
/// loaded, version-checked config.
///
/// A path wins over a name when both would resolve, because a caller that typed a path meant
/// that store and not this process's.
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
    // Not a directory we can read: treat it as a name in this process's store. Report the
    // store root, because "no such import" is nearly always the wrong `--store`.
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
