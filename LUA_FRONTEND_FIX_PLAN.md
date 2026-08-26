# CTADL Lua frontend: APISIX CVE-2022-29266 defect confirmation and fix plan -- DO-NOT-MERGE

Scope: item 1 of the CVE reproducibility report — *"Lua table-selected callback
resolution"* — the one item `JVM_FRONTEND_FIX_PLAN.md` left open as "a missing
analysis rather than a decoder table". This plan reproduces it on the real
artifact, decomposes it into the defects that actually block it, and lays out
the repair.

The report attributes the whole failure to one cause (function values are not
tracked through tables). That is one of **six** independent defects on the
path, and it is not the first one that bites. Three of the six are in the index
and query engines, not the Lua frontend, and are language-neutral — they
reproduce just as cleanly in C. Two of those three were found while assessing
the repair for the first, and one of them (D4b) was why that repair, on its own,
would not have closed the chain.

Those three engine defects — D4, D4b, D4c — were carried by
**`ENGINE_INDIRECT_CALL_FIX_PLAN.md`** and **have since landed** (`56728caf`,
"Query finds sinks under contexts"). They are summarized here only as far as
this chain needs them.

## Status

| phase | defect | state |
| --- | --- | --- |
| 1 | D1 — call sites allocate two return slots | **outstanding** |
| 2 | D4 / D4b / D4c — engine | **done** — see `ENGINE_INDIRECT_CALL_FIX_PLAN.md` |
| 3 | D2 — `function T.f` binds no function value | **outstanding** |
| 4 | D3 — field-name dispatch for table-selected callbacks | **outstanding** |
| 5 | D5, D6 — `require` / builtin name resolution | **outstanding** |

The Lua frontend is untouched on this branch — `ctadl-ascent/src/languages/lua/mod.rs`
has not changed since `e41fdc6a` — so every reproduction and IR quotation below
still holds verbatim, and the artifact still yields `resolvent: 0`. The engine
work of Phase 2 remains **unobservable on APISIX** until D2 and D3 land, which
is why it was gated on micro-shapes and benchmarked on Kong and baksmali
instead. Phases 1, 3, 4 and 5 — all of them frontend-only — are what remains
between this tree and the acceptance criterion.

## Reproduction

APISIX 2.13.0 source, `ctadl` built from this tree at `0.1.2`:

```
$ curl -sLO https://github.com/apache/apisix/archive/refs/tags/2.13.0.tar.gz
$ ctadl import --store "$S" --language lua --name apisix apisix-2.13.0/apisix
lua: parsed 194 .lua file(s)
warning: lua: 25 construction site(s) had an unresolved metatable; instances fall back to name-based dispatch
warning: lua: 742 call site(s) had a callee that could not be resolved to a qualified name
'apisix': imported 1336 function(s)
```

194 files / 1336 functions, matching the report exactly. Indexing succeeds.
Querying with a source on `plugins.jwt-auth.fetch_jwt_token`'s return and a sink
on `apisix.core.response.exit` argument 1 matches 1 source and 75 sinks and
reports **no source-to-sink flow** — the report's symptom, on the real artifact.

The relevant IR (`ctadl inspect --dump-ir --function run_plugin`) is quoted
throughout; the line numbers in this plan are for this tree.

## The chain that has to close

`apisix/plugin.lua:743`:

```lua
local phase_func = plugins[i][phase]
if phase_func then
    plugin_run = true
    local code, body = phase_func(plugins[i + 1], api_ctx)
    if code or body then
        ...
        core.response.exit(code, body)      -- the sink
```

and `apisix/plugins/jwt-auth.lua:354`:

```lua
function _M.rewrite(conf, ctx)
    ...
    return 401, {message = jwt_obj.reason}  -- the tainted value, second return
```

For CTADL to report, five things must hold:

1. `phase_func` must resolve to `plugins.jwt-auth.rewrite` (and its 23 siblings).
2. `rewrite`'s **second** return value must reach `body` at the call site. This
   is a *return* across an unresolved call, not a summary instantiation: the
   tainted value originates inside `rewrite` (at the modelled source
   `fetch_jwt_token`), so no summary of `rewrite` describes it.
