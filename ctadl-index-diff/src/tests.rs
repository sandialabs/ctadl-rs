//! The diff over hand-built index directories, written with the schema's `try_save`.

use std::path::Path as FsPath;

use source_info::FileSpanId;

use ctadl_index::facts::schema::{self, Table};
use ctadl_index::facts::{
    CallDispatchKey, CallDispatchKeyKind, CallTargetObject, CallTargetObjectKind, FlowEdge,
    FlowVariable, FlowVertex, FormalIndex, FormalType, Function, FunctionId, GLOBAL_FRAME,
    GLOBAL_FRAME_NAME, GlobalId, ImportId, InsnId, IntentPairKind, Label, PackedInsnSiteId, Path,
    QueryEndpoint, Root, Str, TaintDirection, TaintState,
};

use crate::canon::{CanonVar, FnName};
use crate::insn::{CanonInsn, InsnMap, SignatureInterner};
use crate::names::FunctionNames;
use crate::{Options, Report, Samples, Side, diff_index};

/// How a fixture numbers what a run numbers freely, and whether it writes rows backwards.
#[derive(Clone, Copy)]
struct Numbering {
    func: fn(u32) -> u32,
    insn: fn(u64) -> u64,
    span: fn(u32) -> u32,
    import: fn(u32) -> u32,
    reverse: bool,
}

const PLAIN: Numbering = Numbering {
    func: |x| x,
    insn: |x| x,
    span: |x| x,
    import: |x| x,
    reverse: false,
};

/// Every id renumbered, consistently across tables, and every table written backwards.
const SHUFFLED: Numbering = Numbering {
    func: |x| 1000 - 7 * x,
    insn: |x| 90_000 - 13 * x,
    span: |x| x * 31 + 4,
    import: |x| 5 - x,
    reverse: true,
};

const MAIN: u32 = 0;
const FOO: u32 = 1;
const BAR: u32 = 2;
const BAZ: u32 = 3;
const NAMES: [&str; 4] = ["main", "foo", "bar", "baz"];

fn path(s: &str) -> Path {
    Path::parse(s).unwrap()
}

fn local(s: &str) -> FlowVariable {
    FlowVariable::local(Str::from(s))
}

fn java_key() -> CallDispatchKey {
    CallDispatchKeyKind::Java(ctadl_ir::Symbol::from("m"), ctadl_ir::Symbol::from("()V")).into()
}

fn class(name: &str) -> CallTargetObject {
    CallTargetObjectKind::Symbol(ctadl_ir::Symbol::from(name)).into()
}

/// A small index with rows in every table, numbered by `n`.
#[derive(Clone)]
struct Fixture {
    formal_param: Vec<schema::formal_param::Record>,
    actual_param: Vec<schema::actual_param::Record>,
    call: Vec<schema::call::Record>,
    assign: Vec<schema::assign::Record>,
    call_target_assign: Vec<schema::call_target_assign::Record>,
    callee_info: Vec<schema::callee_info::Record>,
    callee_resolvents: Vec<schema::callee_resolvents::Record>,
    resolved_call: Vec<schema::resolved_call::Record>,
    summary: Vec<schema::summary::Record>,
    globals: Vec<schema::globals::Record>,
    global_flow: Vec<schema::global_flow::Record>,
    paths: Vec<schema::paths::Record>,
    taint: Vec<schema::taint::Record>,
    taint_edge: Vec<schema::taint_edge::Record>,
    index_source_map: Vec<schema::index_source_map::Record>,
    import_id: Vec<schema::import_id::Record>,
    function_id: Vec<schema::function_id::Record>,
    external_function: Vec<schema::external_function::Record>,
    intent_pair: Vec<schema::intent_pair::Record>,
    manifest_node: Vec<schema::manifest_node::Record>,
    manifest_node_child: Vec<schema::manifest_node_child::Record>,
    manifest_node_attr: Vec<schema::manifest_node_attr::Record>,
    reverse: bool,
}

/// Ids for one numbering.
struct Ids(Numbering);

