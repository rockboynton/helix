[
  (function_body)
  (sketch_body)
  (object_expr)
  (array_expr)
  (argument_list)
  (param_list)
  (parenthesized_expr)
] @indent
(type_declaration "{") @indent
["}" "]" ")"] @outdent

; Pipeline stages continue one level below the first stage.
(binary_expr operator: (binary_operator) @_operator
  (#eq? @_operator "|>")) @indent
