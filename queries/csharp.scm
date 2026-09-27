; Symbol definitions and edge sites for C# (ADR-0003, ADR-0019).

(class_declaration name: (identifier) @name) @def.class
(record_declaration name: (identifier) @name) @def.class
(struct_declaration name: (identifier) @name) @def.struct
(interface_declaration name: (identifier) @name) @def.interface
(enum_declaration name: (identifier) @name) @def.enum
(method_declaration name: (identifier) @name) @def.method
(constructor_declaration name: (identifier) @name) @def.method
(local_function_statement name: (identifier) @name) @def.function
(namespace_declaration name: [(identifier) (qualified_name)] @name) @def.module

; Calls
(invocation_expression function: (identifier) @call)
(invocation_expression function: (member_access_expression name: (identifier) @call))
(object_creation_expression type: (identifier) @call)

; Imports
(using_directive [(qualified_name) (identifier)] @import)
