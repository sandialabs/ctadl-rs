# Default propagation models per language - DO-NOT-MERGE

Scope: **propagation models only** — the `model.propagation` half, which becomes function
summaries at `index` time. Sources and sinks are out of scope (see §7).

---

## Status — implemented and verified, 2026-07-28

Every phase is implemented, on branch `default-propagation-models`, as seven commits on top of
`500ad79` (the merge of `66a24fd` into this branch). The plan below is left as written except
where a section is annotated; the per-phase deviations are collected under "Where the plan was
wrong" and argued in full in the commit messages.

What ships now, per VMT variant, is `models/defaults/java-index.jsonl` (84 generators),
`native-index.jsonl` (29) and `lua-index.jsonl` (12) — against 55 Java and 14 C generators
loaded together for every import before, three of the C ones endpoint-only and inert.

| Commit | Phase | What landed |
| --- | --- | --- |
| `d4c8906` | 2a | `VirtualMethodTable::Lua` gains an `externals` column, filled by the call lowering (so `build_vmt` now runs *after* lowering) and indexed under both the fq callee text and its last dotted component. F3 is fixed: a Lua model file is no longer inert. |
| `7891931` | 1 | Dispatch on the VMT; files relocated to `models/defaults/{java,native,lua}-index.jsonl` and `languages/jadx/` deleted (F5); dead `sources`/`sinks` stripped from the native file (1d); `--no-default-models` on `index` and `go` (1c); `lua-index.jsonl` shipped (2b); `.jsonl` loading learns blank/`//` lines so a model file can carry its own reasoning. |
| `47474b9` | 0a | The semantics matrix, each row probing at the level its port predicts: six flowy fixtures (`tests/tnt/port_*.tnt`, and `flowy::check` gains a `models` parameter) plus the escaped-bracket shapes flowy cannot spell, in Lua (`tests/port_semantics.rs`). `docs/model-generators.md` gains the prefix-substitution section §0a asked for. |
| `c9a5e10` | 3 | `native-index.jsonl`: symbol aliases (3a) and 17 new generator groups (3b); `realloc`/`reallocarray` split out of the allocator group and the `qsort` self-loop dropped (3c). Landed alone, and measured. |
| `6aee7e8` | 4a | The four Java collection generators move off the synthetic `.rep` onto `.\[]`, the element the frontends actually emit. |
| `bc54014` | 4c | 31 new Java generators — the `java.util` gap — plus two guards: that a collection port still decodes to `Symbol("[]")` and names no `.rep`, and that no shipped port is *over*-escaped. |
| `fcc60a7` | verify | One nightly case per runnable frontend that supplies **no propagation model of its own**, so the shipped default file is the only thing that can make the flow exist. Found and fixed a real defect in 3a's alias list. |

### Test results

Run in this checkout at `fcc60a7` (working tree clean but for this file):

- `cargo test --workspace` — **448 passed, 0 failed, 5 ignored**. The five ignored are
  pre-existing `#[ignore]`d tree-sitter C cases (aspirational/WIP), untouched by this work.
  New coverage inside that number: `tests/default_models.rs` (9 cases — one per VMT arm, the
  parse-every-shipped-file drift guard, the two escaping guards, the no-endpoints guard, the
  comment-skipping guard), `tests/port_semantics.rs` (6), and the six `tnt/port_*.tnt`
  fixtures the flowy harness walks.
- `nix develop .#regression -c cargo xtask regression` — **94 passed, 0 skipped, 0 failed,
  0 xfail of 94**.
- `nix build .#checks.${system}.regression`, the sealed check — the same 94/94.

Both regression runs cover **all four frontends**, which is what the commit messages could not
claim: dex needs the Android toolchain and pcode needs Ghidra, neither of which is on the
developer machine outside Nix. So `6aee7e8`/`bc54014` shipped with the dex half unrun and
`c9a5e10` with the pcode half unrun. Under Nix both run and both pass — including the negative
cases (`Reassignment`, `Lua:negative-no-flow`) that a too-broad container model would break, and
the three cases that exist to test the default files, which report as four entries because the
Java one is discovered by both Java frontends:

| case | frontend | what goes quiet if the defaults break |
| --- | --- | --- |
| `DefaultModelsFlow` | dex | `StringBuilder.append`; `List.add`/`get` and `Map.put`/`get` at the element level — the end-to-end check on `.\[]`, on the frontend that was unrun until now |
| `Jvm:DefaultModelsFlow` | jvm | the same three flows, all three sink lines required |
| `defaultmodels` | pcode | `strcpy` → `strcat` → `strdup` on `.deref` ports — **the pcode column §0a and §3 both left unverified is now measured**, on real Ghidra |
| `Lua:default-models-flow` | lua | `io.read` → `string.format` → `os.execute`: the F3 externals fix and `lua-index.jsonl` together |

### Where the plan was wrong

Six corrections, all of them made in the implementation:

1. **The phase-4 container table does not compose.** It paired bare reads (`A(0) → Return`)
   with element-level writes (`A(1) → A(0).\[]`). Under prefix substitution the write puts taint
   one level below where the read looks, so a `map.put`/`map.get` pair derives *nothing*.
   Shipped element-level on both halves (`bc54014`), which is also the more precise choice — it
   distinguishes "the container is tainted" from "an element is".
2. **Four phase-3 argument indices do not survive contact with the real signature.**
   `strlcpy`/`strlcat` lose the `A(2).deref` half (argument 2 is the size); `readlinkat` and
   `inet_pton` each sit one argument to the right of `readlink`/`inet_aton` and get their own
   entries; `mbsrtowcs` takes a `char **`, so its input is `A(1).deref.deref`.
