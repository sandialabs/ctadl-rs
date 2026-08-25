/*! CTADL parquet file schemas

In CTADL projects, core Datalog structures are stored in parquet files. This module contains their
schemas and functions to save and load them.
*/
use std::path;

use source_info::FileSpanId;

use crate::error::Error;
use crate::facts::parquet;
use crate::facts::{
    CallString, FlowEdge, FlowVariable, FormalIndex, FormalType, Function, FunctionId, ImportId,
    InsnId, Path, TaintState,
};
use crate::query_engine::QueryEndpoint;

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
            parquet::Writer::new(path.join(FILENAME))
                .write_vec(&COLUMNS, items.into_iter().collect())
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
    use crate::facts::CallDispatchKey;
    /// An indirect / virtual call site awaiting resolution: the receiver vertex
    /// (`recv_var`, `recv_path`) plus the frontend-specific [`CallDispatchKey`].
    /// Unifies the former `java_call` and `indirect_call` relations.
    pub type Record = (FunctionId, InsnId, FlowVariable, Path, CallDispatchKey);
    pub const COLUMNS: [&str; 5] = ["func_id", "insn_id", "recv_var", "recv_path", "context"];
    pub const FILENAME: &str = "callee_info.parquet";
    save_load!();
}

pub mod callee_resolvents {
    use super::*;
    use crate::facts::{CallDispatchKey, CallTargetObject};
    /// How a stored call-target [`CallTargetObject`] resolves, under a given
    /// [`CallDispatchKey`], to a concrete callee `target`. Unifies the former
    /// `java_resolvents` (CHA) relation and the identity resolution of C function
    /// pointers.
    pub type Record = (CallTargetObject, CallDispatchKey, FunctionId);
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

pub mod context_assign {
    use super::*;
    /// An assignment derived by instantiating a resolved callee's summary at a dynamically
    /// dispatched call site, valid only under the calling context `call_string` names.
    ///
    /// Persisted (unlike the rest of the hybrid-inlining machinery) because the query engine
    /// traverses these rows under a context annotation rather than having the index collapse
    /// them into plain `assign_like`: collapsing unions the per-context answer away, and the
    /// index worked to compute it. The call string is non-empty by invariant.
    pub type Record = (
        FunctionId,
        FlowVariable,
        Path,
        FlowVariable,
        Path,
        CallString,
    );
    pub const COLUMNS: [&str; 6] = [
        "func_id",
        "dst_var",
        "dst_path",
        "src_var",
        "src_path",
        "call_string",
    ];
    pub const FILENAME: &str = "context_assign.parquet";
    save_load!();
}

pub mod resolved_call {
    use super::*;
    /// The resolved callee of a dynamically dispatched site, under the context that resolves it.
    /// An empty `call_string` means the resolution is unconditional (the target was stored in the
    /// very frame holding the call); a non-empty one names the stack configuration it holds under.
    ///
    /// The call-graph edge a resolved indirect site otherwise has none of: `call` is an input
    /// relation the fixpoint never extends, so without this table a flow that *starts or ends
    /// inside* the resolved callee cannot cross the site at all — only a formal-to-out-formal
    /// flow, which the callee's summary already describes, can.
    pub type Record = (FunctionId, InsnId, FunctionId, CallString);
    pub const COLUMNS: [&str; 4] = ["func_id", "insn_id", "target_id", "call_string"];
    pub const FILENAME: &str = "resolved_call.parquet";
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

#[cfg(test)]
mod tests {
    use crate::facts::{
        CallString, CallTargetObject, FlowEdge, FlowVariable, FunctionId, InsnId, PackedInsnSiteId,
        Path,
    };

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
                CallTargetObject::FunctionId(FunctionId::new(99)),
            ),
            (
                FunctionId::new(2),
                InsnId::new(20),
                var,
                Path::empty(),
                CallTargetObject::Symbol(ctadl_ir::Symbol::from("com/example/Foo")),
            ),
            (
                FunctionId::new(3),
                InsnId::new(30),
                var,
                Path::empty(),
                CallTargetObject::LuaClass(ctadl_ir::Symbol::from("lua$class$Account")),
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::call_target_assign::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::call_target_assign::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
    }

