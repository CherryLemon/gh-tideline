; Components used in JSX (the JavaScript and TSX grammars): `<Foo>`, `<Foo />`, `<ns.Foo>`.

((jsx_opening_element
    name: [
        (identifier) @name
        (member_expression
            property: (property_identifier) @name)
    ]) @reference.class
    (#match? @name "^[A-Z]"))

((jsx_self_closing_element
    name: [
        (identifier) @name
        (member_expression
            property: (property_identifier) @name)
    ]) @reference.class
    (#match? @name "^[A-Z]"))
