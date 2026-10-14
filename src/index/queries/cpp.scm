; Tags for C++, replacing tree-sitter-cpp's queries/tags.scm: functions are tagged on the whole definition (upstream tags
; the declarator, so a scope ends at the signature), namespaces are definitions, and calls and type uses are references
; (upstream has none). When several patterns tag the same name, the one written first wins, so definitions come before
; references and constructors before the general function and method patterns.

; constructors defined in a class body: the function has the name of the class (`@local.reference` is a capture the
; tags crate ignores; it only serves to compare the two names)

((class_specifier
    name: (type_identifier) @local.reference
    body: (field_declaration_list
        (function_definition
            declarator: (function_declarator
                declarator: (identifier) @name)) @definition.constructor))
    (#eq? @local.reference @name))

((struct_specifier
    name: (type_identifier) @local.reference
    body: (field_declaration_list
        (function_definition
            declarator: (function_declarator
                declarator: (identifier) @name)) @definition.constructor))
    (#eq? @local.reference @name))

; constructors defined outside of their class (`Foo::Foo() {}`, `ns::Foo::Foo() {}`)

((function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            scope: (namespace_identifier) @local.reference
            name: (identifier) @name))) @definition.constructor
    (#eq? @local.reference @name))

((function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (qualified_identifier
                scope: (namespace_identifier) @local.reference
                name: (identifier) @name)))) @definition.constructor
    (#eq? @local.reference @name))

((function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (qualified_identifier
                name: (qualified_identifier
                    scope: (namespace_identifier) @local.reference
                    name: (identifier) @name))))) @definition.constructor
    (#eq? @local.reference @name))

; function definitions, with the declarator possibly wrapped in pointer or reference declarators
; (`Foo *f()`, `const Foo &Bar::g() const`); declarations without a body are not definitions

(function_definition
    declarator: (function_declarator
        declarator: [
            (identifier) @name
            (template_function
                name: (identifier) @name)
        ])) @definition.function

(function_definition
    declarator: (_
        (function_declarator
            declarator: [
                (identifier) @name
                (template_function
                    name: (identifier) @name)
            ]))) @definition.function

(function_definition
    declarator: (_
        (_
            (function_declarator
                declarator: [
                    (identifier) @name
                    (template_function
                        name: (identifier) @name)
                ])))) @definition.function

; method definitions: in a class body, qualified (`Foo::bar`, `ns::Foo::bar`), destructors and operators

(function_definition
    declarator: (function_declarator
        declarator: [
            (field_identifier) @name
            (destructor_name) @name
            (operator_name) @name
            (qualified_identifier
                name: [
                    (identifier)
                    (destructor_name)
                    (operator_name)
                    (template_function
                        name: (identifier))
                ] @name)
            (qualified_identifier
                name: (qualified_identifier
                    name: [
                        (identifier)
                        (destructor_name)
                        (operator_name)
                    ] @name))
            (qualified_identifier
                name: (qualified_identifier
                    name: (qualified_identifier
                        name: [
                            (identifier)
                            (destructor_name)
                            (operator_name)
                        ] @name)))
        ])) @definition.method

(function_definition
    declarator: (_
        (function_declarator
            declarator: [
                (field_identifier) @name
                (destructor_name) @name
                (operator_name) @name
                (qualified_identifier
                    name: [
                        (identifier)
                        (destructor_name)
                        (operator_name)
                        (template_function
                            name: (identifier))
                    ] @name)
                (qualified_identifier
                    name: (qualified_identifier
                        name: [
                            (identifier)
                            (destructor_name)
                            (operator_name)
                        ] @name))
                (qualified_identifier
                    name: (qualified_identifier
                        name: (qualified_identifier
                            name: [
                                (identifier)
                                (destructor_name)
                                (operator_name)
                            ] @name)))
            ]))) @definition.method

(function_definition
    declarator: (_
        (_
            (function_declarator
                declarator: [
                    (field_identifier) @name
                    (destructor_name) @name
                    (operator_name) @name
                    (qualified_identifier
                        name: [
                            (identifier)
                            (destructor_name)
                            (operator_name)
                            (template_function
                                name: (identifier))
                        ] @name)
                    (qualified_identifier
                        name: (qualified_identifier
                            name: [
                                (identifier)
                                (destructor_name)
                                (operator_name)
                            ] @name))
                ])))) @definition.method

; class, struct, union and enum definitions (without a body they are only uses of the type)

(class_specifier
    name: [
        (type_identifier) @name
        (template_type
            name: (type_identifier) @name)
    ]
    body: (_)) @definition.class

(struct_specifier
    name: [
        (type_identifier) @name
        (template_type
            name: (type_identifier) @name)
    ]
    body: (_)) @definition.class

(union_specifier
    name: (type_identifier) @name
    body: (_)) @definition.class

(enum_specifier
    name: (type_identifier) @name
    body: (_)) @definition.enum

(enumerator
    name: (identifier) @name) @definition.constant

; namespaces; for `namespace a::b::c` the last name

(namespace_definition
    name: (namespace_identifier) @name) @definition.namespace

(namespace_definition
    name: (nested_namespace_specifier
        (namespace_identifier) @name .)) @definition.namespace

(namespace_definition
    name: (nested_namespace_specifier
        (nested_namespace_specifier
            (namespace_identifier) @name .))) @definition.namespace

(namespace_definition
    name: (nested_namespace_specifier
        (nested_namespace_specifier
            (nested_namespace_specifier
                (namespace_identifier) @name .)))) @definition.namespace

; typedefs and aliases, including pointer, array and function pointer types

(type_definition
    declarator: (type_identifier) @name) @definition.type

(type_definition
    declarator: (_
        declarator: (type_identifier) @name)) @definition.type

(type_definition
    declarator: (_
        declarator: (_
            declarator: (type_identifier) @name))) @definition.type

(type_definition
    declarator: (function_declarator
        declarator: (parenthesized_declarator
            (pointer_declarator
                declarator: (type_identifier) @name)))) @definition.type

(alias_declaration
    name: (type_identifier) @name) @definition.type

; macros

(preproc_def
    name: (identifier) @name) @definition.macro

(preproc_function_def
    name: (identifier) @name) @definition.macro

; calls

(call_expression
    function: [
        (identifier) @name
        (field_expression
            field: [
                (field_identifier) @name
                (template_method
                    name: (field_identifier) @name)
            ])
        (parenthesized_expression
            (identifier) @name)
        (template_function
            name: (identifier) @name)
        (qualified_identifier
            name: [
                (identifier)
                (template_function
                    name: (identifier))
            ] @name)
        (qualified_identifier
            name: (qualified_identifier
                name: [
                    (identifier)
                    (template_function
                        name: (identifier))
                ] @name))
        (qualified_identifier
            name: (qualified_identifier
                name: (qualified_identifier
                    name: [
                        (identifier)
                        (template_function
                            name: (identifier))
                    ] @name)))
    ]) @reference.call

; class, namespace and type names in qualified names (`Foo::bar`, `ns::f`), except std

((namespace_identifier) @name @reference.type
    (#not-eq? @name "std"))

; every type position (defined types are matched by the patterns above)

(type_identifier) @name @reference.type
