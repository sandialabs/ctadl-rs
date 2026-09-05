# Changes to CTADL to make it a library this analysis can use - DO-NOT-MERGE

`ctadl-comparison.md` ends up making two admissions about the front end, and
both are really about CTADL's *API*, not about either analysis:

1. **This crate re-implements `ctadl index`'s preamble by hand.** `read_import`
   (`src/ctadl.rs:143`) hard-codes the store layout and the bitcode filenames,
   and `Preprocess::ctadl()` (`src/ctadl.rs:219`) is a hand copy of four calls
   that live at `ctadl-ascent/src/cli/mod.rs:301-304`. Neither is exported in a
   form that can drift-check itself. When CTADL bumps `IMPORT_FORMAT_VERSION`
   (`ctadl-ascent/src/project.rs:95`, currently `"5"`) this crate does not
   notice — it reads the raw bitcode and either decodes garbage or fails with a
   `bitcode::Error` that names no cause.

2. **Two of this analysis's features are unreachable on any dex/APK import,**
   because the dex front end throws the information away before the IR exists.
   `load_index_var` / `store_index_var` are never populated (`src/ctadl.rs:84`),
   so the critical statements of §4.1.3 collapse to *unresolved dispatch* alone
   — the array-indexing half of the paper's story has never run on real input.
   And `const_assign` carries `"$bytes:4"` rather than a value
   (`constant()`, `src/ctadl.rs`), so `PtVal::Const` is 31% of `points` under
   `--ctadl-pre` while being, in every case, an opaque token.

This document is the plan for fixing both **inside `../ctadl-rs`**, in an order
where each step is independently reviewable and none of them changes what
`ctadl index` produces by default.

---

## Part 0 — the facts, as the code stands

### The passes

```rust
// ctadl-ascent/src/cli/mod.rs:301-304, inside the per-import loop of `index()`
ssa::eliminate_dead_temps(&mut program_info.program);
ssa::coalesce_copies(&mut program_info.program);
ssa::transform_program(&mut program_info.program, prune_unreachable_cfg_nodes);
ssa::propagate_copies(&mut program_info.program);
```

All four are already public from `ctadl-ir` (`ctadl-ir/src/ssa/mod.rs:26,29,32,50`),
which is why this crate could adopt them at all. What is *not* public is the
fact that this is the pipeline, in this order, with `prune` wired to an index
option. That knowledge exists only as four adjacent lines in a CLI function.

### The reader

`load_program_info_without_source_info` (`ctadl-ascent/src/cli/mod.rs:985`) is
**private**, and it is exactly the function this crate reimplements. Its public
sibling `save_program_info` (`:942`) is exported. The asymmetry is the whole
problem: CTADL exports the writer and hides the reader.

The store layout it reads is public (`project.rs:102` `PROGRAM_BITCODE_FILE`,
`:106` `VMT_BITCODE_FILE`, `:730` `IMPORTS`), and `ArtifactImport::load`
(`:273-295`) does the version check — but all of it lives in `ctadl-ascent`,
whose dependency list is datafusion, arrow, parquet, ascent, tree-sitter,
tokio and Ghidra plumbing. Depending on it to read a 6 MB bitcode file is not
a trade this crate can make; hence the hand copy.

### What the dex front end drops

**Array indexes.** `ctadl-ascent/src/languages/dex/mod.rs:797-813` (`aget`) and
`:815-838` (`aput`):

```rust
Instruction::AGet(f) | ... => {
    let array_var = reg_to_var(code_item, f.b, locals);   // the array
    // f.c -- the index register -- is never read
    stmts.push(Statement::new_kind(StatementKind::load(
        dest_var, array_var.clone(), FieldPath::symbol("[]"),
    )));
}
Instruction::APut(f) | ... => {
    // sources are filtered:  .filter(|r| *r != f.b && *r != f.c)
    // so both the array and the index are removed from the flow
}
```