3. `body` must reach `core.response.exit` **inside `run_plugin`'s own frame**.
4. `run_plugin`'s callers must supply whatever drives (1) — they pass the phase
   name as a string literal (`init.lua:443`, `init.lua:463`, `plugin.lua:801`).
5. None of this may connect *every* indirect call to *every* plugin.

Today (2) is silently discarded by the frontend (D1), and (1) has no mechanism
at all. The two engine-side blockers are gone: a resolved indirect call now
yields real call and return edges (D4b) and a flow consumed in the frame that
holds the call is no longer dropped (D4). Each remaining defect is reproduced
below on a self-contained case.

## Defects

### D1. Every call site allocates exactly two return slots, so multi-value returns past the first are dropped

`ctadl-ascent/src/languages/lua/mod.rs:2164`:

```rust
let result = self.fresh_temp();
let err = self.fresh_temp();
let rets: ThinVec<VariableRef> = ThinVec::from(vec![result.clone(), err]);
```

Every `CallAssign` gets `[normal, exception]` regardless of the callee, and
`lower_local_decl` / `lower_assign` bind any extra left-hand target to `nil`
(`mod.rs:1596`, `mod.rs:1640`).

```lua
local function two() return "clean", source() end
local function main()
  local a, b = two()
  sink(b)                                   -- tainted in Lua; clean in CTADL
end
```

```
define main.two() -> 3:
  return <const: "\"clean\"">, %%t0, <const: []>      -- arity is right

define main.main() -> 1:
  %%t0, %%t1 = direct-call main.two()
  assign %a = %%t0
  assign %b = <const: "nil">                          -- the value is gone
```

No flow is reported. This is the same shape as
`local code, body = phase_func(...)`, and in APISIX it is fatal on its own:
`body` — the only tainted operand of the sink — is literally `nil` in the IR:

```
%%t28, %%t29 = funcptr-call %phase_func <indirect-call>(%phase_func, %%t27, @p2)
assign %code = %%t28
assign %body = <const: "nil">                         -- run_plugin, line 741
```

`nightly/tests/lua/multiple-return-flow` does not catch this: its second binding
is the *clean* one, so the test passes for the wrong reason.

The return-slot **indices** are also wrong as a consequence. A callee with
`normal_arity = k` returns at formals `-1 … -k` with the exception slot at
`-(k+1)` (`lower_return`, `mod.rs:1656`), while the call site's `err` sits at
`-2`. For `k ≥ 2` the callee's second return lands in the caller's dead `err`
temp. Over-allocating call-site slots is harmless in both directions
(a Lua exception slot is always `empty_exp()`); under-allocating loses data.

### D2. `function T.f(...)` stores no function value into `T.f`

`lower_stmts`' `function_declaration` arm (`mod.rs:1473`) registers a name for
`local function f` and emits **no statement at all** for the dotted form. The
function exists, correctly qualified, but nothing ties it to the table:

```lua
local M = {}
function M.handler() return source() end
local function run(t) local f = t.handler; sink(f()) end
run(M)
```

```
define main.%chunk() -> 1:
  assign %%t0 = <const: "{}">
  assign %M = %%t0
  %%t1, %%t2 = direct-call main.run(%M)     -- M.handler was never stored

define main.M.handler() -> 2:               -- the function exists, unreachable
```

On APISIX, `plugins.jwt-auth.%chunk` builds `_M` with `version`, `priority`,
`type`, `name`, `schema`, `consumer_schema` — and none of `rewrite`,
`check_schema`, `api`, which are all `function _M.x` declarations.

The anonymous form **does** work (`eval_closure`, `mod.rs:2006`, emits
`Exp::ObjectRef(CallObject::FunctionPtr(..))`, and `codegen/mod.rs:783` turns a
store of one into `call_target_assign`). This is a gap in the named form only.
`T.f = g`, where `g` names an already-defined function, has the same gap: the
right-hand side lowers to an ordinary variable load, not a function value.

