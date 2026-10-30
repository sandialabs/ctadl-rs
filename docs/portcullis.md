# Portcullis, the CTADL model language

A model file tells the taint analyzer what it cannot discover on its own: which functions produce
untrusted data, which consume it dangerously, and how data moves through code it cannot see. This
is the language for saying that.

It is a small Datalog. A file is a list of **rules**; each derives one or more **output** atoms
from a conjunction of atoms over the **input** relations the analyzer already has.

```
// java.net.URL.openConnection(): the receiver flows to the return value, which is a source
source(F::return), propagation(F::return <- F::arg(0)) :-
  fun(F, name = "openConnection", parent = "Ljava/net/URL;");
```

There is no recursion, so no stratification and no fixpoint to reason about: every body reads
built-in relations only, and one pass answers the file.

Files end in `.ctadl` (or `.dl`). The older JSON format is deprecated, and still works by being
migrated to this one as it is loaded; see [§9](#9-migrating-from-the-json-format).

---

## Using model files

### A first model

```
// The value Request.getParameter returns is attacker-controlled.
source(F::return, kind = "UserInput") :-
  fun(F, name = "getParameter", parent = "Ljavax/servlet/ServletRequest;");

// Whatever reaches the first argument of Runtime.exec is a command injection.
sink(F::arg(1), kind = "UserInput") :-
  fun(F, name = "exec", parent = "Ljava/lang/Runtime;");
```

```bash
ctadl import /path/to/app.apk --name my-app
ctadl index my-app
ctadl query my-app --models first.ctadl --output results.sarif
```

CTADL reports every flow from a value `getParameter` returns to the command `exec` runs. A flow is
reported when a source's `kind` reaches a sink of the same `kind`. `ctadl init-model` writes a
starter file with a source, a sink and a propagation to edit.

### Which command reads what

The pipeline is **import → index → query**, and `--models` (repeatable) is accepted by `ctadl
index`, `ctadl query` and `ctadl go`. The two halves of a model file are consumed by different
commands, so a model given only to the wrong one does nothing:

| heads | consumed by | why |
| --- | --- | --- |
| `source`, `sink` | `ctadl query` | they define the search, which runs over a finished index |
| `propagation`, `bridge`, `access_paths`, `dispatch`, the mode heads | `ctadl index` | they become facts the index fixpoint consumes, and it is fixed by the time a query runs |

Passing one file to both commands is normal, and each says what it is ignoring rather than
dropping it silently (see [Phases](#phases)). Adding a `bridge` or a `propagation` after indexing
means re-running `ctadl index`.

```bash
ctadl index my-app --models libc-propagations.ctadl
ctadl query my-app --models sources-and-sinks.ctadl --output results.sarif
ctadl go /path/to/app.apk --name my-app --models my-models.ctadl   # all three steps
```

### The built-in defaults

CTADL ships **propagation-only** defaults in `ctadl-ascent/src/models/defaults/`, one file per
import, chosen by the frontend:

| frontend | file |
| --- | --- |
| dex, apk, jvm, jar | `java-index.ctadl` |
| pcode | `native-index.ctadl` |
| lua | `lua-index.ctadl` |
| cil | `cil-index.ctadl` |
| flowy | *(none)* |

Your `--models` files are added on top, and a mode head of yours outranks a default's (see
[Mode priority](#mode-priority)). `--no-default-models` on `ctadl index` or `ctadl go`
leaves `--models` as the complete set. There are no default sources or sinks: what to search for
is yours to say.

### Checking a model file before you index

Only half of matching needs an index. The rules are evaluated against each imported program's
functions, method table and call sites, which needs the import and nothing else; resolving what
they matched to the index's function ids, fanning endpoints out over call sites and expanding
wildcard sinks is the half that needs the index. Functions are never optimized out, so a source
or sink on a function is fully decided by the first half.

So `ctadl query` without an index, because the project was never indexed or is being indexed
right now, checks the model files instead of failing:

```bash
ctadl import /path/to/app.apk --name my-app
ctadl query my-app --models sources-and-sinks.ctadl --output check.sarif
```

The output is ordinary SARIF, and its notifications say what the rules selected:

| notification | what it says |
| --- | --- |
| `CTADL0008` | There was no index, so this is a model check, and what a model check cannot decide (below). |
| `CTADL0004` | A rule declared a source, sink or propagation and matched nothing, and why: its body matched no function, or its `local` atom matched no local of the functions it did. |
| `CTADL0010` | A bridge rule paired no functions in the programs checked. |
| `CTADL0011` | What each live rule matched: the count, and a few of the names. |
| `CTADL0012` | A model file that could not be read, or a malformed rule. The rest of the file is still checked. |
| `CTADL0100` | The totals. |

Rules are numbered from 0 in file order. The run exits non-zero, because the query asked for
could not be answered, and the built-in defaults are not loaded, as they never are for `ctadl
query`. What a model check cannot tell you:

- **Call-site anchors** count the callees the matched call sites name, not the sites.
- **`arg(_)`** expands over an arity only the index knows; it is counted as one port.
- **A matched function can still be missing from the index.** The query then raises `CTADL0005`.
- **Bridge pair counts.** Whether a bridge paired anything is known; how many calls it covers is
  not.
- **Cross-import bridges.** A bridge's sides can live in two imports, so its verdict means
  something only when the project holds every import the real one will index.

---

## 1. Rules

```
<head>, <head>, … :- <atom>, <atom>, … ;
```

`;` terminates. The body must be satisfied to derive the heads. A rule with no body is a fact:

```
access_paths(".next.next.next");
```

Comments are `//` to end of line, or `/* … */`.

**Variables are uppercase.** That is what keeps them apart from the many built-in relation names,
and it means you never have to remember whether `parent` is a relation or a variable. Reserved,
never a variable or an attribute name: `_`, `return`, `arg`, `param`, `local`, and every built-in
relation name.

`_` is a placeholder that matches anything and binds nothing. Each occurrence is independent.

**Every head variable must be bound in the body.**

---

## 2. Types

- **Primitives** — string (`"…"`), integer, boolean (`true` / `false`).
- **Ports** are first class:

  | written | means |
  | --- | --- |
  | `return` | the return value |
  | `arg(2)` | argument 2, 0-based |
  | `arg(_)` | *every* argument, expanded by the engine over the function's arity |
  | `arg(I)` | argument `I`, where `I` is bound in the body |
  | `local(L)` | local variable `L`, bound in the body by `local(F, L)`; sources and sinks only |
  | `arg(0).foo[2]` | argument 0's `foo` field, then offset 2 |
  | `arg(4)."weird.field[0]"` | a field whose name contains dots and brackets |

  `arg(_)` is expanded from the arity for a function, and from the actual-parameter list for a
  call site. If the arity is unknown you get a warning and no expansion.

  A `local(L)` port hangs off the function `L` is a local of, never a call site or another
  function: `source(F::local(L)) :- fun(F, name = "handle"), local(F, L, name = "buf");`. The
  local is seeded at its first SSA version, its incoming value.

- **Anchored ports** hang a port off a function or a call site: `F::return` is "the return port
  in `F`". The anchor may be a variable or a literal name.
- **Flows** connect two anchored ports: `A -> B`, `A <- B`, `A <-> B`.

### Access paths

A port can be extended into an **access path** that reaches *through* the value:

```
path    := segment*
segment := '.' name           -- a field: .deref, .foo
         | '.' "quoted"       -- a field whose name is not a plain identifier: ."[]", ."a.b"
         | '[' int ']'        -- a numeric offset: [8], [-1]; '.[8]' is the same
```

Paths are always literal, never bound to a variable.

- **`.deref`** is the pointee. C data often arrives through out-parameters: `read(fd, buf, n)`
  taints `arg(1).deref`, not `arg(1)`.
- **Offsets** are pointer arithmetic, a different kind of segment from a field whose name looks
  numeric: `arg(1)[8].deref` is what pcode emits for `*(p + 8)`.
- **Quoted names** take a segment out of the grammar, so a field literally named `a.b` is
  `."a.b"`.

There is no wildcard segment. `.*` is a field literally named `*`. Use a sink's `wildcard` or a
source's `saturating` instead.

Frontends spell an array element differently, and the port must match:

| frontend | port |
| --- | --- |
| dex, jvm | `arg(0)."[]"` |
| cil | `arg(0)."[]"`, and `arg(2).deref` for what a struct receiver or a `ref`/`out` parameter points at |
| lua | `arg(0)."[_elem_]"`, and `arg(0)."[3]"` for the literal key `t[3]` |
| C (tree-sitter) | `arg(0)[3]."[]"` for `a[3]`; `a[0]` and `a[n]` are both `arg(0)."[]"` |
| pcode | `arg(0)[8]` |

On lua a source-level `t[3]` is the *field* `[3]`, not offset 3. The tree-sitter C frontend lowers
a subscript as the pointer arithmetic it is, so one path can hold both: the index is an offset,
and the element read at that address is the field `[]`.

---

## 3. Input relations

These are the program, as the language sees it. Each has some required columns followed by
optional **attributes**; an attribute you do not mention is not constrained.

| relation | columns | attributes |
| --- | --- | --- |
| `fun(F)` | fully qualified function name | `name`, `arity`, `language`, `parent`, `signature`, `has_code`, `import` |
| `name_of(F, N)` | function, one of its names | — |
| `callsig(K)` | a distinct static **call signature** | `name`, `parent`, `signature`, `qualified-id`, `language`, `import` |
| `param(F, Index)` | function, parameter index | — |
| `local(F, L)` | function, one of its declared local variables | `name` |
| `callsite(F, Site)` | the *caller*, the site | `callee_string` |
| `subclass(Sub, Super)` | two classes, direct edge | — |
| `subclass*(Sub, Super)` | reflexive transitive closure | — |
| `subclass+(Sub, Super)` | transitive closure | — |
| `uses_field(F, Fld)` | function, field it loads, stores or updates | — |

Notes worth having:

- **An attribute has at most one value.** `fun(F, name = N)` binds `N` once, and
  `fun(F, name != "x")` and `fun(F, name = N), N != "x"` are the same test.
- `F` is the function's **full name**, the IR's own: `Lcom/example/Foo;->bar(I)V`,
  `<EXTERNAL>::system@00101008`, `os.execute`. To name one function exactly, test it:
  `fun(F), F = "os.execute"`.
- `name` is the **simple** name the frontend gives the function: `bar`, `system`, `execute`.
- `name_of` relates a function to its simple name and to the **other names** a frontend knows it
  by — on pcode, the namespace-qualified `<EXTERNAL>::system` and Ghidra's own spelling before a
  leading `_` is stripped. "Some name of `F` matches" is a join:

  ```
  sink(F::arg(0)) :- name_of(F, N), regex_match(N, "system$");
  source(F::return) :- fun(F), !(name_of(F, N) && N = "getenv");
  ```

  The full name is not a `name_of` row; it is `F`.
- `local` lists every local a function declares, under the name the program text gives it.
  `L` is opaque: it is not an integer, so it cannot index an argument, nor an argument index
  a local. A rule whose `local` atom ran and matched nothing is reported by `CTADL0004` as a
  local not found, naming the local and counting the functions it was asked about.
- `arity`, `has_code`, `param` and `local` read the IR, so they match nothing for a function with no body,
  such as a Lua external. `name` / `signature` / `name_of` and `F` itself are the supported shape
  for a bodyless callee.
- `parent` is the declaring class, where the frontend has a class hierarchy (Java, CIL, Lua).
  Elsewhere it matches nothing, which is fail-closed.
- `callee_string` is the fully qualified callee (joinable with `fun`) or, for an indirect call,
  the variable the program text calls through.
- `callsig` is a **different universe from `fun`**, not a view of it. `fun` is built from the
  method table, which holds implementations; `callsig` is built from the call sites, so it names
  types the program never declares — `Ljava/util/Iterator;` has no implementation row anywhere,
  and only `callsig` can select it. It carries no `arity` or `has_code`: a signature key is
  composed from a call site, not read out of an import's method table. Its `language` and
  `import` are those of the import the call site was read from. Collecting
  the keys costs a pass over every statement, so an import pays for it only when some rule reads
  `callsig`; a rule that reads it against an import collected without them matches nothing.

Every relation name is built in. A file cannot define one, so a name that is not in the table is a
typo and is reported as one.

---

## 4. Attributes

In a body atom, an attribute takes any of:

```
attr = expr      attr != expr     attr < expr
attr > expr      attr <= expr     attr >= expr
attr in { … }
```

`=` with an unbound variable **binds** it; every other operator compares against something already
known. So `fun(F, name = N)` reads out the name and `fun(F, name = "append")` filters on it.

`attr = _` means "has one, whatever it is". That is what makes `!fun(F, parent = _)` say "`F` has
no parent".

A set is a comma-separated list of primitives:

```
fun(F, name = "append", parent in {"Ljava/lang/StringBuffer;", "Ljava/lang/StringBuilder;"})
```

---

## 5. Operators

These sit in atom position but are not relations: they filter, they never generate.

| written | means |
| --- | --- |
| `regex_match(X, "pattern")` | regex match |
| `X in { … }` | membership |
| `X = Y`, `X != Y` | equality |
| `X < Y`, `<=`, `>`, `>=` | ordering (numeric on integers, lexicographic on strings) |
| `A && B`, `A \|\| B`, `(…)` | boolean combination |
| `!atom` | negation |

### Negation

A negated **atom** requires every variable in it to be bound by positive atoms to its left, with
`_` as the way to say "any value at all":

```
source(F::return) :- fun(F), !fun(F, parent = _);       // F has no parent
```

A negated **group** is one existence check over a whole subquery, and the variables local to it
are existentially quantified:

```
source(F::return) :- fun(F), !(name_of(F, N) && regex_match(N, "^get"));
```

That reads "no name of `F` matches `^get`". Written as two separate negations it would mean
something else entirely — `N` would not be shared.

---

## 6. Output relations

The point of a model file is to derive these.

| head | takes | attributes |
| --- | --- | --- |
| `source(<port>)` | an anchored port | `kind`, `saturating` (default `false`) |
| `sink(<port>)` | an anchored port | `kind`, `wildcard` (default `true`) |
| `propagation(<flow>)` | a flow within **one** function | — |
| `bridge(<flow>)` | a flow between **two** functions | — |
| `access_paths("…")` | a literal access path | — |
| `K::dispatch(<flow>)` | a flow within **one** call signature | — |
| `K::dispatch(…)` | — | `resolve` (`"inline"` / `"skip"`), `closure_shaped` |
| `skip_analysis(F)`, `analyze(F)` | a function | `priority` (default `0`) |
| `mem2reg_full(F)`, `mem2reg_window(F)` | a function | `priority` (default `0`) |
| `frame_store(F)`, `frame_update(F)` | a function | `priority` (default `0`) |

- **`kind`** is the taint label. A flow is reported when a source's kind reaches a sink of the
  same kind. It defaults to `"taint"` for both directions, so the smallest useful pair of rules
  works without inventing a vocabulary first.
- **`saturating`** (source only): any access path *extending* the source port is tainted too,
  recursively. Reach for it when callers index into the value. C's `argv` is the motivating
  case: `argv[1]` is read at an offset path (`[8].deref`) that is a sibling of the `.deref` the
  source names, so a precise source never reaches it.

  ```
  source(F::arg(1).deref, kind = "argv_input", saturating = true),
    source(F::arg(1).deref.deref, kind = "argv_input", saturating = true) :-
    fun(F, name = "main");
  ```

  A precise source on a pointer does reach an address computed from it (`q = &p[2]`); what it
  does not reach is the memory at that address. Leave it off for precise sources like `getenv`.
- **`wildcard`** (sink only): the sink matches any access-path extension of the port. On by
  default; set `false` to require the exact path.
- **`dispatch`** attaches a model to a call *signature* rather than a function, and **replaces**
  the site's target set with it: one summary row instead of every implementation CHA can reach.
  Anchor it at a `callsig` key. It says exactly one thing —

  | written | means |
  | --- | --- |
  | `K::dispatch(return <- arg(0))` | a summary flow; the receiver is `arg(0)`, as on a `propagation` |
  | `K::dispatch(resolve = "inline")` | defer to hybrid inlining regardless of the threshold |
  | `K::dispatch(resolve = "inline", return <- arg(0))` | inline, and apply this summary flow too |
  | `K::dispatch(resolve = "skip")` | this call moves nothing; the target set is discarded |
  | `K::dispatch(closure_shaped = true)` | a report-time marker; nothing resolves differently |

  A flow and an attribute in one head is an error, but for `resolve = "inline"`, so "forgot the model" and "meant to discard"
  stay different documents. Several flow heads on one key accumulate into one summary. Because a
  dispatch model takes the callee bodies out of the analysis along with the edges, it belongs on
  methods whose behaviour is fixed by the interface they implement — see the commentary in
  `ctadl-ascent/src/models/defaults/java-index.ctadl`.
- **`skip_analysis`** keeps a function's body out of the index: its signature is lowered and its
  blocks are not, so the function's other models are its *entire* behavior, and the calls its
  body made are gone from the call graph. Without it a `propagation` *adds* to what the body
  derives. A marked function with no `propagation` moves nothing.

  ```
  skip_analysis(F), propagation(F::return <- F::arg(0)) :- fun(F, name = "transfer");
  ```

  | | without `skip_analysis` | with it |
  | --- | --- | --- |
  | summaries from the model | yes | yes |
  | summaries derived from the body | yes | **no** |
  | the body's indirect calls feed hybrid inlining | yes | **no** |
  | flow *through* the body, and through the calls it makes | tracked | **not tracked** |
  | the body's calls appear in the call graph | yes | **no** |
  | call sites *of* the function use the model | yes | yes |

  Reach for it when the model is more accurate than the body (a wrapper or stub whose real
  behavior lives elsewhere), or when the body is degenerate: an unwinder, a dispatcher or an
  assembly thunk whose summary costs the index far more than it contributes. To keep a
  function's call edges and suppress only its dataflow, it is not the tool.

  `F::skip_analysis()` is the same head written with an anchor, and `skip_analysis("…")` names a
  function by its full name — for a native symbol that is the decorated one, such as
  `<EXTERNAL>::abort@00101008`, so binding through `fun(F, name = …)` is usually what you want.
- **`analyze`** is the opposite of `skip_analysis`: the function's body is lowered. It exists to
  override a `skip_analysis` from another rule, such as one in the built-in defaults (see
  [Mode priority](#mode-priority)).
- **`mem2reg_full`** / **`mem2reg_window`** force or forbid full stack-slot promotion for a native
  function, whatever its size and whatever `ctadl index --mem2reg` says. Full promotion gives every
  stack slot its own local, even one whose address escapes; `ctadl index` names each function
  where that may lose flows in its `notifications.sarif`.
- **`frame_store`** / **`frame_update`** coarsen or keep a native function's frame updates,
  whatever `ctadl index --frame` says. With `frame_update` (the default) SSA versions the stack
  frame, so a load sees only the writes that reach it; with `frame_store` every load of a slot sees
  every store to it.

  These six are *mode* heads: each takes one function and is read as that function's import is
  lowered (SSA, then codegen). So a rule with one must be satisfied within one import — its body
  may not split into parts that share no variable, because those are joined only after every
  import is lowered.

### Mode priority

The mode heads come in three pairs of opposites, each choosing one setting of a function: its
body (`skip_analysis` or `analyze`), its stack-slot promotion (`mem2reg_full` or
`mem2reg_window`) and its frame writes (`frame_store` or `frame_update`). Each head may say
`priority = N`, an integer constant; a head that does not say has priority 0.

- Of the modes claimed on one function's setting, the **highest priority wins**, whatever order
  the files and rules are read in.
- Two rules claiming **the same mode** at one priority agree, and that is fine.
- Two rules claiming **different modes** at the winning priority is an error, naming the function
  and both rules, unless a higher claim settles it. `ctadl index` stops on it before lowering the
  import, and a model check (`ctadl query` without an index) reports it.
- The pairs are independent: `skip_analysis(F)` and `mem2reg_full(F)` never conflict.

The built-in defaults claim their modes at `priority = -100`, so any rule of yours that names no
priority outranks them:

```
// The native defaults skip the allocator's body; analyze this one anyway.
analyze(F) :- fun(F, name = "malloc");

// Two files of your own disagree; this one wins.
skip_analysis(F, priority = 10) :- fun(F, name = "parse_config");
```

Each output relation takes exactly one port, flow or path. Several are written as several
comma-separated heads:

```
propagation(F::arg(0) <- F::arg(1)),
  propagation(F::return <- F::arg(1)) :-
  fun(F, name = "append", parent in {"Ljava/lang/StringBuffer;", "Ljava/lang/StringBuilder;"});
```

### Flows

```
F::return <- F::arg(0)      // argument 0 flows to the return value
F::arg(1) -> F::arg(0)      // the same thing, written the other way
F::arg(2).foo <-> F::arg(0).bar   // sugar for two directed flows
```

A **propagation** is one function's summary, so both ports must be anchored at the same function
and neither may be anchored at a call site. A **bridge** is the opposite: its two ports name two
different functions, usually in two different imports.

#### A port pair is a prefix substitution, not a filter

A flow does **not** mean "taint sitting exactly at the input appears exactly at the output". Taint
at the input path *plus any suffix* lands at the output path plus that suffix. The two paths are a
level shift. Take `u = id(t)`, with taint stored at `t.f`:

| model on `id` | means | taint at `t.f` lands at |
| --- | --- | --- |
| `F::return <- F::arg(0)` | `u.X ← t.X` | `u.f`: the suffix rides along |
| `F::return <- F::arg(0).f` | `u.X ← t.f.X` | **`u`**: the port *consumed* the `.f` |
| `F::return.f <- F::arg(0)` | `u.f.X ← t.X` | `u.f.f`: the port *added* a level |
| `F::return.f <- F::arg(0).f` | `u.f.X ← t.f.X` | `u.f`: consumed and re-added |

A longer input path **unwraps** and a longer output path **wraps**; neither narrows what the
model matches. When testing a model, probe at the level the port predicts. The worked matrix is
`ctadl-ascent/tests/tnt/port_*.tnt` (flowy) and `ctadl-ascent/tests/integration/port_semantics.rs`
(Lua).

One asymmetry: a **sink** port covers the paths reachable at its vertex, so a sink on `return`
also sees `return.f`. A sink cannot say "the object itself, but not its fields".

### Bridges

A bridge connects a callee matched in one program to its implementation in another. The **left**
port of each flow is side A, the call side; the **right** is side B, the implementation. The
arrow is the direction: `->` is into the callee, `<-` is out of it, `<->` is both.

```
bridge(F::arg(0) -> G::arg(0).stack[1]),
  bridge(F::arg(1) -> G::arg(0).stack[2]),
  bridge(F::return <- G::arg(0).stack[-1]) :-
  fun(F, name = "mylib.add", language = "lua"),
  fun(G, name = "l_add", language = "pcode");
```

Every bridge head over one `(rule, F, G)` triple accumulates into **one** bridge with several
ports, which is what keeps three ports on one callee argument from aliasing each other. Globals
are mapped unconditionally and are not writable; without them heap flows do not cross the
boundary at all.

This rule mentions two programs, and no single import satisfies its body. That works because the
body is split into connected components, each accumulated as the imports stream past and joined
once at the end — see [§8](#8-how-a-rule-is-evaluated).

A bridge is the declarative counterpart of [the JNI bridge](../README.md#the-jni-bridge), for the
boundaries no built-in pass sees: a Lua script calling a C function registered in a `luaL_Reg`
table, a call through a table field or a `dlsym`'d pointer, or a `RegisterNatives` entry whose
class the JNI pass could not recover.

- **Ports name slots, not declaration order.** A map is right only for the frontend the method
  was observed through: Dex numbers parameters by register (a `long` or `double` takes two), JVM
  by argument position. `arg(_)` is rejected: a wildcard has no correspondent on the other side.
- **`return` is the normal return.** A Java method's exception return is never mapped.
- **Every match pairs with every match.** A rule whose `F` matches three callees and whose `G`
  matches two implementations bridges six pairs. Narrow the atoms when that is not what you want.
- **It attaches inside the callee.** The bridge gives `F` a summary that every call site of it
  uses, which is why a bridge cannot be anchored at a call site: it would name only the
  statically emitted calls, precisely not the indirect and virtual ones a bridge exists for.
- **It is index-time only.** It creates call facts the index consumes; `ctadl query` ignores it
  and says so.

A bridge that goes wrong (a side matching nothing, a wrong slot, a wrong path) produces *fewer
flows*, not an error. `ctadl index` logs one line per bridge rule at `info`, and it is the one
place a *mis*-paired bridge shows:

```
bridge my-models.ctadl:3: 1 from, 1 to, 1 pair(s) bridged
```

**Composition past the seam is exact-match only.** Every path a bridge writes is literal, and a
pathful port composes with the callee's summary only where that summary's endpoint lands on
exactly that path or a prefix of it. A summary that lands deeper leaves a residue the analysis
does not track, and the flow is dropped silently. So the Lua example above has a precondition the
syntax does not show: `l_add` itself needs `propagation` rules at the map's paths, because a
native frontend derives offset-only paths and can never mention `.stack[1]`. When you know the
residues a callee produces, declare them with `access_paths`.

A bridge over a pair the built-in JNI pass also links bridges it twice. For a `Java_…` method,
which should rarely need one, pass `--no-jni-bridge` to `ctadl index`.

### Call-site anchors

A port anchored at a call site denotes the actual at that call, so a source or sink can apply to
some calls of a function and not others:

```
// `emit`'s argument is a sink only when called from inside `handleRequest`.
sink(S::arg(0), kind = "TaintedData") :-
  callsite(C, S, callee_string = G), fun(G, name = "emit"), fun(C, name = "handleRequest");
```

A call site matches by the callee it **names**: a call written against `Base.run` matches a rule
selecting `Base.run`, not an override `Mid.run` it may dispatch to. Only sources and sinks take
a call-site anchor; a propagation and a bridge are whole-function summaries.

### `access_paths`

`access_paths(".next.next.next");` registers a path with the indexer. The index extends a path
only to paths the program and the models mention, so a path nobody writes is a flow that silently
does not happen. Use it for the residues a bridged callee produces, or for a structure deeper than
the code ever spells, and sparingly: the path set is what bounds the analysis.

---

## 7. Modes: what makes a rule executable

A rule is **well-moded** when every operator has all its variables bound by atoms **to its left**.
This is a load-time error, checked strictly left to right:

```
source(F::return) :- regex_match(F, ".*Foo.*"), fun(F);   // error: F is not bound yet
source(F::return) :- fun(F), regex_match(F, ".*Foo.*");   // fine
```

The engine is then free to reorder — it runs filters as soon as their variables exist and picks
the cheapest generator otherwise. Join order is the engine's business; modedness is yours,
because it is the part you can see.

### One parsing wrinkle

`arity < -1` and a mistyped `arity <- 1` differ by one space, and maximal munch takes the arrow.
The two positions are disjoint (arrows only in output arguments, comparisons only in attribute
position), so the parser says "arrow not allowed here" rather than pointing one token past the
mistake. Write the space.

---

## 8. How a rule is evaluated

The body is split into **connected components** by shared variables. Each component is evaluated
against every import as it streams past, and its solutions accumulate; the components are joined
after the import loop ends.

Nearly every rule has one component and notices nothing — its solution set is the union over
imports. The exception is the bridging shape above, whose two sides live in two programs: there,
the cross product after the loop *is* the pairing.

Nothing is retained between imports but the bindings. The relation tables are built per import and
dropped with it, which is what keeps a multi-artifact index streaming rather than resident.

### Phases

The engine is phased. `ctadl index` keeps the `propagation` / `bridge` / `access_paths` /
`dispatch` / mode heads; `ctadl query` keeps the `source` / `sink` ones. Each says how many rules contributed nothing to
it, and a rule contributing at least one head to the running phase is never counted:

```
source(F::return), propagation(F::return <- F::arg(0)) :- fun(F, name = "openConnection");
```

is live in both phases and warned about in neither.

`ctadl index` does match the `source` / `sink` heads too, without reporting on them: a matched
endpoint in a call signature's targets refuses that signature's `dispatch` model, so a file with
both kinds of rule belongs on both commands.

---

## 9. Migrating from the JSON format

`ctadl migrate-models` rewrites a JSON / JSON5 / JSONL model-generator file in this language:

```bash
ctadl migrate-models my-models.jsonl            # writes my-models.ctadl
ctadl migrate-models my-models.jsonl -o -       # to stdout
ctadl migrate-models my-models.jsonl --dry-run  # check only
ctadl migrate-models my-models.jsonl --language dex --language jvm  # a file for these languages
```

Some JSON spellings meant more on some frontends than others: a `name` or `signature` also matched
the function's full name on pcode, Lua, C and flowy, and a `qualified-id` matched pcode's
namespace-qualified name. The migration writes a second rule for each, testing the language. A
generator's `in`, or `--language` for a file that is only ever loaded for some languages (the
shipped defaults are), leaves out the rules no language it can match in needs.

The migration is checked by loading it, so a file that translates but does not check is a failure
rather than a surprise on the next index.

A JSON file passed to `--models` goes through the same translation, in memory, every time it is
loaded: `ctadl index`, `ctadl query`, the model check and `ctadl report` run the migration on the
Portcullis engine, exactly as they run a `.ctadl` file. Each JSON file draws one warning per run that the
format is deprecated, and the migration's own warnings are logged with it. A generator that does
not translate fails the command with the errors below, each naming its generator; the model check
reports it and checks the rest of the file. Diagnostics about a migrated file name the JSON file
and the generator index — in provenance, `CTADL0004`, the dead-rule warning and engine errors —
not a line of a translation the author has never seen.

How the JSON constructs map:

| JSON | Portcullis |
| --- | --- |
| `find: methods` | the subject variable `F`, bound by `fun(F, …)` |
| `find: callsites` | `callsite(C, S, callee_string = F)`, ports anchored at `S` |
| `find: dispatch` | the key variable `K`, bound by `callsig(K, …)` |
| `in: {language, languages, import}` | `fun(F, language = …, import = …)`, or the same on `callsig` |
| `signature_match` name/parent | `fun(F, name = …, parent = …)`, and on pcode, Lua, C and flowy a second rule matching the full name, `F = …` |
| `signature_match` qualified-id | `F = …`, and on pcode a second rule matching the namespace-qualified name through `name_of` |
| `name` / `signature` / `signature_pattern` | `fun(F, name = N), regex_match(N, …)`, and on pcode, Lua, C and flowy a second rule with `regex_match(F, …)` |
| `has_code` | `fun(F, has_code = …)` |
| `number_parameters` | `fun(F, arity <op> …)` |
| `uses_field` | `uses_field(F, …)` |
| `Variable(buf).p` source or sink port | `F::local(L).p` with `local(F, L, name = "buf")`, in a rule of its own so the generator's other ports do not depend on it |
| `parent` / `extends` | `fun(F, parent = P)` and one formula over `P` (`!`, `&&`, `\|\|` for a nested `not` / `all_of` / `any_of`), plus `subclass(P, S)` for `extends`, over `S` |
| `any_of` | **several rules with the same head** — that is how Datalog spells "or" |
| `all_of` | more atoms in one body |
| `not` | `!atom`, or `!(a && b)` when the constraint needs two atoms |
| `in_function` | constraints on `C`, the caller column of `callsite` |
| `model: {propagation: [...]}` under `find: dispatch` | `K::dispatch(<out> <- <in>)` |
| `model: {propagation: []}` | `K::dispatch(resolve = "skip")` — a head is an atom, and there is no empty one |
| `model: {resolve: "inline"}` | `K::dispatch(resolve = "inline")` |
| `model: {resolve: "inline", propagation: [...]}` | `K::dispatch(resolve = "inline", <out> <- <in>)` |
| `model: {closure_shaped: true}` | `K::dispatch(closure_shaped = true)` |
| `model: {modes: ["skip-analysis"]}` | `skip_analysis(F)`, and likewise `mem2reg_full`, `mem2reg_window`, `frame_store`, `frame_update`; within a generator the last of each pair wins. Every migrated mode is at priority 0, so two generators that disagree on a function are an error where the JSON let the last one read win (see [Mode priority](#mode-priority)) |

A migrated file means what the JSON meant, with one exception: where the JSON matcher that used
to run these files had a bug or a limitation, the migration takes Portcullis's meaning rather than
reproducing it. `tests/integration/migration_equivalence.rs` pins each of these differences:

- **Universe.** A generator with no `where`, or a top-level `not`, ranged over the method table
  in the JSON, so it missed a lowered function the table does not name and matched nothing on a
  program with no table (flowy). `fun(F)` is every function.
- **Full names.** `name`, `signature` and `signature_match` also accept a function's full name,
  because Portcullis publishes it as a spelling of every function.
- **Lua classes.** `parent` and `extends` matched nothing on Lua in the JSON; Portcullis's
  `parent` and `subclass` include metatable-recovered classes.
- **`in_function` under `not` or `any_of`.** The JSON matcher narrowed the callee set by the
  caller's constraint there. Portcullis tests the caller, as the constraint says.
- **`find: callsites` anchors at the callee a site names.** The JSON attached the endpoint to
  every matching callee and let the resolved call graph find its sites, so a call naming
  `Base.run` also picked up a model on the override `Mid.run`. `callsite` joins on the static
  `callee_string`. Expressing the resolved form needs a relation over resolved targets — a
  `resolves_to(Site, Callee)` or similar, fed from the call graph rather than the program text
  — which is future work.

A migration says what the JSON means or fails: it never writes a rule that matches more or less.
A generator the format does not allow fails, naming the generator and what is wrong with it.
Keys the format allows and that have no effect (`taint`, `forward_self`, an empty `modes` list, an `on-unmatched` or `on-ambiguous` other than `"error"`) are warnings. These the
format allows and Portcullis cannot say fail the migration, naming the generator:

- A `bridge` with no `arguments` map, or an empty one. The JSON format means an identity map over
  the arity the two sides share, which needs the fact base; an empty map bridges only the
  globals. Write the map.
- `on-unmatched` / `on-ambiguous: "error"`, which fails the run, and a rule cannot.

---

## 10. Errors

Every diagnostic names a file, line and column, and says what to do:

```
> models.ctadl:4:22: 'F' is not bound at this point. An operator can only test variables that an
  atom to its left has already bound; move the atom that binds 'F' earlier in the body.
```

Rule errors accumulate: a file with three bad rules reports three. A *syntax* error is the
exception — there is no resynchronization point, so it is reported alone.

`ctadl index` and `ctadl query` also log, at `warn`, every rule that was live for the running
phase and matched nothing. That is the condition worth hunting for: a rule that is doing nothing
looks exactly like a clean program.

---

## 11. Recipes

| goal | heads |
| --- | --- |
| Mark a function's output as untrusted | `source(F::return)` |
| Mark a whole value, every offset and field, as untrusted (`argv`) | `source(…, saturating = true)` |
| Flag a dangerous API | `sink(F::arg(N))` |
| Summarize a library function CTADL cannot see | `propagation(…)`, often with a `sink` |
| A source or sink only in some calling contexts | a port anchored at a `callsite` site |
| A source or sink on a named local | `F::local(L)` with `local(F, L, name = "…")` |
| Join a callee in one artifact to its implementation in another | `bridge(…)` |
| Apply a model to only some of a project's imports | `fun(F, language = …)` or `fun(F, import = …)` |
| Register an access path the code never writes | `access_paths("…")` |
| Trust the model and skip the body | `skip_analysis(F)` |
| Keep a body another rule, or a default, skips | `analyze(F)`, with a `priority` above the other rule's |
| Take a contract method's implementations out of the call graph | `K::dispatch(<flow>)` |
| Discard a call that moves nothing | `K::dispatch(resolve = "skip")` |
| Keep a signature's bodies reachable but off CHA | `K::dispatch(resolve = "inline")` |

## 12. Tips

- **Check before you index.** `ctadl query` against an un-indexed project tells you which rules
  select something and which select nothing; it needs only the import. See
  [Checking a model file before you index](#checking-a-model-file-before-you-index).
- **Group families with sets.** `fun(F, name in {…}, parent in {…})` models a library's methods
  in one rule.
- **Get the ports right for the language.** Java taints through `return` and `arg(N)`; C through
  `.deref` out-parameters.
- **Kinds are your vocabulary.** A flow is reported when a source's `kind` reaches a sink of the
  same `kind`; keep them consistent across files.
- **Do not name lua functions by `%anonN`.** A function with no name of its own is numbered by a
  counter that runs across the whole import, so adding a file renumbers the rest. Pin it by its
  module and a `uses_field` or `arity` test, or give it a name in the source.
- **Propagations at index time, sources and sinks at query time.** Pass the same file to both;
  each command takes its half.
