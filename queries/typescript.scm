; Symbol definitions and edge sites for TypeScript (ADR-0003).

(function_declaration name: (identifier) @name) @def.function
(class_declaration name: (type_identifier) @name) @def.class
(method_definition name: (property_identifier) @name) @def.method
(interface_declaration name: (type_identifier) @name) @def.interface
(type_alias_declaration name: (type_identifier) @name) @def.type
(enum_declaration name: (identifier) @name) @def.enum
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)]) @def.function

; Calls
(call_expression function: (identifier) @call)
(call_expression function: (member_expression property: (property_identifier) @call))

; Imports
(import_statement source: (string) @import)