### D3. A dynamic index key collapses to `[_elem_]`, which is disjoint from named-field stores

`key_segment` (`mod.rs:1953`) turns a non-literal subscript into the symbol
`[_elem_]`. Symbol segments match only themselves — deliberately, and documented
as the frontend's problem in `ctadl-ascent/src/facts.rs:1496`:

> a frontend that wants two accesses to may-alias (a non-constant subscript and
> the element it might be, say) must spell them the same path — that choice
> belongs to the frontend, not here.

So `t[k]` cannot read anything `t.f = …` wrote, for function values or plain
data:

```lua
local M = {}
M.h = source()
local function run(t, k) return t[k] end
sink(run(M, "h"))                            -- no flow
```

The static-key case is fine — this works today:

```lua
local M = {}
M.h = function(x) return x end
local function run(t, v) local f = t.h; return f(v) end
sink(run(M, source()))                       -- flow found
```

In APISIX both index reads collapse:

```
%%t24 = load @p1.\[_elem_]        -- plugins[i]
%%t25 = load %%t24.\[_elem_]      -- plugins[i][phase]   <- the field name is lost
assign %phase_func = %%t25
```

The array read (`plugins[i]`) is not the problem — `[_elem_]` is used on both
sides of an array. The **field** read is: `phase` holds `"rewrite"`, and nothing
in the IR records that.

There is a second reason the module table cannot carry the answer even if D2 is
fixed: APISIX loads plugins with `pcall(require, "apisix.plugins." .. name)`
(`plugin.lua:104`) and appends them with `core.table.insert`. `required_module`
(`mod.rs:745`) resolves only a literal `require "m"` spelled with the bare name
`require`, and `eval_builtin_call` (`mod.rs:2221`) recognizes `table.insert`
only when it is spelled exactly `table.insert`. Both miss. Any fix that depends
on tracking *which* table `plugins[i]` is will not close this chain; the
resolution has to key on something else. See D5/D6 and "Why field-name keying".

### D4, D4b, D4c. A resolved indirect call was unusable — engine, language-neutral — **fixed** *(see `ENGINE_INDIRECT_CALL_FIX_PLAN.md`)*

Three defects in the index and query engines, reproduced there in full, in Lua
and in C, and repaired there. What this chain needs to know about them:

- **D4** — when the function pointer arrived from a *caller*, resolution yielded a
  `context_assign` tagged with a call string that no rule made usable *in the
  frame that contains the call*, so a flow consumed there was dropped. APISIX has
  exactly that sink placement: `core.response.exit(code, body)` sits inside
  `run_plugin`, the frame holding the indirect call, while the phase string that
  must drive resolution comes from `run_plugin`'s callers. The query engine now
  traverses a contextual assign under a call-string obligation
  (`query_engine/search.rs`, `PathState` / `refine`).
- **D4b** — call resolution produced *summary instantiations and nothing else*, so
  a dynamically resolved callee had no call edge and no return edge, and taint
  crossed the site only for a formal-to-out-formal flow. A `resolved_call`
  relation is now factored out of the two rules that resolve a call, persisted as
  `resolved_call.parquet`, and turned into real call and return edges at query
  time.
- **D4c** — `callee_by_site` was a `HashMap`, not a multimap, so a site with
  several `call` rows kept whichever loaded last. It is now
  `HashMap<PackedInsnSiteId, Vec<FunctionId>>` and the entry edge fans out over
  every target. It matters here because a resolved indirect site is multi-target
  by construction.

Five shapes distinguish them — one indirect call each, differing only in where
taint enters and where it is consumed. All five now report, in C and in Lua,
under both query regimes:

| # | taint crosses the site as | sink | before Phase 2 | now |
| --- | --- | --- | --- | --- |
| 1 | callee summary (arg → ret) | caller of the frame | flow found | flow found |
| 2 | callee summary (arg → ret) | the frame itself | no flow — **D4** | flow found |
| 3 | return out of the callee | the frame itself | no flow — **D4b** | flow found |
| 4 | return out of the callee | caller of the frame | no flow — **D4b** | flow found |
| 5 | argument into the callee | inside the callee | no flow — **D4b** | flow found |

