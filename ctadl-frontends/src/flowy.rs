/*! Importing a flowy artifact.

This is the import half of flowy support. The checking half is `ctadl_ascent::codegen::flowy`.
The two halves are in different crates because importing needs only
[`ctadl_flowy::compile_program`] and a `bitcode` write, while checking needs the index engine,
the query engine and the formatter. Putting them in one module would make flowy the one
language whose import could not be described without the engine.
*/

use ctadl_import::error::{Error, ErrorContext};
use ctadl_import::project::ArtifactImport;
use ctadl_ir::ProgramInfo;

/// Imports a flowy artifact into the store. It also saves the program's requirements, so that
/// they can be checked when a query runs.
pub fn import(import: &ArtifactImport) -> Result<ProgramInfo, Error> {
    let program = ctadl_flowy::compile_program(&import.artifact_path).err_context(|| {
        format!(
            "compiling flowy program: {}",
            import.artifact_path.display()
        )
    })?;

    // Save requirements
    let data = bitcode::serialize(&program.requirements).map_err(Error::from)?;
    std::fs::write(import.requirements_path(), data)
        .map_err(Error::from)
        .err_context(|| {
            format!(
                "writing requirements: {}",
                import.requirements_path().display()
            )
        })?;

    Ok(program.program_info)
}
