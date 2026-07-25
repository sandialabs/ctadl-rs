# CTADL Model Generators

The **model generator** is CTADL's declarative language for telling the taint
analyzer things it can't discover on its own: which functions produce tainted
data, which functions consume it dangerously, and how data flows through code
that the analyzer can't see (library calls, native functions, stubs).

A model-generator file is JSON/JSON5/JSONL that conforms to
[`ctadl-model-generator.schema.json`](../ctadl-ascent/src/models/ctadl-model-generator.schema.json).
This document explains the schema and the situations you'd reach for each part.

---

## 1. Why model generators exist

CTADL is a *compositional* taint analyzer: it computes a summary for each
function and stitches summaries together across calls. That works well for code
CTADL can see. But real programs constantly call code CTADL *can't* analyze:

- **Standard library / framework functions** — `String.toString()`, `strcpy`,
  `getenv`. There's no bytecode/pcode to analyze, or analyzing it every time
  would be wasteful.
- **Sources of untrusted input** — `recv`, `getenv`, an HTTP request handler.
  Nothing in the code marks these as "attacker-controlled"; *you* have to.
- **Dangerous sinks** — `system`, `execve`, a SQL `execute`. Again, the code
  doesn't know these are security-sensitive.

Model generators let you patch all of this from the outside. A generator is a
rule: **find** some set of program elements, optionally filter them with
**where** constraints, and attach a **model** describing their taint behavior.

---

## 2. Where they fit in the pipeline

The pipeline is **import → index → query**. Models are loaded with `--models`
(repeatable) on the commands that consume them:

```bash
# Propagation models are most useful at index time (they become function summaries)
ctadl index my-app --models libc-propagations.jsonl

# Source/sink models are most useful at query time (they define what to search for)
ctadl query my-app --models sources-and-sinks.json5 --output results.sarif

# One-shot form
ctadl go my-app /path/to/app.apk --models my-models.json
```

CTADL also ships **built-in default models** that are always loaded, e.g. the
JVM/jadx defaults and the C/pcode defaults
(`ctadl-ascent/src/languages/{jadx,pcode}/default-index.jsonl`). Your `--models`
files are unioned on top of these. You can scaffold a starter file with:

```bash
ctadl init-model model.json5
```

---

## 3. File format

The top-level object has one key:

```jsonc
{
  "model_generators": [ /* the list of rules — this is the main event */ ]
}
```

Each element of `model_generators` is one rule. For large model sets CTADL also
accepts **JSONL** (`.jsonl`): one model-generator object per line, streamed
efficiently. The built-in defaults use this format:

```jsonl
{"find":"methods","where":[...],"model":{"propagation":[...]}}
{"find":"methods","where":[...],"model":{"sinks":[...]}}
```

A JSONL line has exactly the shape of one element of the `model_generators`
array.

---

## 4. Anatomy of a model generator

```jsonc
{
  "find":  "methods",          // WHAT kind of program element to match
  "where": [ /* constraints */ ], // WHICH ones (all constraints must hold)
  "model": { /* ... */ }        // HOW to model the matches
}
```

### `find` — what to match

| Value        | Matches                                                        |
| ------------ | ------------------------------------------------------------- |
| `methods`    | Functions / methods (the common case).                        |
| `callsites`  | Individual call *sites* — a specific call, not the callee everywhere. |
| `variables`  | Variables. *(declared in the schema but not yet handled by the loader.)* |
| `fields`     | Fields of objects/structs. *(declared in the schema but not yet handled by the loader.)* |

> **Implementation note.** The loader
> ([`models/json.rs`](../ctadl-ascent/src/models/json.rs)) currently only
> branches on `methods` and `callsites`; a `find` of `variables` or `fields`
> raises a parse error today. They're reserved in the schema.

### `where` — which ones

An array of **constraints**. All constraints in the array must match (implicit
AND); combine alternatives with the `any_of` / `all_of` / `not` combinators
described below. If `where` is omitted, the rule matches everything of that
`find` kind.

