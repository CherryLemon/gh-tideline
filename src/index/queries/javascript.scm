; Additions to ecma_base.scm and ecma.scm for the JavaScript grammar only.

; class fields holding functions

(class_body
    (field_definition
        property: (property_identifier) @name
        value: [
            (arrow_function)
            (function_expression)
        ]) @definition.method)

; `class A extends B`, `class A extends ns.B`

(class_heritage
    [
        (identifier) @name
        (member_expression
            property: (property_identifier) @name)
    ]) @reference.class
