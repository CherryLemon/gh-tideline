; Tags for Python, based on tree-sitter-python's queries/tags.scm. When several patterns tag the same name, the one
; written first wins, so methods come before the general function pattern.

; class definitions

(class_definition
    name: (identifier) @name) @definition.class

; method definitions: functions directly in a class body, decorated or not

(class_definition
    body: (block
        [
            (function_definition
                name: (identifier) @name) @definition.method
            (decorated_definition
                definition: (function_definition
                    name: (identifier) @name) @definition.method)
        ]))

; function definitions

(function_definition
    name: (identifier) @name) @definition.function

; module-level assignments

(module
    (expression_statement
        (assignment
            left: (identifier) @name) @definition.constant))

; calls and decorators

(call
    function: [
        (identifier) @name
        (attribute
            attribute: (identifier) @name)
    ]) @reference.call

(decorator
    [
        (identifier) @name
        (attribute
            attribute: (identifier) @name)
    ]) @reference.call

; base classes

(class_definition
    superclasses: (argument_list
        [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
            (keyword_argument
                value: (identifier) @name)
        ])) @reference.class

; type annotations, including the generic type in `List[Item]`, except the builtin types

((type
    [
        (identifier) @name
        (attribute
            attribute: (identifier) @name)
    ]) @reference.type
    (#not-match? @name "^(bool|bytes|complex|dict|float|frozenset|int|list|object|set|str|tuple|type)$"))

((generic_type
    .
    (identifier) @name) @reference.type
    (#not-match? @name "^(dict|frozenset|list|set|tuple|type)$"))