> **Unrecognized constraints are a hard error.** A `where` constraint whose
> `constraint` discriminator the loader does not recognize (including the removed
> `parameter` / `any_parameter`, or a bare integer comparison used outside of
> `number_parameters`) fails model loading rather than being silently skipped.
> Model files are versioned alongside the analyzer, and silently skipping unknown
> constraints previously masked real bugs (e.g. `any_of` behaving as AND). Keep
> your model files in sync with the analyzer you run them against.

> **So are unrecognized *fields*, and constraints with no usable field.** A
> constraint must carry at least one field the loader acts on, and every field it
> carries must be one that constraint honors. `{"constraint": "signature", "name":
> "…"}` — `name` where `pattern` was meant — is rejected on both counts.
>
> This matters more than it looks. A generator's working set starts as *every*
> function, and each constraint narrows it, so a constraint the loader could not
> act on used to leave the generator matching **the whole program**: a model meant
> to mark one method as a source silently became a global source. The
> `CTADL0004` diagnostic only reports generators that matched *nothing*, so
> nothing caught it. This mirrors `additionalProperties: false` in the JSON
> schema, so an editor wired to the `$schema` URL flags the same mistakes as you
> type.

### `model` — how to model them

A `model` object carries one or more of: `sources`, `sinks`, `taint`,
`propagation`, `modes`, `forward_call`, `forward_self`. These are covered in
[§7](#7-the-model-what-to-attach).

> **What's actually wired up.** As of this branch the loader consumes
> **`sources`**, **`sinks`**, and **`propagation`**. `taint`, `modes`,
> `forward_call`, and `forward_self` are defined in the schema but are **not yet
> implemented** — a model using them parses but has no effect (the lone
> `forward_self` line in the jadx defaults is currently inert). They're
> documented below for completeness and forward compatibility.

---

## 5. Where-constraints reference

Each constraint is an object with a `constraint` discriminator plus its own
fields. The main ones:

### Name / signature matching

| `constraint`       | Fields | Matches |
| ------------------ | ------ | ------- |
| `name`             | `pattern` | Regex match against a function/variable **name**. |
| `signature_match`  | `name` / `names`, `parent` / `parents`, `qualified-id` / `qualified-ids` | Structured match on a method signature: a simple name (or list), an owning class (`parent`/`parents`, Java only), or an exact fully-qualified id. This is the workhorse for library modeling. Fields given together are ANDed; a list field ORs within itself. |
| `signature` / `signature_pattern` | `pattern` | Regex against the whole mangled signature, e.g. `".*\\(\\)Ljava/lang/String;"`. |
| `qualified-id` / `qualified-ids` | (on `signature_match`) | **Exact, whole-string** match — not a regex — on the method's fully-qualified id. See below. |

`qualified-id` is the one way to name a single method on every frontend. `name` matches the
bare name, so it cannot separate two same-named methods; `parent`/`parents` can, but the
class hierarchy exists only for the Java VMT. What the id looks like:

| Frontend | `qualified-id` | Notes |
| -------- | -------------- | ----- |
| jvm / dex | `Lcom/example/Foo;->bar(I)V` | The method id, descriptor included. |
| pcode | `Foo::bar`, `<EXTERNAL>::system` | Namespace-qualified but **not** address-qualified, so it is stable across binaries. A function in no namespace has its simple name as its id. The decorated IR id (e.g. `<EXTERNAL>::system@00101008`) also resolves, for models that spell it out verbatim. |

An id that names no function in the program matches nothing (it does not match everything).

> **On pcode, one id can select several functions.** Because the id carries no
> address, every imported thunk for the same symbol shares one — a binary with three
> `system` thunks has three functions whose `qualified-id` is `<EXTERNAL>::system`,
> and the constraint selects all three. That is usually what you want (they are the
> same logical callee), but if you need exactly one, spell out the decorated IR id
> instead. On jvm/dex the id is unique, so this does not arise.

### Structural / nesting

| `constraint`   | Fields | Matches |
| -------------- | ------ | ------- |
| `parent`       | `inner` | **Java-only.** The method's owning class satisfies the `inner` constraint (a `name` regex or `signature_match` equality). On non-Java frontends this warns and matches nothing. |
| `extends`      | `inner` | **Java-only.** A superclass/interface of the method's owning class satisfies `inner`. On non-Java frontends this warns and matches nothing. |
| `uses_field`   | `name` / `names` | The function reads or writes the named field(s) (via an IR `Load`/`Store`). |

### Predicates on the element

| `constraint`        | Fields | Matches |
| ------------------- | ------ | ------- |
| `has_code`          | `value` (bool) | Whether the function has a body (`true`) or is external/stub (`false`). Handy to model only the functions CTADL *can't* see. |
| `number_parameters` | `inner` | Applies an integer comparison (`inner`) to the parameter count. |
| integer compare     | `constraint` is one of `<` `<=` `>` `>=` `!=` `==`, plus `value` | Only valid as the `inner` of `number_parameters`. Used on its own it is an error. |

### Combinators

| `constraint` | Fields | Meaning |
| ------------ | ------ | ------- |
| `any_of`     | `inners` (array) | OR of inner constraints. |
| `all_of`     | `inners` (array) | AND of inner constraints. |
| `not`        | `inner` | Negation. |

### Callsite-only

| `constraint`  | Fields | Matches |
| ------------- | ------ | ------- |
| `in_function` | `inner` | For `find: callsites`, restricts to call sites located *inside* a caller function that satisfies `inner`. Under `find: methods` there is no caller to restrict, so it is a load-time error rather than a no-op. See [§8](#8-callsites-and-in_function). |

---

## 6. Ports and access paths

Sources, sinks, and propagations attach to a **port** — the part of a matched
element that carries taint. A port is a string:

- `Return` — the return value.
- `Argument(n)` — the *n*-th argument (0-based; for instance methods
  `Argument(0)` is typically the receiver / `this`).
- `Argument(*)` — *all* arguments (wildcard).

Ports can be extended into **access paths** to reach *through* a value:

- `.deref` — the pointee. `Argument(0).deref` is "what argument 0 points at",
  essential for C where data arrives through out-parameters:
  `read(fd, buf, n)` taints `Argument(1).deref`, not `Argument(1)`.
- **Fields** — a dot-path like `.foo.bar` reaches named fields of a
  structured value.

You can also target fields via the model's `field` (single) or `fields` (list)
keys instead of inlining them in the port string.

---

## 7. The `model` — what to attach

### `sources`

Marks a port as an *origin* of taint. Each source has a `kind` (a taint label
**you** choose, e.g. `UserInput`, `command_injection`) and a `port`.

```jsonc
// A method whose return value is attacker-controlled
{
  "find": "methods",
  "where": [{ "constraint": "signature_match", "name": "source", "parent": "LSourceSinkExample;" }],
  "model": { "sources": [{ "kind": "UserInput", "port": "Return" }] }
}
```

```jsonc
// libc: recv() writes untrusted bytes through argument 1's pointer
{
  "find": "methods",
  "where": [{ "constraint": "signature_match", "names": ["read","fread","pread","recvmsg"] }],
  "model": { "sources": [{ "port": "Argument(1).deref", "kind": "user_input" }] }
}
```

Source ports carry one extra knob:

- **`saturating`** (source-only, default **false**): mark the source as
  *saturating* — the seeded vertex is tainted **and any subfield/offset read off
  it is tainted too, recursively**. A precise (non-saturating) source taints
  only the exact path you name; a saturating source taints the whole access-path
  subtree beneath it. Set it on a value where "all of it is attacker-controlled"
  regardless of how callers index in. (Rejected on sinks/propagations — it's the
  source-side mirror of the sink-only `wildcard` flag.)

  The motivating case is C's `argv`: `argv[1]` compiles to `*(argv + 1)`, read
  at an *offset* path (`.[8].deref`) that is a sibling of the `.deref` path the
  source is modeled at. Precise, path-matched propagation never connects the two,
  so the flow is lost. Marking the `argv` ports `saturating` taints every offset
  read off the base, reconnecting `argv[i]` to the source:

  ```jsonc
  // main(argc, argv): every element/offset of argv is attacker-controlled
  {
    "find": "methods",
    "where": [{ "constraint": "signature_match", "name": "main" }],
    "model": { "sources": [
      { "port": "Argument(1).deref",       "kind": "argv_input", "saturating": true },
      { "port": "Argument(1).deref.deref", "kind": "argv_input", "saturating": true }
    ]}
  }
  ```

  Saturation is purely internal to the search: reported flows (SARIF) are
  unchanged in shape — a saturating source just reconstructs flows a precise
  source would drop. It applies in the default demand-driven search engine; the
  `CTADL_QUERY_DATALOG=1` closure engine ignores the flag.

### `sinks`

Marks a port as a *dangerous consumer*. A flow from a source of the matching
`kind` to a sink is what the query reports.

```jsonc
// A method whose argument 0 is dangerous
{
  "find": "methods",
  "where": [{ "constraint": "signature_match", "name": "sink", "parent": "LSourceSinkExample;" }],
  "model": { "sinks": [{ "kind": "TaintedData", "port": "Argument(0)" }] }
}
```

```jsonc
// libc: system()/execve() argument 0 is a command-injection sink
{
  "find": "methods",
  "where": [{ "constraint": "signature_match", "names": ["system","popen","execl","execve"] }],
  "model": { "sinks": [{ "port": "Argument(0).deref", "kind": "command_injection" }] }
}
```

Sink ports carry two extra knobs:

- **`wildcard`** (sink-only, default **true**): the sink matches any access-path
  *extension* of the port — `Argument(1)` also catches `Argument(1).deref` and
  `Argument(1).[12].deref`. Set `false` to require the exact path. (Rejected on
  sources/propagations.)
- **`all_fields`** / **`field`** / **`fields`**: sensitize specific (or all)
  fields of the port.

### `taint` *(schema-only, not yet implemented)*

Same shape as `sources`/`sinks` (a list of `kind`+`port`). Intended to place a
taint label directly. Not currently consumed by the loader.

### `propagation`

Describes how taint flows *through* a function — the summary for code CTADL
can't see. Each entry has an `input` port and an `output` port: taint arriving
at `input` appears at `output`.

```jsonc
// StringBuilder.append(x): taint flows from the arg to both the return and the receiver
{
  "find": "methods",
  "where": [{ "constraint": "signature_match", "name": "append",
              "parents": ["Ljava/lang/StringBuffer;","Ljava/lang/StringBuilder;"] }],
  "model": { "propagation": [
    { "input": "Argument(1)", "output": "Return" },
    { "input": "Argument(1)", "output": "Argument(0)" }
  ]}
}
```

```jsonc
// strcpy(dst, src): src flows into dst and the return; also a buffer-overflow sink
{
  "find": "methods",
  "where": [{ "constraint": "signature_match", "names": ["strcpy","strncpy","strcat"] }],
  "model": {
    "sinks": [{ "port": "Argument(1).deref", "kind": "buffer_overflow" }],
    "propagation": [
      { "input": "Argument(1).deref", "output": "Argument(0).deref" },
      { "input": "Argument(1).deref", "output": "Return.deref" }
    ]
  }
}
```

> Propagation is **not** supported with `find: callsites` (it's a whole-function
> summary concept).

### `modes` *(schema-only, not yet implemented)*

Analysis directives. Defined value: `["skip-analysis"]` — don't analyze the body
of the matched function (rely on the model alone). Not currently consumed by the
loader.

### `forward_call` *(schema-only, not yet implemented)*

Model a *callsite* by forwarding it to another function — useful when a call is
dispatched dynamically and you want CTADL to treat it as calling a specific
target. Fields: `receiver` (port of the virtual receiver; omit for a direct
call) and `where` (constraints selecting the callee). Not currently consumed by
the loader.

### `forward_self` *(schema-only, not yet implemented)*

Forward calls to the matched method to *another method on the same object*,
possibly with a different signature. The classic case is Android's
`AsyncTask.execute` really running `doInBackground`:

```jsonc
{
  "find": "methods",
  "where": [
    { "constraint": "signature_match", "name": "execute" },
    { "constraint": "signature_pattern", "pattern": ".*\\(\\[Ljava/lang/Object;\\)Landroid/os/AsyncTask;" }
  ],
  "model": { "forward_self": { "where": [{ "constraint": "signature_match", "name": "doInBackground" }] } }
}
```

Only meaningful for object-oriented languages (e.g. Java).

---

## 8. Callsites and `in_function`

`find: callsites` matches individual **call sites** rather than the callee
everywhere it's used. This lets a model apply only to *some* calls of a function
— for example, "treat `log()` as a sink, but only when it's called from inside
request handlers."

The `in_function` constraint narrows a callsite match by its **caller**: its
`inner` constraint is evaluated against the containing function. When present,
CTADL emits callsite-scoped endpoints for the cross product of matched callees
and matched callers; with no `in_function`, all call sites of the callee match.

```jsonc
// Only calls to `emit` that occur inside a function named `handleRequest`
{
  "find": "callsites",
  "where": [
    { "constraint": "signature_match", "name": "emit" },
    { "constraint": "in_function",
      "inner": { "constraint": "name", "pattern": "handleRequest" } }
  ],
  "model": { "sinks": [{ "kind": "TaintedData", "port": "Argument(0)" }] }
}
```

Use callsite models when the *context* of a call matters — the same callee is
benign in most places but sensitive in a few, or you want to inject a
source/sink at a precise location without over-tainting every call.

---

## 9. Typical use cases at a glance

| Goal | `find` | `model` keys |
| --- | --- | --- |
| Mark a function's output as untrusted input | `methods` | `sources` |
| Mark a whole value (all offsets/fields) as untrusted, e.g. `argv` | `methods` | `sources` + `saturating: true` |
| Flag a dangerous API | `methods` | `sinks` |
| Summarize a library function CTADL can't see | `methods` | `propagation` (often + `sinks`) |
| Both taint-through and dangerous (e.g. `strcpy`) | `methods` | `propagation` + `sinks` |
| Source/sink only in a specific calling context | `callsites` | `sources`/`sinks` + `in_function` |
| Resolve a dynamic dispatch to a concrete target | `callsites` | `forward_call` *(not yet implemented)* |
| Frameworks that call your override indirectly | `methods` | `forward_self` *(not yet implemented)* |
| Skip analyzing a body and trust the model | `methods` | `modes: ["skip-analysis"]` *(not yet implemented)* |

### End-to-end example

A minimal source→sink model (from `nightly/tests/java/source-sink-example.json`):

```jsonc
{
  "model_generators": [
    {
      "find": "methods",
      "where": [{ "constraint": "signature_match", "name": "source", "parent": "LSourceSinkExample;" }],
      "model": { "sources": [{ "kind": "UserInput", "port": "Return" }] }
    },
    {
      "find": "methods",
      "where": [{ "constraint": "signature_match", "name": "sink", "parent": "LSourceSinkExample;" }],
      "model": { "sinks": [{ "kind": "TaintedData", "port": "Argument(0)" }] }
    }
  ]
}
```

Run it:

```bash
ctadl query my-app --models source-sink-example.json --output results.sarif
```

CTADL reports every flow where data returned by `source()` reaches
`Argument(0)` of `sink()`.

---

## 10. Tips

- **`signature_match` with `names`/`parents`** is the most maintainable way to
  model families of library methods — group by owning class.
- **Get the ports right for the language.** Java methods usually taint through
  `Return` / `Argument(n)`; C functions frequently work through `.deref`
  out-parameters.
- **Kinds are your own vocabulary.** A flow is reported when a source's `kind`
  reaches a sink of a matching `kind`. Keep them consistent across your models.
- **Reach for `saturating` when callers index into the source.** If taint should
  cover *every* element/offset/field of a value — `argv`, an array of untrusted
  strings, a struct that's wholly attacker-controlled — mark the source
  `saturating: true` instead of trying to enumerate each path. Leave it off
  (the default) for precise sources like `getenv` so you don't over-taint.
- **Prefer propagation models at `index` time** (they become reusable
  summaries) and **source/sink models at `query` time** (they define the search).
- **Reach for `callsites` + `in_function`** only when call context matters;
  method-level models are simpler and cover most cases.
