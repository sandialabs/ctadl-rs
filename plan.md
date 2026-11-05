# Implementation plan

The work in [spec.md](spec.md), ordered. Each stage is one or a few PRs onto `ir-redesign`,
leaves `cargo test` green and the nightly regression attributed (every changed answer named
to a rule or a register row), and ends with the tests that prove it. Rule and requirement
numbers (S*, R*) are the spec's.

Order rationale: the path algebra and the removal of `Update` are small and independent, so
they go first; the statement change is the big mechanical one and everything after needs its
shape; the reference implementation comes next so that the passes are checked against it and
the engines measured against it; the engines themselves change only where stages 1–3 take
their input away; front-end conventions come last so their regression deltas are
attributable on their own; docs close. Stages 1 and 2 can run in parallel, as can 4 and 5,
and the per-front-end PRs of stage 7.

## Stage 1: path segments and normalization (R1, R2, R7)

`deref` and `#value` become segment variants; one module owns the equations.

Files:

- `ctadl-ir/src/mir/mod.rs`: `PathSegment::{Field, Offset, Deref, Value}`; delete
  `DEREF_FIELD`, `VALUE_FIELD`; `AccessPath::new` normalizes.
- `ctadl-ir/src/mir/path.rs` (new): `normalize`, `classify`, `rewrite_address_dest`,
  `concat`, `strip_prefix`.
- `ctadl-ir/src/mir/path_syntax.rs`: parse and print the variants; parsers normalize.
- `ctadl-index/src/facts.rs`: `PathNode` variants; `Path::from_accesses`, `prepend_onto`,
  `match_prefix_split` call `mir::path`.
- `ctadl-index/src/index_engine/path_set.rs`: `concat` absorption via `mir::path`;
  `index_engine/mod.rs`: `with_value_paths` closes under S2.
- Every match on the reserved names: `ctadl-ir/src/ssa/mem2reg/mod.rs` (`DEREF`),
  `frontends/ctadl-pcode/src/lib.rs`, `frontends/ctadl-c/src/lib.rs`,
  `frontends/ctadl-cil/src/{ptr,effects}.rs`, `frontends/ctadl-flowy/src/{parse,lib}.rs`,
  `ctadl-ascent/src/models/portcullis/parse.rs`, `ctadl-ascent/src/query_engine/{search,endpoints}.rs`,
  `ctadl-ascent/src/codegen/mod.rs`.
- `ctadl-import/src/project.rs`: bump `IMPORT_FORMAT_VERSION` (segment encoding changed).

Tests:

- New, `ctadl-ir/src/mir/path.rs`: property tests for the S2 equations, idempotence of
  `normalize`, totality and order of `classify`, `strip_prefix ∘ concat` round trip.
- New, `ctadl-index/src/facts.rs` and `path_set.rs`: agreement with `mir::path` on random
  segment lists (offset sums, `[0]`, `#value` absorbing offsets on both sides).
- Existing: `path_set.rs` tests, flowy parse tests, `portcullis/tests.rs`, all front-end
  `tests.rs`.
