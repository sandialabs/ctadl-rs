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

Files end in `.ctadl` (or `.dl`). They are passed the same way JSON model files are:

```bash
ctadl index my-app --models propagations.ctadl
ctadl query my-app --models sources-and-sinks.ctadl --output results.sarif
```

The older JSON format is deprecated, and still works by being migrated to this one as it is
loaded; see [§9](#9-migrating-from-the-json-format).

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

Access paths in ports are always literal — never bound to a variable. A segment that is not a
plain identifier is quoted, which takes it out of the path grammar: the Java array element,
written `\[]` in JSON, is `."[]"` here.

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
| `skip_analysis(F)` | a function | — |
| `mem2reg_full(F)`, `mem2reg_window(F)` | a function | — |
| `frame_store(F)`, `frame_update(F)` | a function | — |

- **`kind`** is the taint label. A flow is reported when a source's kind reaches a sink of the
  same kind. It defaults to `"taint"` for both directions, so the smallest useful pair of rules
  works without inventing a vocabulary first.
- **`saturating`** (source only): any access path *extending* the source port is tainted too,
  recursively. Reach for it when callers index into the value — C's `argv` is the motivating case.
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

  `F::skip_analysis()` is the same head written with an anchor, and `skip_analysis("…")` names a
  function by its full name — for a native symbol that is the decorated one, such as
  `<EXTERNAL>::abort@00101008`, so binding through `fun(F, name = …)` is usually what you want.
- **`mem2reg_full`** / **`mem2reg_window`** force or forbid full stack-slot promotion for a native
  function, whatever its size and whatever `ctadl index --mem2reg` says. Full promotion gives every
  stack slot its own local, even one whose address escapes; `ctadl index` names each function
  where that may lose flows in its `notifications.sarif`.
- **`frame_store`** / **`frame_update`** coarsen or keep a native function's frame updates,
  whatever `ctadl index --frame` says. With `frame_update` (the default) SSA versions the stack
  frame, so a load sees only the writes that reach it; with `frame_store` every load of a slot sees
  every store to it.

  These five are *mode* heads: each takes one function and is read as that function's import is
  lowered (SSA, then codegen). So a rule with one must be satisfied within one import — its body
  may not split into parts that share no variable, because those are joined only after every
  import is lowered. When two rules give one function conflicting `mem2reg_*` (or `frame_*`)
  modes, the later rule wins.

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

A port pair is a *prefix substitution*, not a filter — taint at the input path, plus any suffix,
lands at the output path plus that suffix. See §7 of
[model-generators.md](model-generators.md#a-port-pair-is-a-prefix-substitution-not-a-filter) for
the worked matrix; the semantics are unchanged by the syntax.

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
not a line of a translation the author has never seen. The shipped defaults in
`ctadl-ascent/src/models/defaults/` are this tool's output, checked in next to the `.jsonl` they
came from; a test requires each `.ctadl` to be exactly what this tool writes for its `.jsonl`.

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
| `model: {modes: ["skip-analysis"]}` | `skip_analysis(F)`, and likewise `mem2reg_full`, `mem2reg_window`, `frame_store`, `frame_update`; within a generator the last of each pair wins |

A migration says what the JSON means or fails: it never writes a rule that matches more or less.
A generator the JSON format does not allow fails with the error the JSON loader raised for it.
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

`ctadl index` and `ctadl query` also log, at `info`, every rule that was live for the running
phase and matched nothing. That is the condition worth hunting for: a rule that is doing nothing
looks exactly like a clean program.