The index register is not merged, not approximated, not recorded: it is
dropped at the point of lowering. JVM does the same (`languages/jvm/mod.rs:1072,1096`).
This is *sound* for a taint index — every element of the array is one vertex,
so a write at `i` is seen by a read at `j` — and it is the reason
`ctadl-comparison.md` could only ever measure the dispatch half of hybrid
inlining.

**Constants.** `dex/mod.rs:598-676` does keep them, but as bytes:

```rust
Instruction::Const4(f)  => Some(Exp::new_bytes(f.lit.to_be_bytes().to_vec())),
Instruction::Const16(f) => Some(Exp::new_bytes(f.lit.to_be_bytes().to_vec())),
Instruction::Const(f)   => Some(Exp::new_bytes(f.lit.to_be_bytes().to_vec())),
```

`Exp` (`ctadl-ir/src/mir/mod.rs:419`) has five variants — `Variable`,
`AccessPath`, `Str`, `Bytes`, `ObjectRef` — and **no integer**. So:

- the *same* value has different representations depending on which dex opcode
  produced it: `const/4 v0, 1` is `[0x01]` and `const/16 v0, 1` is `[0x00, 0x01]`,
  two distinct `Exp::Bytes`, two distinct constants to any consumer;
- there is no width, signedness or numeric identity to recover, which is why
  `constant()` in this crate degrades every one of them to `$bytes:<len>`;
- literal-carrying arithmetic (`add-int/lit8`, `new-array` size,
  `fill-array-data`, `packed-switch` keys) falls through the `_ =>` arm at
  `dex/mod.rs:650-670` and loses the literal entirely.

There is no pressure inside CTADL to fix any of this, because its own codegen
discards constants one layer later: `trans_exp` (`codegen/mod.rs:846-854`)
returns `None` for `Str`, `Bytes` *and* `ObjectRef`. A constant that reaches
the IR is dead weight to `ctadl index` and load-bearing to this analysis.

### What already exists, unmerged

`../ct-unknown-offset/variable-offset-ir.md` (on branch `unknown-offset`,
untracked, 342 lines) is a complete design for exactly the missing IR
construct: a `VarOffset(Option<VariableRef>)` variant on `FieldAccess` and
`PathSegment`, spelled `.[*]`, matching only itself, with the index variable
riding on the IR and canonicalised away in `facts::Path`. It is written to be
a **no-behaviour-change** commit — nothing in the tree emits `.[*]` when it
lands — and it explicitly puts the front ends out of scope, with the note that
"Dex and JVM are already index-insensitive and therefore sound", so their `[]`
collapse is "staying permanently".

That last decision is the one this plan revisits — not by overturning it, but
by making it a mode (see 2.2). Everything else in that document stands and
should be implemented as written; it is the prerequisite for the array half.

---

## Part 1 — the entry point: read, import, preprocess, in one call

Three steps, smallest first. Step 1.1 alone removes the drift risk on the
passes and costs nothing; 1.2 is the real work; 1.3 is a small correctness fix
that 1.2 makes possible.

### 1.1 `ctadl_ir::ssa::Pipeline` — name the pipeline where the passes live

New in `ctadl-ir/src/ssa/mod.rs`:

```rust
/// The IR-to-IR passes that must run between reading an import and generating
/// facts. Order is a property of the pipeline, not of the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pipeline {
    pub dead_temps: bool,
    pub coalesce: bool,
    pub ssa: bool,
    pub copy_prop: bool,
    /// Passed to `transform_program`; ignored unless `ssa`.
    pub prune_unreachable: bool,
}

impl Pipeline {
    /// Exactly what `ctadl index` runs, and the only definition of that.
    pub fn index_default() -> Self { ... }   // all four, prune = true
    pub fn none() -> Self { ... }
    pub fn ssa_only() -> Self { ... }
    /// A stable string for provenance -- e.g. "dt+co+ssa(prune)+cp".
    pub fn tag(&self) -> String { ... }
}

pub fn run_pipeline(program: &mut Program, p: Pipeline) { /* the four calls */ }
```

`cli::index` (`:301-304`) becomes one call:

```rust
ssa::run_pipeline(
    &mut program_info.program,
    ssa::Pipeline::index_default().prune(prune_unreachable_cfg_nodes),
);
```

Why in `ctadl-ir` and not in the new crate of 1.2: the passes are there, the
type has no new dependencies, and **this crate already depends on `ctadl-ir`
alone**. So step 1.1 is usable here the day it merges — `Options::preprocess`
(`src/ctadl.rs:256`) becomes a re-export of `ssa::Pipeline`, and
`ctadl-comparison.md`'s claim that "it is the same four passes `ctadl index`
runs" becomes checkable by the compiler instead of by reading a CLI function.

Add one test in `ctadl-ir`: `Pipeline::index_default()` is idempotent on an
already-preprocessed program (all four passes are documented no-ops on SSA
input; flowy imports rely on that already).

**Effort:** an afternoon. **Risk:** none — pure refactor, byte-identical output.

### 1.2 Extract `ctadl-frontend`: a crate that turns artifacts into IR

The blocker is that `languages/` is stuck in a crate that also contains the
Datalog engine. The good news, from the actual `use` lines, is that the
coupling is one module deep:

```
languages/dex/mod.rs:16     use crate::error::{Error, ErrorContext};
languages/jvm/mod.rs:13     use crate::error::{Error, ErrorContext};
languages/lua/mod.rs:92     use crate::error::{Error, ErrorContext};
languages/pcode/mod.rs:12   use crate::error::{Error, ErrorContext};
```

That is the *entire* crate-internal dependency of the four core front ends.
`apk_native.rs` and `xapk.rs` add `crate::project::{ArtifactImport,
ArtifactLanguage}` and `crate::cli::{ImportOptions, import, save_program_info}`;
`project.rs` itself needs only `crate::error`. `jni.rs` is the exception — it
reaches into `crate::facts`, `crate::codegen` and `crate::index_engine`, because
JNI *linking* emits facts. Its import-time half (`jni/registry.rs`, the
`RegisterNatives` scan) needs only `error` + `project`.

So the split is:

```
ctadl-frontend/                      # new crate, ctadl-ir + readers only
  src/error.rs                       # the import-reachable half of ctadl-ascent's Error
  src/store.rs                       # from project.rs: StorePaths, ArtifactImport,
                                     #   IMPORT_FORMAT_VERSION, program/vmt paths
  src/languages/{dex,jvm,lua,pcode,tree_sitter_c,apk_native,xapk,jni/registry}
  src/lib.rs                         # the entry points below
```

`ctadl-ascent` then depends on it and re-exports for source compatibility
(`pub use ctadl_frontend::{languages, project};`), keeps `jni.rs`'s
fact-emitting half, and `cli::import` becomes a thin wrapper over
`ctadl_frontend::import_artifact` plus the store write.

**The error split.** `ctadl-ascent`'s `Error` (`error.rs:174`) carries
datafusion, parquet, arrow, ascent-side and tree-sitter variants. Split it:
`ctadl_frontend::Error` gets `Io`, `Path`, `Json`, `Json5`, `Bitcode`, `Dex`,
`Jvm`, `Verify`, `TreeSitter*`, `Pcode*`, `NothingToImport`,
`IncompatibleImport`, `SourceInfoParquet`; `ctadl_ascent::Error` keeps the rest
plus `#[error(transparent)] Frontend(#[from] ctadl_frontend::Error)`.
`ErrorContext` moves with it. Every `?` in the moved files keeps working; the
call sites in `ctadl-ascent` that consume them need the `From` impl and nothing
else.

**Feature flags,** so a consumer pays for what it reads:

```toml
[features]
default = ["dex", "jvm", "apk"]
dex   = ["dep:dex-reader"]
jvm   = ["dep:jvm-reader"]
apk   = ["dex", "dep:zip"]
pcode = ["dep:pcode-reader"]          # + Ghidra discovery
lua   = ["dep:tree-sitter", "dep:tree-sitter-lua"]
c     = ["dep:tree-sitter", "dep:tree-sitter-c"]
```

