pub mod jni;

/// The JVM front end. It is its own crate, and it depends on `jvm-reader` and nothing else: no
/// datalog engine and no tree-sitter. Re-exported here so the name `crate::languages::jvm::…`
/// works.
pub use ctadl_jvm as jvm;

/// The Dex front end. It is its own crate, and it depends on `dex-reader` and nothing else.
/// Re-exported here so the name `crate::languages::dex::…` works.
pub use ctadl_dex as dex;

/// The pcode front end. It is its own crate, and it also holds the `RegisterNatives` scanner,
/// `ctadl_pcode::jni_registry`, because the two need each other. That module explains why they
/// ship together. Re-exported here so the name `crate::languages::pcode::…` works.
pub use ctadl_pcode as pcode;

/// The Lua front end. It is its own crate, and it depends on tree-sitter and the Lua grammar
/// only, not on the C grammar. Re-exported here so the name `crate::languages::lua::…`
/// works.
pub use ctadl_lua as lua;

/// The C front end, which is its own crate. Its tests are unlike the other front ends'. They
/// test the engine as much as the front end, so `ctadl-c` lists `ctadl-ascent` as a
/// dev-dependency. That makes a cycle, which Cargo allows for dev-dependencies, and it keeps
/// 5,000 lines of tests next to the code they test. Re-exported here so the name
/// `crate::languages::tree_sitter_c::…` works.
pub use ctadl_c as tree_sitter_c;
