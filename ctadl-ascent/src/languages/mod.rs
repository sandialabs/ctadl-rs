pub mod apk_native;
pub mod dex;
pub mod flowy;
pub mod jni;
pub mod lua;
pub mod pcode;
pub mod tree_sitter_c;
pub mod xapk;

/// The JVM front end, extracted to its own crate: `jvm-reader` and nothing else, with no
/// datalog engine and no tree-sitter behind it. Re-exported here so `crate::languages::jvm::…`
/// still names it.
pub use ctadl_jvm as jvm;
