//! Tests for `RegisterNatives` table recovery and class attribution.
//!
//! The two halves fail in different ways, so they are tested apart. Attribution carries the real
//! risk -- a merged run fabricates a cross-class link, which is worse than a miss -- and it needs
//! no ELF at all, so it is driven from synthetic `(table_addr, name, descriptor)` lists. Parsing
//! is exercised against ELF byte buffers built here, one per path the scan has to get right.

use super::*;

// ---------------------------------------------------------------------------
// Attribution
// ---------------------------------------------------------------------------

/// A contiguous table starting at `base`, at the ELF64 entry size. Callers concatenate several to
/// lay tables out adjacent, or leave a hole to make a gap.
fn table(
    base: u64,
    rows: &[(&'static str, &'static str)],
) -> Vec<(u64, &'static str, &'static str)> {
    rows.iter()
        .enumerate()
        .map(|(i, (name, descriptor))| (base + 24 * i as u64, *name, *descriptor))
        .collect()
}

/// A registry over those triples. Every entry gets a function, since attribution does not read
/// one; what it does with a `None` is a matter for `link`.
fn registry(entry_size: u64, rows: &[(u64, &'static str, &'static str)]) -> JniRegistry {
    JniRegistry {
        entry_size,
        entries: rows
            .iter()
            .map(|(table_addr, name, descriptor)| RegistryEntry {
                table_addr: *table_addr,
                fn_addr: 0x1000 + table_addr,
                name: name.to_string(),
                descriptor: descriptor.to_string(),
                function: Some(format!("fn_{name}")),
                veneer_target: None,
            })
            .collect(),
    }
}

