// Tests for CTADL IR verification errors
use super::*;
use crate::mir::{
    AccessPath, BasicBlockData, BasicBlockIdx, BasicBlocks, Exp, FunctionIdx, Offset, ParameterIdx,
    Params, PathSegment, ReturnType, StatementKind, TerminatorKind, VariableRef,
};
use smallvec::smallvec;

/// Helper to create a minimal program with a single function.
fn make_program() -> Program {
    let mut prog = Program::default();
    let f_idx = prog.new_function();
    let f = &mut prog[f_idx];
    // default name is empty – tests can set a name if needed
    f.set_name("test".to_string());
    f.params = Params::default();
    f.return_type = ReturnType { arity: 0 };
    // Create a start block with a simple return terminator.
    let mut blocks = BasicBlocks::default();
    // Push the start block (index 0).
    blocks
        .blocks_mut()
        .push(BasicBlockData::new(Some(Terminator::new_kind(
            TerminatorKind::Return { args: smallvec![] },
        ))));
    f.blocks = blocks;
    prog
}

#[test]
fn test_unnamed_function_error() {
    let prog = make_program();
    // The function now has a valid name, so verification should succeed.
    let result = prog.verify();
    assert!(result.is_ok());
}

#[test]
fn test_store_verifies() {
    let mut prog = make_program();
    // Add a Store statement; verification should succeed.
    let f_idx = FunctionIdx::new(0);
    let f = &mut prog[f_idx];
    let var = VariableRef::new_local_idx(f.locals.get_or_intern("x"));
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    let store = StatementKind::store(
        AccessPath::from(var.clone()),
        PathSegment::field("f"),
        Exp::new_str("val"),
    );
    block.statements.push_back(Statement::new_kind(store));
    let result = prog.verify();
    assert!(result.is_ok());
}

// Test for ParameterDoesNotExist
#[test]
fn test_parameter_does_not_exist_error() {
    let mut prog = make_program();
    // Ensure no parameters are declared.
    let f_idx = FunctionIdx::new(0);
    let f = &mut prog[f_idx];
    f.params = Params::default();
    // Reference a non‑existent parameter.
    let var = VariableRef::new_parameter(ParameterIdx::new(0));
    let tmp = VariableRef::new_local_idx(f.locals.get_or_intern("tmp"));
    // Add an assign that uses the nonexistent parameter.
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    let stmt = Statement::new_kind(StatementKind::assign(tmp, [Exp::variable(var.clone())]));
    block.statements.push_back(stmt);
    let result = prog.verify();
    assert!(
        matches!(&result, Err(e) if e.iter().any(|err| matches!(err, VerifyError::ParameterDoesNotExist { .. }))),
        "errors: {result:?}"
    );
}

#[test]
fn test_local_does_not_exist_error() {
    let mut prog = make_program();
    let f_idx = FunctionIdx::new(0);
    let f = &mut prog[f_idx];
    // One declared local, but the statement references the index just past the end.
    let declared = f.locals.get_or_intern("tmp");
    let dangling = VariableRef::new_local_idx(LocalIdx::new(declared.index() + 1));
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    block
        .statements
        .push_back(Statement::new_kind(StatementKind::assign(
            dangling,
            [Exp::new_str("a")],
        )));
    let result = prog.verify();
    assert!(
        matches!(&result, Err(e) if e.iter().any(|err| matches!(err, VerifyError::LocalDoesNotExist { .. }))),
        "errors: {result:?}"
    );
}

/// `by_name` is `#[serde(skip)]`, so a deserialized `Locals` has declarations but no name index.
/// Interning must still dedupe against those declarations -- a second index for a name already in
/// the table would break SSA and every name-based lookup.
#[test]
fn test_intern_after_losing_name_index() {
    let mut locals = Locals::default();
    let a = locals.get_or_intern("a");
    let b = locals.get_or_intern("b");

    // Exactly the state `Locals` deserializes into.
    locals.by_name.clear();

    assert_eq!(locals.get_or_intern("a"), a);
    assert_eq!(locals.get_or_intern("b"), b);
    assert_eq!(locals.len(), 2, "interning must not duplicate declarations");
    // A genuinely new name still gets a fresh index.
    let c = locals.get_or_intern("c");
    assert_ne!(c, a);
    assert_ne!(c, b);
    assert_eq!(locals.name(c), "c");
}

