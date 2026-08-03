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
    - Port arguments can be bound to variables: `sink(arg(I)@F) :- fun(F), param(F, I);`
  - Ports can be anchored to a function `anchored-port := port @ F` or a call site (both of which are strings)
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

Port anchors specify a port anchored in a function: `return@F` means "the return port in F." Ports
and port flows are a concise way of specifying source, sink, propagation, bridge models in a unified
syntax. Access paths in port flows are always literal, never bound to a variable. Multiple ports /
port flows may be given by comma separating the individual ports / flows. A port anchored at a site
denotes the actual at that call.

```
arg(_)@F // passed to sink, means every argument of F is a sink
arg(2)@F // passed to source, means arg(2) of F is a source

// propagation, argument 0 flows to the return value, function F (can be a bound variable)
return@F <- arg(0)@F
// propagation of F's arg(1) to arg(0)
arg(1)@F -> arg(0)@F
// propagation, sugar for two directed flows
arg(2).foo@F <-> arg(0).bar@F

// multiple spec bridge model
arg(0)@F -> arg(0).stack[0]@G,
  arg(1)@F -> arg(0).stack[1]@G,
  arg(2)@F -> arg(0).stack[2]@G

// port anchored at callsite
bridge(arg(1).baz@S -> arg(0).stack[2]@G) :-
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
source(return@F), propagation(return@F <- arg(0)@F) :-
  fun(F, name = "openConnection", parent = "Ljava/net/URL;");

// {"find":"methods","where":[{"constraint":"signature_match","names":["getAbsolutePath","getAbsoluteFile","getCanonicalFile","getName","getParentFile","getPath"],"parent":"Ljava/io/File;"}],"model":{"propagation":[{"input":"Argument(0)","output":"Return"}]}}
propagation(return@F <- arg(0)@F) :-
  fun(F,
    name in {"getAbsolutePath","getAbsoluteFile","getCanonicalFile","getName","getParentFile","getPath"},
    parent = "Ljava/io/File;");
```

## Engine

### Migration

All existing tests get migrated to use this new format. All existing builtin models as well.

Build a model migrator that takes existing json files and re-expresses them in this format.

The existing jsonl built-in models and their schema will be replaced wholesale by this new modeling
capability and a grammar defining the Datalog DSL. The json model matchers will disappear.

### Validation

After migrating existing model_generators, the new model_generators should produce semantically
equivalent matching results.

There will be good error messages. There will be a mode that produces a count of matched model heads
kept for that phase so that one can debug models that should match but aren't.

The existing tests must all pass, including regression.

### Implementation

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