/// Attributes `registry` against the `(class, name, descriptor)` natives, returning the table
/// count, the unattributed count, and the `(class, name)` pairs in entry order.
fn attribute_against<'a>(
    registry: &'a JniRegistry,
    natives: &[(&'a str, &'a str, &'a str)],
) -> (usize, usize, Vec<(&'a str, String)>) {
    let index = ClassIndex::build(natives.iter().copied());
    let report = attribute(registry, &index);
    let pairs = report
        .attributed
        .iter()
        .map(|hit| (hit.class, hit.entry.name.clone()))
        .collect();
    (report.tables, report.unattributed, pairs)
}

/// Sugar for the expected `(class, name)` list.
fn pairs(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(c, n)| (c.to_string(), n.to_string()))
        .collect()
}

/// The same, over what `attribute_against` returned.
fn owned(rows: &[(&str, String)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(c, n)| (c.to_string(), n.clone()))
        .collect()
}

/// The Facebook Lite shape: three tables laid out adjacent with no gap between them, each run a
/// *subset* of what its class declares (`SuperpackArchive` declares 14 natives and registers 13).
///
/// Splitting on address gaps alone yields two runs here, not three -- guards 2 and 3 are what
/// recover the third boundary. Asserting **3** is the direct regression for both.
#[test]
fn three_adjacent_tables_segment_into_three_runs() {
    let mut rows = table(
        0x2a000,
        &[("open", "()V"), ("read", "(I)I"), ("close", "()V")],
    );
    // The second table begins exactly where the first ends, and its leading `open` is declared by
    // the first table's class too -- the overlap that lets a greedy walk swallow the boundary.
    rows.extend(table(
        0x2a000 + 24 * 3,
        &[("open", "()V"), ("write", "([B)V")],
    ));
    rows.extend(table(0x2a000 + 24 * 5, &[("decompress", "(J)J")]));
    let registry = registry(24, &rows);

    let (tables, unattributed, attributed) = attribute_against(
        &registry,
        &[
            ("Lcom/x/Archive;", "open", "()V"),
            ("Lcom/x/Archive;", "read", "(I)I"),
            ("Lcom/x/Archive;", "close", "()V"),
            // Declared but never registered: matching is by containment, never by count.
            ("Lcom/x/Archive;", "extra", "()V"),
            ("Lcom/x/File;", "open", "()V"),
            ("Lcom/x/File;", "write", "([B)V"),
            ("Lcom/x/Decompressor;", "decompress", "(J)J"),
        ],
    );

    assert_eq!(tables, 3, "{attributed:?}");
    assert_eq!(unattributed, 0);
    assert_eq!(
        owned(&attributed),
        pairs(&[
            ("Lcom/x/Archive;", "open"),
            ("Lcom/x/Archive;", "read"),
            ("Lcom/x/Archive;", "close"),
            ("Lcom/x/File;", "open"),
            ("Lcom/x/File;", "write"),
            ("Lcom/x/Decompressor;", "decompress"),
        ])
    );
}

/// Guard 2 alone: two adjacent tables whose classes share a *leading* method. A repeated
/// `(name, descriptor)` is proof of a boundary, because a `JNINativeMethod[]` cannot register the
/// same method twice.
#[test]
fn a_repeated_signature_splits_the_run() {
    let mut rows = table(0x1000, &[("init", "()V"), ("a", "()V")]);
    rows.extend(table(
        0x1000 + 48,
        &[("init", "()V"), ("b", "()V"), ("c", "()V")],
    ));
    let registry = registry(24, &rows);

    let (tables, unattributed, attributed) = attribute_against(
        &registry,
        &[
            ("LA;", "init", "()V"),
            ("LA;", "a", "()V"),
            // `A` declares `b` too, so nothing but the repeated `init` can close the first run:
            // without guard 2 the walk swallows the boundary and gives `B`'s entries to `A`.
            ("LA;", "b", "()V"),
            ("LB;", "init", "()V"),
            ("LB;", "b", "()V"),
            ("LB;", "c", "()V"),
        ],
    );
    assert_eq!(tables, 2);
    assert_eq!(unattributed, 0);
    assert_eq!(
        owned(&attributed),
        pairs(&[
            ("LA;", "init"),
            ("LA;", "a"),
            ("LB;", "init"),
            ("LB;", "b"),
            ("LB;", "c"),
        ])
    );
}

/// Guard 3, asserted where it is a statement about the rule rather than about a fixture: a class
/// that declares fewer natives than a run is long cannot be that run's class, however well it
/// matches.
///
/// It cannot be reached through [`attribute`] on a well-formed Java side -- a class that declares
/// every *distinct* entry of a run declares at least as many natives as the run is long, and
/// guard 2 is what guarantees the entries are distinct. That is why it is tested directly: it is
/// a backstop for the case where that assumption about someone else's input does not hold.
#[test]
fn a_class_that_declares_too_few_natives_is_dropped_from_a_run() {
    let index = ClassIndex::build(
        [
            ("LA;", "a", "()V"),
            ("LB;", "a", "()V"),
            ("LB;", "b", "()V"),
        ]
        .into_iter(),
    );
    // Both classes declare the entry, but only `B` declares enough natives for a run of two.
    assert_eq!(narrow(&["LA;", "LB;"], &["LA;", "LB;"], &index, 2), ["LB;"]);
    assert_eq!(
        narrow(&["LA;", "LB;"], &["LA;", "LB;"], &index, 1),
        ["LA;", "LB;"]
    );
}

/// Guard 1: a hole in the addresses is a table boundary with certainty. Here one class declares
/// both entries, so nothing else could separate them.
#[test]
fn an_address_gap_splits_the_run() {
    let mut rows = table(0x1000, &[("a", "()V")]);
    rows.extend(table(0x1000 + 24 * 3, &[("b", "()V")]));
    let registry = registry(24, &rows);

    let (tables, unattributed, attributed) =
        attribute_against(&registry, &[("LA;", "a", "()V"), ("LA;", "b", "()V")]);
    assert_eq!(tables, 2);
    assert_eq!(unattributed, 0);
    assert_eq!(owned(&attributed), pairs(&[("LA;", "a"), ("LA;", "b")]));
}

/// A run that stays multi-class to the end attributes nothing. Guessing between the candidates is
/// what the measured-and-cut tier 2 would have done.
#[test]
fn a_run_that_stays_ambiguous_is_left_unattributed() {
    let registry = registry(24, &table(0x1000, &[("a", "()V"), ("b", "()V")]));
    let (tables, unattributed, attributed) = attribute_against(
        &registry,
        &[
            ("LA;", "a", "()V"),
            ("LA;", "b", "()V"),
            ("LB;", "a", "()V"),
            ("LB;", "b", "()V"),
        ],
    );
    assert_eq!(tables, 1);
    assert_eq!(unattributed, 2);
    assert!(attributed.is_empty());
}

/// The VLC shape: well-formed tables whose Java classes ship outside `classes.dex` -- libbluray's
/// BD-J bindings, a feature-split dex. Nothing matches, so each entry closes its own run, nothing
/// is misattributed, and no link is emitted. Attributing nothing is the right answer.
#[test]
fn a_library_whose_java_half_is_absent_attributes_nothing() {
    let registry = registry(
        24,
        &table(
            0x1000,
            &[
                ("getTitleInfosN", "(J)[Lorg/videolan/TitleInfo;"),
                ("getBdjoN", "(J)Lorg/videolan/bdjo/Bdjo;"),
            ],
        ),
    );
    let (_, unattributed, attributed) =
        attribute_against(&registry, &[("Lcom/example/Unrelated;", "other", "()V")]);
    assert!(attributed.is_empty());
    assert_eq!(unattributed, 2);
}

/// Overloads are what the symbol convention cannot resolve and what a registration names exactly:
/// same simple name, different descriptors, both attributed to the same class in one run.
#[test]
fn overloads_are_distinguished_by_descriptor() {
    let registry = registry(24, &table(0x1000, &[("f", "(I)V"), ("f", "(J)V")]));
    let (tables, unattributed, attributed) =
        attribute_against(&registry, &[("LA;", "f", "(I)V"), ("LA;", "f", "(J)V")]);
    assert_eq!(tables, 1);
    assert_eq!(unattributed, 0);
    assert_eq!(owned(&attributed), pairs(&[("LA;", "f"), ("LA;", "f")]));
}

// ---------------------------------------------------------------------------
// Name and descriptor predicates
// ---------------------------------------------------------------------------

#[test]
fn method_names_admit_identifiers_and_the_two_constructors() {
    assert!(is_java_method_name("readBytesNative"));
    assert!(is_java_method_name("_x$1"));
    assert!(is_java_method_name("<init>"));
    assert!(is_java_method_name("<clinit>"));

    assert!(!is_java_method_name(""));
    assert!(!is_java_method_name("1st"));
    // The string most likely to sit next to a name in `.rodata` is a signature, and rejecting it
    // is what makes the "advance by one pointer" scan reject misaligned triples.
    assert!(!is_java_method_name("(I)V"));
    assert!(!is_java_method_name("Ljava/lang/String;"));
    assert!(!is_java_method_name("<other>"));
}

/// The return type is checked too: `descriptor_params` stops at the `)` and never looks past it,
/// so without this `"(I)garbage"` would pass.
#[test]
fn descriptors_are_validated_through_the_return_type() {
    assert!(is_method_descriptor("()V"));
    assert!(is_method_descriptor("(JII[BI)V"));
    assert!(is_method_descriptor(
        "(Ljava/lang/String;)Ljava/lang/String;"
    ));
    assert!(is_method_descriptor("()[[I"));

    assert!(!is_method_descriptor("(I)garbage"));
    assert!(!is_method_descriptor("(I)"));
    assert!(!is_method_descriptor("(I)VV"));
    assert!(!is_method_descriptor("(Q)V"));
    assert!(!is_method_descriptor("not a descriptor"));
    assert!(!is_method_descriptor(""));
}

// ---------------------------------------------------------------------------
// ELF scanning
// ---------------------------------------------------------------------------

/// Bad magic, a truncated header and a zero-length file are three separate quiet returns. All
/// three ship under `lib/<abi>/*.so` in the reference corpus -- 17 of its 370 libraries are not
/// ELF at all.
#[test]
fn a_non_elf_file_is_a_quiet_no_op() {
    assert!(scan_bytes(b"").is_none());
    // A ZIP of dex, shipped as `libdex_df_*.so`.
    assert!(scan_bytes(b"PK\x03\x04\x14\x00\x08\x00\x08\x00\x00\x00\x00\x00\x00\x00").is_none());
    // Custom packed containers.
    assert!(scan_bytes(b"\x7fKOM\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00").is_none());
    assert!(scan_bytes(b"SKCL\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00").is_none());
    // Right magic, nothing behind it.
    assert!(scan_bytes(b"\x7fELF\x02\x01\x01").is_none());
    // Right magic and a full `e_ident`, but no header.
    assert!(scan_bytes(b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00").is_none());
}

/// The standard path: `.data.rel.ro` holds zeros and a `SHT_RELA` section carries the real
/// addresses as `R_AARCH64_RELATIVE` addends. Every slot of `libsuperpack-jni.so` resolves this
/// way.
#[test]
fn elf64_resolves_slots_through_relative_relocations() {
    let scan = scan_bytes(&Elf64::new().relocated().build()).expect("scanning");
    assert_eq!(scan.entry_size, 24);
    assert_eq!(scan.load_bias, 0);
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].name, "readBytesNative");
    assert_eq!(scan.entries[0].descriptor, "(JII[BI)V");
    assert_eq!(scan.entries[0].table_addr, DATA_ADDR);
    assert_eq!(scan.entries[0].fn_addr, TEXT_ADDR);
}

/// The `.relr.dyn` path, which needs no decoder at all: those formats leave the value in place,
/// so the in-place read covers them. Nine of one app's eleven libraries are like this.
#[test]
fn elf64_falls_back_to_the_value_stored_in_the_file() {
    let scan = scan_bytes(&Elf64::new().build()).expect("scanning");
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].name, "readBytesNative");
    assert_eq!(scan.entries[0].fn_addr, TEXT_ADDR);
}

