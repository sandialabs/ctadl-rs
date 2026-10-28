//! `CTADL_DUMP_RELATIONS=<dir>`: writes every relation of the index engine to `<dir>`, one
//! `<relation>.tsv` per relation, once the fixpoint is reached. `scripts/fact-dump-diff.sh` sorts
//! and compares these dumps to check that two builds or engines derive the same sets.
//!
//! A row is one line, its columns separated by tabs. Every column is printed by content, never by
//! an intern id: a function by its name, a string, path or call target by its text, and a
//! decision set by its decisions, sorted. Rows come out in whatever order the engine holds them,
//! which differs between runs and engines; only the sorted file is comparable.
//!
//! Every relation in the program's `relation_sizes_summary` must be written, or the dump panics
//! naming the ones that were not, so a relation added to `index_rules` and not to
//! [`dump_relations!`](crate::dump_relations) is caught on its first dump.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use crate::facts::{
    CallDispatchKey, CallTargetObject, FlowVariable, FlowVertex, FormalIndex, FormalType,
    FunctionId, IdMap, InsnId, IntentKind, IntentPairKind, PackedInsnSiteId, Path, Str,
};
use crate::index_engine::path_set::PathSetRef;
use crate::index_engine::{Decision, DecisionId, DecisionSet, IndexConfig, Rows, unpack_site};

/// The environment variable that turns the dump on and names its directory.
pub const DUMP_ENV: &str = "CTADL_DUMP_RELATIONS";

/// What a column needs to print itself by content.
pub struct Cx<'a> {
    id_map: Option<&'a IdMap>,
}

impl Cx<'_> {
    fn function(&self, f: FunctionId, out: &mut String) {
        match self.id_map.and_then(|m| m.get_function(f)) {
            Some(name) => out.push_str(name.0.as_ref()),
            // Only a run without an id map (a unit test) gets here.
            None => write!(out, "#{}", f.id).unwrap(),
        }
    }
}

/// One column of a dumped row.
pub trait DumpCol {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String);
}

impl<T: DumpCol + ?Sized> DumpCol for &T {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        (**self).dump_col(cx, out)
    }
}

macro_rules! dump_display {
    ($($t:ty),*) => {$(
        impl DumpCol for $t {
            fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
                write!(out, "{self}").unwrap()
            }
        }
    )*};
}
dump_display!(
    i64,
    FormalIndex,
    FlowVariable,
    FlowVertex,
    Path,
    Str,
    CallDispatchKey
);

macro_rules! dump_debug {
    ($($t:ty),*) => {$(
        impl DumpCol for $t {
            fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
                write!(out, "{self:?}").unwrap()
            }
        }
    )*};
}
dump_debug!(FormalType, IntentKind, IntentPairKind);

impl DumpCol for FunctionId {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        cx.function(*self, out)
    }
}

impl DumpCol for InsnId {
    fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
        write!(out, "{}", self.id).unwrap()
    }
}

impl DumpCol for PackedInsnSiteId {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        let site = unpack_site(self);
        cx.function(site.func_id, out);
        write!(out, ":{}", site.insn_id.id).unwrap()
    }
}

impl<A: DumpCol, B: DumpCol> DumpCol for (A, B) {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        self.0.dump_col(cx, out);
        out.push(' ');
        self.1.dump_col(cx, out);
    }
}

impl DumpCol for CallTargetObject {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        match self {
            CallTargetObject::FunctionId(f) => {
                out.push_str("ptr<");
                cx.function(*f, out);
                out.push('>');
            }
            _ => write!(out, "{self}").unwrap(),
        }
    }
}

/// As [`Decision`]'s `Display`, with the target's function by name.
impl DumpCol for Decision {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        write!(out, "[arg{}{} = ", self.formal, self.path.to_dot_string()).unwrap();
        self.target.dump_col(cx, out);
        out.push(']');
    }
}

impl DumpCol for DecisionId {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        self.get().dump_col(cx, out)
    }
}

/// Sorted by the decisions' text, since a set holds its decisions in id order, and ids are
/// minted in the order the run first derives each decision.
impl DumpCol for DecisionSet {
    fn dump_col(&self, cx: &Cx<'_>, out: &mut String) {
        if self.is_top() {
            out.push('⊤');
            return;
        }
        let mut ds: Vec<String> = self
            .decisions()
            .iter()
            .map(|d| {
                let mut s = String::new();
                d.dump_col(cx, &mut s);
                s
            })
            .collect();
        ds.sort_unstable();
        write!(out, "{{{}}}", ds.join(", ")).unwrap()
    }
}

