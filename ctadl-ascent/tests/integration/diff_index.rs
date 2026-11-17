//! `diff-index` on real indexes. A rewrite that changes nothing an index means (row order,
//! row groups, function, instruction, span and import numbering) reports `IDENTICAL`, and an
//! edit that does is reported as exactly what it changed.
//!
//! Each test copies an index that `cli::index` wrote and rewrites the copy's tables through the
//! schema's own `try_load` and `try_save`, or through arrow to split its row groups. There are
//! two fixtures: a C program with direct and indirect calls, and two Lua imports joined by a
//! bridge whose callee port has a sub-path, so it has a bridge temporary and two imports.

use std::cell::Cell;
use std::fmt::Debug;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ctadl_ascent::cli;
use ctadl_ascent::facts::schema::{self, Table};
use ctadl_ascent::facts::{
    CallArgId, CallTargetObject, CallTargetObjectKind, FlowEdge, FlowVariable, FlowVariableKind,
    FlowVertex, FormalIndex, FunctionId, GLOBAL_FRAME, ImportId, InsnId, Label, PackedInsnSiteId,
    Path as FactPath, QueryEndpoint, Str, TaintDirection, TaintState,
};
use ctadl_ascent::project::{AnalysisProject, ArtifactImport, ArtifactLanguage, SubImports};
use ctadl_index::index_engine::unpack_site;
use ctadl_index_diff::{Options, Report, Sample, TableReport, diff_index};
use source_info::FileSpanId;

use crate::common::{copy_index, store_dir};

/// Direct calls, a function pointer passed as an argument, and one stored in a struct field.
/// In `wrap`, `apply(twice, b)` and `through(&o, r)` are two calls with different arguments.
const OPS_C: &str = r"
    struct ops { int (*f)(int); int tag; };
    int id(int p) { return p; }
    int twice(int p) { return p + p; }
    int apply(int (*f)(int), int x) { return f(x); }
    int through(struct ops *o, int x) { return o->f(x); }
    int wrap(int a, int b) {
        struct ops o;
        o.f = id;
        o.tag = a;
        int r = apply(twice, b);
        int s = through(&o, r);
        return s + apply(id, a) + o.tag;
    }";

/// The app half of the bridge fixture: `alpha_stub` is called and defined nowhere.
const BRIDGE_APP: &str = r#"
local function app_source()
  return io.read()
end

local function app_sink(x)
  print(x)
end

local function main_app()
  local t = app_source()
  local u = alpha_stub(t)
  app_sink(u)
end

main_app()
"#;

/// The library half, whose `gamma_impl` reads its argument off a sub-path.
const BRIDGE_LIB: &str = r#"
local function gamma_impl(s)
  return s
end

return gamma_impl
"#;

/// Imports `sources`, each `(name, file name, language, text)`, into the store, indexes them
/// as `project` with `models`, and returns the index directory.
fn index_project(
    project: &str,
    sources: &[(&str, &str, ArtifactLanguage, &str)],
    models: &[PathBuf],
) -> PathBuf {
    let dir = store_dir();
    let mut names = Vec::new();
    for &(name, file, language, text) in sources {
        let src = dir.join(file);
        std::fs::write(&src, text).unwrap();
        let import = ArtifactImport::try_create(name, language, &src).unwrap();
        cli::import(&import, cli::ImportOptions::default()).unwrap();
        names.push(name);
    }
    let project = AnalysisProject::try_create(project, &names, SubImports::All).unwrap();
    cli::index(&project, &[], models, false, cli::IndexOptions::default()).unwrap();
    project.dir().join("index")
}

/// The index of [`OPS_C`].
fn ops_index() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        index_project(
            "diff_index_ops_proj",
            &[(
                "diff_index_ops",
                "diff_index_ops.c",
                ArtifactLanguage::C,
                OPS_C,
            )],
            &[],
        )
    })
}

