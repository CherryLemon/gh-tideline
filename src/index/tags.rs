//! Definitions and references of names in one source file, extracted with tree-sitter's tags queries.
//!
//! A query marks a name with `@name` and the node it belongs to with `@definition.<kind>` or `@reference.<kind>`. The
//! queries in `queries/` are ours (most replace the ones shipped with the grammars, which tag little more than
//! definitions): they also tag calls and type uses as references, and tag whole definitions so a scope spans the
//! entire function or class. Where several patterns tag the same name node, the pattern written first wins.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use tree_sitter::Language;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

use super::lang::Grammar;

const RUST_QUERY: &str = include_str!("queries/rust.scm");
const GO_QUERY: &str = include_str!("queries/go.scm");
const PYTHON_QUERY: &str = include_str!("queries/python.scm");
const JAVA_QUERY: &str = include_str!("queries/java.scm");
const C_QUERY: &str = include_str!("queries/c.scm");
const CPP_QUERY: &str = include_str!("queries/cpp.scm");
/// Additions to the upstream JavaScript and TypeScript queries, shared by JavaScript, TypeScript and TSX.
const ECMA_QUERY: &str = include_str!("queries/ecma.scm");
const JAVASCRIPT_QUERY: &str = include_str!("queries/javascript.scm");
const JSX_QUERY: &str = include_str!("queries/jsx.scm");
const TYPESCRIPT_QUERY: &str = include_str!("queries/typescript.scm");

/// A byte order mark, ignored like the editor ignores it.
const BOM: &[u8] = b"\xEF\xBB\xBF";
/// The longest snippet, in UTF-16 code units.
const SNIPPET_UNITS: usize = 200;
/// How many code units before the name a snippet starts when the name is too far into a long line to fit.
const SNIPPET_LEAD: usize = 40;
/// How many bytes before and after a name `snippet_at` looks at, so a snippet of a huge line costs no more than of a
/// short one. Enough for `SNIPPET_UNITS` of any text and for the whitespace that indents a line.
const SNIPPET_WINDOW: usize = 1024;
/// The longest name kept. Longer ones are generated junk, not identifiers.
const MAX_NAME_BYTES: usize = 256;

/// A definition or a reference of a name in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// The capture suffix: for definitions `function`, `method`, `constructor`, `class`, `interface`, `enum`, `type`,
    /// `constant`, `variable`, `module`, `namespace` or `macro`; for references `call`, `type`, `class` or
    /// `implementation` (the `impl` blocks of Rust, which have no name of their own).
    pub kind: String,
    pub is_def: bool,
    /// 1-based line of the name.
    pub line: u32,
    /// 1-based UTF-16 column of the name.
    pub column: u32,
    /// Exclusive.
    pub end_column: u32,
    /// 1-based first line of the whole definition node (for references: the line).
    pub scope_start: u32,
    /// 1-based last line of the whole definition node, inclusive (for references: the line).
    pub scope_end: u32,
    /// The trimmed line (see `line_snippet`); filled for definitions and `implementation` tags, empty for other
    /// references.
    pub snippet: String,
    /// 1-based UTF-16 column of the name inside `snippet`; 0 when `snippet` is empty.
    pub snippet_column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The cancellation flag was raised.
    Cancelled,
    Failed(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Cancelled => f.write_str("cancelled"),
            ParseError::Failed(message) => f.write_str(message),
        }
    }
}

impl From<tree_sitter_tags::Error> for ParseError {
    fn from(error: tree_sitter_tags::Error) -> Self {
        match error {
            tree_sitter_tags::Error::Cancelled => ParseError::Cancelled,
            other => ParseError::Failed(other.to_string()),
        }
    }
}

thread_local! {
    /// Parsers are expensive to create and cheap to reuse.
    static CONTEXT: RefCell<TagsContext> = RefCell::new(TagsContext::new());
}

/// Tags of one file, ordered by (line, column). A leading byte order mark is ignored. `cancel` is tree-sitter's
/// cancellation flag: once it is non-zero the parse stops with `Err(Cancelled)`.
///
/// When overlapping patterns tag the same name more than once as a definition (or more than once as a reference),
/// only the first tag is kept, and a reference that sits exactly on the name of a definition is dropped: the catch-all
/// patterns that tag every type name also match the name of a type being defined. Source that is not valid code
/// yields whatever tags the parser could recover, never an error.
pub fn parse(
    grammar: Grammar,
    src: &[u8],
    cancel: Option<&AtomicUsize>,
) -> Result<Vec<Tag>, ParseError> {
    // The flag is otherwise only looked at while parsing, which a small file may finish without ever doing.
    if cancelled(cancel) {
        return Err(ParseError::Cancelled);
    }
    let src = src.strip_prefix(BOM).unwrap_or(src);
    let config = config(grammar)?;
    let found = CONTEXT.with_borrow_mut(
        |context| -> Result<Vec<tree_sitter_tags::Tag>, ParseError> {
            let (found, _) = context.generate_tags(config, src, cancel)?;
            Ok(found.collect::<Result<_, _>>()?)
        },
    )?;

    let line_starts = line_starts(src);
    let row_of = |offset: usize| line_starts.partition_point(|&start| start <= offset) - 1;
    let mut tags = Vec::new();
    for i in select(src, &found) {
        let found = &found[i];
        let name = String::from_utf8_lossy(&src[found.name_range.clone()]).into_owned();
        let kind = config.syntax_type_name(found.syntax_type_id);
        let row = found.span.start.row;
        let (scope_start, scope_end, snippet, snippet_column) =
            if found.is_definition || kind == "implementation" {
                let line_start = line_starts[row];
                let line_end = line_starts.get(row + 1).map_or(src.len(), |&next| next - 1);
                let (snippet, snippet_column) = snippet_at(
                    &src[line_start..line_end],
                    found.name_range.start - line_start,
                );
                // A node can end with its line break (a C `#define`), which belongs to the next line.
                let last = found.range.end.saturating_sub(1).max(found.range.start);
                (
                    row_of(found.range.start),
                    row_of(last),
                    snippet,
                    snippet_column,
                )
            } else {
                (row, row, String::new(), 0)
            };
        tags.push(Tag {
            name,
            kind: kind.to_string(),
            is_def: found.is_definition,
            line: row as u32 + 1,
            column: found.utf16_column_range.start as u32 + 1,
            end_column: found.utf16_column_range.end as u32 + 1,
            scope_start: scope_start as u32 + 1,
            scope_end: scope_end as u32 + 1,
            snippet,
            snippet_column,
        });
    }
    Ok(tags)
}