impl Ids {
    fn f(&self, i: u32) -> FunctionId {
        FunctionId::new((self.0.func)(i))
    }
    fn i(&self, i: u64) -> InsnId {
        InsnId::new((self.0.insn)(i))
    }
    fn arg(&self, insn: u64, formal: i16) -> FlowVariable {
        FlowVariable::call_actual(self.i(insn), FormalIndex::new(formal))
    }
    fn site(&self, f: u32, insn: u64) -> PackedInsnSiteId {
        PackedInsnSiteId::try_from_parts(self.f(f), self.i(insn)).unwrap()
    }
    fn span(&self, s: u32) -> FileSpanId {
        FileSpanId((self.0.span)(s))
    }
    fn import(&self, i: u32) -> ImportId {
        ImportId((self.0.import)(i))
    }
    fn ptr(&self, f: u32) -> CallTargetObject {
        CallTargetObjectKind::FunctionId(self.f(f)).into()
    }
}

impl Fixture {
    fn new(n: Numbering) -> Fixture {
        let ids = Ids(n);
        let f = |i| ids.f(i);
        let i = |i| ids.i(i);
        let e = Path::empty();
        let endpoint = |site: Option<PackedInsnSiteId>| QueryEndpoint {
            infunc: f(MAIN),
            vertex: FlowVertex::new(ids.arg(10, 0), e),
            label: Label(Str::from("L")),
            direction: TaintDirection::Forward,
            call_site: site,
            saturating: false,
        };
        Fixture {
            formal_param: vec![
                (f(FOO), FormalIndex::new(0), FormalType::Shared),
                (f(BAR), FormalIndex::new(0), FormalType::Copied),
            ],
            // Instruction 10 calls foo, 11 calls bar through a receiver; 12, in foo, has no
            // input facts.
            actual_param: vec![
                (f(MAIN), i(10), FormalIndex::new(0), local("x"), e),
                (f(MAIN), i(10), FormalIndex::new(-1), ids.arg(10, -1), e),
                (f(MAIN), i(11), FormalIndex::new(0), local("y"), path(".f")),
            ],
            call: vec![(f(MAIN), i(10), f(FOO)), (f(MAIN), i(11), f(BAR))],
            assign: vec![
                (f(MAIN), local("a"), path(".f"), ids.arg(10, 0), e),
                (
                    f(FOO),
                    FlowVariable::formal_index(FormalIndex::new(0)),
                    e,
                    local("z"),
                    e,
                ),
                (
                    f(MAIN),
                    FlowVariable::global(GlobalId::new("g")),
                    e,
                    ids.arg(11, -1),
                    e,
                ),
            ],
            call_target_assign: vec![
                (f(MAIN), i(11), local("t"), e, ids.ptr(BAZ)),
                (f(MAIN), i(11), local("u"), e, class("C")),
            ],
            callee_info: vec![(f(MAIN), i(11), local("o"), e, java_key())],
            callee_resolvents: vec![
                (ids.ptr(BAZ), CallDispatchKey::c(), f(BAZ)),
                (class("C"), java_key(), f(BAR)),
            ],
            resolved_call: vec![(f(MAIN), i(11), f(BAR)), (f(FOO), i(12), f(BAZ))],
            summary: vec![
                (f(FOO), Root::from(-1i16), e, Root::from(0i16), path(".f")),
                (
                    f(BAR),
                    Root::global(GlobalId::new("g")),
                    e,
                    Root::from(0i16),
                    e,
                ),
            ],
            globals: vec![
                (GlobalId::new("g"), ids.import(0), Some(ids.span(5))),
                (GlobalId::new("h"), ids.import(1), None),
            ],
            global_flow: vec![(GlobalId::new("g"), e, GlobalId::new("h"), path(".f"))],
            paths: vec![(e,), (path(".f"),)],
            taint: vec![
                (
                    f(MAIN),
                    TaintState::Free,
                    ids.arg(10, 0),
                    e,
                    endpoint(Some(ids.site(MAIN, 10))),
                ),
                (
                    f(MAIN),
                    TaintState::Restricted,
                    local("x"),
                    e,
                    endpoint(None),
                ),
            ],
            taint_edge: vec![
                (
                    FlowEdge::Call(ids.site(MAIN, 10)),
                    f(MAIN),
                    ids.arg(10, 0),
                    e,
                    f(FOO),
                    FlowVariable::formal_index(FormalIndex::new(0)),
                    e,
                ),
                (
                    FlowEdge::Intra,
                    f(FOO),
                    FlowVariable::formal_index(FormalIndex::new(0)),
                    e,
                    f(FOO),
                    local("z"),
                    e,
                ),
                (
                    FlowEdge::Global,
                    f(FOO),
                    local("z"),
                    e,
                    GLOBAL_FRAME,
                    FlowVariable::global(GlobalId::new("g")),
                    e,
                ),
            ],
            index_source_map: vec![
                (f(MAIN), i(10), ids.span(1), ids.import(0)),
                (f(MAIN), i(11), ids.span(2), ids.import(0)),
                (f(FOO), i(12), ids.span(3), ids.import(1)),
            ],
            import_id: vec![
                (ids.import(0), "app".to_string()),
                (ids.import(1), "lib".to_string()),
            ],
            function_id: (0..4)
                .map(|k| (f(k), Function::from(Str::from(NAMES[k as usize]))))
                .collect(),
            external_function: vec![(f(BAZ),)],
            intent_pair: vec![(f(FOO), i(12), f(BAR), IntentPairKind::Explicit)],
            manifest_node: vec![(0, "manifest".to_string()), (1, "activity".to_string())],
            manifest_node_child: vec![(0, 1)],
            manifest_node_attr: vec![(1, "name".to_string(), "Main".to_string())],
            reverse: n.reverse,
        }
    }