- New, `ctadl-ascent/tests/tnt/value_offset.tnt`: `t = x.[8]; s.#value = t` reaches a sink on
  `s` when `x.#value` is the source (S2's `p.[k].#value = p.#value`).

## Stage 2: delete `Update`; frame slot writes are stores (R20)

Files:

- `frontends/ctadl-pcode/src/lib.rs`: `push_assign_or_store` loses the `Update` branch;
  entry-block stack parameters bind with stores.
- `ctadl-ir/src/mir/{mod,builder}.rs`: delete `StatementKind::Update`;
  `ctadl-ir/src/ssa/frame/` deleted; `ctadl-ir/src/ssa/mod.rs` (pipeline, `FrameWrites`);
  `ssa/coalesce/mod.rs`, `ssa/mem2reg/mod.rs` (`Update` arms).
- `frontends/ctadl-flowy/src/{parse,lib}.rs`: the `x = [y.p := v]` form goes.
- `ctadl-ascent/src/codegen/{mod,flowy}.rs`: no `update_sans_path`;
  `ctadl-index/src/facts/schema.rs`, `index_engine/{mod,dump}.rs`: relation and rules
  (`mod.rs:1652`, `2115`) deleted; `engines/{serial,par,lsm}/src/lib.rs` declarations.
- `ctadl-ascent/src/query_engine/{mod,search}.rs`: `QueryFacts.update_sans_path` and the
  functional-update step (`search.rs:430`).
- `ctadl-ascent/src/{cli/mod,main}.rs`, `ctadl-import/src/project.rs`: `--frame` and its
  config record; `ctadl-ascent/src/models/{matches.rs,portcullis/{allocator,relations}.rs}`:
  the `frame-store`/`frame-update` modes.
- `ctadl-import/src/project.rs`: bump `INDEX_FORMAT_VERSION` (`update_sans_path.parquet` gone).

Tests:

- Existing, adjusted: `ctadl-ir/src/ssa/{tests,mem2reg/tests}.rs`,
  `ctadl-ascent/src/{codegen/tests,index_engine_tests,models/tests}.rs`,
  `ctadl-ascent/tests/portcullis.rs`.
- `grep -rn Update\|update_sans_path\|FrameWrites` over `*.rs` is empty.
- Nightly `cargo xtask regression` on the C and firmware cases; the PR records the delta
  against `--frame store` on the current tree (expected: identical, since that mode already
  produced stores). The path set is unchanged by this stage (the field write already appeared
  as an assign; the sans-path copy carried empty paths); `--compare-paths` confirms it.

## Stage 3: assignments over full access paths (R3–R6, R8, R23)

The IR carries access paths as operands; `Load`/`Store`/`Address`/`FieldRef`/`ParameterType`
go; the verifier defines well-formedness.

Files:

- `ctadl-ir/src/mir/mod.rs`: `Exp::{Path(AccessPath), Value(AccessPath), ObjectRef, Str, Int,
  Bytes}`; `StatementKind::Assign { dest: AccessPath, sources }`; `load_access_path` /
  `store_access_path` build one `Assign` (fixing the `x.f.[8]` reorder at `mod.rs:1707`);
  `FunctionData.params` without `ParameterType`; `CallStyle::FuncPtrCall.callee: AccessPath`
  (`mir/call.rs`); `mir/{builder,visit/mod,encode}.rs`.
- `ctadl-ir/src/mir/verify.rs`: the R6 checks.
- Passes: `ctadl-ir/src/ssa/mod.rs` (a store defines nothing; `Exp::Value(path)` reads its
  base), `ssa/{dead_temps,coalesce,copy_prop,mem2reg}/mod.rs` (slot patterns on `AccessPath`).
- Front ends, all seven (`frontends/*/src/lib.rs`, `ctadl-cil/src/{effects,ptr,method,calls}.rs`,
  `ctadl-flowy/src/lib.rs`): callers of `load_access_path`/`store_access_path`; parameter
  types removed.
- `ctadl-ascent/src/codegen/mod.rs`: `trans_exp` over `AccessPath`; `cap_path` kept and
  rewritten over `Assign` operands (R3); `Load`/`Store` arms become one `Assign` arm;
  `formal_param` without type.
  `codegen/flowy.rs`, `ctadl-ascent/src/graphviz.rs` and the IR dump in `cli/` follow.
- `ctadl-import/src/project.rs`: bump `IMPORT_FORMAT_VERSION`.

Tests:

- New, `ctadl-ir/src/mir/verify.rs`: one test per R6 check (normal form, `value` placement,
  void operands, store defining its base, empty-path write to `globals`).
- New, `ctadl-ir/src/mir/tests.rs`: `x.f.[8] = v` lowers to destination path `f.[8]`.
- Existing: every pass test; every front-end `tests.rs`; `ctadl-ascent/src/codegen/tests.rs`;
  `ctadl-ascent/tests/c/*.json`; the `xtask` golden suites (`jvm_opcodes`, `dex_opcodes`,
  `asm_flow`, `cil` parity), regenerated and reviewed: the only diffs are load chains
  collapsed into paths and the `f.[8]` fix.
- Path-set invariant: for every nightly artifact and every `tests/tnt` program,
  `paths.parquet` after this stage equals `paths.parquet` before it, modulo stage 1's
  normalization. New, `xtask/src/regression.rs`: `--compare-paths <baseline-index>` diffs the
  two files. The labeling is invariant under composition (R24) but the admissible set (R29)
  is not, so this is what makes the next line a real check.
- Nightly regression: with the path sets equal, no changed answers; any change is a bug.

## Stage 4: reference implementation and corpus (R10–R12, R42, R24)

Files:

- `ctadl-ir/src/semantics/{mod,tests}.rs` (new): the evaluator of §3.5 over `ProgramInfo`,
  with path-length and unfolding-depth bounds.
- `frontends/ctadl-flowy/tests/semantics/*.flowy` (new corpus) with expected observables in a
  trailing `// expect:` block; `frontends/ctadl-flowy/tests/semantics.rs` runs them.
- `frontends/ctadl-flowy/tests/passes.rs` (new): for each pass and the full pipeline, oracle
  before = oracle after on the corpus and on `ctadl-ascent/tests/tnt/*.tnt`.
- `ctadl-ir/src/ssa/{dead_temps,coalesce,copy_prop,mem2reg}/mod.rs`: the R11 correctness
  paragraph and side condition in each module doc; `coalesce` asserts single use.

Tests (the corpus, one program per rule):

- S2: offsets add, `[0]`, `#value` absorbs offsets both sides.
- S3: void propagates nothing; an address carries its base's labels (`p = q.[8]`).
- S5/S6: store vs copy; suffix carriage through a copy (`y = x` with `x.f` tainted).
- S7/S8: `l = y; l.f = x` reaches `y.f.g`; phi does not merge (`y = x1; y = x2`); the suffix
  `#value` stays forward-only; `p.f.#value` and `p.deref.#value` move backward.
- S9: the two-store merge, as an expected flow.
- S10: `r1 = id(a); r2 = id(b)` gives `r2` only `b`; a bodyless call carries nothing.
- S11/S12: function pointer through a struct field resolves; class label through `new`;
  constant string reaching a bridge.
- S13/S14: a global written in a callee reaches the caller; a guarded summary fires only at
  the establishing call site.
- S16: `x1 = src; x2 = clean; sink(x2)` empty; `sink(p.f); p.f = src` reported.
- S17: exact ports (`r` tainted, `r.f` not).
- Pass differential: all green except two crafted programs that document the `mem2reg
  window:N` loss and the `full` loss, marked as the register's rows.

## Stage 5: engines (R26–R37, R40, R41)

The Datalog rules do not change. This stage documents them, measures them against the
reference, deletes the second query engine, and adds the notifications.

Files:

- `ctadl-index/src/index_engine/mod.rs`: the R26 doc comment on `ext_dst`, `edge_split`,
  `ext_fml`, the alias summary rule and the call-site rules; collection of unresolved
  `callee_info` and bodyless `call` rows for notifications. No rule text changes.
- `ctadl-ascent/src/query_engine/mod.rs`: `taint_analysis_datalog`, `copy_alias`,
  `alias_of_field` and the `CTADL_QUERY_DATALOG` switch deleted; `ctadl-ascent/src/cli/mod.rs`:
  the env var's help text.
- `ctadl-ascent/src/cli/mod.rs`, `ctadl-import/src/{project,sarif}.rs`: notifications
  written; `index_config.json` records `alias_rule`, `hybrid_context`, `deref_paths`; two new
  notification message ids.

Tests:

- New, `ctadl-ascent/tests/integration/reference_parity.rs`: for every corpus program and
  every `tests/tnt` program, diff the search's (source, sink) pairs against the reference's;
  every difference must match a row of the §4 register by name, and the test fails on an
  unmatched difference (R36). The register rows for the index and query are thereby
  exercised, each by at least one corpus program.
- `ctadl-ascent/src/index_engine_tests.rs` and `ctadl-ascent/tests/tnt/*.tnt`: unchanged in
  expectation; a case that stages 1–3 re-baselined names its cause. A case whose answer
  depends on a path being admissible names that path in its program (a statement that
  mentions it, or a `// paths:` line), so any later re-baseline can tell a rule change from a
  path-set change.
- `ctadl-ascent/tests/integration/{bridging_models,bridging_end_to_end,absorbing_functions,port_semantics}.rs`
  unchanged.
- Notifications: an integration test with an unresolved function-pointer call and a bodyless
  callee checks both message ids appear in `notifications.sarif` and in the query's SARIF.
- Nightly regression: no changed answers (nothing in the rules changed).

## Stage 6: models and ports (R38, R39)

Files:

- `ctadl-ascent/src/models/portcullis/{parse,emit,check}.rs`: port paths normalized, `.*`
  rejected with a message, `#value` only terminal (kept).
- `docs/portcullis.md`: ports section cites S17/S18; prefix substitution cites S6.

Tests:

- `ctadl-ascent/src/models/portcullis/tests.rs`: `.*` rejected; `x.[0].f` parses as `x.f`.
- `ctadl-ascent/tests/portcullis.rs`, `tests/integration/class_predicates.rs`.

## Stage 7: front-end conventions (R14–R19, R43, R44)

One PR per front end; they are independent.

- **C**, `frontends/ctadl-c/src/lib.rs`: `p->f` and `(*p).f` are `p.deref.f`
  (`flatten_lvalue`); arithmetic and all comparisons value-only (`flatten_binary`);
  `q = p + i` is copy-plus-value; `docs/limitations.md` gains `&s.f`.
  Tests: `ctadl-c/src/tests.rs` (field, deref, arithmetic cases rewritten),
  `ctadl-ascent/tests/c/*.json` re-baselined, nightly `c` cases attributed.
- **pcode**, `frontends/ctadl-pcode/src/lib.rs`: `PTRADD`/`PTRSUB` with a non-constant
  operand become copy-plus-value; `merge` mode stays. Tests: nightly firmware and `jni`
  cases; `xtask report_eval` numbers recorded.
- **JVM / Dex**, `frontends/ctadl-jvm/src/lib.rs`, `frontends/ctadl-dex/src/lib.rs`:
  arithmetic value-only; `MethodHandle`/`const-method-handle` with a known method become
  `ptr<F>`. Tests: `tests.rs` in each, `xtask` `jvm_opcodes` and `dex_opcodes` goldens,
  nightly `java` and `android-icc` cases.
- **CIL**, `frontends/ctadl-cil/src/effects.rs`: arithmetic value-only. Tests:
  `ctadl-cil/src/tests.rs`, `xtask cil` parity and flow suites, nightly `cil`.
- **Lua**, `frontends/ctadl-lua/src/lib.rs`: arithmetic value-only. Tests: nightly `lua`.
- Each front end's crate docs gain the S19 conventions paragraph (R14).

## Stage 8: documentation and configuration (R41, R45)

Files:

- `intent.md` → `docs/ir-semantics.md`; `spec.md` → `docs/ir-redesign.md`; this file →
  `docs/ir-redesign-plan.md`.
- `docs/spec.md`: §2 access path, §4.1 import/index, §4.2 (field propagation, finite access
  paths, summaries, aliasing, "value taint only moves forward") rewritten to cite
  `docs/ir-semantics.md`.
- `docs/debugging.md`: relation names (`assign`, `summary`, no `update_sans_path`).
- `ctadl-import/src/project.rs`: the import config records `CTADL_EXP_INT_ARITH` and the
  mem2reg policy when not default.
- `README.md`: the one-paragraph description of the IR.

Tests: `cargo doc --no-deps` clean; the `ctadl inspect` and `--dump-ir` paths in
`tests/integration/cli.rs` still pass.

## Acceptance

- `cargo test` across the workspace, `cargo xtask regression` for every front end, each
  changed answer attributed to a rule or a register row in the PR that changed it.
- Reference parity (stage 5) green on the corpus and `tests/tnt`: every difference between
  the engines and the reference matches a register row.
- Index time and peak memory on the firmware set and one large APK within noise of the tree
  before stage 1 (the rules did not change; a regression is a codegen or path-set bug).
- `grep` finds no `DEREF_FIELD`, `VALUE_FIELD`, `Load`, `Store`, `Address`, `FieldRef`,
  `Update`, `update_sans_path`, `CTADL_QUERY_DATALOG`.
