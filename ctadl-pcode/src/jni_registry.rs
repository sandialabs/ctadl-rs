/*! Natives bound through `RegisterNatives`, recovered from the library's data sections.

The JNI bridge (`ctadl_ascent::languages::jni`) links a Java `native` method to its implementation by *name*: it mangles
`Java_com_example_Foo_bar` and looks that symbol up. Real Android apps mostly do not use that
convention. They call `env->RegisterNatives(clazz, table, n)` from `JNI_OnLoad`, passing an array of

```c
typedef struct { const char *name; const char *signature; void *fnPtr; } JNINativeMethod;
```

and the implementations keep private, unexported names.

Those tables are recoverable without Ghidra and without any dataflow analysis, because they are
plain initialized data. This module does that in three parts:

1. [`scan_bytes`] walks a shared library's writable, non-executable `PROGBITS` sections at pointer
   stride and accepts a triple whose first two slots point at a Java method name and a method
   descriptor and whose third points into executable code.
2. [`scan_import`] turns each `fnPtr` into the IR function that lives at that address -- or, when
   the pointer is a linker's branch veneer, at the far end of its one branch (see
   [`aarch64_branch_target`]) -- and writes the result beside the import's other artifacts as
   `jni-registry.json`.
3. [`attribute`] recovers the *class* -- which the table itself does not carry -- from the Dex
   side, by segmenting the address-ordered entries into runs that one class can explain.

# Why the scan can be this simple

A slot's value comes from one of three places. It is the addend of a relative dynamic relocation at
that offset when one exists; failing that, `st_value + addend` of an absolute pointer-width
relocation against a symbol this object defines; failing both, the word stored in the file. That
last rule is what covers `.relr.dyn` and 32-bit `.rel.dyn`, both of which keep the value in place --
so `RELR` needs no decoder here.

The absolute case is the one a library reaches by *exporting* its implementations: an exported
function is preemptible, so the linker leaves the reference symbolic and the word in the file stays
zero. Reading only the first two sources would reject the whole triple, and one unreadable `fnPtr`
costs the entire table.

Relocation sections are selected by `sh_type`, never by name: Android ships two *packed* formats
(`SHT_ANDROID_RELA` = `0x60000002`, `SHT_ANDROID_RELR` = `0x6fffff00`) under the standard section
names, and decoding an APS2 blob as an array of `Elf_Rela` yields plausible-looking garbage
addends. Only `SHT_RELA` is read; everything else falls through to the in-place read, which is
correct for those formats anyway. `object::read::elf::SectionHeader::rela` applies that gate for us.

Every failure is a quiet no-op. That is not just Mach-O/PE insurance: 17 of the 370 `.so` files in
the reference corpus are not ELF at all -- ZIPs of dex shipped under `lib/`, custom packed
containers, and one zero-length placeholder.
*/

use std::collections::BTreeMap;
use std::path::Path;

use hashbrown::hash_map::HashMap;
use hashbrown::hash_set::HashSet;
use object::elf;
use object::read::elf::{FileHeader, ProgramHeader, Rela, SectionHeader, Sym};
use object::{Endian, Endianness};
use serde::{Deserialize, Serialize};

use ctadl_import::error::{Error, ErrorContext};
use ctadl_import::project::ArtifactImport;

/// One recovered `JNINativeMethod`.
///
/// Both addresses are ELF virtual addresses, which is what a human spot-checks against the
/// library; the Ghidra address they were resolved through is `image_base + vaddr` and is not
/// persisted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Address of the entry itself, i.e. of the `JNINativeMethod` slot in `.data.rel.ro`.
    /// Rows are ordered by this, and [`attribute`] segments by it: a `JNINativeMethod[]` is
    /// contiguous, and that contiguity is the whole basis of class attribution.
    #[serde(default)]
    pub table_addr: u64,
    /// The `fnPtr` slot, with the Thumb bit already masked off.
    #[serde(default)]
    pub fn_addr: u64,
    /// The registered method's simple name.
    #[serde(default)]
    pub name: String,
    /// Its full method descriptor, e.g. `(JII[BI)V`.
    #[serde(default)]
    pub descriptor: String,
    /// Fully-qualified IR name of the function at `fn_addr`, or `None` when the disassembler
    /// found no function there.
    ///
    /// Such a row is kept and counted rather than dropped: dropping one would punch a hole in the
    /// address contiguity [`attribute`] depends on.
    #[serde(default)]
    pub function: Option<String>,
    /// Where a branch veneer at `fn_addr` led, when that is how [`function`][Self::function] was
    /// found. `None` for a row that resolved directly, and for one that did not resolve at all.
    ///
    /// `fn_addr` stays the address in the table -- that is the pointer `RegisterNatives` actually
    /// receives, and it is what a human spot-checks against the ELF -- so this is what records
    /// that the two differ. See [`aarch64_branch_target`].
    #[serde(default)]
    pub veneer_target: Option<u64>,
}

