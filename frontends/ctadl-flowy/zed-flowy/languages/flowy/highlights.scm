[
  "var"
  "def"
  "where"
  "summaries"
  "goto"
  "return"
  "ptr"
] @keyword

(param_type) @keyword

(function_def name: (ident) @function)
(function_ptr name: (ident) @function)
(call_stmt callee: (path . base: (ident) @function))
(assign_call_stmt callee: (path . base: (ident) @function))

(block label: (ident) @label)
(goto_stmt target: (ident) @label)

(param name: (ident) @variable.parameter)
(field (ident) @property)

(int) @number
(string) @string
(escape_sequence) @string.escape
(comment) @comment

(summary_op) @operator
["=" ":=" "*"] @operator
["." "," ":" ";"] @punctuation.delimiter
["(" ")" "[" "]" "{" "}" "<" ">"] @punctuation.bracket
