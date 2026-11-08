/*! Native libraries packaged inside an APK.

An Android app's `native` methods are bodyless stubs in the Dex; the code that runs
lives in the `.so` files under `lib/<abi>/` inside the same APK. The
JNI bridge (`ctadl_ascent::languages::jni`)
already joins the two, but only if the native half was imported -- and it resolves
*across imports*, after the whole import loop has interned both programs' functions.

So importing an APK also imports its native libraries, each as its own **sub-import**
lowered through the [pcode front end][ctadl_pcode]. It has to be a separate
import rather than more IR inside the APK's own: [`ProgramInfo::vmt`](ctadl_ir::ProgramInfo::vmt) is an enum, and
`ctadl_ascent::languages::jni::JniObserver::observe` reads its variant to decide which half
of the boundary an import contributes. One `ProgramInfo` is either the Java side or the
native side, never both.

The sub-import names are recorded on the parent's [`ArtifactImport::sub_imports`], and
[`AnalysisProject::try_create`](ctadl_import::project::AnalysisProject::try_create) expands them, so naming the APK in
`ctadl index` co-indexes everything that came out of it.

# What this deliberately does not do

* **One ABI, not all of them.** See [`dex_reader::apk::ABI_PREFERENCE`]: the per-ABI
  builds are copies of one program, so importing several costs a full disassembly per
  copy and leaves duplicate `Java_…` symbols the bridge can only call ambiguous.
* **Nothing outside `lib/<abi>/`.** An app that ships a library in `assets/` and
  extracts it at runtime is not followed.
* **Nothing fatal.** No Ghidra, or one library that fails to disassemble, degrades to
  a warning: an APK import that loses its native half is still a useful Dex import.

# Several libraries at once

The libraries are independent imports, and each one is a Ghidra run whose JVM startup and
auto-analysis cost a fixed amount whatever the library's size, so they run concurrently under a
[`Budget`] of cores and memory. The biggest library starts first, since it is the one most
likely to finish last. A library's share of the budget follows its size:

* **Cores**: at least one per [`BYTES_PER_GHIDRA_THREAD`], and at least an even split of the
  cores free when it starts among the libraries not yet started -- so a dozen small libraries
  run side by side on a core each, and the last ones spread out over the cores the others gave
  back.
* **Heap**: [`ghidra_heap_mib`], which grows with the library and stops at Ghidra's default cap.
  A library alone in its APK keeps that default, as do the retries below.

A run that runs out of heap anyway is retried once with the default cap and the whole budget to
itself. Ghidra keeps going after an analysis task runs out of memory, and exports whatever it
got, so a heap cap that is too small would otherwise change the imported program rather than
fail.

Results are reported, and the sub-import list built, in entry order, the order the serial loop
used, whatever order the runs finish in. The log lines the runs write themselves (Ghidra's
progress, say) interleave; each names the library it is about.

# Native-only APKs

An Android App Bundle is distributed as several APKs -- a base one holding the Dex and
one `config.<abi>.apk` per ABI holding nothing but that ABI's `lib` directory. XAPK
bundles (APKPure and the like) ship exactly that. So the two halves above can arrive in
*different files*, and an APK with no `classes*.dex` at all is a normal thing to import:
it becomes a parent import with an empty Java program and one native sub-import per
library. [`require_native_libs`] is what keeps that from also swallowing the splits that
hold only resources, which have no code in them in either language.
*/

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use dex_reader::apk::{
    for_each_native_lib_of_file, looks_like_object_file, native_lib_entries_of_file,
    preferred_abi_of_file,
};

use crate::ImportOptions;
use crate::budget::{Budget, run_ordered};
use ctadl_import::error::{Error, ErrorContext};
use ctadl_import::project::{ArtifactImport, ArtifactLanguage};

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
    import_native_libs_in(parent, opts, None)
}

