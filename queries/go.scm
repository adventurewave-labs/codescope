; Symbol definitions and edge sites for Go (ADR-0003).

(function_declaration name: (identifier) @name) @def.function
(method_declaration name: (field_identifier) @name) @def.method
; Interface declarations are refined from @def.type to Interface in the
; extractor (ADR-0024) — two @def patterns on one node would fight over the
; last-match rule, so the query stays single-purpose.
(type_spec name: (type_identifier) @name) @def.type
(const_spec name: (identifier) @name) @def.constant

; Calls
(call_expression function: (identifier) @call)
(call_expression function: (selector_expression field: (field_identifier) @call))

; Imports
(import_spec path: (interpreted_string_literal) @import)