**APISIX is shape 3.** `fetch_jwt_token` is a *direct* call inside
`plugins.jwt-auth.rewrite`, so the source vertex sits in `rewrite`'s frame and
has to leave on `rewrite`'s second return, through `plugin.lua:743`'s
`funcptr-call`. No summary of `rewrite` carries it; the site needs a **return
edge**, which is what D4b's repair supplies.

But on the real artifact the machinery is still idle, and stays idle until D2
and D3 land:

```
$ RUST_LOG=debug ctadl index apisix
hybrid inlining: critical_summary: 0.12 (171/1386), resolvent: 0,
  context_assign: 0.00 (0/144170), context_locals: 0.00 (0/46855), context_summary: 0
```

Zero resolvents: `phase_func` is read off `[_elem_]` and no function value was
ever stored where it could be read (D2 + D3), so rule 2.1 never fires. This is
why the engine work was a separate plan gated against micro-shapes, and why it
cannot be *demonstrated* on APISIX until Phases 3 and 4 land.

### D5. `require` resolution is literal-only *(secondary)*

`required_module` (`mod.rs:745`) requires the callee to be the bare identifier
`require` and the argument to be a string literal. `pcall(require, name)`,
`local req = require`, and `require(prefix .. name)` all fall through to an
opaque external. Not on the critical path once D3 lands, but it is why the
module table cannot be tracked, and it costs precision everywhere else.

### D6. Builtin recognition is textual *(secondary)*

`eval_builtin_call` matches on source text, so `core.table.insert`,
`local insert = table.insert`, and `tab.insert` are all missed even though
`apisix/core/table.lua:39` is literally `insert = table.insert`. Recognition
should run after alias resolution (`qname_of`), or the aliases should be
modelled in `lua-index.jsonl`.

## Fix plan

Phase 2 is **done**. What remains is Phases 1, 3, 4 and 5, all frontend-only,
in that order: D1 must land first or nothing downstream is observable on APISIX
(`body` is literally `nil` in the IR), and Phase 4's dispatch has nothing to
resolve to until Phase 3 puts function values into the tables.

### Phase 1 — D1: correct return-slot allocation

1. Add an arity pre-pass. `collect_functions` already registers every
   `FuncEntry` before any lowering, and `max_return_arity` (`mod.rs:1258`) is
   purely syntactic. Compute it for every entry up front into
   `arity_by_fidx: IndexVec<FunctionIdx, usize>` and `arity_by_name:
   HashMap<String, usize>` instead of only inside `lower_function`.
2. Replace `eval_call`'s fixed `rets` with `eval_call_multi(node, blk, want)`
   returning `Vec<Exp>`; `eval_call` becomes `want = 1`. Allocate
   `max(want, callee_arity_if_known) + 1` slots, exception slot last.
3. Thread `want` from the three multi-value contexts: `lower_local_decl` and
   `lower_assign` (when the call is the last right-hand expression and there are
   more targets than expressions), and `lower_return` (`return f()` propagates
   all of `f`'s returns; `return (f())` truncates to one — the
   `parenthesized_expression` node already distinguishes them).
4. With `arity_by_name` in hand, fix the companion imprecision in
   `max_return_arity`: `function g() return f() end` currently declares arity 1
   however many values `f` returns.
5. Optional, same mechanism: `g(f())` expands `f`'s returns into `g`'s trailing
   arguments.

Gate: the D1 case above reports a flow; `multiple-return-flow` still passes with
its clean binding still clean.

### Phase 2 — D4, D4b, D4c: make a resolved indirect call usable — **done**

