// Tests for CTADL IR verification errors
use super::*;
use crate::mir::{
    AccessPath, BasicBlockData, BasicBlockIdx, BasicBlocks, Exp, FunctionIdx, Offset, OffsetAccess,
    OffsetAccesses, ParameterIdx, Params, ReturnType, StatementKind, TerminatorKind, VariableRef,
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
        AccessPath::without_fields(var.clone()),
        FieldRef::symbol("f"),
        Exp::new_str("val"),
    );
    block.statements.push_back(Statement::new_kind(store));
    let result = prog.verify();
    assert!(result.is_ok());
}

// Test for ParameterDoesNotExist (no assertions, just runs verification)
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
    // Add an assign that uses the nonexistent parameter (as an access path).
    let block = &mut f.blocks[BasicBlockIdx::START_BLOCK];
    let stmt = Statement::new_kind(StatementKind::assign(tmp, [Exp::Variable(var.clone())]));
    block.statements.push_back(stmt);
    // Run verification; we don't assert on the result because the behavior may be buggy.
    let result = prog.verify();
    assert!(
        matches!(&result, Err(e) if e.iter().any(|err| matches!(err, VerifyError::ParameterDoesNotExist { .. }))),
        "errors: {:?}",
        &result
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
        "errors: {:?}",
        &result
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
fn test_field_accesses_with_offsets() {
    // Test creating OffsetAccesses with offsets
    let offset_path = OffsetAccesses::with_offset(42);
    assert_eq!(offset_path.len(), 1);

    // Test display format for offsets
    assert_eq!(format!("{}", offset_path), ".[42]");

    // Test multiple offsets (access paths are offset-only)
    let mixed_path = OffsetAccesses::with_offsets([10, 20]);
    assert_eq!(mixed_path.len(), 2);
    assert_eq!(format!("{}", mixed_path), ".[10].[20]");

    // Test creating access path with offsets
    let mut locals = Locals::default();
    let var = VariableRef::new_local_idx(locals.get_or_intern("obj"));
    let field_accesses = OffsetAccesses::with_offset(5);
    let access_path = AccessPath {
        base: var,
        accesses: field_accesses,
    };
    // Base is the local `obj`, followed by the single offset.
    let base = access_path.base.variable.local().unwrap();
    assert_eq!(locals.name(base), "obj");
    assert_eq!(format!("{}", access_path.accesses), ".[5]");
}

#[test]
fn test_offset_newtype() {
    // Test Offset newtype
    let offset = Offset(123);
    assert_eq!(offset.0, 123);
    assert_eq!(format!("{}", offset), "123");

    // Test OffsetAccess (offset-only) and PathSegment (mixed) display
    let symbol_access = PathSegment::symbol("test");
    let offset_access = OffsetAccess::Offset(Offset(456));

    assert_eq!(format!("{}", symbol_access), "test");
    assert_eq!(format!("{}", offset_access), "[456]");
}

/// The IR's `Display` impls emit the canonical access-path grammar, so an IR dump can be pasted
/// into a model port or a `.flowy` file and mean the same thing. Before this, none of them
/// escaped anything: a C field literally named `a.b` rendered as `.a.b`, indistinguishable from
/// two segments, and the frontends' `Symbol("[3]")` rendered as `.[3]`, indistinguishable from
/// an offset.
#[test]
fn test_display_uses_canonical_grammar() {
    // A leading '[' is escaped; a '[' inside a name needs no escape.
    assert_eq!(PathSegment::symbol("[_elem_]").to_string(), r"\[_elem_]");
    assert_eq!(PathSegment::symbol("[3]").to_string(), r"\[3]");
    assert_eq!(PathSegment::symbol("a[3]").to_string(), "a[3]");
    // ... so a bracketed symbol and a real offset are now distinguishable.
    assert_ne!(
        PathSegment::symbol("[3]").to_string(),
        PathSegment::offset(3).to_string()
    );
    assert_eq!(PathSegment::offset(3).to_string(), "[3]");

    // Dots and backslashes in a field name are escaped.
    assert_eq!(PathSegment::symbol("a.b").to_string(), r"a\.b");
    assert_eq!(PathSegment::symbol(r"a\b").to_string(), r"a\\b");
    assert_eq!(FieldRef::symbol("a.b").to_string(), r".a\.b");

    // Offsets are decimal, negatives included.
    assert_eq!(
        OffsetAccesses::with_offsets([-40, 255]).to_string(),
        ".[-40].[255]"
    );

    // And everything the IR prints parses back to what it printed.
    for seg in [
        PathSegment::symbol("[_elem_]"),
        PathSegment::symbol("a.b"),
        PathSegment::symbol(r"a\b"),
        PathSegment::offset(-40),
    ] {
        assert_eq!(path_syntax::parse_segment(&seg.to_string()).unwrap(), seg);
    }
}