/// The settings that change what is derived. The engine and storage are left out: they must not
/// change the result, and leaving them out lets two engines' dumps be compared.
impl DumpCol for IndexConfig {
    fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
        let IndexConfig {
            alias_rule,
            hybrid_context,
            context_join,
            parallelism: _,
            storage: _,
            deref_paths,
        } = self;
        write!(
            out,
            "alias_rule={alias_rule} hybrid_context={hybrid_context:?} \
             context_join={context_join} deref_paths={deref_paths}"
        )
        .unwrap()
    }
}

/// The set's size; its paths are the `paths` relation.
impl DumpCol for PathSetRef {
    fn dump_col(&self, _: &Cx<'_>, out: &mut String) {
        write!(out, "{} paths", self.0.len()).unwrap()
    }
}

/// A whole row: a tuple of columns.
pub trait DumpRow {
    fn dump_row(&self, cx: &Cx<'_>, out: &mut String);
}

macro_rules! dump_row_tuple {
    ($first:ident $(, $rest:ident)*) => {
        impl<$first: DumpCol $(, $rest: DumpCol)*> DumpRow for ($first, $($rest,)*) {
            #[allow(non_snake_case)]
            fn dump_row(&self, cx: &Cx<'_>, out: &mut String) {
                let ($first, $($rest,)*) = self;
                push_col($first, cx, out);
                $(
                    out.push('\t');
                    push_col($rest, cx, out);
                )*
            }
        }
    };
}
dump_row_tuple!(A);
dump_row_tuple!(A, B);
dump_row_tuple!(A, B, C);
dump_row_tuple!(A, B, C, D);
dump_row_tuple!(A, B, C, D, E);
dump_row_tuple!(A, B, C, D, E, F);
dump_row_tuple!(A, B, C, D, E, F, G);
dump_row_tuple!(A, B, C, D, E, F, G, H);

/// Prints `col`, escaping what would break the line-per-row, tab-per-column format.
fn push_col(col: &impl DumpCol, cx: &Cx<'_>, out: &mut String) {
    let start = out.len();
    col.dump_col(cx, out);
    if out[start..].contains(['\\', '\t', '\n', '\r']) {
        let raw = out.split_off(start);
        for c in raw.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c => out.push(c),
            }
        }
    }
}

/// The dump of one run, in progress.
pub struct RelationDump<'a> {
    dir: PathBuf,
    cx: Cx<'a>,
    written: Vec<String>,
}

/// The file of one relation, in progress.
pub struct RelationWriter<'c, 'a> {
    cx: &'c Cx<'a>,
    out: std::io::BufWriter<std::fs::File>,
    line: String,
    path: PathBuf,
}

impl RelationWriter<'_, '_> {
    pub fn row(&mut self, row: &impl DumpRow) {
        self.line.clear();
        row.dump_row(self.cx, &mut self.line);
        self.line.push('\n');
        self.out
            .write_all(self.line.as_bytes())
            .unwrap_or_else(|e| panic!("{DUMP_ENV}: writing {}: {e}", self.path.display()));
    }
}

impl<'a> RelationDump<'a> {
    /// The dump `CTADL_DUMP_RELATIONS` asks for, if it is set. Creates the directory.
    pub fn from_env(id_map: Option<&'a IdMap>) -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os(DUMP_ENV)?);
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("{DUMP_ENV}: creating {}: {e}", dir.display()));
        log::info!("dumping the index relations to {}", dir.display());
        Some(RelationDump {
            dir,
            cx: Cx { id_map },
            written: Vec::new(),
        })
    }

    /// Writes relation `name`, whose rows `fill` passes to the writer. Refuses to overwrite an
    /// earlier dump, so a second index run in one process cannot silently replace the first.
    pub fn relation(&mut self, name: &str, fill: impl FnOnce(&mut RelationWriter<'_, 'a>)) {
        let path = self.dir.join(format!("{name}.tsv"));
        let file = std::fs::File::create_new(&path)
            .unwrap_or_else(|e| panic!("{DUMP_ENV}: creating {}: {e}", path.display()));
        let mut w = RelationWriter {
            cx: &self.cx,
            out: std::io::BufWriter::with_capacity(1 << 20, file),
            line: String::new(),
            path,
        };
        fill(&mut w);
        w.out
            .flush()
            .unwrap_or_else(|e| panic!("{DUMP_ENV}: writing {}: {e}", w.path.display()));
        self.written.push(name.to_string());
    }

    /// Checks that every relation in `relation_sizes` (the program's `relation_sizes_summary`)
    /// was written.
    pub fn finish(self, relation_sizes: &str) {
        let missing: Vec<_> = super::profile::parse_relation_sizes(relation_sizes)
            .into_keys()
            .filter(|name| !self.written.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "{DUMP_ENV}: relations not dumped: {}; list them in `dump_relations!`",
            missing.join(", ")
        );
        log::info!(
            "dumped {} relations to {}",
            self.written.len(),
            self.dir.display()
        );
    }
}

