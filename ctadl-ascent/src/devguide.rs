/*! Development guide.

# Overview

- [ctadl_ir] - crate that defines CTADL IR, visitors
- [ctadl_flowy] - crate for the flowy testing & demo language
- [crate::facts] - data types representing various facts
- [crate::facts::schema] - schema & datatypes for CTADL's parquet files
- [crate::models] - JSON model generators and Datalog models

CTADL's overall flow is as follows:

1. [`crate::cli::import`] translates a frontend language into CTADL IR [`ctadl_ir::ProgramInfo`].
   This includes translating data flow statements, calls, constants, standard control flow,
   exceptions. It must produce a well defined IR. The IR is stored into the CTADL store (see
   [`crate::project`].
2. [`crate::cli::index`] loads some number of imported programs and indexes them using Datalog. The
   process of turning the programs into indexable-facts is called code generation (codegen) because
   it's quite like what a compiler does. It turns the programs into SSA form, applies relevant
   models, and generates raw [`crate::index_engine::IndexFacts`].
3. [`crate::cli::query`] runs a taint analysis query. It loads parts of program info and the index.
4. [`crate::cli::format`] loads query results and formats them in some human- or machine-consumable
   format, SARIF for example. Formatting is done in the [`crate::query_engine::formatter`] module.

The primary storage format is parquet. This is used for most facts. The IR is more structured so we
store it in a binary format.

# Facts

- [crate::facts::parquet] - encoding/decoding facts with parquet format

# Important Modules

- [crate::project] - manages how CTADL stores all its information
- [crate::languages::dex] - DEX language frontend
- [crate::languages::jvm] - JVM language frontend
- [crate::languages::tree_sitter] - C language frontend
- [crate::codegen] - Frontend language to Datalog code generation
- [crate::models] - Handles loading models for index/query
- [crate::index_engine] - datalog, datatypes for core CTADL index phase
- [crate::query_engine] - datalog, datatypes for core CTADL taint analysis phase
- [crate::query_engine::formatter] - datalog, takes taint results and associates them with instructions

We don't maintain the dex-parser or jvm-parser crates. They're vendored.

# Users

Users may be interested in writing their own Datalog to customize the algorithm. Datalog is run
with [`ascent::ascent`] or [`ascent::ascent_run`].

- To add models, users can use [`crate::codegen::models`].

# Code Development

- All code is native rust; no external dependencies. This means we can vendor-dependencies-and-build basically anywhere we care about.
- The code is research-grade. We don't really strive for backward compatibility. We do strive for well-defined problems that deal with practical messiness gracefully.
- Strive for good error messages for users.
- We designed CTADL with the following types of change in mind:
  1. Adding a new language. You can parse anything, write rust code, as long as you eventually
     produce our IR. Our data-flow-analysis specific optimizations are implemented for the IR, so
     you generally don't need to worry about those.
  2. Modeling language constructs in arbitrary ways

*/
