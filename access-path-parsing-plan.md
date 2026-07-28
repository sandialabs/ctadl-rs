# One access-path grammar, everywhere - DO-NOT-MERGE

## Context

An access path (`facts::Path`) is a sequence of `mir::PathSegment`s, each either a
`Symbol` or an `Offset(i64)`. Five different pieces of code in this workspace turn a path
into a string or a string into a path, and **no two of them agree**:

| | `parse_path_string`<br>`facts.rs:457` | `split_dot_segments`<br>`models/json.rs:1809` | `parse_fields`<br>`tree_sitter/test_utils.rs:289` | flowy PEG<br>`flowy.pest:31-36` |
| --- | --- | --- | --- | --- |
| leading `.` | strips **all** | required, else silently `[]` | filtered | required per segment |
| `.[42]` | `Offset(42)` | `Symbol("[42]")` | `Symbol("[42]")` | `Offset(42)` |
| `.[foo]`, `.[]` | **segment silently dropped** | `Symbol("[foo]")` | `Symbol("[foo]")` | parse error |
| `\.` | unescapes | not supported | not supported | impossible |
| empty segment | skipped | **emits `Symbol("")`** | filtered | error |
| trailing `.` | ignored | **emits `Symbol("")`** | filtered | error |

Plus three spellings for one offset: `Path::to_dot_string` decimal `[42]`,
`mir::Offset`'s `Display` signed hex `[0x2a]`, and a model port's `"[42]"` which is a
symbol whose name happens to contain brackets.

Two concrete defects fall out, and they are the reason this is worth doing:

**1. The fact database silently corrupts every array access.** `facts/parquet.rs:516-541`
is the on-disk encoding for *every* `Path` column — `assign`, `summary`, `actual_param`,
`call_target_assign`, `callee_info`, `paths`, endpoints (`facts/schema.rs:49,65,74,86,107,115,123,140,143`).
It writes `to_dot_string()` and reads back `s.parse().unwrap_or_else(|_| Path::empty())`.
`to_dot_string` escapes `.` but not `[`, and the parser treats a bare `[` as an offset:

| frontend | segment | written | read back as |
| --- | --- | --- | --- |
| dex `dex/mod.rs:751,774`, jvm `jvm/mod.rs:1046,1070` | `Symbol("[]")` | `.[]` | **deleted** |
| lua `lua/mod.rs:1921,…`, C `tree_sitter/mod.rs:1881` | `Symbol("[_elem_]")` | `.[_elem_]` | **deleted** |
| lua `lua/mod.rs:1918,1945`, C `tree_sitter/mod.rs:1878` | `Symbol("[3]")` | `.[3]` | `Offset(3)` — **type flip** |

`index` and `query` are separate processes, so every path crosses this boundary. Two
consequences: array element access is accidentally field-*insensitive* (the empty path is a
prefix of everything), and Lua's `t[1][2]` becomes `Offset(1)`,`Offset(2)` which
`Path::from_accesses` then **sums into `Offset(3)`** — the same path as `t[3]`.

This is also the unexplained row in `default-propagation-models.md` §F2. A propagation port
`Argument(0).[_elem_]` is stored in the summary as `Symbol("[_elem_]")`, written as
`.[_elem_]`, and read back as the empty path — i.e. as the bare port `Argument(0)`, which is
exactly the score of 1 that was measured where 0 was predicted. Same for `.[zzz]`. **F2's
"fourth path not yet found" is `facts/parquet.rs:538`; §0b can be closed by this work rather
than re-measured.**

**2. A model port can never name an offset.** `parse_port` (`models/json.rs:1732`) yields
`Vec<&str>`; those strings are stored verbatim in an Arrow column
(`models/mod.rs:703`) and revived through `impl<S: AsRef<str>> FromIterator<S> for Path`
(`facts.rs:540`), which makes **every** segment a `Symbol`. So `Argument(1).[8].deref` is
`Symbol("[8]"), Symbol("deref")` and can never match the `Offset(8), Symbol("deref")` that
pcode's `push_offset` (`pcode/mod.rs:121`) actually emits — even though
`docs/model-generators.md:285-298` and `ctadl-model-generator.schema.json:270` both
advertise exactly that spelling. This is `default-propagation-models.md` §0c.

