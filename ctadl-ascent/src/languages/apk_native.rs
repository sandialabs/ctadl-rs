/*! Native libraries packaged inside an APK.

An Android app's `native` methods are bodyless stubs in the Dex; the code that runs
lives in the `.so` files under `lib/<abi>/` inside the same APK. The
[JNI bridge][crate::languages::jni]
already joins the two, but only if the native half was imported -- and it resolves
*across imports*, after the whole import loop has interned both programs' functions.

So importing an APK also imports its native libraries, each as its own **sub-import**
lowered through the [pcode frontend][crate::languages::pcode]. It has to be a separate
import rather than more IR inside the APK's own: [`ProgramInfo::vmt`] is an enum, and
[`crate::languages::jni::JniObserver::observe`] reads its variant to decide which half
of the boundary an import contributes. One `ProgramInfo` is either the Java side or the
native side, never both.

The sub-import names are recorded on the parent's [`ArtifactImport::sub_imports`], and
[`crate::project::AnalysisProject::try_create`] expands them, so naming the APK in
`ctadl index` co-indexes everything that came out of it.

# What this deliberately does not do

* **One ABI, not all of them.** See [`dex_reader::apk::ABI_PREFERENCE`]: the per-ABI
  builds are copies of one program, so importing several costs a full disassembly per
  copy and leaves duplicate `Java_…` symbols the bridge can only call ambiguous.
* **Nothing outside `lib/<abi>/`.** An app that ships a library in `assets/` and
  extracts it at runtime is not followed.
* **Nothing fatal.** No Ghidra, or one library that fails to disassemble, degrades to
  a warning: an APK import that loses its native half is still a useful Dex import.

# Native-only APKs

An Android App Bundle is distributed as several APKs -- a base one holding the Dex and
one `config.<abi>.apk` per ABI holding nothing but that ABI's `lib` directory. XAPK
bundles (APKPure and the like) ship exactly that. So the two halves above can arrive in
*different files*, and an APK with no `classes*.dex` at all is a normal thing to import:
it becomes a parent import with an empty Java program and one native sub-import per
library. [`require_native_libs`] is what keeps that from also swallowing the splits that
hold only resources, which have no code in them in either language.
*/

use std::path::{Path, PathBuf};

use dex_reader::apk::{
    looks_like_object_file, native_lib_entries, native_lib_entries_of_file, preferred_abi,
    read_native_libs,
};

use crate::cli::ImportOptions;
use crate::error::{Error, ErrorContext};
use crate::languages::pcode;
use crate::project::{ArtifactImport, ArtifactLanguage};

/// Fails unless the APK at `apk_path` has at least one `lib/<abi>/*.so` entry. Only the ZIP central
/// directory is read.
///
/// # Errors
///
/// [`Error::NothingToImport`] when there are none, or [`Error::Dex`] if the APK cannot
/// be listed at all.
pub fn require_native_libs(apk_path: &Path) -> Result<(), Error> {
    let entries = native_lib_entries_of_file(apk_path)
        .map_err(Error::Dex)
        .err_context(|| format!("listing native libraries in '{}'", apk_path.display()))?;
    if entries.is_empty() {
        return Err(Error::NothingToImport {
            message: format!(
                "'{}' has no classes*.dex entries and no native libraries under lib/<abi>/. \
                 Split APKs of an app bundle divide one app across several files: the code is \
                 in the base APK (and in config.<abi>.apk for native code), not in the \
                 resource-only ones. Import those instead.",
                apk_path.display(),
            ),
        });
    }
    Ok(())
}

