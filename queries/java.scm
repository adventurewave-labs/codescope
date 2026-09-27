; Symbol definitions and edge sites for Java (ADR-0003, ADR-0019).

(class_declaration name: (identifier) @name) @def.class
(record_declaration name: (identifier) @name) @def.class
(interface_declaration name: (identifier) @name) @def.interface
(enum_declaration name: (identifier) @name) @def.enum
(method_declaration name: (identifier) @name) @def.method
(constructor_declaration name: (identifier) @name) @def.method

; Calls (constructor calls bind to the constructor, which shares the class name)
(method_invocation name: (identifier) @call)
(object_creation_expression type: (type_identifier) @call)

; Imports
(import_declaration (scoped_identifier) @import)
