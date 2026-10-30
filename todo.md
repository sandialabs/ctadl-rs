# TODO - DO-NOT-MERGE

Open items left after the function-name work on this branch (see "Function names and the VMT" in
`DESIGN.md`).

## Shared dispatch relations

`Dispatch::Java`, `Dispatch::Cil` and `Dispatch::Lua` each carry their own `methods`,
`hierarchy`, `interfaces` and `abstract_methods`, and `ClassHierarchyAnalysis::build`
(`ctadl-ascent/src/codegen/mod.rs`) feeds all three to `run_cha` in the same four-column shape.
The plan was to move these into shared fields and keep only what one language has (Java
`natives`, CIL `pinvokes`, the CHA flavour) per language.

Not done: it touches about 70 sites and would give up the `JavaClass` / `CilClass` / … newtype
distinction. It also does nothing for naming, since the core already has no alias concept now
that CIL writes bridges. Decide whether it is worth a branch of its own.

## jvm and dex disagree on abstract methods in CHA

Both frontends create an IR function, and a name table entry, for every declared method,
abstract ones included. But jvm also puts abstract methods in `Dispatch::Java::methods`, the
column CHA resolves calls against, and dex does not. The `abstract_methods` doc in
`ctadl-ir/src/mir/call.rs` says abstract declarations are absent from `methods`, so jvm
contradicts it. CHA behaviour is unchanged for now; decide which is right and bring the other
frontend in line.

## Pre-existing rustfmt drift

`cargo fmt --all -- --check` fails on files this work did not change:
`ctadl-ascent/src/models/dsl/eval.rs` and `ctadl-ascent/src/models/dsl/mod.rs` (as of the merge
base), and import ordering in `xtask/src/apk.rs`, `xtask/src/cil/models.rs` and
`xtask/src/regression.rs`.

## Precision of shared-key CIL bridges

When a CIL bridge's key is a different method the class declares (`void IQux.Baz()` beside a
public `Baz()`), the bridge is named `{key}#{declaring class}` and shares the key's row, so a
call under that key reaches both bodies. Fixing it precisely means keying interface call sites
by the interface they name, which is a change to CHA. `Stats::shared_key_bridges` counts how
often it happens.
