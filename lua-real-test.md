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
> 4. **§5.8 (new) 57 of the 304 flows are 627 steps long.**
>
> Three counters drifted slightly and are restated at their measured values
> (§5.8's global-heap share 227→**235**, the `response-ratelimiting` sink count
> 104→**105**, and the `saturating: false` short-flow split 33/11→**31/13**).

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

The dominant remaining problem is not recall but **precision**, and this run
measured it much more sharply than the last one: `$globals` is a single object,
so **235 of the 304 reported flows (77%) ride the global heap**, and a *single*
saturating source in `response-ratelimiting` accounts for **183 results across
105 distinct sinks** — 57 of them via code flows **627 steps long** (§5.8).

Second in line is **triage**: a third of the report points the reviewer at the
wrong line (§5.4).

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
aliases — unchanged from the previous run.

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
functions of §5.9 — the fix in follow-up 4 would recover them.

### 2.2 A shim file is mandatory

Model generators only match functions **defined** in the indexed IR. For a Lua
VMT the loader falls into the generic branch (`models/json.rs`) and indexes
`program_info.program.functions` by name. Unresolved callees (`os.execute`,
`ngx.req.get_headers`, `httpc:request_uri`) get a `FunctionId` and a `call` fact
but **no `FunctionData`** — so no `where` constraint can ever select them.

`examples/kong/kong-externals.lua` supplies stubs for the whole external
boundary (`ngx.*`, `resty.http`, Lua stdlib, Kong PDK). Indexing `kongsrc`
without it and running the same model gives **`Matched 0 sources and 312
sinks`** — zero sources, and the only sinks that survive are Kong's *own*
functions that happen to be named `query`, `render`, `run`, `load`, which the
bare-name colon-call patterns pick up. (The previous run reported `0 sources and
0 sinks`; the 312 is the bare-name collision of §5.0/B showing up at small scale,
not a change in the shim's role.)

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

---

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
has no vertex for it (§5.7). All six numbers reproduce the previous run exactly.

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
  where: [{ constraint: "signature_match", names: ["kong.plugins.acme.api.%anon449"] }],
  model: { sources: [{ kind: "UserInput", port: "Variable(host)", saturating: true }] },
}
```

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
43  kong.plugins.datadog.statsd_logger.new          -> ngx.log
21  kong.db.schema.run_transformation_checks        -> ngx.log
18  kong.plugins.oauth2.access.generate_token       -> ngx.log
12  kong.db.dao.new                                 -> ngx.log
```

Config parsing and schema validation — not request data. `Variable(name)`
matches a bare source name, so a `host` parsed from plugin config is
indistinguishable from a `host` read off the Admin API. The function-scoped form
in Group C is the usable one; the program-wide form is a survey instrument, not
a model.

(The previous run attributed the #2 slot to `kong.db.dao.new` with 44 hits. On
this tree that splits: `statsd_logger.new` 43, `db.dao.new` 12. Both are bare
`new`; the conclusion is unchanged.)

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
| index (+ shim) | 1.8 s / 263 MB | 7.9 s / 973 MB |
| query | 44 s / 1.68 GB | 145 s / 6.85 GB (7.6 GB max RSS) |
| sources / sinks matched | 211 / 2 081 | 244 / 3 632 |
| results | 304 (175 distinct source→sink location pairs) | 437 |

All the shape numbers (functions, assignments, sources, sinks, results) reproduce
the previous run exactly; only wall-clock moved, and in both directions (import
got faster, query slower), so treat the timings as machine noise rather than a
trend.

Both are workable; A is the one to use. B's extra 1 551 sinks are mostly junk —
`spec/` defines hundreds of helper functions named `query`, `request`, `load`,
`run`, `render`, which the colon-call sink patterns (which must be bare names,
§2.1) collide with. Import `kong/` alone unless you specifically want the test
tree analyzed.

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
`request_uri` sink raises the matched-sink count from 2 081 to 2 266 and changes
the answer **not at all**: 304 results and the same 175 distinct source→sink
pairs either way, set-identical. On config B the same edit gives 3 912 sinks
instead of 3 632, and again 437 results either way.

