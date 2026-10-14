//! The languages the index understands: grammars, the families that share names, and which paths are parsed.

/// A tree-sitter grammar. TSX is separate from TypeScript because it parses JSX.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Grammar {
    Rust,
    Go,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Java,
    C,
    Cpp,
}

impl Grammar {
    /// In declaration order, so `grammar as usize` indexes this array.
    pub const ALL: [Grammar; 9] = [
        Grammar::Rust,
        Grammar::Go,
        Grammar::Python,
        Grammar::TypeScript,
        Grammar::Tsx,
        Grammar::JavaScript,
        Grammar::Java,
        Grammar::C,
        Grammar::Cpp,
    ];

    /// The name used in API responses and in the index.
    pub fn id(self) -> &'static str {
        match self {
            Grammar::Rust => "rust",
            Grammar::Go => "go",
            Grammar::Python => "python",
            Grammar::TypeScript => "typescript",
            Grammar::Tsx => "tsx",
            Grammar::JavaScript => "javascript",
            Grammar::Java => "java",
            Grammar::C => "c",
            Grammar::Cpp => "cpp",
        }
    }

    pub fn from_id(id: &str) -> Option<Grammar> {
        Grammar::ALL.into_iter().find(|g| g.id() == id)
    }

    pub fn family(self) -> Family {
        match self {
            Grammar::Rust => Family::Rust,
            Grammar::Go => Family::Go,
            Grammar::Python => Family::Python,
            Grammar::TypeScript | Grammar::Tsx | Grammar::JavaScript => Family::Js,
            Grammar::Java => Family::Java,
            Grammar::C | Grammar::Cpp => Family::C,
        }
    }

    /// From the extension; `content` is only looked at for `.h` (C or C++). None: unsupported extension.
    pub fn for_path(path: &str, content: &[u8]) -> Option<Grammar> {
        if extension(path) == Some("h") {
            return Some(if looks_like_cpp(content) {
                Grammar::Cpp
            } else {
                Grammar::C
            });
        }
        Grammar::for_path_only(path)
    }

    /// From the extension alone; None for `.h` (which needs the content) and unsupported extensions.
    pub fn for_path_only(path: &str) -> Option<Grammar> {
        Some(match extension(path)? {
            "rs" => Grammar::Rust,
            "go" => Grammar::Go,
            "py" | "pyi" => Grammar::Python,
            "ts" | "mts" | "cts" => Grammar::TypeScript,
            "tsx" => Grammar::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Grammar::JavaScript,
            "java" => Grammar::Java,
            "c" => Grammar::C,
            "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" => Grammar::Cpp,
            _ => return None,
        })
    }
}

/// Grammars whose names see each other: a lookup only matches definitions and references of its own family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    Rust,
    Go,
    Python,
    /// TypeScript, TSX and JavaScript.
    Js,
    Java,
    /// C and C++.
    C,
}

impl Family {
    /// The name the index stores.
    pub fn id(self) -> &'static str {
        match self {
            Family::Rust => "rust",
            Family::Go => "go",
            Family::Python => "python",
            Family::Js => "js",
            Family::Java => "java",
            Family::C => "c",
        }
    }

    /// From the extension alone (`.h` is C). None: unsupported extension.
    pub fn for_path(path: &str) -> Option<Family> {
        if extension(path) == Some("h") {
            return Some(Family::C);
        }
        Grammar::for_path_only(path).map(Grammar::family)
    }
}

/// Blobs larger than this are not parsed (status `too_large`).
pub const MAX_BLOB_BYTES: u64 = 1 << 20;

/// Paths the repository index leaves out: a `node_modules` path segment and `*.min.js`. (The symbols of a single
/// file ignore this.)
pub fn skipped_path(path: &str) -> bool {
    path.split('/').any(|segment| segment == "node_modules") || path.ends_with(".min.js")
}

/// The extension of the last path segment; a dotfile like `.rs` has none.
fn extension(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let (stem, extension) = name.rsplit_once('.')?;
    (!stem.is_empty()).then_some(extension)
}

