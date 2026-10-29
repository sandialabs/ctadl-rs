# Model matching DSL - DO-NOT-MERGE

The current model matching is basically a domain specific language (DSL) implemented inside JSON
(see the model-generators docs). This proposal lifts it to a real DSL with semantics and a decent
syntax, along with an execution engine.

The engine is phased: `index` time and `query` time. Each one accepts a set of models: the indexer
wants propagation/bridge/access_paths and the query wants source/sinks. They are specified in the
same format and can be written in the same file. The phases will give a summary warning with a count
of models ignored for that phase. Specifically, each phase keeps its own rule heads and there is no
warning when a rule contributes at least one head to the running phase.

## Models this language needs to express

Below are some examples that this language needs to be able to express. In general, though, we must
be able to express all the models in the default models built in to the jsonl files in this repo. On
the matching side, these need to be expressible:

- All methods named `openConnection` from the `Ljava/net/URL;` class descriptor
- All methods named `toString` that take no arguments and return a string
- The method `append` from both `StringBuffer` and `StringBuilder`

On the action side, for each method in the set:

- The return value is a source
- Argument 2 field `.bar` is a sink
- Argument 2's `.foo` field propagates to the return value
- Argument 1`.baz` of calls to function F go to function G arg 0`.stack[2]`

## DSL for describing sets of methods

This DSL looks like a Datalog language. It has built-in relations with defined meanings. It has no
recursive rule evaluation. Since there is no recursion in this language, stratification is not
necessary. Each relation has required columns followed by a dictionary of attributes which can
optionally be bound. Each attribute is functionally dependent, i.e., there is at most one
corresponding to each value for the required columns. Variables are uppercase; this is done so it's
easy to differentiate them from the many built-in relations and keywords. Reserved words, never
variables or attribute names: `_`, `return`, `arg`, `param`, all built-in relation names.

Types:
  - Primitive: string, bool, integer
  - Ports are a first-class data type:
    - `arg(0).foo[2]`
    - `arg(4)."weird.field[0]"`
    - `return`
    - `arg(_)` expanded by the engine on all arguments, i.e., the current `Argument(*)` idea. For
      functions, it is expanded using the arity of the function. If the arity is unknown, a warning
      is shown. For callsites, it is expanded using the actuals list of the matching callsites
    - Port arguments can be bound to variables: `sink(F::arg(I)) :- fun(F), param(F, I);`
  - Ports can be anchored to a function `anchored-port := F :: port` or a call site (both of which are strings)
  - Flows are between ports `flow := anchored-port ("<-" | "->" | "<->") anchored-port`

Built-in input relations:

- `fun(F)` - `F` is the fully qualified name of a function, same as the name in FunctionData
  - attributes: `name`, `arity`, `language`, `parent`, `signature`, `has_code`, `qualified-id`, `import`
  - the `name` attribute is a simple name
- `param(F, Index)` valid parameter indices for function F, populated wherever arity is known, from body or signature
- `callsite(F, Site)` callsite `Site` is inside the function `F`
  - attributes: `callee_string`, the fully qualified name of a function (joinable with `fun`) or the
    variable name from the program text in the case of indirect call
- `subclass(Sub, Super)` - the sub and superclasses
- `subclass*(Sub, Super)` - the sub and superclass, reflexive transitive closure
- `subclass+(Sub, Super)` - the sub and superclass, transitive closure
- `uses_field(F, Fld)` - the function accesses a field

Rules look like datalog. The basic pattern is a head is derived from body atoms.

```
<head> :- <atom>, ...;
```

`;` is a terminator. The body must be satisfied to derive the head. If an attribute is not
mentioned, then it is not constrained. `fun(F, parent = P)` when the function `F` is a Pcode
function (which has no `parent`) just fails the match, no error. Every head variable must be bound
in the body of the corresponding rule. Multiple, comma-separated head atoms are supported.
Underscore `_` is used in a couple of places as a placeholder that matches anything, binds nothing,
and each occurrence is independent.

