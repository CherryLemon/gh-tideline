; Additions to ecma_base.scm (appended to it) that are valid in the JavaScript, TypeScript and TSX grammars. Upstream
; defines only function-valued variables and no constructors.

; module-level variables that are not functions (function-valued ones are matched by the upstream patterns)

(program
    (lexical_declaration
        (variable_declarator
            name: (identifier) @name) @definition.constant))

(program
    (variable_declaration
        (variable_declarator
            name: (identifier) @name) @definition.constant))

(program
    (export_statement
        declaration: (lexical_declaration
            (variable_declarator
                name: (identifier) @name) @definition.constant)))

(program
    (export_statement
        declaration: (variable_declaration
            (variable_declarator
                name: (identifier) @name) @definition.constant)))

; constructors (upstream skips them)

((method_definition
    name: (property_identifier) @name) @definition.constructor
    (#eq? @name "constructor"))

; `new ns.Foo()`

(new_expression
    constructor: (member_expression
        property: (property_identifier) @name)) @reference.class