    fn save(&self, dir: &FsPath) {
        fn put<R: Clone>(
            rows: &[R],
            reverse: bool,
            save: impl FnOnce(Vec<R>) -> Result<(), ctadl_index::error::Error>,
        ) {
            let mut rows = rows.to_vec();
            if reverse {
                rows.reverse();
            }
            save(rows).unwrap()
        }
        let r = self.reverse;
        macro_rules! save_all {
            ($($t:ident),*) => {$(
                put(&self.$t, r, |rows| schema::$t::try_save(dir, rows));
            )*};
        }
        save_all!(
            formal_param,
            actual_param,
            call,
            assign,
            call_target_assign,
            callee_info,
            callee_resolvents,
            resolved_call,
            summary,
            globals,
            global_flow,
            paths,
            taint,
            taint_edge,
            index_source_map,
            import_id,
            function_id,
            external_function,
            intent_pair,
            manifest_node,
            manifest_node_child,
            manifest_node_attr
        );
    }

    fn write(&self) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        self.save(dir.path());
        dir
    }
}

fn diff(a: &tempfile::TempDir, b: &tempfile::TempDir, opts: &Options) -> Report {
    diff_index(a.path(), b.path(), opts).unwrap()
}

/// The error diffing `a` and `b` fails with.
fn diff_err(a: &tempfile::TempDir, b: &tempfile::TempDir) -> String {
    diff_index(a.path(), b.path(), &Options::default())
        .unwrap_err()
        .to_string()
}

/// Asserts that `samples` shows exactly `want`, in order.
fn assert_shown(samples: &Samples, want: &[Path]) {
    let shown: Vec<&str> = samples.rows.iter().map(|s| s.row.as_str()).collect();
    let want: Vec<String> = want.iter().map(|p| p.to_string()).collect();
    assert_eq!(shown, want);
}

fn table<'r>(r: &'r Report, name: &str) -> &'r crate::TableReport {
    r.tables.iter().find(|t| t.table == name).unwrap()
}

fn only(t: Table) -> Options {
    Options {
        tables: vec![t.name().to_string()],
        ..Options::default()
    }
}

/// The report of the same diff with the sides swapped.
fn mirror(r: &Report) -> Report {
    let mut m = r.clone();
    std::mem::swap(&mut m.a, &mut m.b);
    for u in &mut m.not_compared {
        std::mem::swap(&mut u.in_a, &mut u.in_b);
    }
    for t in &mut m.tables {
        std::mem::swap(&mut t.in_a, &mut t.in_b);
        std::mem::swap(&mut t.rows_a, &mut t.rows_b);
        std::mem::swap(&mut t.only_in_a, &mut t.only_in_b);
        std::mem::swap(&mut t.more_in_a, &mut t.more_in_b);
        for s in t.more_in_a.rows.iter_mut().chain(&mut t.more_in_b.rows) {
            std::mem::swap(&mut s.count_a, &mut s.count_b);
        }
    }
    m
}