/// A `.rela.dyn` typed `SHT_ANDROID_RELA` (`0x60000002`) holds a packed APS2 blob, not an array
/// of `Elf64_Rela`. Decoding it as one yields garbage addends -- and plausible-looking ones,
/// which is worse -- so selection is by `sh_type`, never by section name.
///
/// The two halves of this test share one fixture and differ only in that number, which is what
/// makes it a test of the gate rather than of the fixture: with `SHT_RELA` the (deliberately
/// wrong) addends are read and the candidate is rejected; with the Android type they are ignored
/// and the in-place value stands.
#[test]
fn a_relocation_section_is_selected_by_type_not_by_name() {
    let honored = Elf64::new()
        .with_misleading_relocs(object::elf::SHT_RELA)
        .build();
    assert!(
        scan_bytes(&honored).expect("scanning").entries.is_empty(),
        "a SHT_RELA section must be read, so these addends must break the candidate"
    );

    let ignored = Elf64::new()
        .with_misleading_relocs(SHT_ANDROID_RELA)
        .build();
    let scan = scan_bytes(&ignored).expect("scanning");
    assert_eq!(scan.entries.len(), 1, "a packed section must be ignored");
    assert_eq!(scan.entries[0].name, "readBytesNative");
    assert_eq!(scan.entries[0].fn_addr, TEXT_ADDR);
}