In body atoms, in attribute position, the following are legal: `attr (= expr | != expr | < expr | >
expr | <= expr | >= expr | in {set})`. In attribute position of head atoms, only `attr = expr` is
allowed.

It has operators that can be used in atom position but aren't true relations:

- `regex_match(str, pattern)` - regex match
- `var in { ... }` where inside the braces is a comma-separated set of primitives of the appropriate
  type, such as string or number
- `X = Y`, and `!=` string comparison
- `X < Y` and `=`, `!=`, `>`, `<=`, `>=` - numeric comparison
- `&&`, `||`, Boolean combination of Boolean-typed things

Atoms occurring in a rule body can be negated under certain conditions. It uses the `!` syntax.

1. All variables occurring in the relation text must be bound by positive atoms.
2. Attributes are legal in a negated atom. For instance, `!fun(F, parent = _)` succeeds when `F` has
   no parent.


Built-in output relations. The point of writing a model file in this DSL is to communicate to the
engine these output relations:

- `source(<anchored-port>)` 
  - `saturating = <bool>`, default false. A saturating source means that *any access path that
    extends the associated source port* is considered also to be a source (this is the existing
    semantics)
- `sink(<anchored-port>)` 
  - `wildcard = <bool>`, default true. A wildcard sink means that *any access path that extends the
    associated sink port* is considered to match (this is the existing semantics)
- `propagation(<flow>)` errors if the flow mentions two distinct functions or depends on a callsite
- `bridge(<flow>)` accepts any flow except propagations
- `access_paths(path)`, adds strings to analysis access paths, can be just bare e.g.
  `access_paths(".foo.bar")`

Port anchors specify a port anchored in a function: `F::return` means "the return port in F." Ports
and port flows are a concise way of specifying source, sink, propagation, bridge models in a unified
syntax. Access paths in port flows are always literal, never bound to a variable. Multiple ports /
port flows may be given by comma separating the individual ports / flows. A port anchored at a site
denotes the actual at that call.

```
F::arg(_) // passed to sink, means every argument of F is a sink
F::arg(2) // passed to source, means arg(2) of F is a source

// propagation, argument 0 flows to the return value, function F (can be a bound variable)
F::return <- F::arg(0)
// propagation of F's arg(1) to arg(0)
F::arg(1) -> F::arg(0)
// propagation, sugar for two directed flows
F::arg(2).foo <-> F::arg(0).bar

// multiple spec bridge model
F::arg(0) -> G::arg(0).stack[0],
  F::arg(1) -> G::arg(0).stack[1],
  F::arg(2) -> G::arg(0).stack[2]

// port anchored at callsite
S::bridge(arg(1).baz -> G::arg(0).stack[2]) :-
  callsite(_, S, callee_string = F),
  fun(F, name = "luaCallNativeAdd", language = "lua"),
  fun(G, name = "luaNativeAdd", language = "pcode");
```

You can bind columns like in, e.g., `fun(F), regex_match(F, ".*Foo.*")`. You can also bind optional
attributes, e.g., `fun(F, name = N), regex_match(N, "^get.*")`

Parsing note: `arity < -1` and a mistyped `arity <- 1` differ by one space; maximal munch takes the
arrow. The positions are disjoint (arrows only in output arguments, comparisons only in
attribute/operator position), so it's benign -- the parser must be told to emit "arrow not allowed
here."

### Examples

You can conveniently specify some models that need to match on multiple parent classes like so:

```
fun(F, name = "append", parent in {"Ljava/lang/StringBuffer;", "Ljava/lang/StringBuilder;"})
```

```
// Flows java.net.URL.openConnection's "this" argument to return and sets return value source
source(F::return), propagation(F::return <- F::arg(0)) :-
  fun(F, name = "openConnection", parent = "Ljava/net/URL;");

// {"find":"methods","where":[{"constraint":"signature_match","names":["getAbsolutePath","getAbsoluteFile","getCanonicalFile","getName","getParentFile","getPath"],"parent":"Ljava/io/File;"}],"model":{"propagation":[{"input":"Argument(0)","output":"Return"}]}}
propagation(F::return <- F::arg(0)) :-
  fun(F,
    name in {"getAbsolutePath","getAbsoluteFile","getCanonicalFile","getName","getParentFile","getPath"},
    parent = "Ljava/io/File;");
```

