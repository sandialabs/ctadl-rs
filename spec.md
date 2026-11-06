# Compositional globals: Requirements and Design

How the intent in `intent.md` — replace the single `$globals` model with one that allows programs
to define arbitrarily many global variables while remaining compositional — becomes part of
CTADL. The first part states what must be true when the work is done; the second says how the
code should be shaped to make it true. It does not order the work; that is planning.

This document refines [docs/ir.md](docs/ir.md) (the Calls, Globals and Summaries sections and
the well-formedness list), [docs/spec.md](docs/spec.md) §2 and §4.2 (the "Globals" bullet) and
§4.3 (bridges), and [docs/portcullis.md](docs/portcullis.md) §3 and §6. Where they disagree,
this document is current and they are to be updated (R38). The principles of docs/spec.md §1
stand: compositional throughout, simple explainable rules, deliberately unsound, one
language-independent IR, rules written as Datalog.

The IR redesign's spec (`git show 12fe3395~1:spec.md`) numbered the semantics S1–S19 and its
requirements R1–R45; the index's doc comments cite both. This document's G-rules replace S13 and
the globals clauses of S10 and S14; its R-numbers are its own.

Line references are to the tree at the time of writing and will drift.

## 0. Why

Today a global `g` is the field `$globals.g` of one variable, `Variable::GlobalHeap`
(`ctadl-ir/src/mir/mod.rs:339-349`), which codegen turns into a pseudo-formal, `GLOBALS_INDEX =
i16::MIN`, declared `Shared` on every function (`ctadl-ascent/src/codegen/mod.rs:838-843`) and
passed at every call site (`:1236-1245`). Four things follow, and the intent is to end all four.

- **Not compositional in cost.** A flow between two globals inside `f` is a summary row of
  `f` (`G.a <- G.b`), instantiated in every caller, where it becomes a summary row of the
  caller, and so on up: every transitive caller carries every callee's global-to-global flow,
  and every call site re-instantiates it (`ctadl-index/src/facts.rs:1042-1052`,
  `index_engine/mod.rs:1712-1717`). The shipped native defaults document the consequence and
  work around it by skipping the allocator's body
  (`ctadl-ascent/src/models/defaults/native-index.ctadl:276-285`). A flow between two globals
  holds in every calling context; it does not belong in any summary.
- **Context-sensitive where the semantics is not.** A call target held in a global is a
  *decision* of hybrid inlining (`index_engine/mod.rs:1836-1850`, since `formal(G)` is a
  formal like any other), so an indirect call through `g.handler` resolves only in a caller
  whose call chain establishes it. The semantics says a global is one cell shared by every
  instance (S10, S13): the store resolves the call wherever it is. This is an unregistered
  deviation that loses flows and costs decisions.
- **One namespace.** Every program in a project shares `$globals`, so `static int x` in two C
  files is one cell (`frontends/ctadl-c/src/lib.rs:3379-3383`), two binaries' address `0x4000`
  is one cell (`frontends/ctadl-pcode/src/lib.rs:1474-1483`), and nothing can say which program
  a global belongs to. Conversely, the JNI bridge and every Portcullis bridge must pass the
  whole of `$globals` across the boundary, unconditionally and invisibly
  (`ctadl-ascent/src/models/spec.rs:355-368`, `languages/jni.rs:614-616`), for a flow through
  a native global to be found at all.
