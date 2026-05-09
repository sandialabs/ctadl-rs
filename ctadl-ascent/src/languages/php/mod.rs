/*! PHP language frontend

Converts PHP into CTADL IR.
*/

use crate::error::Error;
use ctadl_ir::mir::call::VirtualMethodTable;
use ctadl_ir::mir::{Program, ProgramInfo};
use std::fs;
use std::path::{Path, PathBuf};

pub fn import_php<P: AsRef<Path>>(path: P) -> Result<ProgramInfo, Error> {
    let path = path.as_ref();
    if path.is_dir() {
        return import_php_directory(path);
    }

    let mut program_info = new_program_info();
    parse_php_file(
        &mut program_info,
        path,
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown.php"),
    )?;
    Ok(program_info)
}

fn import_php_directory(path: &Path) -> Result<ProgramInfo, Error> {
    let mut files = Vec::new();
    collect_php_files(path, &mut files)?;
    files.sort();

    if files.is_empty() {
        return Err(Error::Path {
            message: format!("no PHP files found in directory '{}'", path.display()),
        });
    }

    let mut program_info = new_program_info();
    for file in files {
        let relative_name = file
            .strip_prefix(path)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        parse_php_file(&mut program_info, &file, &relative_name)?;
    }

    Ok(program_info)
}

fn new_program_info() -> ProgramInfo {
    ProgramInfo {
        program: Program::default(),
        vmt: VirtualMethodTable::new_php(),
        source_info: source_info::SourceInfo::default(),
    }
}

// TODO this should be iterative not recursive
fn collect_php_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_php_files(&path, files)?;
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("php"))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn parse_php_file(
    program_info: &mut ProgramInfo,
    path: &Path,
    file_name: &str,
) -> Result<(), Error> {
    let source = fs::read_to_string(path)?;
    let source_path = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();
    php_reader::lower_php_into(&source, file_name, &source_path, program_info)?;
    Ok(())
}