Two claims from the previous run are deleted rather than restated, because
neither reproduces:

- "removing the field ports loses 1 unique flow and gains 5" — on this tree the
  difference is exactly zero, in both configurations;
- "the field-port model made the config-B query cost 603 s / 11.5 GB max RSS,
  and removing the ports cut it 5×" — measured here, the field-port model runs
  **134 s / 6.83 GB peak** against **145 s / 6.85 GB** without, i.e. the
  redundant ports now cost nothing at all. Whatever produced the 5× is gone.

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
  match nothing. Confirmed unchanged in 2026-07-26.
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

#### 5.2.1 `qualified-id` matches nothing on Lua — **NEW (2026-07-26)**

`qualified-id` / `qualified-ids` is the mechanism #79 shipped in its place, and
`docs/model-generators.md:167` calls it *"the one way to name a single method on
every frontend."* On the Lua frontend it names nothing at all:

| generator on the same string | sinks matched |
| --- | ---: |
| `{"constraint": "signature_match", "names": ["kong.request.get_headers"]}` | **15** |
| `{"constraint": "signature_match", "qualified-id": "kong.request.get_headers"}` | **0** |

The cause is a missing arm, not a naming mismatch. `ModelIndex` construction
(`ctadl-ascent/src/models/json.rs:224-305`) builds four lookup tables; the
`VirtualMethodTable::Lua` branch at `:278-292` fills
`program_method_names` and `program_method_signatures` (keyed on both the
trailing simple name and the fully-qualified IR name) and **never touches
`program_method_qualified_ids`**. The `Java` arm fills it, the `Native` arm fills
it, and so does the `Unknown` / `CplusPlus` *fallback* at `:294-305` — so Lua is
the one frontend worse off than an unrecognized language. The frontend table at
`docs/model-generators.md:171` correspondingly has rows for jvm/dex and pcode
and none for Lua.

The failure is silent but safe: `docs/model-generators.md:182` specifies that an
id naming no function matches nothing rather than everything, so this degrades to
zero sinks, not to §5.2's old universal match. Nothing in the checked-in model is
affected — it uses `names` with fully-qualified spellings throughout, which
works, and is why §2.1's qualified naming still holds. But the documented way to
separate two same-named Lua functions does not exist, which leaves `names` and
its trailing-simple-name collisions as the only lever, and that is precisely what
`qualified-id` was introduced to replace.

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

A flow seeded at a named local still has no code-flow step for the local, and it
is **worse than the previous run recorded**. The `acme` SSRF now renders as a
single step:

```
1  sink call-arg(138126, 1) in request_uri      | acme/api.lua:98
```

The intermediate `string.format` step the previous run saw at `:96` is gone too,
so the entire flow is one line. `host` at `:78` — the actual source, and the only
thing that tells you *why* this is a finding — never appears in the code flow,
and no step is labelled `source`.

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
line. Re-confirmed byte-for-byte in 2026-07-26, vertex ids included.

#### 5.4.1 The same defect on the sink side — **NEW (2026-07-26)**

The previous runs recorded this only for sources. It is not source-specific, and
measured across the whole base run it is the report's second-largest problem
after §5.8. Of the 304 results:

| | count | share |
| --- | ---: | ---: |
| result's primary `locations[0]` ≠ last code-flow step's location | **99** | 32% |
| `properties.sinkCallee` not even named in the last code-flow step | **17** | 5% |
| results whose whole code flow is byte-identical to another result's with a *different* `sinkCallee` | **33** (13 distinct flows) | 11% |

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

This run quantified it, and the numbers are much starker than the previous one's:

- **235 of 304 flows (77%)** contain at least one `-32768` step. (2026-07-25 read
  227/75% here; 2026-07-26 measures 235 by the same test — *does any step message
  carry `-32768`* — so eight flows moved across. Everything else about the
  measurement is unchanged.)
