# My plan for overhauling models - DO-NOT-MERGE

BLUF: Overhaul the model matching so that it is done after linking multiple IRs (imports) together,
before codegen and the index phase. Then save out a small description of all the models actually
matched alongside the index so that the query phase can pick those up.

One constraint:

- All model matching is done on the IR, irrespective of (and before) codegen.
- It is done during the index phase, after loading all the IR from all the artifacts. This way, we
  have all the code and all the VMTs.

Three new mechanisms:

- Add a "summaries" field to the VMT that can be used to express propagation models, their
  associated access paths, etc. Codegen can use this directly to generate code and record the
  relevant access paths.
- Add a "paths" field to the ProgramInfo that records a set of access paths. This field is used to
  express access paths that *do not* occur syntactically in the IR. In particular in can be used to
  deal with composed access paths due to composition in bridging models
- Bridging models generate into a new VMT field "bridges" that, like summaries, is basically a
  native representation of exactly what bridging models express -- how the caller's method's
  parameters (and subfields) should be shuttled into the actuals of a call. By implemented a new
  field, "bridges," we don't need to worry about turning bridging models into IR and back -- we just
  codegen them directly. Briding models don't need to be optimized, like real IR code, either. They
  should faithfully represent the model the user chose.

What stays the same:

- Matching is done against the VirtualMethodTable (VMT) for each language. This has the appropriate
  information and language-specific metadata needed for matching.