Landed in `56728caf` under **`ENGINE_INDIRECT_CALL_FIX_PLAN.md`**, whose
implementation summary carries the gates, tests, benchmarks and the two design
points the plan left open. In outline, what shipped: a `resolved_call` relation
factored out of the two rules that resolve a call, `context_assign.parquet` and
`resolved_call.parquet` persisted (`INDEX_FORMAT_VERSION` 3 → 4), a query engine
that traverses contextual assigns under a call-string obligation and builds
**call and return edges** from `resolved_call`, `callee_by_site` as a multimap,
and `--index-context-collapse` (default off) as the A/B baseline. All five shapes
report in C and Lua under both query regimes; index time and RSS improved; the
one cost is query-state multiplication, most of it D4c's fan-out.

What this chain gets from it, and what Phase 4 still owes it:

- The **return edge** at `plugin.lua:743`. Phase 4's dispatch resolves
  `phase_func` to `plugins.jwt-auth.rewrite`, and that resolution now yields a
  real return edge rather than only a summary instantiation that does not
  describe the flow.
- **D4c's multimap fix**, since a field-dispatched site is multi-target by
  construction.
- `resolved_call` is what "`phase_func` resolving to ≤ 24 targets" in Acceptance
  is read off — the *realized* fan-out, as opposed to Phase 4's
  `--max-field-resolvents`, which caps what dispatch codegen emits in the first
  place. A cap applied at the engine would gate the query-side edges but could
  not retract a summary instantiation the fixpoint has already derived.
- **Phase 4 must satisfy the engine's anchoring rule for the entry edge.** The
  entry edge inherits the site's argument convention, and a `FuncPtrCall` carries
  the callee value as actual argument 0 (`languages/lua/mod.rs:2169`), which lines
  up with a closure's leading `%self` but is off by one for a named
  `function _M.f(...)`. A `LuaField` dispatch row must therefore be anchored at
  Phase 4 option (a)'s plain-argument statement, not the self-prefixed one — the
  engine cannot paper over it. Returns sit at negative indices and are
  unaffected, which is why the APISIX chain itself is insensitive to this but the
  entry edge is not.

### Phase 3 — D2: named function definitions are function values

In `lower_stmts`' `function_declaration` arm, when the name node is a
`dot_index_expression` or `method_index_expression`, emit the store the
anonymous form already emits:

```rust
// `function T.f(...)` binds a first-class function value into T.f, exactly as
// `T.f = function(...)` does. Without this the definition is unreachable from
// the table it is written into.
let target = self.eval_lvalue(name, blk);
let qname  = self.program[fidx].name.clone();
self.assign_to(blk, target, Exp::ObjectRef(CallObject::FunctionPtr(qname.into())));
```

Also make `eval_expr` yield `Exp::ObjectRef(CallObject::FunctionPtr(q))` for a
bare name that `qname_of` resolves to a defined function, so `T.f = g` and
`callbacks[#callbacks+1] = g` carry the value too.

Note the ordering constraint: `local function f` is declared and aliased in the
same arm, and the store must be emitted into the *current* block, so this arm
now needs `cur` — it does not have it today.

Gate: the D2 case reports; `plugins.jwt-auth.%chunk` shows
`store %_M.rewrite := ptr<plugins.jwt-auth.rewrite>`.

### Phase 4 — D3: field-name dispatch for table-selected callbacks

This is the report's actual ask. The design keys resolution on the **field
name**, resolved as a value through the engine's existing call-target
propagation, rather than on the identity of the table.

**Why field-name keying.** Table identity is unavailable in APISIX and would
stay unavailable after D2/D5: the plugin table is produced by
`pcall(require, "apisix.plugins." .. name)` from a computed string. The field
name, by contrast, is a string literal at every `run_plugin` call site, and it
is exactly the discriminator the report asks for. Fan-out is bounded and
meaningful — in APISIX 2.13.0, 24 functions are declared `function X.rewrite`,
24 `access`, 22 `log`, 12 `header_filter`. Twenty-four edges out of 1336
functions is the plugin set, not "every indirect call to every plugin".

**Mechanism.** Four pieces, each mirroring something that already exists.

