/*! CTADL parquet file schemas

In CTADL projects, core Datalog structures are stored in parquet files. This module contains their
schemas and functions to save and load them.
*/
use std::path;

use source_info::FileSpanId;

use crate::error::{Error, ErrorContext};
use crate::facts::parquet;
use crate::facts::{
    FlowEdge, FlowVariable, FormalIndex, FormalType, Function, FunctionId, InsnId, Path, TaintState,
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
    pub type Record = (FunctionId, FlowVariable, Path, FlowVariable, Path);
    pub const COLUMNS: [&str; 5] = ["func_id", "dst_var", "dst_path", "src_var", "src_path"];
    pub const FILENAME: &str = "assign.parquet";
    save_load!();
}

pub mod call_target_assign {
    use super::*;
    use crate::facts::CallTargetObject;
    pub type Record = (FunctionId, InsnId, FlowVariable, Path, CallTargetObject);
    pub const COLUMNS: [&str; 5] = ["func_id", "insn_id", "dst_var", "dst_path", "target"];
    pub const FILENAME: &str = "call_target_assign.parquet";
    save_load!();
}

pub mod callee_info {
    use super::*;
    use crate::facts::CallTargetContext;
    /// An indirect / virtual call site awaiting resolution: the receiver vertex
    /// (`recv_var`, `recv_path`) plus the frontend-specific [`CallTargetContext`].
    /// Unifies the former `java_call` and `indirect_call` relations.
    pub type Record = (FunctionId, InsnId, FlowVariable, Path, CallTargetContext);
    pub const COLUMNS: [&str; 5] = ["func_id", "insn_id", "recv_var", "recv_path", "context"];
    pub const FILENAME: &str = "callee_info.parquet";
    save_load!();
}

pub mod callee_resolvents {
    use super::*;
    use crate::facts::{CallTargetContext, CallTargetObject};
    /// How a stored call-target [`CallTargetObject`] resolves, under a given
    /// [`CallTargetContext`], to a concrete callee `target`. Unifies the former
    /// `java_resolvents` (CHA) relation and the identity resolution of C function
    /// pointers.
    pub type Record = (CallTargetObject, CallTargetContext, FunctionId);
    pub const COLUMNS: [&str; 3] = ["object", "context", "target_id"];
    pub const FILENAME: &str = "callee_resolvents.parquet";
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

pub mod taint_edge {
    use super::*;
    /// An edge of the taint graph in execution / data-flow order: the source
    /// vertex `(src_func, src_var, src_path)` flows to the destination vertex
    /// `(dst_func, dst_var, dst_path)`. `edge` classifies the step as a
    /// flow-insensitive intraprocedural (assign/alias) edge or as an
    /// interprocedural call/return edge anchored at a call instruction.
    pub type Record = (
        FlowEdge,
        FunctionId,
        FlowVariable,
        Path,
        FunctionId,
        FlowVariable,
        Path,
    );
    pub const COLUMNS: [&str; 7] = [
        "edge", "src_func", "src_var", "src_path", "dst_func", "dst_var", "dst_path",
    ];
    pub const FILENAME: &str = "taint_edge.parquet";
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

pub mod external_function {
    use super::*;
    pub type Record = (FunctionId,);
    pub const COLUMNS: [&str; 1] = ["func_id"];
    pub const FILENAME: &str = "external_function.parquet";
    save_load!();
}

#[cfg(test)]
mod tests {
    use crate::facts::{
        CallTargetObject, FlowEdge, FlowVariable, FunctionId, InsnId, PackedInsnSiteId, Path,
    };

    /// The `call_target_assign` schema encodes a [`CallTargetObject`] into a tag column
    /// plus nullable function-id and symbol columns; both variants must survive a parquet
    /// round-trip, and the payload of each variant must land in the right column.
    #[test]
    fn call_target_assign_object_round_trips() {
        let var = FlowVariable::default();
        let records: Vec<super::call_target_assign::Record> = vec![
            (
                FunctionId::new(1),
                InsnId::new(10),
                var,
                Path::empty(),
                CallTargetObject::FunctionId(FunctionId::new(99)),
            ),
            (
                FunctionId::new(2),
                InsnId::new(20),
                var,
                Path::empty(),
                CallTargetObject::Symbol(ctadl_ir::Symbol::from("com/example/Foo")),
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::call_target_assign::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::call_target_assign::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
    }

    /// The `callee_info` schema encodes a [`CallTargetContext`] (tag + nullable name/desc
    /// symbol columns). Both the `Java` and `C` arms must survive a parquet round-trip.
    #[test]
    fn callee_info_context_round_trips() {
        use crate::facts::CallTargetContext;
        let var = FlowVariable::default();
        let records: Vec<super::callee_info::Record> = vec![
            (
                FunctionId::new(1),
                InsnId::new(10),
                var,
                Path::empty(),
                CallTargetContext::Java(
                    ctadl_ir::Symbol::from("doThing"),
                    ctadl_ir::Symbol::from("(I)V"),
                ),
            ),
            (
                FunctionId::new(2),
                InsnId::new(20),
                var,
                Path::empty(),
                CallTargetContext::C,
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::callee_info::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::callee_info::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
    }

    /// The `callee_resolvents` schema encodes both a [`CallTargetObject`] and a
    /// [`CallTargetContext`] (each a tag + nullable columns). The CHA (`Symbol`/`Java`) and
    /// identity function-pointer (`FunctionId`/`C`) resolutions must both round-trip.
    #[test]
    fn callee_resolvents_round_trips() {
        use crate::facts::CallTargetContext;
        let records: Vec<super::callee_resolvents::Record> = vec![
            (
                CallTargetObject::Symbol(ctadl_ir::Symbol::from("com/example/Foo")),
                CallTargetContext::Java(
                    ctadl_ir::Symbol::from("doThing"),
                    ctadl_ir::Symbol::from("(I)V"),
                ),
                FunctionId::new(99),
            ),
            (
                CallTargetObject::FunctionId(FunctionId::new(7)),
                CallTargetContext::C,
                FunctionId::new(7),
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::callee_resolvents::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::callee_resolvents::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
    }

    /// The `taint_edge` schema encodes a [`FlowEdge`] into a tag column plus a
    /// nullable site column; every variant must survive a parquet round-trip,
    /// including the anchoring call site of `Call`/`Return` edges.
    #[test]
    fn taint_edge_flow_edge_round_trips() {
        let site = PackedInsnSiteId::try_from_parts(FunctionId::new(7), InsnId::new(42)).unwrap();
        let var = FlowVariable::default();
        let node = |f: u32| (FunctionId::new(f), var, Path::empty());
        let records: Vec<super::taint_edge::Record> = vec![
            (
                FlowEdge::Intra,
                node(1).0,
                node(1).1,
                node(1).2,
                node(2).0,
                node(2).1,
                node(2).2,
            ),
            (
                FlowEdge::Call(site),
                node(2).0,
                node(2).1,
                node(2).2,
                node(3).0,
                node(3).1,
                node(3).2,
            ),
            (
                FlowEdge::Return(site),
                node(3).0,
                node(3).1,
                node(3).2,
                node(4).0,
                node(4).1,
                node(4).2,
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::taint_edge::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::taint_edge::try_load(dir.path()).unwrap();

        let edges: Vec<FlowEdge> = loaded.iter().map(|r| r.0).collect();
        assert_eq!(
            edges,
            vec![
                FlowEdge::Intra,
                FlowEdge::Call(site),
                FlowEdge::Return(site)
            ]
        );
        assert_eq!(loaded, records);
    }
}