/// [`import_native_libs`] under `budget`, shared with whatever else the caller runs at the same
/// time, or under a budget of its own built from `opts.jobs` when `None`.
pub(crate) fn import_native_libs_in(
    parent: &ArtifactImport,
    opts: ImportOptions<'_>,
    budget: Option<&Budget>,
) -> Result<Vec<String>, Error> {
    if !opts.native_libs {
        return Ok(Vec::new());
    }

    let apk_path = &parent.artifact_path;
    let entries = native_lib_entries_of_file(apk_path)
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
    let (abi, unusable) = match opts.native_abi {
        Some(abi) => (abi, Vec::new()),
        None => {
            let choice = preferred_abi_of_file(apk_path, &entries).expect("entries is non-empty");
            (choice.abi, choice.unusable)
        }
    };
    if !unusable.is_empty() {
        log::info!(
            "{}: skipping {} -- {} no entry that is a loadable object file (an empty \
             placeholder, say); pass --native-abi to override",
            apk_path.display(),
            unusable.join(", "),
            if unusable.len() == 1 {
                "it has"
            } else {
                "they have"
            },
        );
    }
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
    if !native_frontend_available() {
        log::warn!(
            "{}: skipping {} {} native librar{} -- {}, so they cannot be disassembled; the \
             Dex half of this APK is imported either way.",
            apk_path.display(),
            selected,
            abi,
            if selected == 1 { "y" } else { "ies" },
            NO_NATIVE_FRONTEND,
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

    let dest_dir = extracted_libs_dir(parent).join(abi);
    std::fs::create_dir_all(&dest_dir)
        .map_err(Error::Io)
        .err_context(|| format!("creating native library dir: '{}'", dest_dir.display()))?;
    let libs = extract_native_libs(apk_path, abi, &dest_dir).err_context(|| {
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
            .map(|lib| lib.file_name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );

    // A library alone in its APK has nothing to share the budget with, so it keeps Ghidra's
    // default heap, as a single run always has.
    let alone = libs.iter().filter(|lib| lib.object_size.is_some()).count() == 1;
    let mut tasks = Vec::new();
    for lib in libs {
        let Some(size) = lib.object_size else {
            log::warn!(
                "{}: skipping '{}': not an object file a disassembler can load",
                apk_path.display(),
                lib.entry_name,
            );
            continue;
        };
        tasks.push(LibTask {
            name: sub_import_name(&parent.name, abi, &lib.file_name),
            dest: dest_dir.join(&lib.file_name),
            entry_name: lib.entry_name,
            size,
            heap_mib: if alone {
                DEFAULT_MAX_HEAP_MIB
            } else {
                ghidra_heap_mib(size)
            },
        });
    }

    let own_budget;
    let budget = match budget {
        Some(budget) => budget,
        None => {
            own_budget = Budget::from_jobs(opts.jobs);
            &own_budget
        }
    };
    // Biggest first, so the library most likely to finish last starts first; ties in entry
    // order, so the start order is fixed too.
    let mut start_order: Vec<usize> = (0..tasks.len()).collect();
    start_order.sort_by_key(|&i| std::cmp::Reverse(tasks[i].size));
    let waiting = std::sync::atomic::AtomicUsize::new(tasks.len());

    let mut names = Vec::new();
    let (mut reused, mut failed) = (0usize, 0usize);
    run_ordered(
        &tasks,
        &start_order,
        budget.cores(),
        |task| {
            let after = waiting.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            import_one(task, after, opts, budget)
        },
        |_| false,
        |index, result| {
            let task = &tasks[index];
            match result {
                Ok(Outcome::Imported) => names.push(task.name.clone()),
                Ok(Outcome::Reused) => {
                    log::info!(
                        "skipping native import '{}': destination exists and hash matches",
                        task.name
                    );
                    reused += 1;
                    names.push(task.name.clone());
                }
                Err(e) => {
                    failed += 1;
                    // Not fatal: the Dex half of the APK is still worth having, and the JNI
                    // bridge reports the resulting unlinked methods at index time.
                    log::warn!(
                        "{}: failed to import native library '{}' as '{}': {e}",
                        apk_path.display(),
                        task.entry_name,
                        task.name,
                    );
                }
            }
        },
    );

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

/// Says whether this build can turn a native library into IR at all. That needs two things: the
/// pcode front end compiled in, and Ghidra installed on the machine. Missing either one leads
/// to the same outcome, which is that the Java half is imported and the libraries are skipped
/// with one line of explanation. So both are checked here together.
#[inline]
fn native_frontend_available() -> bool {
    #[cfg(feature = "pcode")]
    {
        ctadl_pcode::ghidra_available()
    }
    #[cfg(not(feature = "pcode"))]
    {
        false
    }
}

/// The reason [`native_frontend_available`] returned false, written for the user to read.
#[cfg(feature = "pcode")]
const NO_NATIVE_FRONTEND: &str =
    "Ghidra was not found (set GHIDRA_HOME or put `ghidra` on PATH to analyze them)";
#[cfg(not(feature = "pcode"))]
const NO_NATIVE_FRONTEND: &str = "this build was compiled without ctadl-frontends' `pcode` feature";

/// One library extracted out of the APK by [`extract_native_libs`].
struct ExtractedLib {
    entry_name: String,
    file_name: String,
    /// Its size in bytes, or `None` when it is not an object file (see
    /// [`looks_like_object_file`]) and so was not written out.
    object_size: Option<u64>,
}

/// Writes every `lib/<abi>/*.so` entry of the APK at `apk_path` that is an object file to
/// `dest_dir`, one at a time, so neither the APK nor its libraries are ever held in memory
/// whole. Returns all of them, in entry order, the rest marked as not object files.
///
/// Every library is written before any import starts: the up-to-date check compares the
/// *content* hash recorded by a previous import against the file on disk, so the file has to be
/// the one this APK actually contains right now.
fn extract_native_libs(
    apk_path: &Path,
    abi: &str,
    dest_dir: &Path,
) -> Result<Vec<ExtractedLib>, Error> {
    let mut libs = Vec::new();
    for_each_native_lib_of_file(apk_path, abi, |entry, reader| -> Result<(), Error> {
        let mut magic = Vec::with_capacity(4);
        reader.take(4).read_to_end(&mut magic)?;
        let object_size = if looks_like_object_file(&magic) {
            // Built from `abi` and `file_name`, never from the raw entry name, so a hostile
            // entry cannot write outside `dest_dir`. See `dex_reader::apk::NativeLibEntry`.
            let dest = dest_dir.join(&entry.file_name);
            let mut write = || -> std::io::Result<u64> {
                let mut file = std::io::BufWriter::new(std::fs::File::create(&dest)?);
                file.write_all(&magic)?;
                let rest = std::io::copy(reader, &mut file)?;
                file.flush()?;
                Ok(magic.len() as u64 + rest)
            };
            Some(write().err_context(|| format!("writing native library: '{}'", dest.display()))?)
        } else {
            None
        };
        libs.push(ExtractedLib {
            entry_name: entry.entry_name.clone(),
            file_name: entry.file_name.clone(),
            object_size,
        });
        Ok(())
    })?;
    Ok(libs)
}

/// The JVM heap cap of Ghidra's default, used for a library that has the budget to itself.
#[cfg(feature = "pcode")]
const DEFAULT_MAX_HEAP_MIB: u64 = ctadl_pcode::ghidra::DEFAULT_MAX_HEAP_MIB;
#[cfg(not(feature = "pcode"))]
const DEFAULT_MAX_HEAP_MIB: u64 = 40 * 1024;

/// The heap every Ghidra run gets, however small its library: the JVM, Ghidra's program
/// database and its analyzers before the library adds anything.
const MIN_GHIDRA_HEAP_MIB: u64 = 3 * 1024;

/// The heap a Ghidra run gets per byte of library, on top of [`MIN_GHIDRA_HEAP_MIB`].
/// Generous: a run that needs more than its cap is retried, alone, with the default, at the cost
/// of the time the first attempt took.
const GHIDRA_HEAP_PER_BYTE: u64 = 2048;

/// The JVM heap cap, in MiB, for a Ghidra run over a library of `size` bytes that shares the
/// budget with others.
fn ghidra_heap_mib(size: u64) -> u64 {
    (MIN_GHIDRA_HEAP_MIB + size.saturating_mul(GHIDRA_HEAP_PER_BYTE) / (1 << 20))
        .min(DEFAULT_MAX_HEAP_MIB)
}

/// How big a library has to be to start with one more Ghidra thread. Ghidra spends most of its
/// time on a small library in work that does not run in parallel -- starting the JVM, loading
/// the program, the analyzers that walk it in order -- so a small one gets one core, and only a
/// library large enough to keep the parallel decompiler busy asks for more.
#[cfg(feature = "pcode")]
const BYTES_PER_GHIDRA_THREAD: u64 = 2 << 20;

/// The memory, in MiB, the in-process half of an import holds for a library of `size` bytes:
/// its facts read back and lowered to IR. An estimate, counted against the budget while it runs.
#[cfg(feature = "pcode")]
fn lowering_mib(size: u64) -> u64 {
    512 + size.saturating_mul(64) / (1 << 20)
}

/// One library to import, and the share of the budget it asks for.
struct LibTask {
    /// The sub-import name.
    name: String,
    /// Where the library was extracted to.
    dest: PathBuf,
    /// The ZIP entry it came from, for messages.
    entry_name: String,
    size: u64,
    /// The JVM heap cap its Ghidra run gets, in MiB.
    #[cfg_attr(not(feature = "pcode"), allow(dead_code))]
    heap_mib: u64,
}

/// What [`import_one`] did, so the caller can report reuse separately from work.
enum Outcome {
    Imported,
    Reused,
}

/// Imports one extracted library as its pcode sub-import, under a share of `budget`.
/// `waiting` is how many libraries, this one included, had yet to start when it did.
fn import_one(
    task: &LibTask,
    waiting: usize,
    opts: ImportOptions<'_>,
    budget: &Budget,
) -> Result<Outcome, Error> {
    // Disassembling a library takes minutes, so reusing an unchanged one is the
    // difference between a re-import that is cheap and one that is not.
    if opts.skip_existing && ArtifactImport::is_up_to_date(&task.name, &task.dest)? {
        return Ok(Outcome::Reused);
    }

    let child = ArtifactImport::try_create(&task.name, ArtifactLanguage::Pcode, &task.dest)?;
    lower_native(&child, task, waiting, budget)?;

    // `export_facts` records Ghidra's image base on the config and re-saves it, so
    // reload rather than writing the stale in-memory copy back over it.
    let mut child = ArtifactImport::load_by_name(&task.name)?;
    child.record_artifact_hash()?;
    Ok(Outcome::Imported)
}

/// Disassembles one extracted library and writes it to the store: Ghidra under a share of
/// `budget` sized for the library, then the lowering to IR, in process, under a smaller one.
///
/// This is a separate function so that the `pcode` feature has to switch out only this one
/// function, and not the extraction and scheduling around it. When the feature is off,
/// [`import_native_libs`] returns before anything calls this.
#[cfg(feature = "pcode")]
fn lower_native(
    child: &ArtifactImport,
    task: &LibTask,
    waiting: usize,
    budget: &Budget,
) -> Result<(), Error> {
    use crate::budget::Request;
    use ctadl_pcode::ghidra::GhidraLimits;

    let request = Request {
        min_cores: task.size.div_ceil(BYTES_PER_GHIDRA_THREAD).max(1) as usize,
        sharing: waiting,
        mib: task.heap_mib + crate::budget::JVM_OVERHEAD_MIB,
    };
    let mut grant = budget.acquire(request);
    let limits = GhidraLimits {
        jobs: grant.cores(),
        max_heap_mib: Some(task.heap_mib),
    };
    log::debug!(
        "'{}': {} byte(s), Ghidra gets {} thread(s) and a {} MiB heap",
        child.name,
        task.size,
        limits.jobs,
        task.heap_mib,
    );
    if let Err(e) = ctadl_pcode::export_facts(child, limits) {
        if task.heap_mib >= DEFAULT_MAX_HEAP_MIB || !ctadl_pcode::ghidra_ran_out_of_memory(child) {
            return Err(e);
        }
        drop(grant);
        log::info!(
            "'{}': Ghidra ran out of memory with a {} MiB heap; retrying alone with {} MiB",
            child.name,
            task.heap_mib,
            DEFAULT_MAX_HEAP_MIB,
        );
        grant = budget.acquire(Request {
            min_cores: budget.cores(),
            sharing: 1,
            mib: budget.memory_mib().max(DEFAULT_MAX_HEAP_MIB),
        });
        ctadl_pcode::export_facts(
            child,
            GhidraLimits {
                jobs: grant.cores(),
                max_heap_mib: Some(DEFAULT_MAX_HEAP_MIB),
            },
        )?;
    }
    // Ghidra has exited. What is left runs in this process, mostly on this one thread.
    grant.shrink(1, lowering_mib(task.size));
    budget.install(|| {
        let program_info = ctadl_pcode::lower_facts(child)?;
        ctadl_import::save_program_info(program_info, child)
    })
}

#[cfg(not(feature = "pcode"))]
fn lower_native(
    _child: &ArtifactImport,
    _task: &LibTask,
    _waiting: usize,
    _budget: &Budget,
) -> Result<(), Error> {
    unreachable!("import_native_libs returns early when the `pcode` feature is off")
}

/// The sub-import name for one library: `<parent>__<abi>__<stem>`.
///
/// It becomes a directory under the store's `imports/`, so it goes through [`sanitize`].
/// The parent's name is the prefix because the library name alone is not unique -- two
/// apps both shipping `libcrypto.so` would otherwise overwrite each other's import.
fn sub_import_name(parent: &str, abi: &str, file_name: &str) -> String {
    let stem = file_name.strip_suffix(".so").unwrap_or(file_name);
    sanitize(&format!("{parent}__{abi}__{stem}"))
}

/// Replaces every character outside `[A-Za-z0-9._-]` with `_`, so the result is a single
/// path component: it has no separator and cannot climb out of the directory it is joined to.
pub(crate) fn sanitize(raw: &str) -> String {
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
    parent.import_path().join("native")
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
