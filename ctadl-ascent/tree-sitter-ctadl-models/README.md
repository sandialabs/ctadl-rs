# tree-sitter-ctadl-models

A tree-sitter grammar for the ctadl model DSL (`.ctadl`), ported from the peg grammar in
`../src/models/dsl/parse.rs`. It accepts the same language, with node names that follow the peg
rules; see the header of `grammar.js` for the details. Nothing in the workspace builds or uses
it; it exists for editor syntax highlighting (`queries/highlights.scm`), and the Zed extension
in `../zed-ctadl-models` uses it.

`src/` is generated. After editing `grammar.js`, regenerate and check the model files:

```sh
nix shell nixpkgs#tree-sitter -c tree-sitter generate
nix shell nixpkgs#tree-sitter -c tree-sitter parse -q --stat ../src/models/defaults/*.ctadl ../tests/tnt/*.models.ctadl
```

`tree-sitter parse -q` can miss an `ERROR` node that error recovery tucked inside an otherwise
complete tree; grep the full tree output for `(ERROR` or `(MISSING` when it matters.
