pub mod jni;

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

/// The C front end, extracted to its own crate. Its tests are the exception in this split: they
/// are the engine's regression suite as much as the front end's, so `ctadl-c` takes
/// `ctadl-ascent` as a *dev*-dependency -- a cycle Cargo permits -- rather than relocating 5,000
/// lines. Re-exported here so `crate::languages::tree_sitter_c::…` still names it.
pub use ctadl_c as tree_sitter_c;