/// A `fnPtr` that does not point into executable code is not a table entry, whatever the two
/// strings look like. Together with the method-name test this is where the real filtering
/// happens.
#[test]
fn a_function_pointer_outside_executable_code_is_rejected() {
    let elf = Elf64::new().with_fn_addr(DATA_ADDR).build();
    assert!(scan_bytes(&elf).expect("scanning").entries.is_empty());
}

/// 32-bit ARM: stride 12, and the Thumb bit set on the function pointer. 92% of the corpus's
/// 1269 32-bit ARM entries carry it, and unmasked they match no function entry point at all --
/// so without the mask, 92% of every armeabi-v7a app maps to nothing.
#[test]
fn elf32_masks_the_thumb_bit() {
    let scan = scan_bytes(&build_elf32()).expect("scanning");
    assert_eq!(scan.entry_size, 12);
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].name, "readBytesNative");
    // Stored as `TEXT_ADDR | 1`; recovered masked, which is what the function map is keyed by.
    assert_eq!(scan.entries[0].fn_addr, TEXT_ADDR);
    // Veneer following is AArch64 only. `.text` is zeroed here, but the guard is on the machine,
    // not on what the bytes happen to be: a 32-bit `fnPtr` may be Thumb or ARM and this decoder
    // reads neither. Measured on the corpus, the 32-bit libraries hold no veneers here anyway.
    assert_eq!(scan.entries[0].veneer_target, None);
}

