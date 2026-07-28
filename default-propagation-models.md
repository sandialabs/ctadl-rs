# Default propagation models per language - DO-NOT-MERGE

Scope: **propagation models only** — the `model.propagation` half, which becomes function
summaries at `index` time. Sources and sinks are out of scope (see §7).

## Context

CTADL ships two built-in model files and loads **both, unconditionally, for every import**:

```rust
// models/mod.rs:41-57
pub fn try_load_default_models(program_info: &ProgramInfo) -> Result<ModelsBatch, Error> {
    let jadx_default  = include_bytes!("../languages/jadx/default-index.jsonl");   // 55 generators
    let pcode_default = include_bytes!("../languages/pcode/default-index.jsonl");  // 14 generators
    jadx_models.union_with(&pcode_models)?;
```

Called from `cli::index` (`cli/mod.rs:73`) and nowhere else. `cli::index` then uses only
`models_batch.summary` (`cli/mod.rs:100`) — the `endpoint` half is dropped, so the
`sources`/`sinks` entries in the pcode file are parsed, validated, and discarded on every
index. Propagation is the only half of a default model that does anything today.

Findings below marked **measured** were run against `target/release/ctadl` built at `0829891`.

### F1. Defaults are not language-aware

A Lua import matches all 55 Java generators and all 14 C ones; a flowy import matches all 69.
A full match pass per import, contributing nothing.

### F2. Port paths are a level shift; the one unexplained row is now explained

Fixture (Lua, so that a model can be attached to a function whose body propagates nothing):

```lua
local function id(v) return nil end
local function handler()
  local t = {}
  t.f = source()        -- IR: store %t.f := %%t1
  local u = id(t)       -- IR: direct-call main.id(%t)
  sink(u.f)             -- IR: %%t5 = load %u.f
end
```

Only the propagation port on `id` varies. Taint is stored at `t.f`; the sink reads `u.f`:

| model on `id` | effective summary | taint lands at | measured | expected |
| --- | --- | --- | --- | --- |
| *(no model — control)* | — | — | 0 | 0 |
| `Argument(0)` → `Return` | `u.X ← t.X` | `u.f` | 1 | 1 |
| `Argument(0).f` → `Return` | `u.X ← t.f.X` | `u` | 0 | 0 |
| `Argument(0)` → `Return.f` | `u.f.X ← t.X` | `u.f.f` | 0 | 0 |
| `Argument(0).[12]` → `Return` | `u.X ← t.[12].X` | *(nothing at `t.[12]`)* | 0 | 0 |
| `Argument(0).[_elem_]` → `Return` | `u.X ← t.[_elem_].X` | *(nothing at `t.[_elem_]`)* | **1** | **0** |
| `Argument(0).[zzz]` → `Return` | `u.X ← t.[zzz].X` | *(nothing at `t.[zzz]`)* | **1** | **0** |

> **Do not copy these port strings.** The table records the spelling as measured at `0829891`.
> Under the canonical grammar (`f84af56`), `Argument(0).[_elem_]` and `Argument(0).[zzz]` are
> **load errors** — an unescaped `[` at segment start means an offset. The equivalents today are
> `Argument(0).\[_elem_]` and `Argument(0).\[zzz]`, and `Argument(0).[12]` now means a real
> `Offset(12)` rather than `Symbol("[12]")`. The last two rows are the anomaly resolved in §0b.

**The semantics.** A propagation `In → Out` becomes `assign_like(out_var, out_path, in_var,
in_path)` (`codegen/models.rs:95-97`, consumed at `index_engine/mod.rs:1097-1103`). The two
forward field-propagation rules (`index_engine/mod.rs:1063-1072`) fire through
`Path::substitute_prefix` (`facts.rs:216`): taint at `in_var.p`, for any `p` extending
`in_path`, lands at `out_path` followed by the remaining suffix. A port pair is therefore a
**prefix substitution — a level shift, not a filter**:

- `A(0) → Return` maps `t.X` to `u.X` for every `X`. `t.f` reaches `u.f`, the sink fires.
- `A(0).f → Return` maps `t.f.X` to `u.X`. It *unwraps* a level: `t.f` lands on bare `u`, and
  the sink reads `u.f` — one level below where the taint now sits. **0 is the model doing
  exactly what it says**, not a defect.
- `A(0) → Return.f` maps `t.X` to `u.f.X`, so `t.f` lands at `u.f.f`. 0, again correctly.

