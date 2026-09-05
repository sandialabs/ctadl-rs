/*! App bundles (`.xapk`): a ZIP of split APKs.

An Android App Bundle is not distributed as one APK. It is a base APK holding the Dex, a
`config.<abi>.apk` holding that ABI's native libraries, and -- usually the majority of the file
count -- one resource-only split per language and screen density. APKPure and the like ship that
set zipped up as a single `.xapk`.

This module unwraps the bundle: extract each `*.apk`, import each through the ordinary APK path,
and record the result on the parent.

# Two things that are easy to get wrong

* **The sub-import list is flat.** [`crate::project::AnalysisProject::ephemeral`] expands exactly
  one level -- `once(name).chain(subs)`, with no recursion. So a bundle's `sub_imports` must
  hold, for each split, *its own name followed by its own `sub_imports`*. Nesting bundle → split
  → library without flattening drops every `.so` at index time, which looks exactly like the JNI
  bug this all exists to fix.
* **A resource-only split is skipped, not fatal.** They are the majority: one real bundle has 23
  of 30. [`apk_native::require_native_libs`] raises [`Error::NothingToImport`] for a split with
  neither Dex nor `lib/`; that one error is caught here and logged at debug. Anything else
  propagates.
*/

use std::path::{Path, PathBuf};

use dex_reader::apk::{has_dex_entries_of_file, read_bundle_entry, split_apk_entries_of_file};

use crate::cli::ImportOptions;
use ctadl_import::error::{Error, ErrorContext};
use crate::project::{ArtifactImport, ArtifactLanguage};

/// Imports every split APK out of an app bundle, returning the *flattened* sub-import names for
/// the caller to record on the parent import: each split, followed by that split's own
/// sub-imports.
///
/// # Errors
///
/// If the bundle cannot be read or listed, if a split cannot be written to the import directory,
/// or if importing a split fails for any reason other than its holding no code at all.
pub fn import_bundle(
    parent: &ArtifactImport,
    opts: ImportOptions<'_>,
) -> Result<Vec<String>, Error> {
    let bundle = &parent.artifact_path;
    let entries = split_apk_entries_of_file(bundle)
        .map_err(Error::Dex)
        .err_context(|| format!("listing splits in app bundle: '{}'", bundle.display()))?;
    if entries.is_empty() {
        return Err(Error::NothingToImport {
            message: format!(
                "'{}' has no top-level *.apk entries, so it is not an app bundle. An .xapk is a \
                 ZIP of split APKs; if this is a plain APK, rename it or pass `--language apk`.",
                bundle.display(),
            ),
        });
    }

    let splits_dir = splits_dir(parent);
    std::fs::create_dir_all(&splits_dir)
        .map_err(Error::Io)
        .err_context(|| format!("creating splits dir: '{}'", splits_dir.display()))?;

    // Extract first, then import: which splits carry Dex is a property of their contents, and
    // the import order depends on it.
    let mut splits: Vec<Split> = Vec::new();
    for entry in &entries {
        // Built from the stem, never from the raw ZIP entry name, so a hostile entry cannot
        // write outside `splits_dir`. See `dex_reader::apk::SplitApkEntry`.
        let dest = splits_dir.join(format!("{}.apk", sanitize(&entry.stem)));
        let bytes = read_bundle_entry(bundle, &entry.entry_name)
            .map_err(Error::Dex)
            .err_context(|| {
                format!(
                    "extracting '{}' from app bundle: '{}'",
                    entry.entry_name,
                    bundle.display()
                )
            })?;
        std::fs::write(&dest, &bytes)
            .map_err(Error::Io)
            .err_context(|| format!("writing split APK: '{}'", dest.display()))?;
        let has_dex = has_dex_entries_of_file(&dest)
            .map_err(Error::Dex)
            .err_context(|| format!("listing '{}'", dest.display()))?;
        splits.push(Split {
            name: sub_import_name(&parent.name, &entry.stem),
            path: dest,
            has_dex,
        });
    }
    order_splits(&mut splits);

    log::info!(
        "{}: {} split APK(s): {}",
        bundle.display(),
        splits.len(),
        splits
            .iter()
            .map(|split| split.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );

    let mut names = Vec::new();
    let mut skipped = 0usize;
    for Split { name, path, .. } in &splits {
        match import_split(name, path, opts) {
            Ok(sub_imports) => {
                // Flat, not nested: `AnalysisProject::ephemeral` expands one level only.
                names.push(name.clone());
                names.extend(sub_imports);
            }
            Err(e) if is_nothing_to_import(&e) => {
                skipped += 1;
                // The import directory was created before the split turned out to hold nothing;
                // leaving it behind would put an entry in `ctadl inspect`'s listing for every
                // language and density a bundle ships, which on a real one is most of the file.
                discard_import(name);
                log::debug!(
                    "{}: skipping '{}': it holds no code",
                    bundle.display(),
                    name
                );
            }
            Err(e) => return Err(e),
        }
    }
    log::info!(
        "{}: {} split(s) imported, {} resource-only split(s) skipped",
        bundle.display(),
        splits.len() - skipped,
        skipped,
    );
    Ok(names)
}

/// One extracted split, ready to import.
#[derive(Debug, PartialEq, Eq)]
struct Split {
    /// Sub-import name: `<parent>__<stem>`.
    name: String,
    /// Where it was extracted to, under the parent's `splits/`.
    path: PathBuf,
    has_dex: bool,
}

/// Puts the Dex-bearing splits first, so [`crate::cli::index`]'s per-import source-span scoping
/// stays in import order and the Java half of a JNI boundary is observed before the native half.
///
/// A stable sort, so the entry order -- already sorted by name -- decides within each group.
fn order_splits(splits: &mut [Split]) {
    splits.sort_by_key(|split| !split.has_dex);
}

/// Imports one extracted split as the APK sub-import `name`, returning *its* sub-imports (the
/// native libraries it in turn produced).
fn import_split(name: &str, dest: &Path, opts: ImportOptions<'_>) -> Result<Vec<String>, Error> {
    // The up-to-date check compares the content hash recorded by a previous import against the
    // file on disk, and the file was written just now, so this is checked after extraction.
    if opts.skip_existing && ArtifactImport::is_up_to_date(name, dest)? {
        let existing = ArtifactImport::load_by_name(name)?;
        log::info!("skipping split import '{name}': destination exists and hash matches");
        return Ok(existing.sub_imports);
    }

    let child = ArtifactImport::try_create(name, ArtifactLanguage::Apk, dest)?;
    crate::cli::import(&child, opts)?;

    // Reload rather than writing the stale in-memory copy back: the APK import records the
    // native libraries it extracted on the child's own config.
    let mut child = ArtifactImport::load_by_name(name)?;
    child.record_artifact_hash()?;
    Ok(child.sub_imports)
}

/// Removes the import directory created for a split that turned out to hold no code.
/// Best-effort: a store that cannot be tidied is not a reason to fail an import that otherwise
/// succeeded.
fn discard_import(name: &str) {
    let dir =
        crate::project::StorePaths::resolve(crate::project::StorePaths::relative_import_dir(name));
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        log::debug!("could not remove '{}': {e}", dir.display());
    }
}