## Engine

### Migration

All existing tests get migrated to use this new format. All existing builtin models as well.

Build a model migrator that takes existing json files and re-expresses them in this format. It can
be used for backward compatibility, so that the existing models in the wild can still be used.

The existing jsonl built-in models and their schema will be replaced wholesale by this new modeling
capability and a grammar defining the Datalog DSL. The json model matchers will disappear.

### Validation

After migrating existing model_generators, the new model_generators should produce semantically
equivalent matching results.

There will be good error messages. There will be a mode that produces a count of matched model heads
kept for that phase so that one can debug models that should match but aren't.

The existing tests must all pass, including regression.

### Implementation

The pest parser is a dependency of this system; parsing should be done with pest.

Rules must be well-moded otherwise it is a load-time error. Operators on variables must have all
variables bound or the rule isn't executable, and will throw an error message to that effect. The
execution engine may evaluate in any binding-consistent order. So, for example `... :-
regex_match(F, ".foo.*"), fun(F)` will throw an error message, but reversing the order of atoms
in the body doesn't.

The engine will be a custom semi-naive implementation that interprets the rules. Join order is chose
by the engine subject to the modedness constraints. The builtin relations are backed by the existing
ProgramMatchIndex tables.

## Differences from current state

- `fields` and `all_fields` aren't even used and they aren't in this design
- `in_function` is subsumed by callsite's caller column
- no support for the inert schema keys (taint, modes/skip-analysis, forward_self)
- reserved-but-unimplemented find: variables/fields. generally, all relations are built in so any potential relation name is reserved
- if the model migrator meets these unsupported keys in the wild, it should warn that they aren't translated

## `find: dispatch`

Added to the JSON format after this document was first written, and carried across rather than
left behind: a dispatch model attaches to a call *signature* and replaces the site's target set,
which is what makes the hybrid call graph affordable, and defaults that quietly lost it would
disable the feature for Java.

It needs its own universe, not a view of `fun`: the method table holds implementations, and about
half an app's interface call sites name a type that has no row there. So `callsig(K)` is an input
relation of its own, built from the call sites, carrying the four attributes a key can have
(`name`, `parent`, `signature`, `qualified-id`) and none of the ones that read a body.

The head is `K::dispatch(...)`, saying exactly one of: a summary flow, `resolve = "inline"`,
`resolve = "skip"` (the JSON's empty `propagation` list — a head is an atom, and there is no
empty one to write), or `closure_shaped = true`. Index-time, like a propagation.

Collecting the keys costs a pass over every statement, so it is gated: an import pays only when
some rule reads `callsig`, or when the language's defaults do.

## What was built

Model matching is now a real language with an execution engine, the shipped defaults are written
in it, and the JSON format keeps working through a migrator.

Start here if you want to *use* it: [`docs/model-dsl.md`](docs/model-dsl.md).

### 1. The language

A model file is a list of rules. Each derives one or more **output** atoms from a conjunction of
atoms over the **input** relations the analyzer already has.

```
// java.net.URL.openConnection(): the receiver flows to the return value, which is a source
source(F::return), propagation(F::return <- F::arg(0)) :-
  fun(F, name = "openConnection", parent = "Ljava/net/URL;");
```

Files end in `.ctadl` (or `.dl`) and are passed exactly as JSON model files are:

```bash
ctadl index my-app --models propagations.ctadl
ctadl query my-app --models sources-and-sinks.ctadl --output results.sarif
```

Everything the design specifies is in:

| | |
| --- | --- |
| **Input relations** | `fun` (with `name`, `arity`, `language`, `parent`, `signature`, `has_code`, `qualified-id`, `import`), `param`, `callsite` (with `callee_string`), `subclass`, `subclass*`, `subclass+`, `uses_field` |
| **Output relations** | `source` (`kind`, `saturating`), `sink` (`kind`, `wildcard`), `propagation`, `bridge`, `access_paths` |
| **Ports** | `return`, `arg(0)`, `arg(_)`, `arg(I)`, `arg(0).foo[2]`, `arg(4)."weird.field[0]"`, anchored as `F::port` |
| **Flows** | `A -> B`, `A <- B`, `A <-> B` |
| **Attribute operators** | `=`, `!=`, `<`, `>`, `<=`, `>=`, `in { … }` |
| **Atom operators** | `regex_match`, `in`, comparisons, `&&`, `\|\|`, `!` |
| **Syntax** | `,`-separated head atoms, `:-`, `;` terminator, `_` wildcard, `//` and `/* */` comments |

Variables are uppercase, which is what keeps them apart from the many built-in relation names.
Every relation name is reserved, so an unrecognized one is reported as the typo it is, with a
"did you mean" where one applies.


### 2. Files added

| file | lines | job |
| --- | --- | --- |
| `ctadl-ascent/src/models/dsl/grammar.pest` | 120 | the surface syntax |
| `ctadl-ascent/src/models/dsl/ast.rs` | 353 | abstract syntax; program-independent |
| `ctadl-ascent/src/models/dsl/parse.rs` | 732 | pest → AST, plus the checks the grammar cannot make |
| `ctadl-ascent/src/models/dsl/check.rs` | 904 | relation shapes, **modes**, and the execution plan |
| `ctadl-ascent/src/models/dsl/relations.rs` | 428 | the input relations, materialized over one program |
| `ctadl-ascent/src/models/dsl/eval.rs` | 912 | the engine |
| `ctadl-ascent/src/models/dsl/emit.rs` | 501 | groundings → `ProgramModelMatches` |
| `ctadl-ascent/src/models/dsl/migrate.rs` | 1029 | the JSON format, re-expressed in this one |
| `ctadl-ascent/src/models/dsl/tests.rs` | 539 | 39 unit tests: syntax, checking, migration |
| `ctadl-ascent/tests/model_dsl.rs` | 760 | 29 engine tests against real programs |
| `docs/model-dsl.md` | 353 | the language reference |
| `ctadl-ascent/src/models/defaults/*.ctadl` | 405 | the migrated shipped defaults |

Modified: `cli/mod.rs`, `cli/model_check.rs`, `codegen/model_matches.rs`, `error.rs`, `main.rs`,
`models/{mod,matches,spec,match_index}.rs`, `tests/{default_models,flowy_tests}.rs`,
`docs/model-generators.md`. About 590 added lines across those, and 32 removed — the DSL is a
peer of the existing machinery rather than a rewrite of it.


### 3. Three decisions worth knowing about

#### Bodies split into connected components

A bridging rule names a callee in one program and its implementation in another:

```
bridge(F::arg(0) -> G::arg(0).stack[1]) :-
  fun(F, name = "mylib.add", language = "lua"),
  fun(G, name = "l_add",     language = "pcode");
```

No single import satisfies that body, and under streaming only one program is ever in memory. So
the body is split into connected components by shared variables, each component is accumulated as
the imports go past, and the components are joined once the loop ends. The cross product *is* the
pairing — the same semantics the existing bridge machinery has, reached from the language side
rather than bolted on. A single-component rule, which is nearly all of them, notices nothing: its
solution set is the union over imports.

Nothing survives between imports but the bindings. The relation tables are built per import and
dropped with it, so the memory posture stays streaming.

#### Modes are checked left to right; join order is not

A rule is well-moded when every operator has its variables bound by atoms **to its left**. This is
a load-time error, exactly as the design specifies:

```
source(F::return) :- regex_match(F, ".*Foo.*"), fun(F);   // error: F is not bound yet
source(F::return) :- fun(F), regex_match(F, ".*Foo.*");   // fine
```

The engine is then free to reorder — filters as soon as their variables exist, indexed lookups
before scans. Modedness is a property the author can see; join order is not, so only the first is
theirs.