3. **The 3a alias spelling was backwards.** The plan said to add the leading-underscore forms.
   The pcode frontend derives the VMT simple name with `trim_start_matches('_')`, so
   `__memcpy_chk` and `___strcpy_chk` reach the matcher as `memcpy_chk`/`strcpy_chk` — an alias
   spelled *with* its underscores matches nothing, and the Mach-O `_`-prefixed forms are pure
   noise. Every alias is listed underscore-stripped. The pcode nightly case is what caught this,
   which is the argument for having written it.
4. **`setmetatable` belongs in the Lua "deliberately absent" list**, not the coercion group: the
   frontend lowers it directly (`eval_setmetatable`), so it never appears as a call target and a
   model on it is inert.
5. **Over-escaping is a silent failure mode.** `.\[]` needs two levels of quoting in a `.jsonl`
   file and is written `"\\[]"`; `"\\\\[]"` gets past both the JSON parser *and* the path parser
   and yields a `Symbol` named `\[]`, which matches nothing. A file-generating script makes that
   mistake easily — this one did. `no_shipped_default_port_is_over_escaped` is the guard.
6. **Two facts the 0a fixtures forced out, now in the docs.** A model **sink** port materializes
   over the paths reachable at its vertex, so a sink written `Return` also seeds `Return.f` and
   no bare-port sink can say "the object but not its fields". And a query that finds nothing
   still emits a `C0001.tainted-path` result, as `kind: "open"` — presence of the rule is not a
   flow, `kind: "fail"` is. Any future fixture that asserts on SARIF has to check `kind`.

### Deliberately not done

- **Sources and sinks** — out of scope by request (§7), unchanged.
- **Wiring `-l c`** (F4) and the PHP file — out of scope (§7), unchanged.
- **The level-correct Lua element ports** (`table.concat` reading `A(0).\[_elem_]`, `rawset`
  writing it, an unwrapping `table.remove`). Ungated since #85 but still unmeasured; the shipped
  file uses the strictly more permissive bare ports and its header records the held variants so
  they are not lost.
- **A pcode arm in `tests/port_semantics.rs`.** It would need Ghidra in a unit test. The pcode
  spellings are pinned by the flowy fixtures (`.deref`, `.[<numeric>]`) and exercised end to end
  by the `defaultmodels` nightly case, which is where Ghidra already is.
- **`java.util` beyond the table**, and any second measured pass on the firmware corpus for
  phase 4 — the Java expansion has no equivalent of §6's blowup risk, since the Java corpora in
  the suite are small.

---

## Context

*Everything from here down describes the state of the tree at `500ad79`, before this branch, and
is left as written; the annotations say what each finding turned into.*

CTADL ships two built-in model files and loads **both, unconditionally, for every import**:

```rust
// models/mod.rs:44-60
pub fn try_load_default_models(program_info: &ProgramInfo) -> Result<ModelsBatch, Error> {
    let jadx_default  = include_bytes!("../languages/jadx/default-index.jsonl");   // 55 generators
    let pcode_default = include_bytes!("../languages/pcode/default-index.jsonl");  // 14 generators
    jadx_models.union_with(&pcode_models)?;
```

Called from `cli::index` (`cli/mod.rs:73`) and nowhere else. `cli::index` then uses only
`models_batch.summary` (`cli/mod.rs:100`) — the `endpoint` half is dropped, so the
`sources`/`sinks` entries in the pcode file are parsed, validated, and discarded on every
index. Propagation is the only half of a default model that does anything today.

