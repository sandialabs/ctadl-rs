pub mod apk_native;
pub mod flowy;
pub mod jni;
pub mod tree_sitter_c;
pub mod xapk;

/// The JVM front end, extracted to its own crate: `jvm-reader` and nothing else, with no
/// datalog engine and no tree-sitter behind it. Re-exported here so `crate::languages::jvm::…`
/// still names it.
pub use ctadl_jvm as jvm;

/// The Dex front end, extracted to its own crate: `dex-reader` and nothing else. Re-exported
/// here so `crate::languages::dex::…` still names it.
pub use ctadl_dex as dex;

/// The pcode front end, extracted to its own crate along with the `RegisterNatives` scanner it
/// is one cycle with (`ctadl_pcode::jni_registry`; see that module for why they ship together).
/// Re-exported here so `crate::languages::pcode::…` still names it.
pub use ctadl_pcode as pcode;

/// The Lua front end, extracted to its own crate: tree-sitter and its Lua grammar, and no C
/// grammar behind them. Re-exported here so `crate::languages::lua::…` still names it.
pub use ctadl_lua as lua;
