; Tags for Java, based on tree-sitter-java's queries/tags.scm. When several patterns tag the same name, the one written
; first wins, so definitions come before references and the catch-all type references come last.

; types

(class_declaration
    name: (identifier) @name) @definition.class

(record_declaration
    name: (identifier) @name) @definition.class

(enum_declaration
    name: (identifier) @name) @definition.enum

(interface_declaration
    name: (identifier) @name) @definition.interface

(annotation_type_declaration
    name: (identifier) @name) @definition.interface

; enum constants

(enum_constant
    name: (identifier) @name) @definition.constant

; methods and constructors

(method_declaration
    name: (identifier) @name) @definition.method

(constructor_declaration
    name: (identifier) @name) @definition.constructor

(compact_constructor_declaration
    name: (identifier) @name) @definition.constructor

; calls

(method_invocation
    name: (identifier) @name) @reference.call

; `new Foo()`, `new Foo<>()`, `extends Foo`

(object_creation_expression
    type: [
        (type_identifier) @name
        (generic_type
            (type_identifier) @name)
    ]) @reference.class

(superclass
    [
        (type_identifier) @name
        (generic_type
            (type_identifier) @name)
    ]) @reference.class

; annotations

(marker_annotation
    name: (identifier) @name) @reference.type

(annotation
    name: (identifier) @name) @reference.type

; every other type position (`implements Foo`, declarations, generics, casts), except `var`

((type_identifier) @name @reference.type
    (#not-eq? @name "var"))
