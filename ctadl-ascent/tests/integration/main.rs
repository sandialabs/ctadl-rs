//! The integration tests, as one binary: each file under `tests/` would otherwise be its own
//! binary, and every one of them links all of `ctadl-ascent` and its dependencies.

mod common;

mod absorbing_functions;
mod bridging_end_to_end;
mod bridging_models;
mod class_predicates;
mod cli;
mod default_models;
mod determinism;
mod dispatch_models;
mod flowy_tests;
mod jni_summary;
mod json_error_handling;
mod lua_model_externals;
mod migration_equivalence;
mod model_check;
mod models_loading;
mod multi_import_sarif;
mod port_semantics;
mod sarif_uris;
mod store_relocation;
mod xapk_bundle;