An earlier draft of this finding read the `.f` row as a bug ("a bare port carries field-level
taint through an opaque function, a symbol-path port does not"). That was a misreading of the
fixture: the port selects where taint is *read from* and *written to*, and the fixture only ever
probes `u.f`. Four of six rows are correct behavior. **Withdrawn.**

**Bracketed segments are ordinary symbols, everywhere.** Traced end to end: `argument_regex`
(`json.rs:147`) captures the access path as `(.*)`; `split_dot_segments` (`json.rs:1809`) splits
on `.` and is bracket-blind; `AccessPathFieldBuilder` stores segments verbatim
(`models/mod.rs:703-707`); `codegen_summary` rebuilds via `FromIterator for Path`
(`facts.rs:540`), which maps every segment to `Symbol`. So `.[]`, `.[_elem_]` and `.[zzz]` reach
`facts.summary` as `Symbol("[]")`, `Symbol("[_elem_]")`, `Symbol("[zzz]")` — brackets intact.
(`parse_path_string` at `facts.rs:457`, which *does* silently drop non-numeric bracket contents,
is not on this path — the earlier draft was right to rule it out.)

That matters because the frontends spell the element field the same way:

- Lua: `PathSegment::symbol("[_elem_]")` (`lua/mod.rs:1921,1948,1953,2191,2208`)
- dex: `FieldPath::symbol("[]")` (`dex/mod.rs:751,774`)
- jvm: `mir::FieldPath::symbol("[]")` (`jvm/mod.rs:1046,1070`)

All three are plain symbols whose *name* happens to contain brackets, and a model port matches
them exactly. **The element field is modelable on all three frontends.** The earlier claim that
it was unmodelable was inferred from the misread rows. **Withdrawn.**

**~~What remains is one anomaly~~ RESOLVED (see §0b).** `.[_elem_]` and `.[zzz]` should both
score 0 here — nothing is stored at `t.[_elem_]` or `t.[zzz]`, so `substitute_prefix` cannot
match. Both measured 1: the score for a port strictly *wider* than the one written. The
measurement was correct; the "fourth path" is `facts/parquet.rs:538`, the index→query on-disk
round trip, which lies *past* the `parse_port → facts.summary` span this trace covers. It read
`.[_elem_]` back as the **empty** path, i.e. as the bare port, which scores 1. Fixed in
`f84af56`; **the caveat below is therefore stale.**

> **Stale, kept for the record.** The trace above concluded "(`parse_path_string` at
> `facts.rs:457`, which *does* silently drop non-numeric bracket contents, is not on this path —
> the earlier draft was right to rule it out.)" That is wrong. `parse_path_string` is not on the
> `parse_port → facts.summary` path, but it *is* on the `facts.summary → parquet → query` path,
> and that is where the segment was destroyed. The lesson: "bracketed segments are ordinary
> symbols, everywhere" held in-process and failed across the process boundary, which is the only
> boundary that matters when `index` and `query` are separate programs.

**~~One mechanism-identified defect~~ FIXED (see §0c).** `FromIterator for Path` mapped
every model-port segment to `PathSegment::Symbol`, so there was **no way to write a
`PathSegment::Offset` in a model file.** pcode is the only frontend that emits offsets
(`pcode/mod.rs:122`); on pcode a port `.[12]` was `Symbol("[12]")` and could never match the
`Offset(12)` the frontend produced. That impl is deleted and ports now parse with the canonical
grammar, so `.[12]` is a real `Offset(12)`.

One correction to the note as written: it said "Lua's numeric keys are
`symbol(format!("[{}]", …))` (`lua/mod.rs:1918,1945`), so `.[12]` does work there." Under the
canonical grammar it no longer does — `.[12]` is now an *offset*, and Lua's `t[12]` is the
*symbol* `[12]`, so a Lua port must be written `.\[12]`. The two were previously indistinguishable
(and, across the parquet boundary, Lua's was silently converted to an offset anyway, which is how
`t[1][2]` became `Offset(1),Offset(2)` and then summed to `Offset(3)` — the same path as `t[3]`).

Remaining consequences:

- **The four collection generators in the Java defaults are the standard synthetic-container
  idiom, not an accident.** `jadx/default-index.jsonl:52-55` use `.rep`, which no frontend
  emits; `List.add` writes it and `List.get` reads it, so add→get chains compose. That is by
  design. The real cost is that they never join a *real* array, which is written and read as
  `[]` — and per the trace above `[]` **is** writable, so `.rep` → `[]` is a live option (§4a),
  **no longer gated** — the F2 re-measure resolved to a fixed encoding bug (§0b), and `[]`
  now survives the index→query round trip.
- pcode's entire default file is built on `.deref`, a plain symbol, so it is in the working
  class — **still not measured** (no Ghidra in this environment); §0 should confirm it, but it
  is no longer a project-gating risk.

### F3. Lua externals cannot be modeled at all — measured

The IR names them correctly:

```
%%t2, %%t3 = direct-call string.format(<const: "\"echo %s\"">, %x) [9]
%%t4, %%t5 = direct-call os.execute(%cmd) [10]
```

but `VirtualMethodTable::Lua.functions` (`ctadl-ir .../call.rs:185-197`, built at
`lua/mod.rs:986-995`) holds *only functions the frontend lowered*, and
`ModelGeneratorIngest::new` builds every match index from it (`models/json.rs:279-307`). A
model naming `execute`, and a second naming `qualified-id: "os.execute"`, both match nothing:

```
$ ctadl query luaproj -m q.json -o out.sarif
Matched 1 sources and 0 sinks        # the 1 is a *defined* Lua function
```

dex/jvm push referenced-only methods into the VMT from `Context::ext` (`dex/mod.rs:436-438`,
`jvm/mod.rs:409-418`); pcode gets real `<EXTERNAL>::…` thunk functions. Lua has no equivalent,
so **a Lua propagation-model file is inert until this is fixed.**

Related, also measured — method-call syntax lowers to the **bare** name:

