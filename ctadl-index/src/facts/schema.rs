/*! CTADL parquet file schemas

In CTADL projects, core Datalog structures are stored in parquet files. This module contains their
schemas and functions to save and load them.
*/
use std::path;

use source_info::FileSpanId;

use crate::error::Error;
use crate::facts::parquet;
use crate::facts::{
    FlowVariable, FormalIndex, FormalType, Function, FunctionId, GlobalId, ImportId, InsnId,
    IntentKind, Path, Root, Str,
};

// Captures the Record type and FILENAME and COLUMNS constants.
//
// Neither half adds context of its own: `parquet::Writer`/`parquet::Reader` already name the
// full path of the file they failed on, which is strictly more than `FILENAME` says.
macro_rules! save_load {
    () => {
        pub fn try_save<P: AsRef<path::Path>>(
            path: P,
            items: impl IntoIterator<Item = Record>,
        ) -> Result<(), Error> {
            let path = path.as_ref();
            parquet::Writer::new(path.join(FILENAME)).write_iter(&COLUMNS, items)
        }

        pub fn try_load<P: AsRef<path::Path>>(path: P) -> Result<Vec<Record>, Error> {
            let path = path.as_ref();
            parquet::Reader::new(path.join(FILENAME)).read_vec(&COLUMNS)
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

pub mod callee_info {
    use super::*;
    use crate::facts::CallDispatchKey;
    /// An indirect or virtual call site the index leaves unresolved: the receiver vertex
    /// (`recv_var`, `recv_path`) plus the frontend-specific [`CallDispatchKey`].
    pub type Record = (FunctionId, InsnId, FlowVariable, Path, CallDispatchKey);
    pub const COLUMNS: [&str; 5] = ["func_id", "insn_id", "recv_var", "recv_path", "context"];
    pub const FILENAME: &str = "callee_info.parquet";
    save_load!();
}

pub mod resolved_call {
    use super::*;
    /// An indirect / virtual call site the index resolved.
    pub type Record = (FunctionId, InsnId, FunctionId);
    pub const COLUMNS: [&str; 3] = ["func_id", "insn_id", "target_id"];
    pub const FILENAME: &str = "resolved_call.parquet";
    save_load!();
}

pub mod summary {
    use super::*;
    /// A function's flow from root `src.src_path` to root `dst.dst_path`.
    pub type Record = (FunctionId, Root, Path, Root, Path);
    pub const COLUMNS: [&str; 5] = ["func_id", "dst", "dst_path", "src", "src_path"];
    pub const FILENAME: &str = "summary.parquet";
    save_load!();
}

pub mod globals {
    use super::*;
    /// A global and an import that declares it, with the declaration's span in that import's
    /// source-info database when the front end recorded one. A global several imports
    /// declare has a row per import.
    pub type Record = (GlobalId, ImportId, Option<FileSpanId>);
    pub const COLUMNS: [&str; 3] = ["name", "import_id", "source_span_id"];
    pub const FILENAME: &str = "globals.parquet";
    save_load!();
}

pub mod global_flow {
    use super::*;
    /// The program-level flows between two global paths: `src.src_path` reaches
    /// `dst.dst_path` in every instance, read by a `--summary` consumer.
    pub type Record = (GlobalId, Path, GlobalId, Path);
    pub const COLUMNS: [&str; 4] = ["dst", "dst_path", "src", "src_path"];
    pub const FILENAME: &str = "global_flow.parquet";
    save_load!();
}

pub mod paths {
    use super::*;
    pub type Record = (Path,);
    pub const COLUMNS: [&str; 1] = ["path"];
    pub const FILENAME: &str = "paths.parquet";
    save_load!();
}

pub mod index_source_map {
    use super::*;
    /// Where an indexed instruction came from in its artifact's source.
    ///
    /// The [`FileSpanId`] is only meaningful *inside* the import named by the [`ImportId`]:
    /// each import has its own source-info database and numbers its spans from zero, while
    /// function and instruction ids are project-global. A span read against the wrong
    /// import's database still resolves -- to an unrelated line in an unrelated artifact --
    /// so the two travel together and are joined together (see [`import_id`]).
    pub type Record = (FunctionId, InsnId, FileSpanId, ImportId);
    pub const COLUMNS: [&str; 4] = ["func_id", "insn_id", "source_span_id", "import_id"];
    pub const FILENAME: &str = "index_source_map.parquet";
    save_load!();
}

pub mod import_id {
    use super::*;
    /// The artifact import each [`ImportId`] in [`index_source_map`] stands for, by the name
    /// it has in the store, in the order `ctadl index` walked them.
    ///
    /// Recorded rather than recomputed from the project config, so a project whose import
    /// list changed after it was indexed cannot silently shift every span onto the wrong
    /// artifact: the index says what it was built from.
    ///
    /// A plain `String` rather than the interned `Str` every other name column uses: there is
    /// one row per import, so interning buys nothing, and `ctadl inspect` prints the name
    /// instead of the opaque intern id.
    pub type Record = (ImportId, String);
    pub const COLUMNS: [&str; 2] = ["id", "name"];
    pub const FILENAME: &str = "import_id.parquet";
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

pub mod intent_pair {
    use super::*;
    use crate::facts::IntentPairKind;
    pub type Record = (FunctionId, InsnId, FunctionId, IntentPairKind);
    pub const COLUMNS: [&str; 4] = ["func_id", "insn_id", "receiver_id", "kind"];
    pub const FILENAME: &str = "intent_pair.parquet";
    save_load!();
}

// The facts below are not index tables. A staged index run writes them to its stage directory,
// beside the facts `IndexFacts::try_save` writes, so that a stage run in a fresh process can
// read a batch's whole input.

pub mod const_str_assign {
    use super::*;
    /// A string constant stored at a vertex, in the index rules' encoding: the instruction is
    /// dropped, since no rule reads it.
    pub type Record = (FunctionId, FlowVariable, Path, Str);
    pub const COLUMNS: [&str; 4] = ["func_id", "var", "path", "str"];
    pub const FILENAME: &str = "const_str_assign.parquet";
    save_load!();
}

pub mod intent_send {
    use super::*;
    /// An intent sent at `send_insn_id` through the bridge call at `bridge_insn_id`, its
    /// intent in actual `index`.
    pub type Record = (FunctionId, InsnId, InsnId, FormalIndex, IntentKind);
    pub const COLUMNS: [&str; 5] = ["func_id", "bridge_insn_id", "send_insn_id", "index", "kind"];
    pub const FILENAME: &str = "intent_send.parquet";
    save_load!();
}

pub mod intent_filter {
    use super::*;
    /// A receiver's intent filter: an action and a MIME type.
    pub type Record = (Str, Str, IntentKind, FunctionId);
    pub const COLUMNS: [&str; 4] = ["action", "mime", "kind", "receiver_id"];
    pub const FILENAME: &str = "intent_filter.parquet";
    save_load!();
}

pub mod intent_component {
    use super::*;
    /// A receiver by its component class.
    pub type Record = (Str, IntentKind, FunctionId);
    pub const COLUMNS: [&str; 3] = ["class", "kind", "receiver_id"];
    pub const FILENAME: &str = "intent_component.parquet";
    save_load!();
}

pub mod intent_frame {
    use super::*;
    /// A function whose string constants the intent rules follow.
    pub type Record = (FunctionId,);
    pub const COLUMNS: [&str; 1] = ["func_id"];
    pub const FILENAME: &str = "intent_frame.parquet";
    save_load!();
}

pub mod fact_summary {
    use super::*;
    /// A summary the facts seed before the fixpoint, in [`summary`]'s encoding.
    pub type Record = summary::Record;
    pub const COLUMNS: [&str; 5] = summary::COLUMNS;
    pub const FILENAME: &str = "fact_summary.parquet";
    save_load!();
}

pub mod frame_edge {
    use super::*;
    /// A flow between two global paths, an edge of the global frame, in [`global_flow`]'s
    /// encoding.
    pub type Record = global_flow::Record;
    pub const COLUMNS: [&str; 4] = global_flow::COLUMNS;
    pub const FILENAME: &str = "frame_edge.parquet";
    save_load!();
}

pub mod manifest_node {
    use super::*;
    pub type Record = (u32, String);
    pub const COLUMNS: [&str; 2] = ["node_id", "tag"];
    pub const FILENAME: &str = "manifest_node.parquet";
    save_load!();
}

pub mod manifest_node_child {
    use super::*;
    pub type Record = (u32, u32);
    pub const COLUMNS: [&str; 2] = ["parent_id", "child_id"];
    pub const FILENAME: &str = "manifest_node_child.parquet";
    save_load!();
}

pub mod manifest_node_attr {
    use super::*;
    pub type Record = (u32, String, String);
    pub const COLUMNS: [&str; 3] = ["node_id", "key", "value"];
    pub const FILENAME: &str = "manifest_node_attr.parquet";
    save_load!();
}

#[cfg(test)]
mod tests {
    use crate::facts::{FlowVariable, FunctionId, InsnId, IntentPairKind, Path};

    /// A table larger than a row group is written as several, and reads back whole and in
    /// order; an empty one still makes a readable file.
    #[test]
    fn tables_round_trip_across_row_groups() {
        let dir = tempfile::tempdir().unwrap();
        let rows: Vec<super::resolved_call::Record> = (0..super::parquet::ROWS_PER_GROUP * 2 + 7)
            .map(|i| {
                (
                    FunctionId::new(i as u32 % 1000),
                    InsnId::new(i as u64),
                    FunctionId::new(7),
                )
            })
            .collect();
        super::resolved_call::try_save(dir.path(), rows.clone()).unwrap();
        assert_eq!(super::resolved_call::try_load(dir.path()).unwrap(), rows);
        super::resolved_call::try_save(dir.path(), Vec::new()).unwrap();
        assert!(
            super::resolved_call::try_load(dir.path())
                .unwrap()
                .is_empty()
        );
    }

    /// The `callee_info` schema encodes a [`CallDispatchKey`] (tag + nullable name/desc
    /// symbol columns). The `Java`, `C`, `Lua` and `Cil` arms must all survive a parquet
    /// round-trip; `Lua` populates the name column but leaves the descriptor null.
    #[test]
    fn callee_info_dispatch_key_round_trips() {
        use crate::facts::{CallDispatchKey, CallDispatchKeyKind};
        let var = FlowVariable::default();
        let records: Vec<super::callee_info::Record> = vec![
            (
                FunctionId::new(1),
                InsnId::new(10),
                var,
                Path::empty(),
                CallDispatchKeyKind::Java(
                    ctadl_ir::Symbol::from("doThing"),
                    ctadl_ir::Symbol::from("(I)V"),
                )
                .into(),
            ),
            (
                FunctionId::new(2),
                InsnId::new(20),
                var,
                Path::empty(),
                CallDispatchKey::c(),
            ),
            (
                FunctionId::new(3),
                InsnId::new(30),
                var,
                Path::empty(),
                CallDispatchKeyKind::Lua(ctadl_ir::Symbol::from("deposit")).into(),
            ),
            (
                FunctionId::new(4),
                InsnId::new(40),
                var,
                Path::empty(),
                CallDispatchKeyKind::Cil(
                    ctadl_ir::Symbol::from("Parse"),
                    ctadl_ir::Symbol::from("(System.String)->System.Int32"),
                )
                .into(),
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::callee_info::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::callee_info::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
    }

    /// Writes `rows` with 4-row groups, then checks they span at least two groups and read
    /// back whole and in order.
    fn round_trips_in_groups<T>(columns: &[&str], filename: &str, rows: Vec<T>)
    where
        T: Clone + PartialEq + std::fmt::Debug,
        super::parquet::DefaultEncoder: super::parquet::EncodeColumns<T>,
        super::parquet::DefaultDecoder: super::parquet::DecodeColumns<T>,
    {
        assert!(rows.len() > 4);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(filename);
        super::parquet::Writer::new(&file)
            .rows_per_group(4)
            .write_iter(columns, rows.clone())
            .unwrap();
        let reader = super::parquet::Reader::new(&file);
        assert!(reader.num_row_groups().unwrap() >= 2);
        assert_eq!(reader.read_vec::<T>(columns).unwrap(), rows);
    }

    /// The stage-file schemas round-trip across row groups, every variant of their enums
    /// included.
    #[test]
    fn stage_schemas_round_trip_across_row_groups() {
        use super::*;
        use crate::facts::{IntentKind, Root, Str};
        let kinds = [
            IntentKind::Activity,
            IntentKind::Receiver,
            IntentKind::StartedService,
            IntentKind::BoundService,
        ];
        let path = |s: &str| Path::from_symbol_names([s]);
        let var = |i: usize| match i % 3 {
            0 => FlowVariable::local(Str::from(format!("v{i}").as_str())),
            1 => FlowVariable::formal_index(FormalIndex::new(i as i16)),
            _ => FlowVariable::global(GlobalId(Str::from(format!("G{i}").as_str()))),
        };
        let root = |i: usize| match i % 2 {
            0 => Root::formal(FormalIndex::new(i as i16)),
            _ => Root::global(GlobalId(Str::from(format!("G{i}").as_str()))),
        };
        let n = 9;
        round_trips_in_groups(
            &const_str_assign::COLUMNS,
            const_str_assign::FILENAME,
            (0..n)
                .map(|i| {
                    let s = Str::from(format!("s{i}").as_str());
                    (FunctionId::new(i as u32), var(i), path("f"), s)
                })
                .collect(),
        );
        round_trips_in_groups(
            &intent_send::COLUMNS,
            intent_send::FILENAME,
            (0..n)
                .map(|i| {
                    (
                        FunctionId::new(i as u32),
                        InsnId::new(i as u64),
                        InsnId::new(100 + i as u64),
                        FormalIndex::new(i as i16 - 1),
                        kinds[i % 4],
                    )
                })
                .collect(),
        );
        round_trips_in_groups(
            &intent_filter::COLUMNS,
            intent_filter::FILENAME,
            (0..n)
                .map(|i| {
                    (
                        Str::from(format!("action{i}").as_str()),
                        Str::from(format!("mime{i}").as_str()),
                        kinds[i % 4],
                        FunctionId::new(i as u32),
                    )
                })
                .collect(),
        );
        round_trips_in_groups(
            &intent_component::COLUMNS,
            intent_component::FILENAME,
            (0..n)
                .map(|i| {
                    let class = Str::from(format!("C{i}").as_str());
                    (class, kinds[i % 4], FunctionId::new(i as u32))
                })
                .collect(),
        );
        round_trips_in_groups(
            &intent_frame::COLUMNS,
            intent_frame::FILENAME,
            (0..n).map(|i| (FunctionId::new(i as u32),)).collect(),
        );
        round_trips_in_groups(
            &fact_summary::COLUMNS,
            fact_summary::FILENAME,
            (0..n)
                .map(|i| {
                    let f = FunctionId::new(i as u32);
                    (f, root(i), path("a"), root(i + 1), path("b"))
                })
                .collect(),
        );
        round_trips_in_groups(
            &frame_edge::COLUMNS,
            frame_edge::FILENAME,
            (0..n)
                .map(|i| {
                    let g = |j: usize| GlobalId(Str::from(format!("G{j}").as_str()));
                    (g(i), path("a"), g(i + 1), Path::empty())
                })
                .collect(),
        );
    }

    #[test]
    fn intent_pair_round_trips() {
        let records: Vec<super::intent_pair::Record> = vec![
            (
                FunctionId::new(1),
                InsnId::new(10),
                FunctionId::new(2),
                IntentPairKind::Explicit,
            ),
            (
                FunctionId::new(1),
                InsnId::new(11),
                FunctionId::new(3),
                IntentPairKind::Implicit,
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::intent_pair::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::intent_pair::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
    }
}