1. *Carry the key with the value.* At an index read `x = t[k]`, alongside the
   existing `[_elem_]` load, emit `store %x.%key := <k>`. `%key` is a reserved
   segment no Lua identifier can produce. Because it is an ordinary path
   suffix, it survives copies (`local phase_func = plugins[i][phase]`), field
   stores, and call boundaries for free, through the same
   `call_target_assign_like` closure that already carries `LuaClass` tags. Emit
   it for dynamic keys, and for literal keys whose name is a known callback
   field name.

2. *Make a field name a first-class object.* Add `CallObject::LuaField(Symbol)`
   next to `LuaClass` (`ctadl-ir/src/mir/call.rs:127`) and
   `fx::CallTargetObject::LuaField` / `fx::CallDispatchKey::LuaField` in the
   facts schema. A string literal whose content is a known callback field name
   lowers to a temp carrying both the constant and the tag — the pattern
   `eval_setmetatable` (`mod.rs:2275`) already uses:

   ```rust
   assign %t = <const: "\"rewrite\"">, ObjectRef(LuaField("rewrite"))
   ```

   Restricting to names that some definition actually binds keeps the fact
   count proportional to the callback surface, not to every string in the
   program.

3. *A field CHA.* `build_vmt` (`mod.rs:970`) already emits a
   `functions: Vec<(simple, fq)>` column for every collected function, where
   `simple` is exactly `def_name` — the field name. Add a
   `field_functions: Vec<(Symbol, Symbol)>` column restricted to definitions
   whose name node was dotted/method form (plus the `T.f = <function>`
   assignments D2 adds), and emit from `emit_callee_resolvents`
   (`codegen/mod.rs:1007`):

   ```
   callee_resolvents(LuaField(name), CallDispatchKey::LuaField, fq)
   ```

4. *Dispatch at the call site.* Emit
   `callee_info(site, FlowVertex(callee_var, callee_path ++ .%key), LuaField)`.
   Reuse `CallResolutionStrategy::Mixed` exactly as the `LuaCall` arm does
   (`codegen/mod.rs:535`, `:599`): a singleton resolvent becomes a direct `call`
   edge, an empty one emits nothing, and a larger set is deferred to
   `callee_info`. The deferred set is resolved by the engine, which — since
   Phase 2 — turns it into a `resolved_call` row and hence a real call/return
   edge. That last step is what the APISIX chain travels on.

**The argument-convention problem, and what to do about it.** `eval_call`
inserts the callee value as actual argument 0 for a `FuncPtrCall`
(`mod.rs:2169`), because an anonymous closure declares a leading `%self`
parameter through which it reads its upvalues (`lower_function`, `mod.rs:1200`).
A named `function _M.rewrite(conf, ctx)` has no such parameter, so resolving it
through a `FuncPtrCall`'s argument list shifts every argument by one.

Three ways out:

- **(a) Two statements, recommended for this round.** At a table-selected call
  site emit the existing `FuncPtrCall` (self-prefixed args, resolves closures)
  *and* a second `CallAssign` with plain args carrying the `LuaField` dispatch,
  both writing the same `rets`. Correct for both callee shapes, no IR change, no
  model-visible signature change; costs one extra statement per such site (166
  `FuncPtrCall` sites in all of APISIX). The SARIF formatter already
  deduplicates code-flow steps by source region, so the pair collapses in
  traces.
- **(b) Give CHA-reachable named functions a leading `%self`.** Uniform, but it
  renumbers the arguments of most module functions, which breaks every model
  written as `Argument(0)` — including the sink model for
  `apisix.core.response.exit`. Rejected.
- **(c) Move the closure self-value out of the positional argument list** into a
  reserved formal slot, the way `GLOBALS_INDEX` already works. This is the right
  long-term answer and would delete the whole problem, but it touches the IR and
  every frontend. Follow-up, not this round.

Note for triage: even under (a) with the misalignment left in place, the APISIX
flow would still be found, because it travels on the **return** and the return
slots are unaffected. The alignment fix is correctness, not a blocker. That it
travels on the return is also why Phase 2 had to supply a *return edge* at the
site: neither the argument convention nor `jwt-auth.rewrite`'s summary carries
it.

