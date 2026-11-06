# Implementation plan

The work in [spec.md](spec.md), ordered. Each stage is one or a few PRs onto
`compositional-globals`, leaves `cargo test` green and the nightly regression attributed (every
changed answer named to a G-rule, an R-number or a register row), and ends with the tests that
prove it. Rule and requirement numbers (G*, R*) are the spec's; S-numbers are the IR redesign's.

Order rationale. The IR change deletes `Variable::GlobalHeap`, so every front end, codegen and the
reference implementation must move in the same PR; the index and the query must not. Stage 1
therefore keeps the index's pseudo-formal alive behind a five-line shim in `trans_variable_ref`
that lowers the new `Variable::Global(g)` with path `p` to today's `$globals.<name>.p`. Under that
shim the front ends' naming and storage conventions (R8–R12) change the fact base by renaming
only, so their regression deltas are attributed to R8–R12 against an index whose semantics has
not moved; and stage 2, which moves the index and the query and deletes the shim, is attributed
to the G-rules alone. The two kinds of change are never in one diff. Stage 2 is itself split:
2a introduces every fact type the index and query will need while producing none of the new
rows, so it is checked by identity of the fact base; 2b is the behavior. Stage 0 is independent;
stage 3 needs 2b's `Root::Global`; docs close. (Stage 1b, C storage, was folded into stage 1:
see the note there.)

Status: every stage is implemented (0, 1 with 1b folded in, 2a, 2b, 3, 4). Each stage below that is done ends with an "As built"
paragraph recording where the implementation departs from the text above it and why; the text
itself is left as it was planned.

## Stage 0: the approximations register moves (R34)

The register the parity test cites lives in a file that no longer exists at the cited path
(`git show 12fe3395~1:spec.md` §4). It moves verbatim now so that stages 1–3 edit a file that is
there.

Files:

- `docs/approximations.md` (new): the table of the redesign's spec §4, verbatim, with its two
  framing sentences.
- `docs/ir.md:6-7`, `docs/spec.md:152-153`,
  `ctadl-ascent/tests/integration/reference_parity.rs:7,33`: the citation points at
  `docs/approximations.md`.

Tests: `reference_parity.rs` unchanged in outcome; `cargo doc --no-deps` clean.