    /// The `callee_info` schema encodes a [`CallDispatchKey`] (tag + nullable name/desc
    /// symbol columns). The `Java`, `C` and `Lua` arms must all survive a parquet round-trip;
    /// `Lua` populates the name column but leaves the descriptor null.
    #[test]
    fn callee_info_dispatch_key_round_trips() {
        use crate::facts::CallDispatchKey;
        let var = FlowVariable::default();
        let records: Vec<super::callee_info::Record> = vec![
            (
                FunctionId::new(1),
                InsnId::new(10),
                var,
                Path::empty(),
                CallDispatchKey::Java(
                    ctadl_ir::Symbol::from("doThing"),
                    ctadl_ir::Symbol::from("(I)V"),
                ),
            ),
            (
                FunctionId::new(2),
                InsnId::new(20),
                var,
                Path::empty(),
                CallDispatchKey::C,
            ),
            (
                FunctionId::new(3),
                InsnId::new(30),
                var,
                Path::empty(),
                CallDispatchKey::Lua(ctadl_ir::Symbol::from("deposit")),
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::callee_info::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::callee_info::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
    }

    /// The `callee_resolvents` schema encodes both a [`CallTargetObject`] and a
    /// [`CallDispatchKey`] (each a tag + nullable columns). The JVM CHA (`Symbol`/`Java`), the
    /// identity function-pointer (`FunctionId`/`C`) and the Lua CHA (`LuaClass`/`Lua`)
    /// resolutions must all round-trip.
    #[test]
    fn callee_resolvents_round_trips() {
        use crate::facts::CallDispatchKey;
        let records: Vec<super::callee_resolvents::Record> = vec![
            (
                CallTargetObject::Symbol(ctadl_ir::Symbol::from("com/example/Foo")),
                CallDispatchKey::Java(
                    ctadl_ir::Symbol::from("doThing"),
                    ctadl_ir::Symbol::from("(I)V"),
                ),
                FunctionId::new(99),
            ),
            (
                CallTargetObject::FunctionId(FunctionId::new(7)),
                CallDispatchKey::C,
                FunctionId::new(7),
            ),
            (
                CallTargetObject::LuaClass(ctadl_ir::Symbol::from("lua$class$Account")),
                CallDispatchKey::Lua(ctadl_ir::Symbol::from("deposit")),
                FunctionId::new(42),
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

    /// A [`CallString`] column encodes as a delimited string of its frames, so both ends of the
    /// range must survive a round trip: the empty string (an *unconditional* resolution, which
    /// is what tells the query engine an edge needs no context) and a multi-frame one (a real
    /// stack configuration, whose frame *order* is what `refine`/`pop` read). Collapsing either
    /// would silently change which flows a query can cross.
    #[test]
    fn resolved_call_call_string_round_trips() {
        let site = |f: u32, i: u64| {
            PackedInsnSiteId::try_from_parts(FunctionId::new(f), InsnId::new(i)).unwrap()
        };
        let multi = CallString::intern(&[site(1, 10), site(2, 20), site(3, 30)]);
        let single = CallString::intern(&[site(4, 40)]);
        let records: Vec<super::resolved_call::Record> = vec![
            // Unconditional: the in-frame bypass's shape.
            (
                FunctionId::new(1),
                InsnId::new(5),
                FunctionId::new(99),
                CallString::new(),
            ),
            (
                FunctionId::new(2),
                InsnId::new(6),
                FunctionId::new(98),
                single,
            ),
            (
                FunctionId::new(3),
                InsnId::new(7),
                FunctionId::new(97),
                multi,
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::resolved_call::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::resolved_call::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
        // Frame order is load-bearing (the current frame is the *last*), so assert it explicitly
        // rather than relying on `CallString`'s pointer equality to have caught a reversal.
        assert_eq!(loaded[2].3.len(), 3);
        assert_eq!(loaded[2].3.top(), Some(site(3, 30)));
    }

    /// The `context_assign` schema is the `assign` schema plus a [`CallString`]. Its rows are
    /// non-empty by invariant, but the codec is shared with `resolved_call`, so this pins the
    /// six-column shape and the path/variable columns riding alongside the context.
    #[test]
    fn context_assign_round_trips() {
        let site = |f: u32, i: u64| {
            PackedInsnSiteId::try_from_parts(FunctionId::new(f), InsnId::new(i)).unwrap()
        };
        let var = FlowVariable::default();
        let cs = CallString::intern(&[site(1, 10), site(2, 20)]);
        let records: Vec<super::context_assign::Record> = vec![
            (
                FunctionId::new(1),
                var,
                Path::empty(),
                var,
                Path::empty(),
                cs,
            ),
            (
                FunctionId::new(2),
                var,
                Path::empty(),
                var,
                Path::empty(),
                CallString::intern(&[site(7, 70)]),
            ),
        ];

        let dir = tempfile::tempdir().unwrap();
        super::context_assign::try_save(dir.path(), records.clone()).unwrap();
        let loaded = super::context_assign::try_load(dir.path()).unwrap();
        assert_eq!(loaded, records);
    }
}