/// Imports the preferred-ABI native libraries out of an APK, one pcode sub-import
/// each. Returns the sub-import names in entry order, for the caller to record on the
/// parent import.
///
/// Returns an empty vector -- never an error -- when the APK has no native libraries,
/// when the caller disabled them, or when Ghidra is not available to disassemble them.
///
/// # Errors
///
/// Only for failures that are not specific to one library: the APK cannot be read, or
/// its entries cannot be listed.
pub fn import_native_libs(
    parent: &ArtifactImport,
    opts: ImportOptions<'_>,
) -> Result<Vec<String>, Error> {
    if !opts.native_libs {
        return Ok(Vec::new());
    }

    let apk_path = &parent.artifact_path;
    let apk_bytes = std::fs::read(apk_path)
        .map_err(Error::Io)
        .err_context(|| format!("reading APK: '{}'", apk_path.display()))?;
    let entries = native_lib_entries(&apk_bytes)
        .map_err(Error::Dex)
        .err_context(|| format!("listing native libraries in '{}'", apk_path.display()))?;
    if entries.is_empty() {
        log::debug!("{}: no native libraries under lib/", apk_path.display());
        return Ok(Vec::new());
    }

    // One ABI. An explicit `--native-abi` is honored even if the APK does not have it,
    // so the resulting "no libraries" is reported against what the user actually asked
    // for rather than silently falling back to another ABI's copy.
    //
    // Chosen before the Ghidra probe below so that probe can say how many libraries are
    // actually at stake: an APK shipping one library for four ABIs has four entries but
    // only ever imports one of them.
    let abi = match opts.native_abi {
        Some(abi) => abi,
        None => preferred_abi(&entries).expect("entries is non-empty"),
    };
    let selected = entries.iter().filter(|e| e.abi == abi).count();
    let skipped: Vec<&str> = {
        let mut other: Vec<&str> = entries
            .iter()
            .map(|e| e.abi.as_str())
            .filter(|a| *a != abi)
            .collect();
        other.sort_unstable();
        other.dedup();
        other
    };

    // Ask before extracting anything: on a machine with no Ghidra the whole exercise is
    // wasted, and the user deserves one clear line rather than a per-library failure.
    if !pcode::ghidra_available() {
        log::warn!(
            "{}: skipping {} {} native librar{} -- Ghidra was not found, so they cannot be \
             disassembled. Set GHIDRA_HOME or put `ghidra` on PATH to analyze them; the \
             Dex half of this APK is imported either way.",
            apk_path.display(),
            selected,
            abi,
            if selected == 1 { "y" } else { "ies" },
        );
        return Ok(Vec::new());
    }

    if skipped.is_empty() {
        log::info!(
            "{}: importing native libraries for {}",
            apk_path.display(),
            abi
        );
    } else {
        log::info!(
            "{}: importing native libraries for {} (ignoring {}; pass --native-abi to choose)",
            apk_path.display(),
            abi,
            skipped.join(", "),
        );
    }

    let libs = read_native_libs(&apk_bytes, abi)
        .map_err(Error::Dex)
        .err_context(|| {
            format!(
                "extracting {abi} native libraries from '{}'",
                apk_path.display()
            )
        })?;
    if libs.is_empty() {
        log::warn!(
            "{}: no native libraries for ABI '{}'",
            apk_path.display(),
            abi
        );
        return Ok(Vec::new());
    }
    // Name them: each becomes its own sub-import, and this is what tells the user which
    // parts were parsed out of the APK before the (slow) disassembly of each begins.
    log::info!(
        "{}: {} native librar{} found for {}: {}",
        apk_path.display(),
        libs.len(),
        if libs.len() == 1 { "y" } else { "ies" },
        abi,
        libs.iter()
            .map(|(e, _)| e.file_name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    // The APK is not needed past this point and can be tens of megabytes.
    drop(apk_bytes);

    let dest_dir = parent.import_path.join("native").join(abi);
    std::fs::create_dir_all(&dest_dir)
        .map_err(Error::Io)
        .err_context(|| format!("creating native library dir: '{}'", dest_dir.display()))?;

    let mut names = Vec::new();
    let (mut reused, mut failed) = (0usize, 0usize);
    for (entry, bytes) in libs {
        if !looks_like_object_file(&bytes) {
            log::warn!(
                "{}: skipping '{}': not an object file a disassembler can load",
                apk_path.display(),
                entry.entry_name,
            );
            continue;
        }
        // Built from `abi` and `file_name`, never from the raw entry name, so a hostile
        // entry cannot write outside `dest_dir`. See `dex_reader::apk::NativeLibEntry`.
        let dest = dest_dir.join(&entry.file_name);
        let name = sub_import_name(&parent.name, abi, &entry.file_name);

        match import_one(&dest, &bytes, &name, opts.skip_existing) {
            Ok(Outcome::Imported) => names.push(name),
            Ok(Outcome::Reused) => {
                reused += 1;
                names.push(name);
            }
            Err(e) => {
                failed += 1;
                // Not fatal: the Dex half of the APK is still worth having, and the JNI
                // bridge reports the resulting unlinked methods at index time.
                log::warn!(
                    "{}: failed to import native library '{}' as '{}': {e}",
                    apk_path.display(),
                    entry.entry_name,
                    name,
                );
            }
        }
    }

    log::info!(
        "{}: {} native librar{} ready ({} imported, {} reused, {} failed)",
        apk_path.display(),
        names.len(),
        if names.len() == 1 { "y" } else { "ies" },
        names.len() - reused,
        reused,
        failed,
    );
    Ok(names)
}

/// What [`import_one`] did, so the caller can report reuse separately from work.
enum Outcome {
    Imported,
    Reused,
}

/// Writes one extracted library to `dest` and imports it as the pcode sub-import
/// `name`.
fn import_one(
    dest: &Path,
    bytes: &[u8],
    name: &str,
    skip_existing: bool,
) -> Result<Outcome, Error> {
    // Write before the up-to-date check: it compares the *content* hash recorded by the
    // previous import against the file on disk, so the file has to be the one this APK
    // actually contains right now.
    std::fs::write(dest, bytes)
        .map_err(Error::Io)
        .err_context(|| format!("writing native library: '{}'", dest.display()))?;

    // Disassembling a library takes minutes, so reusing an unchanged one is the
    // difference between a re-import that is cheap and one that is not.
    if skip_existing && ArtifactImport::is_up_to_date(name, dest)? {
        log::info!("skipping native import '{name}': destination exists and hash matches");
        return Ok(Outcome::Reused);
    }

    let child = ArtifactImport::try_create(name, ArtifactLanguage::Pcode, dest)?;
    let program_info = pcode::import_pcode(&child)?;
    crate::cli::save_program_info(program_info, &child)?;

    // `import_pcode` records Ghidra's image base on the config and re-saves it, so
    // reload rather than writing the stale in-memory copy back over it.
    let mut child = ArtifactImport::load_by_name(name)?;
    child.record_artifact_hash()?;
    Ok(Outcome::Imported)
}

/// The sub-import name for one library: `<parent>__<abi>__<stem>`.
///
/// It becomes a directory under the store's `imports/`, so every character outside
/// `[A-Za-z0-9._-]` is replaced. The parent's name is the prefix because the library
/// name alone is not unique -- two apps both shipping `libcrypto.so` would otherwise
/// overwrite each other's import.
fn sub_import_name(parent: &str, abi: &str, file_name: &str) -> String {
    let stem = file_name.strip_suffix(".so").unwrap_or(file_name);
    let raw = format!("{parent}__{abi}__{stem}");
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Where an APK import stashes the libraries it extracted. Exposed so a caller that
/// wants to look at them (or clean them up) does not have to know the layout.
pub fn extracted_libs_dir(parent: &ArtifactImport) -> PathBuf {
    parent.import_path.join("native")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_import_name_is_prefixed_and_drops_the_so_suffix() {
        assert_eq!(
            sub_import_name("app", "arm64-v8a", "libfoo.so"),
            "app__arm64-v8a__libfoo"
        );
    }

    /// Two APKs shipping the same library name get distinct imports.
    #[test]
    fn sub_import_names_of_different_parents_do_not_collide() {
        assert_ne!(
            sub_import_name("app_a", "arm64-v8a", "libcrypto.so"),
            sub_import_name("app_b", "arm64-v8a", "libcrypto.so"),
        );
    }

    /// The name becomes a directory, so nothing that could redirect a path survives.
    #[test]
    fn sub_import_name_sanitizes_path_characters() {
        let name = sub_import_name("app", "arm64-v8a", "../../evil.so");
        assert_eq!(name, "app__arm64-v8a__.._.._evil");
        assert!(!name.contains('/'));

        assert_eq!(
            sub_import_name("a b", "x86", "lib f\\o:o.so"),
            "a_b__x86__lib_f_o_o"
        );
    }

    /// A library name without the suffix is left alone rather than losing a character.
    #[test]
    fn sub_import_name_tolerates_a_missing_suffix() {
        assert_eq!(sub_import_name("app", "x86", "libfoo"), "app__x86__libfoo");
    }
}
