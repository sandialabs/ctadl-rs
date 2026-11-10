//! The facts schema, the taint index engine and the engine-free parts of a staged index run:
//! the part of CTADL that `codegen` produces facts for. `ctadl-ascent` re-exports these modules
//! under their own names.

pub mod error;
pub mod facts;
pub mod index_engine;
pub mod staged;