// ---------------------------------------------------------------------------
// Branch veneers
// ---------------------------------------------------------------------------

/// The instruction decoder, against the bytes Messenger 563 actually ships: `0x17ff246e` at
/// `0x40e74` is the veneer for `writeNative`, and it branches back to `0xa02c`.
#[test]
fn an_aarch64_branch_decodes_to_its_target() {
    assert_eq!(aarch64_branch_target(0x17ff_246e, 0x40e74), Some(0xa02c));
    // Forward, and the two extremes of the immediate.
    assert_eq!(aarch64_branch_target(0x1400_0010, 0x1000), Some(0x1040));
    assert_eq!(aarch64_branch_target(0x1400_0000, 0x1000), Some(0x1000));
    assert_eq!(aarch64_branch_target(0x17ff_ffff, 0x1000), Some(0xffc));
}

/// Anything that is not a `B` is not a veneer. `BL` differs from `B` in exactly one bit, and
/// taking it would follow a call out of a real function body rather than through a stub.
#[test]
fn only_a_plain_branch_is_followed() {
    assert_eq!(aarch64_branch_target(0x9400_0010, 0x1000), None); // BL
    assert_eq!(aarch64_branch_target(0xd65f_03c0, 0x1000), None); // RET
    assert_eq!(aarch64_branch_target(0xa9bf_7bfd, 0x1000), None); // STP -- a real prologue
    assert_eq!(aarch64_branch_target(0x0000_0000, 0x1000), None); // padding
}

/// The Messenger shape end to end: the `fnPtr` points at a stub holding one `B`, so the scan
/// records where it goes. `fn_addr` stays the address in the table -- that is the pointer
/// `RegisterNatives` receives, and changing it would break a spot-check against the ELF.
#[test]
fn a_function_pointer_at_a_veneer_records_its_target() {
    let scan =
        scan_bytes(&Elf64::new().with_veneer_to(TEXT_ADDR + 0x40).build()).expect("scanning");
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].fn_addr, TEXT_ADDR);
    assert_eq!(scan.entries[0].veneer_target, Some(TEXT_ADDR + 0x40));
}

/// A pointer into an ordinary function body records nothing, so resolution goes on using the
/// pointer itself. This is the Facebook Lite shape, and it is also every entry in a library
/// linked without veneers.
#[test]
fn a_function_pointer_at_ordinary_code_records_no_veneer() {
    // `stp x29, x30, [sp, #-16]!` -- the prologue at the head of a real function.
    let scan = scan_bytes(&Elf64::new().with_code(0xa9bf_7bfd).build()).expect("scanning");
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].veneer_target, None);
}

/// A branch leaving executable code is not a veneer to anything, whatever it decodes to. Without
/// this the row would carry a target that no function can ever sit at.
#[test]
fn a_branch_out_of_executable_code_is_not_a_veneer() {
    let scan = scan_bytes(&Elf64::new().with_veneer_to(RODATA_ADDR).build()).expect("scanning");
    assert_eq!(scan.entries.len(), 1);
    assert_eq!(scan.entries[0].veneer_target, None);
}

// ---------------------------------------------------------------------------
// Minimal ELF fixtures
// ---------------------------------------------------------------------------

/// Android's packed `.rela.dyn` type. Not in `object::elf`, which is the point: a build that
/// selected relocation sections by name would decode this as `Elf_Rela`.
const SHT_ANDROID_RELA: u32 = 0x6000_0002;

