//! What the tests in this binary share.

use std::path::Path;
use std::sync::OnceLock;

use ctadl_ascent::project::init_store_path;
use tempfile::TempDir;

/// The store every test in this binary uses, set up on first call.
///
/// `init_store_path` sets a process-global root and fails if called twice, so all the tests in
/// the binary share one store. They keep out of each other's way by giving their imports and
/// projects distinct names. The directory is never deleted: a static is not dropped at exit.
pub fn store_dir() -> &'static Path {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().expect("temp store");
        init_store_path(Some(dir.path())).expect("init store");
        dir
    })
    .path()
}
