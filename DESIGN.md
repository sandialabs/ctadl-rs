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

### What the DSL can say that the JSON cannot

A JSON generator selects one set of functions (or call sites) with a tree of constraints, each
asking about that one subject. A DSL rule body is a join, so it can name several things and
relate them through shared variables. Nearly every JSON model translates (see Migration),
but these do not translate back:

- **Choose a function by what it calls.** `callsite` is an ordinary relation, so a rule
  can bind the caller and attach the model there. JSON's call-site matching only works the other
  way: `find: callsites` attaches to the callee, and `in_function` only narrows by the caller.

  ```
  source(F::return) :- callsite(F, _, callee_string = G), fun(G, name = "getenv");
  ```

- **Relate the caller and the callee.** `in_function` checks the caller on its own, so it cannot
  compare the two, for example to require that they belong to different classes:

  ```
  sink(S::arg(0)) :- callsite(C, S, callee_string = G),
                     fun(C, parent = P), fun(G, parent = Q), P != Q;
  ```

- **Negate a subquery.** `!( … && … )` is an existence check over a join, with the
  group's own variables bound inside it: "functions that never call `close`". JSON's `not` can
  only negate a constraint on the same subject.

  ```
  source(F::return) :- fun(F), !(callsite(F, _, callee_string = G) && fun(G, name = "close"));
  ```

- **Choose ports with a condition.** `arg(I)` with `I` from `param(F, I)` and an ordinary
  comparison names "every argument but the receiver" for any arity. JSON offers one argument or
  `Argument(*)`.

  ```
  sink(F::arg(I)) :- fun(F, name = "exec"), param(F, I), I >= 1;
  ```

- **Walk the class hierarchy transitively.** `subclass+` / `subclass*` are the transitive closure;
  JSON's `extends` looks at the owning class's direct supertypes only. The DSL's `parent` and
  `subclass` also cover Lua's metatable-recovered classes, where JSON's `parent` / `extends`
  warn and match nothing.

- **Compare any two values.** Comparisons and `regex_match` work on any bound variable, not only
  on a fixed attribute of the subject, so two attributes, or attributes of two functions, can be
  compared.

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
- `skip_analysis(F)` keeps `F`'s body out of the index, so its other models are its whole
  behavior (the JSON's `modes: ["skip-analysis"]`). `mem2reg_full(F)` / `mem2reg_window(F)` and
  `frame_store(F)` / `frame_update(F)` set how `F`'s stack slots and frame are lowered (the other
  `modes`). These mode heads are index-time, and only in a rule one import satisfies: they are read
  while that import is lowered.

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

Parsing is done with the `peg` crate, which the rest of the system already uses.

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
- no support for the inert schema keys (taint, forward_self); `modes` is not inert: each value is a
  mode head (`skip_analysis`, `mem2reg_full`, …)
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
| **Output relations** | `source` (`kind`, `saturating`), `sink` (`kind`, `wildcard`), `propagation`, `bridge`, `access_paths`, `dispatch`, `skip_analysis`, `mem2reg_full`, `mem2reg_window`, `frame_store`, `frame_update` |
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
| `ctadl-ascent/src/models/dsl/ast.rs` | 353 | abstract syntax; program-independent |
| `ctadl-ascent/src/models/dsl/parse.rs` | 901 | the PEG grammar and its tree → AST, plus the checks the grammar cannot make |
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
`docs/model-generators.md`. About 590 added lines across those, and 32 removed. JSON files
reach the engine through the migrator.

Removed: the JSON matcher (`models/json.rs`, `models/universe_set.rs`, about 2,800 lines), the
value-based JSON loading API in `models/mod.rs`, the JSON bridge scan and pairing
(`scan_model_files`, `matches::observe_import`, the spec-driven half of
`codegen/model_matches.rs`), and the `ProgramMatchIndex` tables only it read. What remains of
the JSON format is input to the migrator: the validator (`models/json_schema.rs`), the
program-independent parsing it shares with the model check (`models/spec.rs`), and
`dsl/migrate.rs`.


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
failure rather than a surprise on the next index. A generator that cannot carry across exactly
fails the migration rather than being approximated or skipped. A generator the JSON format does
not allow fails with the error the JSON loader raised for it: the validator in
`models/json_schema.rs` checks a generator's shape without a program, and reports what the
loader reported, in the same order.

#### JSON model files are deprecated

A `.json` / `.json5` / `.jsonl` file passed to `--models` is migrated in memory and run through
the DSL engine, the same as a `.ctadl` file; there is no other matcher. Each such file draws a
deprecation warning, once per run: JSON model files will be removed in a future release, and
`ctadl migrate-models` converts one to the DSL. `DslFile::read` does the migration, so every
path-based entry point — `ctadl index`, `ctadl query`, the model check, `ctadl report`, the flowy
check — takes a JSON file the same way. `DslFile::from_json_generators` does the same for
generators already in hand, which is how the tests load them.