// One layout, shared by both fixtures: three sections at fixed addresses and file offsets, far
// enough apart that nothing overlaps.
const TEXT_ADDR: u64 = 0x1000;
const RODATA_ADDR: u64 = 0x2000;
const DATA_ADDR: u64 = 0x3000;
const TEXT_OFF: u64 = 0x1000;
const RODATA_OFF: u64 = 0x2000;
const DATA_OFF: u64 = 0x3000;
const RELA_OFF: u64 = 0x4000;
const SHSTR_OFF: u64 = 0x5000;
const SHOFF: u64 = 0x6000;
const FILE_SIZE: usize = 0x8000;

/// The name and descriptor every fixture registers, laid out back to back in `.rodata`.
const FIXTURE_NAME: &[u8] = b"readBytesNative\0";
const FIXTURE_DESCRIPTOR: &[u8] = b"(JII[BI)V\0";
const DESCRIPTOR_OFF: u64 = FIXTURE_NAME.len() as u64;

/// `\0.text\0.rodata\0.data.rel.ro\0.rela.dyn\0.shstrtab\0`.
fn shstrtab() -> Vec<u8> {
    let mut out = vec![0u8];
    for name in [".text", ".rodata", ".data.rel.ro", ".rela.dyn", ".shstrtab"] {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
    }
    out
}

/// Offset of `name` inside [`shstrtab`].
fn shstr_off(name: &str) -> u32 {
    let table = shstrtab();
    let text = String::from_utf8(table).expect("ascii");
    (text.find(&format!("\0{name}\0")).expect("section name") + 1) as u32
}

/// A byte-level ELF64 shared library: one executable `.text`, a `.rodata` holding the two
/// strings, and a `.data.rel.ro` holding a single `JNINativeMethod`.
struct Elf64 {
    /// Write the three slot values into `.data.rel.ro` (the in-place path). Off for the
    /// relocated path, which leaves them zero.
    in_place: bool,
    /// `sh_type` of the relocation section, and whether its addends are the right ones.
    relocs: Option<(u32, bool)>,
    fn_addr: u64,
    /// The instruction word at [`TEXT_ADDR`], where `fn_addr` points by default. Zero -- padding,
    /// which decodes as no branch -- unless a test says otherwise.
    code: u32,
}

impl Elf64 {
    fn new() -> Self {
        Self {
            in_place: true,
            relocs: None,
            fn_addr: TEXT_ADDR,
            code: 0,
        }
    }

    /// Puts one AArch64 `B target` at [`TEXT_ADDR`], which is what a linker's veneer holds.
    fn with_veneer_to(mut self, target: u64) -> Self {
        let imm26 = (((target as i64 - TEXT_ADDR as i64) / 4) as u32) & 0x03ff_ffff;
        self.code = 0x1400_0000 | imm26;
        self
    }

    /// Puts an arbitrary instruction word there instead.
    fn with_code(mut self, word: u32) -> Self {
        self.code = word;
        self
    }

    /// The slots are zero in the file and a `SHT_RELA` section supplies the real addresses.
    fn relocated(mut self) -> Self {
        self.in_place = false;
        self.relocs = Some((object::elf::SHT_RELA, true));
        self
    }

    /// Keeps the in-place values and adds a relocation section of `sh_type` whose addends would
    /// point the name slot at the *descriptor* string. Reading them turns a valid candidate into
    /// one whose name is `(JII[BI)V`, which is not a method name -- so honouring or ignoring the
    /// section is directly observable.
    fn with_misleading_relocs(mut self, sh_type: u32) -> Self {
        self.relocs = Some((sh_type, false));
        self
    }

    fn with_fn_addr(mut self, addr: u64) -> Self {
        self.fn_addr = addr;
        self
    }

