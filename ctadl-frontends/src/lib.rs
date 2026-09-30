/*! Picks a front end for an artifact and turns it into CTADL IR.

The language front ends sit below this crate, one crate per language. None of them knows about
the others. The engine, `ctadl-ascent`, sits above. This crate holds the part in between: the
`match import.language` that picks a front end, plus the two container formats, which are ways
of packaging other artifacts rather than languages of their own:

* an APK. Its Java half is Dex. Its native half is a set of `.so` files, each imported through
  the pcode front end ([`apk_native`]).
* an app bundle (`.xapk`), which is a ZIP file of split APKs. Each one is imported recursively
  ([`xapk`]).

Neither one can live in `ctadl-dex`, because that would pull Ghidra into a crate that exists in
order to avoid it.

# Features

A program that uses this crate says which languages it reads. The default set is `dex`, `jvm`
and `apk`. `pcode`, `lua`, `c`, `xapk` and `flowy` are off unless asked for. Each one adds a
single front-end crate and the matching `ctadl-import` error variants.

[`ArtifactLanguage`] always lists every language, whatever is enabled. It is an enum, so the
unused names cost nothing, and `--help` should keep naming every language the tool knows about.
The features control the match arms instead. Ask for a language that was left out and the import
fails with [`Error::NothingToImport`], which names the feature that would turn it on. The
command-line flag itself still parses.

Turning on `apk` turns on `dex`, but not `pcode`. An APK imported without `pcode` still gets its
Java half, and the import reports that this build has no native front end. That is the same
outcome as importing an APK on a machine that has no Ghidra, which is common and supported.
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

/// Settings for one import, beyond the artifact itself and its language.
///
/// Every field here matters only for an APK. An APK is the one artifact that imports other
/// artifacts out of itself: its native libraries. See [`apk_native`]. [`Default`] gives the
/// simple behavior, which is to import everything and reuse nothing.
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions<'a> {
    /// Reuse a sub-import that was already done, when the artifact hash stored with it still
    /// matches, instead of importing it again. `main` does the same check for the parent
    /// artifact. This field carries the flag down to the sub-imports, where skipping the work
    /// saves much more time, because each one is a full disassembly run.
    pub skip_existing: bool,
    /// Import the native libraries packaged inside an APK. On by default.
    pub native_libs: bool,
    /// Import the libraries for this ABI instead of the preferred one. See
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

/// Builds the error a match arm returns when its language was left out of the build.
///
/// This reuses [`Error::NothingToImport`] on purpose instead of adding a new variant. From the
/// caller's point of view, this build really does have nothing it can import out of that
/// artifact. The message names the feature that would change that.
// This function is unused when every language is enabled, which is how `ctadl-ascent` builds
// it, and used in every smaller build. Both are fine, and listing the whole feature set in a
// `cfg_attr` to say so is not worth it.
#[allow(dead_code)]
fn unsupported(language: ArtifactLanguage, feature: &str) -> Error {
    Error::NothingToImport {
        message: format!(
            "this build cannot import {language} artifacts: it was compiled without \
             ctadl-frontends' `{feature}` feature"
        ),
    }
}

/// Imports one artifact into a [`ProgramInfo`], picking the front end from its language.
///
/// This writes to the store only when an artifact produces other imports, such as an APK's
/// native libraries or a bundle's split APKs. Those have to be finished before this function
/// returns. Saving the returned program is up to the caller. See
/// [`ctadl_import::save_program_info`], or [`open_or_import`] for the whole round trip.
// Only the container-format arms read `opts`, and the log line at the end cannot be reached in
// a build with no language enabled at all. Both depend on the feature set, not on the code.
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
    // The type is written out because in a build with no language enabled, every arm returns
    // early and there is nothing for the compiler to infer from.
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

/// Imports an APK: first the Java half from `classes*.dex`, then the native half from
/// `lib/<abi>/`.
#[cfg(feature = "apk")]
fn import_apk(import: &ArtifactImport, opts: ImportOptions<'_>) -> Result<ProgramInfo, Error> {
    // Do the Dex half first. It is cheap, and it is what fails quickly on a file that is not
    // really an APK, before any native library is extracted or handed to Ghidra.
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
    // A split APK from an app bundle has no Dex of its own, so its libraries are the whole
    // import. Check this before extracting anything, for two reasons. An APK with neither half
    // then fails right away. And the reported reason describes what is in the APK, rather than
    // how far `import_native_libs` got with it, since that function also returns no
    // sub-imports when Ghidra is missing.
    if dex_count == 0 {
        apk_native::require_native_libs(&import.artifact_path)?;
        if opts.native_libs {
            log::info!(
                "{}: no classes*.dex entries; importing as a native-only split APK",
                import.artifact_path.display(),
            );
        } else {
            // This is not an error, because the user asked for it. But the import will be
            // empty, and that is worth saying out loud.
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

/// Runs [`import_artifact`] and then writes the result to the store. This is what `ctadl
/// import` does for one artifact, and what a container format does for each artifact it
/// unpacks.
pub fn import_and_save(import: &ArtifactImport, opts: ImportOptions<'_>) -> Result<(), Error> {
    let program_info = import_artifact(import, opts)?;
    log::debug!("encoding");
    ctadl_import::save_program_info(program_info, import)
}

/// Records, in an artifact's own config, the imports it produced out of itself.
///
/// This reloads the config instead of saving the caller's copy back, because a sub-import may
/// have changed the parent's config in the meantime. The caller reloads after this call to see
/// the names written here.
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

/// Imports the artifact if it is missing or out of date, then opens it. This is what another
/// tool wants when it has an artifact in hand and does not care whether the store already holds
/// an import of it.
///
/// `artifact` is the file on disk. `name` is what the import is called in the store. The import
/// is redone when the store holds nothing under that name, or when the content hash recorded
/// for it no longer matches `artifact`. The IR then comes back through
/// [`ctadl_import::open_import`], so the version check and the preprocessing passes are the
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
        // `import_artifact` may have rewritten the config, adding sub-imports or a Ghidra
        // image base. So record the hash on a freshly loaded copy, not on the one created
        // above.
        let mut import = ArtifactImport::load_by_name(name)?;
        import
            .record_artifact_hash()
            .err_context(|| format!("recording the artifact hash of import '{name}'"))?;
    }
    ctadl_import::open_import(name, pipeline)
}
