//! One side's names for function and import ids.

use std::path::Path as FsPath;

use rustc_hash::FxHashMap;

use ctadl_index::facts::schema::{Table, function_id, import_id};
use ctadl_index::facts::{FunctionId, GLOBAL_FRAME, GLOBAL_FRAME_NAME, ImportId, Str};

use crate::canon::FnName;
use crate::table::load_raw;
use crate::{Error, Side};

/// `FunctionId -> name`, from `function_id.parquet`'s `id` column, not from row positions.
/// [`GLOBAL_FRAME`], which the table never holds, is named [`GLOBAL_FRAME_NAME`].
pub(crate) struct FunctionNames {
    by_id: FxHashMap<FunctionId, FnName>,
    global_frame: FnName,
}

impl FunctionNames {
    /// The names in `dir`'s `function_id.parquet`, or none if `present` is false. A repeated
    /// id is an error.
    pub(crate) fn load(dir: &FsPath, side: Side, present: bool) -> Result<Self, Error> {
        let mut by_id = FxHashMap::default();
        if present {
            let rows = load_raw(dir, side, Table::FunctionId, |d| function_id::try_load(d))?;
            by_id.reserve(rows.len());
            for (i, (id, name)) in rows.into_iter().enumerate() {
                let repeated = id == GLOBAL_FRAME || by_id.insert(id, FnName(name.0)).is_some();
                if repeated {
                    return Err(Error::new(format!(
                        "{} in {side}, row {i}: function id {} is repeated or reserved",
                        function_id::FILENAME,
                        id.id
                    )));
                }
            }
        }
        Ok(FunctionNames {
            by_id,
            global_frame: FnName(Str::from(GLOBAL_FRAME_NAME)),
        })
    }

    /// The name of `id`. An id `function_id.parquet` does not hold is an error.
    pub(crate) fn get(&self, id: FunctionId) -> Result<FnName, String> {
        if id == GLOBAL_FRAME {
            return Ok(self.global_frame);
        }
        self.by_id
            .get(&id)
            .copied()
            .ok_or_else(|| format!("function id {} is not in {}", id.id, function_id::FILENAME))
    }
}

/// `ImportId -> name`, from `import_id.parquet`.
pub(crate) struct ImportNames {
    by_id: FxHashMap<ImportId, Str>,
}

impl ImportNames {
    /// The names in `dir`'s `import_id.parquet`, or none if `present` is false. A repeated id
    /// is an error.
    pub(crate) fn load(dir: &FsPath, side: Side, present: bool) -> Result<Self, Error> {
        let mut by_id = FxHashMap::default();
        if present {
            let rows = load_raw(dir, side, Table::ImportId, |d| import_id::try_load(d))?;
            for (i, (id, name)) in rows.into_iter().enumerate() {
                if by_id.insert(id, Str::from(name)).is_some() {
                    return Err(Error::new(format!(
                        "{} in {side}, row {i}: import id {} is repeated",
                        import_id::FILENAME,
                        id.0
                    )));
                }
            }
        }
        Ok(ImportNames { by_id })
    }

    /// The name of `id`. An id `import_id.parquet` does not hold is an error.
    pub(crate) fn get(&self, id: ImportId) -> Result<Str, String> {
        self.by_id
            .get(&id)
            .copied()
            .ok_or_else(|| format!("import id {} is not in {}", id.0, import_id::FILENAME))
    }
}