    fn build(self) -> Vec<u8> {
        let mut out = vec![0u8; FILE_SIZE];

        out[..4].copy_from_slice(&object::elf::ELFMAG);
        out[4] = object::elf::ELFCLASS64;
        out[5] = 1; // ELFDATA2LSB
        out[6] = 1; // EV_CURRENT
        put16(&mut out, 0x10, 3); // e_type = ET_DYN
        put16(&mut out, 0x12, object::elf::EM_AARCH64);
        put32(&mut out, 0x14, 1); // e_version
        put64(&mut out, 0x20, 0x40); // e_phoff
        put64(&mut out, 0x28, SHOFF); // e_shoff
        put16(&mut out, 0x34, 64); // e_ehsize
        put16(&mut out, 0x36, 56); // e_phentsize
        put16(&mut out, 0x38, 1); // e_phnum
        put16(&mut out, 0x3a, 64); // e_shentsize
        put16(&mut out, 0x3c, 6); // e_shnum
        put16(&mut out, 0x3e, 5); // e_shstrndx

        // One PT_LOAD at vaddr 0, as every Android shared library has.
        put32(&mut out, 0x40, object::elf::PT_LOAD);
        put32(&mut out, 0x44, 5); // p_flags = R|X
        put64(&mut out, 0x48, 0); // p_offset
        put64(&mut out, 0x50, 0); // p_vaddr

        put_strings(&mut out);
        put32(&mut out, TEXT_OFF as usize, self.code);
        if self.in_place {
            put64(&mut out, DATA_OFF as usize, RODATA_ADDR);
            put64(
                &mut out,
                (DATA_OFF + 8) as usize,
                RODATA_ADDR + DESCRIPTOR_OFF,
            );
            put64(&mut out, (DATA_OFF + 16) as usize, self.fn_addr);
        }

        if let Some((_, truthful)) = self.relocs {
            let name_addend = if truthful {
                RODATA_ADDR
            } else {
                RODATA_ADDR + DESCRIPTOR_OFF
            };
            let addends = [
                (DATA_ADDR, name_addend),
                (DATA_ADDR + 8, RODATA_ADDR + DESCRIPTOR_OFF),
                (DATA_ADDR + 16, self.fn_addr),
            ];
            for (i, (offset, addend)) in addends.iter().enumerate() {
                let at = RELA_OFF as usize + i * 24;
                put64(&mut out, at, *offset);
                put64(&mut out, at + 8, u64::from(object::elf::R_AARCH64_RELATIVE));
                put64(&mut out, at + 16, *addend);
            }
        }

        let names = shstrtab();
        out[SHSTR_OFF as usize..][..names.len()].copy_from_slice(&names);

        // Section headers; index 0 is the null section and stays zeroed.
        let mut sh = |i: usize, name: &str, ty: u32, flags: u64, addr: u64, off: u64, size: u64| {
            let at = SHOFF as usize + i * 64;
            put32(&mut out, at, shstr_off(name));
            put32(&mut out, at + 4, ty);
            put64(&mut out, at + 8, flags);
            put64(&mut out, at + 16, addr);
            put64(&mut out, at + 24, off);
            put64(&mut out, at + 32, size);
        };
        let alloc = u64::from(object::elf::SHF_ALLOC);
        let write = u64::from(object::elf::SHF_WRITE);
        let exec = u64::from(object::elf::SHF_EXECINSTR);
        let progbits = object::elf::SHT_PROGBITS;
        sh(
            1,
            ".text",
            progbits,
            alloc | exec,
            TEXT_ADDR,
            TEXT_OFF,
            0x100,
        );
        sh(
            2,
            ".rodata",
            progbits,
            alloc,
            RODATA_ADDR,
            RODATA_OFF,
            0x100,
        );
        sh(
            3,
            ".data.rel.ro",
            progbits,
            alloc | write,
            DATA_ADDR,
            DATA_OFF,
            24,
        );
        sh(
            4,
            ".rela.dyn",
            self.relocs.map_or(object::elf::SHT_NULL, |(ty, _)| ty),
            alloc,
            0,
            RELA_OFF,
            3 * 24,
        );
        sh(
            5,
            ".shstrtab",
            object::elf::SHT_STRTAB,
            0,
            0,
            SHSTR_OFF,
            names.len() as u64,
        );
        // `sh_entsize` on the relocation section, which `object` reads to size its array.
        put64(&mut out, SHOFF as usize + 4 * 64 + 56, 24);
        out
    }
}

