; The base of the JavaScript, TypeScript and TSX queries: the tags query of tree-sitter-javascript 0.25.0, without the
; `(comment)* @doc .` prefix that upstream puts on five of its patterns to collect doc comments (and without the
; `#strip!` and `#select-adjacent!` predicates that only served those). We keep no doc comments, and tree-sitter needs
; time quadratic in the length of a run of consecutive comments to match the prefix: 10 000 comment lines take a second.

((method_definition
    name: (property_identifier) @name) @definition.method
    (#not-eq? @name "constructor"))

[
    (class
        name: (_) @name)
    (class_declaration
        name: (_) @name)
] @definition.class

[
    (function_expression
        name: (identifier) @name)
    (function_declaration
        name: (identifier) @name)
    (generator_function
        name: (identifier) @name)
    (generator_function_declaration
        name: (identifier) @name)
] @definition.function

(lexical_declaration
    (variable_declarator
        name: (identifier) @name
        value: [(arrow_function) (function_expression)]) @definition.function)

(variable_declaration
    (variable_declarator
        name: (identifier) @name
        value: [(arrow_function) (function_expression)]) @definition.function)

(assignment_expression
    left: [
        (identifier) @name
        (member_expression
            property: (property_identifier) @name)
    ]
    right: [(arrow_function) (function_expression)]) @definition.function

(pair
    key: (property_identifier) @name
    value: [(arrow_function) (function_expression)]) @definition.function

((call_expression
    function: (identifier) @name) @reference.call
    (#not-match? @name "^(require)$"))

(call_expression
    function: (member_expression
        property: (property_identifier) @name)
    arguments: (_) @reference.call)

(new_expression
    constructor: (_) @name) @reference.class

(export_statement
    value: (assignment_expression
        left: (identifier) @name
        right: [
            (number)
            (string)
            (identifier)
            (undefined)
            (null)
            (new_expression)
            (binary_expression)
            (call_expression)
        ])) @definition.constant
