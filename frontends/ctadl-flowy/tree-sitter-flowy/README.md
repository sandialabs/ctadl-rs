# tree-sitter-flowy

A tree-sitter grammar for flowy (`.tnt`), ported from `../src/flowy.pest`. It accepts the same
language as the pest grammar, with the same rule names. See the header of `grammar.js` for the
pest quirks it reproduces. Nothing in the workspace builds or uses it; it exists for editor syntax
highlighting (`queries/highlights.scm`).

`src/` is generated. After editing `grammar.js`, regenerate and
check the test cases:

```sh
nix shell nixpkgs#tree-sitter -c tree-sitter generate
nix shell nixpkgs#tree-sitter -c tree-sitter parse -q --stat ../../../ctadl-ascent/tests/tnt/*.tnt
```
