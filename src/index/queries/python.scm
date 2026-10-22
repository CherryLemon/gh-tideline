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

; the parameter types of `Callable[[A, B], C]`, which are a list, except the builtin types

((type
    (list
        [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
        ])) @reference.type
    (#not-match? @name "^(bool|bytes|complex|dict|float|frozenset|int|list|object|set|str|tuple|type)$"))

; names used as values, like a function that is wrapped or kept: passed to a call (`deco(f)`, `Thread(target=f)`),
; returned (`return wrapper`), assigned (`handler = f`, a default `cb=f`) or held (`[f, g]`, `{"a": f}`); a query cannot
; tell a function from a variable, so variables are tagged too, except `self` and `cls`

((call
    arguments: (argument_list
        [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
            (keyword_argument
                value: [
                    (identifier) @name
                    (attribute
                        attribute: (identifier) @name)
                ])
        ])) @reference.value
    (#not-match? @name "^(self|cls)$"))

(([
    (return_statement
        [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
            (expression_list
                [
                    (identifier) @name
                    (attribute
                        attribute: (identifier) @name)
                ])
        ])
    (assignment
        right: [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
            (expression_list
                [
                    (identifier) @name
                    (attribute
                        attribute: (identifier) @name)
                ])
        ])
    (default_parameter
        value: [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
        ])
    (typed_default_parameter
        value: [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
        ])
    (list
        [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
        ])
    (tuple
        [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
        ])
    (set
        [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
        ])
    (pair
        value: [
            (identifier) @name
            (attribute
                attribute: (identifier) @name)
        ])
]) @reference.value
    (#not-match? @name "^(self|cls)$"))

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