- One source — `response-ratelimiting/access.lua:30`, a legitimate
  `kong.client.get_forwarded_ip()` — produces **183 of the 304 results** and
  reaches **105 distinct sink locations**, in `conf_loader` (57),
  `clustering/*` (48), `dns/client` (15), `api/endpoints`, `db/declarative`,
  `llm/drivers`, `aws-lambda`, `oauth2`, `request-transformer`. The run before
  last estimated "~20 distinct sinks" for this source; the real figure is five
  times that.

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
the endpoint helpers, none of which the source touches.

**How long these actually get — NEW (2026-07-26).** The 9-step trace above is a
*representative*, and it badly understates the artifact. The step-count
distribution over the 304 results is bimodal, and the upper mode is a spike:

```
steps    2   3   4   6   7   8   9  10  11  12 …  42  44  48  50 | 627
results 23   3   7  20  10  19  18  36  26   5 …  18   4  10   5 |  57
```

**57 results — 19% of the report — carry a code flow of 627 steps.** All 57 have
the same source (`response-ratelimiting/access.lua:30`, the one above) and the
same sink callee (`ngx.log`), landing in `conf_loader/parse.lua`; they are the 57
`conf_loader` sinks itemized above. A single field, `kong.ctx.plugin.identifier`,
rides the `-32768` formal through the router, the DAOs, `cmd/utils/process_secrets`
and the config loader, and every intermediate hop is emitted as a step:

```
  1 source call-arg(177174, -1) in kong.client.get_forwarded_ip     | access.lua:30
  2 return call-arg(177301, -1) in ...access.execute                | access.lua:72
  3 return call-arg(197986, -32768).kong.ctx.plugin.identifier      | atc.lua:383
  …                            623 further global-heap steps …
627 sink   call-arg(41427, 1).[1] in ngx.log                        | parse.lua:449
```

627 steps is not a flow a reviewer can triage — it is unreadable at any length,
and it is wholly manufactured by the conflation this section describes. It also
means the SARIF is far larger than the result count suggests. This is a second,
independent argument for follow-up 1, and it is the reason that follow-up now
carries a size cost as well as a precision cost.

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
threading, and it is the same shape as the 8→9 hop the previous run flagged in
the `acme` trace.

Quantified the other way: re-running the identical model with `saturating: false`
on every source drops results from **304 to 65** — reproducing both previous runs
exactly, and killing all 57 of the 627-step flows. The oldest run described the
survivors as "tight 2–3 step flows within one or two files"; that is true of 44
of the 65, but the remaining 21 are 7–15 steps, so the long-range flows do not
all come from saturation. (The 44 split 31 two-step / 13 three-step here, against
33/11 on 2026-07-25 — the 44 and 21 totals are identical, two flows shifted
buckets.) Turning saturation off is still not the answer — several §4
flows (`request-transformer`, `ai-prompt-template`) need it, because callers
index into the returned table — but it locates the cost precisely.

### 5.9 `%anonNNN` names renumber with the import set

Re-confirmed exactly. A function assigned to a table field (`["/acme"] = { POST =
function(self) …`) is named `%anonNNN` by a per-import counter. The `acme` POST
handler is `kong.plugins.acme.api.%anon449` importing `kong/` alone and
`kong.plugins.acme.api.%anon451` importing the repo root — verified by dumping
the IR for both artifacts. A model that names one silently matches nothing under
the other. This makes any `%anon` handle unusable in a checked-in model, and it
is the reason the `acme` finding took a function-scoped generator to pin down at
all.

### 5.10 Minor

- No `default-index.jsonl` for Lua. The jadx and pcode defaults are compiled in
  unconditionally (`models/mod.rs`, `try_load_default_models`) regardless of
  artifact language, so a Lua file defining `read`, `system`, or `strcpy`
  silently inherits C models.
- `find: "methods"` with no `where` matches only recognized *class* methods on a
  Lua VMT, not free functions — always supply at least one name constraint.
- Co-indexed-program location attribution was not re-measured. At the 2 programs
  this run uses (shim + tree) every location resolved to the right *file*. That
  is a separate question from §5.4.1, which is about the wrong *line* within the
  right file; no result was ever attributed to the wrong program or artifact.