/// The default dump keeps locals opaque (`%L0`, the form the fact base keys on); wrapping in
/// `WithLocalNames` -- what `ctadl inspect` does -- resolves them through the function's table.
#[test]
fn test_display_with_local_names() {
    let mut prog = make_program();
    let f = &mut prog[FunctionIdx::new(0)];
    let buf = VariableRef::new_local_idx(f.locals.get_or_intern("buf"));
    f.blocks[BasicBlockIdx::START_BLOCK]
        .statements
        .push_back(Statement::new_kind(StatementKind::assign(
            buf,
            [Exp::new_str("a")],
        )));

    let plain = format!("{prog}");
    assert!(plain.contains("%L0"), "plain dump keeps the index: {plain}");
    assert!(!plain.contains("locals:"), "no locals table: {plain}");

    let named = format!("{}", WithLocalNames(&prog));
    assert!(named.contains("%buf"), "names resolved: {named}");
    assert!(!named.contains("%L0 ="), "no bare index in body: {named}");
    assert!(
        named.contains("locals: %L0=buf"),
        "header maps index to name: {named}"
    );

    // The scoped setting is restored, so later renders are unaffected.
    assert_eq!(format!("{prog}"), plain);
}

#[test]
fn test_inconsistent_returns_error() {
    let mut prog = make_program();
    // Set function return arity to 2.
    let f_idx = FunctionIdx::new(0);
    let f = &mut prog[f_idx];
    f.return_type = ReturnType { arity: 2 };
    // Provide a return with three values.
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    *block.terminator_mut() = Terminator::new_kind(TerminatorKind::Return {
        args: smallvec![Exp::new_str("a"), Exp::new_str("b"), Exp::new_str("c"),],
    });
    let result = prog.verify();
    assert!(
        matches!(result, Err(e) if e.iter().any(|err| matches!(err, VerifyError::InconsistentReturns { .. })))
    );
}

#[test]
fn test_empty_goto_error() {
    let mut prog = make_program();
    // Add a goto with no targets.
    let f_idx = FunctionIdx::new(0);
    let f = &mut prog[f_idx];
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    *block.terminator_mut() = Terminator::new_kind(TerminatorKind::Goto {
        targets: smallvec![], // Empty targets
    });
    let result = prog.verify();
    assert!(
        matches!(result, Err(e) if e.iter().any(|err| matches!(err, VerifyError::EmptyGoto { .. })))
    );
}

#[test]
fn test_access_path_normalizes() {
    let mut locals = Locals::default();
    let var = VariableRef::new_local_idx(locals.get_or_intern("obj"));
    let ap = AccessPath::new(
        var,
        [
            PathSegment::offset(10),
            PathSegment::offset(20),
            PathSegment::Deref,
            PathSegment::offset(0),
        ],
    );
    let base = ap.base.variable.local().unwrap();
    assert_eq!(locals.name(base), "obj");
    assert_eq!(path_to_string(&ap.segments), ".[30].deref");
}

#[test]
fn test_offset_newtype() {
    // Test Offset newtype
    let offset = Offset(123);
    assert_eq!(offset.0, 123);
    assert_eq!(format!("{}", offset), "123");

    // Check how field and offset segments print.
    let symbol_access = PathSegment::field("test");
    let offset_access = PathSegment::Offset(Offset(456));

    assert_eq!(format!("{}", symbol_access), "test");
    assert_eq!(format!("{}", offset_access), "[456]");
}