**Outcome.** One grammar, one parser, one printer, used by the fact store, model ports, the
IR's `Display`, flowy, and the test DSLs. Print and parse become exact inverses, malformed
paths fail loudly instead of mutating, and a model port gains the ability to write an
`Offset` — which is the capability §0c asks for.

### The grammar

```
path    := segment*                      -- "" is the empty path
segment := '.' ( offset | symbol )       -- a leading '.' is required before every segment
offset  := '[' ('+'|'-')? DIGIT+ ']'     -- decimal i64, nothing else
symbol  := one or more chars, up to the next UNESCAPED '.',
           and NOT beginning with an unescaped '['
escape  := '\' ANY  ->  the literal char   ( '\.' '\[' '\\' )
```

Hard errors: a segment not preceded by `.`; an empty segment (`..`, trailing `.`); `[` at
segment start whose contents are not a valid `i64` or which is unterminated; a trailing lone
`\`.

Printing: `.` before each segment; offsets decimal; symbols escape `\`, `.`, and a **leading**
`[`. So `Symbol("[]")` prints `.\[]` and `Symbol("[_elem_]")` prints `.\[_elem_]`.

Two decisions already taken:

- **Bracketed symbol names stay.** The frontends keep emitting `Symbol("[]")` /
  `Symbol("[_elem_]")` / `Symbol("[3]")`; escaping on print is the permanent answer, so a
  model port naming a Java array element is written `"Argument(0).\\[]"` in JSON. No rename
  of these synthetic fields is planned.
- **Offsets are decimal.** `mir::Offset`'s `Display` moves from signed hex to decimal so the
  IR dump, the fact store, model ports and the flowy grammar all agree. If hex is wanted for
  readability in an IR dump it goes in a side comment on the statement, never inside a path.

---

## Change 1 — one parser and one printer

New module `ctadl-ir/src/mir/path_syntax.rs`, re-exported from `ctadl_ir::mir`. It belongs
in `ctadl-ir`, not `ctadl-ascent`, because `mir::PathSegment`/`Offset`/`FieldAccesses` need
it for their `Display` impls and `ctadl-ir` cannot depend on `ctadl-ascent`. `facts::Path`
lives in `ctadl-ascent` and wraps it.

```rust
/// Where and why an access-path string is not a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathSyntaxError { pub at: usize, pub kind: PathSyntaxErrorKind }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSyntaxErrorKind {
    MissingLeadingDot,          // text before the first '.', or a segment not preceded by '.'
    EmptySegment,               // ".." or a trailing '.'
    UnterminatedOffset,         // "[42" with no ']'
    InvalidOffset(String),      // "[foo]", "[0x2a]", "[]" -- carries the bracket contents
    TrailingEscape,             // path ends in a lone '\'
}

/// Parse one segment, WITHOUT its leading '.'.
pub fn parse_segment(s: &str) -> Result<PathSegment, PathSyntaxError>;
/// Print one segment, WITHOUT a leading '.'.
pub fn write_segment(out: &mut String, seg: &PathSegment);
pub fn segment_to_string(seg: &PathSegment) -> String;

