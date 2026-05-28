; Symbol definitions and edge sites for Go (ADR-0003).

(function_declaration name: (identifier) @name) @def.function
(method_declaration name: (field_identifier) @name) @def.method
(type_spec name: (type_identifier) @name) @def.type
(const_spec name: (identifier) @name) @def.constant

; Calls
(call_expression function: (identifier) @call)
(call_expression function: (selector_expression field: (field_identifier) @call))

; Imports
(import_spec path: (interpreted_string_literal) @import)
