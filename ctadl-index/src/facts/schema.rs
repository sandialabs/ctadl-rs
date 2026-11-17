/*! CTADL parquet file schemas

In CTADL projects, core Datalog structures are stored in parquet files. This module contains their
schemas and functions to save and load them.
*/
use std::path;

use source_info::FileSpanId;

use crate::error::Error;
use crate::facts::QueryEndpoint;
use crate::facts::parquet;
use crate::facts::{
    FlowEdge, FlowVariable, FormalIndex, FormalType, Function, FunctionId, GlobalId, ImportId,
    InsnId, Path, Root, TaintState,
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

// Declares every table: a module per table, as `name { items }`, holding `items`, its `FILENAME`
// (`name.parquet`) and `save_load!`; and [`Table`], with a variant per module. A table is
// declared here and nowhere else, so whatever matches on [`Table`] without a wildcard arm must
// handle a new one before it compiles.
macro_rules! tables {
    ($($(#[$meta:meta])* $name:ident { $($item:tt)* })*) => {
        $(
            $(#[$meta])*
            pub mod $name {
                use super::*;
                $($item)*
                pub const FILENAME: &str = concat!(stringify!($name), ".parquet");
                save_load!();
            }
        )*

        paste::paste! {
            /// One variant per table this module declares.
            #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub enum Table {
                $([<$name:camel>],)*
            }

            impl Table {
                /// Every table, in declaration order.
                pub const ALL: &[Table] = &[$(Table::[<$name:camel>],)*];

                /// The file the table is stored in, its module's `FILENAME`.
                pub fn filename(self) -> &'static str {
                    match self {
                        $(Table::[<$name:camel>] => $name::FILENAME,)*
                    }
                }

                /// The table's name: its file name without `.parquet`.
                pub fn name(self) -> &'static str {
                    match self {
                        $(Table::[<$name:camel>] => stringify!($name),)*
                    }
                }

                /// The table stored in `filename`, if any.
                pub fn from_filename(filename: &str) -> Option<Table> {
                    match filename {
                        $(f if f == $name::FILENAME => Some(Table::[<$name:camel>]),)*
                        _ => None,
                    }
                }

                /// The table named `name` (a file name without `.parquet`), if any.
                pub fn from_name(name: &str) -> Option<Table> {
                    match name {
                        $(stringify!($name) => Some(Table::[<$name:camel>]),)*
                        _ => None,
                    }
                }
            }
        }
    };
}

tables! {
    formal_param {
        pub type Record = (FunctionId, FormalIndex, FormalType);
        pub const COLUMNS: [&str; 3] = ["func_id", "index", "type"];
    }

    actual_param {
        pub type Record = (FunctionId, InsnId, FormalIndex, FlowVariable, Path);
        pub const COLUMNS: [&str; 5] = ["func_id", "insn_id", "formal_index", "variable", "path"];
    }

    call {
        pub type Record = (FunctionId, InsnId, FunctionId);
        pub const COLUMNS: [&str; 3] = ["func_id", "insn_id", "target_id"];
    }

    assign {
        pub type Record = (FunctionId, FlowVariable, Path, FlowVariable, Path);
        pub const COLUMNS: [&str; 5] = ["func_id", "dst_var", "dst_path", "src_var", "src_path"];
    }

    call_target_assign {
        use crate::facts::CallTargetObject;
        pub type Record = (FunctionId, InsnId, FlowVariable, Path, CallTargetObject);
        pub const COLUMNS: [&str; 5] = ["func_id", "insn_id", "dst_var", "dst_path", "target"];
    }

    callee_info {
        use crate::facts::CallDispatchKey;
        /// An indirect / virtual call site awaiting resolution: the receiver vertex
        /// (`recv_var`, `recv_path`) plus the frontend-specific [`CallDispatchKey`].
        pub type Record = (FunctionId, InsnId, FlowVariable, Path, CallDispatchKey);
        pub const COLUMNS: [&str; 5] = ["func_id", "insn_id", "recv_var", "recv_path", "context"];
    }

    callee_resolvents {
        use crate::facts::{CallDispatchKey, CallTargetObject};
        /// How a stored call-target [`CallTargetObject`] resolves, under a given
        /// [`CallDispatchKey`], to a concrete callee `target`.
        pub type Record = (CallTargetObject, CallDispatchKey, FunctionId);
        pub const COLUMNS: [&str; 3] = ["object", "context", "target_id"];
    }

    resolved_call {
        /// An indirect / virtual call site the index resolved.
        pub type Record = (FunctionId, InsnId, FunctionId);
        pub const COLUMNS: [&str; 3] = ["func_id", "insn_id", "target_id"];
    }

    summary {
        /// A function's flow from root `src.src_path` to root `dst.dst_path`.
        pub type Record = (FunctionId, Root, Path, Root, Path);
        pub const COLUMNS: [&str; 5] = ["func_id", "dst", "dst_path", "src", "src_path"];
    }

    globals {
        /// A global and an import that declares it, with the declaration's span in that import's
        /// source-info database when the front end recorded one. A global several imports
        /// declare has a row per import.
        pub type Record = (GlobalId, ImportId, Option<FileSpanId>);
        pub const COLUMNS: [&str; 3] = ["name", "import_id", "source_span_id"];
    }

    global_flow {
        /// The program-level flows between two global paths: `src.src_path` reaches
        /// `dst.dst_path` in every instance, read by a `--summary` consumer.
        pub type Record = (GlobalId, Path, GlobalId, Path);
        pub const COLUMNS: [&str; 4] = ["dst", "dst_path", "src", "src_path"];
    }

    paths {
        pub type Record = (Path,);
        pub const COLUMNS: [&str; 1] = ["path"];
    }

    taint {
        pub type Record = (FunctionId, TaintState, FlowVariable, Path, QueryEndpoint);
        pub const COLUMNS: [&str; 5] = ["func_id", "taint_state", "dst_var", "dst_path", "endpoint"];
    }

    taint_edge {
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
    }

    index_source_map {
        /// Where an indexed instruction came from in its artifact's source.
        ///
        /// The [`FileSpanId`] is only meaningful *inside* the import named by the [`ImportId`]:
        /// each import has its own source-info database and numbers its spans from zero, while
        /// function and instruction ids are project-global. A span read against the wrong
        /// import's database still resolves -- to an unrelated line in an unrelated artifact --
        /// so the two travel together and are joined together (see [`import_id`]).
        pub type Record = (FunctionId, InsnId, FileSpanId, ImportId);
        pub const COLUMNS: [&str; 4] = ["func_id", "insn_id", "source_span_id", "import_id"];
    }

    import_id {
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
    }

    function_id {
        pub type Record = (FunctionId, Function);
        pub const COLUMNS: [&str; 2] = ["id", "name"];
    }

    external_function {
        pub type Record = (FunctionId,);
        pub const COLUMNS: [&str; 1] = ["func_id"];
    }

    intent_pair {
        use crate::facts::IntentPairKind;
        pub type Record = (FunctionId, InsnId, FunctionId, IntentPairKind);
        pub const COLUMNS: [&str; 4] = ["func_id", "insn_id", "receiver_id", "kind"];
    }

    manifest_node {
        pub type Record = (u32, String);
        pub const COLUMNS: [&str; 2] = ["node_id", "tag"];
    }

    manifest_node_child {
        pub type Record = (u32, u32);
        pub const COLUMNS: [&str; 2] = ["parent_id", "child_id"];
    }

    manifest_node_attr {
        pub type Record = (u32, String, String);
        pub const COLUMNS: [&str; 3] = ["node_id", "key", "value"];
    }
}

#[cfg(test)]
mod tests {
    use crate::facts::{
        CallTargetObjectKind, FlowEdge, FlowVariable, FunctionId, InsnId, IntentPairKind,
        PackedInsnSiteId, Path,
    };

    /// Every table has its own file, and there are as many as the schema declares.
    #[test]
    fn table_filenames_unique() {
        let names: std::collections::BTreeSet<_> =
            super::Table::ALL.iter().map(|t| t.filename()).collect();
        assert_eq!(names.len(), super::Table::ALL.len());
        assert_eq!(super::Table::ALL.len(), 22);
    }

    /// A table's file name and name lead back to it; a file no table is stored in leads
    /// nowhere.
    #[test]
    fn from_filename_round_trips() {
        for &t in super::Table::ALL {
            assert_eq!(super::Table::from_filename(t.filename()), Some(t));
            assert_eq!(super::Table::from_name(t.name()), Some(t));
            assert_eq!(t.filename(), format!("{}.parquet", t.name()));
        }
        assert_eq!(
            super::Table::from_filename("update_sans_path.parquet"),
            None
        );
        assert_eq!(super::Table::from_name("update_sans_path"), None);
        assert_eq!(super::Table::Call.filename(), super::call::FILENAME);
    }

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

    /// The `call_target_assign` schema encodes a [`CallTargetObject`] into a tag column
    /// plus nullable function-id and symbol columns; every variant must survive a parquet
    /// round-trip, and the payload of each variant must land in the right column. `Symbol` and
    /// `LuaClass` share the symbol column, so only the tag keeps them apart — the case that
    /// would silently merge a JVM and a Lua import's classes if the tag were dropped.
    #[test]
    fn call_target_assign_object_round_trips() {
        let var = FlowVariable::default();
        let records: Vec<super::call_target_assign::Record> = vec![
            (
                FunctionId::new(1),
                InsnId::new(10),
                var,
                Path::empty(),
                CallTargetObjectKind::FunctionId(FunctionId::new(99)).into(),
            ),
            (
                FunctionId::new(2),
                InsnId::new(20),
                var,
                Path::empty(),
                CallTargetObjectKind::Symbol(ctadl_ir::Symbol::from("com/example/Foo")).into(),
            ),
            (
                FunctionId::new(3),
                InsnId::new(30),
                var,
                Path::empty(),
                CallTargetObjectKind::LuaClass(ctadl_ir::Symbol::from("lua$class$Account")).into(),
            ),
            (
                FunctionId::new(4),
                InsnId::new(40),
                var,
                Path::empty(),
                CallTargetObjectKind::CilClass(ctadl_ir::Symbol::from("[App]Acme.Gadget")).into(),
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::call_target_assign::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::call_target_assign::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
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

    /// The `callee_resolvents` schema encodes both a [`CallTargetObject`] and a
    /// [`CallDispatchKey`] (each a tag + nullable columns). The JVM CHA (`Symbol`/`Java`), the
    /// identity function-pointer (`FunctionId`/`C`), the Lua CHA (`LuaClass`/`Lua`) and the
    /// .NET CHA (`CilClass`/`Cil`) resolutions must all round-trip.
    #[test]
    fn callee_resolvents_round_trips() {
        use crate::facts::{CallDispatchKey, CallDispatchKeyKind};
        let records: Vec<super::callee_resolvents::Record> = vec![
            (
                CallTargetObjectKind::Symbol(ctadl_ir::Symbol::from("com/example/Foo")).into(),
                CallDispatchKeyKind::Java(
                    ctadl_ir::Symbol::from("doThing"),
                    ctadl_ir::Symbol::from("(I)V"),
                )
                .into(),
                FunctionId::new(99),
            ),
            (
                CallTargetObjectKind::FunctionId(FunctionId::new(7)).into(),
                CallDispatchKey::c(),
                FunctionId::new(7),
            ),
            (
                CallTargetObjectKind::LuaClass(ctadl_ir::Symbol::from("lua$class$Account")).into(),
                CallDispatchKeyKind::Lua(ctadl_ir::Symbol::from("deposit")).into(),
                FunctionId::new(42),
            ),
            (
                CallTargetObjectKind::CilClass(ctadl_ir::Symbol::from("[App]Acme.Gadget")).into(),
                CallDispatchKeyKind::Cil(
                    ctadl_ir::Symbol::from("Parse"),
                    ctadl_ir::Symbol::from("(System.String)->System.Int32"),
                )
                .into(),
                FunctionId::new(43),
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
            (
                FlowEdge::Global,
                node(4).0,
                node(4).1,
                node(4).2,
                crate::facts::GLOBAL_FRAME,
                FlowVariable::global(crate::facts::GlobalId::new("g")),
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
                FlowEdge::Return(site),
                FlowEdge::Global
            ]
        );
        assert_eq!(loaded, records);
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
