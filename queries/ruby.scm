; Symbol definitions and edge sites for Ruby (ADR-0003, ADR-0019).

(method name: (_) @name) @def.function
(singleton_method name: (_) @name) @def.function
(class name: (constant) @name) @def.class
(class name: (scope_resolution name: (constant) @name)) @def.class
(module name: (constant) @name) @def.module
(module name: (scope_resolution name: (constant) @name)) @def.module

; Calls (require/require_relative are imports, handled below)
(call method: (identifier) @call)
; A paren-less, argument-less call (`save`) is a bare identifier; in statement
; position it is (almost always) a method call, not a local variable read.
(body_statement (identifier) @call)
(then (identifier) @call)
(else (identifier) @call)
(program (identifier) @call)

; Imports
((call
   method: (identifier) @_req
   arguments: (argument_list (string (string_content) @import)))
 (#match? @_req "^require(_relative)?$"))