/// Parse a whole path into segments, in order. Does no normalization -- callers that need
/// offset-run merging pass the result to `facts::Path::from_accesses`.
pub fn parse_segments(s: &str) -> Result<Vec<PathSegment>, PathSyntaxError>;
/// Print a whole path.
pub fn write_path<'a>(out: &mut String, segs: impl IntoIterator<Item = &'a PathSegment>);
```

`Display for PathSyntaxError`: `` invalid access path at byte {at}: {kind} `` with kinds
reading e.g. `` expected '.' before an access-path segment ``, `` empty access-path segment ``,
`` '[foo]' is not a decimal offset; write '\[foo]' for a field named '[foo]' ``. That last
message is the one users will actually hit, so it must name the fix.

The per-segment pair is the important half: the models layer stores one segment per Arrow
row (Change 4) and needs to parse and print segments in isolation.

**One representability gap to close deliberately:** `Symbol("")` has no spelling in this
grammar (an empty segment is an error, and there is no escape that produces nothing). No
frontend appears to construct one, but `split_dot_segments` can today (`.a..b` →
`Symbol("")`), so it is reachable from a model file. `write_segment` should `debug_assert!`
on it and `Path::from_accesses` should reject it, rather than emit a string that will not
parse back.

---

## Change 2 — `facts::Path` conversions

In `ctadl-ascent/src/facts.rs`:

- **Delete** `parse_path_string` (`:457`). Replace with
  `pub fn Path::parse(s: &str) -> Result<Path, PathSyntaxError>`, which is
  `parse_segments(s).map(Path::from_accesses)`. Keep the normalization (offset-run merging,
  `Offset(0)` dropping) inside `from_accesses` where it is today — it is a *semantic*
  property of the path type, deliberately documented at `facts.rs:120-128`, not a parsing
  property. Note in the doc comment that parse is therefore not injective.
- **Rewrite** `to_dot_string` (`:171`) to delegate to `write_path`, so printer and parser
  cannot drift.
- **Delete** `impl From<&str> for Path` (`:550`) — an infallible conversion is the wrong
  shape now. `impl FromStr for Path` (`:556`) stays but its `Err` becomes `PathSyntaxError`
  instead of `()`. Every `.parse()` in the tree currently has a dead
  `unwrap_or_else(|_| Path::empty())`; those become real error handling.
- **Delete** `impl<S: AsRef<str>> FromIterator<S> for Path` (`:540`) and
  `impl From<&[&str]> for Path` (`:530`). Both silently make every segment a `Symbol` and
  are precisely how defect 2 happens. Replace with an explicitly-named
  `Path::from_symbol_names(iter: impl IntoIterator<Item = impl AsRef<str>>) -> Path` for the
  handful of places that genuinely want "these are all field names". Deleting the
  `FromIterator` impl is what makes the compiler find every site.
- **`impl Display for Path` (`:517`) becomes the canonical printer** — `to_dot_string()`,
  dropping the `path(...)` wrapper, so `Display` and `FromStr` are inverses. This changes
  user-visible output: `Display for FlowVertex` (`facts.rs:1113`) currently renders
  `%L1path(.deref)` and will render `%L1.deref`, which is what every hand-rolled
  `format!("{}{}", var, path.to_dot_string())` in `query_engine/` and `formatter.rs` was
  written to work around. Those hand-rolled sites can then be simplified, but that is
  cleanup, not required.

Test churn: `facts.rs` tests use `let p: Path = ".x.[2]".into();` at roughly 25 sites
(`:1590-1800`). They become `".x.[2]".parse().unwrap()`; a local
`fn p(s: &str) -> Path { s.parse().unwrap() }` in the test module keeps them terse.

---

## Change 3 — the parquet round-trip stops corrupting paths

`ctadl-ascent/src/facts/parquet.rs:516-541`. `encode_column` needs no change once
`to_dot_string` escapes properly. `into_decode_array`'s
`s.parse().unwrap_or_else(|_| facts::Path::empty())` must stop swallowing errors.

`DecodeColumn` returns `impl IntoIterator<Item = facts::Path>` with no error channel, so
this fails at the row level. Given decode is now infallible-by-construction for anything
this build wrote, a malformed row means a database written by an older build. Handle it as a
**store-version** problem rather than a per-row one:

- Add `INDEX_FORMAT_VERSION` to `ctadl-ascent/src/project.rs`, mirroring the existing
  `IMPORT_FORMAT_VERSION` (`:58-74`) and its `ArtifactImport::load` check (`:191-205`).
  Write it into `index/index_config.json` from `Project::index_path` (`:469`); check it
  where the query engine loads facts. Give it the same doc-comment history block. The
  existing `Error::IncompatibleImport` variant is the template for an
  `Error::IncompatibleIndex { found, expected, project }` whose message says
  **"re-run `ctadl index <project>`"**.
- Keep a `panic!` (not a silent `Path::empty()`) in the decoder as the backstop, with a
  message naming the offending string and pointing at the version check. It should be
  unreachable once the version gate is in.

---

## Change 4 — model ports parse with the shared grammar

`ctadl-ascent/src/models/json.rs`:

- `parse_access_path` (`:1774`) and `split_dot_segments` (`:1809`) are **deleted**; the tail
  capture goes straight to `path_syntax::parse_segments`.
- `ParsedPort.ap` (`:1728`) changes from `Vec<&'a str>` to `Vec<PathSegment>` (owned; the
  lifetime parameter survives only for `var_name`).
