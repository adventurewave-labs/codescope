; Symbol definitions and edge sites for JavaScript (ADR-0003).

(function_declaration name: (identifier) @name) @def.function
(class_declaration name: (identifier) @name) @def.class
(method_definition name: (property_identifier) @name) @def.method
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)]) @def.function

; Calls
(call_expression function: (identifier) @call)
(call_expression function: (member_expression property: (property_identifier) @call))

; Imports
(import_statement source: (string) @import)
