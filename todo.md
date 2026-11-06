# TODO

Each item removes rows of the approximations register in
[docs/limitations.md](docs/limitations.md). Rule numbers (S*) are those of
[docs/ir.md](docs/ir.md).

- **Load-chain composition as an IR pass.** Codegen's load-chain composition (`cap_path`,
  `ctadl-ascent/src/codegen/mod.rs`) moves into the last IR pass before codegen. The admissible
  path set depends on it, because the set is built from the paths the program mentions; that
  dependence is stated and tested in the pass.
- **Index equal to the semantics.** `locals` gains a scope column (`Fixed`/`All`, stored like
  `Cond`). One backward rule of S8's shape replaces the formal-side rules (`edge_split`,
  `ext_fml`) and the alias summary rule (`alias_of_formal` + `prog_store`). `summary`'s `out`
  is decided by scope, retiring `FormalType` and `isout`. Call sites become one-way copies,
  with the backward rule carrying the write-back. Target and string labels share the rule text
  by macro. This removes the index rows of the register that come from these rules: the
  backward alias rule, `Shared` formals and `Copied` formals.
- **Query equal to the semantics.** `labeled_successors` (`ctadl-ascent/src/query_engine/search.rs`)
  is restated as S6–S8, and saturating sources and wildcard sinks become endpoint expansions.
  This removes the query rows: the object hop, and saturating sources and wildcard sinks.
- **Exception propagation in JVM and Dex.** Pass a callee's escaping exception on to the
  caller's handler (the JVM and Dex entry in [docs/limitations.md](docs/limitations.md);
  branch `jvm-exception-propagation`).