/// The index of the two Lua imports, joined by a bridge from `alpha_stub` to `gamma_impl`
/// whose callee port is a sub-path, which makes a bridge temporary.
fn bridge_index() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let (app, lib) = ("diff_index_bridge_app", "diff_index_bridge_lib");
        let generators = [
            serde_json::json!({
                "find": "methods",
                "in": {"import": app},
                "where": [{"constraint": "signature_match", "name": "alpha_stub"}],
                "model": {"bridge": {
                    "to": {"in": {"import": lib},
                           "where": [{"constraint": "signature_match", "name": "gamma_impl"}]},
                    "arguments": [
                        {"from": "Argument(0)", "to": "Argument(0).stack", "direction": "in"},
                        {"from": "Return", "to": "Return", "direction": "out"}
                    ]
                }}
            }),
            serde_json::json!({
                "find": "methods",
                "in": {"import": lib},
                "where": [{"constraint": "signature_match", "name": "gamma_impl"}],
                "model": {"propagation": [{"input": "Argument(0).stack", "output": "Return"}]}
            }),
        ];
        let model = store_dir().join("diff_index_bridge-models.jsonl");
        let text: String = generators.iter().map(|g| format!("{g}\n")).collect();
        std::fs::write(&model, text).unwrap();
        let dir = index_project(
            "diff_index_bridge_proj",
            &[
                (app, "diff_index_bridge_app.lua", ArtifactLanguage::Lua, BRIDGE_APP),
                (lib, "diff_index_bridge_lib.lua", ArtifactLanguage::Lua, BRIDGE_LIB),
            ],
            &[model],
        );
        let bridged = schema::actual_param::try_load(&dir)
            .unwrap()
            .iter()
            .any(|r| matches!(r.3.kind(), FlowVariableKind::Local(s) if s.as_str().starts_with("$bridge")));
        assert!(bridged, "the bridge fixture must pass a bridge temporary");
        dir
    })
}

fn fixtures() -> [&'static Path; 2] {
    [ops_index(), bridge_index()]
}

/// Every table `dir` holds.
fn tables_in(dir: &Path) -> Vec<Table> {
    let mut tables: Vec<Table> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| Table::from_filename(e.unwrap().file_name().to_str()?))
        .collect();
    tables.sort();
    tables
}

/// The diff of `a` and `b`, every differing row shown.
fn diff_all(a: &Path, b: &Path) -> Report {
    let opts = Options {
        limit: usize::MAX,
        ..Options::default()
    };
    diff_index(a, b, &opts).unwrap_or_else(|e| panic!("{e}"))
}

fn assert_identical(a: &Path, b: &Path, what: &str) {
    let report = diff_index(a, b, &Options::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        report.first_line(),
        "IDENTICAL",
        "{what}:\n{}",
        report.to_text()
    );
}

fn table(report: &Report, t: Table) -> &TableReport {
    report
        .tables
        .iter()
        .find(|r| r.table == t.name())
        .unwrap_or_else(|| panic!("no {} in the report", t.name()))
}

fn same<T>(x: T) -> T {
    x
}

fn nothing() {}

/// How a rewrite renumbers what a run numbers freely, each a bijection, and what it does after
/// each row. [`GLOBAL_FRAME`] is never renumbered: it is reserved.
struct Remap<'a> {
    func: &'a dyn Fn(FunctionId) -> FunctionId,
    insn: &'a dyn Fn(InsnId) -> InsnId,
    span: &'a dyn Fn(FileSpanId) -> FileSpanId,
    import: &'a dyn Fn(ImportId) -> ImportId,
    row_done: &'a dyn Fn(),
}

const IDENTITY: Remap<'static> = Remap {
    func: &same::<FunctionId>,
    insn: &same::<InsnId>,
    span: &same::<FileSpanId>,
    import: &same::<ImportId>,
    row_done: &nothing,
};

