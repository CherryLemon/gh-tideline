; Additions to ecma_base.scm and the upstream TypeScript tags query for the TypeScript and TSX grammars. Upstream has no
; type aliases, enums, namespaces or class fields holding functions, and no references to types other than type
; annotations. When several patterns tag the same name, the one written first wins, so the catch-all type references
; come last.

; type aliases, enums and namespaces

(type_alias_declaration
    name: (type_identifier) @name) @definition.type

(enum_declaration
    name: (identifier) @name) @definition.enum

(internal_module
    name: (identifier) @name) @definition.module

; class fields holding functions

(class_body
    (public_field_definition
        name: (property_identifier) @name
        value: [
            (arrow_function)
            (function_expression)
        ]) @definition.method)

; `class A extends B`, `class A extends ns.B`

(extends_clause
    value: [
        (identifier) @name
        (member_expression
            property: (property_identifier) @name)
    ]) @reference.class

; class fields and default parameters holding a name, like a function that is kept (`handler = this.onClick`,
; `cb = noop`)

([
    (public_field_definition
        value: [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (required_parameter
        value: [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
    (optional_parameter
        value: [
            (identifier) @name
            (member_expression
                property: (property_identifier) @name)
        ])
]) @reference.value

; every other type position (`implements Foo`, generics, assertions, ...)

(type_identifier) @name @reference.type
