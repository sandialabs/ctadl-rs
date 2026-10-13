(var_def
  "var" @context
  name: (ident) @name) @item

(function_def
  "def" @context
  name: (ident) @name) @item

(block
  label: (ident) @name) @item