/// One import's recovered tables: the contents of its `jni-registry.json`.
///
/// Every field carries `#[serde(default)]`, following the [`ArtifactImport`] conventions, so a
/// later addition does not break a store written by this build.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JniRegistry {
    /// `sizeof(JNINativeMethod)`: 24 on ELF64, 12 on ELF32. [`attribute`] needs it to tell a
    /// contiguous table from two tables with a gap between them.
    #[serde(default)]
    pub entry_size: u64,
    /// Recovered entries, sorted by [`RegistryEntry::table_addr`].
    #[serde(default)]
    pub entries: Vec<RegistryEntry>,
}

impl JniRegistry {
    /// Reads the registry out of an import directory. `Ok(None)` when the import has none -- every
    /// import that is not an ELF shared library, and every import made before this existed.
    ///
    /// # Errors
    ///
    /// If the file exists but cannot be read or parsed.
    pub fn load(import: &ArtifactImport) -> Result<Option<Self>, Error> {
        let path = import.jni_registry_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).err_context(|| format!("reading '{}'", path.display()));
            }
        };
        let registry: Self = serde_json::from_str(&text)
            .err_context(|| format!("deserializing '{}'", path.display()))?;
        Ok(Some(registry))
    }

    /// Writes the registry into an import directory.
    ///
    /// # Errors
    ///
    /// If the file cannot be created or written.
    pub fn save(&self, import: &ArtifactImport) -> Result<(), Error> {
        let path = import.jni_registry_path();
        let file = std::fs::File::create(&path)
            .err_context(|| format!("creating '{}'", path.display()))?;
        serde_json::to_writer_pretty(file, self)
            .err_context(|| format!("writing '{}'", path.display()))?;
        Ok(())
    }

    /// Pretty-prints the registry at `path`, for `ctadl inspect`.
    ///
    /// It takes a raw path rather than an [`ArtifactImport`] for the same reason
    /// `ctadl_ascent::cli::inspect_bitcode` does: a store should stay inspectable when its import
    /// config is too old to load.
    ///
    /// # Errors
    ///
    /// If the file cannot be read or parsed.
    pub fn print(path: &Path) -> Result<(), Error> {
        let text = std::fs::read_to_string(path)
            .err_context(|| format!("reading '{}'", path.display()))?;
        let registry: Self = serde_json::from_str(&text)
            .err_context(|| format!("deserializing '{}'", path.display()))?;
        println!(
            "{} RegisterNatives entr{} (entry size {})",
            registry.entries.len(),
            if registry.entries.len() == 1 {
                "y"
            } else {
                "ies"
            },
            registry.entry_size,
        );
        for entry in &registry.entries {
            let via = match entry.veneer_target {
                Some(target) => format!(" (via a veneer to {target:#x})"),
                None => String::new(),
            };
            println!(
                "  {:#x}  {:#x}  {}{}  -> {}{via}",
                entry.table_addr,
                entry.fn_addr,
                entry.name,
                entry.descriptor,
                entry.function.as_deref().unwrap_or("<no function>"),
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Import-time scan
// ---------------------------------------------------------------------------

/// Scans an import's artifact for `RegisterNatives` tables and writes `jni-registry.json` into
/// its import directory. Does nothing at all -- without erroring -- when the artifact is not an
/// ELF file on disk, or when it holds no tables.
///
/// `entry_points` maps a function's *Ghidra* entry address to its fully-qualified IR name; see
/// [`crate`]. `image_base` is what Ghidra loaded the library at, so an ELF
/// virtual address `v` is the Ghidra address `image_base + v` once the first `PT_LOAD`'s own
/// `p_vaddr` is subtracted (it is 0 for every Android shared library, but it is read rather than
/// assumed).
///
/// # Errors
///
/// Only if the registry cannot be written. A file that cannot be read, or is not an ELF, or is
/// truncated, is a quiet no-op.
pub fn scan_import(
    import: &ArtifactImport,
    image_base: Option<i64>,
    entry_points: &BTreeMap<i64, String>,
) -> Result<(), Error> {
    // `artifact_path` is not always a file: a `ghidra://` server URL or a `.gpr` project has no
    // ELF to scan.
    let path = &import.artifact_path;
    match crate::ghidra::GhidraSource::detect(path) {
        Ok(crate::ghidra::GhidraSource::Binary(_)) => {}
        Ok(_) => {
            log::debug!(
                "jni registry: '{}' is not a binary file, so it has no RegisterNatives tables \
                 to scan",
                import.name
            );
            return Ok(());
        }
        Err(e) => {
            log::debug!("jni registry: skipping '{}': {e}", import.name);
            return Ok(());
        }
    }

    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(e) => {
            log::debug!(
                "jni registry: skipping '{}': cannot read '{}': {e}",
                import.name,
                path.display()
            );
            return Ok(());
        }
    };
    let Some(scan) = scan_bytes(&data) else {
        log::debug!(
            "jni registry: '{}' is not an ELF file this scan understands",
            import.name
        );
        return Ok(());
    };
    if scan.entries.is_empty() {
        log::debug!(
            "jni registry: no RegisterNatives tables in '{}'",
            import.name
        );
        return Ok(());
    }

    // Without an image base there is no way to turn an ELF address into the Ghidra address the
    // function map is keyed by. Emitting rows with no function would silently degrade tier-1
    // attribution into "everything unattributed", so say so instead.
    let Some(image_base) = image_base else {
        log::warn!(
            "jni registry: '{}' has {} RegisterNatives entr{}, but no image base was recorded, \
             so they cannot be mapped to functions; not writing a registry",
            import.name,
            scan.entries.len(),
            if scan.entries.len() == 1 { "y" } else { "ies" },
        );
        return Ok(());
    };
    if scan.load_bias != 0 {
        log::debug!(
            "jni registry: '{}' has a first PT_LOAD at {:#x}; subtracting it from every \
             recovered address",
            import.name,
            scan.load_bias
        );
    }

    let mut mapped = 0usize;
    let mut through_veneer = 0usize;
    let entries: Vec<RegistryEntry> = scan
        .entries
        .into_iter()
        .map(|raw| {
            let ghidra =
                |addr: u64| image_base.wrapping_add(addr.wrapping_sub(scan.load_bias) as i64);
            let mut function = entry_points.get(&ghidra(raw.fn_addr)).cloned();
            let mut veneer_target = None;
            // Only when the pointer itself named nothing: a veneer Ghidra *did* make a function
            // of is already the right answer, and following it would name the callee instead.
            if function.is_none()
                && let Some(target) = raw.veneer_target
                && let Some(name) = entry_points.get(&ghidra(target))
            {
                function = Some(name.clone());
                veneer_target = Some(target);
                through_veneer += 1;
            }
            if function.is_some() {
                mapped += 1;
            }
            RegistryEntry {
                table_addr: raw.table_addr,
                fn_addr: raw.fn_addr,
                name: raw.name,
                descriptor: raw.descriptor,
                function,
                veneer_target,
            }
        })
        .collect();

    let via = if through_veneer == 0 {
        String::new()
    } else {
        format!(", {through_veneer} through a branch veneer")
    };
    log::info!(
        "jni registry: {} RegisterNatives entr{} recovered from '{}' ({} with a function{via})",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" },
        import.name,
        mapped,
    );
    JniRegistry {
        entry_size: scan.entry_size,
        entries,
    }
    .save(import)
}

/// One `JNINativeMethod` as it sits in the library, before an IR function is attached.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawEntry {
    table_addr: u64,
    fn_addr: u64,
    name: String,
    descriptor: String,
    /// Where the branch at `fn_addr` goes, when `fn_addr` holds one single AArch64 `B` and its
    /// target is executable. Recovered here because only the ELF can answer it; used only as a
    /// fallback, in [`scan_import`].
    veneer_target: Option<u64>,
}

/// What [`scan_bytes`] recovered from one library.
#[derive(Debug, Default, PartialEq, Eq)]
struct Scan {
    /// `sizeof(JNINativeMethod)` for this ELF class.
    entry_size: u64,
    /// `p_vaddr` of the first `PT_LOAD`. Zero for every Android shared library; read rather than
    /// assumed, because the Ghidra address of an ELF address `v` is `image_base + (v - bias)`.
    load_bias: u64,
    /// Recovered entries, in address order.
    entries: Vec<RawEntry>,
}

/// Longest string either pointer slot may point at. A `JNINativeMethod`'s name and signature are
/// both short; this only bounds the work done rejecting a bad candidate.
const MAX_STRING: usize = 512;

/// Scans an ELF image for `JNINativeMethod` tables. `None` when `data` is not an ELF this build
/// understands -- a truncated header, a foreign magic, an empty file -- which are all things the
/// reference corpus actually contains under `lib/<abi>/*.so`.
fn scan_bytes(data: &[u8]) -> Option<Scan> {
    // Three separate quiet returns, all of which the corpus contains: a zero-length file, a
    // truncated header, and a foreign magic (`PK\x03\x04`, `\x7fKOM`, `SKCL`).
    let ident = data.get(..16)?;
    if ident[..4] != elf::ELFMAG {
        return None;
    }
    // `Ident::class` is the fifth byte.
    match ident[4] {
        elf::ELFCLASS32 => scan_elf::<elf::FileHeader32<Endianness>>(data),
        elf::ELFCLASS64 => scan_elf::<elf::FileHeader64<Endianness>>(data),
        _ => None,
    }
}

fn scan_elf<Elf>(data: &[u8]) -> Option<Scan>
where
    Elf: FileHeader<Endian = Endianness>,
{
    let header = Elf::parse(data).ok()?;
    let endian = header.endian().ok()?;
    let machine = header.e_machine(endian);
    let pointer = if header.is_type_64() { 8u64 } else { 4 };
    let entry_size = pointer * 3;
    // A Thumb function pointer carries its low bit set, and unmasked it matches no function
    // entry point. This is the common case on armeabi-v7a, not a corner case: 92% of the
    // reference corpus's 32-bit ARM entries have it set.
    let thumb_mask = machine == elf::EM_ARM;

    let load_bias = header
        .program_headers(endian, data)
        .ok()?
        .iter()
        .find(|ph| ph.p_type(endian) == elf::PT_LOAD)
        .map_or(0, |ph| ph.p_vaddr(endian).into());

    let sections = header.sections(endian, data).ok()?;

    // Where code lives: a `fnPtr` has to land in one of these.
    let mut executable: Vec<(u64, u64)> = Vec::new();
    // Every allocated section that has bytes in the file, so a pointer slot can be followed to
    // the string it names (which lives in `.rodata`, not in the section being scanned).
    let mut mapped: Vec<(u64, &[u8])> = Vec::new();
    // Load-time value of every slot a dynamic relocation writes, keyed by the address it applies
    // to. Both kinds this reads land here: a relative relocation's addend, and an absolute one's
    // resolved symbol address.
    let mut resolved: HashMap<u64, u64> = HashMap::new();

    for section in sections.iter() {
        let flags: u64 = section.sh_flags(endian).into();
        let addr: u64 = section.sh_addr(endian).into();
        let size: u64 = section.sh_size(endian).into();
        if flags & u64::from(elf::SHF_ALLOC) != 0 {
            if flags & u64::from(elf::SHF_EXECINSTR) != 0 {
                executable.push((addr, addr.saturating_add(size)));
            }
            if let Ok(bytes) = section.data(endian, data)
                && !bytes.is_empty()
            {
                mapped.push((addr, bytes));
            }
        }
        // `rela` is gated on `sh_type == SHT_RELA`, which is exactly the discipline this needs:
        // a section *named* `.rela.dyn` may be a packed Android blob with a non-standard type.
        if let Ok(Some((relas, link))) = section.rela(endian, data) {
            // The linked symbol table, needed only by the absolute arm. Resolved once per
            // section, and its failure costs only that arm: `sh_link` is 0 on a relocation
            // section that references no symbol, and losing the relative addends over that would
            // be a regression.
            let symbols = sections.symbol_table_by_index(endian, data, link).ok();
            for rela in relas {
                let r_type = rela.r_type(endian, false);
                let offset: u64 = rela.r_offset(endian).into();
                let addend: i64 = rela.r_addend(endian).into();
                if is_relative(machine, r_type) {
                    resolved.insert(offset, addend as u64);
                } else if is_absolute_pointer(machine, r_type)
                    && let Some(symbols) = symbols.as_ref()
                    && let Some(index) = rela.symbol(endian, false)
                    && let Ok(sym) = symbols.symbol(index)
                    // An undefined symbol is bound to some other library at load time, and its
                    // `st_value` here is zero -- which is the very failure this arm exists to
                    // fix, so taking it would only move the bug.
                    && sym.st_shndx(endian) != elf::SHN_UNDEF
                {
                    let value: u64 = sym.st_value(endian).into();
                    // `or_insert`, so that a relative relocation at the same address wins
                    // whichever order the two are seen in: it is the linker's final answer.
                    resolved
                        .entry(offset)
                        .or_insert(value.wrapping_add(addend as u64));
                }
            }
        }
    }

    let mut entries = Vec::new();
    for section in sections.iter() {
        // Tables live in initialized, writable, non-executable data: `.data.rel.ro`, `.data`.
        if section.sh_type(endian) != elf::SHT_PROGBITS {
            continue;
        }
        let flags: u64 = section.sh_flags(endian).into();
        if flags & u64::from(elf::SHF_ALLOC) == 0
            || flags & u64::from(elf::SHF_WRITE) == 0
            || flags & u64::from(elf::SHF_EXECINSTR) != 0
        {
            continue;
        }
        let Ok(bytes) = section.data(endian, data) else {
            continue;
        };
        let base: u64 = section.sh_addr(endian).into();

        let slot = |addr: u64| -> Option<u64> {
            if let Some(value) = resolved.get(&addr) {
                return Some(*value);
            }
            let offset = addr.checked_sub(base)? as usize;
            read_word(bytes, offset, pointer, endian)
        };

        let mut offset = 0u64;
        while offset + entry_size <= bytes.len() as u64 {
            let addr = base + offset;
            let candidate = (|| {
                let name = read_cstr(&mapped, slot(addr)?)?;
                if !is_java_method_name(name) {
                    return None;
                }
                let descriptor = read_cstr(&mapped, slot(addr + pointer)?)?;
                if !is_method_descriptor(descriptor) {
                    return None;
                }
                let mut fn_addr = slot(addr + 2 * pointer)?;
                if thumb_mask {
                    fn_addr &= !1;
                }
                let is_executable = |a: u64| executable.iter().any(|(lo, hi)| a >= *lo && a < *hi);
                if !is_executable(fn_addr) {
                    return None;
                }
                let veneer_target = if machine == elf::EM_AARCH64 {
                    read_u32(&mapped, fn_addr, endian)
                        .and_then(|word| aarch64_branch_target(word, fn_addr))
                        .filter(|target| is_executable(*target))
                } else {
                    None
                };
                Some(RawEntry {
                    table_addr: addr,
                    fn_addr,
                    name: name.to_string(),
                    descriptor: descriptor.to_string(),
                    veneer_target,
                })
            })();
            // Advance by a whole entry on a match, by one pointer otherwise. Scanning at a flat
            // pointer stride would otherwise yield overlapping triples over a real table; the
            // misaligned ones are rejected on their own merits, but this is what keeps table
            // *contiguity* -- which `attribute` reads as a table boundary -- meaningful.
            match candidate {
                Some(entry) => {
                    entries.push(entry);
                    offset += entry_size;
                }
                None => offset += pointer,
            }
        }
    }

    entries.sort_by_key(|e| e.table_addr);
    Some(Scan {
        entry_size,
        load_bias,
        entries,
    })
}

/// True when `r_type` is the machine's "relative" relocation, whose addend *is* the value the
/// slot takes at load time.
fn is_relative(machine: u16, r_type: u32) -> bool {
    match machine {
        elf::EM_AARCH64 => r_type == elf::R_AARCH64_RELATIVE,
        elf::EM_X86_64 => r_type == elf::R_X86_64_RELATIVE,
        elf::EM_ARM => r_type == elf::R_ARM_RELATIVE,
        elf::EM_386 => r_type == elf::R_386_RELATIVE,
        _ => false,
    }
}

/// True when `r_type` is the machine's pointer-width *absolute* relocation, whose slot takes the
/// referenced symbol's address plus the addend.
///
/// This is what a `fnPtr` gets when the implementation it names is **exported**: an exported
/// function in a shared object is preemptible, so the linker cannot commit to an address and must
/// leave the reference symbolic instead of emitting a `RELATIVE`. The word in the file is then
/// zero. Libraries built the ordinary Android way -- hidden visibility -- never reach here.
fn is_absolute_pointer(machine: u16, r_type: u32) -> bool {
    match machine {
        elf::EM_AARCH64 => r_type == elf::R_AARCH64_ABS64,
        elf::EM_X86_64 => r_type == elf::R_X86_64_64,
        elf::EM_ARM => r_type == elf::R_ARM_ABS32,
        elf::EM_386 => r_type == elf::R_386_32,
        _ => false,
    }
}

/// Reads a pointer-sized little/big-endian word out of `bytes` at `offset`.
fn read_word(bytes: &[u8], offset: usize, pointer: u64, endian: Endianness) -> Option<u64> {
    let end = offset.checked_add(pointer as usize)?;
    let slice = bytes.get(offset..end)?;
    Some(if pointer == 8 {
        endian.read_u64_bytes(slice.try_into().ok()?)
    } else {
        u64::from(endian.read_u32_bytes(slice.try_into().ok()?))
    })
}

/// Reads the 4-byte instruction word at `addr`, wherever in the image that is.
fn read_u32(mapped: &[(u64, &[u8])], addr: u64, endian: Endianness) -> Option<u32> {
    for (base, bytes) in mapped {
        if addr < *base || addr >= base + bytes.len() as u64 {
            continue;
        }
        let offset = (addr - base) as usize;
        let slice = bytes.get(offset..offset.checked_add(4)?)?;
        return Some(endian.read_u32_bytes(slice.try_into().ok()?));
    }
    None
}

/// The target of an AArch64 `B` at `addr`, or `None` when `word` is any other instruction.
///
/// A linker that cannot reach the implementation with one `BL` emits a **veneer**: a stub holding
/// nothing but this branch, and registers *that* address. Ghidra creates no function object at a
/// bare 4-byte thunk, so the `fnPtr` names nothing and the native goes unlinked -- which is why
/// Messenger's `libsuperpack-jni.so` mapped 0 of 28 entries while the same library in Facebook
/// Lite, linked without veneers, mapped 28 of 28.
///
/// Following the branch gives the real implementation. That is the right answer for a genuine
/// veneer and also for the one other thing this decodes, a one-instruction tail-call thunk:
/// either way the arguments arrive unchanged at the target, so it is where the taint goes.
///
/// One hop, deliberately. Every one of the 62 veneers in the reference corpus is a single branch
/// straight to its implementation; a chain would leave the row unresolved, exactly as it is today.
///
/// The encoding is `0b000101` : `imm26`, and the offset is `imm26` sign-extended and scaled by 4.
/// AArch64 only: measured across the corpus, the 32-bit libraries hold no veneers at all here --
/// their unmapped pointers are ordinary Thumb functions Ghidra did not recognize, which following
/// a branch cannot fix.
fn aarch64_branch_target(word: u32, addr: u64) -> Option<u64> {
    if word >> 26 != 0b000101 {
        return None;
    }
    // Sign-extend imm26, then scale: `((word << 6) as i32 >> 6)` keeps the sign in one step.
    let offset = i64::from(((word << 6) as i32) >> 6) * 4;
    Some(addr.wrapping_add(offset as u64))
}

/// Follows a pointer slot to the NUL-terminated string it names, wherever in the image that is.
fn read_cstr<'a>(mapped: &[(u64, &'a [u8])], addr: u64) -> Option<&'a str> {
    if addr == 0 {
        return None;
    }
    for (base, bytes) in mapped {
        if addr < *base || addr >= base + bytes.len() as u64 {
            continue;
        }
        let rest = &bytes[(addr - base) as usize..];
        let end = rest.iter().take(MAX_STRING).position(|b| *b == 0)?;
        return std::str::from_utf8(&rest[..end]).ok();
    }
    None
}