/// `dir`'s instruction map, built into `sigs`.
fn insn_map(dir: &FsPath, side: Side, sigs: &mut SignatureInterner) -> InsnMap {
    let fns = FunctionNames::load(dir, side, true).unwrap();
    InsnMap::build(dir, side, |t| dir.join(t.filename()).exists(), &fns, sigs).unwrap()
}

fn fn_name(s: &str) -> FnName {
    FnName(Str::from(s))
}

// Instructions.

/// Calls of `foo` with equal facts (20 and 21) share a name; one with a different argument
/// (22), or with more facts (10), does not.
#[test]
fn instructions_named_by_their_facts() {
    let mut fx = Fixture::new(PLAIN);
    for (insn, var) in [(20, "x"), (21, "x"), (22, "y")] {
        fx.call.push((
            FunctionId::new(MAIN),
            InsnId::new(insn),
            FunctionId::new(FOO),
        ));
        fx.actual_param.push((
            FunctionId::new(MAIN),
            InsnId::new(insn),
            FormalIndex::new(0),
            local(var),
            Path::empty(),
        ));
    }
    let dir = fx.write();
    let mut sigs = SignatureInterner::new();
    let map = insn_map(dir.path(), Side::A, &mut sigs);
    let name = |i| map.get(InsnId::new(i), fn_name("main"));
    assert_eq!(name(20), name(21));
    assert_ne!(name(20), name(22));
    assert_ne!(name(20), name(10));
}

#[test]
fn renumbered_functions_keep_names() {
    let a = Fixture::new(PLAIN).write();
    let mut fx = Fixture::new(Numbering {
        func: SHUFFLED.func,
        ..PLAIN
    });
    // `function_id.parquet` in an order that is neither by id nor by name.
    fx.function_id.swap(0, 2);
    fx.function_id.swap(1, 3);
    fx.function_id.swap(0, 3);
    let b = fx.write();
    let r = diff(&a, &b, &Options::default());
    assert!(r.identical(), "{}", r.to_text());
}

#[test]
fn two_sides_share_signatures() {
    let a = Fixture::new(PLAIN).write();
    let b = Fixture::new(SHUFFLED).write();
    let ids = Ids(SHUFFLED);
    let main = fn_name("main");
    let foo = fn_name("foo");
    let names = |first: &FsPath, second: &FsPath, plain_first: bool| {
        let mut sigs = SignatureInterner::new();
        let m1 = insn_map(first, Side::A, &mut sigs);
        let m2 = insn_map(second, Side::B, &mut sigs);
        let (plain, shuffled) = if plain_first { (m1, m2) } else { (m2, m1) };
        for (k, f) in [(10, main), (11, main), (12, foo)] {
            assert_eq!(
                plain.get(InsnId::new(k), f),
                shuffled.get(ids.i(k), f),
                "instruction {k}"
            );
        }
        let rendered: Vec<String> = [10, 11, 12]
            .iter()
            .map(|&k| {
                let cx = crate::render::Cx::new(&sigs);
                let mut s = String::new();
                crate::render::Col::col(
                    &plain.get(InsnId::new(k), if k == 12 { foo } else { main }),
                    &cx,
                    &mut s,
                );
                s
            })
            .collect();
        (sigs.len(), rendered)
    };
    let ab = names(a.path(), b.path(), true);
    let ba = names(b.path(), a.path(), false);
    assert_eq!(ab, ba);
    // The empty signature and those of instructions 10 and 11.
    assert_eq!(ab.0, 3);
    assert_eq!(ab.1[0], "main@foo(-1:call-arg(this, -1), 0:local(x))");
    assert_eq!(
        ab.1[1],
        "main@bar(0:local(y).f) local(t)=ptr<baz> local(u)=java<C> recv local(o) java_call<m()V>"
    );
    assert_eq!(ab.1[2], "foo@<no facts>");
}

#[test]
fn second_instruction_in_input_is_an_error() {
    let mut fx = Fixture::new(PLAIN);
    fx.actual_param.push((
        FunctionId::new(MAIN),
        InsnId::new(10),
        FormalIndex::new(1),
        Ids(PLAIN).arg(11, 0),
        Path::empty(),
    ));
    let err = diff_err(&fx.write(), &Fixture::new(PLAIN).write());
    assert!(err.contains("actual_param.parquet in A"), "{err}");
    assert!(
        err.contains("a call argument of instruction 11 in a row about instruction 10"),
        "{err}"
    );
}