/// The indices into `found` of the tags to keep, in source order: tags with an identifier-like name, the first one
/// per name node and kind of tag, and no reference on the name of a definition.
///
/// The tags crate keeps one tag per name node, preferring the pattern written first, but only among the tags it has
/// not handed out yet. A pattern that matches late (a C++ class, once the body is seen) loses to a catch-all that
/// already matched the same name, so both can arrive.
fn select(src: &[u8], found: &[tree_sitter_tags::Tag]) -> Vec<usize> {
    let mut kept: HashMap<(usize, usize, bool), usize> = HashMap::new();
    for (i, tag) in found.iter().enumerate() {
        if std::str::from_utf8(&src[tag.name_range.clone()]).is_ok_and(plausible_name) {
            kept.entry((tag.name_range.start, tag.name_range.end, tag.is_definition))
                .or_insert(i);
        }
    }
    let mut indices: Vec<usize> = kept
        .iter()
        .filter(|&(&(start, end, is_def), _)| is_def || !kept.contains_key(&(start, end, true)))
        .map(|(_, &i)| i)
        .collect();
    indices.sort_by_key(|&i| (found[i].name_range.start, i));
    indices
}

/// The trimmed text of 1-based `line` (a byte order mark and a trailing `\r` removed, leading and trailing whitespace
/// trimmed, at most 200 UTF-16 units, cut at a char boundary) and the 1-based UTF-16 column that `column` (a 1-based
/// UTF-16 column of the untrimmed line) has inside it. None when the line does not exist.
///
/// A line too long to show whole is cut around `column` instead of at its start, so the column stays inside the text.
pub fn line_snippet(src: &[u8], line: u32, column: u32) -> Option<(String, u32)> {
    let src = src.strip_prefix(BOM).unwrap_or(src);
    let start = match line.checked_sub(1)? {
        0 => 0,
        breaks => {
            let mut breaks_seen = src.iter().enumerate().filter(|&(_, &byte)| byte == b'\n');
            breaks_seen.nth(breaks as usize - 1)?.0 + 1
        }
    };
    let end = src[start..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(src.len(), |at| start + at);
    let text = &src[start..end];
    let name_byte = byte_offset_of(text, column.saturating_sub(1) as usize);
    Some(snippet_at(text, name_byte))
}

/// `line` (without its `\n`) trimmed and cut for display around the name starting at byte `name_byte`: the snippet
/// and the 1-based UTF-16 column of the name inside it.
fn snippet_at(line: &[u8], name_byte: usize) -> (String, u32) {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let name_byte = name_byte.min(line.len());
    // Only a window of the line is looked at, so a huge line (a bundle on one line) costs no more than a short one.
    let mut from = name_byte.saturating_sub(SNIPPET_WINDOW);
    while from < name_byte && line[from] & 0xC0 == 0x80 {
        from += 1;
    }
    let to = line.len().min(name_byte + SNIPPET_WINDOW);
    let head = String::from_utf8_lossy(&line[from..name_byte]);
    let tail = String::from_utf8_lossy(&line[name_byte..to]);
    let head: &str = if from == 0 { head.trim_start() } else { &head };

    let head_units = head.encode_utf16().count();
    // A name too far into the line to fit gets a window that starts shortly before it.
    let drop_units = if head_units > SNIPPET_UNITS - SNIPPET_LEAD {
        head_units - SNIPPET_LEAD
    } else {
        0
    };
    let mut dropped = 0;
    let mut units = 0;
    let mut snippet = String::new();
    for ch in head.chars().chain(tail.chars()) {
        if dropped < drop_units {
            dropped += ch.len_utf16();
            continue;
        }
        if units + ch.len_utf16() > SNIPPET_UNITS {
            break;
        }
        units += ch.len_utf16();
        snippet.push(ch);
    }
    snippet.truncate(snippet.trim_end().len());
    (snippet, (head_units - dropped) as u32 + 1)
}

/// The byte offset in `line` of the char that starts at the 0-based UTF-16 `unit`, or the end of the line when it is
/// further. A run of invalid UTF-8 counts as one unit, like the parser counts it.
fn byte_offset_of(line: &[u8], unit: usize) -> usize {
    let mut offset = 0;
    let mut units = 0;
    for chunk in line.utf8_chunks() {
        for ch in chunk.valid().chars() {
            if units >= unit {
                return offset;
            }
            units += ch.len_utf16();
            offset += ch.len_utf8();
        }
        if !chunk.invalid().is_empty() {
            if units >= unit {
                return offset;
            }
            units += 1;
            offset += chunk.invalid().len();
        }
    }
    offset
}

/// Start offsets of every line.
fn line_starts(src: &[u8]) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            src.iter()
                .enumerate()
                .filter(|&(_, &byte)| byte == b'\n')
                .map(|(at, _)| at + 1),
        )
        .collect()
}

/// Whether `name` looks like an identifier. The upstream queries also capture things like `ns.Thing` or `(getClass())`.
fn plausible_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && !name.chars().any(|c| c.is_whitespace() || c.is_control())
        // C++ operators (`operator()`, `operator[]`) are the one name with punctuation.
        && (name.starts_with("operator")
            || !name.contains(['.', ',', ';', ':', '(', ')', '[', ']', '{', '}', '"', '\'', '`', '\\']))
}

fn cancelled(cancel: Option<&AtomicUsize>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed) != 0)
}

