# Lua support: real-world test against Kong - DO-NOT-MERGE

Exercising the tree-sitter Lua frontend (branch `lua`) on a production OpenResty
codebase — [Kong](https://github.com/Kong/kong), checked out at **`../kong`**
(i.e. `/Users/dbueno/proj/kong`), commit `391ee48d3`. 605 `.lua` files under
`kong/`; 1309 in the whole repo once `spec/` and `t/` are counted.

Goal: write CTADL sources/sinks for Kong, index it, and see whether connected
source→sink paths are found.

Artifacts produced: `examples/kong/{kong-model.json5, kong-model-vars.json5,
kong-externals.lua, README.md}`.

> **Re-run 2026-07-25**, against `lua` rebased onto main `c63f079` ("Better
> diagnostics", #78) — one main release later than the previous run, and with the
> branch's own CHA / import-fix commits now in the history. Everything below was
> re-measured on this tree. Claims the previous write-up made that this run does
> **not** reproduce have been rewritten or deleted rather than restated; each
> such spot is called out inline.
>
> **Re-run 2026-07-26**, against `lua` on main `4d90113` ("More model validation,
> better error messages, changed to `qualified-id`", #79) — one further main
> release. Every measurement below was taken again on this tree. The great
> majority reproduce *exactly*, several byte-for-byte down to SARIF vertex ids;
> only the four items below moved, and each is marked inline:
>
> 1. **§5.2 `unqualified-id` is fixed** by #79 — follow-up 4's first half is done.
> 2. **§5.2 (new) `qualified-id` matches nothing on the Lua frontend** — the
>    replacement mechanism #79 shipped is unusable here.
> 3. **§5.4 (new) the same mis-rendering happens on the *sink* side**, and across
>    more of the report than the source-side account covers.
> 4. ~~**§5.8 (new) 57 of the 304 flows are 627 steps long.**~~ — **retracted
>    2026-07-26b**: those are 57 *separate* flows per result, not one long one.
>
> Three counters drifted slightly and are restated at their measured values
> (§5.8's global-heap share 227→**235**, the `response-ratelimiting` sink count
> 104→**105**, and the `saturating: false` short-flow split 33/11→**31/13**).
> The first of those was also not drift: 227 and 235 are two different
> measurements of the same tree (see §5.8).
>
> **Re-run 2026-07-26 (second)**, against `lua` at `57a9485`, i.e. the same main
> release `4d90113` plus four branch commits (`Tests`, `qualified-id`, `Process
> Lua functions`, `Match consistently`) written in response to the run above.
> Two of the report's findings are now closed and two of its claims were wrong:
>
> 1. **§5.2.1 `qualified-id` now works on Lua** — follow-up 4 is delivered, and
>    the checked-in model has been rewritten onto it (§5.2.1, §2.4).
> 2. **§5.10 (new) same-named functions within one module are now matchable**,
>    which turns Kong's real sandbox entry point into the code-injection sink the
>    model always meant to name; and §5.11's note that a `where`-less
>    `find: methods` matches only class methods is now wrong — it matches every
>    function.
> 3. **§5.8's "627-step code flows" was a mis-reading** and is retracted. Those
>    57 results carry **57 alternative code flows each**, ~12 steps apiece; no
>    single flow in the report exceeds 16 steps. The 57 turn out to share a
>    byte-identical prefix and differ only in the sink's field path, which makes
>    the SARIF-size half of the finding both real and cheap to fix (§5.8).
> 4. **§5.4's "the acme flow collapsed to one step" does not reproduce** — it
>    renders as two, with the `string.format` step present (§5.4).
>
> 5. **§6.1 (new) `index` is not deterministic**, so half the counters this
>    report has been tracking across four runs cannot detect change at all.
>    `query` is byte-stable given an index; re-indexing the *same unchanged
>    artifacts* and re-querying yields a different SARIF (50.2–54.4 MB across
>    four indexes). Endpoint-level counts — sources, sinks, 304 results, the 77%
>    global-heap share, the plugin sweep — are stable; flow-rendering counts —
>    distinct pairs, §5.4.1's percentages, §5.8's per-result step totals — move
>    by ±2–5 between indexes of identical input.
>
> This was the first re-run to compare two *builds* rather than only re-measure
> one. That is what turned up (5), and (5) in turn **retracts several of this
> run's own attributions**: the small movements in §5.0, §5.4.1 and §5.8 are
> index noise, not consequences of the four commits. What the commits demonstrably
> changed is (1), (2), and a +3 in every sink count. Comparisons that hold the
> index fixed and vary only the model (§2.4, §5.1, §5.2.1) remain exact.

---

## 0. Rebasing onto main

`lua` (6 commits) rebased onto `c63f079` with one conflict, in
`xtask/src/regression.rs`. Both sides edited the same doc comment: main extended
`apply_jvm_allowlist`'s comment to explain that `HardFail` is never demoted,
while the Lua branch had inserted `apply_lua_allowlist` immediately below that
comment — appending its own doc lines to the JVM function's comment block rather
than starting a new one. Resolved by giving each function its own doc comment
and putting `apply_lua_allowlist` first. No semantic conflict.

The `VariableRef::new_local` breakage the *previous* run had to patch by hand
(main's `Locals` intern table landing without the Lua frontend being ported) is
now fixed on the branch itself, in the `update to upstream` commit. `cargo build
--release` is clean.

Regression suite: `cargo run -p xtask --release -- regression --frontend lua` →
**17 passed, 1 xfail** (`Lua:closure-flow`, declared xfail in `LUA_XFAIL`), 0
failed. The §5.1 wildcard repro and the `require` resolution case are now
checked-in cases (`Lua:wildcard-table-arg-flow`, `Lua:require-module-flow`) and
both pass.

**2026-07-26:** the branch now sits on `4d90113` (#79), not `c63f079` — no
rebase conflict this time, and no hand-porting was needed. `cargo build
--release` is clean in 73 s and the regression suite gives the same **17 passed,
1 xfail, 0 failed**. #79 is the release that replaced `unqualified-id` with
`qualified-id`, which is what §5.2 now turns on.

**2026-07-26 (second):** no rebase at all — the branch head is now `57a9485`,
still on `4d90113`, plus four commits of its own. `cargo build --release` is
clean in 89 s. The regression suite gives **18 passed, 1 xfail, 0 failed**: one
more case than last time, `Lua:qualified-id-flow`, which pins the §5.2.1 fix
(a `qualified-id` model that must reach the `lib.reader.read` in one module and
*not* the same-named decoy in another).

Note `IMPORT_FORMAT_VERSION` moved 3 → 4 (the Lua VMT gained a `functions`
column), so stores imported before this change must be re-imported.

---

## 1. Headline result

**Yes — connected paths are found**, including interprocedural and cross-file
ones, through table fields, branches, and loops. All 605 files under `kong/`
parse with zero tree-sitter failures in 1 s, and the whole tree indexes and
queries as a single program in under a minute.

`Variable(name)` **works and closes a real gap**: the `acme` Admin API SSRF
(`self.params.host` → `httpc:request_uri`), which no call-based model could
reach, is found and verified against source. But it is defeated by copy
coalescing in 41% of cases — and in ~100% of cases for the `ngx.var` idiom it was
most needed for (§5.5).

The dominant remaining problem is not recall but **precision**: `$globals` is a
single object, so **235 of the 304 reported flows (77%) ride the global heap**,
and a *single* saturating source in `response-ratelimiting` accounts for **183
results across ~105 distinct sinks** — 57 of which each ship **the same 12-step
flow 57 times over**, once per field of the config table the saturating source
painted, for ~630–690 code-flow steps in a single result (§5.8).

Second in line is **triage**: a third of the report points the reviewer at the
wrong line (§5.4).

Model-authoring, by contrast, is in much better shape than it was one run ago.
`qualified-id` works on Lua, the checked-in model is written entirely in terms
of it, and the two same-named-function traps the report kept hitting — a bare
name matching every module's function, and a within-module collision (`f%1`)
matching nothing — are both closed (§5.2.1, §5.10).

---

## 2. What the frontend forced on the model design

### 2.1 Symbols are fully qualified by module

The frontend imports a **directory** at once, treating it as the `require` root,
and names definitions fully:

| Lua (in `<root>/kong/pdk/request.lua`, which `return`s `_M`) | IR name |
| --- | --- |
| `function _M.get_headers()` (module table) | `kong.pdk.request.get_headers` |
| `local function helper()` | `kong.pdk.request.helper` |
| `function T.m()` / `function T:m()` (file-local `T`) | `kong.pdk.request.T.m` |
| `function kong.request.get_header()` (global root) | `kong.request.get_header` |
| anonymous / chunk | `kong.pdk.request.%anon0` / `.%chunk` |

Calls are written with plain Lua names, so the frontend resolves each callee
expression back to that qualified name — through `require`, through fields of a
required module, and through `local` aliasing:

```lua
kong.request.get_headers()                                -- kong.request.get_headers
ngx.req.get_headers()                                     -- ngx.req.get_headers
local get_headers = ngx.req.get_headers ; get_headers()   -- ngx.req.get_headers
```

Re-measured on the `kong/` tree, `get_headers` call sites separate into
`kong.request.get_headers` (15), `ngx.req.get_headers` (10),
`ngx.resp.get_headers` (4), `kong.response.get_headers` (2),
`kong.plugins.opentelemetry.utils.get_headers` (2), and a tail of PDK-internal
aliases — unchanged across all three re-runs.

`require` is resolved to the module's chunk, so `local m = require "a.b"` is a
real call edge to `a.b.%chunk` and the table that chunk returns flows to the
caller. On the `kong/` tree, **1088 of 1680** `require` sites resolve to a module
in the import (counted as `direct-call <mod>.%chunk` vs. residual `direct-call
require`); the rest are genuinely external (`resty.*`, `cjson`, `pl.*`).

Import reports no parse failures on any of the 605 files, and two coverage
warnings worth watching:

```
lua: 188 construction site(s) had an unresolved metatable; instances fall back to name-based dispatch
lua: 1235 call site(s) had a callee that could not be resolved to a qualified name
```

1235 of 18 051 call sites (6.8%) unresolved. Most are the table-field-assigned
functions of §5.9 — the fix in follow-up 5 would recover them. Both warning
counts, and the 1088/1680 `require` split above, reproduce exactly on this run.

### 2.2 A shim file is mandatory

Model generators only match functions **defined** in the indexed IR. For a Lua
VMT the loader falls into the generic branch (`models/json.rs`) and indexes
`program_info.program.functions` by name. Unresolved callees (`os.execute`,
`ngx.req.get_headers`, `httpc:request_uri`) get a `FunctionId` and a `call` fact
but **no `FunctionData`** — so no `where` constraint can ever select them.

`examples/kong/kong-externals.lua` supplies stubs for the whole external
boundary (`ngx.*`, `resty.http`, Lua stdlib, Kong PDK). Indexing `kongsrc`
without it and running the same model gives **`Matched 0 sources and 315
sinks`** — zero sources, and the only sinks that survive are Kong's *own*
functions that happen to be named `query`, `render`, `run`, `load`, which the
bare-name colon-call patterns pick up. (This was 312 last run; the +3 is the
`protect%1` fix of §5.10, and appears in every sink count in this report.)

A stub must spell the API path the way callers write it
(`function kong.request.get_headers()`, not `function get_headers()`), because a
global root names itself. Colon-called methods (`httpc:request_uri`) are the
exception: they have no static receiver type, dispatch on the bare method name,
and keep unqualified stubs.

Note the shim covers *naming*, not *propagation* — and it does not have to.
Taint flows through the return of an unmodeled callee (`string.format` in the
verified `acme` flow below), so string-building does not break a path.

### 2.3 Port conventions

- `foo(a, b)` → `Argument(0) == a`.
- `function T:m(x)` → `Argument(0) == self`, `Argument(1) == x`.
- Colon *calls* `obj:m(x)` pass the receiver as actual 0, so stubs for
  `httpc:request_uri`, `connector:query`, `template:render` take an explicit
  leading `self`.
- `Return` is formal index `-1`. The global heap is formal index `-32768`.

Sources use `saturating: true`, since callers index into the returned table
(`headers["x-foo"]`, `query[name]`) and a precise source would taint only the
base vertex. This is load-bearing for recall and ruinous for precision — see
§5.8.

### 2.4 The model is now written in `qualified-ids` — **UPDATED (2026-07-26b)**

Every dotted pattern in `kong-model.json5` moved from `names` to `qualified-ids`
once §5.2.1 was fixed. This is a safety change, not a behavioural one, and it was
measured both ways on the same index:

| model spelling | sources | sinks | results | primary source→sink pairs |
| --- | ---: | ---: | ---: | ---: |
| `names`, fully-qualified spellings | 211 | 2 084 | 304 | 174 |
| `qualified-ids`, same strings | 211 | 2 084 | 304 | 174 |

The two SARIFs agree on every counter measured in §5, so nothing downstream in
this report depends on which spelling is used. The reason to prefer
`qualified-ids` is what happens when a *future* entry is spelled short: `names`
registers each function under both its qualified name and its trailing simple
name, so `names: ["get_headers"]` matches 35 sinks across unrelated modules,
while `qualified-id: "get_headers"` matches 0 (§5.2.1). Under `qualified-ids` a
typo or an abbreviation fails loudly-by-emptiness instead of quietly widening.

Two families deliberately stay on `names`, and the model says so at each site:

- **colon calls** (`httpc:request_uri`, `connector:query`, `template:render`) —
  no static receiver type, so there is no module to qualify the method with;
- **the `load` / sandbox / protect family** — matching by trailing simple name is
  the *point*, since it is what catches Kong's own wrappers wherever they live.
  §5.10 is a case where that choice now pays off.

## 3. Named-local sources: `Variable(name)`

A source/sink port may be `Variable(<local's source name>)`, optionally with a
trailing access path (`Variable(buf).headers`), instead of a formal position. The
name is resolved per matched function against `FunctionData::locals`.

It matters for Lua specifically because OpenResty exposes the entire nginx
request through `ngx.var.<name>` — a table **field read**, not a call — which no
`find: methods` model can name. Kong's house style binds those reads to a local
immediately, so the local's name is a handle on the value:

```lua
local ngx_var = ngx.var                                  -- table alias
local binary_remote_addr = ngx_var.binary_remote_addr    -- one variable
local host = self.params.host                            -- Admin API param
```

`examples/kong/kong-model-vars.json5` models all three idioms, in three groups.
Each group was run **separately**, paired with a sinks-only copy of
`kong-model.json5`, so nothing comes from the call sources:

| group | what it names | name matches | seeded | dropped | flows |
| --- | --- | ---: | ---: | ---: | ---: |
| A — `ngx.var` reads | `var`, `ngx_var`, and 11 locals named after the nginx variable they hold | 50 | 11 | 39 | **0** |
| B — conventional names | `headers`, `body`, `host`, `path`, `uri`, `method`, `args`, `query`, `params`, `token`, … | 270 | 179 | 91 | 347 |
| C — function-scoped | 3 named functions × 1 local each | 3 | 1 | 2 | 30 |
| **total** | | **323** | **191 (59%)** | **132 (41%)** | |

"dropped" = the name resolved to a `LocalIdx`, but the post-optimization graph
has no vertex for it (§5.7). All twelve numbers reproduce the previous two runs
exactly, including Group A's zero.

### 3.1 Verified: the `acme` Admin API SSRF

Group C's one surviving source is the payoff. `kong/plugins/acme/api.lua`:

```lua
POST = function(self)
  ...
  local host = self.params.host                                          -- :78
  ...
  local check_path = string_format("http://%s/.well-known/acme-challenge/", host)  -- :96
  local httpc = http.new()
  local res, err = httpc:request_uri(check_path .. "x")                  -- :98
```

Modeled as:

```json5
{
  find: "methods",
  where: [{ constraint: "signature_match",
            "qualified-id": "kong.plugins.acme.api.%anon449" }],
  model: { sources: [{ kind: "UserInput", port: "Variable(host)", saturating: true }] },
}
```

(That generator used `names` in the previous two runs and now uses
`qualified-id`, per §2.4. Exactness fixes the *cross-module* collision but does
nothing for the `%anonNNN` instability of §5.9 — `docs/model-generators.md` now
carries an explicit warning against `%anonN` ids for exactly this reason.)

Confirmed against the IR: `%anon449` is the POST handler (`%L13=host`, from
`load @p1.params` → `.host`), and the query reports `request_uri @ api.lua:98`.
Checked against the source: real, and an SSRF an Admin API caller controls.

The same source also reaches exactly the seven `kong.response.exit` calls the
previous run found (`:72`, `:92`, `:109`, `:112`, `:117`, `:121`, `:124`), all of
which really do interpolate `host` into the response body. True positives of low
severity.

It does **not** legitimately reach SQL. Group C's 30 flows are 3 into
`request_uri`, 7 into `kong.response.exit`, and **20 into the Postgres
strategy** — 18 `ngx.log` at `init.lua:{234,271,297}` and 2
`connector._mt.query` (`init.lua:408`, `connector.lua:547`). Those ride
`call-arg(N, -32768).kong.ctx.plugin.identifier`, the global heap, and the sink
argument at `init.lua:408` is `statement[1]`, a *prepared* statement, not entity
data. False positives; see §5.8.

### 3.2 Group A: the `ngx.var` class is still out of reach

Group A is the whole reason the feature is interesting for Lua, and run on its
own it produces **zero flows** (the SARIF carries only the "ran to completion,
found no flow" open result). 39 of its 50 name matches were dropped, including
every one that mattered:

```
Variable selector: local %L2 has no versioned vertex in
'kong.plugins.ip-restriction.handler.do_restrict', so this source/sink seeds nothing
```

That is `local binary_remote_addr = ngx_var.binary_remote_addr`
(`ip-restriction/handler.lua:75`, verified in source) — the canonical example.
The 11 that *did* seed are chunk-level `ngx_var` aliases whose readers see
`$globals.ngx_var` rather than the chunk local, so they connect to nothing
either.

The cause is §5.7: `local x = <expr>` is a copy, and copy coalescing fuses it
away before the graph is built. The idiom the feature targets is precisely the
idiom the optimizer erases.

### 3.3 Group B: reach without precision

Group B (program-wide `where: [{constraint: "name", pattern: ".*"}]` + a list of
conventional names) is the "how far does this go" experiment. It seeds 179
sources and yields 347 flows, but the top of the list is:

```
57  kong.conf_loader.parse.check_and_parse          -> ngx.log
44  kong.db.dao.new                                 -> ngx.log
21  kong.db.schema.run_transformation_checks        -> ngx.log
18  kong.plugins.oauth2.access.generate_token       -> ngx.log
11  kong.plugins.datadog.statsd_logger.new          -> ngx.log
```

Config parsing and schema validation — not request data. `Variable(name)`
matches a bare source name, so a `host` parsed from plugin config is
indistinguishable from a `host` read off the Admin API. The function-scoped form
in Group C is the usable one; the program-wide form is a survey instrument, not
a model.

The #2 slot keeps changing hands: the first run gave `kong.db.dao.new` 44;
2026-07-26 gave `statsd_logger.new` 43 and `db.dao.new` 12; this run gives
`db.dao.new` 44 and `statsd_logger.new` 11. These are two distinct symbols, both
seeded (each has a local matching one of Group B's names), and the pair sums to
55–56 every time — so what moves is which of the two reaches the `ngx.log` sinks,
not the total. Treat the split as unstable and the sum as the measurement.
Everything else in the top five is byte-stable across all three runs, and the
conclusion is unchanged in any case: none of this is request data.

---

## 4. Verified flows (call sources)

Each was checked against the Kong source, not just taken from the SARIF. Sink
and source names below are the **IR-resolved** names, which differ from the text
in the file wherever Kong aliases a PDK function to a local (e.g.
`request-transformer/access.lua:8-9` binds `get_uri_args`/`set_uri_args` to
`kong.request.get_query`/`kong.service.request.set_query`).

| plugin | flow |
| --- | --- |
| `azure-functions` | `get_headers` (handler.lua:50), plus `get_method` / `get_raw_body` / `get_query` written inline at :58–61 → options table → `httpc:request_uri(uri, {...})` (:57) |
| `correlation-id` | `kong.request.get_header` (:57) → `kong.service.request.set_header` (:62); and (:81) → `kong.response.set_header` (:87) — across the `if not correlation_id` join |
| `request-transformer` | `get_headers` (access.lua:164) → `set_headers` (:231); `get_raw_body` (:434) → `set_raw_body` (:450) and `set_header` (:451); `get_query` (:468, written `get_uri_args`) → `set_query` (:481, written `set_uri_args`) |
| `ai-prompt-template` | `kong.request.get_body` (render-prompt-template.lua:71) → `templater.render` (:103) |
| `grpc-web` | `kong.request.get_path` (handler.lua:50) → `ngx.req.set_uri` (:48) and → `kong.response.exit` (:42); `get_raw_body` (:66) → `set_raw_body` (:66) |
| `grpc-gateway` | `kong.request.get_raw_body` (handler.lua:55) → `kong.service.request.set_raw_body` (:60); `ngx.req.get_uri_args` (deco.lua:261) → `ngx.req.set_uri` (handler.lua:62) |
| `oauth2` | `kong.request.get_query` (access.lua:211) → `kong.log.warn` (:609) and → `kong.response.exit` (:451, :824); `get_body` (:913) → `set_query` (:905) |
| `llm/drivers/*` | `kong.request.get_path` (shared.lua:1048) → `kong.service.request.set_path` in 8 drivers; and per-driver `kong.request.get_query` → `set_header` / `set_query` in 7 (anthropic, azure, cohere, gemini, llama2, mistral, openai) |

Two corrections to the previous write-up's table, both from reading this run's
code flows rather than the plugin names: the `llm/drivers` family is seeded by
`get_path` and `get_query`, **not** `get_raw_body`; and the `grpc-gateway` and
`grpc-web` rows each have a second real flow that was not listed.

Every row above was re-derived from this run's code flows and reproduces at the
same line numbers. Two additions worth recording, neither a correction: the
`llm/drivers` `get_path` source reaches `set_scheme` as well as `set_path`, in
the same 8 drivers (which the previous run counted but did not name: `anthropic`,
`azure`, `bedrock`, `cohere`, `gemini`, `llama2`, `mistral`, `openai`); and
`ai-response-transformer` — 4 results in the sweep below, but absent from the
table — has a real flow of its own,
`get_headers`/`get_method`/`get_raw_body` (transform-response.lua:82) →
`kong.tools.stream_api.request` (:98).

The `opentelemetry` false positive from the original run is **still gone**: it
came from `utils.lua:44` defining its own `get_headers(conf_headers)` over plugin
configuration, which bare-name symbols merged with `kong.request.get_headers`.
`kong.plugins.opentelemetry.utils.get_headers` is now a distinct symbol and is
not in the source list. (`opentelemetry/utils.lua:136` does still appear, but as
a *sink* — a bare-name `render` — reached by the §5.8 fan-out.)

### Plugin sweep

Whole `kong/` tree as one program, per-plugin result counts (attributed by the
sink's file):

```
oauth2 22   acme 10   request-transformer 9   aws-lambda 6   opentelemetry 5
ai-response-transformer 4   azure-functions 4   grpc-web 3   prometheus 3
correlation-id 2   grpc-gateway 2   response-ratelimiting 2
ai-prompt-template 1   acl 1        …the other 32 plugins with 0
```

Identical to the previous run. Counts are raw SARIF result counts and are
inflated by the fan-out in §5.8; 230 of the 304 results have a sink outside
`plugins/` entirely.

---

## 5. Measurements

### 5.0 Scale

Two configurations. **A** is the `kong/` tree alone (605 files) — the
apples-to-apples config, staged into a scratch directory so `kong/` is a child of
the `require` root. **B** is the whole repo (1309 `.lua`, adding `spec/` and
`t/`), which is what you get pointing `import` at the checkout.

| | A: `kong/` only | B: whole repo |
| --- | --- | --- |
| `.lua` files | 605 | 1309 (1 skipped: `spec/fixtures/invalid-module.lua`, deliberately invalid) |
| functions | 4 306 | 18 342 |
| assignments | 108 339 | 407 447 |
| call sites | 18 051 | 93 170 |
| import | 1.0 s | 3.6 s |
| index (+ shim) | 1.3 s / 335 MB | 7.2 s / 1.14 GB |
| query | 41 s / 2.06 GB | 111 s / 7.60 GB |
| sources / sinks matched | 211 / **2 084** | 244 / **3 635** |
| results | 304 (**174** distinct source→sink location pairs) | 437 |

Functions, assignments, call sites, sources and results reproduce the previous
run exactly. One cell moved for a reason; the other two are noise of two
different kinds:

- **sinks +3 in both configurations** (2 081→2 084, 3 632→3 635) — real, and the
  only shape change on this run: the `protect%1` fix of §5.10, confirmed by
  diffing the sink markers between builds. It is the same +3 in the no-shim run
  of §2.2 (312→315).
- **distinct pairs 175→174** — *index* noise, not a change. Re-indexing the same
  artifacts and re-querying gives 175, 175, 175 on three further indexes of this
  same build (§6.1). The counter is bracketed 174–175 and cannot detect anything.
- **timings** remain machine noise; both queries got faster this run while memory
  rose, in the same directionless way as last time.

Both are workable; A is the one to use. B's extra 1 551 sinks are mostly junk —
`spec/` defines hundreds of helper functions named `query`, `request`, `load`,
`run`, `render`, which the colon-call sink patterns (which must be bare names,
§2.1) collide with. Import `kong/` alone unless you specifically want the test
tree analyzed.

B is also the worse config for precision, measured for the first time this run:
**365 of its 437 flows (84%) ride the global heap**, against 235 of 304 (77%) in
A, and its largest single result carries **62 alternative code flows** totalling
806 steps (§5.8). Adding `spec/` widens the §5.8 artifact rather than finding
anything new.

The previous run's co-indexing scale table (100 programs → 4 s, 150 → 46 s, 200 →
timeout, 605 → killed) stays dropped: directory import puts a whole tree in one
program, so co-indexing hundreds of programs is not a workflow anyone uses now.

### 5.1 Wildcard sink ports on stub bodies — **FIXED**

The original report's highest-value defect: a wildcard sink port did not expand
over field paths when the sink was an empty stub, so taint arriving on a *field*
of an argument was missed — the common case in Lua, where options tables are the
calling convention. The minimal repro is now a checked-in regression case,
`Lua:wildcard-table-arg-flow`, and it passes.

On Kong the hand-enumerated field ports are now provably redundant. Re-adding
`Argument(2).body` / `.headers` / `.query` / `.path` / `.method` to the
`request_uri` sink raises the matched-sink count from 2 084 to 2 269 and changes
the answer **not at all**: 304 results and the same 174 distinct source→sink
pairs either way, set-identical (verified as a set difference, empty in both
directions). On config B the same edit gives 3 915 sinks instead of 3 635, and
again 437 results either way. Third run in a row with this result.

Two claims from the previous run are deleted rather than restated, because
neither reproduces:

- "removing the field ports loses 1 unique flow and gains 5" — on this tree the
  difference is exactly zero, in both configurations;
- "the field-port model made the config-B query cost 603 s / 11.5 GB max RSS,
  and removing the ports cut it 5×" — measured here, the field-port model runs
  **110 s / 8.23 GB peak** against **111 s / 7.60 GB** without, i.e. the
  redundant ports still cost nothing in time and ~8% in peak memory. Whatever
  produced the 5× is gone.

The conclusion — delete them — still holds on parsimony grounds, and they are
already out of the checked-in model.

### 5.2 Name collisions — **FIXED**

Fully-qualified definitions and resolved call names make Kong's own
`get_headers` a different symbol from the PDK's, which was the source of the
original `opentelemetry` false positive. The disambiguation levers the original
report asked for are not needed for this case.

The disambiguation levers fare differently on this tree:

- `parent` / `parents` are Java-only (`models/json.rs`) — they log
  *"'parent' constraint is Java-only; matching nothing on this frontend"* and
  match nothing. Re-confirmed unchanged in 2026-07-26b, both as a
  `signature_match` field and as a standalone `parent`/`inner` constraint.
- `unqualified-id` — **FIXED in #79.** The previous two runs found it first
  silently matching *nothing* and then silently matching *everything* (287 876
  sinks). It is now rejected at model-load time with a message that enumerates
  the alternatives, which is exactly what follow-up 4 asked for:

  ```
  > unexpected field 'unqualified-id' in model generator at index 0: not a
    recognized field of the 'signature_match' constraint; expected one of
    'name', 'names', 'parent', 'parents', 'qualified-id', 'qualified-ids'
  ```

  It is gone from `docs/model-generators.md` too, replaced by `qualified-id`.

#### 5.2.1 `qualified-id` on Lua — **FIXED (2026-07-26b)**

Reported last run as *"matches nothing on Lua"*: `ModelIndex` construction filled
`program_method_names` and `program_method_signatures` for the
`VirtualMethodTable::Lua` arm but never `program_method_qualified_ids`, so the
one documented way to name a single method exactly was the one lever Lua did not
have. Follow-up 4 asked for the missing line; the `qualified-id` commit added it.
Re-measured on the same string, and with the same probes as last run:

| generator | sinks matched | pre-change build |
| --- | ---: | ---: |
| `{"signature_match", "names": ["kong.request.get_headers"]}` | 15 | 15 |
| `{"signature_match", "qualified-id": "kong.request.get_headers"}` | **15** | **0** |
| `{"signature_match", "names": ["get_headers"]}` | 35 | 35 |
| `{"signature_match", "qualified-id": "get_headers"}` | **0** | 0 |

(The right-hand column was re-run on a build of the pre-fix commit, not quoted
from the previous write-up — so the only cell that moved is the one the fix
targets.)

Rows 1–2 are the fix: the fully-qualified spelling now resolves under
`qualified-id` exactly as it does under `names`. Rows 3–4 are the *point* of the
fix, and the reason the model moved onto it (§2.4): the bare name still matches
35 unrelated functions under `names` and is still correctly refused under
`qualified-id`. A `qualified-id` is never keyed on a simple name, which is what
makes it a disambiguator rather than a second spelling of `names`.

Two supporting changes landed with it. `docs/model-generators.md:175` gained the
Lua row the frontend table was missing, spelling out that a Lua function has only
this one name, that a global root names itself (`ngx.req.get_headers`), and that
a single file imported alone has an empty module name so its ids are bare names.
And `nightly/tests/lua/qualified-id-flow` pins the behaviour: two modules define
`read`, the model names one by qualified id, and the test fails if the flow
reaches the decoy — so the regression suite would now catch a repeat.

The `Unknown`/`CplusPlus` fallback still fills the map, so no frontend is left
worse off than an unrecognized language. What is *not* addressed: `qualified-id`
cannot pin a function the frontend named `%anonNNN`, because that name is not
stable across import sets (§5.9) — `docs/model-generators.md:186` now warns
against it explicitly rather than leaving it to be discovered.

### 5.3 `find: "variables"` and `find: "fields"` — schema/loader mismatch

`ctadl-model-generator.schema.json:330` advertises them:

```json
"find": { "enum": ["methods", "variables", "fields", "callsites"] }
```

`visit_find` in `models/json.rs` accepts only `methods` and `callsites`. Still
broken, but the diagnostic is materially better than the previous run's bare
*"JSON model parsing error"* — #78 turned it into:

```
0: encoding models
1: JSON model parsing error
2: > unexpected constraint type 'variables' in model generator at index 0
```

Either implement them or drop them from the enum.

### 5.4 `Variable(name)` sources are still invisible in the SARIF code flow

A flow seeded at a named local still has no code-flow step for the local. The
`acme` SSRF renders as:

```
1  call call-arg(138123, -1) in string.format   | acme/api.lua:96
2  sink call-arg(138126, 1)  in request_uri     | acme/api.lua:98
```

**Correction to the previous run**, which reported this collapsing to a single
`request_uri` line with the `string.format` step "gone too". It is not gone; the
flow is two steps, as it was two runs ago. The defect itself is unchanged and is
the whole of the finding: `host` at `:78` — the actual source, and the only thing
that tells you *why* this is a finding — never appears in the code flow, and no
step is labelled `source`. The reviewer is shown a string being formatted and a
request being made, with nothing to say where the attacker-controlled value
entered.

One thing the previous run got wrong: it reported that grepping the SARIF for
`%L` returned nothing. It does not — there is an informational
`C0003.taint-source` result reading *"Source of tainted data: local(%L13_0) in
function kong.plugins.acme.api.%anon449"*. But it carries only a
`logicalLocations` index, no `physicalLocation`, and it is not linked from the
`tainted-path` result. So the raw fact is in the file; nothing a reviewer looks
at surfaces it.

Relatedly, the code-flow **source step is unreliable even for call sources**.
The four `azure-functions` results have `properties.sourceCallee` of
`get_headers`, `get_method`, `get_query`, `get_raw_body` respectively — correct —
but all four render the identical single source step *"call-arg(145334, -1) in
kong.request.get_headers | handler.lua:50"* and the identical sink vertex
`call-arg(145356, 2).headers`. Three of the four point the reviewer at the wrong
line. Re-confirmed byte-for-byte in 2026-07-26b, vertex ids included.

#### 5.4.1 The same defect on the sink side

The previous runs recorded this only for sources. It is not source-specific, and
measured across the whole base run it is the report's second-largest problem
after §5.8. Of the 304 results:

| | count | share | 2026-07-26 |
| --- | ---: | ---: | ---: |
| result's primary `locations[0]` ≠ last code-flow step's location | **104** | 34% | 99 / 32% |
| `properties.sinkCallee` not even named in the last code-flow step | **21** | 7% | 17 / 5% |
| results whose primary code flow is byte-identical to another result's with a *different* `sinkCallee` | **33** (13 distinct flows) | 11% | 33 (13) |
| …with a different `sourceCallee` | **12** (3 distinct flows) | 4% | — |

All four rows are measured on each result's **primary** code flow (`codeFlows[0]`,
the one a viewer opens), which matters now that §5.8 has established most results
carry several.

Rows 1 and 2 are **not stable enough to compare across runs**: four indexes of
the identical inputs on this one build give 104/101/103/105 for row 1 and
21/20/20/19 for row 2 (§6.1). The pre-change build's 103 and 23 sit inside and
beside those brackets. So nothing here is attributable to the recent commits, and
the honest statement of the finding is the one that survives the noise: **about a
third of the report — 33–35%, ~100 of 304 results — opens on a line that is not
where the flow ends.** Rows 3 and 4 were stable across every run measured.

`locations[0]` is what a SARIF viewer opens, so the first row is the one that
bites: a third of the report opens on a line that is not where the flow ends.

The clearest specimen is `request-transformer`, whose §4 row lists
`get_raw_body` (:434) → `set_raw_body` (:450) **and** `set_header` (:451). Both
sinks are real and both are reported — but they are reported as two results with
*one* code flow between them:

```
result A  sinkCallee = kong.service.request.set_raw_body   locations[0] = :442
result B  sinkCallee = kong.service.request.set_header     locations[0] = :442
  both render, identically:
    1 source call-arg(176294, -1) in kong.request.get_raw_body            | :434
    2 call   call-arg(176314, 2)  in ...access.transform_url_encoded_body | :442
    3 sink   call-arg(176332, 0)  in kong.service.request.set_raw_body    | :450
```

Against the source (`access.lua:449-451`):

```lua
  if is_body_transformed then
    set_raw_body(body)                      -- :450
    set_header(CONTENT_LENGTH, #body)       -- :451
  end
```

So for result B the actual sink, `set_header` at `:451`, appears in **no** step
and in neither location; the reviewer is sent to `:442`, an intermediate call
into a helper. And both results anchor at `:442` rather than at `:450`, so even
the correctly-rendered result A opens on the wrong line.

This is the same underlying shape as the source-side bug above — one code flow
computed per source/sink *pair* but rendered from a shared representative — and
it argues that follow-up 3 should be widened from "emit the seeding local" to
"render each result's own endpoints at both ends."

Re-confirmed byte-for-byte in 2026-07-26b: same two results, same `:442` anchors,
same vertex ids `176294` / `176314` / `176332`.

### 5.5 `ngx.var.X` reads: still effectively unmodelable

`ngx.var.remote_addr` lowers to a field *load* (`load $globals.ngx` → `.var` →
`.remote_addr`), not a call. `Variable(name)` is the first mechanism that could
have caught it, and §3.2 shows it does not, because of §5.7. Still unreachable,
all verified in source on this checkout:

- `ip-restriction/handler.lua:75` — `local binary_remote_addr =
  ngx_var.binary_remote_addr` (0 flows)
- `jwt/handler.lua:47,49` — `local var = ngx.var` then `var["cookie_" .. v]`
  (0 flows; the previous run cited `:47` for the indexed read, which is the
  alias line — the read is `:49`)
- `runloop/handler.lua` — 52 `var.*` reads

`acme/api.lua:78`'s `self.params.host` is the one member of this class that is
reachable (§3.1) — and only because `self` is a real parameter, so `host` is a
load off a parameter rather than off a chunk-level alias.

### 5.6 `load()`-generated code is an opaque barrier

Unchanged, and re-verified in source. The `key-auth` → PDK → DAO → SQL path dies
at `local sql = statement.make(argv)`
(`kong/db/strategies/postgres/init.lua:448`), where `make` is a string
interpolator built at runtime by `return load(c, "=" .. name, "t", {...})` at
`init.lua:62`. Not a CTADL bug, but the Postgres strategy remains unreachable
end-to-end — `key-auth` has 0 results — and the SQL findings that *do* appear are
the §5.8 artifact, not this path.

### 5.7 `Variable(name)` is defeated by copy coalescing

The selector resolves a name against the **pre-optimization** IR
(`FunctionData::locals`) but seeds against the **post-optimization** graph, which
has run `eliminate_dead_temps` / `coalesce_copies` / `propagate_copies`. A local
whose definition is a plain copy has no `%L{idx}_{version}` vertex left.

`local x = <expr>` is the single most common statement in Lua. Re-measured drop
rate on Kong: **132 of 323 resolved names (41%)**, and 39 of 50 (78%) for the
`ngx.var` group in §3.2 — a group that consists entirely of `local x =
<field read>`. Identical to the previous run.

The code already knows this is happening and warns loudly
(`query_engine/endpoints.rs`), which is how it was diagnosed. But a warning that
fires on the intended use case is a design gap, not a diagnostic. The fix is to
seed the copy-class representative — the same `rep_of` mechanism that fixed §5.1
— rather than requiring a surviving `%L{idx}_{version}`.

### 5.8 `$globals` conflation + `saturating` is the dominant false-positive engine

Lua has one global namespace, and the frontend models it as one object, carried
through every function as an implicit global formal (index `-32768`). A
`saturating` source that reaches *any* global therefore taints reads of *every*
global, program-wide.

This run quantified it, and the numbers are much starker than the earliest one's:

- **235 of 304 results (77%)** contain at least one `-32768` step, counting every
  code flow attached to the result. Counting only each result's *primary* flow —
  the one a viewer opens — it is 227 (75%). Both numbers were measured this run;
  the 227/235 split reported as movement between 2026-07-25 and 2026-07-26 turns
  out to be these two measurements, not two states of the tree.
- One source — `response-ratelimiting/access.lua:30`, a legitimate
  `kong.client.get_forwarded_ip()` — produces **183 of the 304 results** and
  reaches **105–106 distinct sink locations** (the count is index-sensitive,
  the 183 is not — §6.1), in `conf_loader` (58),
  `clustering/*` (39), `dns/client` (20), `api/endpoints`, `db/declarative`,
  `llm/drivers`, `aws-lambda`, `oauth2`, `request-transformer`. The first run
  estimated "~20 distinct sinks" for this source; the real figure is five times
  that. (The pre-change build gives 189 results / 113 sinks for the same source.
  Given §6.1 that gap is unattributed — one index was measured on each side —
  and it is small next to the artifact it describes.)

A representative fan-out, unrelated subsystems end to end:

```
1  source call-arg(177174, -1) in kong.client.get_forwarded_ip            | response-ratelimiting/access.lua:30
2  return call-arg(177301, -1) in ...response-ratelimiting.access.execute | response-ratelimiting/access.lua:72
3  return call-arg(197986, -32768).kong.ctx.plugin.identifier             | router/atc.lua:383
4  return call-arg(198093, -32768).kong.ctx.plugin.identifier             | router/atc.lua:439
5  return call-arg(8037,   -32768).kong.ctx.plugin.identifier             | api/routes/filter_chains.lua:111
6  return call-arg(8098,   -32768).kong.ctx.plugin.identifier             | api/routes/filter_chains.lua:136
7  call   call-arg(8106,   -32768).kong.ctx.plugin.identifier             | api/routes/filter_chains.lua:138
8  call   call-arg(4227,   -32768).kong.ctx.plugin.identifier             | api/endpoints.lua:136
9  sink   call-arg(4214, 0) in kong.log.err                               | api/endpoints.lua:119
```

Steps 3–8 are the global heap threading through the router, the Admin API and
the endpoint helpers, none of which the source touches. This trace re-renders
byte-for-byte on the current build, vertex ids included; the only difference is
that each step now also names its containing function, which is an improvement.

**How much SARIF these actually generate — RESTATED (2026-07-26b).** The previous
run reported *"57 results carry a code flow of 627 steps"* and called it
unreadable at any length. **That reading was wrong and is retracted.** No code
flow in the report is anywhere near that long: the longest single flow is 15–16
steps, and the 57 results in question have primary flows of **12** steps. What
those results actually carry is **57 alternative code flows each** — 57 × 11 =
627, or 57 × 12 = 684, locations in one result depending on the index (§6.1).
The earlier figure was exactly that product, read as a single flow's length. The
number 627 was right; what it counts was not.

Both distributions, measured over the 304 results:

```
primary flow steps    2   3   4   5   6   7   8   9  10  11  12  13  14  15  16
results              37  26   1  12   1  10  22  42  37  43  66   3   2   1   1

flows per result      1   2   3   4   5   6  57
results             143  36   3  24   5  36  57
```

And the 57 are not 57 different routes. Checked step by step, all 57 share a
**byte-identical 11-step prefix** and differ *only in the access path on the
final sink vertex*:

```
  1–11  identical in all 57:  access.lua:30 → access.lua:72 → atc.lua:{383,439}
        → snis.lua:133 → process_secrets.lua:{45,109,175}
        → conf_loader/init.lua:{456,540} → parse.lua:537

  12    sink call-arg(41427, 1)                        in ngx.log | parse.lua:449
        sink call-arg(41427, 1).admin_gui_listen       in ngx.log | parse.lua:449
        sink call-arg(41427, 1).admin_gui_listen.[1]   in ngx.log | parse.lua:449
        sink call-arg(41427, 1).admin_listen           in ngx.log | parse.lua:449
        sink call-arg(41427, 1).client_ssl_cert        in ngx.log | parse.lua:449
        …  52 more, one per Kong configuration field  …
        sink call-arg(41427, 1).trusted_ips            in ngx.log | parse.lua:449
```

That is `saturating` doing exactly what §2.3 warns it does: the source paints
every field of the config table, and each painted field is emitted as its own
code flow to the same line of the same function. So the shape of the finding
survives intact and gets sharper — the problem is not one unreadable flow, and
not 57 routes worth reviewing, but **one flow reported 57 times**, once per field
name.

The size cost the previous run identified is therefore real and its cause is
cheaper to fix than the precision problem: those 57 results alone account for
~36 000 code-flow locations, so the SARIF is far larger than "304 results"
suggests, and on config B the same effect produces a result carrying **62 flows
/ 806 locations**. Collapsing flows that share a prefix and differ only in the
sink's access path would remove almost all of it without touching the analysis.

The worst single artifact this run found is a **command-injection report**:

```
1  source call-arg(117096, -1) in kong.service.response.get_raw_body      | llm/plugin/shared-filters/parse-json-response.lua:22
2  call   call-arg(117107, -1) in kong.tools.gzip.inflate_gzip            | llm/plugin/shared-filters/parse-json-response.lua:25
3  return call-arg(114162, -32768).CACHE_MISS_SENTINEL_LRU                | llm/plugin/base.lua:53
4  return call-arg(114210, -32768).CACHE_MISS_SENTINEL_LRU                | llm/plugin/base.lua:67
5  return call-arg(168365, -32768).CACHE_MISS_SENTINEL_LRU                | plugins/prometheus/prometheus.lua:760
6  return call-arg(94935,  -32768).CACHE_MISS_SENTINEL_LRU                | init.lua:710
7  call   call-arg(94886,  -32768).CACHE_MISS_SENTINEL_LRU                | init.lua:718
8  call   call-arg(49077,  -32768).CACHE_MISS_SENTINEL_LRU                | db/dao/plugins.lua:328
9  call   call-arg(48983,  -32768).CACHE_MISS_SENTINEL_LRU                | db/dao/plugins.lua:276
10 call   call-arg(48738,  -32768).CACHE_MISS_SENTINEL_LRU                | db/dao/plugins.lua:172
11 call   call-arg(220311, -32768).CACHE_MISS_SENTINEL_LRU                | runloop/plugin_servers/init.lua:43
12 call   call-arg(220301, -32768).CACHE_MISS_SENTINEL_LRU                | runloop/plugin_servers/init.lua:37
13 call   call-arg(220256, 0)                                             | runloop/plugin_servers/init.lua:23
14 call   call-arg(221147, 0)                                             | runloop/plugin_servers/process.lua:94
15 sink   call-arg(221044, 0) in io.popen                                 | runloop/plugin_servers/process.lua:52
```

An upstream response body reaching `io.popen` would be the most severe finding in
the report. It is entirely manufactured by a single global field,
`CACHE_MISS_SENTINEL_LRU`, riding the `-32768` formal through eleven functions,
and then — at step 12→13 — jumping *off* the global vertex onto an ordinary
argument. That last hop is genuine over-approximation, not just global
threading, and it is the same shape as the 8→9 hop the first run flagged in
the `acme` trace.

Re-confirmed byte-for-byte in 2026-07-26b, all fifteen vertex ids included, and
it is a single-flow result — this one *is* as short as it looks, which is what
makes it the most plausible-looking false positive in the file.

Quantified the other way: re-running the identical model with `saturating: false`
on every source drops results from **304 to 65** — reproducing all three previous
runs exactly, and killing every one of the 57 multi-flow results. The oldest run
described the survivors as "tight 2–3 step flows within one or two files"; that
is true of 43 of the 65 (31 two-step, 12 three-step), but the remaining 22 are
5–15 steps, so the long-range flows do not all come from saturation. (Previous
runs split this 44/21; one flow moved across.) Turning saturation off is still
not the answer — several §4 flows (`request-transformer`, `ai-prompt-template`)
need it, because callers index into the returned table — but it locates the cost
precisely: it also drops the global-heap share from 77% to 32%.

### 5.9 `%anonNNN` names renumber with the import set

Re-confirmed exactly. A function assigned to a table field (`["/acme"] = { POST =
function(self) …`) is named `%anonNNN` by a per-import counter. The `acme` POST
handler is `kong.plugins.acme.api.%anon449` importing `kong/` alone and
`kong.plugins.acme.api.%anon451` importing the repo root — verified again this
run by dumping the IR for both artifacts. A model that names one silently matches nothing under
the other. This makes any `%anon` handle unusable in a checked-in model, and it
is the reason the `acme` finding took a function-scoped generator to pin down at
all. `%anon449` is still the POST handler under config A (`%L13=host`, from
`load @p1.params` → `.host`) and `%anon451` under config B. Switching that
generator to
`qualified-id` (§2.4) does **not** help here — an exact id is still an id, and
the id itself is what moves. `docs/model-generators.md:186` now warns about this
in the reference docs rather than only in this report.

### 5.10 Same-named functions within one module — **FIXED (2026-07-26b)**

Not previously reported, and found by re-running the pre-change build against
this one. When a module defines two functions of the same name, the second's IR
name is disambiguated to `<module>.f%1`. Model matching used to derive the simple
name by splitting the IR name on `.`, which yields `f%1` — a string no model
spells — so the second function was unmatchable by name. It now comes from a
`functions` column the frontend fills at collection time, with the name the
definition site actually wrote.

This is not hypothetical on Kong. `kong/tools/sandbox/kong.lua` defines
`sandbox.protect` at `:71` and then `local function protect` at `:84`:

```lua
-- IR name: kong.tools.sandbox.kong.protect
function sandbox.protect(chunk, chunkname_or_options, mode, env)     -- :71
...
-- IR name: kong.tools.sandbox.kong.protect%1   <-- was unmatchable
local function protect(chunk, chunkname, env)                        -- :84
...
function sandbox.protect_lua(chunk, chunkname)                       -- :94
  return protect(chunk, chunkname, get_lua_env())                    -- calls protect%1
```

The second one is the real sandbox entry point — `protect_lua`, `protect_schema`
and `protect_handler` all funnel through it — and it is a code-injection sink the
model's `protect` pattern was meant to catch and silently did not. It is now
matched, which is the entire +3 in every sink count this run (2 081→2 084 in
config A, 3 632→3 635 in B, 312→315 with no shim).

Note this only works because the sandbox family stays on `names` (§2.4):
`qualified-id` would require spelling `protect%1`, an artifact of collision order
that is no more stable than a `%anonN`.

### 5.11 Minor

- No `default-index.jsonl` for Lua. The jadx and pcode defaults are compiled in
  unconditionally (`models/mod.rs:41-57`, `try_load_default_models`) regardless of
  artifact language, so a Lua file defining `read`, `system`, or `strcpy`
  silently inherits C models. Unchanged.
- `find: "methods"` with no `where` **now matches every function** — 315 332 sink
  ports on config A. This corrects the previous entry, which said it matched only
  recognized *class* methods on a Lua VMT. The change is deliberate (`matched_functions`
  reads the same `functions` column as §5.10, so "all" on Lua now means what it
  means on java and native, and a top-level `not` is no longer inverted against an
  empty universe). The advice is unchanged and now matters more, not less: always
  supply at least one name constraint.
- Co-indexed-program location attribution was not re-measured. At the 2 programs
  this run uses (shim + tree) every location resolved to the right *file*. That
  is a separate question from §5.4.1, which is about the wrong *line* within the
  right file; no result was ever attributed to the wrong program or artifact.

---

## 6. Reproduction

These are the exact commands this run used. `CT` is the release binary; every
command is run from the repo root, with Kong checked out at `../kong`.

```bash
CT=./target/release/ctadl
cargo build --release

# Kong must be staged so that `kong/` is a CHILD of the require root: the source
# writes `require "kong.pdk.request"`, and the import root is what module names
# are relative to. `import` does not follow symlinks, so this is a real copy.
mkdir -p /tmp/kongroot && rm -rf /tmp/kongroot/kong
cp -R ../kong/kong /tmp/kongroot/kong          # 605 .lua files
```

**Config A — `kong/` only. This is the run every number in this report is from.**

```bash
$CT --store ./store import -l lua -n _shim   examples/kong/kong-externals.lua
$CT --store ./store import -l lua -n kongsrc /tmp/kongroot
$CT --store ./store index kongonly _shim kongsrc

# base run: call sources only  -> 211 sources, 2 084 sinks, 304 tainted-path
$CT --store ./store query kongonly \
    -m examples/kong/kong-model.json5 -o base.sarif
```

The one-shot `go` form reproduces the same `Matched 211 sources and 2084 sinks`
and the same 304 results. The shim is still passed as its own artifact — `go`
imports each argument separately and indexes them together:

```bash
$CT --store ./store go -l lua -n kongonly \
    examples/kong/kong-externals.lua /tmp/kongroot \
    -m examples/kong/kong-model.json5 -o base.sarif
```

**Config B — the whole repo** (§5.0), which is what you get pointing `import` at
the checkout. No staging needed, since the checkout's own `kong/` is already a
child of it:

```bash
$CT --store ./storeB import -l lua -n _shim   examples/kong/kong-externals.lua
$CT --store ./storeB import -l lua -n kongall ../kong
$CT --store ./storeB index kongrepo _shim kongall
$CT --store ./storeB query kongrepo \
    -m examples/kong/kong-model.json5 -o B-base.sarif   # 244 / 3 635, 437 results
```

**Named-local sources** (§3). Run each group *separately*, paired with a
sinks-only copy of the call model, so nothing is inherited from the call sources
and every flow is attributable to the group:

```bash
# sinks-only copy: drop the two source generators
awk '/---------- SOURCES/{s=1} /---------- SINKS/{s=0} !s' \
    examples/kong/kong-model.json5 > /tmp/kong-sinks-only.json5

# RUST_LOG=warn is REQUIRED: the two Variable() failure modes are only visible there
RUST_LOG=warn $CT --store ./store query kongonly \
    -m /tmp/kong-sinks-only.json5 \
    -m /tmp/vars-A.json5 -o vars-A.sarif 2> vars-A.log

grep -c 'not found in'            vars-A.log   # name did not resolve at all
grep -c 'has no versioned vertex' vars-A.log   # resolved, then dropped (§5.7)
```

`/tmp/vars-{A,B,C}.json5` are the three groups of
`examples/kong/kong-model-vars.json5` split into separate files, each wrapped in
`{ model_generators: [ … ] }`. Loading all three at once gives the union, not the
per-group attribution in §3's table.

**The variant runs** used for individual measurements:

```bash
# §5.1  redundant field ports -> 2 269 sinks, same 304 results
$CT --store ./store query kongonly -m /tmp/kong-model-fieldports.json5 -o fieldports.sarif

# §5.8  saturation off -> 65 results
sed 's/saturating: true/saturating: false/g' examples/kong/kong-model.json5 > /tmp/nosat.json5
$CT --store ./store query kongonly -m /tmp/nosat.json5 -o nosat.sarif

# §2.2  no shim -> 0 sources, 315 sinks (and exits non-zero: no analyzable endpoints)
$CT --store ./store index noshim kongsrc
$CT --store ./store query noshim -m examples/kong/kong-model.json5 -o noshim.sarif

# §5.2/§5.2.1  four-way probe: names vs qualified-id, qualified vs bare
$CT --store ./store query kongonly -m /tmp/probe-qid.json5 -o /dev/null   # 15 sinks
```

**Inspecting the IR** — how §2.1, §3.1 and §5.9 were verified:

```bash
$CT --store ./store inspect kongsrc                      # functions / assignments / call styles
$CT --store ./store inspect kongsrc --dump-ir --function 'acme.api' | grep '^define'
$CT --store ./store inspect kongsrc --dump-ir > kong-ir.txt
grep -oE 'direct-call [A-Za-z0-9_.%-]+\.%chunk\(' kong-ir.txt | wc -l   # 1088 resolved
grep -oE 'direct-call require\('                  kong-ir.txt | wc -l   #  592 residual
```

### 6.1 What actually reproduces — **NEW (2026-07-26b)**

Running the commands above twice does **not** give you the same SARIF, and this
has to be stated before any number in §5 is compared against anything.

`query` is deterministic *given an index*: three consecutive queries against one
index produced byte-identical 50 205 090-byte files. But `index` is **not**
deterministic. Re-running `index` on the same two unchanged artifacts and
re-querying gives a different SARIF each time — 50.2 MB, 50.4 MB, 50.5 MB,
54.4 MB across four indexes of identical inputs. The differences are in *which*
code flows get rendered, not in what is found.

Measured across those four indexes, on identical inputs and one binary:

| counter | four indexes | stable? |
| --- | --- | :-: |
| sources / sinks matched | 211 / 2 084 | ✔ |
| `tainted-path` results | 304 | ✔ |
| flows with a global-heap step (any / primary) | 235 / 227 | ✔ |
| per-plugin sweep table (§4) | identical | ✔ |
| results from the `response-ratelimiting` source | 185 | ✔ |
| max code flows on one result | 57 | ✔ |
| distinct source→sink pairs | 174, 175, 175, 175 | ✘ |
| `locations[0]` ≠ last step (§5.4.1) | 104, 101, 103, 105 | ✘ |
| `sinkCallee` absent from last step | 21, 20, 20, 19 | ✘ |
| total code-flow steps on the largest result | 684, 627, 627, 627 | ✘ |

**So: endpoint-level counts are reproducible; flow-rendering counts are not.**
Everything this report leans on — the 304, the 77%, the 183-result fan-out, the
plugin sweep, every §5.2/§5.10 match count — is in the stable column. The §5.4.1
percentages and §5.8's per-result step totals carry ±2–5 of index noise and
should be read as "≈34%" and "≈630–690", not as exact figures.

This also retires an attribution made earlier in this same write-up. The
684-vs-627 difference between the current and pre-change builds is **not** a
build difference: three of this build's own four indexes give 627. Nothing in
§5.8's numbers is attributable to the recent commits. The same applies to the
distinct-pairs counter and to §5.4.1's row-1 and row-2 movements.

If you need to compare two builds, compare the stable counters, or hold the
index fixed and vary only the model — which is how §2.4, §5.1 and §5.2.1 were
measured, and why those comparisons are exact.

`ctadl import` takes a directory and imports the tree under it into one program,
using that directory as the `require` root. It does **not** follow symlinks, so
the tree has to be copied. Module names must match what the source writes, and
Kong writes `require "kong.pdk.request"` — hence a root whose child is `kong/`.

Counting results: the SARIF contains three rule ids, and only one of them is a
finding. `C0001.tainted-path` is the flow (304 in the base run);
`C0003.taint-source` and `C0004.taint-sink` are `informational` endpoint markers
(422 and 3 862), which is why the file has 4 588 `results` entries. Filter on
`ruleId == "C0001.tainted-path"` before counting anything.

Two further traps in counting, both of which produced wrong numbers in earlier
runs of this report:

- A `tainted-path` result may have `kind: "open"` — *"ran to completion and found
  no source-to-sink flow"* — with no `codeFlows` at all. It is the whole content
  of the §3.2 Group A SARIF. Filter on `kind == "fail"` as well as on `ruleId`.
- A result carries a **list** of `codeFlows`, up to 57 of them (§5.8). "Steps in
  the flow" is `codeFlows[0].threadFlows[0].locations`; summing across the whole
  list instead is what produced the retracted "627-step flow". Decide which of
  the two you mean and say so — this report now gives both.

To attribute results to the `Variable(name)` sources, pair
`kong-model-vars.json5` with a sinks-only copy of `kong-model.json5` (delete the
two source generators) rather than diffing two full runs.

Set `RUST_LOG=warn` when working with `Variable(name)`: the two failure modes
(`named local "X" not found in <fn>` and `local %LN has no versioned vertex in
<fn>`) are only visible there.

Pointing `import` at the repo root instead works and pulls in `spec/` and `t/`
— see §5.0 for why you probably do not want that.

`IMPORT_FORMAT_VERSION` is now `4`; a store imported by an older build must be
re-imported, not just re-indexed.

To attribute a moved number to a code change rather than to drift — which is how
§5.0, §5.4.1 and §5.8 were settled this run — build the earlier commit into its
own target directory and give it its own store, then run both on the identical
model:

```bash
git worktree add /tmp/wt-old <old-commit>
(cd /tmp/wt-old && CARGO_TARGET_DIR=/tmp/target-old cargo build --release)
/tmp/target-old/release/ctadl --store ./store-old import ...   # version 3 store
```

Note the current model cannot be run against a pre-`qualified-id` build at all —
it matches 0 sources there (§5.2.1), which is itself a clean confirmation of the
fix. Generate a `names`-spelled copy for the comparison:
`sed -e 's/"qualified-ids"/names/g' -e 's/"qualified-id"/name/g'`.

---

## 7. Suggested follow-ups

1. **Contain `$globals` fan-out** (§5.8) — field-sensitivity on the global
   object, or a cap on saturating propagation through the global formal. This
   stays at the top on this run's numbers: 77% of all reported flows ride the
   global heap, one source owns 60% of the results, 57 results each ship **57
   alternative code flows** that differ only in which unrelated subsystem the
   heap was threaded through (~36 000 code-flow locations between them), and the
   artifact produces a plausible-looking `io.popen` command-injection report.
   Precision, not recall, is the limiting factor at whole-tree scale.
   *(Separable and much cheaper: those 57 flows share a byte-identical 11-step
   prefix and differ only in the access path on the sink vertex — one per config
   field. Collapsing prefix-identical flows into one, or capping alternatives per
   result, removes ~39 000 code-flow locations from the SARIF without touching
   the analysis. It fixes no false positive, but it is what makes the remaining
   ones readable.)*
2. **Seed `Variable(name)` on the copy-class representative** (§5.7). Same
   `rep_of` mechanism that fixed §5.1. Takes the feature from 59% to near-100% on
   its intended idiom and is the only thing standing between the model and the
   whole `ngx.var` source class.
3. **Render each result's own endpoints, at both ends** (§5.4, §5.4.1). Widened
   on 2026-07-26, because the defect is not source-specific: emit the seeding
   local as a step for `Variable`-sourced findings; stop rendering one shared
   code flow for results with different `sourceCallee`s *or* different
   `sinkCallee`s (33 and 12 results); and anchor `locations[0]` on the sink step
   rather than an intermediate call (104 results, 34% of the report). A finding
   whose source or sink line is absent — or wrong — cannot be triaged. This is
   now the largest *unfixed* item after (1).
4. ~~**Populate `program_method_qualified_ids` in the Lua arm of `ModelIndex`**~~
   — **DELIVERED (2026-07-26b)**, along with the Lua row in the frontend table,
   the `%anonN` warning, and a regression case (`Lua:qualified-id-flow`) that
   pins it. See §5.2.1. The checked-in model has been rewritten onto it (§2.4).
   *(The original follow-up 4, making `unqualified-id` an error rather than a
   universal match, was **delivered by #79** — see §5.2. This slot has now been
   closed twice.)*
5. Name a function assigned to a table field (`_M.uuid = uuid.generate_v4`,
   `["/acme"] = { POST = function(self) … }`) after that field rather than
   `%anonN` (§5.9). Also the largest remaining source of
   unresolved-but-internal callees. This rose in priority now that (4) has
   landed: `qualified-id` is the documented way to pin one function, and
   `%anonN` is the one class of function it cannot pin — the frontend now carries
   a per-function simple name (the §5.10 `functions` column) that a field-derived
   name would slot straight into.
6. Reconcile `find: "variables"` / `"fields"` between schema and loader (§5.3).
7. Consider indexing call-edge names alongside definitions in `models/json.rs`,
   so external callees can be modeled without a shim. Now that a call site
   carries the API's qualified name, this would let a model name
   `ngx.req.get_headers` directly.
8. Ship `examples/kong/kong-externals.lua` as the seed of a Lua/OpenResty
   `default-index.jsonl` once (7) lands.
9. Add the Lua frontend to whatever CI gate keeps the other frontends compiling
   **and behaving**. Rebasing onto `4d90113` needed no hand-porting, so the
   compile half is no longer the pressing part — but #79 added a `signature_match`
   field to three of the four `ModelIndex` arms and silently skipped Lua (§5.2.1),
   which compiles fine and fails only at match time. A per-frontend smoke model
   asserting that each documented constraint matches something would have caught
   it; a build gate would not.
   The `Lua:qualified-id-flow` case added this run is one instance of exactly
   that idea, and the argument generalizes: §5.10 was a second silent match-time
   gap (`protect%1`), also invisible to compilation, also found only by running
   the model. What is wanted is one case per documented constraint per frontend,
   asserting a non-empty match — not one case per bug after the fact.
10. **Make `index` deterministic**, or document that it is not (§6.1). Indexing
   the same two unchanged artifacts twice and re-querying gives a different SARIF
   — same findings, different rendered code flows, files differing by up to 4 MB.
   It costs nothing in correctness and a great deal in evaluation: half the
   counters this report has tracked across four runs turn out to be unable to
   detect change at all, and this run initially mis-attributed three of them to
   the four commits before the re-indexing test caught it. A `--seed`, or a sort
   before whatever parallel step is responsible, would make every future
   comparison in this document trustworthy. Cheap, and it gates the credibility
   of follow-ups 1 and 3 — both of which will be judged on exactly these
   counters.

**Status after this run.** Delivered: the original 4 (`unqualified-id`, by #79)
and the current 4 (`qualified-id` on Lua, by this branch), plus §5.1's wildcard
sink ports and §5.2's name collisions from earlier runs. Outstanding, in order:
(1) `$globals` fan-out, (3) endpoint rendering, (2) copy-class seeding, then
(5)–(9), with (10) as a prerequisite for measuring any of them credibly. The top
two have been top two for three runs and neither has moved.
