//! Layer 0: raw, zero-allocation access to the PE image, metadata streams,
//! heaps, and tables. Nothing here resolves names or builds an object graph;
//! see the crate root for the Cecil-shaped model layer.

pub mod coded;
pub(crate) mod cursor;
pub mod heaps;
pub mod metadata;
pub mod pe;
pub mod tables;

pub use coded::CodedIndexKind;
pub use cursor::Cur;
pub use heaps::{BlobIndex, Guid, GuidIndex, StringIndex, Utf16Str};
pub use metadata::Metadata;
pub use pe::{
    DataDirectory, DebugHeaderEntry, ModuleKind, PeImage, Section, TargetArchitecture,
    TargetRuntime, parse_runtime,
};
pub use tables::{RawRow, Table, TableInfo, TableRef};
