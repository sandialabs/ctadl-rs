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

### 1.2 One crate per front end: `ctadl-dex`, `ctadl-jvm`, `ctadl-lua`, `ctadl-c`, `ctadl-pcode`

The blocker is that `languages/` is stuck in a crate that also contains the
Datalog engine. A single `ctadl-frontend` crate would move the boundary but keep
the lump: a consumer that reads Dex would still build tree-sitter, Ghidra
discovery and the C grammar, and the front ends would still be free to reach
sideways into each other. One crate per language is the boundary that actually
pays, because — from the `use` lines, not from intent — **the front ends do not
depend on each other, and each depends on exactly one module of `ctadl-ascent`.**

#### What the `use` lines say

Every non-test crate-internal dependency of the five language front ends, in full:

```
languages/dex/mod.rs:16            use crate::error::{Error, ErrorContext};
languages/jvm/mod.rs:13            use crate::error::{Error, ErrorContext};
languages/lua/mod.rs:92            use crate::error::{Error, ErrorContext};
languages/tree_sitter_c/mod.rs:40  use crate::error::Error;
languages/pcode/mod.rs:12          use crate::error::{Error, ErrorContext};
languages/pcode/ghidra.rs:1        use crate::error::{Error, ErrorContext};
languages/pcode/mod.rs:33          fn import_pcode(import: &crate::project::ArtifactImport)
languages/pcode/mod.rs:79          crate::languages::jni::registry::scan_import(...)
```

That is the whole list. Four of the five need `error` and nothing else; pcode
needs `error`, `ArtifactImport`, and one call into the JNI registry. Their
*external* dependencies are disjoint — `dex-reader`; `jvm-reader`;
`tree-sitter` + `tree-sitter-lua`; `tree-sitter` + `tree-sitter-c` +
`internment`; `pcode-reader` + `flate2` + `rayon` + `which` — which is what
makes the split worth having rather than merely tidy.

The coupling that is real lives in three places, and each one decides a
boundary rather than blocking it (see *Three couplings*, below).

#### The shape

```
ctadl-ir  source-info  dex-reader  jvm-reader  pcode-reader      (leaves, exist)
                              |
                        ctadl-import        error + store + ProgramInfo I/O.
                              |             No language logic. ~1.2 kLOC.
      +----------+------------+------------+-----------+
      |          |            |            |           |
  ctadl-dex  ctadl-jvm    ctadl-lua     ctadl-c    ctadl-pcode
      |          |            |            |           |
      +----------+------------+------------+-----------+
                              |
                       ctadl-frontends      the `match import.language`,
                              |             plus the container formats.
                       ctadl-ascent         engine, codegen, facts, models,
                                            cli, JNI *linking*.
```

Five levels, and the arrows only ever point down. `ctadl-import` is deliberately
tiny and deliberately language-blind: it is `error.rs`'s import-reachable half
plus `project.rs`'s import-reachable half, and it is the only new crate that
this analysis is obliged to depend on (see *The API*).

#### Crate by crate

| Crate | Moves | Code / test LOC | Non-`ctadl-ir` deps | Blocker |
|---|---|---|---|---|
| `ctadl-import` | `error.rs` (import half), `project.rs` (import half) | ~500 / — | serde, serde_json, json5, bitcode, thiserror, hashbrown, log, source-info | none |
| `ctadl-jvm` | `languages/jvm/` | 1,164 / 0 | jvm-reader, smallvec, hashbrown | none |
| `ctadl-dex` | `languages/dex/` | 946 / 258 | dex-reader, smallvec, hashbrown, streaming-iterator | none |
| `ctadl-pcode` | `languages/pcode/`, `languages/jni/registry*` | 4,021 / 893 | pcode-reader, flate2, rayon, which, tempfile, object | owns the registry (below) |
| `ctadl-lua` | `languages/lua/` | 2,585 / 532 | tree-sitter, tree-sitter-lua, smallvec, tempfile | 1 test uses `crate::models` |
| `ctadl-c` | `languages/tree_sitter_c/` | 4,723 / 7,340 | tree-sitter, tree-sitter-c, internment, streaming-iterator | **the tests** (below) |
| `ctadl-frontends` | `languages/apk_native.rs`, `languages/xapk.rs`, `cli::import`'s `match` | ~700 / — | zip (dev), the five above, all optional | none |

`languages/jni.rs` (900 lines) does **not** move: it is JNI *linking*, it emits
facts, and it reaches `crate::codegen`, `crate::facts` and `crate::index_engine`
by design. It stays in `ctadl-ascent` and reads `JniRegistry` back out of
`ctadl-pcode`.