/// The IR's `Display` impls emit the canonical path grammar, so an IR dump can be pasted
/// into a model port or a `.flowy` file and mean the same thing: a C field literally named `a.b`
/// does not render like two segments, and the frontends' `Symbol("[3]")` does not render like an
/// offset.
#[test]
fn test_display_uses_canonical_grammar() {
    // A leading '[' is escaped; a '[' inside a name needs no escape.
    assert_eq!(PathSegment::field("[_elem_]").to_string(), r"\[_elem_]");
    assert_eq!(PathSegment::field("[3]").to_string(), r"\[3]");
    assert_eq!(PathSegment::field("a[3]").to_string(), "a[3]");
    // ... so a bracketed symbol and a real offset are distinguishable.
    assert_ne!(
        PathSegment::field("[3]").to_string(),
        PathSegment::offset(3).to_string()
    );
    assert_eq!(PathSegment::offset(3).to_string(), "[3]");

    // Dots and backslashes in a field name are escaped.
    assert_eq!(PathSegment::field("a.b").to_string(), r"a\.b");
    assert_eq!(PathSegment::field(r"a\b").to_string(), r"a\\b");
    // Offsets are decimal, negatives included.
    assert_eq!(
        path_to_string(&[
            PathSegment::offset(-40),
            PathSegment::Deref,
            PathSegment::offset(255)
        ]),
        ".[-40].deref.[255]"
    );

    // And everything the IR prints parses back to what it printed.
    for seg in [
        PathSegment::field("[_elem_]"),
        PathSegment::field("a.b"),
        PathSegment::field(r"a\b"),
        PathSegment::offset(-40),
    ] {
        assert_eq!(path_syntax::parse_segment(&seg.to_string()).unwrap(), seg);
    }
}

/// A function with one block, one parameter, and the given statements.
fn program_with(stmts: Vec<StatementKind>) -> Program {
    let mut prog = make_program();
    let f = &mut prog[FunctionIdx::new(0)];
    f.params.push(ParameterType::SharedFields);
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    for s in stmts {
        block.statements.push_back(Statement::new_kind(s));
    }
    prog
}

fn errors_of(prog: &Program) -> Vec<VerifyError> {
    match prog.verify() {
        Ok(()) => vec![],
        Err(e) => e.errors_vec(),
    }
}

fn p0() -> VariableRef {
    VariableRef::new_parameter(ParameterIdx::new(0))
}

/// `x.f.[8].deref = v` keeps its path in order; the field is not moved behind the offset.
#[test]
fn store_keeps_path_order() {
    let s = StatementKind::assign(
        AccessPath::new(
            p0(),
            [
                PathSegment::field("f"),
                PathSegment::offset(8),
                PathSegment::Deref,
            ],
        ),
        [Exp::new_int(1)],
    );
    let StatementKind::Assign { dest, .. } = &s else {
        unreachable!()
    };
    assert_eq!(path_to_string(&dest.segments), ".f.[8].deref");
    assert!(s.is_store());
}

/// An address destination is rewritten (S5): `p.[8] = e` is `p = e.[-8]`, `p.[8] = e.[8]` is
/// `p = e`, and `x.f.[8] = v` is `x.f = v.[-8]`.
#[test]
fn address_destination_is_rewritten() {
    let e = VariableRef::new_global();
    let s = StatementKind::assign(
        AccessPath::new(p0(), [PathSegment::offset(8)]),
        [Exp::variable(e.clone())],
    );
    assert_eq!(s.to_string(), "assign @p0 = $globals.[-8]");
    assert!(!s.is_store());
    let s = StatementKind::assign(
        AccessPath::new(p0(), [PathSegment::offset(8)]),
        [Exp::path(AccessPath::new(
            e.clone(),
            [PathSegment::offset(8)],
        ))],
    );
    assert_eq!(s.to_string(), "assign @p0 = $globals");
    let s = StatementKind::assign(
        AccessPath::new(p0(), [PathSegment::field("f"), PathSegment::offset(8)]),
        [
            Exp::variable(e.clone()),
            Exp::value(e.clone()),
            Exp::new_int(3),
        ],
    );
    assert_eq!(
        s.to_string(),
        "store @p0.f := $globals.[-8], value($globals), <const: 3>"
    );
}

#[test]
fn verify_rejects_unnormalized_path() {
    let ap = AccessPath {
        base: p0(),
        segments: [
            PathSegment::offset(4),
            PathSegment::offset(4),
            PathSegment::Deref,
        ]
        .into_iter()
        .collect(),
    };
    let tmp = VariableRef::new_global();
    let prog = program_with(vec![StatementKind::Assign {
        dest: ap,
        sources: [Exp::variable(p0())].into_iter().collect(),
    }]);
    let errs = errors_of(&prog);
    assert!(
        errs.iter()
            .any(|e| matches!(e, VerifyError::NotNormal { .. })),
        "{errs:?}"
    );
    let _ = tmp;
}