**Baseline and what has since landed.** Findings below marked **measured** were run against
`target/release/ctadl` built at `0829891` ("Experimental Lua support (#82)"). Everything the
access-path work changed is now on `main` as **`66a24fd`, "Canonicalize access path encoding
(#85)"** — one grammar for access paths across the fact store, model ports, the IR `Display`
impls, flowy and the test DSLs. `access-path-parsing-plan.md` is the design document for it.
That merge closed the two open questions this plan had (§0b and §0c) and changed how three port
spellings parse, so measurements taken at `0829891` are annotated where their spelling no longer
means what it did.

### F1. Defaults are not language-aware — FIXED in `7891931`

A Lua import matches all 55 Java generators and all 14 C ones; a flowy import matches all 69.
A full match pass per import, contributing nothing.

### F2. Port paths are a level shift, not a filter — settled

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

| model on `id` | effective summary | taint lands at | measured @ `0829891` | expected |
| --- | --- | --- | --- | --- |
| *(no model — control)* | — | — | 0 | 0 |
| `Argument(0)` → `Return` | `u.X ← t.X` | `u.f` | 1 | 1 |
| `Argument(0).f` → `Return` | `u.X ← t.f.X` | `u` | 0 | 0 |
| `Argument(0)` → `Return.f` | `u.f.X ← t.X` | `u.f.f` | 0 | 0 |
| `Argument(0).[12]` → `Return` | `u.X ← t.[12].X` | *(nothing at `t.[12]`)* | 0 | 0 |
| `Argument(0).[_elem_]` → `Return` | `u.X ← t.[_elem_].X` | *(nothing there)* | **1** | 0 |
| `Argument(0).[zzz]` → `Return` | `u.X ← t.[zzz].X` | *(nothing there)* | **1** | 0 |

> **Do not copy the port strings in the last three rows.** They are the spelling as measured at
> `0829891`. Under the canonical grammar (#85), `.[_elem_]` and `.[zzz]` are **load errors** — an
> unescaped `[` at segment start introduces an offset — and must be written `.\[_elem_]` /
> `.\[zzz]`. `Argument(0).[12]` now means a real `Offset(12)` rather than `Symbol("[12]")`.
> The two rows measuring 1 where 0 was predicted were the fact store's on-disk path encoding, not
> the model layer; #85 fixed it, and the escaped spellings now score 0 as predicted.

**The semantics.** A propagation `In → Out` becomes `assign_like(out_var, out_path, in_var,
in_path)` (`codegen/models.rs:96-98`, consumed at `index_engine/mod.rs:1096-1103`). The two
forward field-propagation rules (`index_engine/mod.rs:1063-1072`) fire through
`Path::substitute_prefix` (`facts.rs:216`): taint at `in_var.p`, for any `p` extending
`in_path`, lands at `out_path` followed by the remaining suffix. A port pair is therefore a
**prefix substitution — a level shift, not a filter**:

- `A(0) → Return` maps `t.X` to `u.X` for every `X`. `t.f` reaches `u.f`, the sink fires.
- `A(0).f → Return` maps `t.f.X` to `u.X`. It *unwraps* a level: `t.f` lands on bare `u`, and
  the sink reads `u.f` — one level below where the taint now sits. **0 is the model doing
  exactly what it says**, not a defect.
- `A(0) → Return.f` maps `t.X` to `u.f.X`, so `t.f` lands at `u.f.f`. 0, again correctly.

The fixture only ever probes `u.f`, which is why four of six rows read as failures on a first
pass. They are not. Any test of port semantics must probe at the level the port *predicts*
(§0a).

**The element field is modelable on every frontend.** A bracketed name is an ordinary
`PathSegment::Symbol` whose *name* contains brackets, and the frontends spell it:

- lua: `PathSegment::symbol("[_elem_]")` (`lua/mod.rs:1921,1948,1953,2191,2208`)
- tree-sitter C: `"[_elem_]"` (`tree_sitter/mod.rs:1881`)
- dex: `FieldPath::symbol("[]")` (`dex/mod.rs:751,774`)
- jvm: `mir::FieldPath::symbol("[]")` (`jvm/mod.rs:1046,1070`)

A model port matches these exactly, **provided the bracket is escaped**: `.\[_elem_]`, `.\[]`
(in JSON, `"\\[_elem_]"` / `"\\[]"`). Pinned by
`models_loading.rs::escaped_bracketed_port_stays_a_symbol`.

**Ports can name offsets.** pcode is the only frontend that emits `PathSegment::Offset`
(`pcode/mod.rs:121`). Before #85 every model-port segment became a `Symbol`, so a port `.[12]`
was `Symbol("[12]")` and could never match the `Offset(12)` pcode produced. Ports now parse with
the canonical grammar, so `.[12]` is a real offset. Pinned by
`models_loading.rs::offset_port_produces_a_real_offset`.

Note the consequence for lua and tree-sitter C: a source-level `t[12]` is the *symbol* `[12]`
there, so a port aimed at it is `.\[12]`, while the same spelling unescaped means offset 12 and
matches only pcode. The two used to be indistinguishable.

Remaining consequences for this plan:

- **The four collection generators in the Java defaults are the standard synthetic-container
  idiom, not an accident.** `jadx/default-index.jsonl:52-55` use `.rep`, which no frontend
  emits; `List.add` writes it and `List.get` reads it, so add→get chains compose. That is by
  design. The real cost is that they never join a *real* array, which is written and read as
  `[]` — and `[]` is writable from a port and now survives the index→query round trip, so
  `.rep` → `\[]` is a live, ungated option (§4a).
- pcode's entire default file is built on `.deref`, a plain symbol, so it behaves as written —
  **now measured**, by `nightly/tests/c/defaultmodels.c` under the Nix regression environment,
  which has real Ghidra: `strcpy` → `strcat` → `strdup`, every step a `.deref` port on a
  bodyless libc function, with no propagation model of its own.

### F3. Lua externals cannot be modeled at all — measured; FIXED in `d4c8906`

The IR names them correctly:

```
%%t2, %%t3 = direct-call string.format(<const: "\"echo %s\"">, %x) [9]
%%t4, %%t5 = direct-call os.execute(%cmd) [10]
```

but `VirtualMethodTable::Lua.functions` (`ctadl-ir .../call.rs:185-197`, built at
`lua/mod.rs:987-996`) holds *only functions the frontend lowered*, and
`ModelGeneratorIngest::new` builds every match index from it (`models/json.rs:285-313`). A
model naming `execute`, and a second naming `qualified-id: "os.execute"`, both match nothing:

```
$ ctadl query luaproj -m q.json -o out.sarif
Matched 1 sources and 0 sinks        # the 1 is a *defined* Lua function
```

dex/jvm push referenced-only methods into the VMT from `Context::ext` (`dex/mod.rs:412,432`,
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

`-l c` is in the CLI enum (`main.rs:169`) but `cli::import` panics on it. The tree-sitter C
frontend is reachable only from its own tests and builds no VMT, so `ModelGeneratorIngest`
would fall to the `Unknown` arm (`json.rs:314-330`). **C means pcode** — which is also how
`nightly/tests/c/*` runs.

### F5. `jadx/` is not a frontend — FIXED in `7891931`

`languages/mod.rs` declares `dex`, `jvm`, `lua`, `pcode`, `tree_sitter`. `languages/jadx/`
contains one file, the model file, and no `mod.rs`. The directory is gone; the file lives at
`models/defaults/java-index.jsonl`.

### F6. Two more spellings in the Java defaults — DONE in #85

A trailing `.*` used to parse to the **empty** path, so `Argument(0).*` was a synonym for
`Argument(0)` — harmless, but it read as a wildcard and was used as one. `Argument(0).[*]` was
*not* the same case: it parsed to `Symbol("[*]")`, a literal field name no frontend emits, so
that propagation matched nothing.

Both are gone, in #85, because the canonical grammar makes `.[*]` a hard load error and `.*` a
literal field named `*` — leaving them would have changed their meaning silently. What changed,
exactly (`jadx/default-index.jsonl`):

- **Line 22** (`<init>` on the reader/stream classes) carried four propagations, three of them
  affected: `Argument(0).[*] → Return` was **dropped** (it matched nothing);
  `Argument(0) → Return.*` had its suffix stripped to `Argument(0) → Return`; and
  `Argument(0).* → Return.*` collapsed onto that same rule and was dropped as a duplicate. Two
  propagations remain.
- **Lines 37 and 38** (`addFlags`/`setFlags`/`parseIntent`/`parseUri` on `Intent`; `asList`/`copyOf`
  on `Arrays`): `Argument(2) → Argument(0).*` became `Argument(2) → Argument(0)`.

The `.*` entries were therefore **rewritten, not deleted** — since `.*` denoted the empty path,
dropping the whole propagation would have silently removed a live rule; only the suffix came off.
No `.*` or `.[*]` port remains anywhere in the tree; the shipped files now use exactly
`Argument(*)`, `Argument(*).deref`, `Argument(0)`, `Argument(0).deref`, `Argument(0).rep`,
`Argument(1)`, `Argument(1).deref`, `Argument(2)`, `Return`, `Return.deref`, `Return.rep`.

---

## Phases

| Phase | What | Gates | Landed |
| --- | --- | --- | --- |
| 0 | Pin port path semantics as a test (0a) | none — 0a is the only open item | `47474b9` |
| 1 | Dispatch defaults on the VMT; relocate the files | — | `7891931` |
| 2 | Lua VMT gains externals; ship `lua-index.jsonl` | needs 1 | `d4c8906`, `7891931` |
| 3 | Expand `native-index.jsonl` (C via pcode) | **measure per §6** | `c9a5e10` (measured) |
| 4 | Expand `java-index.jsonl` | — | `6aee7e8`, `bc54014` |

---

## Phase 0 — pin port path semantics

**Settled by #85, nothing here is blocking.** The two anomalous F2 rows were the fact store's
on-disk path encoding, and the "a port cannot name an offset" gap is implemented, not merely
diagnosed. Details in `access-path-parsing-plan.md`; the short version:

- The drop was in the old parquet decoder for *every* `Path` column, which `index` writes and
  `query` reads back in a separate process. `to_dot_string` escaped `.` but not `[`, and the
  reader treated a bare `[` as an offset and silently dropped a segment whose bracket contents
  were not a valid `i64`. A port `Argument(0).[_elem_]` was stored correctly as
  `Symbol("[_elem_]")`, written as `.[_elem_]`, and read back as **the empty path** — i.e. as
  the bare port, which is exactly the score of 1 measured where 0 was predicted. Same for
  `.[zzz]`. Both sides now speak the canonical grammar and `facts/parquet.rs:538` is a panic
  backstop behind an `INDEX_FORMAT_VERSION` gate. Pinned by
  `facts.rs::test_path_round_trip_corpus`.
- Model ports parse with the same grammar, so `.[12]` produces `PathSegment::Offset(12)` and a
  field name beginning with `[` is written `\[` and stays a `Symbol`. A malformed path is a hard
  `JsonModelError::InvalidAccessPath` naming the port and the fix. Pinned by
  `models_loading.rs::{offset_port_produces_a_real_offset,escaped_bracketed_port_stays_a_symbol}`
  and the `json_error_handling.rs` malformed-path cases.

The **element-field decision is therefore taken and ungated**: phase 4 can move `.rep` → `\[]`
and phase 2 can model Lua containers. This could not have been decided correctly earlier —
before the fix a `.rep` → `[]` change would have looked like it worked while `[]` silently
collapsed to the empty path, making the generator match everything rather than array elements.

**0a. Pin the semantics matrix as a test — DONE in `47474b9`.** Every shape below is pinned,
each probing at the level its port *predicts* under prefix substitution rather than at one fixed
depth — the F2 matrix scored 0 on three rows purely because it always probed `u.f`. The split is
flowy for everything it can spell (`ctadl_flowy` needs no toolchain, gets no defaults of its own,
and its `where summaries [...]` clause asserts on the index summary relation directly; `flowy::check`
gained a `models` parameter and the harness now loads a sibling `<stem>.models.jsonl`), and Lua for
the escaped-bracket shapes it cannot — flowy's identifier production is `[A-Za-z_][A-Za-z0-9_]*`,
so it can never write a field name beginning with `[`, which is exactly what lua/dex/jvm emit for
a container element.

| shape | example | meaning | pinned by |
| --- | --- | --- | --- |
| bare | `Argument(0)` | the whole value | `tnt/port_bare.tnt`, `port_semantics.rs::a_bare_port_preserves_the_suffix` |
| `.symbol` | `.f` | a named field | `tnt/port_in_symbol.tnt`, `tnt/port_out_symbol.tnt`, `port_semantics.rs::{an_input_field_port_unwraps_a_level,an_output_field_port_adds_a_level}` |
| `.[numeric]` | `.[12]` | a real `Offset(12)` — matches pcode | `tnt/port_in_offset.tnt`; the negative half in `port_semantics.rs::an_unescaped_numeric_port_is_an_offset_and_lua_has_none` |
| `.\[symbol]` | `.\[_elem_]`, `.\[]` | a `Symbol` whose *name* has brackets — matches lua/dex/jvm | `port_semantics.rs::escaped_element_port_matches_what_lua_emits`; `.\[]` end to end in `nightly/tests/java/DefaultModelsFlow.java` |
| `.[symbol]` | `.[_elem_]` | **hard load error**; assert the error, not a score | `json_error_handling.rs` (needs no program) |
| `.deref` | `.deref` | pcode's pointee | `tnt/port_in_deref.tnt`; on real pcode by `nightly/tests/c/defaultmodels.c` |
| two-segment | `.[8].deref` | the mixed case that motivated the grammar work | `tnt/port_two_segment.tnt` |

The `.[numeric]` / `.\[symbol]` split is the substance: those two were the *same* port before,
and on lua and tree-sitter C the frontends emit the escaped one while pcode emits the offset.
The existing tests (`facts.rs::test_path_round_trip_corpus`,
`models_loading.rs::offset_port_produces_a_real_offset`) pin the *encoding*; 0a is what pins the
*taint semantics* end to end, which nothing did before.

No pcode arm in `port_semantics.rs`: it would put Ghidra in a unit test. The pcode column is
covered instead by the flowy fixtures for the encodings and by the `defaultmodels` nightly case
for the semantics, which passes under Nix — so **the `.deref` column is no longer unverified**.

**Docs deliverable: DONE.** `docs/model-generators.md` §6 carries the access-path grammar,
states that `.[n]` is an offset and really produces one, that a field name beginning with `[` is
written `\[`, that `.*` and `.[*]` are not wildcards, and gives the per-frontend table of which
spelling matches what. The prefix-substitution statement that was still missing — the
`A(0).f → Return` unwrapping example, the part that misreads — landed with the fixtures in
`47474b9`, under §7 *Propagation*, together with the sink-materialization asymmetry the fixtures
turned up.

---

## Phase 1 — dispatch on the VMT, relocate the files

**DONE in `7891931`, as written** — 1a through 1e, no deviations. One addition the plan did not
call for: `.jsonl` loading now skips blank lines and lines beginning `//`, without consuming a
generator index, so a default file can carry the reasoning that would otherwise drift away into a
separate document. All three shipped files use it.

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

Delete `languages/jadx/`. Update the two `include_bytes!` and `docs/model-generators.md:51-52`.
`jadx` names a decompiler that is not a frontend here, `pcode` is one of two possible C paths,
and these are model *data*, not language *code*. Naming them for the VMT variant that selects
them makes 1a's table the whole story. Keep the `-index` suffix: it names the stage they load
at, and leaves room if query-time defaults are ever added.

### 1c. `--no-default-models`

Add to `IndexArgs` (`main.rs:198`) and `GoArgs` (`main.rs:273`), threaded to `cli::index`.
Needed for the A side of the phase-3 measurement, and for anyone who wants a model file to be
the complete story.

### 1d. Strip the dead endpoints

Remove the `sources`/`sinks` entries from the pcode file as part of the move (5 lines carry
them). They have never had an effect (`cli::index` uses `.summary` only) and their presence
implies CTADL ships default sources and sinks, which it does not.

### 1e. Tests

- One unit test per VMT arm: a Lua `ProgramInfo` loads zero Java generators and vice versa.
  Assert on `ModelsBatch::summary.num_rows()`.
- A test that walks every file in `defaults/` and asserts zero `JsonModelError`s. The loader
  hard-errors on unknown fields (`docs/model-generators.md:116-136`) and now on malformed access
  paths, so a stale default file breaks *every* index — this is the cheap guard against drift.

Both landed in `ctadl-ascent/tests/default_models.rs`, which grew to nine cases as later phases
added guards: the four VMT arms, `every_shipped_default_file_parses`, the two escaping guards
(`java_collection_generators_name_the_real_array_element`,
`no_shipped_default_port_is_over_escaped`), `no_shipped_default_declares_an_endpoint` for 1d, and
`jsonl_comments_are_skipped_without_consuming_an_index`.

---

## Phase 2 — Lua externals, then Lua propagation models

**DONE — 2a in `d4c8906`, 2b in `7891931`.** Two departures from the text below. `build_vmt` now
runs *after* lowering rather than before, because only the call lowering knows what was called;
nothing in lowering reads the VMT, and every other column comes from the collection/recognition
passes, which are complete either way. And `setmetatable` moved out of the coercion group into
the deliberately-absent list: the frontend lowers it directly (`eval_setmetatable`), so it never
appears as a call target and a model on it would be inert. The `Verification (must flip)` gate
below holds — `nightly/tests/lua/default-models-flow.lua` is that repro, kept as a regression
case rather than as a one-off measurement.

### 2a. The frontend change (prerequisite, per F3)

Add an externals column to `VirtualMethodTable::Lua` (`ctadl-ir/src/mir/call.rs:179-201`):

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
`string.format(x)` and `s:format(x)`. Note that for *defined* functions the frontend carries the
simple name in the VMT rather than re-deriving it from the fq name (see the `functions` doc
comment and `json.rs:292-303`); an external has no definition site, so splitting the fq name is
the only source available and that difference is worth a comment.

Index them in `ModelGeneratorIngest::new`'s Lua arm exactly as `functions` is indexed today
(`json.rs:285-313`): simple *and* fq into `program_method_names`/`program_method_signatures`, fq
**only** into `program_method_qualified_ids` — the comment there explains why keying
`qualified-id` on bare names reintroduces the collisions it exists to remove. Add them to
`universe` so a top-level `not` sees them.

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

**Every entry below uses bare ports** — not because paths are broken (`[_elem_]` is writable,
matches the Lua frontend exactly, and survives the index→query round trip as of #85) but because
a bare port is the strictly more permissive choice and the level-shifting entries deserve their
own measured pass. They are listed separately below so they are not forgotten.

Each `names` list covers the bare form, so `string.sub(s,…)` and `s:sub(…)` both hit it.

| Group | Names | Propagation |
| --- | --- | --- |
| string producers | `format`, `rep`, `sub`, `upper`, `lower`, `reverse`, `char` | `Argument(*)` → `Return` |
| string transforms | `gsub`, `match`, `gmatch`, `byte`, `len` | `Argument(0)` → `Return` |
| table read | `table.concat`, `table.remove`, `table.unpack`, `unpack` | `Argument(0)` → `Return` |
| raw access | `rawget`, `next` | `Argument(0)` → `Return` |
| raw access | `rawset` | `Argument(2)` → `Argument(0)` |
| coercion | `tostring`, `tonumber`, `assert` (`setmetatable` moved to *deliberately absent*) | `Argument(*)` → `Return` |
| os | `os.date`, `os.getenv`, `os.tmpname` | `Argument(*)` → `Return` |
| io | `io.open`, `io.read`, `io.lines`, bare `read` | `Argument(*)` → `Return` |
| io | bare `write` | `Argument(1)` → `Argument(0)` |
| json | `cjson.encode`, `cjson.decode`, `cjson.safe.encode`, `cjson.safe.decode` | `Argument(0)` → `Return` |
| openresty codecs | `ngx.escape_uri`, `ngx.unescape_uri`, `ngx.encode_base64`, `ngx.decode_base64`, `ngx.encode_args`, `ngx.decode_args`, `ngx.quote_sql_str`, `ngx.md5`, `ngx.sha1_bin` | `Argument(0)` → `Return` |
| openresty regex | `ngx.re.match`, `ngx.re.gsub`, `ngx.re.sub` | `Argument(0)` → `Return` |

*Level-correct forms — ungated, still unmeasured.* **Spell the Lua element field
`.\[_elem_]`, escaped**, and in JSON `"\\[_elem_]"`; a bare `.[_elem_]` is a hard load error,
since `[` at segment start means an offset. The entries: `table.concat` reading
`A(0).\[_elem_]` rather than `A(0)`; `rawset` writing `A(0).\[_elem_]`; a
`table.remove`/`unpack` that unwraps one level (`A(0).\[_elem_] → Return`, which puts an
element's taint on the returned value rather than at `Return.\[_elem_]`). The bare-port versions
above are strictly more permissive, which is the safe direction for a default, but they lose the
distinction between "the table is tainted" and "an element is".

**Deliberately absent — do not add:** `table.insert`, `ipairs`, `pairs`, `select`, and
`setmetatable`. The frontend recognizes these syntactically and lowers them straight to dataflow
(`lua/mod.rs:2180-2208`, `eval_setmetatable`); a model would double-count on the first four and
be inert on `setmetatable`, which never appears as a call target. The comment saying so is at the
top of the shipped file, because "the stdlib list is missing `table.insert`" reads as an
oversight.

**Gaps to record, not fix:** `pcall`/`xpcall`/`coroutine.*` invoke a function-valued argument
and need `forward_call`, unimplemented (`docs/model-generators.md:424-431`).

---

## Phase 3 — expand `native-index.jsonl` (C via pcode)

**DONE in `c9a5e10`, landed alone and measured (§6).** Four rows of the 3b table below ship with
different argument indices — `strlcpy`/`strlcat` lose the `A(2).deref` half because argument 2 is
the size; `readlinkat` and `inet_pton` get their own entries one argument to the right of
`readlink`/`inet_aton`; `mbsrtowcs` takes a `char **` and so reads `A(1).deref.deref`. 3a shipped
with the alias spellings *inverted* from what is written below — see the note there. `printf_chk`
and `isoc99_fscanf` are deliberately absent: they would need `printf` and `fscanf` generators,
and both of those were endpoint-only and went out with 1d.

Keep the existing 14 generators; drop their `sources`/`sinks` (§1d). `.deref` is a plain symbol
and so behaves as written; the pcode column is confirmed by the `defaultmodels` nightly case
rather than by a 0a unit fixture, since that is where Ghidra already is. **`.[<numeric>]` is now a real `Offset` and matches what pcode emits**, so offset
entries are writable here. (On lua and tree-sitter C the same spelling also means an offset,
which is *not* what those frontends emit — there the element field must be escaped,
`.\[_elem_]`.)

### 3a. Highest-value addition: symbol aliases

The firmware corpus (glibc/uClibc ARM binaries from `karonte`, described in
`.scratch/bench/RESULTS.md` — untracked, see §6) frequently imports a symbol that is not the
plain libc name: `__memcpy_chk`, `__strcpy_chk`, `__sprintf_chk`, `__snprintf_chk`,
`__strcat_chk`, `__vsnprintf_chk`, `__printf_chk`, `__isoc99_sscanf`, `__isoc99_fscanf`. The
pcode frontend strips Ghidra's `<EXTERNAL>::…@addr` decoration but not the symbol itself. The
`_chk` variants take extra size/flag arguments, but every model that would cover them is
positional at index 0/1 or uses `Argument(*)`, so **the fix is to extend the existing `names`
lists** — no new generators, no index arithmetic. Add the leading-underscore forms (`_strcpy`,
`_memcpy`, …) the same way the Python CTADL defaults do for decorated Mach-O symbols.

Cheapest coverage win in the plan, and aimed squarely at the corpus the benchmarks measure.

> **The last sentence is wrong, and shipping it that way matched nothing.** The pcode frontend
> derives a function's VMT simple name with `func_data.name.trim_start_matches('_')`
> (`languages/pcode/mod.rs`), so `__memcpy_chk`, `___strcpy_chk` and `_strdup` reach the matcher
> as `memcpy_chk`, `strcpy_chk` and `strdup`. An alias spelled *with* its underscores matches
> neither the simple name (already stripped) nor the fully-qualified one (still carrying
> `<EXTERNAL>::…@addr`). Every alias is listed underscore-stripped, and the leading-underscore
> Mach-O forms are gone entirely — the frontend has always handled those. `fcc60a7` found this,
> via `nightly/tests/c/defaultmodels.c`: on a fortifying host (macOS/clang fortifies even at
> `-O0`) its `strcpy` arrives as `___strcpy_chk`, and the case passes only because `strcpy_chk`
> is in the list.

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

**DONE — 4a in `6aee7e8`, 4b/4c in `bc54014`.** The one substantive change from the 4c table:
**containers ship element-level on both halves**, not bare reads against element-level writes.
The table below pairs `A(0) → Return` reads with `A(1) → A(0).\[]` writes, and under prefix
substitution those do not compose — the write puts taint one level below where the read looks, so
a `map.put`/`map.get` pair derives nothing. Since 4a had just moved the collection generators onto
`\[]`, element-level is the consistent half to keep, and it is the more precise one. Writers carry
both `Argument(1)` and `Argument(2)` because these methods are overloaded on arity (`add(E)` and
`add(int, E)`, `put(K, V)`, `set(int, E)`) and a generator matches by name, so it sees every
overload; `Argument(*)` would cover them but would also add a self-edge `A(0) → A(0).\[]` that
deepens a path every time it fires. The old `List.add`/`List.get` pair is dropped — the new
collection groups cover both with a wider parent list — while `iterator` and `Iterator.next` stay,
since nothing else covers them.

### 4a. `.rep` → `\[]` is a live option, ungated

An earlier draft ruled this out on the grounds that `[]` could not survive the port parser. It
can, and as of #85 it also survives the index→query round trip, which is the part that was
actually broken. dex and jvm both write `FieldPath::symbol("[]")`. So the four collection
generators at `jadx/default-index.jsonl:52-55` can be moved off the synthetic `.rep` and onto
the field the frontends actually emit, which makes `List.add`/`List.get` chains join real array
writes instead of only each other.

> **Spell it `.\[]`, not `.[]`.** Under the canonical grammar an unescaped `[` at segment start
> is an *offset*, so a port written `Argument(0).[]` is a hard load error (`InvalidOffset("")`).
> The Java array element is a `Symbol` whose name is `[]`, written `Argument(0).\[]` — and in
> JSON, `"Argument(0).\\[]"`. The error message names the fix, so getting this wrong fails
> loudly rather than silently matching nothing.

Do it as its own commit with the 0a matrix test green, and check the `unexpected_lines` cases in
`nightly/tests/java` — `[]` is a real field with real writes behind it, so this widens what the
generators reach, which is the point and also the risk.

> Done as its own commit. **Nothing in `nightly/tests/java` uses `unexpected_lines`**, so that
> check could not be run as specified; the suite is weaker against widening than against breaking.
> What stands in for it: the `Reassignment` negative case (dex and jvm), `Lua:negative-no-flow`,
> and the fact that all 94 regression cases — dex included, under Nix — still pass.

### 4b. The rule for `parents`

`parent`/`parents` matches the class **declared at the invoke site**, not the runtime type.
That is why the existing `iterator` generator lists eight parents. Every generator below must
list the interface *and* the common concrete types. State this at the top of the file.

Also note: dex and jvm name instance fields differently — dex wraps the field's `pretty_name`
(`dex/mod.rs:706,724`), jvm builds `<Lclass;->field:descriptor>` (`jvm/mod.rs:630-633`) — so
default models must stay positional. Named-field ports are not portable between the two Java
frontends.

### 4c. New generators

The gap is `java.util`. All bare-port except where a level change is genuinely required, and
those are marked *(held)* — ungated, but still unmeasured. Any element-field port among them is
written `.\[]` on dex/jvm, escaped.

> **What shipped differs on the container rows.** The *(held)* writes went in as written, and the
> element *reads* paired with them (`Map read`, `Collection read`) went in at the element level
> too — `A(0).\[] → Return`, not the bare `A(0) → Return` this table shows — because a bare read
> against an element-level write composes to nothing. Writers carry `Argument(1)` *and*
> `Argument(2)`, since these methods are overloaded on arity and a generator matches by name.
> `Map views` and `Collection bulk` stayed bare on purpose: a view is a collection over the *same*
> elements, so the element level rides along. Every other row shipped as written. See the phase-4
> note above and `bc54014`.

| Group | Parents | Members | Propagation |
| --- | --- | --- | --- |
| Map read | `Map`, `HashMap`, `LinkedHashMap`, `TreeMap`, `Hashtable`, `ConcurrentHashMap` | `get`, `getOrDefault`, `remove` | `A(0)` → `Return` |
| Map views | same | `keySet`, `values`, `entrySet` | `A(0)` → `Return` |
| Map write | same | `put`, `putIfAbsent` | *(held)* `A(1)`,`A(2)` → `A(0).\[]` |
| Map.Entry | `Map$Entry` | `getKey`, `getValue` | `A(0)` → `Return` |
| Collection read | `Collection`, `List`, `Set`, `Queue`, `Deque`, `ArrayList`, `LinkedList`, `Vector`, `Stack`, `HashSet`, `TreeSet`, `ArrayDeque` | `get`, `peek`, `poll`, `pop`, `remove`, `getFirst`, `getLast`, `element` | `A(0)` → `Return` |
| Collection bulk | same | `toArray`, `subList`, `stream` | `A(0)` → `Return` |
| Collection write | same | `add`, `addAll`, `push`, `offer`, `addFirst`, `addLast`, `set` | *(held)* `A(1)` → `A(0).\[]` |
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

- Phase 0: the matrix test itself is the deliverable, plus the docs paragraph on prefix
  substitution. Each row asserts where taint lands, not merely whether a fixed probe fires —
  that distinction is what F2's first draft got wrong. Treat a spelling whose effective path
  differs from its written path as a bug to be fixed or rejected, not documented as a quirk.
- Phase 1: the unit tests in 1e; `RUST_LOG=info ctadl index` on a Lua project no longer reports
  Java summary rows.
- Phase 2: the F3 repro must flip to `1 sinks`.
- Phases 2-4: add one `nightly/` case per frontend that passes **with no propagation model of
  its own** — Lua running `io.read` → `string.format` → `os.execute`, a pcode case through
  `strcpy`/`snprintf`, a dex case through `StringBuilder`. Today nothing tests the default files
  at all: every nightly case supplies its own models, which is why port-path semantics were
  never pinned anywhere. `cargo xtask regression [--frontend dex|jvm|lua|pcode]` is the harness.
- Phase 4: re-run `cargo xtask regression --frontend dex`; check the `unexpected_lines` cases
  still hold — that is where a too-broad container model shows up.

**What was actually run, 2026-07-28.** All of the above except the `unexpected_lines` check,
which cannot be run as written because no case in `nightly/tests/java` sets that key (the
`Reassignment` case carries a `// TODO this test case needs "unexpected_lines" 7` and an empty
`expected_lines`, so it is a negative case in the weaker sense of "no flow anywhere").

| check | result |
| --- | --- |
| `cargo test --workspace` | 448 passed, 0 failed, 5 ignored (pre-existing tree-sitter aspirational cases) |
| `nix develop .#regression -c cargo xtask regression` | 94 passed, 0 skipped, 0 failed, 0 xfail |
| `nix build .#checks.aarch64-darwin.regression` (sealed, forced rebuild) | the same 94/94 |
| phase-0 matrix | 6 flowy fixtures + 6 Lua cases, all green |
| phase-1 dispatch | `default_models.rs`, one case per VMT arm, asserting on `summary.num_rows()` rather than on log output |
| phase-2 F3 repro | `Lua:default-models-flow` PASS — the flow exists only with the externals column *and* the shipped Lua file |
| phase-3 A/B | measured on the karonte corpus; see below |
| phase-4 dex | `DefaultModelsFlow`, `ArrayListFlow`, `ArrayListIteratorFlow`, `Reassignment` all PASS on dex under Nix |

The regression numbers are the whole suite — all four frontends, dex and pcode included, which
is only true inside the Nix environment. Outside it, dex has no Android toolchain and pcode no
Ghidra, and that is why every phase-3/4 commit message carefully says which half it could not
run.

**Phase 3 needs a measured A/B, but the blowup risk is smaller than it was.** Every new
propagation generator adds summary edges at every call site of the modeled function, and
`smbd`/`wpa_supplicant` call `strcpy`/`snprintf` thousands of times, so the obligation to
measure stands. What has changed is the baseline: on the firmware corpus, `ath_dev` and
`cfg80211` — which an older `main` could not finish at all, dying past 90 GB — now converge in
seconds at roughly 1 GB (`.scratch/bench/RESULTS.md`, measured 2026-07-24). Nothing in that run
came near the 85 GB guard.

Two caveats on the harness, because this plan used to cite a document that is not here:

- **There is no `BENCHMARKS.md` in this checkout**, and no `.scratch/bench/suite.sh`. The
  `.scratch/bench/` tree was rebuilt from scratch and is **untracked**: `env.sh`, `mkproj.sh`,
  `bench.sh`, `runall.sh`, `collect.sh`, `cmdi.sh`, `gen/gen.py`, and `RESULTS.md`. Reproduce
  with `mkproj.sh <name> <binary>` → `runall.sh` → `collect.sh`. Re-derive any absolute figure
  rather than quoting one.
- The corpus binaries live under `../karonte/firmware/`. `RESULTS.md` records the two corpus
  fingerprints that identify them (`ath_dev` 3,356 formals, `cfg80211` 2,836 formals); check
  those before comparing against any recorded number. `gen_N` is a synthetic re-reconstruction
  and its absolute rows are not continuous with anything older — treat it as a self-consistent
  A/B only.

The harness's answer check is that both sides produce a byte-identical
`relation increase: locals:` line. That check **is expected to trip** for phase 3: adding
library summaries is precisely a semantic change. The obligation is to quantify it, not avoid
it. Land phase 3 as its own commit so the row delta is attributable, and use
`--no-default-models` (1c) for the A side.

**Measured (`c9a5e10`, results in `.scratch/bench/RESULTS.md`).** Three-way A/B — A no models,
B0 the pre-expansion file, B1 the post-expansion file — on the two targets whose corpus
fingerprints identify them plus `ath_dfs`:

| target | A rows | B0 rows | B1 rows | B1−B0 |
| --- | --: | --: | --: | --: |
| `ath_dfs` | 284,790 | 300,523 | 300,523 | **0** |
| `ath_dev` | 1,540,537 | 4,193,466 | 4,193,466 | **0** |
| `cfg80211` | 9,997,688 | 11,165,261 | 11,165,269 | **+8** |

**The expansion is free on this corpus.** Two targets are byte-identical across the change; the
`cfg80211` delta is 8 `locals` rows and 4 formals, all attributable to one name — `strlcpy`,
imported twice. No other new name is imported by any of the three, which are wifi drivers; the
`_chk` and `isoc99_` symbols live in the userspace daemons the aliases were aimed at. Peak
memory does not reproduce well enough to quote a delta (the same side moved up to 20% between
runs), but the absolute magnitudes are stable at ~180 MB / ~660 MB / ~1.1 GB — three orders of
magnitude below the 85 GB guard.

**The pre-existing defaults are the expensive part, and that is not new.** B0 over A is +5.5%
rows on `ath_dfs`, **+172%** on `ath_dev`, +11.7% on `cfg80211` — the price the tool has always
paid for `strcpy`/`memcpy`/`sprintf` summaries at thousands of call sites. Side A is a
measurement of a configuration that did not exist before `--no-default-models`.

---

## 7. Out of scope

- **Sources and sinks.** Out of scope by request. Recording one measured fact for whenever they
  are picked up: source and sink `kind`s do **not** have to pair. An `AAA` source into a `ZZZ`
  sink reports `fail C0001.tainted-path — Taint flow labelled 'AAA'`, contradicting
  `docs/model-generators.md:537-538`. So a default source/sink set would report the cross
  product of every default source and sink in any program, which argues for opt-in bundles
  rather than always-on. CTADL ships none today (§Context).
- **Wiring `-l c`.** The tree-sitter C frontend builds no VMT and `cli::import` panics on it
  (F4). When it lands it gets a `Native` VMT and inherits `native-index.jsonl` free — an
  argument for naming the file after the VMT variant rather than after `pcode`. Note it emits
  `Symbol("[_elem_]")` and `Symbol("[3]")` like lua, not pcode's offsets, so its ports need the
  escaped spelling.
- **PHP.** `ctadl/src/ctadl/models/squiggli_php/default-index.json` is `{}` and there is no PHP
  frontend here.
- **`find: variables` / `find: fields`.** Unimplemented in the loader
  (`docs/model-generators.md:101-107`).
- **The model-level `field` / `fields` keys.** `docs/model-generators.md:281` advertises them;
  `models/json.rs` implements neither.