#### Three couplings that decide the boundaries

**1. The container formats are dispatch, not a language.** `apk_native.rs` calls
`pcode::import_pcode` (`:50`) and `cli::save_program_info` (`:293`); `xapk.rs`
calls `cli::import` recursively (`:167`). Neither can live in `ctadl-dex`
without dragging Ghidra into a crate whose whole point is not having it. They
belong in `ctadl-frontends`, whose job already *is* the recursive
`match import.language` — and the APK's own zip and Dex-entry handling is
already in `dex-reader` (`dex_reader::apk::*`), so nothing is being invented to
put them there.

**2. pcode and the JNI registry are one crate, because they are one cycle.**
`pcode/mod.rs:79` calls `jni::registry::scan_import`, and `jni/registry.rs:219`
calls `pcode::ghidra::GhidraSource::detect`. This is not incidental: the
registry scan needs Ghidra's image base and entry-point map, which only exist
mid-`import_pcode`. Ship them together as `ctadl-pcode` (module `jni_registry`).
The alternative — hoisting the `GhidraSource::detect` guard into `import_pcode`
and passing a `is_binary: bool` down, so the registry becomes a standalone ELF
scanner — is about twenty lines and can be done later if the registry ever needs
to be readable without pcode. It is not needed now: `jni.rs` reads the *file*,
not the scanner.