impl Remap<'_> {
    fn f(&self, f: FunctionId) -> FunctionId {
        if f == GLOBAL_FRAME { f } else { (self.func)(f) }
    }

    fn i(&self, i: InsnId) -> InsnId {
        (self.insn)(i)
    }

    fn span(&self, s: FileSpanId) -> FileSpanId {
        (self.span)(s)
    }

    fn import(&self, i: ImportId) -> ImportId {
        (self.import)(i)
    }

    fn var(&self, v: FlowVariable) -> FlowVariable {
        match v.kind() {
            FlowVariableKind::CallArg(packed) => {
                let arg: CallArgId = packed.try_into().unwrap();
                FlowVariable::call_actual(self.i(arg.insn_id), arg.formal())
            }
            _ => v,
        }
    }

    fn target(&self, t: CallTargetObject) -> CallTargetObject {
        match *t.kind() {
            CallTargetObjectKind::FunctionId(f) => {
                CallTargetObjectKind::FunctionId(self.f(f)).into()
            }
            _ => t,
        }
    }

    fn site(&self, s: PackedInsnSiteId) -> PackedInsnSiteId {
        let site = unpack_site(&s);
        PackedInsnSiteId::try_from_parts(self.f(site.func_id), self.i(site.insn_id)).unwrap()
    }

    fn edge(&self, e: FlowEdge) -> FlowEdge {
        match e {
            FlowEdge::Call(s) => FlowEdge::Call(self.site(s)),
            FlowEdge::Return(s) => FlowEdge::Return(self.site(s)),
            FlowEdge::Intra | FlowEdge::Global => e,
        }
    }

    fn endpoint(&self, e: QueryEndpoint) -> QueryEndpoint {
        QueryEndpoint {
            infunc: self.f(e.infunc),
            vertex: FlowVertex::new(self.var(e.vertex.0), e.vertex.1),
            call_site: e.call_site.map(|s| self.site(s)),
            ..e
        }
    }
}

/// For each table an index can hold, how [`Remap`] rewrites its row. Generates [`remap_table`]
/// and [`select_rows`].
macro_rules! index_tables {
    ($($t:ident: |$r:ident, $row:pat_param| $map:expr;)*) => {
        /// Rewrites each row of `table` in `dir` with `remap`, and saves the result if `save`.
        fn remap_table(dir: &Path, table: Table, remap: &Remap<'_>, save: bool) {
            match table.name() {
                $(stringify!($t) => {
                    let rows = schema::$t::try_load(dir).unwrap();
                    let mut out = Vec::with_capacity(rows.len());
                    for $row in rows {
                        let $r = remap;
                        out.push($map);
                        (remap.row_done)();
                    }
                    if save {
                        schema::$t::try_save(dir, out).unwrap();
                    }
                })*
                other => panic!("no rewrite for table {other}"),
            }
        }

        /// Keeps the rows of `table` in `dir` at the positions `pick` returns, given how many
        /// there are, in that order.
        fn select_rows(dir: &Path, table: Table, pick: &dyn Fn(usize) -> Vec<usize>) {
            match table.name() {
                $(stringify!($t) => {
                    let rows = schema::$t::try_load(dir).unwrap();
                    let out: Vec<_> = pick(rows.len()).into_iter().map(|i| rows[i].clone()).collect();
                    schema::$t::try_save(dir, out).unwrap();
                })*
                other => panic!("no rewrite for table {other}"),
            }
        }
    };
}

index_tables! {
    formal_param: |r, (f, i, t)| (r.f(f), i, t);
    actual_param: |r, (f, insn, i, v, p)| (r.f(f), r.i(insn), i, r.var(v), p);
    call: |r, (f, insn, t)| (r.f(f), r.i(insn), r.f(t));
    assign: |r, (f, dv, dp, sv, sp)| (r.f(f), r.var(dv), dp, r.var(sv), sp);
    call_target_assign: |r, (f, insn, v, p, t)| (r.f(f), r.i(insn), r.var(v), p, r.target(t));
    callee_info: |r, (f, insn, v, p, k)| (r.f(f), r.i(insn), r.var(v), p, k);
    callee_resolvents: |r, (o, k, t)| (r.target(o), k, r.f(t));
    resolved_call: |r, (f, insn, t)| (r.f(f), r.i(insn), r.f(t));
    summary: |r, (f, d, dp, s, sp)| (r.f(f), d, dp, s, sp);
    globals: |r, (g, import, span)| (g, r.import(import), span.map(|s| r.span(s)));
    global_flow: |_r, row| row;
    paths: |_r, row| row;
    taint: |r, (f, s, v, p, e)| (r.f(f), s, r.var(v), p, r.endpoint(e));
    taint_edge: |r, (e, sf, sv, sp, df, dv, dp)|
        (r.edge(e), r.f(sf), r.var(sv), sp, r.f(df), r.var(dv), dp);
    index_source_map: |r, (f, insn, span, import)| (r.f(f), r.i(insn), r.span(span), r.import(import));
    import_id: |r, (id, name)| (r.import(id), name);
    function_id: |r, (id, name)| (r.f(id), name);
    external_function: |r, (f,)| (r.f(f),);
    intent_pair: |r, (f, insn, receiver, k)| (r.f(f), r.i(insn), r.f(receiver), k);
}

