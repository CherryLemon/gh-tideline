; Tags for C, replacing tree-sitter-c's queries/tags.scm: functions are tagged on the whole definition (upstream tags the
; declarator, so a scope ends at the signature), and calls and type uses are references (upstream has none). When several
; patterns tag the same name, the one written first wins, so definitions come before references.

; function definitions; declarations without a body are not definitions. The name is found by following the
; `declarator` fields from the function down to its function declarator, which never leads into the parameters (a
; wildcard that matches any child would tag `cb` for `void f(int cb(int)) {}`). A pattern cannot repeat, so every
; depth is written out: up to five pointer declarators around the function declarator (`char **f(void)`) ...

(function_definition
    declarator: (function_declarator
        declarator: (identifier) @name)) @definition.function

(function_definition
    declarator: (_
        declarator: (function_declarator
            declarator: (identifier) @name))) @definition.function

(function_definition
    declarator: (_
        declarator: (_
            declarator: (function_declarator
                declarator: (identifier) @name)))) @definition.function

(function_definition
    declarator: (_
        declarator: (_
            declarator: (_
                declarator: (function_declarator
                    declarator: (identifier) @name))))) @definition.function

(function_definition
    declarator: (_
        declarator: (_
            declarator: (_
                declarator: (_
                    declarator: (function_declarator
                        declarator: (identifier) @name)))))) @definition.function

(function_definition
    declarator: (_
        declarator: (_
            declarator: (_
                declarator: (_
                    declarator: (_
                        declarator: (function_declarator
                            declarator: (identifier) @name))))))) @definition.function

; ... and a function that returns a function pointer or a pointer to an array, whose name is inside parentheses
; (`int (*f(void))(int)`, `char *(**f(void))[3]`), with up to one pointer declarator in front and two inside

(function_definition
    declarator: (_
        declarator: (parenthesized_declarator
            (_
                declarator: [
                    (function_declarator
                        declarator: (identifier) @name)
                    (_
                        declarator: (function_declarator
                            declarator: (identifier) @name))
                ])))) @definition.function

(function_definition
    declarator: (_
        declarator: (_
            declarator: (parenthesized_declarator
                (_
                    declarator: [
                        (function_declarator
                            declarator: (identifier) @name)
                        (_
                            declarator: (function_declarator
                                declarator: (identifier) @name))
                    ]))))) @definition.function

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

; names used as values, like a function that is passed or kept: passed to a call (`qsort(v, n, size, compare)`),
; returned, assigned (`handler_t h = on_exit;`) or held in an initializer (`{ .open = dev_open }`); a query cannot tell a
; function from a variable, so variables are tagged too (`NULL` is no identifier)

([
    (argument_list
        [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
        ])
    (return_statement
        [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
        ])
    (init_declarator
        value: [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
        ])
    (assignment_expression
        right: [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
        ])
    (initializer_list
        [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
        ])
    (initializer_pair
        value: [
            (identifier) @name
            (field_expression
                field: (field_identifier) @name)
        ])
]) @reference.value

; every type position (defined types are matched by the patterns above)

(type_identifier) @name @reference.type
