/*! Importing a flowy artifact.

Flowy's *import* half, split from its *check* half (`ctadl_ascent::codegen::flowy`), because the two
sit on opposite sides of the front-end boundary: importing needs only
[`ctadl_flowy::compile_program`] and a `bitcode` write, while checking needs the index engine,
the query engine and the formatter. Keeping them in one module made flowy the one language
whose dispatch arm could not be described without the engine.
*/

use ctadl_import::error::{Error, ErrorContext};
use ctadl_import::project::ArtifactImport;
use ctadl_ir::ProgramInfo;

/// Imports a flowy artifact into the store. This also saves the requirements so that they can be
/// checked at query time.
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
