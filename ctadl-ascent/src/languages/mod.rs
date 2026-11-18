pub mod android_intent;
pub mod android_manifest;
pub mod jni;

// Each front end is its own crate, re-exported here so `crate::languages::<name>::…` paths work.

/// The JVM front end.
pub use ctadl_jvm as jvm;

/// The Dex front end.
pub use ctadl_dex as dex;

/// The pcode front end, which also holds the `RegisterNatives` scanner.
pub use ctadl_pcode as pcode;

/// The Lua front end.
pub use ctadl_lua as lua;

/// The .NET (CIL) front end.
pub use ctadl_cil as cil;

/// The C front end.
pub use ctadl_c as tree_sitter_c;