**Precision guards.**

- Only field names bound to a function definition are eligible. `check_schema`
  is the worst case in APISIX at 71 targets.
- Log per-site fan-out at `trace`, and count sites that exceed a configurable
  cap (`--max-field-resolvents`, suggested default 64); over the cap, drop the
  dispatch row rather than emit the union, and report the count at index time
  the way the existing `unresolved_call_count` warning does.
- The dispatch is additive: it never removes an edge the funcptr data-flow path
  already found.

Gate: every acceptance criterion in "Acceptance" below.

### Phase 5 — D5, D6: name resolution *(after the gate is green)*

- `required_module`: accept `require` reached through an alias
  (`local req = require`), `pcall(require, "m")` / `xpcall`, and
  `require(C .. name)` where `C` is a chunk-level string constant — resolving to
  the module *prefix* when the suffix is dynamic.
- `eval_builtin_call`: match on `qname_of` rather than source text so
  `core.table.insert` and `local insert = table.insert` are recognized; or model
  the aliases in `lua-index.jsonl`.

## Acceptance

The report's criterion, unchanged: *the existing models produce a path through
`jwt-auth.rewrite` to `apisix.core.response.exit` without a summary specifically
connecting `<indirect-call>` to that handler.*

Concretely, after all four phases:

```
$ ctadl import --store "$S" --language lua --name apisix apisix-2.13.0/apisix
$ ctadl index --store "$S" --models .../index.jsonl apisix apisix
$ ctadl query --store "$S" --models .../query.json5 -o "$S/results.sarif" apisix
```

must report a `C0001.tainted-path` result whose code flow contains
`plugins.jwt-auth.rewrite` and terminates at
`apisix.core.response.exit` argument 1, with:

- no model naming `<indirect-call>`;
- `phase_func` resolving to ≤ 24 targets at the `rewrite` site, all of them
  `*.rewrite` (read off `resolved_call`);
- the code flow's step out of `plugins.jwt-auth.rewrite` being a *return* at
  `plugin.lua:743`'s call instruction — the D4b edge, not a summary step;
- the import warning counts for unresolved callees no worse than today's 742.

## Tests

New nightly cases under `nightly/tests/lua/`, each a source/sink pair with its
`-query.json`:

| case | defect | shape |
| --- | --- | --- |
| `multi-return-second-flow` | D1 | `local a, b = f()` where `b` is the tainted return |
| `return-call-propagates` | D1 | `return f()` forwarding two values |
| `table-field-named-function-flow` | D2 | `function M.h()` read back as `t.h` |
| `function-name-as-value-flow` | D2 | `M.h = g` where `g` is `local function g` |
| `dynamic-key-callback-flow` | D3 | `t[k]` with `k` a literal passed in from a caller |
| `plugin-registry-flow` | D1–D3 | the full APISIX shape, ~30 lines (below) |
| `dynamic-key-wrong-name-no-flow` | D3 | negative: a different field name must not connect |

None of these exist yet. The D4/D4b/D4c cases — the five micro-shapes in Lua and
C, and the multi-target dispatch case — landed with
`ENGINE_INDIRECT_CALL_FIX_PLAN.md` and are in the tree
(`nightly/tests/lua/caller-supplied-callback-flow`,
`resolved-callee-{source,source-return,sink}-flow`, `multi-target-dispatch-flow`,
and `nightly/tests/c/funcptrcallee{frame,source,sink}.c`).

The last positive case is the whole chain in miniature and is the one to write
first — it fails today at three separate points, all of them frontend-side:

```lua
local function source() return io.read() end
local function sink(x) print(x) end

local plugin = {}
function plugin.rewrite(ctx)
  return 401, source()
end

local registry = { plugin }

local function run_phase(phase)
  for i = 1, #registry do
    local handler = registry[i][phase]
    if handler then
      local code, body = handler(nil)
      sink(body)
    end
  end
end

run_phase("rewrite")
```

