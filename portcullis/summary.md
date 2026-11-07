# Model matching DSL — what was built -- DO-NOT-MERGE

Implements [`DESIGN.md`](DESIGN.md). Model matching is now a real language with an execution
engine, the shipped defaults are written in it, and the JSON format keeps working through a
migrator.

Start here if you want to *use* it: [`docs/model-dsl.md`](docs/model-dsl.md).

---

## 1. The language

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

---

## 2. Files added

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

---

## 3. Three decisions worth knowing about

### Bodies split into connected components

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

### Modes are checked left to right; join order is not

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

### Negated groups quantify their own locals

A negated **atom** requires every variable in it to be bound, with `_` as the way to say "any
value at all" — the design's rule. A negated **group** is one existence check over a subquery,
and the variables local to it are existentially quantified:

```
source(F::return) :- fun(F), !(fun(F, name = N) && regex_match(N, "^get"));
```

That reads "no name of `F` matches `^get`". Written as two separate negations it would mean
something else, because `N` would not be shared. This is what lets the migrator translate a
negated `name` regex at all.

---

## 4. Migration and the defaults

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

### The shipped defaults

`java-index.ctadl`, `native-index.ctadl` and `lua-index.ctadl` are this tool's output, checked in
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
`DESIGN.md` asks for by name, and it is what stops the pair from drifting: edit one and not the
other and the test fails, naming the file.

---

## 5. Diagnostics

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

---

## 6. Testing

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
  same things on Java, native, Lua and flowy programs.
- **Regression** — `cargo xtask regression --filter models:` and `--frontend lua` both pass, the
  latter exercising the full import → index → query pipeline with DSL defaults.

Manual end-to-end check, for what it looks like in practice: index a flowy program with a DSL
propagation model, query it with DSL source/sink models, get a reported taint flow.

---

## 7. What was left out, and why

**The JSON matcher is still there.** `DESIGN.md` says "the json model matchers will disappear."
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
