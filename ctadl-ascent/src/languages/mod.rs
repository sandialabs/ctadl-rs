pub mod jni;

/// The JVM front end, a crate of its own: `jvm-reader` and nothing else, with no datalog engine
/// and no tree-sitter behind it. Re-exported here so `crate::languages::jvm::…` names it.
pub use ctadl_jvm as jvm;

/// The Dex front end, a crate of its own: `dex-reader` and nothing else. Re-exported here so
/// `crate::languages::dex::…` names it.
pub use ctadl_dex as dex;

/// The pcode front end, a crate of its own, holding the `RegisterNatives` scanner it is one
/// cycle with (`ctadl_pcode::jni_registry`; see that module for why they ship together).
/// Re-exported here so `crate::languages::pcode::…` names it.
pub use ctadl_pcode as pcode;

/// The Lua front end, a crate of its own: tree-sitter and its Lua grammar, and no C grammar
/// behind them. Re-exported here so `crate::languages::lua::…` names it.
pub use ctadl_lua as lua;

/// The C front end, a crate of its own. Its tests are the exception among the front ends: they
/// are the engine's regression suite as much as the front end's, so `ctadl-c` takes
/// `ctadl-ascent` as a *dev*-dependency -- a cycle Cargo permits -- rather than keeping 5,000
/// lines of tests apart from what they test. Re-exported here so
/// `crate::languages::tree_sitter_c::…` names it.
pub use ctadl_c as tree_sitter_c;