```
%%t2, %%t3 = direct-call sub(%s, <const: "1">, <const: "3">) [6]    -- from s:sub(1,3)
%%t5, %%t6 = direct-call format(%%t4, @p0) [8]                       -- from ("lit %s"):format(a)
%%t8, %%t9 = direct-call table.concat(%%t7, <const: "\",\"">) [10]   -- from table.concat(...)
```

so `string.format(x)` and `s:format(x)` are two different callee names for one library
function.

### F4. Source-level C is not importable — measured

```
$ ctadl import -l c nightly/tests/c/callchain.c -n ctest
thread 'main' panicked at ctadl-ascent/src/cli/mod.rs:44:14: not implemented
```

`-l c` is in the CLI enum (`main.rs:168`) but `cli::import` panics on it. The tree-sitter C
frontend is reachable only from its own tests and builds no VMT, so `ModelGeneratorIngest`
would fall to the `Unknown` arm (`json.rs:308-320`). **C means pcode** — which is also how
`nightly/tests/c/*` runs.

### F5. `jadx/` is not a frontend

`languages/mod.rs` declares `dex`, `jvm`, `lua`, `pcode`, `tree_sitter`. `languages/jadx/`
contains one file, the model file, and no `mod.rs`.

### F6. Two more spellings in the Java defaults — DONE, ahead of §4a

A trailing `.*` (`jadx/default-index.jsonl:22,38`) parses to the **empty** path
(`json.rs:1776`), so `Argument(0).*` is a synonym for `Argument(0)` — harmless, but it reads
as a wildcard and is used as one. `Argument(0).[*]` (line 22) is *not* the same case: it parses
to `Symbol("[*]")`, a literal field name no frontend emits, so that generator matches nothing.
~~Both are dropped in §4a.~~ **Both are already dropped**, in `6bb2e63`, because the canonical
grammar makes `.[*]` a hard load error and `.*` a literal field named `*` — leaving them would
have changed their meaning silently.

Two corrections to this finding as written. **The line numbers are 22, 37 and 38**, not 22 and
38: an exhaustive scan of every port string in the tree (11 distinct ports across shipped model
files, `nightly/tests/**` and `ctadl-ascent/tests/**`) found a third `Argument(0).*` on line 37.
And the `.*` entries were **rewritten, not deleted** — since `.*` denoted the empty path, dropping
the whole propagation would have silently removed a live `Argument(0) → Return` rule; only the
suffix came off. `Argument(0).[*]`, which genuinely matched nothing, did lose its entry.

---

## Phases

| Phase | What | Gates |
| --- | --- | --- |
| 0 | Pin port path semantics as a test; ~~resolve the F2 anomaly~~ (0b/0c done) | 0a only; element-field ports unblocked |
| 1 | Dispatch defaults on the VMT; relocate the files | — |
| 2 | Lua VMT gains externals; ship `lua-index.jsonl` | needs 1 |
| 3 | Expand `native-index.jsonl` (C via pcode) | **measure per §6** |
| 4 | Expand `java-index.jsonl` | — (`.rep` → `[]` needs 0) |

---

## Phase 0 — pin port path semantics (0b and 0c resolved; 0a remains)

Scope shrank once F2 was re-read: port paths are prefix substitution and behave as written on
every row but two. Bare-port content (phases 2-4) no longer waits on this. ~~What still depends on
it is anything that names an element field — `.rep` → `[]` in phase 4, the held container
entries in phase 2.~~

**Scope shrank again.** The two anomalous rows are explained and the offset gap is closed, both by
`access-path-parsing-plan.md` (commits `f84af56` and `6bb2e63`) — see 0b and 0c below. So the
element-field work in phases 2 and 4 is **no longer gated on anything**; what remains of phase 0
is 0a, the semantics matrix, which is now a nice-to-have rather than a blocker. One thing did get
*added* to it: the grammar split `.[foo]` into an escaped-symbol form and an offset form, so the
matrix has more rows than it did.

**0a. Pin the matrix as a test.** *(The one item of phase 0 still open.)* Extend
`ctadl-ascent/tests/` with a fixture per frontend that pins, for each port shape below, on both
the input and output side: where does taint land, and does the sink observe it? Probe at
the level the port *predicts* under prefix substitution, not just at one fixed depth — the
original matrix scored 0 on three rows purely because it always probed `u.f`. Do it for flowy
(cheapest — `ctadl_flowy::compile_program_contents` needs no toolchain and `tests/tnt/` already
exists), Lua, and pcode. The pcode `.deref` column is still unverified.

The shapes have changed since this was written, because the canonical grammar split what used to
be one case into three. The row set should now be:

| shape | example | meaning |
| --- | --- | --- |
| bare | `Argument(0)` | the whole value |
| `.symbol` | `.f` | a named field |
| `.[numeric]` | `.[12]` | a real `Offset(12)` — matches pcode; **new**, was `Symbol("[12]")` |
| `.\[symbol]` | `.\[_elem_]`, `.\[]` | a `Symbol` whose *name* has brackets — matches lua/dex/jvm |
| `.[symbol]` | `.[_elem_]` | **hard load error** now; assert the error, not a score |
| `.deref` | `.deref` | pcode's pointee |
| two-segment | `.[8].deref` | the mixed case that motivated all of this |