/// Whether a `.h` file is C++: it matches `\b(class|namespace|template)\b|::|public:`. C headers may use `class`
/// or `new` as identifiers, which the C++ grammar rejects, so C is the default.
fn looks_like_cpp(src: &[u8]) -> bool {
    let contains = |needle: &[u8]| src.windows(needle.len()).any(|w| w == needle);
    if contains(b"::") || contains(b"public:") {
        return true;
    }
    // Words are runs of `[A-Za-z0-9_]`.
    src.split(|&b| !(b.is_ascii_alphanumeric() || b == b'_'))
        .any(|word| matches!(word, b"class" | b"namespace" | b"template"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_grammars() {
        let cases = [
            ("src/a.rs", Grammar::Rust),
            ("main.go", Grammar::Go),
            ("a/b.py", Grammar::Python),
            ("stubs/a.pyi", Grammar::Python),
            ("a.ts", Grammar::TypeScript),
            ("a.mts", Grammar::TypeScript),
            ("a.cts", Grammar::TypeScript),
            ("view.tsx", Grammar::Tsx),
            ("a.js", Grammar::JavaScript),
            ("a.mjs", Grammar::JavaScript),
            ("a.cjs", Grammar::JavaScript),
            ("view.jsx", Grammar::JavaScript),
            ("A.java", Grammar::Java),
            ("a.c", Grammar::C),
            ("a.cc", Grammar::Cpp),
            ("a.cpp", Grammar::Cpp),
            ("a.cxx", Grammar::Cpp),
            ("a.c++", Grammar::Cpp),
            ("a.hpp", Grammar::Cpp),
            ("a.hh", Grammar::Cpp),
            ("a.hxx", Grammar::Cpp),
            ("a.h++", Grammar::Cpp),
        ];
        for (path, grammar) in cases {
            assert_eq!(Grammar::for_path_only(path), Some(grammar), "{path}");
            assert_eq!(Grammar::for_path(path, b""), Some(grammar), "{path}");
            assert_eq!(Family::for_path(path), Some(grammar.family()), "{path}");
        }
        for path in [
            "README",
            "a.txt",
            "a.json",
            "Makefile",
            "a.",
            ".rs",
            "dir.rs/file",
            "a.RS",
            "src/.go",
        ] {
            assert_eq!(Grammar::for_path_only(path), None, "{path}");
            assert_eq!(Grammar::for_path(path, b"class A {}"), None, "{path}");
            assert_eq!(Family::for_path(path), None, "{path}");
        }
    }

    #[test]
    fn ids_round_trip_and_follow_declaration_order() {
        for (i, grammar) in Grammar::ALL.into_iter().enumerate() {
            assert_eq!(grammar as usize, i);
            assert_eq!(Grammar::from_id(grammar.id()), Some(grammar));
        }
        assert_eq!(Grammar::from_id("js"), None);
        assert_eq!(Grammar::from_id(""), None);
        let families = [
            Family::Rust,
            Family::Go,
            Family::Python,
            Family::Js,
            Family::Java,
            Family::C,
        ];
        let ids: Vec<_> = families.iter().map(|f| f.id()).collect();
        assert_eq!(ids, ["rust", "go", "python", "js", "java", "c"]);
        let ids: Vec<_> = Grammar::ALL.iter().map(|g| g.id()).collect();
        assert_eq!(
            ids,
            [
                "rust",
                "go",
                "python",
                "typescript",
                "tsx",
                "javascript",
                "java",
                "c",
                "cpp"
            ]
        );
    }

    #[test]
    fn families_group_the_js_family_and_c_and_cpp() {
        assert_eq!(Grammar::TypeScript.family(), Family::Js);
        assert_eq!(Grammar::Tsx.family(), Family::Js);
        assert_eq!(Grammar::JavaScript.family(), Family::Js);
        assert_eq!(Grammar::C.family(), Family::C);
        assert_eq!(Grammar::Cpp.family(), Family::C);
        for grammar in [Grammar::Rust, Grammar::Go, Grammar::Python, Grammar::Java] {
            let alone: Vec<_> = Grammar::ALL
                .iter()
                .filter(|g| g.family() == grammar.family())
                .collect();
            assert_eq!(alone, [&grammar]);
        }
    }

    #[test]
    fn header_files_are_sniffed_for_cpp() {
        let c = [
            &b"#ifndef A_H\n#define A_H\nint f(int x);\nstruct s { int v; };\n#endif\n"[..],
            b"",
            b"int classify(int); // subclass and namespaces are not keywords here\n",
            b"int template_count; int my_class; int namespace2;\n",
            b"/* public */ int public_x;\n",
        ];
        for src in c {
            assert_eq!(
                Grammar::for_path("a.h", src),
                Some(Grammar::C),
                "{}",
                String::from_utf8_lossy(src)
            );
        }
        let cpp = [
            &b"class A { int v; };\n"[..],
            b"namespace ns { int f(); }\n",
            b"template <class T> T id(T t);\n",
            b"int x = std::max(1, 2);\n",
            b"struct S { public: int v; };\n",
            b"typedef int T; // class\n",
            b"\xEF\xBB\xBFclass A;\n",
        ];
        for src in cpp {
            assert_eq!(
                Grammar::for_path("a.h", src),
                Some(Grammar::Cpp),
                "{}",
                String::from_utf8_lossy(src)
            );
        }
        // Without content the extension alone is not enough.
        assert_eq!(Grammar::for_path_only("a.h"), None);
        assert_eq!(Family::for_path("a.h"), Some(Family::C));
        assert_eq!(Family::for_path("dir/sub/a.h"), Some(Family::C));
    }

    #[test]
    fn node_modules_and_minified_files_are_skipped() {
        for path in [
            "node_modules/a/index.js",
            "web/node_modules/a/b.ts",
            "a/node_modules",
            "x.min.js",
            "dist/app.min.js",
        ] {
            assert!(skipped_path(path), "{path}");
        }
        for path in [
            "src/a.js",
            "node_modules_old/a.js",
            "my_node_modules/a.js",
            "a.min.ts",
            "a.js.min",
            "minjs",
        ] {
            assert!(!skipped_path(path), "{path}");
        }
    }
}