/// Rewrites every table in `dir` with `remap`.
fn remap_index(dir: &Path, remap: &Remap<'_>) {
    for t in tables_in(dir) {
        remap_table(dir, t, remap, true);
    }
}

/// Which rows [`count_rows`] counts.
#[derive(Clone, Copy, PartialEq)]
enum Naming {
    /// Every row.
    All,
    /// Rows that name this function.
    Function(FunctionId),
    /// Rows that name this instruction, in a column or a call argument.
    Insn(InsnId),
    /// Rows that name this import.
    Import(ImportId),
}

/// How many rows of `tables` in `dir` match `naming`.
fn count_rows(dir: &Path, tables: &[Table], naming: Naming) -> usize {
    let (hit, rows) = (Cell::new(false), Cell::new(0));
    let mark = |n: Naming| hit.set(hit.get() || n == naming);
    let func = |f: FunctionId| {
        mark(Naming::Function(f));
        f
    };
    let insn = |i: InsnId| {
        mark(Naming::Insn(i));
        i
    };
    let import = |i: ImportId| {
        mark(Naming::Import(i));
        i
    };
    let done = || rows.set(rows.get() + usize::from(hit.replace(false) || naming == Naming::All));
    let remap = Remap {
        func: &func,
        insn: &insn,
        import: &import,
        row_done: &done,
        ..IDENTITY
    };
    for &t in tables {
        remap_table(dir, t, &remap, false);
    }
    rows.get()
}

/// Adds `taint` and `taint_edge`, which `index` does not write, with a row per call site, so
/// that renumbering reaches a `QueryEndpoint`, a `FlowEdge` and the call sites inside them.
fn add_taint_tables(dir: &Path) {
    let calls = schema::call::try_load(dir).unwrap();
    assert!(!calls.is_empty(), "{} has no calls", dir.display());
    let e = FactPath::empty();
    let mut taint = Vec::new();
    let mut edges = Vec::new();
    for (f, insn, target) in calls {
        let site = PackedInsnSiteId::try_from_parts(f, insn).unwrap();
        let arg = FlowVariable::call_actual(insn, FormalIndex::new(0));
        let endpoint = QueryEndpoint {
            infunc: f,
            vertex: FlowVertex::new(arg, e),
            label: Label(Str::from("L")),
            direction: TaintDirection::Forward,
            call_site: Some(site),
            saturating: false,
        };
        taint.push((f, TaintState::Free, arg, e, endpoint));
        edges.push((
            FlowEdge::Call(site),
            f,
            arg,
            e,
            target,
            FlowVariable::formal_index(FormalIndex::new(0)),
            e,
        ));
    }
    schema::taint::try_save(dir, taint).unwrap();
    schema::taint_edge::try_save(dir, edges).unwrap();
}

/// A fixed permutation of `0..n`.
fn shuffled(n: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    for i in (1..n).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        order.swap(i, (state >> 33) as usize % (i + 1));
    }
    order
}

