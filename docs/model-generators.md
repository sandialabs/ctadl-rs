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

> A Java `native` method is *not* one of these cases. When the library
> implementing it is indexed alongside the app, CTADL links the two itself and
> maps the arguments across the JNI ABI; you do not need a propagation model for
> it. See [The JNI bridge](jni.md).

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

**Which phase consumes what.** The split is not cosmetic; a model given to the wrong command
does nothing:

| model | consumed by | why |
| --- | --- | --- |
| `sources`, `sinks` | `ctadl query` | they define the search, which runs over a finished index |
| `propagation`, `bridge`, `access_paths` | `ctadl index` | they become facts the index fixpoint consumes, and it is fixed by the time a query runs |

Both commands accept `--models`, and passing one file to both is normal. Each now says out loud
what it is ignoring rather than dropping it in silence: `ctadl query` warns that it is skipping
the propagation and bridging models in your file, and `ctadl index` warns that it is skipping
the source/sink ones. Adding a `bridge` or a `propagation` after indexing means re-running
`ctadl index`.

CTADL also ships **built-in default propagation models**, in
`ctadl-ascent/src/models/defaults/`. Exactly one file is loaded per import,
chosen by the frontend's method table:

| Frontend | File |
| --- | --- |
| dex, apk, jvm, jar | `java-index.jsonl` |
| pcode | `native-index.jsonl` |
| lua | `lua-index.jsonl` |
| flowy | *(none)* |

Your `--models` files are unioned on top of the selected default. Pass
`--no-default-models` to `ctadl index` or `ctadl go` to suppress it, leaving
`--models` as the complete set. Note the defaults are **propagation only** —
CTADL ships no default sources or sinks. You can scaffold a starter file with:

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
array. Blank lines and lines starting with `//` are skipped, so a `.jsonl` model
file can carry commentary; the built-in defaults use this to record why an entry
is (or deliberately is not) there. Skipped lines do not consume a generator
index — the index that error messages and `CTADL0004` report counts generators,
not lines.

The regression suite checks every generator in the built-in files against this
schema (`cargo xtask regression --filter models:`, one report entry per file),
so the two cannot drift apart: a keyword added to the loader and used in a
shipped default has to land in the schema as well, or the check fails.

---

## 4. Anatomy of a model generator

```jsonc
{
  "find":  "methods",          // WHAT kind of program element to match
  "in":    { /* scope */ },    // WHICH IMPORTS to look in (optional)
  "where": [ /* constraints */ ], // WHICH ones (all constraints must hold)
  "model": { /* ... */ }        // HOW to model the matches
}
```

> **Unrecognized keys are a hard error**, here and inside `model` and `bridge`, for the same
> reason unrecognized *constraints* are (below): the JSON schema's `additionalProperties: false`
> is checked by your editor, never at load time, so a misspelled key would otherwise be dropped
> silently and the generator would do something other than what it says.

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

### `in` — which imports

A project can index several artifacts at once — an APK and the `.so` implementing its
native methods, say. `in` scopes a generator to some of them; omit it and the generator
applies to every import.

```jsonc
{ "find": "methods", "in": { "language":  "pcode" },                        "where": [ … ], "model": { … } }
{ "find": "methods", "in": { "languages": ["jvm", "jar", "dex", "apk"] },   "where": [ … ], "model": { … } }
{ "find": "methods", "in": { "import":    "app_dex" },                      "where": [ … ], "model": { … } }
```

| key | meaning |
| --- | --- |
| `language` | One artifact language: `jvm`, `jar`, `dex`, `apk`, `c`, `lua`, `pcode`, `flowy`. |
| `languages` | A non-empty list of them; admits an import whose language is any one. |
| `import` | The import's name, as given to `ctadl import --name`. |

Keys within one `in` block are ANDed: `{"languages": ["dex","apk"], "import": "app"}` is that
import, and only if it is one of those two languages. An absent key leaves that dimension
unconstrained.