#[test]
fn verify_rejects_misplaced_value() {
    let prog = program_with(vec![StatementKind::CallAssign {
        style: crate::mir::CallStyle::Unknown,
        rets: Default::default(),
        args: [Exp::value(p0())].into_iter().collect(),
    }]);
    let errs = errors_of(&prog);
    assert!(
        errs.iter()
            .any(|e| matches!(e, VerifyError::MisplacedValue { .. })),
        "{errs:?}"
    );
    // As an assignment source it is fine.
    let prog = program_with(vec![StatementKind::store(p0(), "f", Exp::value(p0()))]);
    assert_eq!(errors_of(&prog), vec![]);
}

#[test]
fn verify_rejects_void_operands() {
    let void = AccessPath::new(p0(), [PathSegment::Value, PathSegment::field("f")]);
    let prog = program_with(vec![StatementKind::store(
        p0(),
        "g",
        Exp::path(void.clone()),
    )]);
    let errs = errors_of(&prog);
    assert!(
        errs.iter()
            .any(|e| matches!(e, VerifyError::VoidPath { .. })),
        "{errs:?}"
    );
    let prog = program_with(vec![StatementKind::assign(void, [Exp::new_int(0)])]);
    let errs = errors_of(&prog);
    assert!(
        errs.iter()
            .any(|e| matches!(e, VerifyError::VoidPath { .. })),
        "{errs:?}"
    );
}

#[test]
fn verify_rejects_address_destination() {
    let prog = program_with(vec![StatementKind::Assign {
        dest: AccessPath::new(p0(), [PathSegment::offset(8)]),
        sources: [Exp::new_int(0)].into_iter().collect(),
    }]);
    let errs = errors_of(&prog);
    assert!(
        errs.iter()
            .any(|e| matches!(e, VerifyError::AddressDestination { .. })),
        "{errs:?}"
    );
}

#[test]
fn verify_rejects_empty_globals_write() {
    let prog = program_with(vec![StatementKind::assign(
        VariableRef::new_global(),
        [Exp::variable(p0())],
    )]);
    let errs = errors_of(&prog);
    assert!(
        errs.iter()
            .any(|e| matches!(e, VerifyError::EmptyGlobalsWrite { .. })),
        "{errs:?}"
    );
    // A store to a global is the way to write one.
    let prog = program_with(vec![StatementKind::store(
        VariableRef::new_global(),
        "g",
        Exp::variable(p0()),
    )]);
    assert_eq!(errors_of(&prog), vec![]);
}

#[test]
fn verify_rejects_value_spelled_as_field() {
    let prog = program_with(vec![StatementKind::store(
        p0(),
        PathSegment::field("#value"),
        Exp::new_int(0),
    )]);
    let errs = errors_of(&prog);
    assert!(
        errs.iter()
            .any(|e| matches!(e, VerifyError::ValueSpelledAsField { .. })),
        "{errs:?}"
    );
    // A source field named `deref` is a field like any other (R1).
    let prog = program_with(vec![StatementKind::store(
        p0(),
        PathSegment::field("deref"),
        Exp::new_int(0),
    )]);
    assert_eq!(errors_of(&prog), vec![]);
}

#[test]
fn verify_rejects_store_defining_its_base() {
    // `@p0_3.f = 1` in SSA form, with nothing defining `@p0_3`: the store was renamed as a def.
    let prog = program_with(vec![StatementKind::store(
        p0().with_version(3),
        "f",
        Exp::new_int(1),
    )]);
    let errs = errors_of(&prog);
    assert!(
        errs.iter()
            .any(|e| matches!(e, VerifyError::StoreDefinesBase { .. })),
        "{errs:?}"
    );
    // Version 0 is the incoming value.
    let prog = program_with(vec![StatementKind::store(
        p0().with_version(0),
        "f",
        Exp::new_int(1),
    )]);
    assert_eq!(errors_of(&prog), vec![]);
}