#[test]
fn shuffled_rows() {
    for base in fixtures() {
        let (_tmp, copy) = copy_index(base);
        for t in tables_in(&copy) {
            select_rows(&copy, t, &shuffled);
        }
        let ids = |dir: &Path| -> Vec<u32> {
            schema::function_id::try_load(dir)
                .unwrap()
                .iter()
                .map(|r| r.0.id)
                .collect()
        };
        assert_ne!(ids(base), ids(&copy), "function_id must be shuffled too");
        assert_identical(base, &copy, "rows shuffled");
    }
}

#[test]
fn split_row_groups() {
    use arrow::record_batch::RecordBatchReader;
    use parquet::arrow::ArrowWriter;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::file::properties::WriterProperties;

    for base in fixtures() {
        let (_tmp, copy) = copy_index(base);
        let mut split = 0;
        for t in tables_in(&copy) {
            let path = copy.join(t.filename());
            let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap())
                .unwrap()
                .with_batch_size(2)
                .build()
                .unwrap();
            let schema = reader.schema();
            let batches: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
            let props = WriterProperties::builder()
                .set_max_row_group_row_count(Some(1))
                .build();
            let mut writer =
                ArrowWriter::try_new(File::create(&path).unwrap(), schema, Some(props)).unwrap();
            for batch in &batches {
                writer.write(batch).unwrap();
            }
            let meta = writer.close().unwrap();
            if meta.num_row_groups() > 1 {
                split += 1;
            }
        }
        assert!(split > 3, "only {split} tables were split");
        assert_identical(base, &copy, "row groups split");
    }
}

/// Each fixture, with taint tables added, is identical to a copy renumbered by `remap`.
/// `probe` reads a value from an index that the renumbering must change.
fn assert_renumbering_identical<T: PartialEq + Debug>(
    remap: &Remap<'_>,
    probe: impl Fn(&Path) -> T,
    what: &str,
) {
    for base in fixtures() {
        let (_tmp_a, a) = copy_index(base);
        add_taint_tables(&a);
        let (_tmp_b, b) = copy_index(&a);
        remap_index(&b, remap);
        assert_ne!(probe(&a), probe(&b), "{what}: nothing was renumbered");
        assert_identical(&a, &b, what);
    }
}

#[test]
fn renumbered_functions() {
    let has_function_target = schema::call_target_assign::try_load(ops_index())
        .unwrap()
        .iter()
        .any(|r| matches!(r.4.kind(), CallTargetObjectKind::FunctionId(_)));
    assert!(has_function_target, "a call target must name a function");
    let func = |f: FunctionId| FunctionId::new(f.id ^ 0x5555);
    assert_renumbering_identical(
        &Remap {
            func: &func,
            ..IDENTITY
        },
        |dir| schema::function_id::try_load(dir).unwrap()[0].0,
        "functions renumbered",
    );
}

#[test]
fn renumbered_insns_and_spans() {
    let insn = |i: InsnId| InsnId::new(i.id ^ 0x0555_5555);
    let span = |s: FileSpanId| FileSpanId(s.0 ^ 0x5555);
    let import = |i: ImportId| ImportId(i.0 ^ 0x55);
    assert_renumbering_identical(
        &Remap {
            insn: &insn,
            span: &span,
            import: &import,
            ..IDENTITY
        },
        |dir| schema::call::try_load(dir).unwrap()[0].1,
        "instructions, spans and imports renumbered",
    );
}

/// The tables an instruction's name is made from.
const INPUT_TABLES: [Table; 4] = [
    Table::Call,
    Table::ActualParam,
    Table::CallTargetAssign,
    Table::CalleeInfo,
];

/// The function and instruction of the first row of `table`, an input table.
fn first_input_row(dir: &Path, table: Table) -> (FunctionId, InsnId) {
    match table {
        Table::Call => schema::call::try_load(dir)
            .unwrap()
            .first()
            .map(|r| (r.0, r.1)),
        Table::ActualParam => schema::actual_param::try_load(dir)
            .unwrap()
            .first()
            .map(|r| (r.0, r.1)),
        Table::CallTargetAssign => schema::call_target_assign::try_load(dir)
            .unwrap()
            .first()
            .map(|r| (r.0, r.1)),
        Table::CalleeInfo => schema::callee_info::try_load(dir)
            .unwrap()
            .first()
            .map(|r| (r.0, r.1)),
        _ => panic!("{} is not an input table", table.name()),
    }
    .unwrap()
}