- A parse failure becomes a hard `JsonModelError`, following the existing convention in
  `error.rs:7-43` — every variant carries `index: usize`, `Display` is lowercase with no
  trailing period and appends `in model generator at index {index}`. Add:
  ```rust
  InvalidAccessPath { index: usize, text: String, source: PathSyntaxError },
  ```
  Raise it from `parse_port` (`:1732`) with `index: 0` and let the existing patch-the-index
  `map_err` at `:1757-1770` fill it in — extend that match with the new variant. Errors
  accumulate through `add_json_error` (`:369`) into `JsonModelErrors` like every other
  load-time error, so one bad port does not hide the rest.
- Two behavior changes fall out, both of which are the point:
  - `parse_access_path`'s special case mapping exactly `".*"` to the empty path disappears.
    `.*` now parses as `Symbol("*")` — a literal field, which is what it always was
    everywhere else. `default-propagation-models.md` §F6 wants these dropped anyway
    (Change 7).
  - `.[*]` becomes a hard error (`InvalidOffset("*")`) rather than `Symbol("[*]")`.
- While in `parse_port`: **anchor the three regexes** (`:146,151,156`). They are unanchored
  today, so `"MyReturnType"` matches `return_regex` and is silently accepted as a `Return`
  port with access path `Type`. Add `^…$` and let a non-match produce the existing
  `InvalidArgumentFormat`. Same fail-open class as `qualified-id-plan.md` Change 1.

`ctadl-ascent/src/models/mod.rs`:

- `AccessPathBuilder::append` (`:651`) takes `&[PathSegment]` instead of `&[&str]`.
- `AccessPathFieldBuilder::append` (`:703`) keeps its `field: Utf8` column but stores
  `segment_to_string(seg)` — the canonical **escaped segment** spelling, no leading dot.
  Chosen over adding a `kind: UInt8` discriminator column because it needs no schema change,
  no second encoding decision, and it exercises the same round-trip guarantee the whole
  change is establishing; a property test (below) pins it.
- `build_ap_map` (`:1171`) returns `HashMap<u64, facts::Path>` instead of
  `HashMap<u64, Vec<String>>`, building each path with `parse_segment` per row. The dedup key
  at `mod.rs:253-264` keeps `Vec<String>` of the canonical segment spellings.

`ctadl-ascent/src/codegen/models.rs:36,59` and
`ctadl-ascent/src/query_engine/endpoints.rs:211` then use the `facts::Path` from the map
directly, deleting the three `.iter().cloned().collect()` calls that were the Symbol-only
funnel. **This is the line that makes `Argument(1).[8].deref` produce a real `Offset(8)`.**

---

## Change 5 — the IR's `Display` impls speak the same grammar

`ctadl-ir/src/mir/mod.rs`:

- `Display for Offset` (`:233`) — signed hex → decimal.
- `Display for PathSegment` (`:224`), `Display for FieldAccess` (`:245`),
  `Display for FieldAccesses` (`:889`), `Display for FieldPath` (`:941`) — delegate to
  `write_segment` so symbols get escaped (none of them escape anything today) and offsets
  are decimal.
