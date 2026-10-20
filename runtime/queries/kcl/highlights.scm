; General identifiers first; contextual captures take precedence below.
(identifier) @variable

["import" "from" "as" "export"] @keyword.control.import
"fn" @keyword.function
"return" @keyword.control.return
["if" "else"] @keyword.control.conditional
"type" @keyword.storage.type
"var" @keyword.storage.modifier
"sketch" @keyword

(fn_definition name: (identifier) @function)
(non_fn_definition name: (identifier) @function value: (fn_expression))
(param name: (identifier) @variable.parameter)
(member_expr property: (identifier) @variable.other.member)
(fn_call callee: (identifier) @function)
(fn_call callee: (namespace_expr name: (identifier) @function))
(fn_call callee: (member_expr property: (identifier) @function.method))
(labeledArg label: (identifier) @variable.parameter)
(object_property key: (identifier) @variable.other.member)
(namespace_expr namespace: (identifier) @namespace)
(type_name (identifier) @type)
(type_declaration name: (identifier) @type)
(enum_variant name: (identifier) @type.enum.variant)
(annotation "@" @attribute name: (identifier) @attribute)
(annotation_kv key: (identifier) @variable.parameter)
(tag) @label

((identifier) @constant.builtin
  (#any-of? @constant.builtin "XY" "XZ" "YZ" "X" "Y" "Z" "PI" "TAU" "E" "CW" "CCW" "START" "END"))

(binary_operator) @operator
(prefix_operator) @operator
["=" ".." "|"] @operator
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," "." ":" "::"] @punctuation.delimiter
["@" "?" "$" ";"] @punctuation.special
(boolean) @constant.builtin.boolean
(number) @constant.numeric
(unit) @type.builtin
(string) @string
(escape_sequence) @constant.character.escape
(shebang) @keyword.directive
[(comment) (block_comment)] @comment
