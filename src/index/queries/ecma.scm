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

; names used as values, like a function that is passed or kept: passed to a call or `new` (`items.map(render)`,
; `addEventListener("click", this.onClick)`), returned, assigned (`const f = render`, `img.onload = done`, a default
; `cb = noop`, also in `{ cb = noop }`) or held (`[a, b]`, `{ run: render }`, `{ render }`); a query cannot tell a
; function from a variable, so variables are tagged too

([
    (arguments
        [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (return_statement
        [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (variable_declarator
        value: [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (assignment_expression
        right: [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (assignment_pattern
        right: [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (object_assignment_pattern
        right: [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (array
        [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (pair
        value: [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (shorthand_property_identifier) @name
]) @reference.value
