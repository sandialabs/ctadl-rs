# The CTADL model DSL

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

The older JSON format still works; see [§9](#9-migrating-from-the-json-format).

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
never a variable or an attribute name: `_`, `return`, `arg`, `param`, and every built-in relation
name.

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
  | `arg(0).foo[2]` | argument 0's `foo` field, then offset 2 |
  | `arg(4)."weird.field[0]"` | a field whose name contains dots and brackets |

  `arg(_)` is expanded from the arity for a function, and from the actual-parameter list for a
  call site. If the arity is unknown you get a warning and no expansion.

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
| `fun(F)` | fully qualified function name | `name`, `arity`, `language`, `parent`, `signature`, `has_code`, `qualified-id`, `import` |
| `param(F, Index)` | function, parameter index | — |
| `callsite(F, Site)` | the *caller*, the site | `callee_string` |
| `subclass(Sub, Super)` | two classes, direct edge | — |
| `subclass*(Sub, Super)` | reflexive transitive closure | — |
| `subclass+(Sub, Super)` | transitive closure | — |
| `uses_field(F, Fld)` | function, field it loads or stores | — |

Notes worth having:

- `name` is a **simple** name. Where a frontend publishes more than one spelling for a function —
  a native symbol known both as `system` and as `<EXTERNAL>::system@00101008`, a Lua callee
  reachable as `execute` and as `os.execute` — all of them match, and `fun(F, name = N)` binds `N`
  once per spelling.
- `arity`, `has_code` and `param` read the IR, so they match nothing for a function with no body:
  an external, a dex/jvm `ext` stub. `name` / `signature` / `qualified-id` are the supported shape
  for a bodyless callee.
- `parent` exists where the frontend has a class hierarchy (Java, Lua). Elsewhere it matches
  nothing, which is fail-closed.
- `callee_string` is the fully qualified callee (joinable with `fun`) or, for an indirect call,
  the variable the program text calls through.

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
source(F::return) :- fun(F), !(fun(F, name = N) && regex_match(N, "^get"));
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

- **`kind`** is the taint label. A flow is reported when a source's kind reaches a sink of the
  same kind. It defaults to `"taint"` for both directions, so the smallest useful pair of rules
  works without inventing a vocabulary first.
- **`saturating`** (source only): any access path *extending* the source port is tainted too,
  recursively. Reach for it when callers index into the value — C's `argv` is the motivating case.
- **`wildcard`** (sink only): the sink matches any access-path extension of the port. On by
  default; set `false` to require the exact path.

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

The engine is phased. `ctadl index` keeps the `propagation` / `bridge` / `access_paths` heads;
`ctadl query` keeps the `source` / `sink` ones. Each says how many rules contributed nothing to
it, and a rule contributing at least one head to the running phase is never counted:

```
source(F::return), propagation(F::return <- F::arg(0)) :- fun(F, name = "openConnection");
```

is live in both phases and warned about in neither.

---

## 9. Migrating from the JSON format

`ctadl migrate-models` rewrites a JSON / JSON5 / JSONL model-generator file in this language:

```bash
ctadl migrate-models my-models.jsonl            # writes my-models.ctadl
ctadl migrate-models my-models.jsonl -o -       # to stdout
ctadl migrate-models my-models.jsonl --dry-run  # check only
```

The migration is checked by loading it, so a file that translates but does not check is a failure
rather than a surprise on the next index. The shipped defaults in
`ctadl-ascent/src/models/defaults/` are this tool's output, checked in next to the `.jsonl` they
came from; a test loads both against the same program and requires identical matches.

How the JSON constructs map:

| JSON | DSL |
| --- | --- |
| `find: methods` | the subject variable `F`, bound by `fun(F, …)` |
| `find: callsites` | `callsite(C, S, callee_string = F)`, ports anchored at `S` |
| `in: {language, languages, import}` | `fun(F, language = …, import = …)` |
| `signature_match` name/parent/qualified-id | `fun(F, name = …, parent = …, qualified-id = …)` |
| `name` / `signature` / `signature_pattern` | `fun(F, name = N), regex_match(N, …)` |
| `has_code` | `fun(F, has_code = …)` |
| `number_parameters` | `fun(F, arity <op> …)` |
| `uses_field` | `uses_field(F, …)` |
| `parent` / `extends` | `fun(F, parent = P)`, plus `subclass(P, S)` for `extends` |
| `any_of` | **several rules with the same head** — that is how Datalog spells "or" |
| `all_of` | more atoms in one body |
| `not` | `!atom`, or `!(a && b)` when the constraint needs two atoms |
| `in_function` | constraints on `C`, the caller column of `callsite` |

Three things do not carry across, and are reported rather than approximated:

- `taint`, `modes`, `forward_self` — schema-only in the JSON loader too; a model using them
  parses and has no effect either way.
- `Variable(...)` ports, which select a local by source name.
- A `bridge` with no `arguments` map. The JSON loader falls back to an identity map over the
  arity the two sides share, which needs the fact base and cannot be written as a rule. Write the
  map.

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