As built (commit `00cee6ee`). The register moved with a short header saying where it came from
and that its R- and S-numbers are the redesign's. Two more citations of "`spec.md` §4" were
repointed: `ctadl-ascent/src/codegen/flowy.rs` (`engine_observable`'s doc) and
`ctadl-import/src/project.rs` (`IndexConfig`'s results-changing options).

## Stage 1: the IR declares globals (R1–R7, R9–R12, R8's naming, R33's `docs/ir.md` part)

One PR. The IR gains the per-program table; the verifier, SSA and the reference implementation
follow; every front end declares what it used to spell as a field of `$globals`; codegen lowers
a global back to the pseudo-formal so the index is untouched.

Files, `ctadl-ir`:

- `src/mir/mod.rs`: `Variable::Global(GlobalIdx)` replaces `GlobalHeap` (`:336`);
  `GlobalIdx`, `GlobalData { name, span }`, `Program.globals: IndexVec<GlobalIdx, GlobalData>`
  beside `functions` (`:691-694`) with a `serde(skip)` `by_name` rebuilt on load;
  `Program::declare_global(name, span) -> GlobalIdx` interns by name, first span wins;
  `VariableRef::global(idx)` replaces `new_global()` (`:748, :781`); `Display` prints `$name`
  escaped by the field rules (`:875`); the module doc at `:70-73` states G1–G4.
- `src/mir/builder.rs:235`: `new_global_var(idx)`. `src/mir/builder_tests.rs:153-170` follows.
- `src/mir/path_syntax.rs`: `$name` parses to a global looked up by name in the program being
  parsed; `$globals` is not special.
- `src/mir/verify.rs`: `EmptyGlobalsWrite` and its version-0 exemption (`:297-307, :319`) go;
  new variants `GlobalOutOfRange`, `VersionedGlobal`, `CopyIntoGlobal`, `CallResultIntoGlobal`,
  `PhiOverGlobal` (R2). `check_store_bases` unchanged.
- `src/ssa/mod.rs:433-440`: the entry binding goes; `PhiPlace` and `SsaRename` skip
  `Variable::Global`; the module doc at `:15-21` and `src/ssa/copy_prop/mod.rs:51-56` drop
  their global-heap sentences (R4).
- `src/semantics/mod.rs`: `Owner::Instance(usize) | Global(Symbol)` (`:190-198`); `var()`
  (`:266-272`) maps a `Variable::Global` through the program's table to its name; the entry
  binding's two-way copy (`:325-380`) goes; `evaluate` takes `&[&Program]` so that two programs
  share one cell table (G3, §3.4); the module doc at `:25` states G2–G6.
- `src/mir/encode.rs`: nothing to change (derived serde); `ctadl-import/src/project.rs:124`:
  `IMPORT_FORMAT_VERSION` 13 → 14 with a history line (R7).

Files, front ends (each declares; each crate doc states its convention):

- `frontends/ctadl-flowy/src/{parse,lib}.rs`: top-level `var g;` (`parse.rs:47`) calls
  `declare_global`; `parse_access_path` (`lib.rs:1003-1022`) and `parse_summary_ap` (`:858-873`)
  resolve a global name to `VariableRef::global(idx)` with no leading field; a parameter or
  local of a global's name is an error; a whole-variable write `g = p` is a parse error naming
  G4 (R12). `tests/semantics/s13_globals.flowy` is rewritten under R12 (`g.deref = p`).
- `frontends/ctadl-c/src/lib.rs`: `build_access_path` (`:3348-3383`) declares a `VarKind::Global`
  name — an `extern` or tentative one under its symbol, a file-scope `static` under the
  unit-qualified name `plan_definitions` (`:1252-1264`) uses for static functions, from
  `UnitRegistry` (`:1608-1621`) — and builds `$g` + today's path; `literal_address_path`
  (`:547-557`) declares `$<address K>`; the callee classification (`:4507-4521`) keys on
  `Variable::Global` with a non-empty path. Storage (`deref`) is stage 1b.
  `src/test_utils.rs:331, :858-894, :930` (the `$globals` DSL and the `GLOBALS_INDEX`
  summary helpers) read `$g` and keep using the pseudo-formal until stage 2b.
- `frontends/ctadl-pcode/src/lib.rs`: `global_path(addr)` (`:1474-1483`) declares
  `<import>::ram` once per program, from the import name the front end is given, and builds
  `$<import>::ram.[A].deref`; the module docs at `:13, :128, :1385` follow (R9).
- `frontends/ctadl-jvm/src/lib.rs:923-947`, `frontends/ctadl-dex/src/lib.rs:976-1020`,
  `frontends/ctadl-cil/src/{effects.rs:274-311, ptr.rs:311-313,450}`: the static-field cases
  declare the owning class and store or load its field; `jvm_field_symbol` and Dex's
  `pretty_name` yield the field without the class; CIL's `<UNKNOWN::?token>` becomes the global
  `UNKNOWN` with the token as its field (R10).
- `frontends/ctadl-lua/src/lib.rs:2400-2413`: `build_var` declares `_ENV` once and builds
  `$_ENV.n`; the crate doc at `:73-78` follows (R11).
- `ctadl-ascent/examples/ir1.rs:157`: declares a global.

Files, codegen (the shim):

- `ctadl-ascent/src/codegen/mod.rs:1370-1380`: `(Variable::Global(idx), None)` maps to the
  pseudo-formal `GLOBALS_INDEX` with `Field(name)` prepended to the path; the versioned arm
  goes (nothing versions a global now). The `Return` arm's global write-back (`:1259-1279`)
  goes (R3): with no SSA renaming, a store to `$g.p` already lands on the `Shared`
  pseudo-formal. The globals formal (`:838-843`) and actual (`:1236-1245`) stay until stage 2b.
- `ctadl-ascent/src/codegen/flowy.rs:620-628`: `port_to_index` lowers a global port the same
  way, so the "not yet checked" bail goes now.
- `ctadl-ascent/src/models/portcullis/allocator.rs:297, :336-344`: matches `Variable::Global`
  whose name is the import's memory global, first segment an offset (R30; the IR match, not
  the facts).
- `ctadl-ascent/src/models/portcullis/migrate.rs:1476-1483`: the Global tag's error reads
  "write `G::self` with `global(G, …)`" (lands in stage 3's parser; the message changes now).

Files, docs: `docs/ir.md` states G1–G8 (Syntax: declaration and `$name`; the well-formedness
list gains R2's checks and loses "a copy into the whole of `$globals`"; Calls: G2; Globals and
Summaries: G4–G6; a "Globals" bullet under Front ends naming R8–R12) (R33's first part).

Tests:

- New, `ctadl-ir/src/mir/tests.rs`: one test per R2 check, replacing
  `verify_rejects_empty_globals_write` (`:419`); `declare_global` interns (second declaration
  returns the first index, first span kept).
- New, `ctadl-ir/src/mir/path_syntax.rs`: round trip of `$g$util\.c.deref` and of
  `$libA.so::ram.[4096].deref`.
- New, `ctadl-ir/src/ssa/tests.rs`: a function that stores to and loads from `$g` has no phi,
  no entry binding and no versioned global reference after the pipeline; `copy_prop` and
  `dead_temps` tests that named `$globals` are rewritten.
- `ctadl-ir/src/semantics/tests.rs`: `globals_are_shared_and_bodyless_calls_reported`
  (`:205`) rewritten to a declared global; new: two programs declaring `g` share one cell and
  `g` and `h` do not (G3, through `evaluate(&[a, b])`); nothing is passed for a global at
  `instantiate` (a callee that names `$g` sees the caller's store without an actual).
- `frontends/ctadl-flowy/tests/semantics/` gains the R35 corpus, run on the reference
  implementation: `g02_shared.flowy` (store in one function, load in another, no common
  caller), `g06_formal_global_return.flowy` (formal → global → return across two calls in one
  caller), `g06_target_via_global.flowy` (target label stored to a global in one function
  resolves an indirect call in another), `g03_two_programs.flowy` (two programs split at a
  `// --- program ---` line; one name shares, two do not), `g04_whole_write.flowy` (parse
  error expected), `g19_alias_backward.flowy`, `g25_hop_after_call.flowy`.
  `tests/common/mod.rs` splits multi-program files and calls the slice-taking `evaluate`.
  `reference_parity.rs` runs these too; in stage 1 the engines still resolve an indirect call
  through a global only on an establishing call chain, so `g06_target_via_global` and
  `g03_two_programs` carry a `// parity:` line naming the register's "call target held in a
  global" row until stage 2b removes both the row and the line.
- Front ends: `frontends/ctadl-c/src/tests.rs` `simple_assign_global` (`:84`),
  `simple_global_assign` (`:99`), `a_named_global_still_lowers_to_a_symbolic_field` (`:4249`)
  and the `$globals` DSL strings read `$g`; `frontends/ctadl-jvm/src/tests.rs`,
  `ctadl-dex/src/tests.rs`, `ctadl-cil/src/tests.rs`: a static field lowers to `$Lcom/C;.f` /
  `$Namespace.Type.f`; `ctadl-lua`: a free name is `$_ENV.n`; `ctadl-pcode`: a global is
  `$<import>::ram.[A].deref`. The `xtask` goldens (`xtask/src/{jvm_opcodes,dex_opcodes,asm_flow,cil}`)
  regenerated; every changed line is a `$globals.<x>` → `$globals.<class>.<f>` (or `_ENV`,
  `ram`) rename, attributed to R10/R11/R9 in the PR (R13).
- Codegen, `ctadl-ascent/src/codegen/tests.rs:136, :279-315` (`test_local_to_global_field`):
  pinned to the shim's spelling for now; `model_matches/tests.rs` and `jni/tests.rs`
  untouched.
- `ctadl-import`: an import written at version 13 is rejected with the re-import message.
- Path-set check: `cargo xtask regression --compare-paths` against the tree before this PR:
  the C, flowy and `.tnt` path sets are identical; the Java, Dex, CIL, Lua and pcode sets
  differ only by the renames above. No changed nightly answer.

As built (stage 1, with stage 1b folded in). Where the implementation departs from the above:

- **C storage moved into stage 1.** The stage-1 form "`$g` + today's path" is not a well-formed
  program: today's `g = v` is `$globals.g = v`, a store, and under R2 it becomes `$g = v`, a copy
  into a global, which the verifier rejects. So R8's storage convention (every C global is the
  cell `$g.deref`, `&g` the copy `$g`) is part of stage 1, and stage 1b below has nothing left
  but what it already listed. The C path-set delta of this stage is therefore R8's `deref`
  insertion, not identity.
- **C naming.** A file-scope `static` is qualified by its unit (`count$one.c`) only when another
  unit of the import also declares a file-scope variable of that name; then every such `static`
  is qualified (unlike functions, where the first definition keeps the bare name), so the one
  with external linkage keeps it. A `static` no other unit declares keeps its name, so a
  single-unit import's names do not change. Two units with the same base name still merge.
  A direct call `f(x)` is decided before the callee's name is resolved (`names_a_function` in
  `collect_call`), so a function's name is never declared as a global; the callee
  classification moved into `Context::callee_style`. A constant address `(T *)K` is the global
  `$<address K>` itself, a pointer to its storage, so `((T *)K)->f` is `$<address K>.deref.f`,
  which is the path the shim gives today's `$globals.<address K>.deref.f`.
- **Display and parsing.** `Variable::Global` prints as `$name` only while a program's names are
  published: by `Display for Program` (which also lists the globals), `WithGlobalNames`, or
  `Program::global_names_guard`; elsewhere it prints as `$#<index>`. `ctadl inspect`'s per-function
  dump and the CIL MIR goldens take the guard. `path_syntax::parse_global_access_path` and
  `write_symbol` are the parser and printer; the field rules escape `.`, so `libA.so::ram`
  prints `$libA\.so::ram`, and the unescaped `$libA.so::ram.[4096].deref` parses as the global
  `libA` (both round trips are in `path_syntax`'s tests).
- **Reference implementation.** `evaluate(&Program)` stays; `evaluate_programs(&[&Program])` is
  the multi-program form. A global's cells are keyed by its name. There was no separate
  two-way entry copy to delete: sharing was SSA's `$globals_0 = $globals` plus `var()`, both gone.
- **Front ends.** JVM and Dex name a static field by its name alone (no descriptor); Dex splits
  `pretty_name`'s `Lcls;->name:type` in the front end. CIL's unknown-owner global is
  `UNKNOWN_CLASS` (`[?]?`), the spelling its symbols already used, with field `?<token>`; the
  emitter carries the program's `Globals` (`Emitter::with_globals`). pcode declares
  `<import>::ram` when it starts processing an import, named by `memory_global_name`, so a
  program with no global access still declares it. The allocator heuristic (R30) matches any
  global base, since a native program declares only its memory.
- **flowy.** A requirement names a global port by name, `PortBase::Global`, since requirements
  are read apart from the program; `port_to_index` lowers it with the shim. A file may hold
  several programs of one project separated by `// --- program ---`
  (`ctadl_flowy::split_programs`, `compile_programs`); the corpus harness, the pass differential
  test and `engine_observable` evaluate them together. A corpus program whose text has
  `// expect-error: <text>` must fail to compile with that text and is skipped by the other
  two. The corpus is `g02_shared`, `g03_two_programs`, `g04_whole_write`,
  `g06_formal_global_return`, `g06_target_via_global` (both cases of R35: a direct read, and
  a read through a copy to a second global), `g19_alias_backward` (both functions called from
  one caller, so it tests the backward rule and not the missing caller) and
  `g25_hop_after_call`.
- **Codegen.** The shim is `CodegenVisitor::capture_of`: a global's path is composed onto the
  field of its name, so `$g.p` is the vertex `$globals.g.p` everywhere a path is built, and
  `trans_variable_ref` maps any global to the pseudo-formal. `codegen_function_with_globals`
  lowers a lone function that names globals. With no SSA entry binding there is no
  `$globals_k` local any more: a load or store of a global is on the pseudo-formal directly.
- **Register.** The engines lose a flow through a global between functions that no call chain
  connects, whether data (`g02`, `g03`, `g25`) or a call target (`g06_target_via_global`), so
  stage 1 registers two temporary rows, "global passed at calls" and "call target held in a
  global", both loses, both removed by stage 2b with their `// parity:` lines. The plan named
  only the second.
- **Docs.** `docs/ir.md` states G1–G8 as planned. `docs/limitations.md` and
  `docs/approximations.md` take stage 1b's C edits (the `&g` limitation goes, the file-scope
  initializer row is added). The import-version test is
  `an_import_from_before_declared_globals_is_refused`.
- **Regression.** `cargo xtask regression` against a baseline run of stage 0's tree: 255 of 259
  cases pass and the same 4 xfail, with every nightly answer unchanged. `apk:import` failed
  once, on the version pin in `xtask/src/apk.rs`, which is now 14. `--compare-paths` reports
  146 of 223 path sets different, because it compares exact strings. Every difference is a
  rename: 112 are JVM/Dex `<Lcls;->f:desc>` to `Lcls;.f` (R10), 8 are CIL `<[asm]C::f>` to
  `\[asm]C.f` (R10), 6 are pcode `.[A].deref` to `.<import>::ram.[A].deref` (R9), 17 combine
  those three (APK, JNI), and 5 C indexes differ by R8's `deref` after a global
  (`C_globalflow`, `C_globalstruct`, `C_valfieldsum`, `C_valindex`, `C_valstrlen`). The other
  77 are identical. The classification script is not committed; it rewrites both sides by
  these rules and compares the sets.

## Stage 1b: C storage convention (R8, R38's C part)

Folded into stage 1 (see "As built" there); nothing remains to do here. The text is kept as
the record of what that work is.

One PR, after stage 1, independent of stage 2a.

Files:

- `frontends/ctadl-c/src/lib.rs`: `build_access_path` builds `$g` + `in_cell(path)` for every
  global; the `is_cell` exclusion of globals (`:3376`) goes; `&g` is the copy `$g`, `g = v` the
  store `$g.deref = v`; `literal_address_path` builds `$<address K>.deref`; the callee
  classification (`:4507-4521`) treats a path beyond `deref` as an indirect call through the
  global's storage; the module doc "Address-taken globals" (`:59-64`) is rewritten to R8.
- `docs/limitations.md:10-12`: the C limitation goes; `docs/approximations.md`: the C `&g` row
  is removed and the "file-scope initializer is not lowered" row added (§4).

Tests:

- `frontends/ctadl-c/src/tests.rs`: `addr_of_a_global_record_is_a_copy` (`:2991`) becomes the
  positive expectation (a write through `&g` is observed at `g.f`);
  `global_flows_across_functions` (`:2723`) on `$g.deref`; the indirect-call tests at
  `:5332-5821` on `$g.deref` and `$g.deref.f`; new: a C static in two units is two globals,
  an `extern` in two units is one; `(T*)K` is `$<address K>.deref`.
- `ctadl-ascent/tests/c/*.json` re-baselined; nightly `c` cases (`globalflow.c`,
  `globalstruct.c` with its `unexpected_lines` on `g_pair.b` still clean) attributed to R8.
  `--compare-paths`: every C change is a `$globals.g.p` → `$globals.g.deref.p` insertion.

## Stage 2a: the fact types (R22, R14's types, R21)

One PR after stage 1 (stage 1b may run beside it). Every type the index and query will need
arrives, but nothing produces a global root, a global vertex or a frame row yet, so the stage
is behavior-neutral: the fact base before and after is identical row for row. The three
engines are touched here once, for the column types, and not again.

Files:

- `ctadl-index/src/facts.rs`: `pub enum Root { Formal(FormalIndex), Global(GlobalId) }` with
  `is_global()`, replacing `FormalIndex` in the root columns; `GlobalId`, a project-level
  interned id; `FlowVariableKind::Global(GlobalId)` and `FlowVariable::{global, as_global}`;
  `isout` (`:1652`) takes a `Root` and is true for a global root at every path;
  `GLOBAL_FRAME: FunctionId` reserved and present in `IdMap` (`:1778`) with a name;
  `IndexFacts.globals: Vec<(GlobalId, Str, ImportId, Option<FileSpanId>)>`
  (`index_engine/mod.rs:97`), empty, saved by `try_save` (`:1805`). `GLOBALS_INDEX`,
  `is_globals` and `call_actual`'s globals case stay until 2b.
- `ctadl-index/src/facts/schema.rs`: `summary` (`:111-117`) gains root-kind columns beside
  `dst_index`/`src_index`; new modules `globals` (`globals.parquet`) and `global_flow`
  (`global_flow.parquet`), written empty.
- `ctadl-index/src/index_engine/mod.rs`: `locals` (`:1519`), `critical_summary` (`:1543`),
  `critical_arg` (`:1866`), `critical_reach` (`:1879`) carry `Root`; `call_arg!` (`:1293`)
  takes a `Root` and maps `Formal(n)` as today, `Global(g)` to `FlowVariable::global(g)`; the
  formal seed (`:1566-1569`) and the summary rules (`:1762-1769, :1784-1798`) wrap their index
  in `Root::Formal`; rule 1.1 (`:1806-1809`) matches `Root::Formal` only. `Decision.formal`
  (`decision.rs:35`) stays a `FormalIndex`. The trie stores (`c_locals_trie.rs`,
  `locals_trie.rs`, `trie_stores!` at `:2450`) and `dump.rs` carry and print `Root`.
- `ctadl-index/engines/{serial,par,lsm}/src/lib.rs`: nothing but what the rule text pulls in
  (R21); checked by building all three.
- `ctadl-ascent/src/codegen/{model_matches.rs:208, flowy.rs:615-628}`: `expand_port` and
  `port_to_index` return `Root`, still `Root::Formal(GLOBALS_INDEX)` for a global port.
- `ctadl-ascent/src/query_engine/{search,endpoints}.rs`,
  `ctadl-index/src/facts/query_endpoint.rs`, `ctadl-ascent/src/cli/mod.rs:1621-1680`,
  `frontends/ctadl-c/src/test_utils.rs:858-894`: callers that read a root column match on
  `Root::Formal` and treat `Root::Global` as unreachable for now.
- `ctadl-import/src/project.rs:177`: `INDEX_FORMAT_VERSION` 7 → 8 (R22; the `summary` schema
  and the two new files).

Tests:

- `ctadl-ascent/src/codegen/tests.rs`, `src/index_engine_tests.rs`, `tests/tnt/*.tnt`:
  unchanged in expectation; a test that constructs a root column uses `Root::Formal`.
- New, `ctadl-index/src/facts.rs`: `Root` round-trips through the `summary` parquet schema;
  `GLOBAL_FRAME` is in a fresh `IdMap` and `get_function` names it.
- `ctadl-import`: an index written at version 7 is rejected.
- `cargo xtask regression` and `--compare-paths` against stage 1b: identical answers and
  identical path sets. Any difference is a bug in the type plumbing.

As built (commit `f3dbea30`). Where the implementation departs from the above:

- **`GlobalId` is the interned name.** `GlobalId(Str)`: a global is its name (G3), so the
  project-level interner the plan calls for is `Str`'s, two imports' declarations meet at one id
  with nothing to do, and a `--summary` project's global roots need no mapping (they serialize as
  names). `IndexFacts.globals` is therefore `(GlobalId, ImportId, Option<FileSpanId>)`, without
  the separate name column.
- **`Root` is packed.** `Root(u32)`: a formal's `i16`, or the top bit and the global's name id,
  with `RootKind` for matching. Four bytes, so the `locals` leaves do not grow. It converts from a
  `FormalIndex`, an `i16` and a `GlobalId`.
- **A global `FlowVariable` shares the local tag.** The four 2-bit tags were all taken, and a
  3-bit tag would cut a call-arg's instruction id; a global is `TAG_LOCAL` with bit 61 set and the
  name's id below. `is_local` excludes it, and parquet writes it as tag 4 with the name.
- **The critical relations stay formal.** `critical_summary`, `critical_arg`, `critical_reach`,
  `resolvent` and `Decision` keep `FormalIndex`: only a formal root can enter them (rule 1.1 and
  the others filter `Root::as_formal`), which is R17's "over formal roots only" by type.
- **`GLOBAL_FRAME`** is `FunctionId((1 << 28) - 1)`, the largest an `InsnSiteId` holds, named
  `<globals>` by every `IdMap` (`get_function`, `get_function_id`) and never handed out, so no
  existing id shifts. `call_arg!` takes anything that converts to a `Root`.
- **`summary.parquet`** writes each root as `<root>_index` and `<root>_global`, one of them null;
  the columns are `dst`/`src` (so `dst_index`, `dst_global`, ...). `global_flow.parquet` is
  `(dst, dst_path, src, src_path)` over global names.
- **Tests.** `ctadl-index`'s facts tests: `root_round_trips` (memory and the summary schema),
  `global_flow_variable_round_trips`, `global_frame_is_named_in_every_id_map`; the integration
  test `index_version_gate_rejects_an_index_from_before_declared_globals`. 1929 workspace tests
  pass, the stage-1 count plus these four.
- **Regression.** `cargo xtask regression --release` on stage 1 (`4e55f6fa`) and on 2a, each in
  its own worktree: both 255 of 259 passed, the same 4 xfail. Every index's `paths.parquet` and
  `assign.parquet` are identical between the two runs (223 of 223), and so is every
  `summary.parquet` by content: 3 differ in the ids of synthetic dispatch functions, which are
  interned in a different order from run to run, not in any row.

## Stage 2b: the index and the query (R14–R20, R23–R27, R29, R38's index part)

One PR after 2a and 1b. The shim goes, the pseudo-formal goes, the frame arrives, and the
query hops through globals. This is the one PR whose deltas are attributed to G-rules alone.
Measured before and after (R40): the pre-change numbers are taken on the tree at the end of
stage 2a.

Files, facts and codegen:

- `ctadl-index/src/facts.rs`: `GLOBALS_INDEX` (`:692`), `is_globals` (`:1149-1155`),
  `call_actual`'s globals case (`:1042-1052`) and `TryFromVariableError::Global` (`:1217`) go.
- `ctadl-index/src/facts/query_endpoint.rs:96-128, :143`: a `Global` endpoint is returned
  unchanged by `anchored_at_callsites` by kind, not by `GLOBALS_INDEX`.
- The stage-1 shim goes: `CodegenVisitor::capture_of`'s global case and `global_names`
  (`codegen/mod.rs`), and `port_to_index`'s `PortBase::Global` arm (`codegen/flowy.rs`), which
  returns `Root::Global(id)`. The C test helpers `check_param_into_global_in` and
  `check_returns_global_in` take the shim's `.g.deref` spelling and move to `Root::Global`.
- `ctadl-ascent/src/codegen/mod.rs`: `trans_variable_ref` (`:1370-1380`) maps
  `Variable::Global(idx)` to `FlowVariable::global(id)` through a project-level interner
  keyed by name (G3), recording a `globals` row per declaring import with the span resolved
  as `facts.rs:975-986` does; the globals formal (`:838-843`) and actual (`:1236-1245`) go;
  `compute_arg_arity`'s comment (`:1164`) and the module doc (`:15`) lose "globals"; the
  `GLOBALS_INDEX` re-export (`:393`) goes.
- `model_matches.rs:208, :275-278`: `expand_port` yields `Root::Global(id)`;
  `PortPair::globals()` is no longer pushed. `flowy.rs:615-628`: `port_to_index` returns
  `Root::Global(id)`.
- `ctadl-ascent/src/models/spec.rs:355-368`: `PortPair::globals()` goes; `BridgePort::parse`
  (`:328-335`) says "a global is shared and needs no bridge" (R29).
- `ctadl-ascent/src/languages/jni.rs:614-616`: the globals pair goes; `emit_bridge` emits the
  written pairs and the return only (R29).

Files, rules (`ctadl-index/src/index_engine/mod.rs`, one text for the three engines, R21):

- Seeds: `global_mention(FunctionId, GlobalId)` from every `assign_like`, `callee_info`,
  `call_target_assign` and `const_str_assign` row whose base is a global; the globals seed
  beside the formal seed (`:1566-1569`); `GLOBAL_FRAME` seeded for every global.
- Root flows: the summary rule (`:1762-1769`) and the aliasing summary rule (`:1784-1798`)
  derive `root_flow`; the three rules of spec §3.7 split it into `summary` (mixed roots, or
  two global roots under a decision, R18) and `assign_like(GLOBAL_FRAME, …)` (two global roots,
  `Always`, R16). `compute_alias_of_formal` (`:1340-1376`) starts from mentioned globals too
  (R19).
- Label exchange, by path (R17, spec §3.7): `global_read(FunctionId, GlobalId, Path)` from
  every `assign_like` source and `callee_info` callee whose base is a global; labels go up
  from every function to `GLOBAL_FRAME`, and come down only at the paths a function reads and
  their extensions, as a keyed prefix join in the style of `call_target_key_wild`
  (`:2130-2138`), for `call_target_assign_like` and for `const_reaches`; the frame is an
  `intent_frame` whenever any function is. Not by `global_mention`: on pcode every function
  mentions `$<import>::ram`, and the by-mention form is functions × labels (spec §5).
- Hybrid inlining: rule 1.1 is already over `Root::Formal` (2a); the return-direction tag
  rules (`:2155-2179`) and `tag_closure_func` lose their globals case.
- Each rule touched gets the R26-style doc comment naming its G-rule.

Files, query (`ctadl-ascent/src/query_engine`):

- `search.rs`: `heap_calls_by_caller` (`:146, :276-296, :362, :555-562`) goes; the exit edge
  (`:509-534`) has no globals case; `reads_of_global` indexes every `assign` row whose source
  base is a global; `FlowEdge::Global` into and out of `(GLOBAL_FRAME, $g, p)` sets
  `TaintState` (`:568-590`) to its initial state (R24, R25); `tainted_insn` (`:796`) guards on
  `formal() >= 0` alone (R27).
- `endpoints.rs:162, :228-231`: a `Global` endpoint has `infunc = GLOBAL_FRAME`; the
  `out_formals` row goes (R26).
- `formatter.rs`: a vertex at `GLOBAL_FRAME` is located by `globals.parquet`'s span, else the
  logical location `<program>::$name` (R27).

Files, CLI and `--summary` (`ctadl-ascent/src/cli/mod.rs`, `src/main.rs`):

- `load_and_map_summaries` (`cli/mod.rs:1621-1680`): reads the summary project's
  `globals.parquet`, interns each name into the consumer's table, rewrites global roots to the
  consumer's ids, and loads `global_flow.parquet` as `GLOBAL_FRAME` edges (R20).
- `ctadl index` logs, at `info` beside the JNI bridge's lines, globals per program and the
  names declared by more than one program; the per-name list at `debug` (R23).
- `inspect_artifact` (`main.rs:1072`) prints a program's globals.
- `frontends/ctadl-c/src/test_utils.rs:6, :858-894`: the summary helpers take `Root::Global`.
- `ctadl-ascent/src/models/defaults/native-index.ctadl:276-285`: the comment is rewritten to
  the remaining reason (R31); `skip_analysis` stays.
- `docs/approximations.md`: the "call target held in a global" row is removed, the `Shared`
  formals row narrowed, the "hop through a global" note added (§4).

Tests:

- Codegen, `ctadl-ascent/src/codegen/tests.rs`: no function has a globals formal and no call
  site a globals actual; a store to `$g.f` is an `assign` row with a `Global` destination
  vertex; two programs declaring `Lcom/C;` intern to one `GlobalId` and `IndexFacts.globals`
  has two rows for it. `model_matches/tests.rs:165-188, 222-234, 335-355` and
  `jni/tests.rs:189-209` (`return_and_globals_appear_exactly_once`) pin the absence of the
  globals pair (R38).
- Index, `ctadl-ascent/src/index_engine_tests.rs` (hand-built IR, each engine): R15 (a
  function that copies `$a.f` to `$b.g` has no `summary` row; the frame has the edge); R16 (a
  flow from `$a.f` in one function reaches `$b.g.h` through a frame edge closed under the
  field rules); R17 (a target stored to `$g.handler` in one function resolves an indirect call
  in a function with no caller, with `Cond::Always`); R18 (a global-to-global flow under a
  decision is a conditional summary row, applied by rule 3.2 in the caller whose route
  establishes it, where it becomes a frame edge); R19 (the backward rule through `l = $g.x;
  l.f = v`); R20 (a `--summary` round trip: a flow in through one library function, held in a
  library global, out through another, found in the consumer).
- Query, `ctadl-ascent/tests/integration/`: a witness through a global that returns from the
  loading function after a call was taken before the store (R25); a source and a sink on a
  global path, anchored to no call site (R26); the SARIF location of a global vertex with and
  without a declaration span (R27).
- Parity: the `// parity:` lines of stage 1's `g02_shared`, `g03_two_programs`,
  `g06_target_via_global` and `g25_hop_after_call` go, with the register's two temporary rows
  ("global passed at calls", "call target held in a global") and their `ROWS` entries in
  `reference_parity.rs`; `reference_parity.rs` is green on the corpus with every remaining difference a register
  row (R36). The `.tnt` cases are unchanged in expectation (no program text changes here).
- Nightly: `cargo xtask regression` passes; the JNI cases `JniFlow` and `Jni:JniFlow+bridge`
  pass with the globals pair gone, by R24; `nightly/tests/jni/jni-flow.bridge.ctadl`'s comment
  on the globals pseudo-parameter is rewritten. Every changed answer named to a G-rule.
  `--compare-paths`: every change is `$globals.<name>.p` → `$<name>.p` (R39).
- Measurement (R40), recorded in the PR: on the nightly binaries and one real APK, `summary`
  rows with two global roots are zero; total `summary` rows, the `call_target_assign_like`
  and `const_reaches` rows at `GLOBAL_FRAME` and in all other functions, and index time
  before (end of stage 2a) and after. On the binaries the label rows outside the frame must
  not exceed the pre-change count: that is the check that the exchange is by path.
- Index, `index_engine_tests.rs`, for the exchange's shape: a label stored at `$g.a` in one
  function is present in a second function that reads `$g.a` and absent from a third that
  reads only `$g.b`; a function that reads `$g` whole, `l = $g`, sees the label at `$g.a`
  (an extension of what it reads) and resolves `(*l.a)()`.
- `grep -rn 'GLOBALS_INDEX\|is_globals\|heap_calls_by_caller\|GlobalHeap\|new_global'
  --include='*.rs'` is empty.

As built (commit `deb6dc4b`). Where the implementation departs from the above:

- **Rules.** No `root_flow` relation: the formal-destination summary rule is unchanged (its root
  comes from `as_root`), and two rules beside it read `locals` rows whose vertex is a global in a
  function other than the frame, giving a `summary` row unless both roots are global and the row
  is `Always`, in which case it is an edge of the frame. The aliasing rule derives `alias_flow`,
  which splits the same way. `compute_alias_of_formal` seeds a global from every original copy
  out of it in a function (the "mentioned globals" of the plan, restricted to the ones a copy can
  alias). `global_mention` is recursive with `assign_like`, so it covers instantiated edges, and
  the frame mentions every global. The frame's `locals` (R16's closure) are computed but not
  written out: `global_flow.parquet` holds the frame's *edges*, which a consumer closes again,
  rather than their closure.