The JSON matcher that used to run these files has been deleted. Two things it did came across
with the migration. JSON bridges are DSL rules, paired by the engine's components, and the model
check reports one that paired nothing rather than which side failed; `on-unmatched` and
`on-ambiguous` other than `"error"` only changed what the matcher logged, and are migration
warnings. And `ctadl index` matches the source/sink heads of every file, unreported, because a
matched endpoint refuses a dispatch model — the JSON matcher loaded the whole file at index time,
and a `.ctadl` file's endpoints did not gate dispatch until this.

A migrated file means what the JSON meant, with one exception: where the JSON matcher had a bug
or a limitation, the migration takes the DSL's meaning rather than reproducing it.
`tests/integration/migration_equivalence.rs` checks every other case against what the JSON
matcher matched, recorded in `migration_equivalence.golden` while it existed and frozen since,
and pins each of these differences exactly:

- **Universe.** A generator with no `where`, or a top-level `not`, ranged over the method table
  in the JSON, so it missed a lowered function the table does not name and matched nothing on a
  program with no table (flowy). `fun(F)` is every function.
- **Full names.** `name`, `signature` and `signature_match` also accept a function's full name,
  because the DSL publishes it as a spelling of every function.
- **Lua classes.** `parent` and `extends` matched nothing on Lua in the JSON; the DSL's `parent`
  and `subclass` include metatable-recovered classes.
- **`in_function` under `not` or `any_of`.** The JSON matcher narrowed the callee set by the
  caller's constraint there. The DSL tests the caller, as the constraint says.
- **`find: callsites` anchors at the callee a site names.** The JSON attached the endpoint to
  every matching callee and let the resolved call graph find its sites, so a call naming
  `Base.run` also picked up a model on the override `Mid.run`. `callsite` joins on the static
  `callee_string`. Expressing the resolved form needs a relation over resolved targets — a
  `resolves_to(Site, Callee)` or similar, fed from the call graph rather than the program text
  — which is future work.

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
| `parent` / `extends` | `fun(F, parent = P)` and one formula over `P` (`!`, `&&`, `\|\|` for a nested `not` / `all_of` / `any_of`), plus `subclass(P, S)` for `extends`, over `S` |
| `any_of` | several rules with the same head |
| `not` | `!atom`, or `!(a && b)` when the constraint needs two atoms |
| `in_function` | constraints on `C`, the caller column of `callsite` |

#### The shipped defaults

`java-index.ctadl`, `native-index.ctadl`, `lua-index.ctadl` and `cil-index.ctadl` are this tool's output, checked in
next to the `.jsonl` they came from. **The `.ctadl` is what an index loads.** The `.jsonl` stays
as the migrator's input.

They read well:

```
propagation(F::return <- F::arg(0)),
  propagation(F::arg(0) <- F::arg(0)),
  propagation(F::return <- F::arg(1)),
  propagation(F::arg(0) <- F::arg(1)) :-
  fun(F, name = "append", parent in {"Ljava/lang/StringBuffer;", "Ljava/lang/StringBuilder;"});
```