#[test]
fn instruction_with_no_facts_has_empty_signature() {
    let mut fx = Fixture::new(PLAIN);
    let dir = fx.write();
    let mut sigs = SignatureInterner::new();
    let map = insn_map(dir.path(), Side::A, &mut sigs);
    assert_eq!(
        map.get(InsnId::new(12), fn_name("foo")),
        CanonInsn {
            function: fn_name("foo"),
            signature: SignatureInterner::EMPTY
        }
    );
    assert!(sigs.get(SignatureInterner::EMPTY).is_empty());

    // Two such instructions of one function share a name, so their rows differ only in count.
    let b = fx.write();
    fx.resolved_call
        .push((FunctionId::new(FOO), InsnId::new(13), FunctionId::new(BAZ)));
    let a = fx.write();
    let r = diff(&a, &b, &only(Table::ResolvedCall));
    let t = table(&r, "resolved_call");
    assert_eq!((t.only_in_a, t.only_in_b, t.count_differs), (0, 0, 1));
    let s = &t.more_in_a.rows[0];
    assert_eq!((s.count_a, s.count_b), (2, 1));
    assert_eq!(s.row, "foo\tfoo@<no facts>\tbaz");
}

#[test]
fn global_frame_is_named() {
    let dir = Fixture::new(PLAIN).write();
    let fns = FunctionNames::load(dir.path(), Side::A, true).unwrap();
    assert_eq!(fns.get(GLOBAL_FRAME).unwrap().as_str(), GLOBAL_FRAME_NAME);
    assert!(fns.get(FunctionId::new(77)).is_err());

    // `taint_edge` names the global frame; a row of it is printed with that name.
    let a = Fixture::new(PLAIN).write();
    let mut fx = Fixture::new(PLAIN);
    fx.taint_edge.pop();
    let b = fx.write();
    let r = diff(&a, &b, &only(Table::TaintEdge));
    let row = &table(&r, "taint_edge").more_in_a.rows[0].row;
    assert_eq!(row, "global\tfoo\tlocal(z)\t\t<globals>\t$g\t");
}

#[test]
fn duplicate_function_id_is_an_error() {
    let mut fx = Fixture::new(PLAIN);
    fx.function_id[2].0 = FunctionId::new(0);
    let err = diff_err(&fx.write(), &Fixture::new(PLAIN).write());
    assert!(err.contains("function_id.parquet in A, row 2"), "{err}");
    assert!(err.contains("function id 0 is repeated"), "{err}");
}

// Tables.

#[test]
fn every_table_shuffled_is_identical() {
    let a = Fixture::new(PLAIN).write();
    let b = Fixture::new(SHUFFLED).write();
    let r = diff(&a, &b, &Options::default());
    assert!(r.identical(), "{}", r.to_text());
    for &t in Table::ALL {
        let tr = table(&r, t.name());
        assert!(tr.rows_a > 0 && tr.rows_a == tr.rows_b, "{}", t.name());
        let r = diff(&a, &b, &only(t));
        assert!(r.identical(), "{}", r.to_text());
        assert_eq!(r.tables.len(), 1);
    }
}

#[test]
fn multiplicity_is_reported() {
    let mut fx = Fixture::new(PLAIN);
    let b = fx.write();
    fx.call
        .push((FunctionId::new(MAIN), InsnId::new(10), FunctionId::new(FOO)));
    let a = fx.write();
    let r = diff(&a, &b, &Options::default());
    let t = table(&r, "call");
    assert_eq!((t.rows_a, t.rows_b), (3, 2));
    assert_eq!((t.only_in_a, t.only_in_b, t.count_differs), (0, 0, 1));
    assert_eq!(t.more_in_a.rows.len(), 1);
    let s = &t.more_in_a.rows[0];
    assert_eq!((s.count_a, s.count_b), (2, 1));
    assert_eq!(
        r.first_line(),
        format!("DIFFERENT: 1 of {} tables differ: call", Table::ALL.len())
    );
}