The `.[numeric]` / `.\[symbol]` split is the substance: those two were the *same* port before, and
on lua and tree-sitter C the frontends emit the escaped one while pcode emits the offset. The
existing round-trip and port tests (`facts.rs::test_path_round_trip_corpus`,
`models_loading.rs::offset_port_produces_a_real_offset`) pin the *encoding*; 0a is still needed to
pin the *taint semantics* end to end, which nothing does today.

**~~0b. Re-measure the two anomalous rows, then explain them.~~ RESOLVED — no re-measurement
needed.** The measurement was right and the trace was looking in the wrong place. The drop is
`facts/parquet.rs:538`, the on-disk encoding for *every* `Path` column, which `index` writes and
`query` reads back in a separate process. `to_dot_string` escaped `.` but not `[`, and the
reader's `parse_path_string` treated a bare `[` as an offset and silently dropped a segment whose
bracket contents were not a valid `i64`. So a port `Argument(0).[_elem_]` was stored correctly as
`Symbol("[_elem_]")`, written as `.[_elem_]`, and read back as **the empty path** — i.e. as the
bare port `Argument(0)`, which is exactly the score of 1 that was measured where 0 was predicted.
Same for `.[zzz]`.

F2's trace above was sound as far as it went — nothing between `parse_port` and `facts.summary`
touches bracket contents — but it stopped at `facts.summary`, and the corruption happens *after*
that, on the way back off disk. Its parenthetical ruling `parse_path_string` out was therefore
wrong: that function is not on the `parse_port → facts.summary` path, but it is on the
`facts.summary → parquet → query` path, which is the one that matters. **This was the "fourth
path not yet found."** Fixed in `f84af56` (`access-path-parsing-plan.md` Changes 1–3); the
corpus in `facts.rs::test_path_round_trip_corpus` pins it, and running the deleted parser
verbatim against the strings found on disk confirms `.[_elem_]` → `[]` and `.[1]` → `Offset(1)`.

**~~0c. Make the offset gap explicit.~~ RESOLVED — implemented, not merely diagnosed.** Model
ports parse with the canonical access-path grammar, so `.[12]` really does produce
`PathSegment::Offset(12)` and matches what pcode emits; a field name beginning with `[` is
written `\[` and stays a `Symbol`. Both halves of the "avoid the status quo" requirement are met:
the offset spelling now means what it says, and a malformed path (`.[*]`, `.[foo]`, an empty
segment) is a hard `JsonModelError::InvalidAccessPath` naming the port and the fix, per the
`qualified-id-plan.md` fail-loud argument. Implemented in `6bb2e63`
(`access-path-parsing-plan.md` Changes 4 and 7); pinned by
`models_loading.rs::offset_port_produces_a_real_offset` and the `json_error_handling.rs`
malformed-path cases.

**0d. Then take the element-field decision.** With `[]` and `[_elem_]` confirmed writable
(F2) **and now confirmed to survive the index→query round trip** (0b), phase 4 can change
`.rep` → `[]` and phase 2 can model Lua containers. **Both are now ungated.** Note that this
decision could not have been taken correctly before 0b: prior to the fix a `.rep` → `[]` change
would have looked like it worked while `[]` silently collapsed to the empty path, making the
generator match everything rather than array elements.

**Deliverable: DONE.** `docs/model-generators.md` §6 now carries the access-path grammar, states
that `.[n]` is an offset and now really produces one, that a field name beginning with `[` is
written `\[`, and gives the per-frontend table of which spelling matches what
(`Symbol("[]")` on dex/jvm, `Symbol("[_elem_]")` on lua and tree-sitter C, real `Offset`s on
pcode). **Still missing there:** the statement that a port pair is a *prefix substitution*, with
the `A(0).f → Return` unwrapping example — that is the part that misreads, and it remains
worth writing.

---

## Phase 1 — dispatch on the VMT, relocate the files

Independent of phase 0; can land first.

### 1a. Dispatch

`try_load_default_models` already has what it needs; no signature change. Match on
`program_info.vmt`:

| VMT variant | Frontends | Loads |
| --- | --- | --- |
| `Java { .. }` | dex, apk, jvm, jar | `java-index.jsonl` |
| `Native { .. }` | pcode (and clang later) | `native-index.jsonl` |
| `Lua { .. }` | lua | `lua-index.jsonl` |
| `Unknown` | flowy | nothing |

The VMT is the right key rather than `ArtifactLanguage`: dex and jvm want the same file and
differ only in `ArtifactLanguage`, it keeps `cli::index` from threading the language down, and
it gives flowy the correct answer (nothing) for free.

### 1b. Relocate

```
ctadl-ascent/src/models/defaults/{java,native,lua}-index.jsonl
```

Delete `languages/jadx/`. Update the two `include_bytes!` and `docs/model-generators.md:52`.
`jadx` names a decompiler that is not a frontend here, `pcode` is one of two possible C paths,
and these are model *data*, not language *code*. Naming them for the VMT variant that selects
them makes 1a's table the whole story. Keep the `-index` suffix: it names the stage they load
at, and leaves room if query-time defaults are ever added.

### 1c. `--no-default-models`

Add to `IndexArgs` and `GoArgs`, threaded to `cli::index`. Needed for the A side of the phase-3
measurement, and for anyone who wants a model file to be the complete story.

### 1d. Strip the dead endpoints

Remove the `sources`/`sinks` entries from the pcode file as part of the move. They have never
had an effect (`cli::index` uses `.summary` only) and their presence implies CTADL ships
default sources and sinks, which it does not.

