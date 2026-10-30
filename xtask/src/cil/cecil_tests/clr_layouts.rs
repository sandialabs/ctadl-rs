//! Tables-stream layouts that the CLR reads and Cecil misreads, so the
//! comparison with Cecil does not cover them. Each module in the fixtures'
//! `clr` directory is the module of `gap-enc.dll` written in another layout,
//! and must dump as that one does, but for its name and MVID.

use super::{Test, fixtures_root};
use crate::cil::parity::rust_dump;
use cil_reader::Module;
use cil_reader::raw::tables::Table;

/// The dump of `path`, with the module's name and MVID taken out.
fn dump(path: &str) -> String {
    let path = fixtures_root().join(path);
    let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
    let dump = rust_dump(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    dump.lines()
        .filter(|l| !l.starts_with("mvid="))
        .map(|l| l.replace(&stem, "MODULE") + "\n")
        .collect()
}

fn same_as_enc(path: &str) {
    assert_eq!(dump(path), dump("gap-enc.dll"), "{path}");
}

fn module_row_size(path: &str) -> (u8, u32) {
    let data = std::fs::read(fixtures_root().join(path)).unwrap();
    let module = Module::read(&data).unwrap();
    let md = module.metadata();
    (md.heap_sizes, md.table_info(Table::Module).row_size)
}

// HeapSizes' ExtraData bit (0x40): a u32 follows the row counts.
fn extra_data() {
    same_as_enc("clr/gap-extra-data.dll");
    // Generation, then 2-byte Name, Mvid, EncId and EncBaseId indexes.
    assert_eq!(module_row_size("clr/gap-extra-data.dll"), (0x40, 10));
}

// A #JTD stream with #- tables makes every index 4 bytes, though HeapSizes
// and the row counts call for 2. The CLR finds #JTD whatever its case.
fn minimal_delta() {
    for path in [
        "clr/gap-minimal-delta.dll",
        "clr/gap-minimal-delta-case.dll",
    ] {
        same_as_enc(path);
        assert_eq!(module_row_size(path), (0, 18), "{path}");
    }
    // With #~ tables, #JTD changes nothing.
    assert_eq!(module_row_size("gap-jtd-compressed.dll"), (0, 10));
}

pub(super) const TESTS: &[Test] = &[("extra_data", extra_data), ("minimal_delta", minimal_delta)];
