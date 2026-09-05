/*! The dispatch: an artifact, whatever its language, to CTADL IR.

Below this crate sit the language front ends, one crate each, none of which knows about any
other. Above it sits `ctadl-ascent`, the engine. What lives here is the part that is neither: the
`match import.language` that chooses a front end, and the two *container* formats that are
dispatch rather than a language --

* an APK, whose Java half is Dex and whose native half is a set of `.so` files each imported
  through the pcode front end ([`apk_native`]);
* an app bundle (`.xapk`), which is a ZIP of split APKs, each imported recursively ([`xapk`]).

Neither could live in `ctadl-dex` without dragging Ghidra into a crate whose whole point is not
having it.

# Features

A consumer states what it reads. `default = ["dex", "jvm", "apk"]`; `pcode`, `lua`, `c`,
`xapk` and `flowy` are opt-in, and each pulls in exactly one front-end crate plus the matching
`ctadl-import` error variants. [`ArtifactLanguage`](ctadl_import::ArtifactLanguage) stays whole
whatever is enabled -- it is an enum, it costs nothing, and `--help` should keep naming every
language the tool knows about. What the features gate is the *dispatch arms*: a disabled
language reports [`Error::NothingToImport`](ctadl_import::Error::NothingToImport) naming the
feature that would enable it, rather than failing to parse the flag.

`apk` implies `dex` but not `pcode`. An APK imported without `pcode` keeps its Java half and
reports that this build has no native front end -- the same shape as an APK imported on a
machine with no Ghidra, which is already a supported and common case.
*/

#[cfg(feature = "apk")]
pub mod apk_native;
#[cfg(feature = "flowy")]
pub mod flowy;
#[cfg(feature = "xapk")]
pub mod xapk;

use ctadl_import::error::{Error, ErrorContext};
use ctadl_import::project::{ArtifactImport, ArtifactLanguage};
use ctadl_ir::{ProgramInfo, ssa};

/// How to perform one import, beyond the artifact and its language.
///
/// Every field only matters to an APK, which is the one artifact that imports *other*
/// artifacts out of itself (its native libraries; see [`apk_native`]).
/// [`Default`] is the plain behavior: import everything, reuse nothing.
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions<'a> {
    /// Reuse an existing sub-import whose stored artifact hash still matches instead of
    /// redoing it. The parent artifact's own skip check lives in `main`; this is what
    /// carries the flag down to the sub-imports, where the saving (a disassembly run
    /// each) is much larger.
    pub skip_existing: bool,
    /// Import the native libraries packaged inside an APK. On by default.
    pub native_libs: bool,
    /// Import this ABI's libraries rather than the preferred one. See
    /// `dex_reader::apk::ABI_PREFERENCE`.
    pub native_abi: Option<&'a str>,
}

impl Default for ImportOptions<'_> {
    fn default() -> Self {
        Self {
            skip_existing: false,
            native_libs: true,
            native_abi: None,
        }
    }
}

/// The error a dispatch arm reports when its language was compiled out.
///
/// Deliberately [`Error::NothingToImport`] rather than a new variant: from the caller's side
/// this build genuinely has nothing to import out of that artifact, and the message says which
/// feature would change that.
// Dead when every language is enabled, which is how `ctadl-ascent` builds; live in every
// narrower configuration. Both are correct, so neither is worth a `cfg_attr` enumerating the
// feature set.
#[allow(dead_code)]
fn unsupported(language: ArtifactLanguage, feature: &str) -> Error {
    Error::NothingToImport {
        message: format!(
            "this build cannot import {language} artifacts: it was compiled without \
             ctadl-frontends' `{feature}` feature"
        ),
    }
}

/// Imports one artifact into [`ProgramInfo`], dispatching on its language.
///
/// The store is written only where an artifact produces *other* imports -- an APK's native
/// libraries, a bundle's splits -- which have to be complete before this returns. The returned
/// program is the caller's to save; see [`ctadl_import::save_program_info`], and
/// [`open_or_import`] for the whole round trip.
// `opts` is read only by the container-format arms, and the trailing log line is unreachable in
// a build with no language at all. Both are properties of the feature set, not of the code.
#[cfg_attr(not(feature = "apk"), allow(unused_variables))]
#[cfg_attr(
    not(any(
        feature = "dex",
        feature = "jvm",
        feature = "apk",
        feature = "xapk",
        feature = "pcode",
        feature = "lua",
        feature = "c",
        feature = "flowy"
    )),
    allow(unreachable_code)
)]
pub fn import_artifact(
    import: &ArtifactImport,
    opts: ImportOptions<'_>,
) -> Result<ProgramInfo, Error> {
    use ArtifactLanguage::*;
    log::info!(
        "importing {} artifact '{}' from {}",
        import.language,
        import.name,
        import.artifact_path.display()
    );
    // Annotated because a build with *no* language enabled has only diverging arms.
    let program_info: ProgramInfo = match &import.language {
        #[cfg(feature = "dex")]
        Dex => ctadl_dex::import_dex(&import.artifact_path)?,
        #[cfg(not(feature = "dex"))]
        Dex => return Err(unsupported(Dex, "dex")),

        #[cfg(feature = "apk")]
        Apk => import_apk(import, opts)?,
        #[cfg(not(feature = "apk"))]
        Apk => return Err(unsupported(Apk, "apk")),

        #[cfg(feature = "xapk")]
        Xapk => {
            let sub_imports = xapk::import_bundle(import, opts)?;
            record_sub_imports(import, sub_imports)?;
            ProgramInfo::default()
        }
        #[cfg(not(feature = "xapk"))]
        Xapk => return Err(unsupported(Xapk, "xapk")),

        #[cfg(feature = "jvm")]
        Jar => ctadl_jvm::import_jar(&import.artifact_path)?,
        #[cfg(feature = "jvm")]
        Jvm => ctadl_jvm::import_class(&import.artifact_path)?,
        #[cfg(not(feature = "jvm"))]
        Jar => return Err(unsupported(Jar, "jvm")),
        #[cfg(not(feature = "jvm"))]
        Jvm => return Err(unsupported(Jvm, "jvm")),

        #[cfg(feature = "pcode")]
        Pcode => ctadl_pcode::import_pcode(import)?,
        #[cfg(not(feature = "pcode"))]
        Pcode => return Err(unsupported(Pcode, "pcode")),

        #[cfg(feature = "lua")]
        Lua => ctadl_lua::import_lua(&import.artifact_path)?,
        #[cfg(not(feature = "lua"))]
        Lua => return Err(unsupported(Lua, "lua")),

        #[cfg(feature = "flowy")]
        Flowy => flowy::import(import)?,
        #[cfg(not(feature = "flowy"))]
        Flowy => return Err(unsupported(Flowy, "flowy")),

        #[cfg(feature = "c")]
        C => ctadl_c::import_c(&import.artifact_path)?,
        #[cfg(not(feature = "c"))]
        C => return Err(unsupported(C, "c")),
    };
    log::info!(
        "'{}': imported {} function(s)",
        import.name,
        program_info.program.functions.len()
    );
    Ok(program_info)
}

