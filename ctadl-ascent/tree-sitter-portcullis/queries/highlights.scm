; Keywords are scoped to the nodes that make them keywords: `name` admits every one of them as
; an ordinary name (a body atom `source(...)`, an attribute `return = 1`), and those should not
; light up.
(out_rel) @keyword
(port_base ["return" "arg" "param"] @keyword)
(attr_op "in" @keyword)

(rel_atom name: (name) @function)
(closure) @operator
(attr name: (name) @property)
(segment) @property

(var) @variable
(wildcard) @variable.special

(int) @number
(bool) @boolean
(string) @string
(escape_sequence) @string.escape
(comment) @comment

(flow_op) @operator
(attr_op ["<=" ">=" "!=" "<-" "<" ">" "="] @operator)
["&&" "||" "!" "=" ":-"] @operator
["." "," ";" "::"] @punctuation.delimiter
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