/// The tags configuration of a grammar, compiled on first use.
fn config(grammar: Grammar) -> Result<&'static TagsConfiguration, ParseError> {
    static CONFIGS: [OnceLock<Result<TagsConfiguration, String>>; Grammar::ALL.len()] =
        [const { OnceLock::new() }; Grammar::ALL.len()];
    CONFIGS[grammar as usize]
        .get_or_init(|| {
            TagsConfiguration::new(language(grammar), &tags_query(grammar), "")
                .map_err(|e| format!("{} tags query: {e}", grammar.id()))
        })
        .as_ref()
        .map_err(|e| ParseError::Failed(e.clone()))
}

fn language(grammar: Grammar) -> Language {
    match grammar {
        Grammar::Rust => tree_sitter_rust::LANGUAGE.into(),
        Grammar::Go => tree_sitter_go::LANGUAGE.into(),
        Grammar::Python => tree_sitter_python::LANGUAGE.into(),
        Grammar::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Grammar::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Grammar::Java => tree_sitter_java::LANGUAGE.into(),
        Grammar::C => tree_sitter_c::LANGUAGE.into(),
        Grammar::Cpp => tree_sitter_cpp::LANGUAGE.into(),
    }
}

/// The query of a grammar. The TypeScript and TSX ones start from the upstream queries of JavaScript and TypeScript
/// (the TypeScript grammar extends the JavaScript one); the catch-all `type` references stay last in each query so any
/// pattern written before them wins.
fn tags_query(grammar: Grammar) -> String {
    let javascript = tree_sitter_javascript::TAGS_QUERY;
    let typescript = tree_sitter_typescript::TAGS_QUERY;
    let parts: &[&str] = match grammar {
        Grammar::Rust => &[RUST_QUERY],
        Grammar::Go => &[GO_QUERY],
        Grammar::Python => &[PYTHON_QUERY],
        Grammar::JavaScript => &[javascript, ECMA_QUERY, JSX_QUERY, JAVASCRIPT_QUERY],
        Grammar::TypeScript => &[javascript, typescript, ECMA_QUERY, TYPESCRIPT_QUERY],
        Grammar::Tsx => &[
            javascript,
            typescript,
            ECMA_QUERY,
            JSX_QUERY,
            TYPESCRIPT_QUERY,
        ],
        Grammar::Java => &[JAVA_QUERY],
        Grammar::C => &[C_QUERY],
        Grammar::Cpp => &[CPP_QUERY],
    };
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: &str = r#"use std::fmt;
pub const MAX: usize = 3;
static GREETING: &str = "hi";
pub struct Parser<T> {
    x: Vec<Item>,
}
pub enum Kind { A, B(Item) }
pub trait Shape {
    fn area(&self) -> f64;
    fn name(&self) -> String { String::new() }
}
type Alias = Vec<Item>;
impl<T> Parser<T> {
    pub fn new() -> Self { Parser { x: vec![] } }
}
impl fmt::Display for Parser<u8> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { Ok(()) }
}
impl crate::a::Shape for crate::b::Thing {}
fn helper(n: usize) -> u32 {
    let p = Parser::new();
    let k = Kind::B(Item);
    inner::deep();
    p.parse_header(&[]);
    log::info!("x");
    let w = parse::<u32>("1");
    n as u32
}
macro_rules! mac { () => {} }
mod inner {
    pub fn deep() {}
}
"#;

    const GO: &str = r#"package main

const Max = 3

const (
    A = iota
    B
    C, D = 1, 2
)

var global = 1

var (
    ErrNotFound = errors.New("x")
    cache       map[string]Server
)

type Handler interface {
    Serve(w int) error
    io.Reader
}

type Server struct{ n int }
type Alias = Server
type Gen[T any] struct{ v T }

func (s *Server) Serve(w int) error {
    helper(s.n)
    return nil
}

func helper(n int) int {
    const local = 1
    var lv = 2
    r := Map[int](nil)
    fmt.Println(pkg.Util(n))
    var q *pkg.Thing
    return n
}

func Map[T any](xs []T) []T { return xs }
"#;

    const PYTHON: &str = r#"import os
from typing import List, Optional

LIMIT = 3
names: List[str] = []


@decorator
class Base(object, metaclass=Meta):
    attr = 1

    def run(self, x: int, y: "Item") -> Optional[Item]:
        return helper(x) or os.path.join(x)

    @staticmethod
    def make(cls) -> "Base":
        return Base()

    class Inner(pkg.Parent):
        pass


async def fetch(url: pkg.Url) -> List[pkg.Item]:
    r = await client.get(url)
    return Item(r)


def outer():
    def nested():
        pass
    return nested
"#;

    const JAVA: &str = r#"package a.b;

@Deprecated
public class Foo<T> extends Base<T> implements Runnable {
  enum Color { RED, GREEN; void paint() {} }
  record Point(int x, int y) { Point { check(x); } }
  public interface Shape extends Closeable { double area(); }
  @interface Marker { String value(); }
  private final List<Item> items = new ArrayList<>();
  public Foo() { super(); }
  @Override
  public void run() throws IOException {
    helper(1);
    Foo f = new Foo();
    var anon = new Base() { void x() {} };
    a.b.c.call(f);
  }
  static int helper(int n) { return n; }
}
"#;

    const TYPESCRIPT: &str = r#"export interface Shape { area(): number; name: string }
type Id = string
enum Color { Red, Green }
declare namespace NS { function f(): void }
abstract class Base<T> { abstract go(): void }
export class Square extends Base<number> implements Shape, Other<Id> {
  constructor(private s: number) { super() }
  area(): number { return helper(this.s) }
  private go = () => 1
  readonly name: string = 'sq'
}
export function helper(n: number): number { const q = new Square(2); return n * q.area() }
const arrow = (x: Id) => x
let v = <Shape>obj
let w = obj as Square
"#;

    const TSX: &str = r#"import React from 'react'
export interface Props { title: string }
export function Card({ title }: Props): JSX.Element {
  return <Box><Header title={title} /><Footer.Link to="x" /><div /></Box>
}
export default class Page extends React.Component<Props> {
  render() { return <Card title="a" /> }
}
"#;

    const JAVASCRIPT: &str = r#"import React from 'react'