### 1e. Tests

- One unit test per VMT arm: a Lua `ProgramInfo` loads zero Java generators and vice versa.
  Assert on `ModelsBatch::summary.num_rows()`.
- A test that walks every file in `defaults/` and asserts zero `JsonModelError`s. The loader
  hard-errors on unknown fields (`docs/model-generators.md:116-137`), so a stale default file
  breaks *every* index — this is the cheap guard against schema drift.

---

## Phase 2 — Lua externals, then Lua propagation models

### 2a. The frontend change (prerequisite, per F3)

Add an externals column to `VirtualMethodTable::Lua`:

```rust
Lua {
    methods:   Vec<(Symbol, Symbol, Symbol)>,
    functions: Vec<(Symbol, Symbol)>,   // lowered definitions (unchanged)
    externals: Vec<(Symbol, Symbol)>,   // (simple, fq) for called-but-undefined
    hierarchy: HashMap<Symbol, SmallVec<[Symbol; 2]>>,
}
```

Populate in `Lowerer::build_vmt` (`lua/mod.rs:955`) from a set the call lowering fills whenever
it emits a `CallEdges::Explicit` target resolving to no definition. Sort it — it comes from a
`HashSet` and the VMT feeds resolvent order.

Register each external under **both** spellings — fq (`os.execute`) and last dotted component
(`execute`) — because of the method-call lowering in F3: one generator then covers both
`string.format(x)` and `s:format(x)`. Index them in `ModelGeneratorIngest::new`'s Lua arm
exactly as `functions` is indexed today (`json.rs:279-307`): simple *and* fq into
`program_method_names`/`program_method_signatures`, fq **only** into
`program_method_qualified_ids` — the comment there explains why keying `qualified-id` on bare
names reintroduces the collisions it exists to remove. Add them to `universe` so a top-level
`not` sees them.

Externals have no `FunctionData`, so `has_code`/`number_parameters`/`uses_field` will not match
them. That is already true of dex/jvm `ext` entries; document it rather than fix it.

**Alternative considered:** a generic fallback in `ModelGeneratorIngest::new` walking
`program_info.program` for explicit call targets absent from the VMT. One change, helps every
frontend, no IR change. Rejected: that constructor runs once per model file per import, so it
would re-walk every statement each time, and it puts frontend knowledge in the matcher. The VMT
is the frontend's declared interface for this question and dex/jvm already answer it there.

**Verification (must flip):** the F3 repro goes from `1 sources and 0 sinks` to
`1 sources and 1 sinks`.

### 2b. `lua-index.jsonl`

**Every entry below uses bare ports.** Not because paths are broken — F2's re-read shows
`[_elem_]` is writable and matches the Lua frontend exactly, and as of `f84af56` it also survives
the index→query round trip — but because a bare port is the strictly more permissive choice and
the level-shifting entries deserve their own measured pass. They are listed separately below so
they are not forgotten. When they are written, the Lua element field is spelled `.\[_elem_]`
(escaped); a bare `.[_elem_]` is now a load error.

Each `names` list covers the bare form, so `string.sub(s,…)` and `s:sub(…)` both hit it.

| Group | Names | Propagation |
| --- | --- | --- |
| string producers | `format`, `rep`, `sub`, `upper`, `lower`, `reverse`, `char` | `Argument(*)` → `Return` |
| string transforms | `gsub`, `match`, `gmatch`, `byte`, `len` | `Argument(0)` → `Return` |
| table read | `table.concat`, `table.remove`, `table.unpack`, `unpack` | `Argument(0)` → `Return` |
| raw access | `rawget`, `next` | `Argument(0)` → `Return` |
| raw access | `rawset` | `Argument(2)` → `Argument(0)` |
| coercion | `tostring`, `tonumber`, `assert`, `setmetatable` | `Argument(*)` → `Return` |
| os | `os.date`, `os.getenv`, `os.tmpname` | `Argument(*)` → `Return` |
| io | `io.open`, `io.read`, `io.lines`, bare `read` | `Argument(*)` → `Return` |
| io | bare `write` | `Argument(1)` → `Argument(0)` |
| json | `cjson.encode`, `cjson.decode`, `cjson.safe.encode`, `cjson.safe.decode` | `Argument(0)` → `Return` |
| openresty codecs | `ngx.escape_uri`, `ngx.unescape_uri`, `ngx.encode_base64`, `ngx.decode_base64`, `ngx.encode_args`, `ngx.decode_args`, `ngx.quote_sql_str`, `ngx.md5`, `ngx.sha1_bin` | `Argument(0)` → `Return` |
| openresty regex | `ngx.re.match`, `ngx.re.gsub`, `ngx.re.sub` | `Argument(0)` → `Return` |

*~~Held for phase 0b~~ — unblocked (0b resolved), still unmeasured.* These are the
level-correct forms. **Spell the element field `.\[_elem_]`, escaped** — under the canonical
grammar a bare `.[_elem_]` is a hard load error, since `[` at segment start means an offset:
`table.concat` reading `A(0).\[_elem_]` rather than `A(0)`; `rawset` writing
`A(0).\[_elem_]`; a `table.remove`/`unpack` that unwraps one level
(`A(0).\[_elem_] → Return`, which puts an element's taint on the returned value rather than
at `Return.\[_elem_]`). The bare-port versions
above are strictly more permissive, which is the safe direction for a default, but they lose the
distinction between "the table is tainted" and "an element is".

