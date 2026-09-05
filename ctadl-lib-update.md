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

> **Since fixed by 1.1** (`9487842a`). The four lines are one
> `ssa::run_pipeline(&mut p, ssa::Pipeline::index_default().prune(…))`, and
> `Pipeline::index_default()` is now the only definition of what `ctadl index`
> runs. The paragraph above is kept because it is why the type exists.

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

> **Since fixed by 1.2a** (`b64af28c`). The reader is
> `ctadl_import::load_import`, public, beside a `save_program_info` that moved to
> sit next to it; `ctadl_import::open_import` is the version-checked one-liner.
> Neither is in `ctadl-ascent` any more — `ctadl-import`'s build graph is 140
> crates against `ctadl-ascent`'s 384, and holds no parser and no engine. The
> paragraph above is kept because it is the asymmetry the crate exists to close.

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

**Status: 1.1 and 1.2 have landed on `ir-refactor`; 1.3 is partly done.** The
sections below are kept as the design record — the reasoning is what a reviewer
needs and none of it turned out wrong — with an *As built* note wherever the
result differs from what was proposed. See *Where 1.2 came out
differently* under [Ordering](#ordering) for those, collected.

### 1.1 `ctadl_ir::ssa::Pipeline` — name the pipeline where the passes live

> **Landed** (`9487842a`), as written below.

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

> **Landed**: seven new crates in seven commits (`b64af28c` … `62d9e04d`), and
> the workspace went from 10 members to 17. Nothing `ctadl index` computes
> changed, and test counts are conserved exactly.

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

> **As built:** the survey above is accurate and its conclusion held, but a
> `use`-line survey is the wrong instrument for finding *calls* through a path,
> and two of those turned up: `jni/registry.rs` → `jni::descriptor_params`, and
> `index_engine`'s own test → `ctadl-c`'s `test_utils`. Both are in *Three
> couplings*. Next time, grep for `crate::` rather than for `use crate::`.

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

> **As built:** this shape, exactly, with two amendments. `ctadl-import` took
> `project.rs` whole rather than a half, so it is 1.4 kLOC rather than 1.2, and
> it also holds the `ProgramInfo` I/O that used to live in `cli`. And there is
> one arrow the diagram cannot draw: `ctadl-c` takes `ctadl-ascent` as a
> **dev**-dependency, a cycle Cargo permits, so that its 285 tests — which are
> the engine's regression suite as much as the front end's — could move unedited.
> Nothing in a non-test build follows it.

#### Crate by crate

As built. "LOC" counts everything under `src/` and `tests/`, tests included.
"Graph" is `cargo tree -e normal` deduplicated — the crates a consumer of that
crate *alone* compiles, against `ctadl-ascent`'s **384**. "Tests" is what
`cargo test -p` runs.

| Crate | Moved | LOC | Graph | Tests | Deps beyond `ctadl-ir`/`ctadl-import` |
|---|---|---|---|---|---|
| `ctadl-import` | `error.rs` (import half), `project.rs` (whole), the `ProgramInfo` I/O out of `cli` | 1,361 | **140** | 5 | serde, serde_json, json5, bitcode, thiserror, hashbrown, log, source-info |
| `ctadl-jvm` | `languages/jvm/` | 1,167 | 169 | 0 | jvm-reader, smallvec, hashbrown, log, source-info |
| `ctadl-dex` | `languages/dex/` | 1,203 | 168 | 11 | dex-reader, smallvec, hashbrown, streaming-iterator, log, source-info |
| `ctadl-pcode` | `languages/pcode/`, `languages/jni/registry*` | 4,076 | 296 | 31 | pcode-reader, object, flate2, rayon, which, tempfile, serde, serde_json, log-once |
| `ctadl-lua` | `languages/lua/` | 3,083 | 145 | 14 | tree-sitter, tree-sitter-lua, smallvec, hashbrown, log, source-info |
| `ctadl-c` | `languages/tree_sitter_c/` | 12,060 | 146 | 285 | tree-sitter, tree-sitter-c, internment, streaming-iterator, anyhow, hashbrown |
| `ctadl-frontends` | `languages/{apk_native,xapk,flowy}.rs`, `cli::import`'s `match` and `ImportOptions` | 979 | 185 | 7 | the six above (all optional), dex-reader, bitcode, log |

`languages/jni.rs` (900 lines) does **not** move: it is JNI *linking*, it emits
facts, and it reaches `crate::codegen`, `crate::facts` and `crate::index_engine`
by design. It stays in `ctadl-ascent` and reads `JniRegistry` back out of
`ctadl-pcode`.

**As built.** Three notes on the table:

* `ctadl-import` is larger than the ~500 lines projected, because `project.rs`
  moved whole rather than in halves — it has exactly one crate-internal `use`
  (`error`) and no engine dependency anywhere, so there was no seam to cut.
  `MissingIndex`/`IncompatibleIndex` travelled with `AnalysisProject`: they
  describe the store's layout, which is what this crate now owns.
* `ctadl-frontends` also took flowy's *import* half, which the plan put in
  `codegen`'s split (see *Features*). It had to move in 1.2a rather than here —
  see the ordering notes.
* No crate needs `zip`. The APK's zip handling was already `dex_reader::apk`'s,
  so `apk` takes `dex-reader` directly and `zip` stays a `ctadl-ascent`
  dev-dependency for building test fixtures.

#### Three couplings that decide the boundaries

> **As built:** all three held, and a fourth and fifth turned up that the `use`
> lines did not show — see the end of this subsection.

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

**As built — which way each went.**

1. *Container formats.* As described. They are `ctadl-frontends`, and `xapk`'s
   recursion is `ctadl_frontends::import_and_save`.
2. *pcode + registry.* Shipped together as planned, as `ctadl_pcode::jni_registry`,
   with the twenty-line hoist that would break the cycle written into the module
   doc for whoever needs it. The `pub(super)` items the attribution pass exposed
   to `jni.rs` widened to `pub`, since `super` is now a crate boundary.
3. *`ctadl-c`.* Took the **dev-dependency cycle**, the first option. All 285
   tests moved byte for byte, `super::` kept working, and the engine helpers in
   `test_utils` became `ctadl_ascent::…`. `ctadl-lua`'s one test relocated to
   `ctadl-ascent/tests/lua_model_externals.rs`, but imports from a real `m.lua`
   in a tempdir rather than the crate's private in-memory `lower_lua_units`, so
   nothing had to become `pub` for one test.

**Two couplings the `use` lines did not show**, both found by the compiler:

4. `jni/registry.rs` called `super::descriptor_params` — the JVM method-descriptor
   parser in `jni.rs`. It is self-contained, and the registry needs it to tell a
   `JNINativeMethod`'s descriptor slot from a pointer into arbitrary data, so it
   moved *down* with the scanner and `jni.rs` re-exports it. A call, not a `use`,
   which is why the survey missed it.
5. The arrow really does point back, and not only through `test_utils`'s
   declaration: `index_engine/mod.rs`'s own `parallel_engine_matches_serial`
   *calls* `index_program`/`program_from_string` out of the C front end's private
   test helpers. Rather than making the dev-dependency cycle bidirectional, that
   arrow was deleted — the engine half of those two helpers is thirty lines and
   belongs in the engine's test module. `ctadl-ascent` no longer reaches into
   `ctadl-c` at all.

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

**As built.** Exactly this, and the rule is in `ctadl-import`'s module doc. One
addition the plan did not anticipate: `#[from] ctadl_import::Error` alone is not
enough on the engine side, because a `?` or an `.err_context(…)` there commonly
starts from a *leaf* type — `std::io::Error`, `serde_json::Error`, `bitcode::Error`,
`VerifyErrors`, `source_info`'s `ParquetError`, and the two reader errors. Without
a bridge, all 161 `err_context` sites in `ctadl-ascent` would have had to name
`ctadl_import::Error` explicitly. A `from_import!` macro emits
`impl From<$leaf> for Error { … Error::Import(ctadl_import::Error::from(e)) }` for
each, so every one of them compiles untouched. Only the ~15 sites that *construct*
an import-side variant by name (`Error::Path { … }`, `Error::MissingIndex { … }`)
had to change, and they now say `ctadl_import::Error::…` — which is the right
thing for them to say.

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

**As built.** The feature table shipped with three changes:

```toml
[features]
default = ["dex", "jvm", "apk", "xapk"]             # xapk added
dex   = ["dep:ctadl-dex",   "ctadl-import/dex"]
jvm   = ["dep:ctadl-jvm",   "ctadl-import/jvm"]
apk   = ["dex", "dep:dex-reader"]                   # not `zip`
xapk  = ["apk"]
pcode = ["dep:ctadl-pcode", "ctadl-import/pcode"]
lua   = ["dep:ctadl-lua",   "ctadl-import/ts"]
c     = ["dep:ctadl-c",     "ctadl-import/ts"]
flowy = ["dep:ctadl-flowy", "dep:bitcode", "ctadl-import/flowy"]
```

* **`apk` takes `dex-reader`, not `zip`.** The APK and bundle central-directory
  reading is `dex_reader::apk`'s already, so nothing in the workspace needs `zip`
  outside test fixtures.
* **`apk` implies `dex` but not `pcode`,** which the plan left implicit and which
  is the whole point of the feature: an APK imported without the pcode front end
  keeps its Java half and reports that this build has no native front end. That
  is the same degradation as an APK imported on a machine with no Ghidra, which
  is already supported and common, so the two collapse into one predicate
  (`apk_native::native_frontend_available`) and one `#[cfg]`-gated function —
  not a gated extraction loop.
* **`xapk` is in the default set.** A bundle costs no dependency beyond `apk`,
  and leaving it out by default would mean its three unit tests never ran under
  `cargo test`.

Every combination compiles warning-free: `--no-default-features`, each language
alone, `dex`, `dex,apk`, `dex,apk,xapk`, `lua,c`, `pcode`, `flowy`, and
`--all-features`. Flowy split as described, except that its import half had to
move in 1.2a rather than here (see the ordering notes).

The measured payoff, which is the claim this section makes: a `dex, apk`
consumer's build graph is **169 crates against `ctadl-ascent`'s 384**, with
tree-sitter, datafusion, ascent, tokio, serde-sarif and the Ghidra plumbing all
absent. `arrow` and `parquet` are still in it, arriving transitively through
`source-info`'s `parquet_io` — which is exactly what §1.3 names as the last thing
standing between a consumer and a genuinely light tree.

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

> **As built**, with the signatures that shipped:
>
> ```rust
> // ctadl-import — no language, no engine
> pub fn load_import(&ArtifactImport, SourceInfoMode) -> Result<ProgramInfo, Error>;
> pub fn open_import(name_or_dir: &str, ssa::Pipeline) -> Result<ProgramInfo, Error>;
> pub fn save_program_info(ProgramInfo, &ArtifactImport) -> Result<(), Error>;
> pub fn resolve_import(name_or_dir: &str) -> Result<ArtifactImport, Error>;
>
> // ctadl-frontends — the dispatch
> pub fn import_artifact(&ArtifactImport, ImportOptions<'_>) -> Result<ProgramInfo, Error>;
> pub fn import_and_save(&ArtifactImport, ImportOptions<'_>) -> Result<(), Error>;
> pub fn open_or_import(name: &str, artifact: &Path, ArtifactLanguage,
>                       ImportOptions<'_>, ssa::Pipeline) -> Result<ProgramInfo, Error>;
> ```
>
> Three differences from the sketch above. The switch is spelled `SourceInfoMode`
> (`Skip` | `Read`), not `SourceInfo`, so it does not collide with
> `source_info::SourceInfo`. `open_or_import` takes an `ArtifactLanguage`, because
> creating an `ArtifactImport` needs one and a path alone does not determine it.
> And `import_and_save` exists as a named function rather than as prose about
> "plus the existing `save_program_info` call", because `xapk` recurses into it.
>
> `ctadl-import` also ships `tests/open_import.rs`: the round trip exercised in a
> process with no front end linked in, which is as much the point of the test as
> of the crate.

#### Order of landing

One crate per PR; `cargo test --workspace` is the check each time, and each step
is independently revertible.

**As landed**, one commit each, `cargo test --workspace` green after every one:

| Step | Crate | Commit | Note |
|---|---|---|---|
| 1.2a | `ctadl-import` | `b64af28c` | `ctadl-ascent` re-exports `ctadl_import::project`, so every `crate::project::…` path resolves unchanged. Nothing left `languages/` — but its files switched to `ctadl_import::error`, which is the only preparation the rest needed. |
| 1.2b | `ctadl-jvm` | `7c2fd5fb` | The pilot. The error split from 1.2a was right; the move was `git mv`, a manifest, and a `log` dependency the old crate had supplied ambiently. |
| 1.2c | `ctadl-dex` | `8a058db1` | Its 258 lines of tests are pure IR-shape checks and moved with it. |
| 1.2d | `ctadl-pcode` | `2cfc99af` | Carries `jni/registry` and its 31 tests; `jni.rs` stays and re-exports the module. |
| 1.2e | `ctadl-lua` | `426856ac` | One test relocated. |
| 1.2f | `ctadl-c` | `c1617fb7` | Dev-dependency cycle; 285 tests moved unedited. |
| 1.2g | `ctadl-frontends` | `62d9e04d` | The dispatch, the container formats, `import_artifact`, `open_or_import`. |
| — | doc links | `a4ed1237` | Every intra-doc link that crossed a new boundary. |

**The one ordering change: `ctadl-frontends` cannot land with `ctadl-dex`.**
`apk_native.rs` calls `pcode::import_pcode`, so the dispatch crate depends on
`ctadl-pcode` — and while pcode is still a module of `ctadl-ascent`, that
dependency is a cycle. So the container formats wait for 1.2d and the dispatch
crate lands last, as its own step. Same arrows, same end state, one step later.

A knock-on: with `cli::import` still returning `ctadl_ascent::Error` in 1.2a,
`xapk`'s recursion into it could not typecheck against `ctadl_import::Error`.
Making `cli::import` import-side end to end needed flowy's import half split out
*then*, in 1.2a, rather than with the rest of `ctadl-frontends`. It lived in
`ctadl-ascent/src/languages/flowy.rs` for five commits and moved with the others
in 1.2g.

**Effort:** as estimated for the mechanical part — most of 1.2a–1.2e really is
`crate::` → `ctadl_import::`. What the estimate missed is that the interesting
work is not the moving; it is the two invisible couplings (a call, not a `use`)
and the error bridge, and those are found by compiling, not by reading.

**The alternative, not taken:** gating `ctadl-ascent`'s heavy halves behind a
default-on `engine` feature would have given the same API in one crate. Recorded
here because it stays the fallback if the seven-crate workspace ever proves worse
to live with than it looks — but the numbers argue against it: a feature graph
inside a 19 kLOC crate has to be kept honest by hand, whereas the front ends'
`use` lines had *already* kept these boundaries honest without anyone trying,
and the split found exactly two places where they had not.

### 1.3 Make the version check reachable

> **Landed**, apart from one optional dependency cleanup. 1.2a put
> `ArtifactImport::load` on a reachable path: `ctadl_import::open_import` resolves
> a name *or* a directory through it and never around it, so a stale store fails
> as `IncompatibleImport` naming the artifact to re-import. Two tests in
> `ctadl-import/tests/open_import.rs` now pin that, one per spelling of the
> argument (item 5 under *Validating*), and `ctadl-ir`'s unused `arrow`/`parquet`
> are gone. What is left is optional and is not about correctness:
> feature-gating `source-info`'s `parquet_io`. `import_format_version_beside` was
> already there and did not need changing.

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

> **Done for the direct deps**, and the duplicate was real: `arrow` 57 and its
> thirteen sibling crates leave `Cargo.lock` entirely, because `source-info` is
> on 58 and nothing else asked for 57. Fourteen packages out, none in, and
> nothing else needed changing, since `ctadl-ir` never named either one.
> `ctadl-ir`'s own graph is 118 crates. `source-info`'s `parquet_io` is still
> unconditional (`source-info/src/lib.rs:21`), so `arrow` and `parquet` still
> reach a `ProgramInfo`-only consumer through it — one major of each now instead
> of two.

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
| 1.2a | extract `ctadl-import` | new crate + `ctadl-ascent` | 1 day | `open_import`, light dep tree, and 1.3b | no | **done** (`b64af28c`) |
| 1.2b-e | `ctadl-jvm`, `-dex`, `-pcode`, `-lua` | new crates | 2-3 days | per-language dep trees | no | **done** (`7c2fd5fb`, `8a058db1`, `2cfc99af`, `426856ac`) |
| 1.2f | `ctadl-c` | new crate | 1-3 days | nothing here; optional, last | no | **done** (`c1617fb7`), dev-dep cycle |
| 1.2g | `ctadl-frontends` | new crate | 1 day | the dispatch, `import_artifact`, `open_or_import` | no | **done** (`62d9e04d`) |
| 1.3b | version-checked `open_import`, its regression test, `ctadl-ir` dep cleanup | `ctadl-import`, `ctadl-ir` | hours | correct failure on a stale store | no | **done**; `source-info`'s `parquet_io` gate left open |
| 2.1 | `Exp::Int` + dex/JVM lowering | `ctadl-ir`, front ends | 1-2 days | meaningful `PtVal::Const` | **yes → "6"** | **done** (`51bc36b3`) |
| 2.2A | `VarOffset` per the existing design doc | `ctadl-ir`, `+flowy`, guards | ~1 week | the IR construct | **yes** (same bump) | |
| 2.2B | `ArrayIndexMode::Indexed` for dex/JVM | front ends | 1 day | `load/store_index_var` here | no | |

Landed on `ir-refactor`. 2.1 also moved flowy's integer literals off the byte
encoding (`parse_ref`/`exp_to_count`), which the plan did not call out: flowy
is a front end with the same problem, and its `int` rule admits a sign its
`u32` parse would have panicked on.

### Where 1.2 came out differently

Five things, none of which changes the end state, and all five of the kind only
a compiler finds. Each is written up where it belongs in §1.2; this is the index.

1. **`ctadl-frontends` cannot land with `ctadl-dex`** — `apk_native` calls
   `pcode::import_pcode`, and while pcode is a module of `ctadl-ascent` that is a
   cycle. It lands last instead. *(§1.2, Order of landing.)*
2. **`project.rs` moved whole, not in halves** — no seam to cut, so
   `MissingIndex`/`IncompatibleIndex` travelled with it. *(§1.2, Crate by crate.)*
3. **Flowy's import half had to split in 1.2a**, not with the rest of
   `ctadl-frontends`. *(§1.2, Order of landing.)*
4. **Two couplings the `use`-line survey could not see**, because both are calls:
   `registry.rs` → `jni::descriptor_params`, and `index_engine`'s own test →
   `ctadl-c`'s private `test_utils`. *(§1.2, Three couplings.)*
5. **The feature table shipped with three changes**: `dex-reader` instead of
   `zip`, `apk` not implying `pcode`, and `xapk` in the default set. *(§1.2,
   Features.)*

And one thing the plan did not mention at all: `#[from] ctadl_import::Error` is
not sufficient on the engine side, because most `?`s there start from a leaf
error type rather than from an import error. A `from_import!` macro bridges the
seven leaves, which is what kept all 161 `err_context` call sites compiling
untouched. *(§1.2, The error split.)*

Note for 2.2A, which shares the format bump: 2.1 already spent it, so `"6"`
is taken. If 2.2A lands separately it needs `"7"`, not a second `"6"`.

What is left. **Part 1 is finished**: 1.3b landed with the rest, so every row of
the table through 2.1 is done, and *The measurement* below says so with numbers
rather than with an argument. The only piece of 1.3 not taken is feature-gating
`source-info`'s `parquet_io`, which is a dependency-tree improvement and not a
correctness one.

**2.2A and 2.2B are out of scope for this branch, deliberately.** An
implementation of 2.2A exists and was reverted: it is on `varoffset-backup`
(`86647013`, plus a WIP follow-on), together with `ctadl-varoffset-review.md`,
which argues that a π emitted by a front end is the wrong object — π is the
residue of resolution, so the index variable should travel *beside* the path as
a side relation and the widening should be a pass over the fact base, not a
lowering rule. Read that review before picking 2.2 up again. Note also that the
design doc §2.2 cites throughout, `../ct-unknown-offset/variable-offset-ir.md`,
was never committed and the worktree holding it is gone; that review and
`86647013`'s commit message are what survive of it.

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

   > **Run, for 1.1 through 2.1 together, and it holds** — see *The measurement*
   > below. One correction to the wording: the bar is *content*-identical, not
   > byte-identical, and that is not a concession this branch needs. Re-running
   > the **same binary** on the same APK writes byte-different
   > `assign`/`paths`/`summary`/`index_source_map` files, because row order
   > inside a parquet table is not deterministic. Compare each table as the
   > sorted multiset of its rows.
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

   > **Landed**, as `an_import_from_an_older_build_is_refused_by_name` and
   > `…_by_directory` in `ctadl-import/tests/open_import.rs`. The import is
   > written by this build and only its config's `version` field is edited, so
   > the bitcode is perfectly readable and the version is the one thing wrong —
   > which is the point: the check fires on the config, before the decode. Both
   > spellings of `open_import`'s argument are covered because both resolve
   > through `ArtifactImport::load` and the test is what says neither goes
   > around it.
   >
   > Writing it turned up something worth knowing about the errors themselves:
   > `resolve_import` wraps the diagnostic in an `Error::Context`, and
   > `Error::Context`'s `Display` prints only its own context line. So
   > `err.to_string()` on a stale store says "reading import 'x'" and nothing
   > about the version. Nothing is lost — the variant carries `source`, `main`
   > returns `anyhow::Result`, and the CLI prints the whole chain as
   > `Caused by:` — but a caller that formats the error without walking
   > `source()` will drop the useful half. The tests walk the chain, and
   > `full_message` in that file is the two lines it takes.

### What 1.2 was actually checked against

1.2 moves no logic, so the bar for it is *conservation* rather than a fact-base
diff. What ran, after every one of the seven commits:

* **`cargo test --workspace` green**, and **test counts conserved exactly**:
  513 passing + 4 ignored before 1.2a; 512 + 4 spread across seven crates
  afterwards, plus the one Lua test that became an integration test. Per crate:
  `ctadl-ascent` 168 lib (291 with its integration tests), `ctadl-c` 281 + 4
  ignored, `ctadl-pcode` 31, `ctadl-lua` 14, `ctadl-dex` 11, `ctadl-frontends` 7,
  `ctadl-import` 5. No test was rewritten to keep passing. One moved
  (`a_model_can_name_a_lua_external`, now an integration test) and one kept its
  body while its two helpers were reimplemented in the engine
  (`parallel_engine_matches_serial`); both are documented where they landed.
* **`cargo clippy --workspace --all-targets` clean**, apart from two warnings
  that predate this work in `jvm-reader` and `rustc_graphviz`.
* **`cargo doc --workspace --no-deps`** back to its pre-1.2a warning set — none
  in any of the seven new crates. Worth knowing for next time: an outer `///` on a
  `pub mod` whose file already carries an inner `/*! … */` makes rustdoc resolve
  the *merged* doc in the parent's scope, silently breaking every link the module
  wrote about its own items.
* **Every feature combination** of `ctadl-frontends` compiles warning-free
  (listed under *Features*).
* **The CLI end to end**: `ctadl import -l c`, `ctadl index`, `ctadl inspect`
  against a fresh store, which exercises the dispatch, the store write, and the
  now-public reader on one path.

### The measurement

Item 1 above has now been run, and against the whole branch rather than
step by step — which is the stronger statement and the cheaper one, since what a
reviewer wants to know is whether `ir-refactor` moves `ctadl index` at all.

`backflash.apk` (3,898 functions), imported and indexed under `--strategy hi`,
`mixed` and `cha`, by a release build of `main` (`6ad1e2e1`) and by a release
build of this branch. Both stores written from scratch; the branch's import is
format `"6"` and `main`'s is `"5"`, so the `ir-program.bitcode` files differ by
construction and only the index is comparable — which is the thing the claim is
about.

**Every one of the 13 tables is identical, in all three strategies:**

| relation | hi | mixed | cha |
|---|---|---|---|
| `actual_param` | 38,684 | 38,684 | 38,684 |
| `assign` | 137,315 | 139,387 | 139,588 |
| `call` | 2,477 | 7,521 | 8,586 |
| `call_target_assign` | 912 | 912 | 912 |
| `callee_info` | 5,526 | 453 | 0 |
| `callee_resolvents` | 11,168 | 11,168 | 11,168 |
| `external_function` | 1,523 | 1,523 | 1,523 |
| `formal_param` | 13,990 | 13,998 | 13,999 |
| `function_id` | 3,898 | 3,898 | 3,898 |
| `import_id` | 1 | 1 | 1 |
| `index_source_map` | 43,283 | 43,283 | 43,283 |
| `paths` | 2,266 | 2,266 | 2,266 |
| `summary` | 973 | 1,363 | 1,452 |
| **total** | **262,016** | **264,457** | **265,360** |

Not only the counts: each table matches row for row as a sorted multiset,
columns included. `index_config.json` matches too. And the relations that never
reach parquet are covered by the `index_engine` debug lines, which are identical
line for line in all three runs — including the `locals` closure, which is the
one most sensitive to a change in the IR reaching it (`hi`: 54,880 rows, 3.92
reached per formal, 81.5% of variables reached).

So 1.1, 1.2 and 2.1 together move nothing that `ctadl index` computes, measured
rather than argued.

**The one thing that is not exact, and never was.** Four tables — `assign`,
`paths`, `summary`, `index_source_map` — are byte-different between the two
builds. They are also byte-different between `main` and *itself*, re-run on the
same APK into a fresh store, which is how we know it is row order inside the
parquet file and not a change in what was computed. Anyone repeating this should
compare content, not bytes; a `cmp` loop reports four false differences per
strategy. The comparison used here is `scripts/compare-index.py`, which reads
each table with `pyarrow`, sorts the rows, and hashes them:

```
ctadl --store A index bf_hi backflash.apk --strategy hi
ctadl --store B index bf_hi backflash.apk --strategy hi
scripts/compare-index.py A/projects/bf_hi/index B/projects/bf_hi/index
```

## What changes on this side

None of this has been done yet — it is the downstream half, in the analysis
crate, not in `../ctadl-rs`. **Everything the first two bullets need now exists**
on `ir-refactor`: `ctadl_import::open_import`, `ctadl_ir::ssa::Pipeline`, and a
`ctadl-import` whose build graph is 140 crates with no parser and no engine in it.

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
  `ctadl-frontends = { default-features = false, features = ["dex", "apk"] }` is
  needed only if this crate ever imports an APK itself rather than reading one
  `ctadl import` wrote — measured at 169 crates against `ctadl-ascent`'s 384.
  The comment about arrow/parquet build cost can go either way; the residue is
  `source-info`'s `parquet_io`, and §1.3's last paragraph is how it goes away.
- `ctadl-comparison.md` gets a re-measurement: the `--ctadl-pre` column is
  unchanged by any of this, but the "coverage" verdict table's array row moves
  from *never exercised* to a number.
