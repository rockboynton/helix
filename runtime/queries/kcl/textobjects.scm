(fn_definition body: (function_body . "{" . (_)+ @function.inside "}" .)) @function.around
(fn_expression body: (function_body . "{" . (_)+ @function.inside "}" .)) @function.around
(sketch_expr body: (sketch_body . "{" . (_)+ @class.inside "}" .)) @class.around
(type_declaration) @class.around

(param_list ((param) @parameter.inside . ","? @parameter.around) @parameter.around)
(argument_list ((_) @parameter.inside . ","? @parameter.around) @parameter.around)
(object_expr ((object_property) @entry.inside . ","? @entry.around) @entry.around)
(array_expr ((_) @entry.inside . ","? @entry.around) @entry.around)
[(comment) (block_comment)] @comment.inside
(comment)+ @comment.around
(block_comment) @comment.around
