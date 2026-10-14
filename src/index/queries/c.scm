; Tags for C, replacing tree-sitter-c's queries/tags.scm: functions are tagged on the whole definition (upstream tags the
; declarator, so a scope ends at the signature), and calls and type uses are references (upstream has none). When several
; patterns tag the same name, the one written first wins, so definitions come before references.

; function definitions, with the declarator possibly wrapped in pointer or parenthesized declarators
; (`char *f(void)`, `int (*f(void))(int)`); declarations without a body are not definitions

(function_definition
    declarator: (function_declarator
        declarator: (identifier) @name)) @definition.function

(function_definition
    declarator: (_
        (function_declarator
            declarator: (identifier) @name))) @definition.function

(function_definition
    declarator: (_
        (_
            (function_declarator
                declarator: (identifier) @name)))) @definition.function

(function_definition
    declarator: (_
        (_
            (_
                (function_declarator
                    declarator: (identifier) @name))))) @definition.function

; struct, union and enum definitions (without a body they are only uses of the type)

(struct_specifier
    name: (type_identifier) @name
    body: (_)) @definition.class

(union_specifier
    name: (type_identifier) @name
    body: (_)) @definition.class

(enum_specifier
    name: (type_identifier) @name
    body: (_)) @definition.enum

(enumerator
    name: (identifier) @name) @definition.constant

; typedefs, including pointer, array and function pointer types

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
            field: (field_identifier) @name)
        (parenthesized_expression
            (identifier) @name)
    ]) @reference.call

; every type position (defined types are matched by the patterns above)

(type_identifier) @name @reference.type
