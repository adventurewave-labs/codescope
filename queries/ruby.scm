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
; Same for the right operand of a binary expression (`total * tax_rate`): the
; left operand is usually the accumulator/variable being combined, so only the
; right side is captured. The extractor drops captures that name a parameter
; or assigned local of the enclosing def (they are reads, not calls).
(binary right: (identifier) @call)

; Imports
((call
   method: (identifier) @_req
   arguments: (argument_list (string (string_content) @import)))
 (#match? @_req "^require(_relative)?$"))
