# Zed extension for the ctadl model DSL (.ctadl)

Tree-sitter syntax highlighting, bracket matching, outline, and indentation for model files.
There is no language server.

The grammar is `../tree-sitter-ctadl-models`, which Zed fetches from GitHub at the `rev` pinned
in `extension.toml`. After changing the grammar, push the commit and bump `rev`.
`languages/ctadl-models/highlights.scm` is a copy of
`../tree-sitter-ctadl-models/queries/highlights.scm`, so keep the two in sync.

Only `.ctadl` is claimed. The analyzer also reads `.dl`, but that suffix belongs to Soufflé and
other Datalogs; to use this extension for a `.dl` model file, add it under `file_types` in Zed's
settings.

To install it, run `zed: install dev extension` in Zed and select this directory.