#[test]
fn call_arg_inside_assign_is_canonical() {
    let a = Fixture::new(PLAIN).write();
    let b = Fixture::new(Numbering {
        insn: SHUFFLED.insn,
        ..PLAIN
    })
    .write();
    assert!(diff(&a, &b, &only(Table::Assign)).identical());

    let mut fx = Fixture::new(PLAIN);
    fx.assign.remove(0);
    let b = fx.write();
    let r = diff(&a, &b, &only(Table::Assign));
    let t = table(&r, "assign");
    assert_eq!(
        t.more_in_a.rows[0].row,
        "main\tlocal(a)\t.f\tcall-arg(main@foo(-1:call-arg(this, -1), 0:local(x)), 0)\t"
    );
}

/// `assign`, the largest table, holds two variables a row.
#[test]
fn canon_var_stays_small() {
    assert_eq!(std::mem::size_of::<CanonVar>(), 12);
}

#[test]
fn function_target_inside_cto_is_canonical() {
    let a = Fixture::new(PLAIN).write();
    let b = Fixture::new(Numbering {
        func: SHUFFLED.func,
        ..PLAIN
    })
    .write();
    for t in [Table::CallTargetAssign, Table::CalleeResolvents] {
        assert!(diff(&a, &b, &only(t)).identical(), "{}", t.name());
    }

    // Pointing the target at another function is a difference, printed by name.
    let mut fx = Fixture::new(PLAIN);
    fx.callee_resolvents[0].0 = Ids(PLAIN).ptr(BAR);
    let b = fx.write();
    let r = diff(&a, &b, &only(Table::CalleeResolvents));
    let t = table(&r, "callee_resolvents");
    assert_eq!((t.only_in_a, t.only_in_b), (1, 1));
    assert_eq!(t.more_in_a.rows[0].row, "ptr<baz>\tc_call\tbaz");
    assert_eq!(t.more_in_b.rows[0].row, "ptr<bar>\tc_call\tbaz");
}

#[test]
fn span_ids_dropped() {
    let a = Fixture::new(PLAIN).write();
    let mut fx = Fixture::new(Numbering {
        span: SHUFFLED.span,
        ..PLAIN
    });
    fx.globals[0].2 = None;
    let b = fx.write();
    let r = diff(&a, &b, &Options::default());
    assert!(r.identical(), "{}", r.to_text());
}

/// A fixture whose `paths` is `rows`.
fn with_paths(rows: &[Path]) -> tempfile::TempDir {
    let mut fx = Fixture::new(PLAIN);
    fx.paths = rows.iter().map(|p| (*p,)).collect();
    fx.write()
}

/// `n` paths `.{prefix}{i}`, interned in the reverse of their content order, so that their id
/// order is not their content order.
fn paths(prefix: &str, n: usize) -> Vec<Path> {
    let mut ps: Vec<Path> = (0..n)
        .rev()
        .map(|i| path(&format!(".{prefix}{i:05}")))
        .collect();
    ps.reverse();
    ps
}

#[test]
fn sample_limit_and_remainder() {
    let ps = paths("sample", 30);
    let a = with_paths(&ps);
    let b = with_paths(&[]);
    let r = diff(&a, &b, &only(Table::Paths));
    let t = table(&r, "paths");
    assert_eq!((t.only_in_a, t.only_in_b), (30, 0));
    assert_eq!(t.more_in_a.not_shown, 10);
    assert_shown(&t.more_in_a, &ps[..20]);
    assert!(
        r.to_text()
            .contains("  more in A: 30 rows, 10 not shown\n    [A 1, B 0] .sample00000\n")
    );
}

#[test]
fn all_rows_differ() {
    let pa = paths("a", 10_000);
    let pb = paths("b", 10_000);
    let a = with_paths(&pa);
    let b = with_paths(&pb);
    let r = diff(&a, &b, &only(Table::Paths));
    let t = table(&r, "paths");
    assert_eq!((t.rows_a, t.rows_b), (10_000, 10_000));
    assert_eq!(
        (t.only_in_a, t.only_in_b, t.count_differs),
        (10_000, 10_000, 0)
    );
    for (samples, ps) in [(&t.more_in_a, &pa), (&t.more_in_b, &pb)] {
        assert_eq!(samples.not_shown, 10_000 - 20);
        assert_shown(samples, &ps[..20]);
    }
}

// The driver.