fn function_name(dir: &Path, f: FunctionId) -> String {
    schema::function_id::try_load(dir)
        .unwrap()
        .into_iter()
        .find(|r| r.0 == f)
        .map(|r| r.1.0.as_str().to_string())
        .unwrap()
}

/// Deleting one row is reported as that row. A row of an input table renames its instruction,
/// so every row about it, in every table, may differ, and nothing else does. A row of
/// `function_id` or `import_id` that another table names makes the diff fail instead.
#[test]
fn deleting_a_row_reports_it() {
    let mut deleted = 0;
    for base in fixtures() {
        for t in tables_in(base) {
            if count_rows(base, &[t], Naming::All) == 0 {
                continue;
            }
            let (_tmp, b) = copy_index(base);
            select_rows(&b, t, &|n| (1..n).collect());
            deleted += 1;
            let what = format!("deleting row 0 of {} in {}", t.name(), base.display());

            if t == Table::FunctionId || t == Table::ImportId {
                let deleted_id = match t {
                    Table::FunctionId => {
                        Naming::Function(schema::function_id::try_load(base).unwrap()[0].0)
                    }
                    _ => Naming::Import(schema::import_id::try_load(base).unwrap()[0].0),
                };
                let others: Vec<Table> = tables_in(base).into_iter().filter(|&o| o != t).collect();
                if count_rows(base, &others, deleted_id) > 0 {
                    let err = diff_index(base, &b, &Options::default())
                        .expect_err(&format!("{what}: an id no longer named must be an error"));
                    assert!(err.to_string().contains("is not in"), "{what}: {err}");
                    continue;
                }
            }

            let report = diff_all(base, &b);
            let text = report.to_text();
            if INPUT_TABLES.contains(&t) {
                let (f, insn) = first_input_row(base, t);
                let marker = format!("{}@", function_name(base, f));
                assert!(table(&report, t).differs(), "{what}:\n{text}");
                for d in report.differing() {
                    let dt = Table::from_name(&d.table).unwrap();
                    for s in d.more_in_a.rows.iter().chain(&d.more_in_b.rows) {
                        assert!(
                            s.row.contains(&marker),
                            "{what}: a differing row is not about {marker}: {}\n{text}",
                            s.row
                        );
                    }
                    assert!(
                        d.more_in_a.total() <= count_rows(base, &[dt], Naming::Insn(insn))
                            && d.more_in_b.total() <= count_rows(&b, &[dt], Naming::Insn(insn)),
                        "{what}: more rows of {} differ than name the instruction\n{text}",
                        d.table
                    );
                }
            } else {
                let differing: Vec<&str> = report.differing().map(|d| d.table.as_str()).collect();
                assert_eq!(differing, [t.name()], "{what}:\n{text}");
                let d = table(&report, t);
                assert_eq!(d.more_in_b.total(), 0, "{what}:\n{text}");
                assert_eq!(d.more_in_a.rows.len(), 1, "{what}:\n{text}");
                let s = &d.more_in_a.rows[0];
                assert_eq!(s.count_a, s.count_b + 1, "{what}:\n{text}");
                assert_eq!(d.only_in_a + d.count_differs, 1, "{what}:\n{text}");
            }
        }
    }
    assert!(deleted > 20, "only {deleted} tables had a row to delete");
}