**3. `ctadl-c` is expensive, and it is expensive because of tests.** The
front end is 4,723 lines with one `use crate::` line. Its tests are 7,340 lines
across four files, they use `super::` 90 times (private access to the front
end's internals), and **143 of the 264 tests in `tests.rs`** call
`index_program` / `check_flow` / `get_summary` — that is, the engine. The arrow
also points back: `index_engine/mod.rs:1637` imports
`languages::tree_sitter_c::test_utils::{index_program, program_from_string}`
for the engine's *own* tests.

Three ways out, in order of preference:

- **Dev-dependency cycle.** `ctadl-c` gets `[dev-dependencies] ctadl-ascent = { path = ".." }`,
  and `ctadl-ascent` keeps its ordinary dependency on `ctadl-c`. Cargo permits
  cycles through dev-dependencies. Every test stays where it is, byte for byte;
  `super::` keeps working; the engine helpers in `test_utils` become
  `ctadl_ascent::…`. Cost: `cargo test -p ctadl-c` builds the engine, and the
  cycle would have to be broken before either crate could be published with a
  path-less version. Both are acceptable today.
- **Split `test_utils`.** Roughly 34 of its 40 helpers are pure IR-shape checks
  and stay; the six engine helpers (`index_program`, `get_summary`,
  `summary_search`, `check_summary_count`, `check_flow`, `check_no_flow`) move to
  a `ctadl-ascent` test-support module, and the 143 engine tests move with them
  into `ctadl-ascent/tests/c_frontend.rs`. Cost: a day of surgery, and it forces
  `pub` onto whatever those tests reach through `super::`.
- **Land `ctadl-c` last, or not at all.** The extraction is per-crate and the
  crates are independent; `ctadl-c` can stay a module of `ctadl-ascent`
  indefinitely without holding up the other four. Nothing in Part 2 needs it.

`ctadl-lua` has the same problem at 1/15th the size — one test,
`a_model_can_name_a_lua_external` (`lua/mod.rs:2816`), uses `crate::models`.
Move that one test to `ctadl-ascent/tests/`; the other fourteen are pure IR.

#### The error split

`ctadl-import::Error` gets the import-reachable variants — `Io`, `Path`, `Json`,
`Json5`, `Bitcode`, `Verify`, `SourceInfoParquet`, `NothingToImport`,
`IncompatibleImport`, `Context` — plus the per-language variants **behind the
matching feature**:

```rust
#[cfg(feature = "dex")]   #[error("dex decoding error")] Dex(#[from] dex_reader::error::DexError),
#[cfg(feature = "jvm")]   #[error("jvm decoding error")] Jvm(#[from] jvm_reader::error::ClassFileError),
#[cfg(feature = "pcode")] #[error("pcode fact reading error: {0}")] PcodeFactRead(String),
#[cfg(feature = "ts")]    #[error("tree-sitter parse error: {0}")] TreeSitterParse(String),
// ...
```

One shared enum rather than one enum per language, for two reasons that are
already in the code. First, the variants are *already* shared vocabulary:
`Error::Dex` is constructed by `apk_native.rs` (`:62`, `:102`, `:179`) and
`xapk.rs` (`:46`, `:71`, `:83`) — files that will live in `ctadl-frontends`, not
in `ctadl-dex`. Per-language enums would mean `ctadl-frontends` re-wrapping five
error types to say what one enum says now. Second, `ErrorContext`
(`error.rs:276-297`) is `impl<T, E> ErrorContext<T> for Result<T, E> where E: Into<Error>` —
it is hardwired to *the* `Error`. With one shared enum it moves verbatim and
every `.err_context(…)` in every moved file keeps compiling untouched.

`ctadl_ascent::Error` keeps the rest — datafusion, arrow, parquet, `Flowy`,
`JsonModel`, `Model`, `MissingIndex`, `IncompatibleIndex`, `FactsConvert` — and
gains `#[error(transparent)] Import(#[from] ctadl_import::Error)`. It keeps its
own `ErrorContext` too. The two traits never collide, because a file imports
one or the other: moved files import `ctadl_import`'s, files that stayed import
`crate::error`'s, and the `#[from]` bridges them at the `?`. That rule is worth
writing down in `ctadl-import`'s module doc, because it is the one thing a
reviewer of this refactor will otherwise have to rediscover.

#### Features

`ctadl-frontends` is where a consumer states what it reads:

```toml
[features]
default = ["dex", "jvm", "apk"]
dex   = ["dep:ctadl-dex",   "ctadl-import/dex"]
jvm   = ["dep:ctadl-jvm",   "ctadl-import/jvm"]
apk   = ["dex", "dep:zip"]                          # apk_native, xapk
xapk  = ["apk"]
pcode = ["dep:ctadl-pcode", "ctadl-import/pcode"]   # + Ghidra discovery
lua   = ["dep:ctadl-lua",   "ctadl-import/ts"]
c     = ["dep:ctadl-c",     "ctadl-import/ts"]
flowy = ["dep:ctadl-flowy"]
```

`ArtifactLanguage` stays whole in `ctadl-import` — it is an enum, it costs
nothing, and `--help` should keep naming every language the tool knows about.
The dispatch arms are what get gated: a disabled language reports
`Error::NothingToImport` naming the feature, rather than failing to parse the
flag. `ctadl-ascent` enables everything, so its build and its CLI are unchanged.
This analysis enables `dex, apk` and drops tree-sitter, Ghidra, ascent,
datafusion, tokio and serde-sarif from its tree entirely.

Flowy is the one odd case: `codegen/flowy.rs` is 579 lines of which the import
half is `import()` at `:25` (~55 lines, needing only `flowy::compile_program`
and a `bitcode` write) and the rest is `check`, which is the engine. Split it,
put the import half behind `ctadl-frontends`' `flowy` feature, leave `check` in
`ctadl-ascent`. Small, and it keeps the dispatch `match` complete.

#### The API

Four entry points, none of which has a public form today. Note **where** each
one lands, because it is the point of the whole split:

```rust
// ---- ctadl-import: needs no language at all ----

/// Read an import `ctadl import` already cached, version-checked.
/// This is `cli::load_program_info_without_source_info` (`cli/mod.rs:985`),
/// made public and given a `SourceInfo` switch.
pub fn load_import(import: &ArtifactImport, src: SourceInfo) -> Result<ProgramInfo, Error>;

/// The one-liner: name-or-directory in the store -> preprocessed IR.
/// Equivalent to `load_import` + `ssa::run_pipeline`, and the entry point this
/// whole document exists for.
pub fn open_import(name_or_dir: &str, pipeline: ssa::Pipeline) -> Result<ProgramInfo, Error>;

// ---- the language crates: artifact -> IR, no store involved ----

ctadl_dex::import_dex(path) -> Result<ProgramInfo, Error>       // already exists
ctadl_dex::import_apk(path) -> Result<ApkImport, Error>         // already exists
ctadl_jvm::import_jar / import_class, ctadl_lua::import_lua,
ctadl_c::import_c, ctadl_pcode::import_pcode                    // likewise

// ---- ctadl-frontends: the dispatch ----

/// The `match import.language` currently inlined in `cli::import` (`cli/mod.rs:65-150`),
/// minus the store write.
pub fn import_artifact(import: &ArtifactImport, opts: ImportOptions<'_>)
    -> Result<ProgramInfo, Error>;

/// Import if absent or stale, then open. What a downstream tool wants when it
/// has an APK and does not care whether the store is warm.
pub fn open_or_import(name: &str, artifact: &Path, opts: ImportOptions<'_>,
                      pipeline: ssa::Pipeline) -> Result<ProgramInfo, Error>;
```

`cli::import` becomes `ctadl_frontends::import_artifact` plus the existing
`save_program_info` call, and `save_program_info` itself (`cli/mod.rs:942`) moves
to `ctadl-import` beside its now-public sibling — which is the asymmetry named in
Part 0, closed.

The headline: **`open_import` lives in `ctadl-import`, which knows no language.**
Reading a warm store is exactly `ArtifactImport::load` + `decode` +
`ssa::run_pipeline`, and none of that needs a front end. So this analysis's
actual dependency — the thing §1.3 is about — is one ~1.2 kLOC crate with no
parser in it. `ctadl-dex` enters only if it ever wants to import an APK itself
rather than read one `ctadl import` already wrote.

`ProgramInfo` (`ctadl-ir/src/mir/mod.rs`) already bundles
`program + vmt + source_info`, so the entry point returns one value where this
crate currently juggles two files. The `bitcode::deserialize` hand-roll for the
VMT is already gone: `encode::{encode_vmt, decode_vmt}` landed with 1.1
(`ctadl-ir/src/mir/encode.rs:29,36`).

#### Order of landing

One crate per PR; `cargo test --workspace` is the check each time, and each step
is independently revertible.

| Step | Crate | Why here | Effort |
|---|---|---|---|
| 1.2a | `ctadl-import` | Nothing leaves `languages/` yet. `ctadl-ascent` re-exports (`pub use ctadl_import::{error, project};`) so every existing path still resolves. This is the step that unblocks 1.3 *and* this analysis. | 1 day |
| 1.2b | `ctadl-jvm` | The pilot: 1,164 lines, one `use crate::` line, zero tests to relocate. If the error split is wrong, it is wrong here, cheaply. | 2 hours |
| 1.2c | `ctadl-dex` + `ctadl-frontends` | Dex is the twin of JVM; the dispatch crate has to exist before the container formats can move out of `cli`. | 1 day |
| 1.2d | `ctadl-pcode` | Carries `jni/registry` with it; `jni.rs` stays and switches to `ctadl_pcode::jni_registry::JniRegistry`. | 1 day |
| 1.2e | `ctadl-lua` | One test relocates. | 3 hours |
| 1.2f | `ctadl-c` | Gated on the test decision above. Optional, and last. | 1 day (cycle) / 2-3 days (split) |

**Effort:** three to four days for 1.2a–1.2e, most of it mechanical
`crate::` → `ctadl_import::`; plus 1.2f, whose cost is a choice rather than a
number. **Risk:** low and bounded per step — no logic moves, and the first step
is a pure re-export.

**If the extraction is not wanted at all:** make
`load_program_info_without_source_info` public, add `cli::open_import` next to
it, and gate `ctadl-ascent`'s heavy halves behind a default-on `engine` feature
(`datafusion`, `ascent`, `query_engine`, `index_engine`, `codegen`). Same API,
one crate. But note what the numbers above say: a feature graph inside a
19 kLOC crate has to be kept honest by hand, whereas the front ends' `use` lines
have *already* kept the crate boundaries honest without anyone trying.

### 1.3 Make the version check reachable

Today this crate reads `ir-program.bitcode` directly, so an import written by a
newer CTADL decodes as garbage or as a `bitcode::Error`. `ArtifactImport::load`
(`project.rs:273-295`) already produces the right diagnostic
(`Error::IncompatibleImport`, naming the original artifact path and telling the
user to re-import) — it just is not on any path this crate can call. Step 1.2a
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
| 1.2a | extract `ctadl-import` | new crate + `ctadl-ascent` | 1 day | `open_import`, light dep tree, and 1.3b | no | |
| 1.2b-e | `ctadl-jvm`, `-dex`, `-pcode`, `-lua` + `ctadl-frontends` | new crates | 2-3 days | per-language dep trees | no | |
| 1.2f | `ctadl-c` | new crate | 1-3 days | nothing here; optional, last | no | |
| 1.3b | version-checked `open_import` | `ctadl-import` | hours | correct failure on a stale store | no | |
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
  `ctadl_import::open_import`; the store-path helpers (`store_root`,
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
- `Cargo.toml` — the `ctadl` feature becomes `ctadl-import` plus `ctadl-ir`,
  and nothing else: reading a warm store needs no front end (§1.2, *The API*).
  `ctadl-frontends = { features = ["dex", "apk"] }` is needed only if this crate
  ever imports an APK itself rather than reading one `ctadl import` wrote. The
  comment about arrow/parquet build cost can go either way.
- `ctadl-comparison.md` gets a re-measurement: the `--ctadl-pre` column is
  unchanged by any of this, but the "coverage" verdict table's array row moves
  from *never exercised* to a number.
