/*! CTADL parquet file schemas

In CTADL projects, core Datalog structures are stored in parquet files. This module contains their
schemas and functions to save and load them.
*/
use std::path;

use ctadl_ir::Symbol;
use source_info::FileSpanId;

use crate::error::{Error, ErrorContext};
use crate::facts::parquet;
use crate::facts::{
    FlowVariable, FormalIndex, FormalType, Function, FunctionId, InsnId, Path, TaintState,
};
use crate::query_engine::QueryEndpoint;

// Captures the Record type and FILENAME and COLUMNS constants.
macro_rules! save_load {
    () => {
        pub fn try_save<P: AsRef<path::Path>>(
            path: P,
            items: impl IntoIterator<Item = Record>,
        ) -> Result<(), Error> {
            let path = path.as_ref();
            parquet::Writer::new(path.join(FILENAME))
                .write_vec(&COLUMNS, items.into_iter().collect())
                .err_context(|| format!("saving parquet '{FILENAME}'"))
        }

        pub fn try_load<P: AsRef<path::Path>>(path: P) -> Result<Vec<Record>, Error> {
            let path = path.as_ref();
            parquet::Reader::new(path.join(FILENAME))
                .read_vec(&COLUMNS)
                .err_context(|| format!("loading parquet '{FILENAME}'"))
        }
    };
}

pub mod formal_param {
    use super::*;
    pub type Record = (FunctionId, FormalIndex, FormalType);
    pub const COLUMNS: [&str; 3] = ["func_id", "index", "type"];
    pub const FILENAME: &str = "formal_param.parquet";
    save_load!();
}

pub mod actual_param {
    use super::*;
    pub type Record = (FunctionId, InsnId, FormalIndex, FlowVariable, Path);
    pub const COLUMNS: [&str; 5] = ["func_id", "insn_id", "formal_index", "variable", "path"];
    pub const FILENAME: &str = "actual_param.parquet";
    save_load!();
}

pub mod call {
    use super::*;
    pub type Record = (FunctionId, InsnId, FunctionId);
    pub const COLUMNS: [&str; 3] = ["func_id", "insn_id", "target_id"];
    pub const FILENAME: &str = "call.parquet";
    save_load!();
}

pub mod assign {
    use super::*;
    pub type Record = (FunctionId, InsnId, FlowVariable, Path, FlowVariable, Path);
    pub const COLUMNS: [&str; 6] = [
        "func_id", "insn_id", "dst_var", "dst_path", "src_var", "src_path",
    ];
    pub const FILENAME: &str = "assign.parquet";
    save_load!();
}

pub mod java_obj_assign {
    use super::*;
    pub type Record = (FunctionId, InsnId, FlowVariable, Path, Symbol);
    pub const COLUMNS: [&str; 5] = ["func_id", "insn_id", "dst_var", "dst_path", "class_name"];
    pub const FILENAME: &str = "java_obj_assign.parquet";
    save_load!();
}

pub mod java_call {
    use super::*;
    pub type Record = (FunctionId, InsnId, FlowVariable, Path, Symbol, Symbol);
    pub const COLUMNS: [&str; 6] = [
        "func_id",
        "insn_id",
        "recv_var",
        "recv_path",
        "name",
        "desc",
    ];
    pub const FILENAME: &str = "java_call.parquet";
    save_load!();
}

pub mod java_resolvents {
    use super::*;
    pub type Record = (Symbol, Symbol, Symbol, FunctionId);
    pub const COLUMNS: [&str; 4] = ["class", "name", "desc", "target_id"];
    pub const FILENAME: &str = "java_resolvents.parquet";
    save_load!();
}

pub mod summary {
    use super::*;
    pub type Record = (FunctionId, FormalIndex, Path, FormalIndex, Path);
    pub const COLUMNS: [&str; 5] = ["func_id", "dst_index", "dst_path", "src_index", "src_path"];
    pub const FILENAME: &str = "summary.parquet";
    save_load!();
}

pub mod paths {
    use super::*;
    pub type Record = (Path,);
    pub const COLUMNS: [&str; 1] = ["path"];
    pub const FILENAME: &str = "paths.parquet";
    save_load!();
}

pub mod taint {
    use super::*;
    pub type Record = (FunctionId, TaintState, FlowVariable, Path, QueryEndpoint);
    pub const COLUMNS: [&str; 5] = ["func_id", "taint_state", "dst_var", "dst_path", "endpoint"];
    pub const FILENAME: &str = "taint.parquet";
    save_load!();
}

pub mod index_source_map {
    use super::*;
    pub type Record = (FunctionId, InsnId, FileSpanId);
    pub const COLUMNS: [&str; 3] = ["func_id", "insn_id", "source_span_id"];
    pub const FILENAME: &str = "index_source_map.parquet";
    save_load!();
}

pub mod function_id {
    use super::*;
    pub type Record = (FunctionId, Function);
    pub const COLUMNS: [&str; 2] = ["id", "name"];
    pub const FILENAME: &str = "function_id.parquet";
    save_load!();
}