const API = 'x'
var legacy = function () {}
export const fn = () => 1
export class Widget extends React.Component {
  constructor(props) { super(props) }
  handler = (e) => { this.run(e) }
  value = 5
  render() { return <Panel title="x"><ns.Item /><div /></Panel> }
}
function helper(n) { return new ns.Thing(n) || new this.klass() || new (getClass())() }
"#;

    const C: &str = r#"#define MAX 3
#define SQUARE(x) ((x)*(x))
struct node { int v; struct node *next; };
enum color { RED, GREEN };
typedef struct node node_t;
typedef int (*cb_t)(int);
static char *foo(void) {
    return 0;
}
int (*get_cb(void))(int) { return 0; }
void qux(struct node *n);
int main(int argc, char **argv) {
    node_t m;
    foo();
    obj->fn(2);
    return SQUARE(MAX);
}
"#;

    const CPP: &str = r#"namespace ns {
namespace inner { int deep() { return 1; } }
class Widget : public Base {
 public:
  Widget() : x_(0) {}
  explicit Widget(int x) : x_(x) { init(); }
  ~Widget() override {}
  bool operator==(const Widget& o) const { return true; }
  static Widget *create() { return nullptr; }
  void draw();
  int x_;
};
template <typename T> class Box { T get() const { return v; } };
using Alias = Widget;
}
namespace a::b::c { void f() {} }
void ns::Widget::draw() { helper(); }
ns::Widget::Widget(int a) {}
int *Widget::make(int n) { return nullptr; }
int helper() { ns::Widget w; w.draw(); return std::max(1, 2); }
"#;

    const FIXTURES: [(Grammar, &str); 9] = [
        (Grammar::Rust, RUST),
        (Grammar::Go, GO),
        (Grammar::Python, PYTHON),
        (Grammar::Java, JAVA),
        (Grammar::TypeScript, TYPESCRIPT),
        (Grammar::Tsx, TSX),
        (Grammar::JavaScript, JAVASCRIPT),
        (Grammar::C, C),
        (Grammar::Cpp, CPP),
    ];

    fn tags(grammar: Grammar, src: &str) -> Vec<Tag> {
        parse(grammar, src.as_bytes(), None).unwrap()
    }

    /// Definitions as `kind name line:column first-last`, where `first-last` are the lines of the scope.
    fn defs(grammar: Grammar, src: &str) -> Vec<String> {
        tags(grammar, src)
            .iter()
            .filter(|t| t.is_def)
            .map(|t| {
                let (kind, name, line, column) = (&t.kind, &t.name, t.line, t.column);
                format!(
                    "{kind} {name} {line}:{column} {}-{}",
                    t.scope_start, t.scope_end
                )
            })
            .collect()
    }

    /// References as `kind name line:column`.
    fn refs(grammar: Grammar, src: &str) -> Vec<String> {
        tags(grammar, src)
            .iter()
            .filter(|t| !t.is_def)
            .map(|t| format!("{} {} {}:{}", t.kind, t.name, t.line, t.column))
            .collect()
    }

    #[track_caller]
    fn assert_has(found: &[String], wanted: &[&str]) {
        for wanted in wanted {
            assert!(
                found.iter().any(|f| f == wanted),
                "{wanted} is missing from {found:#?}"
            );
        }
    }

    #[track_caller]
    fn assert_lacks(found: &[String], names: &[&str]) {
        for name in names {
            let named = found.iter().any(|f| f.split(' ').nth(1) == Some(name));
            assert!(!named, "{name} is in {found:#?}");
        }
    }

    #[test]
    fn rust_tags() {
        assert_eq!(
            defs(Grammar::Rust, RUST),
            [
                "constant MAX 2:11 2-2",
                "constant GREETING 3:8 3-3",
                "class Parser 4:12 4-6",
                "class Kind 7:10 7-7",
                "interface Shape 8:11 8-11",
                "method area 9:8 9-9",
                "method name 10:8 10-10",
                "class Alias 12:6 12-12",
                "method new 14:12 14-14",
                "method fmt 17:8 17-17",
                "function helper 20:4 20-28",
                "macro mac 29:14 29-29",
                "module inner 30:5 30-32",
                "function deep 31:12 31-31",
            ]
        );
        let found = refs(Grammar::Rust, RUST);
        assert_has(
            &found,
            &[
                "implementation Parser 13:9",
                "implementation Display 16:11",
                "implementation Shape 19:16",
                "type Thing 19:36",
                "call vec 14:40",
                "type Parser 21:13",
                "call new 21:21",
                "type Kind 22:13",
                "call B 22:19",
                "call deep 23:12",
                "call parse_header 24:7",
                "call info 25:10",
                "call parse 26:13",
            ],
        );
        // `Self` is no type to look up, lowercase path segments are modules, and primitives are no identifiers.
        assert_lacks(
            &found,
            &[
                "Self", "inner", "log", "fmt", "crate", "std", "u32", "usize",
            ],
        );
    }

    #[test]
    fn rust_impl_blocks_are_implementation_references_with_the_scope_of_the_block() {
        let found = tags(Grammar::Rust, RUST);
        let impls: Vec<_> = found
            .iter()
            .filter(|t| t.kind == "implementation")
            .map(|t| {
                let scope = (t.scope_start, t.scope_end);
                (
                    t.name.as_str(),
                    t.line,
                    scope,
                    t.snippet.as_str(),
                    t.snippet_column,
                )
            })
            .collect();
        assert_eq!(
            impls,
            [
                ("Parser", 13, (13, 15), "impl<T> Parser<T> {", 9),
                (
                    "Display",
                    16,
                    (16, 18),
                    "impl fmt::Display for Parser<u8> {",
                    11
                ),
                (
                    "Shape",
                    19,
                    (19, 19),
                    "impl crate::a::Shape for crate::b::Thing {}",
                    16
                ),
            ]
        );
    }

    #[test]
    fn go_tags() {
        assert_eq!(
            defs(Grammar::Go, GO),
            [
                "constant Max 3:7 3-3",
                "constant A 6:5 6-6",
                "constant B 7:5 7-7",
                "constant C 8:5 8-8",
                "constant D 8:8 8-8",
                "variable global 11:5 11-11",
                "variable ErrNotFound 14:5 14-14",
                "variable cache 15:5 15-15",
                "interface Handler 18:6 18-21",
                "method Serve 19:5 19-19",
                "class Server 23:6 23-23",
                "type Alias 24:6 24-24",
                "class Gen 25:6 25-25",
                "method Serve 27:18 27-30",
                "function helper 32:6 32-39",
                "function Map 41:6 41-41",
            ]
        );
        let found = refs(Grammar::Go, GO);
        assert_has(
            &found,
            &[
                "call New 14:26",
                "type Server 15:28",
                "type Reader 20:8",
                "type Server 27:10",
                "call helper 28:5",
                "call Map 35:10",
                "call Println 36:9",
                "call Util 36:21",
                "type Thing 37:16",
            ],
        );
        // Predeclared types are no types to look up; constants and variables of a function are local.
        assert_lacks(
            &found,
            &["int", "error", "any", "string", "nil", "local", "lv"],
        );
    }

    #[test]
    fn python_tags() {
        assert_eq!(
            defs(Grammar::Python, PYTHON),
            [
                "constant LIMIT 4:1 4-4",
                "constant names 5:1 5-5",
                "class Base 9:7 9-20",
                "method run 12:9 12-13",
                "method make 16:9 16-17",
                "class Inner 19:11 19-20",
                "function fetch 23:11 23-25",
                "function outer 28:5 28-31",
                "function nested 29:9 29-30",
            ]
        );
        let found = refs(Grammar::Python, PYTHON);
        assert_has(
            &found,
            &[
                "call decorator 8:2",
                "class object 9:12",
                "class Meta 9:30",
                "type Optional 12:41",
                "type Item 12:50",
                "call helper 13:16",
                "call join 13:37",
                "call staticmethod 15:6",
                "call Base 17:16",
                "class Parent 19:21",
                "type Url 23:26",
                "call get 24:22",
                "call Item 25:12",
            ],
        );
        assert_lacks(&found, &["int", "str"]);
    }

    #[test]
    fn java_tags() {
        assert_eq!(
            defs(Grammar::Java, JAVA),
            [
                "class Foo 4:14 3-19",
                "enum Color 5:8 5-5",
                "constant RED 5:16 5-5",
                "constant GREEN 5:21 5-5",
                "method paint 5:33 5-5",
                "class Point 6:10 6-6",
                "constructor Point 6:32 6-6",
                "interface Shape 7:20 7-7",
                "method area 7:53 7-7",
                "interface Marker 8:14 8-8",
                "constructor Foo 10:10 10-10",
                "method run 12:15 11-17",
                "method x 15:34 15-15",
                "method helper 18:14 18-18",
            ]
        );
        let found = refs(Grammar::Java, JAVA);
        assert_has(
            &found,
            &[
                "type Deprecated 3:2",
                "class Base 4:29",
                "type Runnable 4:48",
                "call check 6:40",
                "class ArrayList 9:40",
                "type Override 11:4",
                "call helper 13:5",
                "type Foo 14:5",
                "class Foo 14:17",
                "class Base 15:20",
                "call call 16:11",
            ],
        );
        assert_lacks(&found, &["var"]);
    }

    #[test]
    fn typescript_tags() {
        assert_eq!(
            defs(Grammar::TypeScript, TYPESCRIPT),
            [
                "interface Shape 1:18 1-1",
                "method area 1:26 1-1",
                "type Id 2:6 2-2",
                "enum Color 3:6 3-3",
                "module NS 4:19 4-4",
                "function f 4:33 4-4",
                "class Base 5:16 5-5",
                "method go 5:35 5-5",
                "class Square 6:14 6-11",
                "constructor constructor 7:3 7-7",
                "method area 8:3 8-8",
                "method go 9:11 9-9",
                "function helper 12:17 12-12",
                "function arrow 13:7 13-13",
                "constant v 14:5 14-14",
                "constant w 15:5 15-15",
            ]
        );
        assert_has(
            &refs(Grammar::TypeScript, TYPESCRIPT),
            &[
                "class Base 6:29",
                "type Shape 6:53",
                "type Other 6:60",
                "type Id 6:66",
                "call helper 8:27",
                "class Square 12:59",
                "call area 12:83",
                "type Id 13:19",
                // `<Shape>obj` is a type assertion in TypeScript, and JSX in TSX.
                "type Shape 14:10",
                "type Square 15:16",
            ],
        );
    }

    #[test]
    fn tsx_tags_jsx_elements_as_class_references() {
        assert_eq!(
            defs(Grammar::Tsx, TSX),
            [
                "interface Props 2:18 2-2",
                "function Card 3:17 3-5",
                "class Page 6:22 6-8",
                "method render 7:3 7-7",
            ]
        );
        let found = refs(Grammar::Tsx, TSX);
        assert_has(
            &found,
            &[
                "class Box 4:11",
                "class Header 4:16",
                "class Link 4:47",
                "class Card 7:22",
                "class Component 6:41",
                "type Props 3:33",
            ],
        );
        // Lowercase elements are HTML.
        assert_lacks(&found, &["div"]);
        // Only the opening tag names an element (the closing tag repeats the name).
        assert_eq!(
            refs(Grammar::Tsx, "const v = <Shape>x</Shape>;\n"),
            ["class Shape 1:12"]
        );
    }

    #[test]
    fn javascript_tags() {
        assert_eq!(
            defs(Grammar::JavaScript, JAVASCRIPT),
            [
                "constant API 2:7 2-2",
                "function legacy 3:5 3-3",
                "function fn 4:14 4-4",
                "class Widget 5:14 5-10",
                "constructor constructor 6:3 6-6",
                "method handler 7:3 7-7",
                "method render 9:3 9-9",
                "function helper 11:10 11-11",
            ]
        );
        let found = refs(Grammar::JavaScript, JAVASCRIPT);
        assert_has(
            &found,
            &[
                "class Component 5:35",
                "call run 7:27",
                "class Panel 9:22",
                "class Item 9:42",
                "class Thing 11:36",
                "class klass 11:57",
                "call getClass 11:73",
            ],
        );
        assert_lacks(&found, &["div"]);
        // The upstream query also tags `ns.Thing` and `(getClass())` whole.
        for tag in tags(Grammar::JavaScript, JAVASCRIPT) {
            assert!(!tag.name.contains(['.', '(']), "{}", tag.name);
        }
    }

    #[test]
    fn c_tags() {
        assert_eq!(
            defs(Grammar::C, C),
            [
                "macro MAX 1:9 1-1",
                "macro SQUARE 2:9 2-2",
                "class node 3:8 3-3",
                "enum color 4:6 4-4",
                "constant RED 4:14 4-4",
                "constant GREEN 4:19 4-4",
                "type node_t 5:21 5-5",
                "type cb_t 6:15 6-6",
                // The scope is the whole function, also behind a pointer declarator or a function pointer result.
                "function foo 7:14 7-9",
                "function get_cb 10:7 10-10",
                "function main 12:5 12-17",
            ]
        );
        assert_has(
            &refs(Grammar::C, C),
            &[
                "type node 3:29",
                "type node 5:16",
                "type node 11:17",
                "type node_t 13:5",
                "call foo 14:5",
                "call fn 15:10",
                "call SQUARE 16:12",
            ],
        );
    }

    #[test]
    fn cpp_tags() {
        assert_eq!(
            defs(Grammar::Cpp, CPP),
            [
                "namespace ns 1:11 1-15",
                "namespace inner 2:11 2-2",
                "function deep 2:23 2-2",
                "class Widget 3:7 3-12",
                "constructor Widget 5:3 5-5",
                "constructor Widget 6:12 6-6",
                "method ~Widget 7:3 7-7",
                "method operator== 8:8 8-8",
                "method create 9:18 9-9",
                "class Box 13:29 13-13",
                "method get 13:37 13-13",
                "type Alias 14:7 14-14",
                // `namespace a::b::c` defines `c`.
                "namespace c 16:17 16-16",
                "function f 16:26 16-16",
                "method draw 17:18 17-17",
                "constructor Widget 18:13 18-18",
                "method make 19:14 19-19",
                "function helper 20:5 20-20",
            ]
        );
        assert_has(
            &refs(Grammar::Cpp, CPP),
            &[
                "type Base 3:23",
                "call init 6:36",
                "type a 16:11",
                "type b 16:14",
                "type ns 17:6",
                "type Widget 17:10",
                "call helper 17:27",
                "call draw 20:32",
                "call max 20:52",
            ],
        );
        // The class name is matched by the catch-all type reference and by the class pattern, which completes later
        // (only after the base classes); one tag remains and it is the definition.
        let at_class_name: Vec<_> = tags(Grammar::Cpp, CPP)
            .into_iter()
            .filter(|t| (t.line, t.column) == (3, 7))
            .collect();
        assert_eq!(at_class_name.len(), 1);
        assert!(at_class_name[0].is_def);
    }

    #[test]
    fn tags_are_consistent_for_every_fixture() {
        for (grammar, src) in FIXTURES {
            let lines: Vec<&str> = src.lines().collect();
            let found = tags(grammar, src);
            assert!(found.len() > 10, "{grammar:?}");
            let mut seen = std::collections::HashSet::new();
            let mut previous = (0, 0);
            for tag in &found {
                let id = format!("{grammar:?} {} {}:{}", tag.name, tag.line, tag.column);
                // In source order, each tag once.
                assert!((tag.line, tag.column) >= previous, "{id} is out of order");
                previous = (tag.line, tag.column);
                assert!(
                    seen.insert((&tag.name, tag.line, tag.column, tag.is_def)),
                    "{id} is twice"
                );
                // The name is where the tag says (columns count UTF-16 units).
                let line: Vec<u16> = lines[tag.line as usize - 1].encode_utf16().collect();
                let name = &line[tag.column as usize - 1..tag.end_column as usize - 1];
                assert_eq!(String::from_utf16(name).unwrap(), tag.name, "{id}");
                // A scope contains its name.
                assert!(
                    tag.scope_start <= tag.line && tag.line <= tag.scope_end,
                    "{id}"
                );
                assert!(tag.scope_end as usize <= lines.len(), "{id}");
                if tag.is_def || tag.kind == "implementation" {
                    // The snippet is the trimmed line, with the name at `snippet_column`.
                    assert_eq!(tag.snippet, lines[tag.line as usize - 1].trim(), "{id}");
                    let snippet: Vec<u16> = tag.snippet.encode_utf16().collect();
                    let start = tag.snippet_column as usize - 1;
                    let name = &snippet[start..start + tag.name.encode_utf16().count()];
                    assert_eq!(String::from_utf16(name).unwrap(), tag.name, "{id}");
                } else {
                    assert_eq!(
                        (tag.scope_start, tag.scope_end),
                        (tag.line, tag.line),
                        "{id}"
                    );
                    assert_eq!((tag.snippet.as_str(), tag.snippet_column), ("", 0), "{id}");
                }
                // A reference on the name of a definition was dropped.
                if !tag.is_def {
                    let on_a_definition = found.iter().any(|d| {
                        d.is_def && (d.line, d.column, &d.name) == (tag.line, tag.column, &tag.name)
                    });
                    assert!(!on_a_definition, "{id}");
                }
            }
        }
    }

    #[test]
    fn only_rust_has_implementation_references() {
        for (grammar, src) in FIXTURES {
            let implementations = tags(grammar, src)
                .iter()
                .filter(|t| t.kind == "implementation")
                .count();
            assert_eq!(implementations > 0, grammar == Grammar::Rust, "{grammar:?}");
        }
    }

    #[test]
    fn kinds_are_the_known_ones() {
        let defs = [
            "function",
            "method",
            "constructor",
            "class",
            "interface",
            "enum",
            "type",
            "constant",
            "variable",
            "module",
            "namespace",
            "macro",
        ];
        let refs = ["call", "type", "class", "implementation"];
        for (grammar, src) in FIXTURES {
            for tag in tags(grammar, src) {
                let known = if tag.is_def {
                    defs.contains(&tag.kind.as_str())
                } else {
                    refs.contains(&tag.kind.as_str())
                };
                assert!(known, "{grammar:?} {} {}", tag.kind, tag.name);
            }
        }
    }

    #[test]
    fn parsing_is_repeatable_across_grammars_on_one_thread() {
        let first: Vec<_> = FIXTURES
            .iter()
            .map(|&(grammar, src)| tags(grammar, src))
            .collect();
        for _ in 0..2 {
            for (i, &(grammar, src)) in FIXTURES.iter().enumerate().rev() {
                assert_eq!(tags(grammar, src), first[i], "{grammar:?}");
            }
        }
    }

    #[test]
    fn every_grammar_has_a_working_query() {
        for grammar in Grammar::ALL {
            assert!(config(grammar).is_ok(), "{grammar:?}");
            assert!(tags(grammar, "").is_empty(), "{grammar:?}");
        }
    }

    #[test]
    fn columns_count_utf16_units() {
        // é is one unit, 中 one, and the emoji two (surrogates); in UTF-8 they take 2, 3 and 4 bytes.
        let src = "/* é 中 😀 */ fn after() {}\n";
        let found = tags(Grammar::Rust, src);
        assert_eq!(found.len(), 1);
        let tag = &found[0];
        assert_eq!(
            (tag.name.as_str(), tag.line, tag.column, tag.end_column),
            ("after", 1, 17, 22)
        );
        assert_eq!((tag.snippet.as_str(), tag.snippet_column), (src.trim(), 17));

        let src = "/* 😀 */ function 中文(x) {}\n";
        let found = tags(Grammar::JavaScript, src);
        let positions: Vec<_> = found
            .iter()
            .map(|t| (t.name.as_str(), t.line, t.column, t.end_column))
            .collect();
        assert_eq!(positions, [("中文", 1, 19, 21)]);
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        let plain = tags(Grammar::Rust, "fn first() {}\nfn second() {}\n");
        let marked = parse(
            Grammar::Rust,
            b"\xEF\xBB\xBFfn first() {}\nfn second() {}\n",
            None,
        )
        .unwrap();
        assert_eq!(marked, plain);
        assert_eq!(
            (
                plain[0].name.as_str(),
                plain[0].column,
                plain[0].snippet.as_str()
            ),
            ("first", 4, "fn first() {}")
        );
    }

    #[test]
    fn crlf_line_endings_do_not_change_positions_or_leak_into_snippets() {
        let lf = tags(Grammar::Rust, "fn a() {\n    b();\n}\n");
        let crlf = tags(Grammar::Rust, "fn a() {\r\n    b();\r\n}\r\n");
        assert_eq!(crlf, lf);
        assert_eq!(
            (
                crlf[0].snippet.as_str(),
                crlf[0].scope_start,
                crlf[0].scope_end
            ),
            ("fn a() {", 1, 3)
        );
        assert_eq!(
            (crlf[1].name.as_str(), crlf[1].line, crlf[1].column),
            ("b", 2, 5)
        );
    }

    #[test]
    fn tabs_count_one_column_and_snippets_are_trimmed() {
        let found = tags(Grammar::Rust, "mod m {\n\t\tfn tabbed() {}   \n}\n");
        let tabbed = found.iter().find(|t| t.name == "tabbed").unwrap();
        assert_eq!(
            (
                tabbed.column,
                tabbed.snippet.as_str(),
                tabbed.snippet_column
            ),
            (6, "fn tabbed() {}", 4)
        );
    }

    #[test]
    fn a_raised_cancel_flag_stops_the_parse() {
        let raised = AtomicUsize::new(1);
        for grammar in Grammar::ALL {
            assert_eq!(
                parse(grammar, b"x", Some(&raised)),
                Err(ParseError::Cancelled),
                "{grammar:?}"
            );
        }
        assert_eq!(ParseError::Cancelled.to_string(), "cancelled");
        assert_eq!(
            ParseError::Failed("bad query".into()).to_string(),
            "bad query"
        );
        let clear = AtomicUsize::new(0);
        assert!(
            parse(Grammar::Rust, RUST.as_bytes(), Some(&clear))
                .unwrap()
                .len()
                > 10
        );
    }

    #[test]
    fn garbage_and_truncated_source_do_not_panic() {
        let mut state = 0x2545_F491_u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for grammar in Grammar::ALL {
            for len in [0, 1, 7, 64, 500, 4000] {
                let bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
                parse(grammar, &bytes, None).unwrap();
            }
            parse(grammar, "{(([<".repeat(3000).as_bytes(), None).unwrap();
            parse(grammar, "\u{FEFF}".as_bytes(), None).unwrap();
        }
        for (grammar, src) in FIXTURES {
            for cut in (0..src.len()).step_by(11) {
                parse(grammar, &src.as_bytes()[..cut], None).unwrap();
            }
        }
        // A name that is not UTF-8 is no name.
        let found = parse(
            Grammar::C,
            b"int caf\xE9(void) { return 0; }\nint ok(void) { return 1; }\n",
            None,
        )
        .unwrap();
        assert!(found.iter().all(|t| t.name != "caf\u{FFFD}"));
        assert!(found.iter().any(|t| t.name == "ok" && t.is_def));
    }

    #[test]
    fn names_that_are_not_identifiers_are_left_out() {
        for name in [
            "foo",
            "Foo_1",
            "$scope",
            "_",
            "~Widget",
            "operator()",
            "operator[]",
            "operator==",
            "中文",
        ] {
            assert!(plausible_name(name), "{name}");
        }
        let long = "a".repeat(MAX_NAME_BYTES + 1);
        for name in [
            "",
            "ns.Thing",
            "(getClass())",
            "a b",
            "\"quoted\"",
            "a\nb",
            "x,y",
            "tab\t",
            "a:b",
            long.as_str(),
        ] {
            assert!(!plausible_name(name), "{name:?}");
        }
        assert!(plausible_name(&"a".repeat(MAX_NAME_BYTES)));
    }

    #[test]
    fn definitions_on_a_huge_line_get_snippets_around_their_names() {
        let src: String = (0..8000)
            .map(|i| format!("function f{i}(a){{return a}};"))
            .collect();
        let found = tags(Grammar::JavaScript, &src);
        let defs: Vec<_> = found.iter().filter(|t| t.is_def).collect();
        assert_eq!(defs.len(), 8000);
        for tag in defs {
            let snippet: Vec<u16> = tag.snippet.encode_utf16().collect();
            assert!(snippet.len() <= SNIPPET_UNITS, "{}", tag.name);
            let start = tag.snippet_column as usize - 1;
            let name = &snippet[start..start + tag.name.len()];
            assert_eq!(String::from_utf16(name).unwrap(), tag.name);
        }
    }

    #[test]
    fn line_snippet_trims_the_line_and_locates_the_name() {
        let src = b"fn a() {}\n    pub fn parse(buf: &[u8]) -> Header {\r\n}\n";
        let snippet = |line, column| line_snippet(src, line, column);
        assert_eq!(
            snippet(2, 12),
            Some(("pub fn parse(buf: &[u8]) -> Header {".to_string(), 8))
        );
        assert_eq!(snippet(1, 4), Some(("fn a() {}".to_string(), 4)));
        assert_eq!(snippet(3, 1), Some(("}".to_string(), 1)));
        // The empty line after the final line break exists, the next one does not.
        assert_eq!(snippet(4, 1), Some((String::new(), 1)));
        assert_eq!(snippet(5, 1), None);
        assert_eq!(snippet(0, 1), None);
        assert_eq!(line_snippet(b"", 1, 1), Some((String::new(), 1)));
        assert_eq!(line_snippet(b"", 2, 1), None);

        assert_eq!(
            line_snippet("\u{FEFF}  x = 1".as_bytes(), 1, 3),
            Some(("x = 1".to_string(), 1))
        );
        assert_eq!(
            line_snippet(b"\t\tfoo(bar)  \t\r\n", 1, 3),
            Some(("foo(bar)".to_string(), 1))
        );
        // Columns count UTF-16 units: the emoji takes two.
        assert_eq!(
            line_snippet("  😀 name".as_bytes(), 1, 6),
            Some(("😀 name".to_string(), 4))
        );
        // A column beyond the line is clamped to its end, and invalid UTF-8 is replaced.
        assert_eq!(line_snippet(b"abc", 1, 99), Some(("abc".to_string(), 4)));
        assert_eq!(
            line_snippet(b"ab\xFFcd", 1, 4),
            Some(("ab\u{FFFD}cd".to_string(), 4))
        );
    }

    #[test]
    fn line_snippet_cuts_at_200_units_on_a_char_boundary() {
        let units = |s: &str| s.encode_utf16().count();
        let exact = "a".repeat(200);
        assert_eq!(
            line_snippet(exact.as_bytes(), 1, 1),
            Some((exact.clone(), 1))
        );
        let (snippet, column) = line_snippet(format!("{exact}b").as_bytes(), 1, 1).unwrap();
        assert_eq!((snippet, column), (exact, 1));
        // The emoji does not fit into the 199 units that are left, so it is left out entirely.
        let (snippet, _) =
            line_snippet(format!("{}😀tail", "a".repeat(199)).as_bytes(), 1, 1).unwrap();
        assert_eq!(snippet, "a".repeat(199));
        let (snippet, _) = line_snippet("é".repeat(300).as_bytes(), 1, 1).unwrap();
        assert_eq!(units(&snippet), 200);
        assert_eq!(snippet, "é".repeat(200));
        // Whitespace left at the cut is trimmed.
        let (snippet, _) = line_snippet(
            format!("{} {}", "a".repeat(199), "b".repeat(50)).as_bytes(),
            1,
            1,
        )
        .unwrap();
        assert_eq!(snippet, "a".repeat(199));
    }

    #[test]
    fn line_snippet_shows_a_name_that_is_far_into_a_long_line() {
        // A name within the first 160 units keeps the start of the line.
        let line = format!("let value = 1; {}", "y".repeat(300));
        let (snippet, column) = line_snippet(line.as_bytes(), 1, 5).unwrap();
        assert_eq!((snippet.encode_utf16().count(), column), (200, 5));
        assert!(snippet.starts_with("let value"));
        // Further out, the snippet starts 40 units before the name.
        for (name_column, expected) in [(161, 161), (162, 41), (601, 41), (200_001, 41)] {
            let before = format!(
                "{}{}",
                "q".repeat((name_column - 1) % 2),
                "x ".repeat((name_column - 1) / 2)
            );
            let line = format!("{before}target(x) {}", "z".repeat(300));
            let (snippet, column) = line_snippet(line.as_bytes(), 1, name_column as u32).unwrap();
            assert_eq!(column, expected, "column {name_column}");
            assert!(snippet.encode_utf16().count() <= SNIPPET_UNITS);
            assert!(
                snippet[column as usize - 1..].starts_with("target"),
                "column {name_column}: {snippet}"
            );
        }
        // Multi-byte text before the name does not break the window.
        let line = format!("{}target", "中é😀".repeat(2000));
        let name_column = "中é😀".encode_utf16().count() * 2000 + 1;
        let (snippet, column) = line_snippet(line.as_bytes(), 1, name_column as u32).unwrap();
        let utf16: Vec<u16> = snippet.encode_utf16().collect();
        assert_eq!(
            String::from_utf16(&utf16[column as usize - 1..]).unwrap(),
            "target"
        );
    }
}