The design's `<-` wrinkle is handled where it says: `arity <- 1` lexes as the arrow (maximal
munch) and the parser reports *"'<-' is a flow arrow and is not allowed here"* rather than
pointing one token past the mistake. `arity < -1` is unaffected.

#### Negated groups quantify their own locals

A negated **atom** requires every variable in it to be bound, with `_` as the way to say "any
value at all" — the design's rule. A negated **group** is one existence check over a subquery,
and the variables local to it are existentially quantified:

```
source(F::return) :- fun(F), !(fun(F, name = N) && regex_match(N, "^get"));
```

That reads "no name of `F` matches `^get`". Written as two separate negations it would mean
something else, because `N` would not be shared. This is what lets the migrator translate a
negated `name` regex at all.


### 4. Migration and the defaults

`ctadl migrate-models` rewrites a JSON / JSON5 / JSONL model file in the DSL:

```bash
ctadl migrate-models my-models.jsonl            # writes my-models.ctadl
ctadl migrate-models my-models.jsonl -o -       # to stdout
ctadl migrate-models my-models.jsonl --dry-run  # check only
```

It loads what it produced before writing it, so a file that translates but does not check is a
failure rather than a surprise on the next index. Anything that cannot carry across is reported
rather than approximated.

The interesting case is disjunction. A `where` is a conjunction, but a constraint may be an
`any_of` or a `not`, so the constraint tree is pushed into negation normal form and then into
DNF, and **each disjunct becomes one rule with the same heads**. That is how Datalog spells "or",
which makes the translation exact: `any_of` was already a union over the matched set.

| JSON | DSL |
| --- | --- |
| `find: methods` | the subject `F`, bound by `fun(F, …)` |
| `find: callsites` | `callsite(C, S, callee_string = F)`, ports anchored at `S` |
| `in: {language, languages, import}` | `fun(F, language = …, import = …)` |
| `signature_match` | `fun(F, name = …, parent = …, qualified-id = …)` |
| `name` / `signature` / `signature_pattern` | `fun(F, name = N), regex_match(N, …)` |
| `has_code`, `number_parameters` | `fun(F, has_code = …)`, `fun(F, arity <op> …)` |
| `uses_field` | `uses_field(F, …)` |
| `parent` / `extends` | `fun(F, parent = P)`, plus `subclass(P, S)` for `extends` |
| `any_of` | several rules with the same head |
| `not` | `!atom`, or `!(a && b)` when the constraint needs two atoms |
| `in_function` | constraints on `C`, the caller column of `callsite` |

#### The shipped defaults

`java-index.ctadl`, `native-index.ctadl`, `lua-index.ctadl` and `cil-index.ctadl` are this tool's output, checked in
next to the `.jsonl` they came from. **The `.ctadl` is what an index loads.** The `.jsonl` stays
as the migrator's input and as the oracle the `.ctadl` is checked against.

They read well:

```
propagation(F::return <- F::arg(0)),
  propagation(F::arg(0) <- F::arg(0)),
  propagation(F::return <- F::arg(1)),
  propagation(F::arg(0) <- F::arg(1)) :-
  fun(F, name = "append", parent in {"Ljava/lang/StringBuffer;", "Ljava/lang/StringBuilder;"});
```