`ctadl-ascent` enables all of them, so its build is unchanged. This crate would
enable `dex, apk` and drop tree-sitter, Ghidra, ascent, datafusion, tokio and
serde-sarif from its tree entirely.

**The API.** Four functions, all of which currently have no public form:

```rust
/// Artifact on disk -> IR, with no store involved at all.
pub fn import_artifact(path: &Path, lang: ArtifactLanguage, opts: ImportOptions)
    -> Result<ProgramInfo, Error>;

/// Read an import `ctadl import` already cached, version-checked.
/// This is `cli::load_program_info_without_source_info`, made public and
/// given a `SourceInfo` switch.
pub fn load_import(import: &ArtifactImport, src: SourceInfo) -> Result<ProgramInfo, Error>;

/// The one-liner: name-or-directory in the store -> preprocessed IR.
/// Equivalent to load_import + ssa::run_pipeline, and the entry point this
/// whole document exists for.
pub fn open_import(name_or_dir: &str, pipeline: ssa::Pipeline)
    -> Result<ProgramInfo, Error>;

/// Import if absent or stale, then open. What a downstream tool wants when it
/// has an APK and does not care whether the store is warm.
pub fn open_or_import(name: &str, artifact: &Path, opts: ImportOptions,
                      pipeline: ssa::Pipeline) -> Result<ProgramInfo, Error>;
```

