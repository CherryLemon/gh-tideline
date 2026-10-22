; Tags for Rust, based on tree-sitter-rust's queries/tags.scm. When several patterns tag the same name, the one written
; first wins, so definitions come before references and the catch-all type references come last.

; ADT definitions

(struct_item
    name: (type_identifier) @name) @definition.class

(enum_item
    name: (type_identifier) @name) @definition.class

(union_item
    name: (type_identifier) @name) @definition.class

; type aliases

(type_item
    name: (type_identifier) @name) @definition.class

; trait definitions

(trait_item
    name: (type_identifier) @name) @definition.interface

; constants and statics

(const_item
    name: (identifier) @name) @definition.constant

(static_item
    name: (identifier) @name) @definition.constant

; method definitions: functions in impl and trait blocks, and trait methods without a body

(impl_item
    body: (declaration_list
        (function_item
            name: (identifier) @name) @definition.method))

(trait_item
    body: (declaration_list
        (function_item
            name: (identifier) @name) @definition.method))

(trait_item
    body: (declaration_list
        (function_signature_item
            name: (identifier) @name) @definition.method))

; function definitions (also in mod blocks and extern blocks)

(function_item
    name: (identifier) @name) @definition.function

(function_signature_item
    name: (identifier) @name) @definition.function

; module definitions

(mod_item
    name: (identifier) @name) @definition.module

; macro definitions

(macro_definition
    name: (identifier) @name) @definition.macro

; implementations: the trait of `impl Trait for Type`, the type of `impl Type` (the tag covers the whole impl block)

(impl_item
    trait: [
        (type_identifier) @name
        (generic_type
            type: (type_identifier) @name)
        (scoped_type_identifier
            name: (type_identifier) @name)
        (generic_type
            type: (scoped_type_identifier
                name: (type_identifier) @name))
    ]) @reference.implementation

(impl_item
    type: [
        (type_identifier) @name
        (generic_type
            type: (type_identifier) @name)
        (scoped_type_identifier
            name: (type_identifier) @name)
        (generic_type
            type: (scoped_type_identifier
                name: (type_identifier) @name))
    ]
    !trait) @reference.implementation

; calls

(call_expression
    function: [
        (identifier) @name
        (field_expression
            field: (field_identifier) @name)
        (scoped_identifier
            name: (identifier) @name)
        (generic_function
            function: [
                (identifier) @name
                (field_expression
                    field: (field_identifier) @name)
                (scoped_identifier
                    name: (identifier) @name)
            ])
    ]) @reference.call

(macro_invocation
    macro: [
        (identifier) @name
        (scoped_identifier
            name: (identifier) @name)
    ]) @reference.call

; capitalized path segments: the type in `Parser::new`, `a::Kind::B`, and the last segment of `use a::Parser`

((scoped_identifier
    path: (identifier) @name) @reference.type
    (#match? @name "^[A-Z]")
    (#not-eq? @name "Self"))

((scoped_identifier
    path: (scoped_identifier
        name: (identifier) @name)) @reference.type
    (#match? @name "^[A-Z]"))

((scoped_identifier
    name: (identifier) @name) @reference.type
    (#match? @name "^[A-Z]"))

; names used as values, like a function that is passed or kept: passed to a call (`.map(parse)`, `spawn(Self::run)`),
; returned (also as the last expression of a function), assigned (`let f = parse;`) or held (`[a, b]`,
; `Handler { run: parse }`, `Handler { parse }`); a query cannot tell a function from a variable, so variables are
; tagged too, except `None`. Written after the capitalized path segments, which keep `Kind::B` a type.

(([
    (arguments
        [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
            (scoped_identifier
                name: (identifier) @name)
        ])
    (return_expression
        [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
            (scoped_identifier
                name: (identifier) @name)
        ])
    (function_item
        body: (block
            [
                (identifier) @name
                (field_expression
                    field: (field_identifier) @name)
                (scoped_identifier
                    name: (identifier) @name)
            ]
            .))
    (let_declaration
        value: [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
            (scoped_identifier
                name: (identifier) @name)
        ])
    (assignment_expression
        right: [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
            (scoped_identifier
                name: (identifier) @name)
        ])
    (const_item
        value: [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
            (scoped_identifier
                name: (identifier) @name)
        ])
    (static_item
        value: [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
            (scoped_identifier
                name: (identifier) @name)
        ])
    (field_initializer
        value: [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
            (scoped_identifier
                name: (identifier) @name)
        ])
    (shorthand_field_initializer
        (identifier) @name)
    (array_expression
        [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
            (scoped_identifier
                name: (identifier) @name)
        ])
    (tuple_expression
        [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
            (scoped_identifier
                name: (identifier) @name)
        ])
]) @reference.value
    (#not-eq? @name "None"))

; every other type position

((type_identifier) @name @reference.type
    (#not-match? @name "^(Self|_)$"))