---

## 6. Reproduction

```bash
cargo build --release

# stage kong/ so it is a child of the require root
mkdir -p /tmp/kongroot && cp -R ../kong/kong /tmp/kongroot/kong

ctadl --store ./store import -l lua -n _shim examples/kong/kong-externals.lua
ctadl --store ./store import -l lua -n kongsrc /tmp/kongroot
ctadl --store ./store index kongonly _shim kongsrc

# call sources only
ctadl --store ./store query kongonly -m examples/kong/kong-model.json5 -o base.sarif

# + named-local sources
ctadl --store ./store query kongonly \
      -m examples/kong/kong-model.json5 \
      -m examples/kong/kong-model-vars.json5 -o vars.sarif
```

`ctadl import` takes a directory and imports the tree under it into one program,
using that directory as the `require` root. It does **not** follow symlinks, so
the tree has to be copied. Module names must match what the source writes, and
Kong writes `require "kong.pdk.request"` — hence a root whose child is `kong/`.

Counting results: the SARIF contains three rule ids, and only one of them is a
finding. `C0001.tainted-path` is the flow (304 in the base run);
`C0003.taint-source` and `C0004.taint-sink` are `informational` endpoint markers
(422 and 3 856), which is why the file has 4 582 `results` entries. Filter on
`ruleId == "C0001.tainted-path"` before counting anything.

To attribute results to the `Variable(name)` sources, pair
`kong-model-vars.json5` with a sinks-only copy of `kong-model.json5` (delete the
two source generators) rather than diffing two full runs.

Set `RUST_LOG=warn` when working with `Variable(name)`: the two failure modes
(`named local "X" not found in <fn>` and `local %LN has no versioned vertex in
<fn>`) are only visible there.

Pointing `import` at the repo root instead works and pulls in `spec/` and `t/`
— see §5.0 for why you probably do not want that.

---

## 7. Suggested follow-ups

1. **Contain `$globals` fan-out** (§5.8) — field-sensitivity on the global
   object, or a cap on saturating propagation through the global formal. This
   stays at the top on this run's numbers: 77% of all reported flows ride the
   global heap, one source owns 60% of the results, that source emits 57 code
   flows of **627 steps** each, and the artifact produces a plausible-looking
   `io.popen` command-injection report. Precision, not recall, is the limiting
   factor at whole-tree scale.
2. **Seed `Variable(name)` on the copy-class representative** (§5.7). Same
   `rep_of` mechanism that fixed §5.1. Takes the feature from 59% to near-100% on
   its intended idiom and is the only thing standing between the model and the
   whole `ngx.var` source class.
3. **Render each result's own endpoints, at both ends** (§5.4, §5.4.1). Widened
   on 2026-07-26, because the defect is not source-specific: emit the seeding
   local as a step for `Variable`-sourced findings; stop rendering one shared
   code flow for results with different `sourceCallee`s *or* different
   `sinkCallee`s (33 results); and anchor `locations[0]` on the sink step rather
   than an intermediate call (99 results, 32% of the report). A finding whose
   source or sink line is absent — or wrong — cannot be triaged.
4. **Populate `program_method_qualified_ids` in the Lua arm of `ModelIndex`**
   (§5.2.1) — `ctadl-ascent/src/models/json.rs:278-292`, keyed on the
   fully-qualified IR name exactly as the arm already keys the names and
   signatures maps. Without it `qualified-id` — which #79 shipped as the
   cross-frontend way to name one method, and which even the `Unknown` fallback
   supports — matches nothing on Lua, leaving no way to disambiguate two
   same-named functions. Add a Lua row to the frontend table at
   `docs/model-generators.md:171` with it.
   *(The original follow-up 4, making `unqualified-id` an error rather than a
   universal match, was **delivered by #79** — see §5.2.)*
5. Name a function assigned to a table field (`_M.uuid = uuid.generate_v4`,
   `["/acme"] = { POST = function(self) … }`) after that field rather than
   `%anonN` (§5.9). Also the largest remaining source of
   unresolved-but-internal callees.
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