- Update the tests pinning hex: `ctadl-ir/src/mir/tests.rs:197,202,215,230` and
  `builder_tests.rs:166`.
- *Optional, per the hex-in-a-comment idea:* where a statement dump wants the old hex
  readability, append it as a trailing comment on the statement rather than inside the path.
  Not required for correctness; skip if it adds noise.

Also fix `examples/offset_example.rs`, which references `FieldAccess::Symbol` and
`FieldAccesses::mixed` — neither exists any more, so the example does not compile.

---

## Change 6 — flowy and the test DSL

- `ctadl-flowy/src/flowy.pest:31-36` already implements the grammar correctly for brackets
  (`offset_p = { "." ~ "[" ~ int ~ "]" }`, decimal, `[foo]` rejected). Its `ident`
  (`[A-Za-z_][A-Za-z0-9_]*`) is a strict *subset* of the canonical symbol production, which
  is fine for a hand-written test DSL — leave it. Add a test asserting that anything flowy
  accepts, `Path::parse` also accepts and agrees on.
- `ctadl-flowy/src/lib.rs:1126` `parse_p` **panics** on `star_p` (`.*`), so `x.* = y;` in a
  `.flowy` file crashes the tool. Convert to a `FlowyError::Compile { message, line, col }`
  (`lib.rs:337-352`).
- `ctadl-ascent/src/languages/tree_sitter/test_utils.rs:289` `parse_fields` splits on `.`
  and makes everything a `Symbol`. Route it through `path_syntax::parse_segments`. Its
  caller `access_path_from_str` (`:318`) does its own `strip_prefix`/`split_once` for the
  `$globals` / `@pN` variable prefix — that part stays. The direct test at `:760-763`
  asserting `"f.[3]"` → `Symbol("[3]")` must become `"f.\\[3]"` → `Symbol("[3]")`, or
  `"f.[3]"` → `Offset(3)` if that fixture wants an offset; pick per what the C frontend
  emits (it emits `Symbol("[3]")`, so escape it).

---

## Change 7 — shipped model files, schema, docs

- The complete set of port strings in the tree (shipped model files, `nightly/tests/**`,
  `ctadl-ascent/tests/**`) is: `Argument(*)`, `Argument(*).deref`, `Argument(0)`,
  `Argument(0).*`, `Argument(0).[*]`, `Argument(0).deref`, `Argument(0).rep`,
  `Argument(1)`, `Argument(1).deref`, `Argument(2)`, `Return`, `Return.*`, `Return.deref`,
  `Return.rep`, `Variable(...)`. **Exactly three change meaning** — `Argument(0).[*]`,
  `Argument(0).*`, `Return.*` — and all three live in
  `ctadl-ascent/src/languages/jadx/default-index.jsonl` lines 22 and 38.
- In that file: `"Argument(0).[*]"` is now a hard load error; drop it (it matched nothing
  anyway). The bare trailing `.*` on lines 22 and 38 now means `Symbol("*")` instead of the
  empty path; drop it too. Both are already slated for removal in
  `default-propagation-models.md` §4a. `.rep` (lines 52-55) and pcode's `.deref` are
  ordinary symbols and are unaffected, as is every `Variable(...)` port.
- `ctadl-ascent/src/models/ctadl-model-generator.schema.json` — `port-spec` /
  `source-sink-port-spec` / `field-spec` (`:222-241`). `field-spec`'s `^([.][^.]+)*` is
  unanchored at the tail and `*` matches zero occurrences, so it currently rejects nothing;
  anchor it and teach it the bracket and escape forms. Also reconcile
  `^Argument\((-?[0-9]+|\*)\)` (`:225,233`) with the loader's `\d+`, which rejects the
  negative index the schema permits — make the loader's regex the authority.