Its taint originates inside the callback, so it is shape 3 and travels on the
D4b return edge — which now exists. Before Phase 4 completes it is still a useful
*IR* test — assert the return-slot allocation, the `FunctionPtr` store and the
`.%key` store are all present — and a flow test once the dispatch lands.

Unit tests in `languages/lua/mod.rs`'s test module (which imports from strings,
so they are cheap):

- a `function T.f` declaration emits a `FunctionPtr` store into `T.f`;
- an index read with a dynamic key emits the `.%key` store;
- `build_vmt` puts dotted definitions in `field_functions` and `local function`
  definitions out of it;
- call-site `rets` length tracks the callee's declared arity.

Engine-level unit tests (`index_engine`, `facts::schema`, `query_engine::search`)
landed with `ENGINE_INDIRECT_CALL_FIX_PLAN.md`.

Finally, add the APISIX 2.13.0 tree to the import corpus as a non-regression
import test (194 files, no external toolchain needed — `--frontend lua` already
runs without one).

## Risks

- **Phase 2's residual engine risks** — query-state multiplication from the
  contexts (measured: +742 % states on Kong, +19 % on baksmali, invisible in
  wall-clock) and the un-mitigated recall risk of the return-side context check —
  are carried by `ENGINE_INDIRECT_CALL_FIX_PLAN.md`. Phase 4's dispatch is the
  first thing that will make those contexts live on a Lua artifact of APISIX's
  size, so re-measure `CTADL_QUERY_SIZES` when it lands.
- **Field-name fan-out.** Bounded by the cap, but a program that names every
  handler `run` will see wide unions. The cap plus the per-site trace log makes
  this visible rather than silent.
- **Facts schema change.** `CallTargetObject::LuaField` /
  `CallDispatchKey::LuaField` are persisted; the parquet schema needs updating
  and old stores need re-indexing. Phase 2 already spent the 3 → 4 bump, so
  Phase 4 needs its own: `INDEX_FORMAT_VERSION` 4 → 5 (`project.rs:147`, and the
  assertion in `ctadl-ascent/tests/cli.rs`).
- **Extra statement per table-selected call site** under option (a). Small
  (166 sites in APISIX) and reversible once option (c) lands.

## Verification transcript

Everything asserted above was produced from this tree with
`target/release/ctadl` at `0.1.2`; the minimal cases are reproduced verbatim in
the defect sections and each is a few lines of Lua or C. The APISIX artifact is
`https://github.com/apache/apisix/archive/refs/tags/2.13.0.tar.gz`, imported
from its `apisix/` subdirectory.

Captured before Phase 2 landed, and still current for the Lua-side claims: no
frontend change has been made since, so the import counts, the IR quotations and
`resolvent: 0` all still hold. The engine-side reproductions (the five shapes in
Lua and C, and D4c under both query regimes) and their post-fix numbers are
recorded in `ENGINE_INDIRECT_CALL_FIX_PLAN.md`; the `hybrid inlining` line below
is the pre-fix format and now also reports `resolved_call`.

- Import reproduces exactly — 194 files, 25 metatable warnings, 742 unresolved
  callees, 1336 functions. The query (source on
  `plugins.jwt-auth.fetch_jwt_token`'s `Return`, sink on
  `apisix.core.response.exit` `Argument(1)`) matches **1 source and 75 sinks**
  and reports `C0001.tainted-path` with no flow.
- `RUST_LOG=debug ctadl index apisix` reports `resolvent: 0`,
  `context_assign: 0.00 (0/144170)`, `context_summary: 0` — the hybrid-inlining
  machinery is idle on this artifact, and stays idle until D2 and D3 land.
- `run_plugin`'s IR is unchanged from the quotations above, including
  `assign %body = <const: "nil">` and the two `[_elem_]` loads;
  `plugins.jwt-auth.rewrite` calls `fetch_jwt_token` **directly** and returns
  `401, %%tN` — the shape-3 configuration.