`tests/default_models.rs::the_dsl_defaults_match_what_the_json_defaults_matched` loads both
forms against the same program and requires identical matches. That is the validation
[Validation](#validation) asks for by name, and it is what stops the pair from drifting: edit one
and not the other and the test fails, naming the file.


### 5. Diagnostics

Every error names a file, line and column, and says what to do:

```
> models.ctadl:4:22: 'F' is not bound at this point. An operator can only test variables that an
  atom to its left has already bound; move the atom that binds 'F' earlier in the body.
```

Rule errors accumulate, so a file with three bad rules reports three. A syntax error is the
exception — there is no resynchronization point, so it is reported alone.

Both phases report what they kept and what they did not:

```
model DSL: 0 source, 0 sink, 1 propagation, 0 bridge, 0 access-path head(s) kept for this phase
warning: ctadl index is ignoring 2 model rule(s) that declare only source/sink heads;
         they take effect in the other phase
warning: 1 model rule(s) are live for this phase and matched nothing: models.ctadl:3
```

Those three lines answer three different questions. The counts are the design's "count of matched
model heads kept for that phase" — the number to look at when a model should be matching and is
not. The phase warning separates a rule that belongs to the other command from one that is broken;
a rule contributing at least one head to the running phase is never counted. The dead-rule warning
names the rules that ran and selected nothing, which is the failure that otherwise looks exactly
like a clean program.

`ctadl query` with no index (the model-check path) reports DSL rules through the existing
`CTADL0004` / `CTADL0011` / `CTADL0100` notifications, with a rule index standing in for a
generator index.


### 6. Testing

483 tests pass. Clippy and `rustfmt` are clean.

- **39 unit tests** (`models/dsl/tests.rs`) — syntax, mode checking, every error message the
  design calls for, and the migrator. Includes a drift guard: every shipped default must migrate
  cleanly and load.
- **29 engine tests** (`tests/model_dsl.rs`) — each input relation against a real program, the
  operators, every head kind, the two-import bridge, phase separation, and loading through the
  ordinary `try_load_models` entry point.
- **1 end-to-end fixture** (`tests/tnt/port_bare_dsl.tnt`) — the DSL twin of `port_bare.tnt`,
  driven through the real index pipeline. Verified load-bearing: break the model and the test
  fails.
- **Equivalence** (`tests/default_models.rs`) — the `.ctadl` and `.jsonl` defaults must match the
  same things on Java, native, Lua, CIL and flowy programs.
- **Regression** — `cargo xtask regression --filter models:` and `--frontend lua` both pass, the
  latter exercising the full import → index → query pipeline with DSL defaults.

Manual end-to-end check, for what it looks like in practice: index a flowy program with a DSL
propagation model, query it with DSL source/sink models, get a reported taint flow.


### 7. What was left out, and why

**The JSON matcher is still there.** [Migration](#migration) says "the json model matchers will disappear."
`json.rs` remains as the JSON front end; JSON files are *not* routed through migrate→DSL. The
reason is `json_error_handling.rs`: 49 tests pin JSON-specific error types, per-generator error
messages, and batch/partial-append semantics that the DSL path would change. The migrator is the
mechanism for that removal and it already runs over every shipped default, so the deletion is a
mechanical follow-up — but it changes observable error text, and doing that silently seemed worse
than saying so.

**A callsite-anchored `bridge` is a load error, not a feature.** The design's
`S::bridge(arg(1).baz -> G::arg(0).stack[2])` example is rejected. A bridge emits `call` rows that
the index fixpoint *consumes*, so a site anchor would name only the statically emitted calls —
precisely not the ones (a call through a table field, a `dlsym`'d pointer, a virtual dispatch) a
bridge exists for. This is the same reason the JSON loader rejects `find: callsites` + `bridge`.
The requirement that example illustrates — "argument 1's `.baz` of calls to F go to G arg 0's
`.stack[2]`" — is expressible by anchoring at the callee, which covers every call site of it, and
the error message says exactly that.

**`Variable(...)` ports have no DSL spelling.** A source or sink on a named local is not
expressible; the migrator warns when it meets one.

**JVM, DEX and pcode regression suites did not run** — this machine has no Java or Ghidra
toolchain. The Java and native defaults are covered semantically by the equivalence test, not end
to end.

**One deliberate departure from strict functional dependence.** `fun(F, name = N)` binds *once per
published spelling* where a frontend has several — a native symbol known both as `system` and as
`<EXTERNAL>::system@00101008`, a Lua callee reachable as `execute` and as `os.execute`. Binding
only a canonical name would silently narrow what a migrated `name` regex matches, which is exactly
what the equivalence requirement forbids. It is documented in `relations.rs` and in
`docs/model-dsl.md`.