- `docs/model-generators.md:227-246` — replace the informal "a dot-path like `.foo.bar`"
  with the grammar above, state that `.[n]` is an **offset** and now really produces one,
  that a field name beginning with `[` is written `\[`, and give the per-frontend table
  (`Symbol("[]")` on dex/jvm, `Symbol("[_elem_]")` on lua and C, real `Offset`s on pcode).
  `:285-298` and `:334-335` already promise `.[8].deref` / `.[12].deref`; they become true.
  This section also serves `default-propagation-models.md` §0's deliverable.

---

## Migration

- Existing `index/` directories are stale: they hold unescaped `.[]` / `.[_elem_]` strings.
  The `INDEX_FORMAT_VERSION` gate from Change 3 turns that into "re-run `ctadl index`".
- Existing *imports* are unaffected — nothing in the import format encodes a path as a
  string (`ir-program.bitcode` is structural `bitcode`), so `IMPORT_FORMAT_VERSION` does not
  need a bump.
- User model files that spell `.[*]` or a trailing `.*` now fail to load, loudly, with a
  message naming the port. That is the intended fail-loud behavior, and only the shipped
  jadx file is affected in-tree.

---

## Commit sequence

Each commit builds and tests on its own.

1. **`path_syntax` module + tests** (Change 1). Pure addition, nothing calls it yet.
2. **`facts::Path` adopts it** (Change 2) + **parquet decode hardening and
   `INDEX_FORMAT_VERSION`** (Change 3). *This is the commit that changes analysis results*
   — bracketed symbols stop collapsing to the empty path. Keep it alone so the delta is
   attributable.
3. **IR `Display` impls** (Change 5). Output-only; no analysis change.
4. **Model ports** (Change 4) + the jadx default-file fixes and schema/docs (Change 7).
   Adds the offset capability; changes analysis results only for models that used `.[*]`
   or a trailing `.*`.
5. **flowy `star_p` fix and test-DSL adoption** (Change 6).

---

## Verification