#[test]
fn identical_first_line() {
    let a = Fixture::new(PLAIN).write();
    let b = Fixture::new(PLAIN).write();
    let r = diff(&a, &b, &Options::default());
    let text = r.to_text();
    assert_eq!(text.lines().next(), Some("IDENTICAL"));
    assert!(text.contains("\ncall: same, 2 rows\n"), "{text}");
    let json: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    assert_eq!(json["result"], "IDENTICAL");
    assert_eq!(json["tables_differ"], 0);
}

#[test]
fn unknown_parquet_listed() {
    let a = Fixture::new(PLAIN).write();
    let b = Fixture::new(PLAIN).write();
    std::fs::copy(
        a.path().join(schema::paths::FILENAME),
        a.path().join("update_sans_path.parquet"),
    )
    .unwrap();
    std::fs::write(a.path().join("profile.json"), "{").unwrap();
    let r = diff(&a, &b, &Options::default());
    assert!(r.identical(), "{}", r.to_text());
    assert_eq!(r.not_compared.len(), 1);
    let u = &r.not_compared[0];
    assert_eq!(
        (u.file.as_str(), u.in_a, u.in_b),
        ("update_sans_path.parquet", true, false)
    );
    assert!(
        r.to_text()
            .contains("not compared, no table is stored in it: update_sans_path.parquet in A\n")
    );
}

#[test]
fn table_on_one_side() {
    let a = Fixture::new(PLAIN).write();
    let b = Fixture::new(PLAIN).write();
    std::fs::remove_file(b.path().join(schema::intent_pair::FILENAME)).unwrap();
    let r = diff(&a, &b, &Options::default());
    assert!(!r.identical());
    let t = table(&r, "intent_pair");
    assert_eq!((t.in_a, t.in_b, t.rows_a, t.rows_b), (true, false, 1, 0));
    assert_eq!(
        r.first_line(),
        format!(
            "DIFFERENT: 1 of {} tables differ: intent_pair",
            Table::ALL.len()
        )
    );
    assert!(r.to_text().contains(
        "intent_pair: only in A, A 1 rows, B no table; 1 only in A, 0 only in B, 0 in both with different counts\n"
    ));

    // An empty table on the other side is a difference in rows, not in presence.
    let mut fx = Fixture::new(PLAIN);
    fx.intent_pair.clear();
    let b = fx.write();
    let t = diff(&a, &b, &Options::default());
    let t = table(&t, "intent_pair");
    assert_eq!((t.in_a, t.in_b, t.only_in_a), (true, true, 1));
}

/// Two fixtures that differ in several tables, in several ways.
fn differing_pair() -> (tempfile::TempDir, tempfile::TempDir) {
    let mut fa = Fixture::new(PLAIN);
    fa.call
        .push((FunctionId::new(MAIN), InsnId::new(10), FunctionId::new(FOO)));
    fa.paths.push((path(".only_a"),));
    let mut fb = Fixture::new(SHUFFLED);
    fb.assign.remove(0);
    fb.paths.push((path(".only_b"),));
    fb.import_id.push((ImportId(77), "extra".to_string()));
    let a = fa.write();
    let b = fb.write();
    std::fs::remove_file(a.path().join(schema::manifest_node::FILENAME)).unwrap();
    std::fs::write(b.path().join("stale.parquet"), "").unwrap();
    (a, b)
}

#[test]
fn swap_is_mirror() {
    let (a, b) = differing_pair();
    let ab = diff(&a, &b, &Options::default());
    let ba = diff(&b, &a, &Options::default());
    assert!(!ab.identical());
    assert_eq!(mirror(&ab), ba);
    assert_eq!(mirror(&ab).to_text(), ba.to_text());
    assert_eq!(mirror(&ab).to_json(), ba.to_json());
}

#[test]
fn output_is_deterministic() {
    let (a, b) = differing_pair();
    let opts = Options {
        limit: 1,
        ..Options::default()
    };
    let first = diff(&a, &b, &opts);
    let second = diff(&a, &b, &opts);
    assert_eq!(first.to_text(), second.to_text());
    assert_eq!(first.to_json(), second.to_json());
}

