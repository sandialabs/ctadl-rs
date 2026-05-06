//! CTADL using the Ascent datalog engine.

pub mod cli;
pub mod codegen;
pub mod devguide;
pub mod error;
pub mod facts;
pub mod index_engine;
pub mod languages;
pub mod models;
pub mod project;
pub mod query_engine;
pub mod stats;

/// Initializes the logger
pub fn init() {
    env_logger::init();
}
