//! The facts schema and the taint index engine: the part of CTADL that `codegen` produces facts
//! for and that the query engine reads. `ctadl-ascent` re-exports these modules under their
//! own names.

pub mod error;
pub mod facts;
pub mod index_engine;
