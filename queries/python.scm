; Symbol definitions and edge sites for Python (ADR-0003).

(function_definition name: (identifier) @name) @def.function
(class_definition name: (identifier) @name) @def.class

; Calls
(call function: (identifier) @call)
(call function: (attribute attribute: (identifier) @call))

; Imports
(import_statement name: (dotted_name) @import)
(import_from_statement module_name: (dotted_name) @import)