/// The APK arm: the Java half out of `classes*.dex`, then the native half out of `lib/<abi>/`.
#[cfg(feature = "apk")]
fn import_apk(import: &ArtifactImport, opts: ImportOptions<'_>) -> Result<ProgramInfo, Error> {
    // Dex first: it is cheap and it is what fails fast on an APK that is not one,
    // before any native library is extracted or handed to Ghidra.
    let ctadl_dex::ApkImport {
        program_info,
        dex_count,
    } = ctadl_dex::import_apk(&import.artifact_path)?;
    if dex_count > 0 {
        log::info!(
            "{}: {} classes*.dex entr{}",
            import.artifact_path.display(),
            dex_count,
            if dex_count == 1 { "y" } else { "ies" },
        );
    }
    // A split APK out of an app bundle has no Dex of its own; its libraries are
    // the whole import. Decided before extracting anything so an APK that has
    // neither half fails immediately, and so the reason is the APK's contents
    // rather than whatever `import_native_libs` happened to be able to do with
    // them (it returns no sub-imports when Ghidra is missing, too).
    if dex_count == 0 {
        apk_native::require_native_libs(&import.artifact_path)?;
        if opts.native_libs {
            log::info!(
                "{}: no classes*.dex entries; importing as a native-only split APK",
                import.artifact_path.display(),
            );
        } else {
            // Not an error -- the user asked for this -- but the result is an
            // import with nothing in it, which is worth saying out loud.
            log::warn!(
                "{}: no classes*.dex entries and --no-native-libs was passed, so this \
                 import will be empty",
                import.artifact_path.display(),
            );
        }
    }
    record_sub_imports(import, apk_native::import_native_libs(import, opts)?)?;
    Ok(program_info)
}

/// [`import_artifact`] followed by the store write: what `ctadl import` does for one artifact,
/// and what a container format does for each thing it unpacks.
pub fn import_and_save(import: &ArtifactImport, opts: ImportOptions<'_>) -> Result<(), Error> {
    let program_info = import_artifact(import, opts)?;
    log::debug!("encoding");
    ctadl_import::save_program_info(program_info, import)
}

/// Records what an artifact imported out of itself onto its own config.
///
/// Reloads rather than saving the caller's copy back: a sub-import may have rewritten the
/// parent's config in the meantime, and the caller reloads after this to pick these names up.
#[cfg(feature = "apk")]
fn record_sub_imports(import: &ArtifactImport, sub_imports: Vec<String>) -> Result<(), Error> {
    if sub_imports.is_empty() {
        return Ok(());
    }
    log::info!(
        "'{}': {} sub-import(s) indexed alongside it: {}",
        import.name,
        sub_imports.len(),
        sub_imports.join(", ")
    );
    let mut updated = ArtifactImport::load_by_name(&import.name)?;
    updated.sub_imports = sub_imports;
    updated.save()
}

/// Import if absent or stale, then open: what a downstream tool wants when it has an artifact
/// and does not care whether the store is warm.
///
/// `artifact` is the thing on disk; `name` is what the import is called in the store. The
/// import is redone when there is none under that name, or when its recorded content hash no
/// longer matches `artifact`. Then the IR comes back through
/// [`ctadl_import::open_import`], so the version check and the preprocessing pipeline are the
/// same ones `ctadl index` uses.
pub fn open_or_import(
    name: &str,
    artifact: &std::path::Path,
    language: ArtifactLanguage,
    opts: ImportOptions<'_>,
    pipeline: ssa::Pipeline,
) -> Result<ProgramInfo, Error> {
    if !ArtifactImport::is_up_to_date(name, artifact)? {
        let import = ArtifactImport::try_create(name, language, artifact)?;
        let program_info = import_artifact(&import, opts)?;
        ctadl_import::save_program_info(program_info, &import)?;
        // `import_artifact` may have rewritten the config (sub-imports, a Ghidra image base),
        // so hash the artifact onto the current copy rather than the one created above.
        let mut import = ArtifactImport::load_by_name(name)?;
        import
            .record_artifact_hash()
            .err_context(|| format!("recording the artifact hash of import '{name}'"))?;
    }
    ctadl_import::open_import(name, pipeline)
}
