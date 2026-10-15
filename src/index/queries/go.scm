; Tags for Go, based on tree-sitter-go's queries/tags.scm. When several patterns tag the same name, the one written
; first wins, so definitions come before references and the catch-all type references come last.

; functions and methods (`method_elem` is a method of an interface)

(function_declaration
    name: (identifier) @name) @definition.function

(method_declaration
    name: (field_identifier) @name) @definition.method

(method_elem
    name: (field_identifier) @name) @definition.method

; types

(type_spec
    name: (type_identifier) @name
    type: (struct_type)) @definition.class

(type_spec
    name: (type_identifier) @name
    type: (interface_type)) @definition.interface

(type_spec
    name: (type_identifier) @name) @definition.type

(type_alias
    name: (type_identifier) @name) @definition.type

; package-level constants and variables (without the `name:` field, `const A, B = 1, 2` yields both names)

(source_file
    (const_declaration
        (const_spec
            (identifier) @name) @definition.constant))

(source_file
    (var_declaration
        (var_spec
            (identifier) @name) @definition.variable))

(source_file
    (var_declaration
        (var_spec_list
            (var_spec
                (identifier) @name) @definition.variable)))

; calls

(call_expression
    function: [
        (identifier) @name
        (parenthesized_expression
            (identifier) @name)
        (selector_expression
            field: (field_identifier) @name)
        (parenthesized_expression
            (selector_expression
                field: (field_identifier) @name))
        (index_expression
            operand: (identifier) @name)
        (index_expression
            operand: (selector_expression
                field: (field_identifier) @name))
    ]) @reference.call

; `Map[int](xs)` parses as a conversion to a generic type

(type_conversion_expression
    type: (generic_type
        type: [
            (type_identifier) @name
            (qualified_type
                name: (type_identifier) @name)
        ])) @reference.call

; every type position except the predeclared types (declared types are matched by the patterns above)

((type_identifier) @name @reference.type
    (#not-match? @name "^(any|bool|byte|comparable|complex64|complex128|error|float32|float64|int|int8|int16|int32|int64|rune|string|uint|uint8|uint16|uint32|uint64|uintptr)$"))