`language` is exactly the one-element case of `languages`; both spellings exist because
`["dex"]` reads worse than `"dex"` for the common case. Giving **both** keys in one block is an
error rather than a union — a reader cannot tell which was meant — and so is `"languages": []`,
which would match nothing quietly.

The plural is not sugar. A language *boundary* has a set on each side, not a language: "the
Java side" is `jvm`/`jar`/`dex`/`apk` and "the native side" is `pcode`. A model scoped to
`"dex"` matches nothing the day the same app is imported from a `.jar` — no error, just an
analysis with less in it.

Without `in`, every model file is matched against every import, which is usually harmless (a
libc name matches nothing in a Dex artifact) and occasionally not (`main`, `read`, and `get`
exist on more than one side of a boundary). It is also what lets one file carry libc models for
the binary and framework models for the app.

> This is deliberately *not* how the built-in default model files are selected. Those are keyed
> on the frontend's method table, which cannot tell `dex` from `apk` from `jar`; `in` can, and
> that distinction matters because those frontends number parameters differently.

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
`propagation`, `modes`, `bridge`, `access_paths`, `forward_self`. These are covered in
[§7](#7-the-model-what-to-attach).

> **What's actually wired up.** As of this branch the loader consumes
> **`sources`**, **`sinks`**, **`propagation`**, **`bridge`**, and **`access_paths`**.
> `taint`, `modes`, and `forward_self` are defined in the schema but are **not yet
> implemented** — a model using them parses but has no effect (the lone
> `forward_self` line in the jadx defaults is currently inert). They're
> documented below for completeness and forward compatibility.
>
> `forward_call` used to be listed here. It was the same-program special case of `bridge`, was
> never implemented, and has been **removed** from the schema; use `bridge`.

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
| lua | `lib.reader.read`, `kong.pdk.request.get_headers` | The module-qualified IR name: the file's `require` path plus the definition's own dotted name. A **global** root names itself instead, so `function ngx.req.get_headers()` is `ngx.req.get_headers` in whatever file defines it. A Lua function has only this one name, so `qualified-id` and a fully-spelled `name` accept the same string — but `name` *also* accepts the bare name the definition writes (`read`), which `qualified-id` deliberately does not. That bare name comes from the frontend, not from splitting the id: two same-named functions in one module have ids `m.f` and `m.f%1`, and `name: "^f$"` matches both. A single file imported on its own is the import root, so its module name is empty and its ids are just its bare names. |

An id that names no function in the program matches nothing (it does not match everything).

> **On pcode, one id can select several functions.** Because the id carries no
> address, every imported thunk for the same symbol shares one — a binary with three
> `system` thunks has three functions whose `qualified-id` is `<EXTERNAL>::system`,
> and the constraint selects all three. That is usually what you want (they are the
> same logical callee), but if you need exactly one, spell out the decorated IR id
> instead. On jvm/dex the id is unique, so this does not arise.

> **On lua, a stdlib or cross-module callee matches by name only.** The Lua frontend publishes
> an *externals* column — the names an import calls but never defines — so a model, or a
> `bridge`, can name `os.execute` or a C function bound through a `luaL_Reg` table even though
> the import contains no body for it. An external has no `FunctionData`, though, so the three
> constraints that read one — `has_code`, `number_parameters`, `uses_field` — cannot match it.
> `signature_match` (or `name`) is the supported shape. Note this rules out the otherwise
> natural `has_code: false` for selecting exactly the bodyless callees. The same is true of the
> dex/jvm `ext` entries.

> **On lua, do not use `%anonN` ids.** A function with no name of its own — one
> assigned into a table literal, say `["/acme"] = { POST = function(self) … }` — is
> named `<module>.%anonN` from a counter that runs across the whole *import*, not
> per file. Adding or removing any file renumbers every later one (the same Kong
> handler is `%anon449` importing `kong/` alone and `%anon451` importing the whole
> repo), so an id that works under one import set silently matches nothing under
> another. Pin such a function by its enclosing module plus a `uses_field` or
> `number_parameters` constraint instead, or give it a real name in the source.

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

Ports can be extended into **access paths** to reach *through* a value. An access
path is a sequence of segments, each introduced by a `.`:

```
path    := segment*                      -- "" is the empty path (the bare port)
segment := '.' ( offset | symbol )       -- a leading '.' is required before every segment
offset  := '[' ('+'|'-')? DIGIT+ ']'     -- decimal i64, nothing else
symbol  := one or more chars, up to the next UNESCAPED '.',
           and NOT beginning with an unescaped '['
escape  := '\' ANY  ->  the literal char   ( '\.' '\[' '\\' )
```

This is the same grammar the IR dump and the fact store use, so a path you see in
an IR dump can be pasted into a port and mean the same thing. A malformed path is
a **load-time error** naming the port, not a silently-mutated path.

- **Symbols** — `.deref` is the pointee. `Argument(0).deref` is "what argument 0
  points at", essential for C where data arrives through out-parameters:
  `read(fd, buf, n)` taints `Argument(1).deref`, not `Argument(1)`. A dot-path
  like `.foo.bar` reaches named fields of a structured value.
- **Offsets** — `.[n]` is a numeric offset (pointer arithmetic), a *different*
  kind of segment from a field with a numeric-looking name. `Argument(1).[8].deref`
  really does name `Offset(8), Symbol("deref")`, which is what the pcode frontend
  emits for `*(p + 8)`.
- **Escapes** — a field name that *begins* with `[` must be written `\[`, or it
  would be read as an offset. `.` and `\` inside a name are escaped the same way,
  so a C field literally named `a.b` is `.a\.b`. In JSON these need a second level
  of escaping: `"Argument(0).\\[]"`.

Which spelling a frontend actually emits for an array element differs, and the
port must match:

| Frontend | Array element segment | Port spelling |
| --- | --- | --- |
| dex, jvm | `Symbol("[]")` | `Argument(0).\[]` — JSON `"Argument(0).\\[]"` |
| lua, C (tree-sitter) | `Symbol("[_elem_]")`, and `Symbol("[3]")` for a literal index | `Argument(0).\[_elem_]`, `Argument(0).\[3]` |
| pcode | a real `Offset` | `Argument(0).[8]` |

Note the pcode row is the only one where `.[n]` (unescaped) is right; on lua and
the tree-sitter C frontend a source-level `t[3]` is the *symbol* `[3]`, not offset 3.

There is no wildcard segment. `.*` is a field literally named `*`, and `.[*]` is a
load error — use the sink-side `wildcard` flag or a `saturating` source instead.

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

#### A port pair is a prefix substitution, not a filter

This is the part that misreads. An `input → output` pair does **not** mean "taint
sitting exactly at `input` appears exactly at `output`". It means: taint at
`input_var.p`, for **any** `p` extending the input path, lands at the output path
followed by whatever was left of `p`. The two paths are a *level shift*.

Take `local u = id(t)`, with taint stored at `t.f`:

| model on `id` | means | taint at `t.f` lands at |
| --- | --- | --- |
| `Argument(0)` → `Return` | `u.X ← t.X` | `u.f` — the suffix rides along |
| `Argument(0).f` → `Return` | `u.X ← t.f.X` | **`u`** — the port *consumed* the `.f` |
| `Argument(0)` → `Return.f` | `u.f.X ← t.X` | `u.f.f` — the port *added* a level |
| `Argument(0).f` → `Return.f` | `u.f.X ← t.f.X` | `u.f` — consumed and re-added |

So a longer input path **unwraps**: `Argument(0).f → Return` puts a field's taint
on the bare returned value, one level *above* where a first reading expects it.
A longer output path **wraps**. Neither one narrows what the model matches.

The practical consequence is for testing: probe at the level the port *predicts*,
not at one fixed depth. A fixture that always reads `u.f` scores nothing on rows
2 and 3 above while the models are doing exactly what they say. The worked matrix
is `ctadl-ascent/tests/tnt/port_*.tnt` (flowy) and
`ctadl-ascent/tests/port_semantics.rs` (Lua).

One asymmetry worth knowing: a **sink** port materializes over the paths reachable
at its vertex, so a sink written `Return` also seeds `Return.f`. Reading a whole
object observes taint in its fields; a sink port cannot express "the object itself,
but not its fields".

### `modes` *(schema-only, not yet implemented)*

Analysis directives. Defined value: `["skip-analysis"]` — don't analyze the body
of the matched function (rely on the model alone). Not currently consumed by the
loader.

### `bridge`

Connects a callee matched in one program to its implementation in another, with an explicit
argument correspondence. This is the declarative counterpart of [the JNI bridge](jni.md), for
the boundaries no built-in pass can see: a Lua script calling a C function registered in a
`luaL_Reg` table, an implementation bound through `RegisterNatives` rather than by symbol name,
a call through a table field or a `dlsym`'d pointer.

```jsonc
{
  "find":  "methods",                 // side A, the call side; the only mode
  "in":    { "language": "lua" },
  "where": [{ "constraint": "signature_match", "name": "mylib.add" }],
  "model": { "bridge": {
    "to": {                           // side B, the implementation side
      "in":    { "language": "pcode" },
      "where": [{ "constraint": "signature_match", "name": "l_add" }]
    },
    "arguments": [                    // the port map
      { "from": "Argument(0)", "to": "Argument(0).stack.[1]",  "direction": "in"  },
      { "from": "Argument(1)", "to": "Argument(0).stack.[2]",  "direction": "in"  },
      { "from": "Return",      "to": "Argument(0).stack.[-1]", "direction": "out" }
    ]
  }}
}
```

Side A is the generator's own `find`/`where`/`in`, so every matching feature applies to it
verbatim. Side B's `to` block is a match block of the same shape. `find` must be `methods`: a
bridge attaches *inside* the matched method, which gives the stub a summary that every call
site of it composes with — one edge per bridged method, not one per call. (`find: callsites` is
rejected outright rather than ignored; see the note at the end of this section.)

**`arguments` is the feature, not a refinement.** Read one entry as *"the caller's `from`
vertex is the callee's `to` vertex"*. Ports use the [port grammar](#6-ports-and-access-paths)
and name **slots**, not declaration order, so a map is correct only for the frontend the method
was observed through — Dex numbers parameters by register (a `long` or `double` consumes two)
while JVM numbers them by argument position. `Argument(*)` is rejected: a wildcard has no
correspondent on the other side.

Three things are handled for you:

- **Globals** are mapped unconditionally and are not writable. Without them heap flows do not
  cross the boundary at all — taint in through one function, held in a native global, out
  through another is the case that fails.
- **`Return`** means the *normal* return. A Java method's exception return is deliberately
  never mapped.
- **Omitting `arguments`** gives the identity mapping over the arity the two sides share, plus
  `Return`. On a bodyless stub that arity is often zero, so you will get a warning saying only
  the return and globals cross; write the map.

`direction` is `in` | `out` | `both`, defaulting to `both` — which is how the engine treats an
ordinary call.

#### Pairing, and saying so when it goes wrong

Every failure mode of a bridge — a `where` that matches nothing, a scope that admits no import,
a wrong slot, a wrong path escaping — produces an analysis with *fewer flows*, not an error,
which is indistinguishable from a clean app. Three things push back:

| key | where | default | fires when |
| --- | --- | --- | --- |
| `on-unmatched` | the generator | `warn` | side A matched nothing anywhere in the project |
| `on-unmatched` | the `to` block | `warn` | side A matched, side B matched nothing |
| `on-ambiguous` | inside `bridge` | `warn` | the pairing is not one-to-one |

All three take `ignore` | `warn` | `error`. There is no `cardinality` key: **every** matched A
is paired with **every** matched B, so a bridge that matches three callees and two
implementations emits six. `on-ambiguous` reports that rather than restricting it; set it to
`ignore` when you know the fan-out is what you want, and narrow the `where` constraints when it
is not.

"Unmatched" means *not matched anywhere in the project*, not per import: a side that matches in
one artifact and not another is matched. A scope naming an import that is not in the project
matches nothing, which is the same condition and gets the same warning.

Reporting on side B is skipped entirely when side A matched nothing — there is no point saying
the implementation is missing when the thing it would implement is missing too. That is why
`on-unmatched: "ignore"` on the generator also silences the `to` side.

Finally, `ctadl index` logs a per-generator count line unconditionally, at `info`:

```
bridge my-models.jsonl:0: 1 from, 1 to, 1 pair(s) bridged
```

A bridge-only generator declares no endpoint, so it has no entry in the endpoint statistics and
can never raise `CTADL0004`. This line is the only surface it appears on, and it is the one
thing that catches a *mis*-paired bridge — wrong slot, wrong path, wrong function matched —
which warn-on-empty cannot see.

#### Composition past the seam is exact-match only

A bridge routes each port through a temporary standing for the callee's parameter, and every
path it writes is **literal**: `from` on the caller's formal, `to` on the temporary. That is
what keeps several ports on one callee index from aliasing each other (the Lua map above puts
three ports on `Argument(0)`), and it means the paths register themselves — a bridge adds
nothing to the path set by hand.

What it does *not* do is compose arbitrarily deeply with the callee's own behaviour. A pathful
port composes with the callee's summary where the summary's endpoint lands on exactly that path
or on a prefix of it. A summary that lands *deeper* produces a residue path that is in neither
the program-path nor the model-path bucket, and **the flow is dropped, silently.**

Two consequences worth internalizing:

- The Lua example above has a precondition the syntax does not show: `l_add`'s behaviour must
  *also* be modelled, by hand-written `propagation` summaries, in the port map's vocabulary at
  the port map's paths. A native frontend derives offset-only paths, so the *derived* summary
  of `l_add` can never mention `.stack.[1]`, and `[-1]` (top of stack) has no static offset at
  all. Without those models the bridge delivers taint to a place nothing reads.
- When you know the residues the callee's summary produces, declare them with
  [`access_paths`](#access_paths). That is the escape hatch, and it is the only one.

#### Interaction with the built-in JNI bridge

A user bridge over a pair the [built-in JNI pass](jni.md) also links **double-bridges** it: two
sites, duplicated flows. If you are writing one for a `Java_…`-named method — which you should
not normally need to — pass `--no-jni-bridge` to `ctadl index` so exactly one mechanism is in
play. That is also what makes an A/B measurement between the two meaningful.

#### Index time only

A bridge creates `call` facts, which the index fixpoint consumes, so it takes effect at
`ctadl index` and **not** at `ctadl query`. Passing one to `query` gets you a warning saying
so. Adding a native artifact late means re-running `ctadl index`.

> **`find: callsites` with a `bridge` is a load error**, not an ignored key. `call` is an input
> relation: indirect and virtual dispatch are resolved *inside* the fixpoint, so a pass running
> at fact time sees only the statically emitted call rows — precisely not the sites (a call
> through a table field, a `dlsym`'d pointer, a virtual dispatch) a callsite bridge would exist
> for. It would have bridged the easy sites, missed the motivating ones, and reported success.

### `access_paths`

A list of access paths to register with the indexer, written with the same grammar as a port's
trailing path:

```jsonc
{ "find": "methods", "model": { "access_paths": [".next.next.next"] } }
```

These need no matching — they are paths that occur *nowhere* in the program's own code, which
is the whole reason a human has to name them. The indexer gates every path-extending
propagation step on membership in a set built from the paths the program and the models
mention, so a path nobody writes is a flow that silently does not happen.

The motivating case is composition across a `bridge` (above): the residues a callee's derived
summary produces are fixpoint *output*, so nothing can enumerate them, but an author who knows
the callee's shape can declare them here. Three fields deep into a linked list is the other
one. Use it sparingly: the path set is what bounds the analysis, and every entry widens it.

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
| Join a callee in one artifact to its implementation in another | `methods` | `bridge` |
| Apply a model to only some of a project's imports | any | `in` (a generator key, not a model key) |
| Register a composed access path the code never writes | `methods` | `access_paths` |
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
