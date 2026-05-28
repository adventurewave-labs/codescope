; Symbol definitions and edge sites for Rust (ADR-0003).
; Capture convention: @def.<kind> on the declaration node, @name on its name,
; @call on a callee identifier, @import on an import path.

(function_item name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(trait_item name: (type_identifier) @name) @def.trait
(type_item name: (type_identifier) @name) @def.type
(const_item name: (identifier) @name) @def.constant
(static_item name: (identifier) @name) @def.constant
(mod_item name: (identifier) @name) @def.module

; Calls
(call_expression function: (identifier) @call)
(call_expression function: (scoped_identifier name: (identifier) @call))
(call_expression function: (field_expression field: (field_identifier) @call))

; Imports
(use_declaration argument: (_) @import)