/// True when `e` is (or wraps) [`Error::NothingToImport`] -- a split with neither Dex nor
/// `lib/<abi>/`, which is the ordinary majority case for a bundle rather than a failure.
fn is_nothing_to_import(e: &Error) -> bool {
    match e {
        Error::NothingToImport { .. } => true,
        Error::Context { source, .. } => source
            .downcast_ref::<Error>()
            .is_some_and(is_nothing_to_import),
        _ => false,
    }
}

/// Where a bundle import stashes the splits it extracted. Exposed so a caller that wants to look
/// at them (or clean them up) does not have to know the layout.
pub fn splits_dir(parent: &ArtifactImport) -> PathBuf {
    parent.import_path().join("splits")
}

/// The sub-import name for one split: `<parent>__<stem>`, following
/// [`crate::languages::apk_native`]'s naming. The parent's name is the prefix because a split's
/// own name is not unique -- every bundle has a `config.arm64_v8a.apk`.
fn sub_import_name(parent: &str, stem: &str) -> String {
    sanitize(&format!("{parent}__{stem}"))
}

/// Replaces every character that has no business in a directory name. Both the sub-import name
/// and the extracted file name go through this: the first becomes a directory under the store's
/// `imports/`, and the second a file under the import's `splits/`.
fn sanitize(raw: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_import_name_is_prefixed_and_sanitized() {
        assert_eq!(
            sub_import_name("chrome", "config.arm64_v8a"),
            "chrome__config.arm64_v8a"
        );
        assert_eq!(sub_import_name("a b", "x/y"), "a_b__x_y");
    }

    /// The split's file name is built from its stem, so nothing in a hostile bundle can redirect
    /// the write out of the import directory.
    #[test]
    fn a_traversing_stem_cannot_escape_the_splits_dir() {
        let name = sanitize("../../evil");
        assert_eq!(name, ".._.._evil");
        assert!(!name.contains('/'));
    }

    /// The one error a bundle import swallows, recognized through the context wrapper the store
    /// layer adds.
    #[test]
    fn nothing_to_import_is_recognized_through_context() {
        let bare = Error::NothingToImport {
            message: "resource-only".to_string(),
        };
        assert!(is_nothing_to_import(&bare));
        let wrapped: Result<(), Error> = Err(bare).err_context(|| "importing split");
        assert!(is_nothing_to_import(&wrapped.unwrap_err()));
        assert!(!is_nothing_to_import(&Error::Path {
            message: "unrelated".to_string()
        }));
    }
}