Two tests in `tests/integration/default_models.rs` hold the pair together.
`the_dsl_defaults_are_the_migration_of_the_json_defaults` requires each `.ctadl` to be, byte for
byte, what `ctadl migrate-models` writes for its `.jsonl`: edit one and not the other and it
fails, naming the file. `the_dsl_defaults_match_what_the_json_defaults_matched` runs each `.ctadl`
on a fixture of every frontend and requires what the JSON matcher matched for the `.jsonl`,
frozen in `default_models.golden` — the validation [Validation](#validation) asks for by name.


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

A migrated JSON file is reported in the JSON's terms, not its translation's. The migrator
records which generator each rule came from, and every surface uses that: provenance is
`file:generator`, `CTADL0004` and the dead-rule warning count a
generator once however many disjuncts it became, and an engine error names the generator rather
than a line of synthetic text. A generator that does not migrate fails `ctadl index` and
`ctadl query` with the migrator's error, which names it; the model check reports it and checks
the rest of the file, as it did for a malformed generator before.


### 6. Testing

`cargo test --workspace` passes. Clippy and `rustfmt` are clean.

- **74 unit tests** (`models/dsl/tests.rs`) — syntax, mode checking, every error message the
  design calls for, and the migrator, including the 29 generator shapes the JSON format rejects,
  each with the loader's error. Includes a drift guard: every shipped default must migrate
  cleanly and load.
- **50 engine tests** (`tests/model_dsl.rs`) — each input relation against a real program, the
  operators, every head kind, the two-import bridge, phase separation, and loading through the
  ordinary `try_load_models` entry point.
- **JSON files on the DSL path** — the tests that used to exercise the JSON matcher
  (`tests/integration/{json_error_handling,models_loading,bridging_models,dispatch_models,...}.rs`,
  `models/tests.rs`) load their generators through the migration, with a shared `load_json`
  helper in `tests/integration/common.rs`.
- **1 end-to-end fixture** (`tests/tnt/port_bare_dsl.tnt`) — the DSL twin of `port_bare.tnt`,
  driven through the real index pipeline. Verified load-bearing: break the model and the test
  fails.
- **Equivalence** — `tests/integration/migration_equivalence.rs` (141 generators) and
  `class_predicates.rs` against the JSON matcher's frozen answers, and
  `tests/integration/default_models.rs`, which holds the `.ctadl` defaults to the `.jsonl` they
  were migrated from on Java, native, Lua, CIL and flowy programs.
- **Regression** — `cargo xtask regression` passes, including `--filter models:` and
  `--frontend lua`, the latter exercising the full import → index → query pipeline with DSL
  defaults.

Manual end-to-end check, for what it looks like in practice: index a flowy program with a DSL
propagation model, query it with DSL source/sink models, get a reported taint flow.


### 7. What was left out, and why

**A callsite-anchored `bridge` is a load error, not a feature.** The design's
`S::bridge(arg(1).baz -> G::arg(0).stack[2])` example is rejected. A bridge emits `call` rows that
the index fixpoint *consumes*, so a site anchor would name only the statically emitted calls —
precisely not the ones (a call through a table field, a `dlsym`'d pointer, a virtual dispatch) a
bridge exists for. This is the same reason the JSON format rejects `find: callsites` + `bridge`.
The requirement that example illustrates — "argument 1's `.baz` of calls to F go to G arg 0's
`.stack[2]`" — is expressible by anchoring at the callee, which covers every call site of it, and
the error message says exactly that.

**`Variable(...)` ports have no DSL spelling.** A source or sink on a named local is not
expressible; migrating a generator that uses one fails, naming it. Stage 2 still knows how to
seed a local (`EndpointMatch::local_index`), and `CTADL0004` keeps its `localNotFound` message id
for when a DSL spelling for a local exists; nothing produces it now.

**Functional dependence holds.** An earlier version bound `fun(F, name = N)` once per published
spelling, which made `fun(F, name != "x")` and `fun(F, name = N), N != "x"` disagree. Each import
now carries a name table (see "Function names and the VMT" above): `fun`'s attributes have one
value each, the full name is `F`, and the other names are `name_of` rows.

## Function names and the VMT (planned)

`fun`'s `name` / `signature` / `qualified-id` are multi-valued today, which breaks the design's
functional dependence: `fun(F, name = N)` binds `N` once per spelling, so `fun(F, name != "x")`
("no spelling is `x`") and `fun(F, name = N), N != "x"` ("some spelling is not `x`") disagree,
and the second holds for nearly every function because the fq is always a spelling. The extra
spellings come from `ProgramFacts::build`: CIL override-alias rows in the VMT, and the fq added
as a spelling (by the Native and Lua table readers, and by the lowered-function loop for every
function).

### The model

The IR namespace is the only identity: every function has one fully-qualified IR name. The VMT
says, per IR name, what other names and facts make sense for it. Nothing in the VMT creates an
identity, and every frontend must decide each function's simple name.

### Steps

1. **CIL bridges replace override aliases.** For each alias row, the CIL frontend synthesizes a
   bridge function under the alias's key whose body calls the implementation (the same
   `method::thunk_body` the delegate thunks use), the way `javac` does for Java. An alias that
   equals the method's own row needs none. When the key is already a different method `D`
   declares (`class C : IA, IB { void IA.M(); public void M(); }`, or `D<U> : B<string>`
   declaring `M(U)`), the bridge gets a disambiguated name (`C::M()->System.Void#IA`) and a row
   under the shared key, so CHA reaches both bodies as today; `Stats` counts these. Keying
   interface call sites by their declared interface, which would fix that precisely, is a
   separate CHA change.
2. **dex: one IR function per declared method**, abstract ones included, as jvm already does.
3. **Restructure `VirtualMethodTable`** into a struct: `names: Vec<FunctionNames>` (`fq`,
   `simple`, `other`, `parent`, `signature`), the shared CHA relations (`implements`,
   `hierarchy`, `interfaces`, `abstract_methods`), and per-language extras (Java `natives`, CIL
   `pinvokes`, the CHA flavour). Every frontend fills `names`, c and flowy included; pcode puts
   its namespace-qualified name and the unstripped `_name` in `other`; an import-time check
   requires exactly one entry per IR function. Bumps `IMPORT_FORMAT_VERSION`.
4. **The DSL reads `names`.** `fun`'s attributes are single-valued again; the fq is no longer a
   spelling (match it as `F = "…"`); `qualified-id` goes away; a `name_of(F, N)` relation ranges
   over `simple` and `other`.
5. **The migrator** emits `name_of` and `F = …` where the JSON matched every spelling, so the
   equivalence goldens hold; `docs/model-dsl.md` and the `relations.rs` module docs follow.
