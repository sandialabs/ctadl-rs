# Zed extension for Portcullis, the ctadl model language (.ctadl)

Tree-sitter syntax highlighting, bracket matching, outline, and indentation for model files.
There is no language server.

The grammar is `../tree-sitter-portcullis`, which Zed fetches from GitHub at the `rev` pinned
in `extension.toml`. After changing the grammar, push the commit and bump `rev`.
`languages/portcullis/highlights.scm` is a copy of
`../tree-sitter-portcullis/queries/highlights.scm`, so keep the two in sync.

Only `.ctadl` is claimed. The analyzer also reads `.dl`, but that suffix belongs to Soufflé and
other Datalogs; to use this extension for a `.dl` model file, add it under `file_types` in Zed's
settings.

To install it, run `zed: install dev extension` in Zed and select this directory.
