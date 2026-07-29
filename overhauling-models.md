# My plan for overhauling models - DO-NOT-MERGE

BLUF: Overhaul the model matching so that it is done on demand, as we load IR, and stored into its
own datastructure that is generated into facts during a new, second phase inside codegen.

We use the bridging-models-design.md as the concrete model_generator syntax to specify bridging models.

Design principles: all model matching is done on the VMT in the IR, irrespective of (and before)
codegen.

The core thing we add is a new struct `ProgramModelMatches`. As we stream IR, we populate a
persistent `ProgramModelMatches`, explained next. It gets codegen'd after all the IR. It stores the
exact information about matched models:

- a `propagations` field to express propagation models, their associated access paths, etc. Codegen
  can use this directly to generate code and record the relevant access paths. The "propagations"
  field also understands `AnyArgument`, `Index`, `Return`, and `Global` so that it can codegen the
  appropriate summaries.

- an `access_paths` field that records a set of access paths. This field is used to express access
  paths that *do not* occur syntactically in the IR. It can hold human-known composed paths, for
  example to declare ".next.next.next", three fields deep into a linked list.

- Bridging models generate into a `bridges` field that, like "propagations", is basically a native
  representation of bridging models -- they express how the caller's method's parameters (and
  subfields) should be shuttled into the parameters of a callee. By implementing a new field,
  "bridges," we don't need to worry about turning bridging models into IR and back -- we just
  codegen them directly. Bridging models don't need to be optimized, like real IR code, either. They
  should faithfully represent the model the user chose.

What stays the same:

- Matching is done against the VirtualMethodTable (VMT) for each language. This has the appropriate
  information and language-specific metadata needed for matching.

The codegen is now split into two phases; the first is what it does today, and the second is
populating facts for all the models:

- The new `ProgramModelMatches::propagations` field populates `model_paths` and the initial
  `summaries`. This way access paths from propagation models get all the way into models.

- The `access_paths` field gets included the initial indexer paths.

- The bridging model paths are individually added to the indexer paths. At this time we can compose
  the from/to paths and add them to the indexer paths. If a bridge model goes from "Argument(0).foo"
  to "Argument(3).bar" then we can add the access path ".foo.bar".

- The bridging models are also codegen'd directly into facts.
  - A fresh callsite is generated inside the "from" function so that assignments and the call to the
    "to" function can be generated.
  - Each from/to pairing from the model gets read from the formal parameter into a temporary, and
    that temporary is passed as the actual to the call site of the callee.

- The new phase is important because it allows us to handle `AnyArgument` expansion, as by the time
  all the IR is codegen'd, we can consult the actual parameters and call sites.


Some notes:

- Matched/instantiated models are represented by the new `PrograModelMatches` struct, populated by
  matching the `model_generator` specs against loaded IR. It represents an instantiation of all the
  model specs, specialized to the IR to index. We don't have to generate new functions or anything;
  we just lift the modeling concepts into first-class IR concepts.

- The query continues to take a `--models` flag and does its own source/sink matching, issuing a
  single warning that query ignores propagation/bridging models if any were found.

- The matched models stored in the VMT are stored in-memory only, because the IR is modified after
  the index loads the artifacts from disk.

- By default, any bridge model match that isn't unique, i.e., singleton x singleton, gets a warning.
  The warning should specify that one can add "on-multiple-match: ignore" to silence the warning.
  So, by default, it is on-multiple-match: warn. "error" can be used when the user knows this is
  bad. Perhaps there's a better name for this? "cardinality" doesn't seem intuitive for users; is
  there some precedent somewhere?

Notes on incorporating existing designs:

- If `from` matches three methods and `to` matches two implementations, then we want the full cross
  product of matches. 

- For the bridging models:
  - "on-unmatched: ignore|warn|error" is a key that is independently part of the "from" side and the
    "to" side of a bridging model.
  - On the "from" side, on-unmatched: warn is the default
  - On the "to" side, on-unmatched: warn is the default
  - If the "from" side doesn't match anything, then the "to" side isn't even attempted a match. So,
    for example, if the from side has on-unmatched: ignore and it matches nothing, then the "to"
    side won't warn even if there were no matches, because no match is attempted.
  - If the "from" side matches but the "to" side matches nothing, then by default a warning should
    be produced. In other words, the attempt to match the "to" side is conditional on the "from"
    side matching something.

- `forward_self` and `forward_call` are special cases of bridging models, so they should be deleted
  in favor of the bridging models.