- **Unnameable.** No model can name a global as a port ("the globals port has no spelling in
  either format", `models/portcullis/migrate.rs:1476-1483`), and a C global cannot live behind
  `deref` because it is a field of a shared object, so a write through `&g` is lost
  (docs/limitations.md:10-12).

## 1. The semantics

These rules replace S13 and the globals clauses of S10 and S14 of the IR redesign; everything
else in [docs/ir.md](docs/ir.md) stands.

- G1. **Declaration.** A program declares its global variables. Each has a name and optionally
  a source location. A global is a variable: an access path may have it as its base, and its
  textual form is `$name`.
- G2. **Sharing.** A global has one set of cells in the whole project, not one per instance of
  the unfolding. S10 becomes: the program is its infinite unfolding; every variable is fresh
  per instance except globals, which every instance shares.
- G3. **Identity.** A global is its name: two programs of a project that declare `g` declare
  one variable. A front end chooses the name and documents the rule; a global that must not be
  shared with another program carries that program's name in its own (`libA.so::ram`), the
  way a C `static` function is already qualified by its unit.
- G4. **Never rebound.** A global is never the destination of a copy or of a call result, and
  is never versioned by SSA. Its storage is the locations below it (`$g.deref`, `$C.f`); where
  a language puts a global's storage is a front-end convention (S19), stated per front end.
- G5. **Calls.** Nothing is passed for globals at a call: a callee names a global and gets the
  same variable. Formally the copies of S10 are `p_i = a_i` and `r = ret` only.
- G6. **Summaries.** A summary is the body's flows between paths rooted at its formals, its
  return and the globals it reaches, **excluding** flows between two global paths. A flow
  between two global paths holds in every instance (G2) and is a fact of the program, not of
  any function. Guards (S14) arise only from target labels at formal paths; a target label at
  a global path holds in every instance and guards nothing. Instantiating a summary (S14)
  substitutes actuals for formals and leaves a global path as it is: it already names the
  shared variable, so no copy of a global is made at a call (G5).
- G7. **Models.** A global path is a port: a source, sink or propagation may name it (S15, S17
  unchanged). A bridge is a call (S15), so it maps formals and returns and nothing for globals.
- G8. **Observable.** Unchanged (S17). A witness may pass through a global between any two
  instances: the store in one function and the load in another need no call between them.

## 2. Requirements

### 2.1 The IR

- R1. `Variable` has a `Global(GlobalIdx)` variant indexing a per-program table of global
  declarations (name, optional span) on `Program`; `Variable::GlobalHeap` is deleted.
  The table is interned by name within a program.
- R2. `verify` (`ctadl-ir/src/mir/verify.rs`) rejects: a global index out of range; a
  `VariableRef` to a global with a version; a copy (empty destination path) into a global, at
  any version; a call result bound to a global; a phi over a global. `EmptyGlobalsWrite` and
  its version-0 exemption (`verify.rs:297-307`) go with the entry binding (R4).
- R3. The write-back of the global heap at the `return`, which codegen emits beside the
  parameters' from the parameter count (`ctadl-ascent/src/codegen/mod.rs:1259-1279`), goes;
  the parameters' write-back is untouched.
- R4. SSA (`ctadl-ir/src/ssa/mod.rs`) leaves globals alone: no entry binding `$globals_0 =
  $globals` (`:433-440`), no phi, no renaming. A global reference is unversioned before and
  after SSA. The passes need no change beyond the removal: a store to a global is a store, and
  `copy_prop`'s anchor rule (`ssa/copy_prop/mod.rs:51-56`) no longer has a globals case.
- R5. The textual form prints a global as `$name`, with the name escaped by the field rules of
  `mir::path_syntax` (a C static qualified by its unit is `$g$util\.c`), and the parser accepts
  it; `$globals` is not special.
- R6. The reference implementation (`ctadl-ir/src/semantics`) implements G2 and G3 directly:
  every global is `Owner::Shared`, keyed by its name (G3) so that two programs' globals of one
  name are one cell; nothing is passed for globals at `instantiate`.
- R7. `IMPORT_FORMAT_VERSION` (`ctadl-import/src/project.rs:124`) is bumped; an older import
  is rejected with the re-import message (docs/spec.md F3). Nothing is migrated in place.

### 2.2 Front ends

Each front end declares its globals (R1), obeys G4 (its output passes R2), and documents its
naming and storage convention in its crate docs. The conventions:

- R8. **C** (`frontends/ctadl-c`). A C global `g` is the global `$g`; its storage is the cell
  `$g.deref`, the convention already used for address-taken locals (`lib.rs:3376`, `in_cell`),
  for every global, since another unit may take its address. `&g` is the copy `$g`; `g = v` is
  the store `$g.deref = v`; `g.f` is `$g.deref.f`. A global with external linkage is named
  by its symbol, as the linker would share it; a file-scope `static` is qualified by its unit
  as static functions already are (`plan_definitions`, `lib.rs:1252-1264`), with the same
  residual (two imports with a same-named unit merge). A constant address `(T*)K`
  (`literal_address_path`, `lib.rs:547-557`) is the global `$<address K>`, whose storage is
  `$<address K>.deref`. The limitation "a write through the
  address of a C global is lost" is removed, and `addr_of_a_global_record_is_a_copy`
  (`tests.rs:2991-3002`) becomes a positive expectation. What else a C name that resolves to
  no scope becomes (enumerators, unexpanded macros; `lib.rs:3370`) is unchanged and
  out of scope.
- R9. **pcode** (`frontends/ctadl-pcode`). One global per import, the image's memory, named by
  the import and Ghidra's address space (`libA.so::ram`), so that two images' addresses are
  distinct cells (G3); a global at address `A` is `$libA.so::ram.[A].deref`, written `$ram`
  below for short. A pointer-typed constant stays an inert constant (`lib.rs:1735-1751`), as
  today, and is a row of the register (§4).
- R10. **JVM, Dex, CIL** (`frontends/ctadl-jvm`, `ctadl-dex`, `ctadl-cil`). One global per
  class that owns a static field, named by the class (`Lcom/C;`,
  `Namespace.Type`); a static field is that global's field, so `putstatic` is the store
  `$Lcom/C;.f = v` and `getstatic` the load `x = $Lcom/C;.f`. A class referenced from several
  imports (an XAPK's splits, a `.jar` and a `.dex`) is one variable (G3). The owner is the
  declared one from the constant pool, as today (`ctadl-jvm/src/lib.rs:735-738`). CIL's
  managed pointers to statics (`ptr.rs:311-313, 450`) resolve to the same paths.
- R11. **Lua** (`frontends/ctadl-lua`). One global, `$_ENV`; a free name `n` is
  `$_ENV.n`, so `kong.request.x` is `$_ENV.kong.request.x`. One VM, one environment across the
  imports of a project, which is what today's one namespace already assumes
  (`lib.rs:73-78`).
- R12. **flowy** (`frontends/ctadl-flowy`). `var g;` declares the global `$g`, and
  `g` in a body is that variable, transparently: `g.deref = p` and `g.f = p` are stores, and a
  whole-variable write `g = p` is a parse error that says to write a location below it (G4).
  A source, sink or summary port on a global names it the same way. The S13 corpus program is
  rewritten under this rule, and the doc sentence "there is no way to have a local and a
  global of the same name" is made true (a parameter shadows a global today, `lib.rs:1003-1022`).
- R13. Every front end's golden tests and the nightly cases are regenerated or re-baselined
  for the new paths, with each changed line attributed to R8–R12.

### 2.3 Index

- R14. A global is a *root* of local reachability like a formal: `locals` and `summary` carry
  a root that is a formal, the return, or a global. No function declares a globals formal, no
  call site passes one, and `GLOBALS_INDEX`, `FlowVariable::is_globals`,
  `FlowVariable::call_actual`'s globals case and `heap_calls_by_caller` go.
- R15. A summary row relates a formal or return path to a formal, return or global path. A
  flow between two global paths is **never** a summary row (G6): it is recorded once, at the
  program level, and the summary of a function with a thousand transitive callees that read
  and write globals is no larger for it. This is the cost bound the intent asks for, and it is
  measured (R40).
- R16. A flow between two global paths found in any function, under no condition, is a
  program-level edge, and program-level reachability between global paths is closed under the
  same field rules (S6, S8 as restricted by the register) as a function's.
- R17. A target label (S11) or string label (S12) at a global path is visible at that path in
  every function that mentions the global, and crosses program-level edges, so an indirect
  call through a global resolves without a decision (G6). Hybrid inlining's critical summaries,
  resolvents and decisions are over formal roots only.
- R18. A flow between two global paths that holds only under a decision is a conditional
  summary row, applied by the existing rule 3.2 at the callers whose route establishes the
  decision, where it becomes a program-level edge (R16). This is G6 under S14: the flow exists
  in the instances where the guard holds, and those are the ones rule 3.2 finds.
- R19. Backward flow through an alias of a global (`l = $g.x; l.f = v` reaching `$g.x.f`)
  follows the same restricted rule as for formals (the "backward alias rule" row of the
  register), with globals among the roots `compute_alias_of_formal` starts from.
- R20. A `--summary` project contributes its functions' summaries, including rows with global
  roots, and its program-level global flows (R16), with globals identified by G3, so that a
  flow in through one library function, held in a library global, and out through another is
  found in the consuming project, as it is today. Conditional rows do not cross, as today.
- R21. The three engines (serial, par, lsm) share one rule text and differ only in execution
  and storage, as today (docs/spec.md §4.2).
- R22. The index saves its project-level table of globals (id, name, declaring programs,
  declaration span) beside the other facts, and `INDEX_FORMAT_VERSION` is bumped; an index or
  summary project built by an older build is rejected.
- R23. `ctadl index` reports, at `info`, the number of globals per program and the names
  declared by more than one program, so an unintended merge across programs is visible
  (docs/spec.md N3).

### 2.4 Query

- R24. A global path is one vertex for the whole project: the search enters it from any
  function's store and leaves it into any function's load, with no call between them (G8).
- R25. A hop through a global resets call/return matching (`TaintState`,
  `ctadl-ascent/src/query_engine/search.rs:568-590`) to its initial state: from a global the
  witness may continue in any instance of any function, so a return out of the function that
  loads the global is admissible even after a call was taken before the store.
- R26. A source or sink on a global path (G7) is a project-level endpoint, anchored to no
  function and fanned out to no call site (`QueryEndpoint::anchored_at_callsites`,
  `ctadl-index/src/facts/query_endpoint.rs:96-128`).
- R27. A result vertex on a global (C0003–C0005) is located at the global's declaration span
  when its front end recorded one, else by a logical location naming the program and the
  global; the steps of a flow through a global are its store and its load, located as any
  instruction is (docs/spec.md N4). C0002's exclusion of the globals pseudo-argument
  (`search.rs:796`) goes.

### 2.5 Models

- R28. Portcullis gains an input relation `global(G)` with attributes `name` and `import` (an
  import that declares it; a shared global matches each of them), and a port
  on it, `G::self.path`, usable in `source`, `sink` and `propagation`, with the path in normal
  form (R39 of the redesign). `global(G)` matching nothing is reported like `local` is
  (`CTADL0004`). A C global's port is `G::self.deref`, a Java static field's `G::self.f`.
- R29. Bridges map no globals. `PortPair::globals()` (`models/spec.rs:355-368`), its
  unconditional emission (`codegen/model_matches.rs:275-278`) and the JNI port map's globals
  pair (`languages/jni.rs:614-616`) go; `BridgePort::parse`'s "mapped unconditionally" error
  (`spec.rs:328-335`) becomes "a global is shared and needs no bridge". The JNI nightly case
  `JniFlow` and its hand-written twin `jni-flow.bridge.ctadl` pass without them, by R24.
- R30. The allocator-by-shape heuristic (`models/portcullis/allocator.rs:336-344`) counts a
  store whose destination is the import's memory global at a constant offset
  (`$ram.[A].deref`, R9), keyed by `A` as today. Its result on the nightly binaries is
  unchanged.
- R31. The shipped defaults keep `skip_analysis` on allocators; the comment in
  `native-index.ctadl:276-285` is rewritten to its remaining reason (none of it is application
  data flow), since the cost reason no longer holds (R15).
- R32. The deprecated JSON format's migration (`migrate.rs:1365-1378`, "would bridge only the
  globals") and docs/deprecated-json-model-generators.md:736-743 say that globals need no
  bridge.

### 2.6 Documentation, register, verification

- R33. `docs/ir.md` states G1–G8: the declaration and textual form in Syntax (the
  well-formedness list gains R2's checks and loses "a copy into the whole of `$globals`"),
  G2 in Calls, G4–G6 in Globals and Summaries, and a "Globals"
  bullet under Front ends naming R8–R12's conventions. docs/spec.md §2 gains a row for
  *Global*, §4.2's "Globals" bullet is rewritten, §4.3's bridge sentence drops globals, and
  §6 drops the C global limitation. docs/limitations.md drops it too. docs/portcullis.md §3
  lists `global`, §6 drops "Globals are mapped unconditionally", §9 updates the migration
  note. Each front end's crate docs state its convention (R8–R12).
- R34. The approximations register, cited by docs/ir.md:6-7 and docs/spec.md:152-153 as
  "the redesign's `spec.md` §4" — a file that no longer exists at that path — moves to
  `docs/approximations.md`, with §4 of this document applied, and both citations point there.
- R35. The flowy semantics corpus pins: a store in one function read in another with no
  common caller (G2); a flow formal → global → return across two calls in one caller (G6);
  a target label stored to a global in one function resolving an indirect call in another
  (G6, R17), and the same through a program-level edge (R16, R17); two programs declaring
  one name sharing one cell, and two names not (G3); a whole-variable write
  rejected (G4, R12); the backward rule through an alias of a global (R19); a hop through a
  global after a call (R25).
- R36. `ctadl-ascent/tests/integration/reference_parity.rs` passes on the corpus with every
  difference matched to a row of the register; the `.tnt` cases and `index_engine_tests.rs`
  are re-baselined only where the program text changes under R8–R12, each re-baseline naming
  its cause.
- R37. The nightly regression (`cargo xtask regression`) passes: `globalflow.c`,
  `globalstruct.c` (its `unexpected_lines` on `g_pair.b` still clean), the Java
  `StaticFieldFlow`, `CrossClassStaticFieldFlow` and `StaticLongChain` cases, and both JNI
  cases; every changed answer is explained by a G-rule or a register row.
- R38. The unit tests named above (`ctadl-c/src/tests.rs:84, 99, 2723, 2991`;
  `ctadl-ir/src/mir/tests.rs:419-437`; `codegen/tests.rs:136, 279-315`;
  `model_matches/tests.rs:165-188, 222-234, 335-355`; `jni/tests.rs:189-209`;
  `semantics/tests.rs:203-243`) are rewritten to the new model rather than deleted; the ones
  that pinned the globals pair pin its absence.
- R39. `cargo xtask regression --compare-paths` on the nightly binaries shows no admissible
  path lost for a reason other than the removal of `$globals`-rooted paths and their
  replacement by `$ram`-rooted ones.
- R40. The summary-size bound (R15) is measured: on the nightly binaries and one real APK,
  the number of `summary` rows with two global roots is zero, and the total summary row
  count and index time are recorded against the pre-change numbers in the PR.

## 3. Design

### 3.1 One variable per global, shared by name, evaluated once

```
   program A              program B                        index
   ┌───────────┐          ┌───────────┐          ┌─────────────────────────────┐
   │ $Lcom/C;  │          │ $Lcom/C;  │ ───────▶ │ Global(id 7)  "Lcom/C;"     │ one vertex
   │ $g$a.c    │          │ $B::ram   │          │ Global(id 8)  "g$a.c"       │
   └───────────┘          └───────────┘          │ Global(id 9)  "B::ram"      │
                                                 └─────────────────────────────┘
                                                               ▲
            f: $7.f = p          h: x = $7.f ; (*x)()          │
            summary(f, Global 7 .f <- Formal 0)                │  frame edges, label
            k: $8.deref = $7.f   -- no summary row --  ───────▶│  exchange (R16, R17)
                                                     GLOBAL_FRAME
```

A global is declared by its program (R1), interned by name to one project-level identity by
codegen (G3), and is a vertex kind of its own in the facts. Within a function it is a root: the rules
that compute reachability from formals compute it from globals too, unchanged. What differs
is what the rows become: formal↔global rows are summaries, global↔global rows are edges of
one distinguished frame, `GLOBAL_FRAME`, where the same rules close them and where target and
string labels are pooled. Nothing is passed at calls, nothing is written back, and the query
treats a global vertex as belonging to every function at once.

### 3.2 `ctadl-ir`: data model

- `Variable` becomes `Local(LocalIdx) | Param(ParameterIdx) | Global(GlobalIdx)`.
  `GlobalIdx` indexes `Program.globals: IndexVec<GlobalIdx, GlobalData>` (on `Program`
  beside `functions`, `mir/mod.rs:691-694`, so it is serialized with the IR) with

  ```rust
  pub struct GlobalData { pub name: Symbol, pub span: Option<SpanId> }
  ```

  `Program::declare_global(name, span) -> GlobalIdx` interns by name (a second declaration of
  one name in one program is one global; the first span wins). `Locals`-style `by_name` is
  `serde(skip)` and rebuilt.
- `VariableRef::new_global()` becomes `VariableRef::global(idx)`, unversioned;
  `BasicBlockBuilder::new_global_var(idx)` likewise. `Variable::new_global()` goes.
- Display: `$name` with field escaping (R5); `path_syntax` parses `$name` to a global looked up
  by name in the program being parsed (flowy, model ports, test paths).
- Encoding: `bitcode` of the derived serde (an enum's tag width is unchanged at three
  variants); `IMPORT_FORMAT_VERSION` bumped with a history line.

### 3.3 Verification, SSA, passes

- `MirVerify` adds the checks of R2, each a `VerifyError` variant with one test in
  `mir/tests.rs`. `visit_variable_ref` bounds-checks `Global` against the program's table and
  rejects a version. `check_store_bases` is unchanged: a store through a global's base is a
  store whose base is defined nowhere and is unversioned, which it already accepts.
- SSA: `PhiPlace` and `SsaRename` skip `Variable::Global` (it has no defs, so today it only
  ever reaches version 0; now it reaches none). The entry binding goes. The module docs at
  `ssa/mod.rs:15-21` and `copy_prop/mod.rs:51-56` drop their global-heap sentences;
  `dead_temps`, `coalesce`, `mem2reg` are untouched.

### 3.4 Reference implementation

`Owner` becomes `Instance(usize) | Global(Symbol)`, the global's name (G3); `var()` maps a
`Variable::Global` to it through the program's table. The entry
binding's two-way copy (`semantics/mod.rs:325-380`) is no longer how sharing happens: a store
`$g.f = v` in any instance writes the shared cell directly, and a load reads it. `instantiate`
is unchanged except that no globals case exists. Multi-program inputs (R35's G3 case) are
evaluated as one program set with one shared cell table.

### 3.5 Front ends

Per front end, what changes; everything else stays.

- **C.** `build_access_path` (`lib.rs:3348-3383`): a `VarKind::Global` name declares (or
  looks up) the global and builds `$g` + `in_cell(path)`; the `is_cell` exclusion of globals
  (`:3376`) goes, and the module doc "Address-taken globals" (`:59-64`) is rewritten to R8.
  `literal_address_path` declares `$<address K>`. Naming: `UnitRegistry` already records
  file-scope declarations (`:1608-1621`); a `static` one is declared under the unit-qualified
  name `plan_definitions` uses for static functions, an `extern` or tentative one under its
  symbol. The callee classification at `:4507-4521` keys on `Global` with a
  path beyond `deref` meaning an indirect call through the global's storage. File-scope
  initializers stay unlowered (§6).
- **pcode.** `global_path(addr)` (`lib.rs:1474-1483`) becomes `$<import>::ram.[A].deref` on
  the import's one declared global, named from the import name the front end is given; the
  module doc at `:13` drops "threaded through every call". Nothing changes in
  `readers/pcode-reader`.
- **JVM / Dex / CIL.** The static-field cases (`ctadl-jvm/src/lib.rs:923-947`,
  `ctadl-dex/src/lib.rs:976-1020`, `ctadl-cil/src/effects.rs:274-311`, `ptr.rs:450`) declare
  the owning class and store or load its field; `jvm_field_symbol` and Dex's
  `pretty_name` supply the field spelling without the class. CIL's `<UNKNOWN::?token>` becomes
  the global `UNKNOWN` with the token as its field.
- **Lua.** `build_var` (`lib.rs:2400-2413`) declares `_ENV` once per program and builds
  `$_ENV.n`.
- **flowy.** Top-level `var` declares; `parse_access_path` (`:1003-1022`) and
  `parse_summary_ap` (`:858-873`) resolve a global name to its `GlobalIdx`; a parameter or
  local of a global's name is an error; a whole-variable write is an error (R12).

### 3.6 Codegen and facts

- `FlowVariable` (`ctadl-index/src/facts.rs:997-1052`) gains a fifth kind, `Global(GlobalId)`,
  with `as_global()`; `is_globals()`, `GLOBALS_INDEX` and the `"$globals_k"` local naming go.
  `GlobalId` is a project-level interned id; codegen interns names (G3), so two programs'
  declarations of one name meet at one id, and records `IndexFacts.globals: Vec<(GlobalId,
  Str, ImportId, Option<FileSpanId>)>`, one row per declaring import (the span resolved in that
  import's source-info database, as `facts.rs:975-986` does for instructions), saved as
  `globals.parquet` (R22).
- `trans_variable_ref` (`codegen/mod.rs:1370-1380`) maps `Variable::Global(idx)` to
  `FlowVariable::global(id)`. `visit_function_data` emits no globals formal (`:838-843`);
  the `Return` arm (`:1259-1279`) writes back the parameters only; call sites emit no globals
  actual (`:1236-1245`). `compute_arg_arity`'s comment loses "globals".
- A `Root` type replaces `FormalIndex` in the root columns:

  ```rust
  pub enum Root { Formal(FormalIndex), Global(GlobalId) }   // return is Formal(RETURN_INDEX)
  ```

  `locals(FunctionId, FlowVariable, Path, Root, Path, Cond)`, `summary(FunctionId, Root, Path,
  Root, Path, Cond)`, `critical_summary`, `critical_arg`, `critical_reach` and the parquet
  `summary` schema (`facts/schema.rs:111-117`: `dst_index`/`src_index` become a root kind and
  id pair). `Decision.formal` stays a `FormalIndex` (R17). `call_arg!(insn, root)` is
  `CallArg(insn, n)` for `Formal(n)` and `Global(g)` for `Global(g)`, which is today's
  `call_actual` with the special case made the rule. `isout(root, ty, p)` is true for a
  global root at every path.
- `GLOBAL_FRAME` is a reserved `FunctionId` (as `RETURN_INDEX` is a reserved index), present
  in the `IdMap` with a name, and is where program-level facts live (§3.7).

### 3.7 Index rules (`ctadl-index/src/index_engine/mod.rs`)

Unchanged in shape; the changes are the seeds, the split of root flows, the frame, and the
label exchange.

- **Mentions and seeds.** `global_mention(FunctionId, GlobalId)` is derived from every
  `assign_like`, `callee_info`, `call_target_assign` and `const_str_assign` row whose vertex
  base is a global, so it covers both program text and edges instantiated from summaries. The
  seed `locals(f, $g, ∅, Global(g), ∅, Always) <-- global_mention(f, g)` is the globals
  counterpart of the formal seed at `:1566-1569`. The frame is seeded for every global.
- **Root flows.** The summary rule (`:1762-1769`) and the aliasing summary rule
  (`:1784-1798`) produce `root_flow(f, r1, p1, r2, p2, c)` instead of `summary` directly;
  then

  ```
  summary(f, r1, p1, r2, p2, c)                        <-- root_flow(...), if !(r1.is_global() && r2.is_global()) ;
  summary(f, r1, p1, r2, p2, c)                        <-- root_flow(...), if r1.is_global() && r2.is_global(), if !c.is_always() ;   // R18
  assign_like(GLOBAL_FRAME, $g1, p1, $g2, p2, Always)  <-- root_flow(f, Global(g1), p1, Global(g2), p2, c), if c.is_always() ;      // R16
  ```

  `compute_alias_of_formal` (`:1340-1376`) starts from every formal and every mentioned
  global (R19). Instantiation (`:1712-1717`) is unchanged: a global root maps to the global
  vertex through `call_arg!`, so a callee's `Global(g).f <- Formal(0)` lands in the caller as
  `assign_like(caller, $g, f, call_arg(insn, 0), ∅)`, which is how the caller's own summary
  `Global(g).f <- Formal(k)` arises when the actual is its formal `k`, and how a conditional
  global-to-global row applied by rule 3.2 becomes a frame edge.
- **Frame.** `GLOBAL_FRAME`'s `assign_like` rows are closed by the existing `ext_dst`,
  `edge_split` and `ext_fml` rules into `locals(GLOBAL_FRAME, $g, p, Global(h), q)`, the
  program-level reachability of R16. The frame has no formals, no call sites and no
  `prog_store`, so no rule produces a summary for it.
- **Label exchange** (R17), the same two rules for `call_target_assign_like` and
  `const_reaches`:

  ```
  call_target_assign_like(GLOBAL_FRAME, $g, p, t) <-- call_target_assign_like(f, $g, p, t) ;
  call_target_assign_like(f, $g, p, t)            <-- call_target_assign_like(GLOBAL_FRAME, $g, p, t), global_mention(f, g) ;
  ```

  Inside `f` the existing propagation (`:2136-2138`) and the local-dispatch bypass
  (`:2066-2069`) then resolve a call through the global, with `Cond::Always`. The frame is an
  `intent_frame` whenever any function is, so string labels cross it too.
- **Hybrid inlining.** Rule 1.1 (`:1806-1809`) derives `critical_summary` for formal roots
  only; 1.2, 2.1, 2.2 and 3.2 are unchanged in text and never see a global root. The
  return-direction tag rules (`:2155-2179`) and `tag_closure_func` are unchanged and no longer
  have a globals case, since no function has a globals out-formal.
- Each rule touched gets the R26-style doc comment naming the G-rule it implements.

### 3.8 The `--summary` boundary

`load_and_map_summaries` (`ctadl-ascent/src/cli/mod.rs:1621-1680`) maps function names as
today and globals by name (G3): the summary project's `globals.parquet` is read, each name is
interned into the consumer's table (creating the global if the consumer declares no such
name), and `summary` rows with global roots are rewritten to the consumer's ids. A new `global_flow.parquet` holds the summary
project's `locals(GLOBAL_FRAME, …)` rows and is loaded as frame edges (R20). JNI's linking
against a summary project (`languages/jni.rs`, "Linking against a summary project") needs no
globals mapping.

### 3.9 Query engine (`ctadl-ascent/src/query_engine`)

- `search.rs`: the search state's function for a global vertex is `GLOBAL_FRAME`. Edges are
  indexed as today by (function, source vertex), plus `reads_of_global: HashMap<(GlobalId,
  Path), Vec<(FunctionId, edge)>>` over every `assign` row whose source base is a global, so
  that from `(GLOBAL_FRAME, $g, p)` the successors are every function's loads of `$g.p` and
  its prefixes, and from a store `$g.p <- x` in `f` the successor is `(GLOBAL_FRAME, $g, p)`.
  Both carry a new `FlowEdge::Global`, which sets `TaintState` to `Free` (R25). The globals
  entry at `:555-562` and `heap_calls_by_caller` go; the exit edge at `:509-534` has no globals
  case. `copies_by_dst` already hops `l = $g` (a local destination).
- `endpoints.rs` and `query_endpoint.rs`: a `Global` endpoint carries a `GlobalId` and path,
  `infunc = GLOBAL_FRAME`, and is returned unchanged by `anchored_at_callsites`. The
  `out_formals` row it pushes today (`endpoints.rs:228-231`) goes.
- Formatter: a vertex at `GLOBAL_FRAME` is located by `globals.parquet`'s span, else a logical
  location `<program>::$name` (R27).
- `tainted_insn` (`search.rs:796`): the guard is `formal() >= 0` alone.

### 3.10 Models (`ctadl-ascent/src/models`, Portcullis)

- Parser: `global(G)` is an input relation over `globals.parquet`-equivalent facts collected at
  import (name, import); `G::self.path` parses to `PortBase::Global` with a path in
  normal form. `FormalIndexTypeTag::Global` carries the `GlobalId`, `expand_port`
  (`model_matches.rs:208`) yields `Root::Global(id)`, and endpoints resolve as in §3.9. A
  propagation whose port is a global emits a `summary` row with a global root (G7). In a
  bridge, `G::self` is rejected with "a global is shared and needs no bridge" (`emit.rs:824`).
- Bridges and JNI: the globals pair and its tests go (R29); `codegen_bridges` and JNI's
  `emit_bridge` emit the written pairs and the return only.
- flowy ports: `port_to_index` (`codegen/flowy.rs:620-628`) returns `Root::Global(id)`; the
  "not yet checked" bail goes.
- `allocator.rs:336-344`: `dest.base.variable` is the import's memory global (the one the
  pcode front end names `<import>::ram`) and the first segment an offset; nothing else
  changes.
- `migrate.rs`: the Global tag's error becomes "write `G::self` with `global(G, …)`".

### 3.11 Reporting, storage, formats

- `ctadl index` logs R23's counts beside the JNI bridge's lines; the per-name list is at
  `debug`.
- Import: bumped format; `globals` in the bitcode program. Index: `globals.parquet`,
  `global_flow.parquet`, the `summary` schema's root columns; bumped `INDEX_FORMAT_VERSION`;
  `index_config.json` unchanged (no new knob: there is no precision/cost trade-off here).
- `ctadl inspect` prints a program's globals.

### 3.12 Documentation

As R33 and R34. `docs/approximations.md` is the register moved verbatim and then edited by §4;
docs/ir.md and docs/spec.md cite it. The `native-index.ctadl` comment is rewritten (R31).

### 3.13 Testing

- `ctadl-ir`: one verifier test per R2 check; SSA tests that a global is never versioned; a
  `path_syntax` round trip of an escaped global name.
- `ctadl-ir::semantics`: the R35 corpus, including a two-program case for G3.
- Front ends: unit tests per convention (R8–R12), in particular the C test that a write
  through `&g` is observed at `g.f`, and `global_flows_across_functions` on `$g.deref`.
- Codegen: no globals formal or actual; a `Global` vertex in `assign`; one name declared by
  two programs interned to one id.
- Index: `index_engine_tests.rs` cases for R15 (no two-global summary row), R16 (frame edge),
  R17 (indirect call through a global resolved `Always`, in a function with no caller), R18
  (conditional global flow applied by 3.2), R19, and the `--summary` round trip of R20.
- Query: a witness through a global that returns after a prior call (R25); endpoint on a
  global (R26, R28); SARIF location of a global vertex (R27).
- Parity and regression: R36, R37, R39, R40.

## 4. Approximations register: changes

Applied to the register at its new home (R34). "Loses"/"adds" as there.

| Change | Where | What | Direction | Reported |
| --- | --- | --- | --- | --- |
| remove (not a row; unregistered) | index | A call target held in a global was a decision, resolved only on a call chain that establishes it | was: loses | — |
| narrow | index | `Shared` formals (pcode, endpoints, JNI): whole-variable write-back through a parameter — no longer includes `$globals` on every function | adds | no |
| unchanged | index | Cells inside a callee merge all callers (S14); globals merge every instance, which is G2, not a deviation | adds | no |
| add | front ends | pcode: the address of a global is an inert constant, not `$ram.[A]`; a write through it is lost | loses | docs/limitations.md |
| add | front ends | C: a file-scope initializer is not lowered | loses | docs/limitations.md |
| remove | front ends | C: a write through `&g` is lost (docs/limitations.md:10-12) | was: loses | — |
| note | query | A hop through a global resets call/return matching; exact under G2, not a row | — | — |
| unchanged | `--summary` | Only unconditional rows cross; now including global-rooted rows and program-level global flows | loses | docs/spec.md §6 |

Anything else that differs from the reference implementation remains a bug.

## 5. Decisions taken

Recorded so they are read as choices, not accidents.

- A global is never a copy destination (G4) rather than "a copy into a global is a store".
  Keeping S5 (store iff the path is non-empty) intact costs one `deref` in C's paths and
  gives `&g` a spelling for free; the alternative would need a second classification rule and
  still have no `&g`.
- A global's identity is its name, with no linkage flag. Whether two programs' `X` are one
  cell has a different answer per global (a Java class across an XAPK's splits: yes; an
  address in two `.so` files: no), so something must say which; a front end saying it by
  qualifying the name (`libA.so::ram`), as C statics are already qualified, costs no IR field,
  no fact column and no model attribute, and leaves one rule to explain.
- C globals are all cells (`$g.deref`), not only address-taken ones: another unit may take the
  address, and one convention is explainable.
- pcode declares one memory global per import, not one per symbol: address arithmetic across
  globals (`&g + 8`) must stay a path, and a per-symbol scheme has no address for `&g`.
  Per-symbol naming is later work (§6).
- JVM/Dex/CIL declare the class, not the field: a static field must be a store (G4), and a
  class is the object that owns it.
- flowy is transparent (R12): it is the IR's test language, and hiding `deref` would hide the
  rule under test.
- Program-level flows live in a distinguished frame evaluated by the existing rules, not in a
  bespoke relation: the field rules and their indexes are reused unchanged, and the frame is
  one more `FunctionId` to every engine.
- The root columns become a `Root` type rather than encoding globals as negative formal
  indices: `i16` cannot hold a large app's class count, `PackedCallArg` packs the index, and
  "formal index" should keep meaning one.
- The port spelling is `G::self.path`, a `::` port like `F::return`; the name is the
  engineering team's to change if the parser prefers another, provided it stays a port.
- No index knob: there is no precision/cost trade-off to expose.

## 6. Later work

Not part of this document's requirements; recorded so the §4 rows have a known remedy.

- **pcode addresses.** A pointer-typed constant that Ghidra marks as a global's address
  (`ExportPcode.java:603-611`) lowers to `$ram.[A]`, giving binaries the `&g` that C now has.
- **Per-symbol binary globals.** Name a global by its symbol when Ghidra has one, keeping
  `$ram` for the rest, so model ports can name `g_slot` rather than an address.
- **C file-scope initializers** (`lower_definitions`, `lib.rs:4562-4600`) and function-local
  `static` (`tests.rs:2866`).
- **Cross-language global sharing by model.** A propagation model between a Java static field
  and a native global (`GetStaticObjectField`) once there is a reason; G7 already allows a
  global port on either side.
- **Index equal to the semantics** and **query equal to the semantics**, as in the redesign's
  §6; the frame makes the one backward rule of that plan apply to globals without change.