**Deliberately absent — do not add:** `table.insert`, `ipairs`, `pairs`, `select`. The
frontend recognizes these syntactically and lowers them straight to dataflow
(`lua/mod.rs:2180-2208`); a model would double-count. Put a comment saying so at the top of the
file, because "the stdlib list is missing `table.insert`" reads as an oversight.

**Gaps to record, not fix:** `pcall`/`xpcall`/`coroutine.*` invoke a function-valued argument
and need `forward_call`, unimplemented (`docs/model-generators.md:144-150`).

---

## Phase 3 — expand `native-index.jsonl` (C via pcode)

Keep the existing 14 generators; drop their `sources`/`sinks` (§1d). `.deref` is a plain symbol
and so behaves as written; phase 0a should still confirm the pcode column, but this phase is no
longer blocked on it. ~~Note per F2 that `.[<numeric>]` cannot match a pcode `Offset` — no entry
below uses one, and none should be added until 0c.~~ **0c is resolved:** `.[<numeric>]` is now a
real `Offset` and *does* match what pcode emits, so offset entries are writable here. (On lua
and tree-sitter C the same spelling means an offset too, which is *not* what those frontends
emit — there the element field must be escaped, `.\[_elem_]`.)

### 3a. Highest-value addition: symbol aliases

The corpus in `BENCHMARKS.md` §3 is glibc/uClibc ARM firmware, where the imported symbol is
frequently not the plain libc name: `__memcpy_chk`, `__strcpy_chk`, `__sprintf_chk`,
`__snprintf_chk`, `__strcat_chk`, `__vsnprintf_chk`, `__printf_chk`, `__isoc99_sscanf`,
`__isoc99_fscanf`. The pcode frontend strips Ghidra's `<EXTERNAL>::…@addr` decoration but not
the symbol itself. The `_chk` variants take extra size/flag arguments, but every model that
would cover them is positional at index 0/1 or uses `Argument(*)`, so **the fix is to extend
the existing `names` lists** — no new generators, no index arithmetic. Add the
leading-underscore forms (`_strcpy`, `_memcpy`, …) the same way the Python CTADL defaults do
for decorated Mach-O symbols.

Cheapest coverage win in the plan, and aimed squarely at the corpus the benchmarks measure.

### 3b. New generators

| Group | Names | Propagation |
| --- | --- | --- |
| BSD-safe string | `strlcpy`, `strlcat` | `A(1).deref`→`A(0).deref`, `A(2).deref`→`A(0).deref` |
| char conversion | `toupper`, `tolower`, `toascii` | `A(0)` → `Return` |
| line readers | `getline` | `A(2).deref` → `A(0).deref.deref` |
| line readers | `getdelim` | `A(3).deref` → `A(0).deref.deref` |
| path | `basename`, `dirname` | `A(0).deref` → `Return.deref` |
| path | `realpath` | `A(0).deref`→`A(1).deref`, `A(0).deref`→`Return.deref` |
| path | `readlink`, `readlinkat` | `A(0).deref` → `A(1).deref` |
| tokenize | `strtok_r` | `A(0).deref`→`Return.deref`, `A(0).deref`→`A(2).deref.deref` |
| mem search | `memchr`, `memrchr`, `memmem`, `rawmemchr` | `A(0).deref` → `Return.deref` |
| string search | `strcasestr`, `index`, `rindex` | `A(0).deref` → `Return.deref` |
| byte order | `ntohl`, `ntohs`, `htonl`, `htons`, `bswap_16/32/64` | `A(0)` → `Return` |
| inet | `inet_ntoa` | `A(0)` → `Return.deref` |
| inet | `inet_ntop` | `A(1).deref`→`A(2).deref`, `A(1).deref`→`Return.deref` |
| inet | `inet_addr`, `inet_aton`, `inet_pton` | `A(0).deref`→`Return`, `A(0).deref`→`A(1).deref` |
| time | `strftime` | `A(3).deref` → `A(0).deref` |
| time | `ctime`, `asctime` | `A(0).deref` → `Return.deref` |
| wide/multibyte | `mbstowcs`, `wcstombs`, `mbsrtowcs` | `A(1).deref` → `A(0).deref` |

Multi-segment paths (`A(0).deref.deref` on `getline`/`getdelim`, `A(2).deref.deref` on
`strtok_r`) are the only two-segment ports in the file. Under prefix substitution they are a
two-level shift; phase 0a's matrix must cover the two-segment case before these land.

### 3c. Two existing entries to revisit

- **`realloc` sits in the allocator group** whose model is `Argument(*)` → `Return.deref`. For
  `malloc`/`calloc` that reads as "the returned buffer is tainted by the size argument", which
  is presumably a deliberate way to *materialize* `Return.deref` as a vertex rather than a
  claim about taint — if so it needs a comment, because it reads as a bug. For `realloc` it is
  separately wrong in a way a comment won't fix: the old contents survive the call and the
  model drops them. Split out `realloc`, `reallocarray` with `A(0).deref` → `Return.deref`.