Note `ProgramInfo` (`ctadl-ir/src/mir/mod.rs:703`) already bundles
`program + vmt + source_info`, so the entry point returns one value where this
crate currently juggles two files and a hand-rolled `bitcode::deserialize` for
the VMT (`src/ctadl.rs:158-161`, with the comment "the VMT is written by
`bitcode::serialize` directly, with no helper in `ctadl_ir::encode` to mirror").
Add `encode::{encode_vmt, decode_vmt}` to `ctadl-ir` while there, so the
asymmetry that forced that comment is gone.

**Effort:** two to three days, most of it mechanical `crate::` → `ctadl_frontend::`
and the error split. **Risk:** moderate but bounded — no logic moves, and
`cargo test -p ctadl-ascent` is the whole check.

**Cheap alternative if the extraction is not wanted:** make
`load_program_info_without_source_info` public, add `cli::open_import` next to
it, and gate `ctadl-ascent`'s heavy halves behind a default-on `engine` feature
(`datafusion`, `ascent`, `query_engine`, `index_engine`, `codegen`). Same API,
one crate, but the feature graph inside a 19 kLOC crate is harder to keep
honest than a crate boundary. Recommended only if crate churn is the objection.

### 1.3 Make the version check reachable

Today this crate reads `ir-program.bitcode` directly, so an import written by a
newer CTADL decodes as garbage or as a `bitcode::Error`. `ArtifactImport::load`
(`project.rs:273-295`) already produces the right diagnostic
(`Error::IncompatibleImport`, naming the original artifact path and telling the
user to re-import) — it just is not on any path this crate can call. Step 1.2
puts it on one. `open_import` must go through `ArtifactImport::load`, never
around it.

While there: `IMPORT_FORMAT_VERSION` should be readable without constructing an
`ArtifactImport` (`import_format_version_beside` at `:159` nearly does this),
and the bump in Part 2 is the first real test of it.

**Also worth doing, unrelated but free:** `ctadl-ir/Cargo.toml` declares
`arrow = "57"` and `parquet = "58"` and **uses neither** (no `use` of either in
`ctadl-ir/src`). They arrive transitively through `source-info` anyway, so
dropping the direct deps removes a duplicate `arrow` major from the build
graph. Feature-gating `source-info`'s `parquet_io` would remove them from a
`ProgramInfo`-only consumer entirely, which is the last thing standing between
this crate and a genuinely light dependency tree.

---

## Part 2 — engine changes, so the analysis can use its own features

### 2.1 Real constants: `Exp::Int`

**Change.** Add to `ctadl-ir/src/mir/mod.rs:419`:

```rust
pub enum Exp {
    Variable(VariableRef),
    AccessPath(AccessPath),
    Str(ArcIntern<str>),
    Bytes(Vec<u8>),
    ObjectRef(CallObject),
    /// An integer constant, sign-extended to i64 and canonical: the *value*,
    /// not the encoding the opcode happened to use. Width, where a consumer
    /// needs it, is a property of the type, not of the constant.
    Int(i64),          // appended last: PathSegment/Exp Ord and bitcode
}                      //   variant order both matter (see below)
```

Constructors `Exp::new_int`, accessor `as_int`, `Display` as decimal
(hex in a side comment, per the `Display for Offset` rule at `mod.rs:238-245`).

**Front ends.** In `dex/mod.rs:598-676`, every `Const*` arm becomes
`Exp::new_int(f.lit as i64)`; `ConstWideHigh16`/`ConstHigh16` keep their shift
and stop round-tripping through bytes. `Bytes` survives for what it is actually
for — `fill-array-data` payloads and pcode blobs. JVM's `ConstantValue`
(`jvm/mod.rs`, via `jvm_reader::flow::ConstantValue`) gets the same treatment.

**Optional, opt-in:** the literal-carrying arithmetic that falls through
`dex/mod.rs:650-670` (`add-int/lit8` and friends, `new-array`'s size operand)
can emit the literal as an additional source. This is a modelling choice, not a
fidelity fix — it makes `d` hold both `s`'s values and the literal — so it
belongs behind an `ImportOptions` flag (`literals: bool`, default off) and
should land after 2.2, where a constant array size actually buys something.

**`ctadl index` impact:** none. `trans_exp` (`codegen/mod.rs:846-854`) returns
`None` for `Bytes` today and will return `None` for `Int`; add the arm
explicitly rather than leaving it to `_ =>`, so the next person has to decide.

**Format version:** `bitcode` encodes an enum's discriminant in
`ceil(log2(variants))` bits, so adding a sixth `Exp` variant changes the wire
format of every program. Bump `IMPORT_FORMAT_VERSION` to `"6"`
(`project.rs:95`) in the same commit. Users re-import; the error message at
`:289` already tells them so by name.

**What it unlocks here:** `constant()` (`src/ctadl.rs`) stops emitting
`$bytes:<len>` and starts emitting the value, so `PtVal::Const` becomes a
domain with equality that means something — which is the precondition for
resolving a constant array index in 2.2, and for `const_assign`'s 7,443 rows
(under `--ctadl-pre`) to carry information rather than a tag.

### 2.2 Array indexes: land `VarOffset`, then give dex a mode

**Step A — the IR.** Implement `../ct-unknown-offset/variable-offset-ir.md`
verbatim. It is already specified down to the five `all(is_offset)` guards that
become runtime panics if missed (`mir/mod.rs:1009`, `:1463`,
`languages/pcode/mod.rs:209`, `languages/tree_sitter_c/mod.rs:2786`,
`ctadl-flowy/src/lib.rs:1278`), and its central decision — `.[*]` matches only
itself, with no absorption of neighbouring offsets — is the one that keeps
`facts.rs`'s `substitute_prefix` invertible. No behaviour change on merge; same
format bump as 2.1, so land them together.

**Step B — dex and JVM adopt it, behind a mode.** The design doc rules dex out
on the grounds that `[]` is sound and index-insensitive. That is right, and the
mode preserves it:

```rust
pub enum ArrayIndexMode {
    /// Today's lowering: `load d, a, "[]"`. The index register is dropped.
    Collapsed,          // default -- `ctadl index` is byte-identical
    /// `load d, a.[*(v_c)], "[]"` -- the same single memory field, plus an
    /// address displacement that names the index variable.
    Indexed,
}
```

The key property: in `Indexed` mode **every** array access — constant index or
not — gets `.[*]`, never `.[k]`. So all of an array's accesses still share one
displacement and one memory field, every flow that exists today still exists,
and CTADL's index is as sound and as precise in `Indexed` mode as in
`Collapsed`. What changes is only that the IR now *names the index variable*,
for a consumer that wants it.

Concretely, `dex/mod.rs:797-838` becomes:

```rust
// aget vA, vB, vC
let array = reg_to_var(code_item, f.b, locals);
let index = reg_to_var(code_item, f.c, locals);
let addr  = AccessPath::without_fields(array).with_var_offset(Some(index));
stmts.push(Statement::new_kind(StatementKind::load(
    dest_var, addr, FieldPath::symbol("[]"))));

// aput vA, vB, vC -- same address, and vC no longer filtered out of nothing:
// it is now a genuine use, recorded in the path rather than discarded.
```

`aput`'s source filter (`.filter(|r| *r != f.b && *r != f.c)`) stays as it is —
the index must not flow *into* the stored value — but the index is no longer
lost, because the path carries it. Note the design doc's warning: a
`VariableRef` inside an access path is a real **use**, so SSA renaming and the
use/def visitors must see it. That plumbing is Step A's job (its "plumbing"
section), and Step B is then a dozen lines per front end.

**JVM** (`jvm/mod.rs:1072,1096`) gets the identical treatment; `aaload`/`aastore`
have the index on the stack, so it is a `VariableRef` by the time it reaches
that code.

**What it unlocks here:** `Translator` grows one arm — a `Load`/`Store` whose
address path ends in `VarOffset(Some(i))` emits
`load_index_var(s, to, base, i)` / `store_index_var(s, base, i, from)` instead
of `load_field`/`store_field`. Those two relations are the *other* source of
`critical` (`src/analysis.rs:194-195`), of `decisive_var` (`:423-424`) and of
the instance-creation rules at `:366-378`. On backflash that turns the paper's
§4.1.3 from one case (dispatch) into both, on real code, for the first time —
and `PtVal::Const` from 2.1 is what lets an instance whose decisive slot is a
constant index actually *resolve* rather than go `stuck`/`top`.

It also gives CTADL something it does not have today: `ctadl index --strategy
hi` could, later, use the same `.[*]` to make an unresolved index a
`critical_summary` the way an unresolved call already is. That is out of scope
here, but the IR change is the thing that makes it possible at all.

**Effort:** Step A is the bulk — it is a 342-line design because the plumbing is
real. Budget a week including tests; Step B is a day for both front ends and
the mode flag.

### 2.3 What needs no CTADL change

Two of the gaps `ctadl-comparison.md` lists are this crate's own decisions, and
naming them keeps them off the CTADL work list:

- **Exception flow.** `rets[1]` is *already in the IR* — every dex call is
  arity 2 and every `Return` carries `[normal, exception]`
  (`languages/dex/mod.rs:306`). `add_call` (`src/ctadl.rs:545`) takes
  `rets.first()` and `add_function` (`:404-414`) takes `args.first()`. Keeping
  the second slot is two extra `bind_ret`/`ret` facts per call in relations that
  already exist, entirely on this side.
- **By-reference parameters / `ParamFlow`.** Decision 2 at `src/ctadl.rs:50-53`.
  The IR carries it; this EDB chooses the paper's by-value shape.

---

## Ordering

| # | Change | Where | Effort | Unblocks | Format bump | Status |
|---|---|---|---|---|---|---|
| 1.1 | `ssa::Pipeline` + `run_pipeline` | `ctadl-ir` | hours | drift-free `Preprocess` here, today | no | **done** (`9487842a`) |
| 1.3a | `encode_vmt` / `decode_vmt` | `ctadl-ir` | hours | symmetric reader | no | **done** (`9487842a`) |
| 1.2 | extract `ctadl-frontend` | new crate + `ctadl-ascent` | 2-3 days | `open_import`, light dep tree | no | |
| 1.3b | version-checked `open_import` | `ctadl-frontend` | hours | correct failure on a stale store | no | |
| 2.1 | `Exp::Int` + dex/JVM lowering | `ctadl-ir`, front ends | 1-2 days | meaningful `PtVal::Const` | **yes → "6"** | **done** (`51bc36b3`) |
| 2.2A | `VarOffset` per the existing design doc | `ctadl-ir`, `+flowy`, guards | ~1 week | the IR construct | **yes** (same bump) | |
| 2.2B | `ArrayIndexMode::Indexed` for dex/JVM | front ends | 1 day | `load/store_index_var` here | no | |

Landed on `ir-refactor`. 2.1 also moved flowy's integer literals off the byte
encoding (`parse_ref`/`exp_to_count`), which the plan did not call out: flowy
is a front end with the same problem, and its `int` rule admits a sign its
`u32` parse would have panicked on.

Note for 2.2A, which shares the format bump: 2.1 already spent it, so `"6"`
is taken. If 2.2A lands separately it needs `"7"`, not a second `"6"`.

1.1 is worth landing on its own this week: it is the one change that pays off
without any of the others. 2.1 and 2.2A share a format bump and should be one
release. 2.2B is the payoff commit — it is small, and everything before it
exists to make it safe.

## Validating that none of this moves `ctadl index`

Every step above claims "no behaviour change by default". The checks that make
that a fact rather than an intention:

1. **`ctadl index` output is byte-identical.** Index `backflash.apk` under
   `--strategy hi`, `mixed` and `cha` before and after each step and diff the
   parquet fact base (or `relation increase` lines from
   `RUST_LOG=…index_engine=debug`, as `ctadl-comparison.md`'s repro block
   already collects). 1.1, 1.2 and 1.3 must be exact; 2.1 and 2.2A must be
   exact because nothing emits the new variants; 2.2B is exact only in
   `Collapsed`, and in `Indexed` should differ in *no* relation but `paths`.
2. **The EDB diff here.** `examples/ctadl_import.rs` prints per-relation
   counts. After 2.2B in `Indexed` mode, `load_field`/`store_field` on the
   `[]` field should fall by exactly the number of new
   `load_index_var`/`store_index_var` rows, and nothing else should move.
3. **`examples/dispatch_diff.rs` stays at `gained = 0`.** It is already the
   tool for "did the front end cost precision"; run it across 2.1 and 2.2B as
   well, with the caveat that `Indexed` mode *adds* critical statements, so the
   expected shape is `gained ≥ 0, dropped = 0` on the dispatch key.
4. **Round-trip tests in `ctadl-ir`** for both new variants: encode/decode,
   `path_syntax` parse/print (`.[*]`, integer constants), and the
   `substitute_prefix` differential test the design doc names
   (`test_substitute_prefix_matches_rebuild`).
5. **A stale-store test:** write an import with version `"5"`, open it with a
   build expecting `"6"`, assert `Error::IncompatibleImport` names the artifact
   path. This is the regression that motivates 1.3.

## What changes on this side

- `src/ctadl.rs:143-165` — `read_import`/`import_edb` become calls to
  `ctadl_frontend::open_import`; the store-path helpers (`store_root`,
  `import_dir`) and the raw `bitcode::deserialize` of the VMT are deleted.
- `src/ctadl.rs:182-250` — `Preprocess` becomes a re-export of
  `ctadl_ir::ssa::Pipeline`; `Preprocess::ctadl()` becomes
  `Pipeline::index_default()`, and the comment claiming parity with
  `cli/mod.rs:301-304` becomes true by construction.
- `constant()` gains an `Exp::Int` arm and stops emitting `$bytes:<len>`.
- `Translator::add_statement` gains the `VarOffset` arms that populate
  `load_index_var` / `store_index_var`, and the module doc at
  `src/ctadl.rs:80-86` ("no CTADL front end emits a variable-index access")
  gets rewritten — it is the sentence this whole plan exists to falsify.
- `Cargo.toml` — the `ctadl` feature becomes
  `ctadl-frontend = { features = ["dex", "apk"] }` plus `ctadl-ir`, and the
  comment about arrow/parquet build cost can go.
- `ctadl-comparison.md` gets a re-measurement: the `--ctadl-pre` column is
  unchanged by any of this, but the "coverage" verdict table's array row moves
  from *never exercised* to a number.
