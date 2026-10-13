# Zed extension for flowy (.tnt)

Tree-sitter syntax highlighting, bracket matching, outline, and indentation for flowy test
cases. There is no language server.

The grammar is `../tree-sitter-flowy`, which Zed fetches from GitHub at the `rev` pinned in
`extension.toml`. After changing the grammar, push the commit and bump `rev`.
`languages/flowy/highlights.scm` is a copy of `../tree-sitter-flowy/queries/highlights.scm`, so
keep the two in sync.

To install it, run `zed: install dev extension` in Zed and select this directory.