**Unit — `ctadl-ir/src/mir/path_syntax.rs` tests (new).** The grammar table, one case each:
`""` → empty; `.foo`; `.foo.bar`; `.[42]`; `.[-8]`; `.[+8]`; `.foo.[42].bar`; `.a\.b`;
`.\[]`; `.\[_elem_]`; `.\[3]`; `.a\\b`. Errors: `foo` (no leading dot), `..a`, `.a.`,
`.[foo]`, `.[]`, `.[42`, `.[0x2a]`, `.a\`. Assert the `at` offset on each error.

**Unit — round-trip property, `ctadl-ascent/src/facts.rs` tests.** Extend the existing
`test_path_serialization` / `test_path_with_dots` / `test_path_with_offsets` block
(`:1804-1840`) with `parse(to_dot_string(p)) == p` over a fixed corpus that **must** include
`Symbol("[]")`, `Symbol("[_elem_]")`, `Symbol("[3]")`, `Symbol("a.b")`, `Symbol("a\\b")`,
`Offset(0)`, `Offset(-1)`, and adjacent offsets — the cases no test covers today and the
exact cases the parquet layer was destroying. Note the two normalizing exceptions
(`Offset(0)` dropped, adjacent offsets summed) explicitly rather than letting them look like
failures.

**Unit — model ports, `ctadl-ascent/src/models/json.rs` `mod parse_port_tests` (`:2128`).**
`Argument(0).[8].deref` yields `[Offset(8), Symbol("deref")]` — the assertion that defect 2
is fixed. Plus: `Argument(0).\[]` → `Symbol("[]")`; `Argument(0).[*]` → `InvalidAccessPath`;
`Argument(0).*` → `Symbol("*")`; `MyReturnType` → `InvalidArgumentFormat` (the anchoring
fix); `Argument(0).a..b` → `EmptySegment`.

**Integration — `ctadl-ascent/tests/json_error_handling.rs`.** One case per new hard error,
following `unknown_constraint_is_hard_error` (`:485`) and the `assert_unexpected_constraint`
helper (`:454`). Assert on the `JsonModelError` variant, not the message text.

**Integration — `ctadl-ascent/tests/models_loading.rs`.** A generator with an offset port
loads and produces a summary whose path is `Offset(8)`.

**End-to-end — the behavior-change measurement. This is the part that matters.**

Today `Symbol("[]")` / `Symbol("[_elem_]")` collapse to the *empty* path on every
index→query round trip, and the empty path is a prefix of everything, so `substitute_prefix`
(`facts.rs:216`) and `is_extension_of` (`facts.rs:233`) match far more broadly than the
frontend intended. After commit 2 they are real, distinct segments. Expect **both**
directions: precision gained, and flows lost where the collapse was doing accidental
field-insensitivity. Both are correct; the obligation is to quantify, not to avoid.

**Which frontends are exposed.** pcode emits real `Offset`s and `Symbol("deref")`, both of
which already round-trip, so `nightly/tests/c` (which runs through pcode — `cli::import`
`unimplemented!()`s on `-l c`, so the tree-sitter C frontend and its `Symbol("[3]")` /
`Symbol("[_elem_]")` are reachable only from unit tests) should be inert. The exposed
corpora are **lua** and **dex/jvm**.

**The direction of loss is specific:** taint stored at a *base* and read back through an
element field. Today the element segment vanishes, so `%arr.[]` and `%arr` are the same
vertex and the load hits directly; afterwards they are distinct and a non-saturating source
on the base no longer reaches the element read. The reverse direction is safe — sinks
default to `wildcard: true` (`docs/model-generators.md:333`), so a sink on `Argument(0)`
still catches `Argument(0).\[]`.

- Run `cargo xtask regression` before and after commit 2, per frontend
  (`--frontend dex`, `--frontend jvm`, `--frontend lua`, `--frontend pcode`), and diff the
  pass/fail sets. Highest risk: `nightly/tests/lua/array-flow.lua` (mixes `items[1] =` →
  `Symbol("[1]")` with `table.insert` → `Symbol("[_elem_]")`, two segments that are
  *different* before and after but for different reasons), the `unexpected_lines` lua cases
  (`multiple-return-flow`, `qualified-id-flow`, `require-module-flow`), and
  `nightly/tests/java/{ArrayFlow,ArrayFlowComplex,ArrayListFlow,ArrayListIteratorFlow}`.
  `nightly/tests/java` declares no `unexpected_lines`, so only `expected_lines` losses will
  show there.
- Any case that flips must be triaged individually into "the old pass was an artifact of the
  collapse" or "a real regression", and the verdict recorded in the commit message. Do not
  adjust `expected_lines` without that verdict.
- Record fact row-count deltas on at least one dex or lua target — the two exposed
  frontends. `RUST_LOG=info ctadl index` already logs the path-length distribution and the
  per-relation counts (`cli/mod.rs:1000-1030`), which is enough: path segments that
  previously vanished now survive, so distinct-path and vertex counts can only go up.
  (`default-propagation-models.md` §6 cites a `BENCHMARKS.md` and `.scratch/bench/suite.sh`;
  neither exists in this checkout — `.scratch/bench/` has `bench.sh` / `runall.sh` /
  `RESULTS.md` instead. Use those if a firmware-scale measurement is wanted, but pcode is
  the frontend this change does *not* touch, so it is not the gate here.)

**Suite.** `cargo test --workspace` (the `ctadl-ir` Display tests and `ctadl-flowy`'s
`tests/tnt/` corpus both move), and `cargo xtask regression` clean at the end of the
sequence.

## Follow-on, not in scope

- `default-propagation-models.md` §0b ("re-measure the two anomalous rows") is **answered**
  by this work — the drop is `facts/parquet.rs:538`. §0c ("make the offset gap explicit") is
  **implemented** by Change 4 rather than merely diagnosed. §0a's port-semantics matrix test
  and the §0 docs deliverable remain worth doing, and Change 7's docs section covers the
  latter.
- `docs/model-generators.md:245` advertises `field` / `fields` model keys; `models/json.rs`
  implements neither. Same fail-open class, out of scope here.
