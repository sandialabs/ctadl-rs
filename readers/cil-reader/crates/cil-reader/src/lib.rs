//! # cil-reader
//!
//! A read-only CIL (ECMA-335) metadata and bytecode reader, modeled on
//! Mono.Cecil's reading API.
//!
//! - [`raw`] — zero-allocation access to the PE image, metadata streams,
//!   heaps and tables. Raw rows hold unresolved indices.
//! - [`model`], re-exported at the crate root — a Cecil-shaped lazy object
//!   model: module → types → members → method bodies → instructions.
//! - [`cil`] — the opcode table and instruction decoding.
//! - [`flow`] — per-instruction dataflow, and a method's CFG and stack
//!   heights.

pub mod cil;
pub mod error;
pub mod flow;
pub mod model;
pub mod raw;
pub mod token;

pub use error::{Error, Result};
pub use model::{Entity, Module};
pub use token::{MetadataToken, TokenType};