/// The ops index with the targets of `apply(twice, b)` and `through(&o, r)` in `wrap` swapped,
/// in a temporary directory.
fn swapped_call_targets() -> (tempfile::TempDir, PathBuf) {
    let base = ops_index();
    let ids: Vec<(FunctionId, String)> = schema::function_id::try_load(base)
        .unwrap()
        .into_iter()
        .map(|(id, name)| (id, name.0.as_str().to_string()))
        .collect();
    let id = |name: &str| ids.iter().find(|r| r.1 == name).unwrap().0;
    // `apply(twice, b)` is the call to `apply` whose first argument is assigned `twice`.
    let passes_twice: Vec<InsnId> = schema::call_target_assign::try_load(base)
        .unwrap()
        .into_iter()
        .filter(|r| matches!(r.4.kind(), CallTargetObjectKind::FunctionId(f) if *f == id("twice")))
        .map(|r| r.1)
        .collect();
    let calls = schema::call::try_load(base).unwrap();
    let in_wrap = |target: &str, pred: &dyn Fn(InsnId) -> bool| -> InsnId {
        let found: Vec<InsnId> = calls
            .iter()
            .filter(|r| r.0 == id("wrap") && r.2 == id(target) && pred(r.1))
            .map(|r| r.1)
            .collect();
        assert_eq!(found.len(), 1, "one call to {target}: {calls:?}");
        found[0]
    };
    let apply_twice = in_wrap("apply", &|i| passes_twice.contains(&i));
    let through = in_wrap("through", &|_| true);

    let (tmp, b) = copy_index(base);
    let swapped: Vec<_> = calls
        .iter()
        .map(|&(f, insn, target)| match insn {
            i if i == apply_twice => (f, insn, id("through")),
            i if i == through => (f, insn, id("apply")),
            _ => (f, insn, target),
        })
        .collect();
    schema::call::try_save(&b, swapped).unwrap();
    (tmp, b)
}

/// Swapping the targets of two calls with different arguments renames both instructions:
/// `call` holds the two original rows only in A and the two swapped rows only in B, and every
/// other differing row is about one of the two.
#[test]
fn swapped_call_targets_reported() {
    let (_tmp, b) = swapped_call_targets();
    let report = diff_all(ops_index(), &b);
    let text = report.to_text();
    assert!(text.starts_with("DIFFERENT: "), "{text}");

    let call = table(&report, Table::Call);
    assert_eq!(
        (call.only_in_a, call.only_in_b, call.count_differs),
        (2, 2, 0),
        "{text}"
    );
    // A row is (caller, instruction, callee), and an instruction is printed as
    // `caller@callee(arguments)`.
    let rows = |side: &[Sample]| -> Vec<(String, String, (usize, usize))> {
        side.iter()
            .map(|s| {
                let cols: Vec<&str> = s.row.split('\t').collect();
                assert_eq!(cols.len(), 3, "{}", s.row);
                assert_eq!(cols[0], "wrap", "{}", s.row);
                (
                    cols[1].to_string(),
                    cols[2].to_string(),
                    (s.count_a, s.count_b),
                )
            })
            .collect()
    };
    let (only_a, only_b) = (rows(&call.more_in_a.rows), rows(&call.more_in_b.rows));
    for (insn, callee, counts) in &only_a {
        assert!(
            insn.starts_with(&format!("wrap@{callee}(")),
            "{insn}\t{callee}"
        );
        assert_eq!(*counts, (1, 0));
    }
    let mut callees: Vec<&str> = only_a.iter().map(|r| r.1.as_str()).collect();
    callees.sort_unstable();
    assert_eq!(callees, ["apply", "through"], "{text}");
    // Each swapped row calls the other callee with its original arguments.
    for (insn, callee, counts) in &only_b {
        assert!(
            insn.starts_with(&format!("wrap@{callee}(")),
            "{insn}\t{callee}"
        );
        assert_eq!(*counts, (0, 1));
        let args = &insn[insn.find('(').unwrap()..];
        let original = only_a
            .iter()
            .find(|r| r.0.ends_with(args))
            .unwrap_or_else(|| panic!("no original call passes {args}:\n{text}"));
        assert_ne!(&original.1, callee, "{text}");
    }
    assert_eq!(only_b.len(), 2, "{text}");

    for d in report.differing() {
        for s in d.more_in_a.rows.iter().chain(&d.more_in_b.rows) {
            assert!(
                s.row.contains("wrap@apply(") || s.row.contains("wrap@through("),
                "{}: a differing row is about neither instruction: {}",
                d.table,
                s.row
            );
        }
    }
}