/// True for a string that could be a Java method name: the two constructors, or an identifier
/// made of the characters a compiler actually emits.
///
/// Deliberately narrower than the JVM's unqualified-name grammar, which forbids only `.;[/`. The
/// looser rule admits arbitrary punctuation, and this predicate's job is to reject three
/// unrelated words that happen to sit next to each other in `.data`.
fn is_java_method_name(s: &str) -> bool {
    if s == "<init>" || s == "<clinit>" {
        return true;
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// True for a well-formed method descriptor, *including* its return type.
///
/// [`descriptor_params`] stops at the `)` and never looks at what follows, so `"(I)garbage"`
/// passes it. Checking the tail here is cheap defence; measured across the reference corpus the
/// strict and lax rules admit identical candidate sets, because the executable-section and
/// method-name tests already do the real filtering.
fn is_method_descriptor(s: &str) -> bool {
    let Some(params) = descriptor_params(s) else {
        return false;
    };
    // `params` are slices of `s` after the opening `(`, so their total length locates the `)`.
    let consumed: usize = params.iter().map(|p| p.len()).sum();
    let Some(tail) = s.get(1 + consumed + 1..) else {
        return false;
    };
    tail == "V" || is_type_descriptor(tail)
}

/// True when `s` is exactly one field type descriptor: `I`, `[[J`, `Ljava/lang/String;`.
fn is_type_descriptor(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while bytes.get(i) == Some(&b'[') {
        i += 1;
    }
    match bytes.get(i) {
        Some(b'L') => {
            let class = &s[i + 1..];
            class.len() > 1 && class.ends_with(';') && !class[..class.len() - 1].contains(';')
        }
        Some(b'Z' | b'B' | b'C' | b'S' | b'I' | b'J' | b'F' | b'D') => i + 1 == bytes.len(),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Attribution
// ---------------------------------------------------------------------------

/// The Java side of attribution, built once per project from every import's `native` methods.
///
/// The table side is per-import (a `table_addr` order only means something within one library);
/// this side is project-global, and that asymmetry is what makes split APKs work at all -- in an
/// app bundle the `.so` and the `classes.dex` live in *different* imports.
#[derive(Debug, Default)]
pub struct ClassIndex<'a> {
    /// (simple name, descriptor) -> the classes declaring a `native` with that exact signature.
    signatures: HashMap<(&'a str, &'a str), Vec<&'a str>>,
    /// class -> how many `native` methods it declares. A run longer than this cannot be that
    /// class's table.
    counts: HashMap<&'a str, usize>,
}

impl<'a> ClassIndex<'a> {
    /// Builds the index from `(class, simple name, descriptor)` triples -- the deduplicated
    /// `native` list [`super::link`] already has in hand.
    pub fn build(natives: impl Iterator<Item = (&'a str, &'a str, &'a str)>) -> Self {
        let mut index = Self::default();
        for (class, name, descriptor) in natives {
            index
                .signatures
                .entry((name, descriptor))
                .or_default()
                .push(class);
            *index.counts.entry(class).or_default() += 1;
        }
        index
    }

    fn classes(&self, name: &'a str, descriptor: &'a str) -> &[&'a str] {
        self.signatures
            .get(&(name, descriptor))
            .map_or(&[][..], |v| v.as_slice())
    }

    fn declared(&self, class: &str) -> usize {
        self.counts.get(class).copied().unwrap_or(0)
    }
}

/// One entry tier 1 attributed, and the class it went to.
#[derive(Debug, PartialEq, Eq)]
pub struct Attributed<'a> {
    pub class: &'a str,
    pub entry: &'a RegistryEntry,
}

/// What [`attribute`] made of one import's entries.
#[derive(Debug, Default)]
pub struct Attribution<'a> {
    /// How many runs the entries segmented into -- the recovered table count.
    pub tables: usize,
    pub attributed: Vec<Attributed<'a>>,
    /// Entries in a run that stayed multi-class, or that matched no declared `native` at all.
    /// Counted and reported, never guessed at.
    pub unattributed: usize,
}

/// Recovers the declaring class of each entry, which the table itself does not carry.
///
/// **Tier 1 -- contiguous runs, and the only tier.** Walk the `table_addr`-ordered entries
/// keeping the set of Java classes that declare *every* entry in the current run, matched on name
/// and full descriptor. When that set goes empty the run closes and a new one starts at the
/// current entry. A closed run whose set is a single class attributes all of its entries to that
/// class; anything else is counted unattributed.
///
/// Matching is by *containment*, never by count: `SuperpackArchive` declares 14 natives and
/// registers 13.
///
/// The greedy rule alone can silently merge two adjacent tables -- if table B's leading entries
/// happen to also be declared by table A's class, the set never empties and B's entries are
/// attributed to A, a fabricated link that is worse than a miss. Three exact guards close it:
///
/// 1. **Split at address gaps.** `table_addr` must be the previous entry's plus the entry size.
/// 2. **Split on a repeated `(name, descriptor)`.** A `JNINativeMethod[]` cannot register the
///    same method twice, so a repeat is proof of a boundary.
/// 3. **Cap a run at the number of natives its class declares.** A run longer than that is
///    impossible for that class.
///
/// Guard 1 alone is provably insufficient: on `libsuperpack-jni.so` splitting on address gaps
/// yields 2 runs for 3 adjacent tables, and adding the other two yields exactly 3. Guard 2 is
/// what recovers the third boundary there; guard 3 is a backstop that, as long as guard 2 holds
/// and no class declares the same signature twice, cannot fire -- a class that declares every
/// *distinct* entry of a run declares at least as many natives as the run is long. It is kept
/// because it costs one comparison and because that proof rests on the Java side being
/// well-formed, which is an assumption about someone else's input.
///
/// Attributing nothing is the right answer when the Java half is absent -- libbluray's BD-J
/// bindings inside VLC, a feature-split dex in TikTok. There the candidate set empties on every
/// entry, each entry closes its own run, and nothing is misattributed.
pub fn attribute<'a>(registry: &'a JniRegistry, index: &ClassIndex<'a>) -> Attribution<'a> {
    let mut out = Attribution::default();
    let entries = &registry.entries;
    if entries.is_empty() {
        return out;
    }

    // Classes that declare every entry of the run currently being built, and the signatures it
    // has already registered.
    let mut live: Vec<&'a str> = Vec::new();
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut start = 0usize;

    for (i, entry) in entries.iter().enumerate() {
        let key = (entry.name.as_str(), entry.descriptor.as_str());
        let declaring = index.classes(entry.name.as_str(), entry.descriptor.as_str());

        if i > start {
            // A zero entry size means the registry predates the field; without it a gap cannot be
            // told from contiguity, so guard 1 abstains rather than splitting everything.
            let contiguous = registry.entry_size == 0
                || entry.table_addr == entries[i - 1].table_addr + registry.entry_size;
            let repeated = seen.contains(&key);
            let next = narrow(&live, declaring, index, i - start + 1);
            if contiguous && !repeated && !next.is_empty() {
                live = next;
                seen.insert(key);
                continue;
            }
            close_run(&mut out, entries, start, i, &live);
            start = i;
        }

        live = narrow_first(declaring, index);
        seen.clear();
        seen.insert(key);
    }
    close_run(&mut out, entries, start, entries.len(), &live);
    out
}

/// The classes still viable for a run of `length` entries: those in `live` that also declare the
/// new entry (guard 2 of the greedy rule) and that declare at least `length` natives (guard 3).
fn narrow<'a>(
    live: &[&'a str],
    declaring: &[&'a str],
    index: &ClassIndex<'a>,
    length: usize,
) -> Vec<&'a str> {
    live.iter()
        .filter(|class| declaring.contains(*class) && index.declared(class) >= length)
        .copied()
        .collect()
}

/// The same, for a run that is starting: every class declaring the entry, since a run of one
/// entry needs only one declared native.
fn narrow_first<'a>(declaring: &[&'a str], index: &ClassIndex<'a>) -> Vec<&'a str> {
    declaring
        .iter()
        .filter(|class| index.declared(class) >= 1)
        .copied()
        .collect()
}

/// Closes the run `entries[start..end]`, attributing it when exactly one class survived.
fn close_run<'a>(
    out: &mut Attribution<'a>,
    entries: &'a [RegistryEntry],
    start: usize,
    end: usize,
    live: &[&'a str],
) {
    if start >= end {
        return;
    }
    out.tables += 1;
    match live {
        [class] => {
            for entry in &entries[start..end] {
                out.attributed.push(Attributed { class, entry });
            }
        }
        many => {
            log::debug!(
                "jni registry: {} entr{} at {:#x} stayed {}; leaving them unattributed",
                end - start,
                if end - start == 1 { "y" } else { "ies" },
                entries[start].table_addr,
                if many.is_empty() {
                    "unmatched by any declared native".to_string()
                } else {
                    format!("ambiguous across {} classes", many.len())
                },
            );
            out.unattributed += end - start;
        }
    }
}

/// Splits a method descriptor's parameter list into individual type descriptors:
/// `(Ljava/lang/String;[IJ)V` yields `["Ljava/lang/String;", "[I", "J"]`. Returns `None` if the
/// descriptor is malformed.
///
/// The scan stops at the `)` that terminates the parameter list rather than at the first `)` in the
/// string, because the JVM's unqualified-name grammar forbids only `.;[/` inside a class name.
pub fn descriptor_params(descriptor: &str) -> Option<Vec<&str>> {
    let inner = descriptor.strip_prefix('(')?;
    let bytes = inner.as_bytes();
    let mut params = Vec::new();
    let mut i = 0;
    loop {
        if *bytes.get(i)? == b')' {
            return Some(params);
        }
        let start = i;
        while *bytes.get(i)? == b'[' {
            i += 1;
        }
        match *bytes.get(i)? {
            b'L' => {
                i += 1;
                // A multi-byte UTF-8 sequence never contains a byte equal to `;`, so this both
                // terminates correctly and leaves `i` on a char boundary.
                while *bytes.get(i)? != b';' {
                    i += 1;
                }
                i += 1;
            }
            b'Z' | b'B' | b'C' | b'S' | b'I' | b'J' | b'F' | b'D' => i += 1,
            _ => return None,
        }
        params.push(&inner[start..i]);
    }
}

#[cfg(test)]
mod tests;