#[test]
fn samples_independent_of_load_order() {
    // The same contents, with every table written forwards on one run and backwards on the
    // other, give the same report.
    let ps = paths("load", 40);
    let mut rev = ps.clone();
    rev.reverse();
    let a1 = with_paths(&ps);
    let a2 = with_paths(&rev);
    let b = with_paths(&[]);
    let opts = Options {
        limit: 5,
        ..Options::default()
    };
    let r1 = diff(&a1, &b, &opts);
    let mut r2 = diff(&a2, &b, &opts);
    r2.a = r1.a.clone();
    assert_eq!(r1, r2);
    assert_shown(&table(&r1, "paths").more_in_a, &ps[..5]);
}

#[test]
fn table_filter_named() {
    let (a, b) = differing_pair();
    let opts = Options {
        tables: vec!["paths".to_string(), "call.parquet".to_string()],
        ..Options::default()
    };
    let r = diff(&a, &b, &opts);
    let names: Vec<&str> = r.tables.iter().map(|t| t.table.as_str()).collect();
    assert_eq!(names, ["call", "paths"]);
    assert_eq!(r.filters.tables, ["call", "paths"]);
    let text = r.to_text();
    assert!(text.contains("\nfilter: tables call, paths\n"), "{text}");
    let json: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    assert_eq!(
        json["filters"]["tables"],
        serde_json::json!(["call", "paths"])
    );

    let bad = Options {
        tables: vec!["no_such_table".to_string()],
        ..Options::default()
    };
    let err = diff_index(a.path(), b.path(), &bad).unwrap_err();
    assert!(err.to_string().contains("no_such_table"));
}

#[test]
fn function_filter_named() {
    let (a, b) = differing_pair();
    let opts = Options {
        function: Some("fo".to_string()),
        ..Options::default()
    };
    let r = diff(&a, &b, &opts);
    // Only `foo`'s rows are compared; `main`'s extra call and missing assignment are not, and
    // `paths`, which is about no function, holds no rows. A table on one side only still
    // differs.
    let differing: Vec<&str> = r.differing().map(|t| t.table.as_str()).collect();
    assert_eq!(differing, ["manifest_node"], "{}", r.to_text());
    assert_eq!(table(&r, "formal_param").rows_a, 1);
    assert_eq!(table(&r, "call").rows_a, 0);
    assert_eq!(table(&r, "assign").rows_a, 1);
    assert_eq!(table(&r, "paths").rows_a, 0);
    assert_eq!(table(&r, "function_id").rows_a, 1);
    assert!(
        r.to_text()
            .contains("\nfilter: functions whose name contains \"fo\"\n")
    );
    let json: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    assert_eq!(json["filters"]["function"], "fo");
}

#[test]
fn json_matches_text() {
    let (a, b) = differing_pair();
    let r = diff(&a, &b, &Options::default());
    let text = r.to_text();
    let json: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    assert_eq!(json["result"], "DIFFERENT");
    assert!(text.starts_with("DIFFERENT: "));
    assert_eq!(json["tables_differ"], r.differing().count());
    assert_eq!(json["a"], r.a);
    assert_eq!(json["not_compared"][0]["file"], "stale.parquet");
    let tables = json["tables"].as_array().unwrap();
    assert_eq!(tables.len(), r.tables.len());
    for (j, t) in tables.iter().zip(&r.tables) {
        assert_eq!(j["table"], t.table);
        for (key, n) in [
            ("rows_a", t.rows_a),
            ("rows_b", t.rows_b),
            ("only_in_a", t.only_in_a),
            ("only_in_b", t.only_in_b),
            ("count_differs", t.count_differs),
        ] {
            assert_eq!(j[key], n, "{} {key}", t.table);
        }
        for side in ["more_in_a", "more_in_b"] {
            for s in j[side]["rows"].as_array().unwrap() {
                let line = format!(
                    "    [A {}, B {}] {}\n",
                    s["count_a"],
                    s["count_b"],
                    s["row"].as_str().unwrap()
                );
                assert!(text.contains(&line), "{line}");
            }
        }
    }
}

#[test]
fn corrupt_table_is_an_error() {
    let a = Fixture::new(PLAIN).write();
    let b = Fixture::new(PLAIN).write();
    let file = a.path().join(schema::call::FILENAME);
    let bytes = std::fs::read(&file).unwrap();
    std::fs::write(&file, &bytes[..bytes.len() / 2]).unwrap();
    let err = diff_err(&a, &b);
    assert!(err.starts_with("loading call in A ("), "{err}");
}