- **`qsort`/`heapsort`/`mergesort`** carry `A(0).deref` → `A(0).deref`, a self-loop that
  derives nothing. Drop it.

### 3d. Cost note on `Argument(*)`

`compute_arg_arity` (`index_engine/mod.rs:258-279`) takes the **max over actual call sites**, so
`Argument(*)` on a variadic fans out to the widest call in the program. One 12-argument
`snprintf` anywhere makes every `snprintf` summary 12×12 program-wide. The printf/scanf
families are the only `Argument(*)` users here; keep it that way.

---

## Phase 4 — expand `java-index.jsonl`

### 4a. `.rep` → `[]` is a live option, ungated (0b resolved)

The earlier draft ruled this out on the grounds that `[]` could not survive the port parser. It
can (F2), and as of `f84af56` it also survives the index→query round trip, which is the part that
was actually broken (§0b). dex and jvm both write `FieldPath::symbol("[]")`. So the four
collection generators at `jadx/default-index.jsonl:52-55` can be moved off the synthetic `.rep`
and onto the field the frontends actually emit, which makes `List.add`/`List.get` chains join
real array writes instead of only each other.

> **Spell it `.\[]`, not `.[]`.** Under the canonical grammar an unescaped `[` at segment start
> is an *offset*, so a port written `Argument(0).[]` is now a hard load error
> (`InvalidOffset("")`). The Java array element is a `Symbol` whose name is `[]`, which is
> written `Argument(0).\[]` — and in JSON, `"Argument(0).\\[]"`. The error message names the
> fix, so getting this wrong fails loudly rather than silently matching nothing.

Do it as its own commit with the 0a matrix test green, and check the `unexpected_lines` cases in
`nightly/tests/java` — `[]` is a real field with real writes behind it, so this widens what the
generators reach, which is the point and also the risk. ~~If 0b comes back showing bracketed
symbols behave anomalously after all, leave `.rep` and add a comment saying why.~~ 0b came back
clean: the anomaly was the parquet encoding, now fixed, so there is no remaining reason to hedge.

~~Independently, drop the bare trailing `.*` on lines 22 and 38 … `Argument(0).[*]` … drop it
too.~~ **Already done** in `6bb2e63` — see F6, including the two corrections there (it was lines
22, 37 *and* 38, and the `.*` entries were rewritten rather than deleted, since `.*` denoted the
empty path and dropping them outright would have removed a live rule).

### 4b. The rule for `parents`

`parent`/`parents` matches the class **declared at the invoke site**, not the runtime type.
That is why the existing `iterator` generator lists eight parents. Every generator below must
list the interface *and* the common concrete types. State this at the top of the file.

Also note: dex and jvm name instance fields differently (dex uses the bare field name,
`dex/mod.rs:661`; jvm uses `<class->field:descriptor>`, `jvm/mod.rs:632`), so default models
must stay positional — named-field ports are not portable between the two Java frontends.

### 4c. New generators

The gap is `java.util`. All bare-port except where a level change is genuinely required, and
those are marked *(held)* — ~~pending phase 0b~~ **now unblocked (0b resolved)**; they remain
unmeasured. Any element-field port among them is written `.\[]` on dex/jvm, escaped.

