; Symbol definitions and edge sites for C (ADR-0003, ADR-0019).

(function_definition declarator: (function_declarator declarator: (identifier) @name)) @def.function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @def.function
(struct_specifier name: (type_identifier) @name body: (_)) @def.struct
(union_specifier name: (type_identifier) @name body: (_)) @def.struct
(enum_specifier name: (type_identifier) @name body: (_)) @def.enum
(type_definition declarator: (type_identifier) @name) @def.type

; Calls
(call_expression function: (identifier) @call)
(call_expression function: (field_expression field: (field_identifier) @call))

; Imports
(preproc_include path: (_) @import)
