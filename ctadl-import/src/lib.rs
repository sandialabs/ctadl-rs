/*! Reads and writes the CTADL store, without any front end.

This crate holds everything needed to read an import that `ctadl import` has already written,
and nothing else: the store layout ([`project`]), the shared error type ([`error`]), and the
`ProgramInfo` reading and writing that goes with them ([`store`]).

Most callers want [`open_import`]. Give it the name of an import in the store, or a path to an
import directory, and it returns preprocessed IR, checking the format version along the way.

# The error rule

The workspace has two `Error` types and two [`ErrorContext`] traits, one pair here and one in
`ctadl-ascent`. They stay out of each other's way because **a file imports one pair or the
other, never both**:

- a file that reads artifacts or the store imports the ones in [`crate::error`];
- a file that runs the engine imports the ones in `ctadl_ascent::error`;
- `ctadl_ascent::Error::Import` is declared `#[from] ctadl_import::Error`, so a `?` that crosses
  from one half to the other converts the error by itself.

This is worth writing down because the error message does not point at the cause. If one file
imports both traits, every `.err_context(…)` call in it becomes ambiguous. The fix is always to
remove one of the two imports, not to spell out the call.
*/

pub mod error;
pub mod project;
pub mod store;

pub use error::{Error, ErrorContext};
pub use project::{
    ArtifactImport, ArtifactLanguage, IMPORT_FORMAT_VERSION, INDEX_FORMAT_VERSION, StorePaths,
};
pub use store::{SourceInfoMode, load_import, open_import, save_program_info};