| Group | Parents | Members | Propagation |
| --- | --- | --- | --- |
| Map read | `Map`, `HashMap`, `LinkedHashMap`, `TreeMap`, `Hashtable`, `ConcurrentHashMap` | `get`, `getOrDefault`, `remove` | `A(0)` → `Return` |
| Map views | same | `keySet`, `values`, `entrySet` | `A(0)` → `Return` |
| Map write | same | `put`, `putIfAbsent` | *(held)* `A(1)`,`A(2)` → `A(0).<elem>` |
| Map.Entry | `Map$Entry` | `getKey`, `getValue` | `A(0)` → `Return` |
| Collection read | `Collection`, `List`, `Set`, `Queue`, `Deque`, `ArrayList`, `LinkedList`, `Vector`, `Stack`, `HashSet`, `TreeSet`, `ArrayDeque` | `get`, `peek`, `poll`, `pop`, `remove`, `getFirst`, `getLast`, `element` | `A(0)` → `Return` |
| Collection bulk | same | `toArray`, `subList`, `stream` | `A(0)` → `Return` |
| Collection write | same | `add`, `addAll`, `push`, `offer`, `addFirst`, `addLast`, `set` | *(held)* `A(1)` → `A(0).<elem>` |
| Collections | `Collections` | `unmodifiableList/Map/Set`, `singletonList`, `nCopies` | `Argument(*)` → `Return` |
| Arrays | `Arrays` | `copyOfRange`, `stream` | `A(0)` → `Return` |
| String | `String` | `join`, `copyValueOf`, `strip`, `stripLeading`, `stripTrailing`, `repeat`, `intern` | `Argument(*)` → `Return` |
| StringBuilder | `StringBuilder`, `StringBuffer` | `reverse`, `deleteCharAt`, `replace`, `setCharAt`, `substring` | `A(0)`→`Return`, `A(*)`→`A(0)` |
| Base64 (SE) | `Base64$Encoder`, `Base64$Decoder` | `encode`, `encodeToString`, `decode` | `A(1)` → `Return` |
| Base64 (Android) | `android/util/Base64` | `encode`, `encodeToString`, `decode`, `decodeToString` | `A(0)` → `Return` |
| URL codec | `URLEncoder`, `URLDecoder` | `encode`, `decode` | `A(0)` → `Return` |
| Scanner | `Scanner` | `next`, `nextLine`, `nextInt`, `nextLong`, `nextDouble` | `A(0)` → `Return` |
| Scanner | `Scanner` | `<init>` | `A(1)` → `A(0)` |
| Properties | `Properties` | `getProperty` | `A(0)` → `Return` |
| Properties | `Properties` | `setProperty` | `A(2)` → `A(0)` |
| Objects | `Objects` | `toString`, `requireNonNull`, `requireNonNullElse` | `Argument(*)` → `Return` |
| Optional | `Optional` | `of`, `ofNullable`, `get`, `orElse`, `orElseGet` | `Argument(*)` → `Return` |
| Streams | `InputStream`, `Reader`, `BufferedReader`, `InputStreamReader` | `readAllBytes`, `readNBytes`, `lines` | `A(0)` → `Return` |
| Writers | `PrintStream`, `PrintWriter`, `Writer`, `BufferedWriter` | `print`, `println`, `printf`, `format`, `append` | `A(1)` → `A(0)` |
| Decompress | `GZIPInputStream`, `InflaterInputStream`, `ZipInputStream` | `<init>` | `A(1)` → `A(0)` |
| JSON | `org/json/JSONObject`, `org/json/JSONArray` | `get`, `getJSONObject`, `getJSONArray`, `optString`, `put`, `<init>` | `A(*)`→`Return`, `A(1)`→`A(0)` |
| Gson | `com/google/gson/Gson` | `toJson`, `fromJson` | `A(1)` → `Return` |
| Enum | `Enum` | `name` | `A(0)` → `Return` |
| Android Intent | `Intent` | `getStringExtra`, `getData`, `getAction` | `A(0)` → `Return` |
| Android Intent | `Intent` | `putExtra` | `A(2)` → `A(0)` |
| Android Uri | `android/net/Uri` | `parse`, `toString`, `getQueryParameter`, `buildUpon`, `build` | `Argument(*)` → `Return` |
| Android prefs | `SharedPreferences`, `SharedPreferences$Editor` | `getString`, `putString` | `A(0)`→`Return`, `A(2)`→`A(0)` |

The existing `System.arraycopy` entry (`A(0)` → `A(2)`) is correct as a bare port and should
stay that way.

---

## 6. Verification

**Per phase.**

- Phase 0: the matrix test itself is the deliverable, plus the docs section. Each row asserts
  where taint lands, not merely whether a fixed probe fires — that distinction is what F2's
  first draft got wrong. Treat a spelling whose effective path differs from its written path as
  a bug to be fixed or rejected, not documented as a quirk.
- Phase 1: the unit tests in 1e; `RUST_LOG=info ctadl index` on a Lua project no longer reports
  Java summary rows.
- Phase 2: the F3 repro must flip to `1 sinks`.
- Phases 2-4: add one `nightly/` case per frontend that passes **with no propagation model of
  its own** — Lua running `io.read` → `string.format` → `os.execute`, a pcode case through
  `strcpy`/`snprintf`, a dex case through `StringBuilder`. Today nothing tests the default files
  at all: every nightly case supplies its own models, which is why port-path semantics were
  never pinned anywhere.
- Phase 4: re-run `nightly/tests/java`; check the `unexpected_lines` cases still hold — that is
  where a too-broad container model shows up.

**Phase 3 has a real risk and an existing gate.** Every new propagation generator adds summary
edges at every call site of the modeled function, and `smbd`/`wpa_supplicant` call
`strcpy`/`snprintf` thousands of times. `BENCHMARKS.md` §2 records four of eight firmware
targets already blowing past an 85 GB guard on both sides. Follow §0 of that document: build
release, re-run `.scratch/bench/suite.sh` for `gen_3200`, `ath_dfs`, `ath_dev` before and after,
record the delta in §2.

Note that §0's first gate — "`locals` row count must not drift unless you *intend* a semantic
change" — **is expected to trip**: adding library summaries is precisely a semantic change. The
obligation is to quantify it, not avoid it. Land phase 3 as its own commit so the row delta is
attributable, and use `--no-default-models` (1c) for the A side.

---

## 7. Out of scope

- **Sources and sinks.** Out of scope by request. Recording one measured fact for whenever they
  are picked up: source and sink `kind`s do **not** have to pair. An `AAA` source into a `ZZZ`
  sink reports `fail C0001.tainted-path — Taint flow labelled 'AAA'`, contradicting
  `docs/model-generators.md:501`. So a default source/sink set would report the cross product of
  every default source and sink in any program, which argues for opt-in bundles rather than
  always-on. CTADL ships none today (§Context).
- **Wiring `-l c`.** The tree-sitter C frontend builds no VMT and `cli::import` panics on it
  (F4). When it lands it gets a `Native` VMT and inherits `native-index.jsonl` free — an
  argument for naming the file after the VMT variant rather than after `pcode`.
- **PHP.** `ctadl/src/ctadl/models/squiggli_php/default-index.json` is `{}` and there is no PHP
  frontend here.
- **`find: variables` / `find: fields`.** Unimplemented in the loader
  (`docs/model-generators.md:104-108`).
