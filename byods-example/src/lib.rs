use lmdb::{Database, DatabaseFlags, Environment};
use std::path::Path;
use std::sync::Arc;

pub mod btreerel;
pub mod lmdbrel;
pub mod util;

#[macro_use]
extern crate lazy_static;

static DB_PATH: &str = "ctadl_lmdb";

pub struct LmdbWrapper {
    env: Environment,
    db: Database,
}

lazy_static! {
    pub static ref LMDB_ROOT: Arc<LmdbWrapper> = {
        let path = Path::new(DB_PATH);

        if path.exists() {
            std::fs::remove_dir_all(path).expect("failed to remove DB_PATH");
        }
        std::fs::create_dir_all(path).expect("failed to create DB_PATH");

        let env = Environment::new()
            .set_map_size(1024 * 1024 * 1024) // 1 GiB maximum map size
            .set_max_dbs(1) // Allow up to 1 database
            // .set_flags(EnvironmentFlags::NO_TLS)
            .open(path).expect("failed to create LMDB environment");

        let db = env.create_db(None, DatabaseFlags::DUP_SORT).expect("failed to create LMDB database");

        Arc::new(
        LmdbWrapper {
            env,
            db,
        })
    };
}
