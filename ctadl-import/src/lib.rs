/*! Reading and writing the CTADL store, with no front end attached.

This crate is everything a consumer needs in order to *read* an import that `ctadl import`
already wrote, and nothing more: the store layout ([`project`]), the shared error type
([`error`]), and the `ProgramInfo` I/O that pairs with them ([`store`]).

The entry point most consumers want is [`open_import`]: name-or-directory in the store to
preprocessed IR, version-checked on the way through.

# The error rule

There are two `Error` types in the workspace and two [`ErrorContext`]
traits, one pair here and one in `ctadl-ascent`. They never collide because **a file imports one
or the other, never both**:

- a file that reads artifacts or the store imports [`crate::error`]'s;
- a file that runs the engine imports `ctadl_ascent::error`'s;
- `ctadl_ascent::Error::Import` is `#[from] ctadl_import::Error`, so a `?` crossing the boundary
  bridges the two on its own.

Worth stating out loud because the failure mode does not name itself: importing both traits
into one file makes every `.err_context(…)` in it ambiguous, and the fix is always to drop one
import rather than to qualify the calls.
*/

pub mod error;
pub mod project;
pub mod store;

pub use error::{Error, ErrorContext};
pub use project::{
    ArtifactImport, ArtifactLanguage, IMPORT_FORMAT_VERSION, INDEX_FORMAT_VERSION, StorePaths,
};
pub use store::{SourceInfoMode, load_import, open_import, save_program_info};