/// How a relation's field is dumped. A field holding its rows writes them. A store-backed
/// relation's field holds no rows, so it writes nothing, and [`EngineStores::dump_stores`]
/// writes the relation from its store instead.
///
/// [`EngineStores::dump_stores`]: super::EngineStores::dump_stores
pub trait DumpField<Row> {
    fn dump_field(&self, dump: &mut RelationDump<'_>, name: &str);
}

fn dump_rows<Row: DumpRow>(rows: &dyn Rows<Row>, dump: &mut RelationDump<'_>, name: &str) {
    dump.relation(name, |w| {
        let mut it = rows.stream();
        while let Some(row) = it.next() {
            w.row(row);
        }
    });
}

impl<Row: DumpRow> DumpField<Row> for Vec<Row> {
    fn dump_field(&self, dump: &mut RelationDump<'_>, name: &str) {
        dump_rows::<Row>(self, dump, name)
    }
}

impl<Row: DumpRow> DumpField<Row> for ascent::boxcar::Vec<Row> {
    fn dump_field(&self, dump: &mut RelationDump<'_>, name: &str) {
        dump_rows::<Row>(self, dump, name)
    }
}

impl<Row: DumpRow> DumpField<Row> for ascent::boxcar::Vec<std::sync::RwLock<Row>> {
    fn dump_field(&self, dump: &mut RelationDump<'_>, name: &str) {
        dump_rows::<Row>(self, dump, name)
    }
}

macro_rules! store_backed {
    ($($t:ty),*) => {$(
        impl<Row> DumpField<Row> for $t {
            fn dump_field(&self, _: &mut RelationDump<'_>, _: &str) {}
        }
    )*};
}
store_backed!(
    super::locals_trie::CountingVec<Row>,
    super::assign_like_trie::SeedVec<Row>,
    super::c_locals_trie::CCountingVec<Row>,
    super::c_assign_like_trie::CSeedVec<Row>,
    lsm::ascent_provider::CountingVec<Row>,
    lsm::ascent_provider::SeedVec<Row>
);

/// Dumps every relation of `index_rules` held in its program field; see [`DumpField`].
#[macro_export]
macro_rules! dump_relations {
    ($dump:expr, $prog:expr) => {
        $crate::dump_relations!(@each $dump, $prog, [
            formal_param, actual_param, call, call_target_assign, const_str_assign, callee_info,
            callee_resolvents, intent_send, intent_filter, intent_component, paths, path_set,
            summary, config, intent_frame, locals, assign_like, prog_store, alias_of_formal,
            call_target_assign_like, const_reaches, resolved_call, fnptr_call, intent_pair,
            critical_summary, critical_call, resolvent, establishes_direct, establishes_via,
            context_assign, context_locals, context_summary, reach_vp, locals_key,
            locals_key_wild, locals_wild, assign_wild, edge_split, edge_split_wild, ext_dst,
            ext_fml, critical_arg, critical_reach, context_summary_set, set_member,
            context_summary_top, set_establishes_direct, set_establishes_via, context_summary_d,
            top_summary_func, ctx_edge_wild, ctx_edge_split, ctx_edge_split_wild, ctx_ext_dst,
            ctx_ext_fml, tag_closure_func
        ])
    };
    (@each $dump:expr, $prog:expr, [$($rel:ident),* $(,)?]) => {
        $(
            $crate::index_engine::dump::DumpField::dump_field(
                &$prog.$rel,
                $dump,
                stringify!($rel),
            );
        )*
    };
}