- **Label exchange.** One key relation for both labels, `global_label_key(g, key, p)` over the
  exact splits (`tag_splits`) of the frame's labelled paths, joined with `global_read(f, g, key)`
  into `global_label_read(f, g, p)`, which brings both `call_target_assign_like` and
  `const_reaches` down. Only exact prefixes: a read at `$g.[8]` does not get a label at
  `$g.[16].deref` the way `call_target_key_wild` would; no case needs it yet. A function that
  receives a label is added to `tag_closure_func`, as is one that holds a target and mentions a
  global. A constant in a body clause is written `&GLOBAL_FRAME` (Ascent compares by reference).
- **Query.** Every search index is keyed by `home(f, v)`, the frame for a global, and its entries
  carry the function of the edge, so a global's loads in every function hang off the one frame
  vertex. Successors are canonicalized at the end of `labeled_successors`: a global vertex moves
  to the frame, and a step between a function and the frame is `FlowEdge::Global` (tag 3 in
  parquet), which sets `TaintState::Free` in both the search and the formatter. No separate
  `reads_of_global` map.
- **Models.** The `Global` model tag has no spelling until stage 3, so `expand_port` and the
  endpoint builder treat it as unreachable here; stage 3 gives it a `GlobalId`.
  `TryFromVariableError::Global` stays (a `mir::Variable::Global` has no name without its
  program's table), with a message saying so.
- **R23** is `IndexFacts::global_declarations` plus `report_globals` in `cli`, logged right after
  the JNI link, before the models. `ctadl inspect` already printed a program's globals (stage 1).
- **R27** needs nothing in the formatter for a flow through a global: its steps are its store
  and its load, which are instructions. A source or sink *on* a global arrives with stage 3.
- **C test helpers.** `check_param_into_global_in` and `check_returns_global_in` keep their
  `.g.deref` argument and read its first segment as the global's name;
  `check_no_param_into_global_in` and `check_returns_global_at_in` replace the
  `GLOBALS_INDEX` calls of `check_no_flow_in` / `check_flow_in`.
- **Parity.** The engines match the reference on `g02_shared`, `g03_two_programs`,
  `g06_target_via_global` and `g25_hop_after_call`, so their `// parity:` lines, the two temporary
  register rows and their `ROWS` entries are gone, and the register's §4 edits for 2b are made.
- **Tests.** `index_engine_tests.rs` gains a `globals` module, written as C programs:
  `a_flow_between_two_globals_is_a_frame_edge_not_a_summary` (R15, R16, G5),
  `a_flow_between_a_formal_and_a_global_is_a_summary_rooted_at_the_global` (R14, R15),
  `a_target_stored_to_a_global_resolves_a_call_in_a_function_with_no_caller` (R17, directly and
  through a frame edge), `labels_come_down_at_the_paths_a_function_reads` (R17's shape, including
  the whole-cell read),
  `a_conditional_flow_between_globals_lands_in_the_frame_at_its_establishing_caller` (R18),
  `a_store_through_a_copy_of_a_global_reaches_the_global` (R19),
  `two_imports_declaring_one_name_share_one_global` (G3, R22, R23's counts) and
  `a_summary_projects_global_flows_cross_into_its_consumer` (R20, through the query with a
  `Global` step). The query's R25 case is the corpus's `g25_hop_after_call`, now without a parity
  line. JNI and bridge tests pin the absence of the globals pair (R38). 1937 workspace tests pass.
- **End to end.** `ctadl go` on `nightly/tests/c/globalflow.c` with `main` removed (no function
  calls both `produce` and `consume`) reports the flow; the stage-1 binary reports none.
- **Regression** (R37, R39), `--release`, `--compare-paths` against 2a: 254 of 259 passed, the
  same 4 xfail, and one failure, `Cil:delegates`, whose expectation is re-baselined: every sink
  is still reached and the clean one still not, but `Handler`'s source line (55) is no longer on
  a reported path. The CIL front end makes a delegate its target object (`calls.rs`), and the
  target of every non-capturing lambda of a class is the compiler's static singleton
  `<>c.<>9`, so all of them store their method at the one global path
  `$<>c.<>9.<delegate::method>`. Stage 2a hid that behind the "call target held in a global"
  deviation; under G6 the label is shared, so `Lambda`'s `f(..)` may call `Handler`'s lambda,
  and `Sink4` is reached first from `Lambda`'s source (35). That is a false positive of the front
  end's convention made visible by G6, registered as a new "adds" row (CIL delegates) and in
  `docs/limitations.md`; the fix is the front end's (a non-capturing lambda's delegate need not
  be its target), outside this spec. `JniFlow` and `Jni:JniFlow+bridge` pass with the globals
  pair gone (R29). `--compare-paths`: 77 indexes identical, 146 different, every difference the
  removal of a global's name segment (`.g.p` on the pseudo-formal to `.p` on `$g`), checked by a
  script that deletes, from each 2a path, every segment naming a global some project of the case
  declares (the `+summary` JNI cases' library global is the summary project's) and requires the
  two sets to correspond; 0 unexplained.
- **Measurement** (R40), re-indexing each case's imports with the 2a and the 2b release builds and
  `CTADL_DUMP_RELATIONS`. The nightly binaries (`nightly/tests/c/*.c` through Ghidra, 27
  relocatable objects) are too small to measure: at most 8 summary rows and 5 label rows, equal
  before and after, and no two-global row. The real APK (`xtask/tests/dex/com.noto_54.apk`):

  | | 2a | 2b |
  | --- | --- | --- |
  | `summary` rows | 140898 | 83088 |
  | of which with a global root | — | 20298 |
  | of which with two global roots | — | 0 |
  | `call_target_assign_like`, outside the frame | 607834 | 798653 |
  | `call_target_assign_like`, in the frame | — | 4394 |
  | `const_reaches`, outside / in the frame | 5637 / — | 5642 / 11 |
  | `tag_closure_func` | 22083 | 24403 |
  | index wall time (s) | 32.7 | 36.5 |

  Summaries shrink by 41%: no transitive caller carries a global-to-global flow any more (R15).
  Label rows outside the frame grow by 31%. 55435 of 2a's rows were on the `$globals`
  pseudo-formal and are gone; of the 246285 new ones, 42727 are on global vertices and the rest
  are what those labels reach inside the functions that read them, which 2a derived only along
  a call chain that established a decision (the "call target held in a global" loss). The
  exchange is by path: `global_label_read` has 26084 rows. R40's bound ("label rows outside the
  frame not above the pre-change count") holds on the binaries only trivially, since they hold
  almost no labels; the APK is where the exchange costs, and it costs the lost flows it
  recovers. A large stripped binary is the measurement still missing.

## Stage 3: models (R28, R32, R38's model part)

One PR after stage 2b.

Files:

- `ctadl-ascent/src/models/portcullis/{relations,parse,ast,check,eval,emit}.rs`: the input
  relation `global(G)` with attributes `name` and `import`, over `IndexFacts.globals`
  collected at import (a shared global matches each declaring import); the port `G::self.path`
  (`parse.rs:233` beside `local`) to `PortBase::Global` with the path in normal form; a
  `global(G)` that matches nothing reported as `CTADL0004`; `emit.rs:824`: `G::self` in a
  `bridge` rejected with "a global is shared and needs no bridge".
- `ctadl-ascent/src/models/mod.rs:322`: `FormalIndexTypeTag::Global` carries the `GlobalId`;
  `spec.rs:328` resolves it; a propagation whose port is a global emits a `summary` row with a
  global root (G7).
- `ctadl-ascent/src/models/portcullis/migrate.rs:1365-1378` and
  `docs/deprecated-json-model-generators.md:736-743`: an empty `arguments` map bridges
  nothing, and globals need no bridge (R32).
- `docs/portcullis.md`: §3 lists `global`; §6 drops "Globals are mapped unconditionally"
  (`:538`); `:285` and `:743` and §9's migration note follow.

Tests:

- `ctadl-ascent/src/models/portcullis/tests.rs`: `global(G, name = "g")` matches; `G::self.deref`
  as a source and as a sink; `G::self.f` in a propagation emits the global-rooted summary row;
  `G::self` in a bridge is the new error; an unmatched `global(G)` is `CTADL0004`.
- `ctadl-ascent/tests/portcullis.rs` and `tests/integration/{bridging_models,
  bridging_end_to_end, port_semantics, migration_equivalence}.rs`: re-baselined where a case
  pinned the globals pair, each naming R29 or R32.
- End to end: a C program with a source on `G::self.deref` of `g` and a sink in another
  function reports the flow with no call between them (R26, R28).

As built (commit `a8ea91f9`). Where the implementation departs from the above:

- **Tag and port.** `FormalIndexTypeTag::Global(GlobalId)`, and the port base is
  `PortBase::GlobalSelf`, spelled `self` and anchored at a variable bound by `global(G)`
  (`VarType::Global`, `Value::Global { name }`, `Row::Global`). `global` is indexed by its
  column (the name), so `global(G, name = "g")` scans one row per global.
- **Endpoints.** A source or sink on a global names the function `<globals>`
  (`GLOBAL_FRAME_NAME`), which every `IdMap` resolves to `GLOBAL_FRAME`, so query endpoint
  building needs only the tag's vertex and `anchored_at_callsites` returns it unchanged (R26).
  A wildcard sink on a global expands over the paths the global has in any function.
- **Propagation.** The function is the anchor of the port that is not a global's; two global
  ports are an error ("a flow between two globals … needs no model"), as are a `self` port on a
  non-global, a global with a non-`self` port, and `self` in a bridge ("a global is shared and
  needs no bridge") or another head. A global root adds no `formal_param` row.
- **CTADL0004.** A `global` atom is probed like a `local` one (`Plan::global_probes` marks which);
  one that ran and never matched gives `UnmatchedReason::GlobalNotFound`, message id
  `globalNotFound`.
- **R27 in the formatter.** No front end records a global's declaration span, so a source or sink
  on a global is named `global <import>::$name` and located by a logical location of kind
  `variable` with that name, the imports read from `globals.parquet` (each declaring import,
  comma-separated). A *flow* into a sink on a global is at no instruction, so no tainted
  instruction's details name its sink; the path search also pairs every source an instruction
  names with every sink on a global, and reports the path at that instruction.
- **Tests.** `ctadl-ascent/tests/portcullis.rs`:
  `global_matches_by_name_and_a_self_port_names_the_global`, `a_propagation_names_a_global_port`,
  `a_self_port_is_rejected_where_a_global_cannot_be`, `a_global_that_matches_nothing_is_reported`,
  and `Needs` for `global`. `tests/integration/global_ports.rs`, end to end on C:
  `a_source_on_a_global_reaches_a_sink_with_no_call_between` (R26, R28, and R27's logical
  location), `a_source_on_an_unread_global_reaches_nothing`,
  `a_sink_on_a_global_is_reached_from_any_store`. 1944 workspace tests pass. The integration
  tests that pinned the globals pair were already rewritten in 2b (R38).

## Stage 4: documentation (R33's remainder, R34's edits)

Files:

- `docs/spec.md`: §2 gains a *Global* row; §4.2's "Globals" bullet (`:164`) is rewritten to
  G2, G5, G6 and the frame; §4.3's bridge sentence drops globals; §6 drops the C limitation.
- `docs/approximations.md`: the §4 table of the spec applied in full (stages 1b and 2 made the
  row edits; this pass checks the table against spec §4 line by line and adds the pcode
  "address of a global is an inert constant" row).
- `docs/limitations.md`: the pcode row and the C initializer row (§4).
- `docs/debugging.md`: `GLOBAL_FRAME`, `globals.parquet`, `global_flow.parquet`, the `Root`
  columns of `summary.parquet`.
- Each front end's crate doc has its convention paragraph (checked, written in stages 1 and
  1b). `README.md`: the one-sentence description of globals.

Tests: `cargo doc --no-deps` clean; `reference_parity.rs` still finds every `// parity:` row
in `docs/approximations.md`.

As built. `docs/spec.md` gains the *Global* concept and a *Summary* that names globals, rewrites
the Summaries and Globals bullets of §4.2 (the frame, the label exchange, the query's hop), says a
bridge maps nothing for globals (§4.3), and says what crosses `--summary` (§6); §6 had no C
global limitation to drop (it lives in `docs/limitations.md`, which stage 1 edited). The register
gains the pcode "address of a global" row, the `--summary` row of spec §4, and the CIL delegate
row stage 2b's regression found; checked against spec §4 row by row. `docs/limitations.md` gains
the pcode and CIL delegate rows. `docs/debugging.md` explains the root columns of
`summary.parquet`, `GLOBAL_FRAME`, `globals.parquet`, `global_flow.parquet` and the global tag of
a vertex. `README.md` says a summary project's flows between globals cross `--summary`. Every
front end's crate doc already stated its convention (stage 1). `cargo doc --no-deps --workspace`
adds no warning; the ones it prints are older than this work (private and redundant links in
untouched text). `docs/portcullis.md` and the deprecated JSON doc were edited with stage 3.

## Acceptance

- `cargo test` across the workspace; `cargo xtask regression` for every front end, each changed
  answer attributed to a G-rule, an R-number or a register row in the PR that changed it
  (R37): `globalflow.c`, `globalstruct.c`, `StaticFieldFlow`, `CrossClassStaticFieldFlow`,
  `StaticLongChain`, `JniFlow`, `Jni:JniFlow+bridge`.
- Reference parity (R36) green on the corpus, including the R35 cases, with every difference a
  row of `docs/approximations.md`; `.tnt` and `index_engine_tests.rs` re-baselined only where
  program text changed under R8–R12, each naming its cause.
- `--compare-paths` across stages 1, 1b, 2a and 2b shows renames and the C `deref` insertion
  only, and no change at all across 2a (R39).
- R40's table in stage 2b's PR: zero two-global summary rows; total summary rows, label rows
  in and outside `GLOBAL_FRAME`, and index time on the nightly binaries and one real APK,
  before and after; label rows outside the frame not above the pre-change count on the
  binaries.
- `grep` finds no `GlobalHeap`, `new_global`, `GLOBALS_INDEX`, `is_globals`,
  `heap_calls_by_caller`, `EmptyGlobalsWrite`, `PortPair::globals`, `$globals`.
