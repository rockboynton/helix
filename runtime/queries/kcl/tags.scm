(fn_definition name: (identifier) @name) @definition.function
(non_fn_definition name: (identifier) @name value: (fn_expression)) @definition.function
(non_fn_definition name: (identifier) @name) @definition.constant
(type_declaration name: (identifier) @name) @definition.type
(enum_variant name: (identifier) @name) @definition.enum
(fn_call callee: (identifier) @name) @reference.call
(fn_call callee: (namespace_expr name: (identifier) @name)) @reference.call