/// The same library at 32-bit ARM: stride 12, values in place, Thumb bit set on `fnPtr`.
fn build_elf32() -> Vec<u8> {
    let mut out = vec![0u8; FILE_SIZE];

    out[..4].copy_from_slice(&object::elf::ELFMAG);
    out[4] = object::elf::ELFCLASS32;
    out[5] = 1;
    out[6] = 1;
    put16(&mut out, 0x10, 3); // ET_DYN
    put16(&mut out, 0x12, object::elf::EM_ARM);
    put32(&mut out, 0x14, 1);
    put32(&mut out, 0x1c, 0x34); // e_phoff
    put32(&mut out, 0x20, SHOFF as u32); // e_shoff
    put16(&mut out, 0x28, 52); // e_ehsize
    put16(&mut out, 0x2a, 32); // e_phentsize
    put16(&mut out, 0x2c, 1); // e_phnum
    put16(&mut out, 0x2e, 40); // e_shentsize
    put16(&mut out, 0x30, 6); // e_shnum
    put16(&mut out, 0x32, 5); // e_shstrndx

    put32(&mut out, 0x34, object::elf::PT_LOAD);
    put32(&mut out, 0x38, 0); // p_offset
    put32(&mut out, 0x3c, 0); // p_vaddr

    put_strings(&mut out);
    put32(&mut out, DATA_OFF as usize, RODATA_ADDR as u32);
    put32(
        &mut out,
        (DATA_OFF + 4) as usize,
        (RODATA_ADDR + DESCRIPTOR_OFF) as u32,
    );
    put32(&mut out, (DATA_OFF + 8) as usize, TEXT_ADDR as u32 | 1);

    let names = shstrtab();
    out[SHSTR_OFF as usize..][..names.len()].copy_from_slice(&names);

    let mut sh = |i: usize, name: &str, ty: u32, flags: u32, addr: u32, off: u32, size: u32| {
        let at = SHOFF as usize + i * 40;
        put32(&mut out, at, shstr_off(name));
        put32(&mut out, at + 4, ty);
        put32(&mut out, at + 8, flags);
        put32(&mut out, at + 12, addr);
        put32(&mut out, at + 16, off);
        put32(&mut out, at + 20, size);
    };
    let progbits = object::elf::SHT_PROGBITS;
    let (alloc, write, exec) = (
        object::elf::SHF_ALLOC,
        object::elf::SHF_WRITE,
        object::elf::SHF_EXECINSTR,
    );
    sh(
        1,
        ".text",
        progbits,
        alloc | exec,
        TEXT_ADDR as u32,
        TEXT_OFF as u32,
        0x100,
    );
    sh(
        2,
        ".rodata",
        progbits,
        alloc,
        RODATA_ADDR as u32,
        RODATA_OFF as u32,
        0x100,
    );
    sh(
        3,
        ".data.rel.ro",
        progbits,
        alloc | write,
        DATA_ADDR as u32,
        DATA_OFF as u32,
        12,
    );
    sh(4, ".rela.dyn", object::elf::SHT_NULL, 0, 0, 0, 0);
    sh(
        5,
        ".shstrtab",
        object::elf::SHT_STRTAB,
        0,
        0,
        SHSTR_OFF as u32,
        names.len() as u32,
    );
    out
}

/// Lays the name and descriptor into `.rodata`, which both fixtures do identically.
fn put_strings(out: &mut [u8]) {
    out[RODATA_OFF as usize..][..FIXTURE_NAME.len()].copy_from_slice(FIXTURE_NAME);
    out[(RODATA_OFF + DESCRIPTOR_OFF) as usize..][..FIXTURE_DESCRIPTOR.len()]
        .copy_from_slice(FIXTURE_DESCRIPTOR);
}

fn put16(out: &mut [u8], at: usize, v: u16) {
    out[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut [u8], at: usize, v: u32) {
    out[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put64(out: &mut [u8], at: usize, v: u64) {
    out[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
