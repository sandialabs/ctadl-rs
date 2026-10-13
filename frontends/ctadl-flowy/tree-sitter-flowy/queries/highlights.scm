[
  "var"
  "def"
  "where"
  "summaries"
  "goto"
  "return"
  "ptr"
] @keyword

(param_passing_style) @keyword

(function_def name: (ident) @function)
(function_ptr (ident) @function)
(call_stmt (ap . (ident) @function))
(assign_call_stmt (ap) (ap . (ident) @function) (actuals))

(block label: (ident) @label)
(goto_stmt (ident) @label)

(param (ident) @variable.parameter)
(field_p (ident) @property)

(int) @number
(string) @string
(escape_sequence) @string.escape
(comment) @comment

(summary_op) @operator
["=" ":=" "*"] @operator
["." "," ":" ";"] @punctuation.delimiter
["(" ")" "[" "]" "{" "}" "<" ">"] @punctuation.bracket
