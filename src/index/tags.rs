//! Definitions and references of names in one source file, extracted with tree-sitter's tags queries.
//!
//! A query marks a name with `@name` and the node it belongs to with `@definition.<kind>` or `@reference.<kind>`. The
//! queries in `queries/` are ours (most replace the ones shipped with the grammars, which tag little more than
//! definitions): they also tag calls and type uses as references, and tag whole definitions so a scope spans the
//! entire function or class. Where several patterns tag the same name node, the pattern written first wins.
//!
//! Running the queries is ours too, not the `tree-sitter-tags` crate's: tree-sitter's query cursor takes minutes on some
//! files that are not code (see `parse`), and the crate cannot be told to give up on them.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::ops::{ControlFlow, Range};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tree_sitter::{
    Language, ParseOptions, ParseState, Parser, Query, QueryCursor, QueryCursorOptions,
    QueryCursorState, StreamingIterator,
};

use super::lang::Grammar;

const RUST_QUERY: &str = include_str!("queries/rust.scm");
const GO_QUERY: &str = include_str!("queries/go.scm");
const PYTHON_QUERY: &str = include_str!("queries/python.scm");
const JAVA_QUERY: &str = include_str!("queries/java.scm");
const C_QUERY: &str = include_str!("queries/c.scm");
const CPP_QUERY: &str = include_str!("queries/cpp.scm");
/// The JavaScript query that JavaScript, TypeScript and TSX start from.
const ECMA_BASE_QUERY: &str = include_str!("queries/ecma_base.scm");
/// Additions to `ECMA_BASE_QUERY` that are valid in all three grammars.
const ECMA_QUERY: &str = include_str!("queries/ecma.scm");
const JAVASCRIPT_QUERY: &str = include_str!("queries/javascript.scm");
const JSX_QUERY: &str = include_str!("queries/jsx.scm");
const TYPESCRIPT_QUERY: &str = include_str!("queries/typescript.scm");

/// A byte order mark, ignored like the editor ignores it.
const BOM: &[u8] = b"\xEF\xBB\xBF";
/// How long `parse` works on one file at most, parsing and matching together. Real code never comes near it (a file of
/// 1 MiB takes about a tenth of a second); text that is not code can: a megabyte of `+` after an `=` takes tree-sitter's
/// query cursor half a minute. The limit is wall-clock time, so a busy machine can only make a file slower by a factor
/// of ten before it is given up on.
const TIME_LIMIT: Duration = Duration::from_millis(1500);
/// The depth in the syntax tree below which nothing is tagged. The query cursor keeps the depth where a match starts in
/// 16 bits: from depth 65 536 on, matches are lost track of, pile up and make the work grow with the square of the
/// depth. Code nested even a thousand levels deep is not code.
const MAX_DEPTH: u32 = 60_000;
/// How many tags `parse` turns into their final form between two looks at the cancellation flag and the deadline.
const TAGS_PER_CHECK: usize = 4096;
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
    /// The file took longer than `TIME_LIMIT`. It may parse on a machine that is less busy, so it is no reason to
    /// remember that the file has no tags.
    TimedOut,
    Failed(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Cancelled => f.write_str("cancelled"),
            ParseError::TimedOut => f.write_str("took too long to parse"),
            ParseError::Failed(message) => f.write_str(message),
        }
    }
}

thread_local! {
    /// Parsers are expensive to create and cheap to reuse.
    static ENGINE: RefCell<Engine> = RefCell::new(Engine {
        parser: Parser::new(),
        cursor: QueryCursor::new(),
    });
}

/// Tags of one file, ordered by (line, column). A leading byte order mark is ignored. `cancel` is tree-sitter's
/// cancellation flag: once it is non-zero (before or during the call) the call stops with `Err(Cancelled)`.
///
/// When overlapping patterns tag the same name more than once as a definition (or more than once as a reference),
/// only the first tag is kept, and a reference that sits exactly on the name of a definition is dropped: the catch-all
/// patterns that tag every type name also match the name of a type being defined. Source that is not valid code
/// yields whatever tags the parser could recover, never an error.
///
/// A file is given up on after `TIME_LIMIT`: the call returns `Err(TimedOut)`, and no tags. It is an error, not an
/// empty result, because how long a file takes depends on the machine: a caller that keeps what it learns about a file
/// must not keep that. Tags nested deeper than `MAX_DEPTH` are left out, which is no error: the depth is a property of
/// the file. Neither happens to code, only to text that makes tree-sitter spend minutes on a megabyte.
pub fn parse(
    grammar: Grammar,
    src: &[u8],
    cancel: Option<&AtomicUsize>,
) -> Result<Vec<Tag>, ParseError> {
    parse_within(grammar, src, cancel, TIME_LIMIT)
}

/// `parse` that gives up after `limit` instead of `TIME_LIMIT`.
fn parse_within(
    grammar: Grammar,
    src: &[u8],
    cancel: Option<&AtomicUsize>,
    limit: Duration,
) -> Result<Vec<Tag>, ParseError> {
    // The flag is otherwise only looked at while working, which a small file may finish without ever doing.
    if cancelled(cancel) {
        return Err(ParseError::Cancelled);
    }
    let src = src.strip_prefix(BOM).unwrap_or(src);
    let config = config(grammar)?;
    let budget = Budget {
        cancel,
        deadline: Instant::now() + limit,
        timed_out: Cell::new(false),
    };
    let mut found = ENGINE.with_borrow_mut(|engine| engine.find(config, src, &budget))?;
    if grammar == Grammar::Rust {
        // `r#type` is the name `type`.
        for tag in &mut found {
            if src[tag.name.clone()].starts_with(b"r#") {
                tag.name.start += 2;
            }
        }
    }

    let line_starts = line_starts(src);
    let row_of = |offset: usize| line_starts.partition_point(|&start| start <= offset) - 1;
    // The row, byte and UTF-16 column where the previous tag starts: the next tag is on the same line more often than
    // not, and counting from the start of a line of 100 000 bytes for each of 8000 tags on it would take forever.
    let mut previous = (usize::MAX, 0, 0);
    let mut tags = Vec::new();
    for (n, i) in select(grammar, src, &found).into_iter().enumerate() {
        // Tree-sitter looks at the budget while it works; this loop is out of its hands, and on a dense megabyte it
        // takes a few hundred milliseconds.
        if n % TAGS_PER_CHECK == 0 && budget.check().is_break() {
            return budget.stopped();
        }
        let found = &found[i];
        let name = tag_name(grammar, &src[found.name.clone()]);
        let kind = config.kind(found.capture);
        let row = row_of(found.name.start);
        let (from_byte, from_column) = match previous {
            (previous_row, byte, column) if previous_row == row => (byte, column),
            _ => (line_starts[row], 0),
        };
        let column = from_column + utf16_len(&src[from_byte..found.name.start]);
        let end_column = column + utf16_len(&src[found.name.clone()]);
        previous = (row, found.name.start, column);
        let (scope_start, scope_end, snippet, snippet_column) =
            if found.is_def || kind == "implementation" {
                let line_start = line_starts[row];
                let line_end = line_starts.get(row + 1).map_or(src.len(), |&next| next - 1);
                let (snippet, snippet_column) =
                    snippet_at(&src[line_start..line_end], found.name.start - line_start);
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
            is_def: found.is_def,
            line: row as u32 + 1,
            column: column as u32 + 1,
            end_column: end_column as u32 + 1,
            scope_start: scope_start as u32 + 1,
            scope_end: scope_end as u32 + 1,
            snippet,
            snippet_column,
        });
    }
    Ok(tags)
}

/// What a call of `parse` may spend: it stops, as soon as tree-sitter asks, when `cancel` is raised or the deadline
/// has passed.
struct Budget<'a> {
    cancel: Option<&'a AtomicUsize>,
    deadline: Instant,
    /// Whether the deadline made the work stop.
    timed_out: Cell<bool>,
}

impl Budget<'_> {
    /// tree-sitter calls this every few hundred steps of the parse and of the matching; it costs an atomic load and a
    /// clock reading.
    fn check(&self) -> ControlFlow<()> {
        if cancelled(self.cancel) {
            ControlFlow::Break(())
        } else if Instant::now() >= self.deadline {
            self.timed_out.set(true);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }

    /// The result of work that stopped early.
    fn stopped<T>(&self) -> Result<T, ParseError> {
        if cancelled(self.cancel) {
            Err(ParseError::Cancelled)
        } else if self.timed_out.get() {
            Err(ParseError::TimedOut)
        } else {
            Err(ParseError::Failed("the parser gave up".to_string()))
        }
    }
}

/// A tag found by a query, before anything is read from the source.
struct Found {
    /// Where the name is.
    name: Range<usize>,
    /// The node the tag is for, grown to include the name.
    range: Range<usize>,
    /// The `@definition.<kind>` or `@reference.<kind>` capture that tags it.
    capture: u32,
    is_def: bool,
}

/// The parser and query cursor of a thread.
struct Engine {
    parser: Parser,
    cursor: QueryCursor,
}

impl Engine {
    /// The tags of `src` in the order the tags crate would hand them out: the matches of the query in the order
    /// tree-sitter finds them, with one tag per name node (the pattern written first wins), except that a tag is handed
    /// out as soon as the matching has moved past its name. `Err(TimedOut)` when the time ran out.
    fn find(
        &mut self,
        config: &Config,
        src: &[u8],
        budget: &Budget,
    ) -> Result<Vec<Found>, ParseError> {
        self.parser
            .set_language(&config.language)
            .map_err(|e| ParseError::Failed(e.to_string()))?;
        self.parser.reset();
        let mut on_parse_progress = |_: &ParseState| budget.check();
        let tree = self.parser.parse_with_options(
            &mut |at, _| src.get(at..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut on_parse_progress)),
        );
        let Some(tree) = tree else {
            return budget.stopped();
        };

        let mut on_query_progress = |_: &QueryCursorState| budget.check();
        self.cursor.set_max_start_depth(Some(MAX_DEPTH));
        let mut matches = self.cursor.matches_with_options(
            &config.query,
            tree.root_node(),
            src,
            QueryCursorOptions::new().progress_callback(&mut on_query_progress),
        );
        let mut found = Vec::new();
        // Tags not handed out yet, each with the index of the pattern that matched it, sorted by where the name ends and
        // starts.
        let mut queue: VecDeque<(Found, usize)> = VecDeque::new();
        while let Some(m) = matches.next() {
            let (mut name, mut tagged) = (None, None);
            for capture in m.captures() {
                match config.captures[capture.index as usize] {
                    Capture::Name => name = Some(capture.node),
                    Capture::Tag { is_def } => tagged = Some((capture, is_def)),
                    Capture::Other => {}
                }
            }
            let (Some(name), Some((tagged, is_def))) = (name, tagged) else {
                continue;
            };
            if name.has_error() {
                continue;
            }
            let (name_range, node_range) = (name.byte_range(), tagged.node.byte_range());
            let tag = Found {
                range: node_range.start.min(name_range.start)..node_range.end.max(name_range.end),
                name: name_range,
                capture: tagged.index,
                is_def,
            };
            let key = |tag: &Found| (tag.name.end, tag.name.start);
            match queue.binary_search_by_key(&key(&tag), |(queued, _)| key(queued)) {
                Ok(at) => {
                    if queue[at].1 > m.pattern_index {
                        queue[at] = (tag, m.pattern_index);
                    }
                }
                Err(at) => queue.insert(at, (tag, m.pattern_index)),
            }
            // What ends before the last name starts is final, unless a pattern that completes late (a C++ class, once
            // the body is seen) matches the same name again.
            while queue.len() > 1 && queue[0].0.name.end < queue[queue.len() - 1].0.name.start {
                found.extend(queue.pop_front().map(|(tag, _)| tag));
            }
        }
        // Matching that stopped early looks like matching that ran out of nodes.
        if cancelled(budget.cancel) || budget.timed_out.get() {
            return budget.stopped();
        }
        found.extend(queue.into_iter().map(|(tag, _)| tag));
        Ok(found)
    }
}

/// The indices into `found` of the tags to keep, in source order: tags with an identifier-like name, the first one
/// per name node and kind of tag, and no reference on the name of a definition.
///
/// `Engine::find` keeps one tag per name node, preferring the pattern written first, but only among the tags it has
/// not handed out yet. A pattern that matches late (a C++ class, once the body is seen) loses to a catch-all that
/// already matched the same name, so both can arrive.
fn select(grammar: Grammar, src: &[u8], found: &[Found]) -> Vec<usize> {
    let mut kept: HashMap<(usize, usize, bool), usize> = HashMap::new();
    for (i, tag) in found.iter().enumerate() {
        if std::str::from_utf8(&src[tag.name.clone()])
            .is_ok_and(|name| plausible_name(grammar, name))
        {
            kept.entry((tag.name.start, tag.name.end, tag.is_def))
                .or_insert(i);
        }
    }
    let mut indices: Vec<usize> = kept
        .iter()
        .filter(|&(&(start, end, is_def), _)| is_def || !kept.contains_key(&(start, end, true)))
        .map(|(_, &i)| i)
        .collect();
    indices.sort_by_key(|&i| (found[i].name.start, i));
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

/// `line_snippet` for each of `places`, the (line, column) pairs of one file, in the order given. The lines are found
/// once instead of once for each place, which would read a file of megabytes as often as it has places.
pub fn line_snippets(src: &[u8], places: &[(u32, u32)]) -> Vec<Option<(String, u32)>> {
    let starts = line_starts(src);
    places
        .iter()
        .map(|&(line, column)| {
            let start = *starts.get(line.checked_sub(1)? as usize)?;
            match start.checked_sub(1) {
                // From the line break before the line, so that a character at the start of the line that looks like a
                // byte order mark is not dropped like the one at the start of the file.
                Some(before) => line_snippet(&src[before..], 2, column),
                None => line_snippet(src, 1, column),
            }
        })
        .collect()
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
    // The indentation is not shown, so it does not count towards the name being too far into the line to fit.
    let head = head.trim_start();

    let head_units = head.encode_utf16().count();
    // A name too far into the line to fit gets a window that starts shortly before it.
    let drop_units = if head_units > SNIPPET_UNITS - SNIPPET_LEAD {
        head_units - SNIPPET_LEAD
    } else {
        0
    };
    let mut shown = head;
    let mut dropped = 0;
    for ch in head.chars() {
        if dropped >= drop_units {
            break;
        }
        dropped += ch.len_utf16();
        shown = &shown[ch.len_utf8()..];
    }
    // The window can start in the middle of the whitespace between two tokens.
    let shown = shown.trim_start();
    let mut units = 0;
    let mut snippet = String::new();
    for ch in shown.chars().chain(tail.chars()) {
        if units + ch.len_utf16() > SNIPPET_UNITS {
            break;
        }
        units += ch.len_utf16();
        snippet.push(ch);
    }
    snippet.truncate(snippet.trim_end().len());
    (snippet, shown.encode_utf16().count() as u32 + 1)
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

/// The number of UTF-16 units of `bytes`, where a run of invalid UTF-8 counts as one, like the parser counts it.
fn utf16_len(bytes: &[u8]) -> usize {
    bytes
        .utf8_chunks()
        .map(|chunk| {
            let valid: usize = chunk.valid().chars().map(char::len_utf16).sum();
            valid + usize::from(!chunk.invalid().is_empty())
        })
        .sum()
}

/// Start offsets of every line. A line ends with `\n`, which also covers `\r\n`: files with LF or CRLF line breaks are
/// the contract. A `\r` on its own is not a line break, as in tree-sitter's rows (an editor that counts it as one, for
/// a file from classic Mac OS, disagrees with the lines here).
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

/// Whether `name` looks like an identifier. The upstream queries also capture things like `ns.Thing`, `(getClass())` or
/// a template function with its arguments (`make<int>`).
///
/// C++ operators (`operator()`, `operator[]`, `operator new`, `operator delete[]`, `operator<`) are the one kind of
/// name with punctuation or a space in it, see `is_operator_name`.
fn plausible_name(grammar: Grammar, name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return false;
    }
    let operator = is_operator_name(grammar, name);
    // An operator may be written across a tab (`operator\t==`), but not across a line break.
    if name
        .chars()
        .any(|c| c.is_control() && !(operator && c == '\t'))
    {
        return false;
    }
    if operator {
        return true;
    }
    !name.chars().any(char::is_whitespace)
        && !name.contains([
            '.', ',', ';', ':', '(', ')', '[', ']', '{', '}', '"', '\'', '`', '\\', '<', '>',
        ])
}

/// Whether `name` is that of a C++ operator, told from an identifier that starts with `operator` (a JavaScript
/// `operators.Binary`) by the grammar and by the character after the keyword.
fn is_operator_name(grammar: Grammar, name: &str) -> bool {
    grammar == Grammar::Cpp
        && name
            .strip_prefix("operator")
            .is_some_and(|rest| !rest.starts_with(is_word_char))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The name of a tag, from the source text of the name. A C++ operator is written without the whitespace that does not
/// separate two words, so `operator ()` and `operator()` are one name, and `operator new[]` and `operator co_await` keep
/// the single space they need.
fn tag_name(grammar: Grammar, text: &[u8]) -> String {
    let name = String::from_utf8_lossy(text);
    if !is_operator_name(grammar, &name) {
        return name.into_owned();
    }
    let mut normalized = String::from("operator");
    let mut space = false;
    for c in name["operator".len()..].chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && is_word_char(c) && normalized.ends_with(is_word_char) {
            normalized.push(' ');
        }
        space = false;
        normalized.push(c);
    }
    normalized
}

fn cancelled(cancel: Option<&AtomicUsize>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed) != 0)
}

/// A compiled query and what its captures mean.
struct Config {
    language: Language,
    query: Query,
    /// For each capture of the query, by index.
    captures: Vec<Capture>,
}

enum Capture {
    /// `@name`, the node whose text is the name.
    Name,
    /// `@definition.<kind>` or `@reference.<kind>`, the node that is tagged.
    Tag { is_def: bool },
    /// Captures that only serve the predicates of the query (`@local.reference`), and those of the `tree-sitter-tags`
    /// crate that mean nothing to this runner (`@doc`, `@ignore`, `@local.scope`, `@local.definition`). A query written
    /// for the crate still loads, but what these captures do there is not done here: `@ignore` keeps no later pattern
    /// from tagging its name, and `@local.scope` and `@local.definition` do not hide the references to local variables
    /// from a pattern with `(#is-not? local)`, so such a query tags more than it would in the crate. They are accepted
    /// because the queries that ship with a grammar can start to use them in a release, and every file of that grammar
    /// would otherwise fail.
    Other,
}

impl Config {
    fn new(grammar: Grammar) -> Result<Self, String> {
        Config::compile(grammar, &tags_query(grammar))
    }

    /// `source` as the tags query of `grammar`. A capture that is neither one of ours nor one of the crate's is an error.
    fn compile(grammar: Grammar, source: &str) -> Result<Self, String> {
        let error = |what: &dyn fmt::Display| format!("{} tags query: {what}", grammar.id());
        let language = language(grammar);
        let query = Query::new(&language, source).map_err(|e| error(&e))?;
        let mut has_name = false;
        let mut captures = Vec::new();
        for name in query.capture_names() {
            captures.push(match *name {
                "name" => {
                    has_name = true;
                    Capture::Name
                }
                "local.reference" | "local.scope" | "local.definition" | "doc" | "ignore" => {
                    Capture::Other
                }
                _ if name.starts_with("definition.") => Capture::Tag { is_def: true },
                _ if name.starts_with("reference.") => Capture::Tag { is_def: false },
                _ => return Err(error(&format!("invalid capture @{name}"))),
            });
        }
        if !has_name {
            return Err(error(&"no @name capture"));
        }
        Ok(Config {
            language,
            query,
            captures,
        })
    }

    /// The kind of a `Capture::Tag`: `function` for `@definition.function`.
    fn kind(&self, capture: u32) -> &str {
        let name = self.query.capture_names()[capture as usize];
        name.split_once('.').map_or(name, |(_, kind)| kind)
    }
}

/// The configuration of a grammar, compiled on first use.
fn config(grammar: Grammar) -> Result<&'static Config, ParseError> {
    static CONFIGS: [OnceLock<Result<Config, String>>; Grammar::ALL.len()] =
        [const { OnceLock::new() }; Grammar::ALL.len()];
    CONFIGS[grammar as usize]
        .get_or_init(|| Config::new(grammar))
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

/// The query of a grammar. The TypeScript and TSX ones start from the JavaScript query and the upstream query of
/// TypeScript (the TypeScript grammar extends the JavaScript one); the catch-all `type` references stay last in each
/// query so any pattern written before them wins.
fn tags_query(grammar: Grammar) -> String {
    let typescript = tree_sitter_typescript::TAGS_QUERY;
    let parts: &[&str] = match grammar {
        Grammar::Rust => &[RUST_QUERY],
        Grammar::Go => &[GO_QUERY],
        Grammar::Python => &[PYTHON_QUERY],
        Grammar::JavaScript => &[ECMA_BASE_QUERY, ECMA_QUERY, JSX_QUERY, JAVASCRIPT_QUERY],
        Grammar::TypeScript => &[ECMA_BASE_QUERY, typescript, ECMA_QUERY, TYPESCRIPT_QUERY],
        Grammar::Tsx => &[
            ECMA_BASE_QUERY,
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

    /// The tags of `src`. The time limit is far above that of `parse`, so a busy machine cannot make a test of what is
    /// tagged fail by being slow; the tests of the limit itself call `parse` or `parse_within`.
    fn tags(grammar: Grammar, src: &str) -> Vec<Tag> {
        parse_within(grammar, src.as_bytes(), None, Duration::from_secs(600)).unwrap()
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
    fn c_functions_are_found_behind_declarators_and_not_among_their_parameters() {
        let src = r#"void takes_fn(int cb(int)) { cb(1); }
void takes_ptr(int (*cb)(int), char *names[]) {}
void takes_nested(int cb(int (*inner)(int))) {}
char ****h4(void) {
    return 0;
}
char *****h5(void) { return 0; }
int (*get(void))(int) { return 0; }
int (**get2(void))(int) { return 0; }
char *(**get3(void))[3] {
    return 0;
}
int *(*get4(void))(int) { return 0; }
int (*get5(void))[3] { return 0; }
void (*signal2(int sig, void (*func)(int)))(int) { return 0; }
int declared_only(int x);
"#;
        // A function declarator among the parameters is no function: `cb` is only called. The scope of the rest is
        // the whole definition.
        assert_eq!(
            defs(Grammar::C, src),
            [
                "function takes_fn 1:6 1-1",
                "function takes_ptr 2:6 2-2",
                "function takes_nested 3:6 3-3",
                "function h4 4:10 4-6",
                "function h5 7:11 7-7",
                "function get 8:7 8-8",
                "function get2 9:8 9-9",
                "function get3 10:10 10-12",
                "function get4 13:8 13-13",
                "function get5 14:7 14-14",
                "function signal2 15:8 15-15",
            ]
        );
        assert_has(&refs(Grammar::C, src), &["call cb 1:30"]);
    }

    #[test]
    fn cpp_functions_are_found_behind_declarators_and_not_among_their_parameters() {
        let src = r#"void takes_fn(int cb(int)) { cb(1); }
void takes_ptr(int (*cb)(int), char *names[]) {}
int (*get_cb(void))(int) { return 0; }
char ***h3(void) {
    return 0;
}
char *(**get3(void))[3] { return 0; }
Foo *f() { return 0; }
Foo *&g() { return x; }
Foo &&mv() { return x; }
const Foo &Bar::h() const { return x; }
template <typename T> T **make() { return 0; }
template <> int *make<int>() { return 0; }
static inline __alloc_size(1, 2) void *alloc_array(size_t n, size_t size) {
    return 0;
}
int declared_only(int x);
char ****h4(void) { return 0; }
char *****h5(void) { return 0; }
char ******h6(void) { return 0; }
"#;
        // The same as in C, with the qualified names and template functions of C++, and the macro with arguments in
        // front of a return type that the parser makes a parenthesized declarator of. A function declarator among the
        // parameters is no function: `cb` is only called. Up to five pointers are looked through, not six.
        assert_eq!(
            defs(Grammar::Cpp, src),
            [
                "function takes_fn 1:6 1-1",
                "function takes_ptr 2:6 2-2",
                "function get_cb 3:7 3-3",
                "function h3 4:9 4-6",
                "function get3 7:10 7-7",
                "function f 8:6 8-8",
                "function g 9:7 9-9",
                "function mv 10:7 10-10",
                "method h 11:17 11-11",
                "function make 12:27 12-12",
                "function make 13:18 13-13",
                "function alloc_array 14:40 14-16",
                "function h4 18:10 18-18",
                "function h5 19:11 19-19",
            ]
        );
        assert_has(&refs(Grammar::Cpp, src), &["call cb 1:30"]);
    }

    #[test]
    fn cpp_functions_are_found_behind_a_macro_with_arguments_and_inside_parentheses() {
        let src = r#"static inline __alloc_size(1, 2) void *alloc_ptr(size_t n) { return 0; }
static inline __alloc_size(1, 2) Foo &alloc_ref(int n) { return x; }
static inline __alloc_size(1, 2) void **alloc_ptrs(int n) { return 0; }
static inline __alloc_size(1, 2) Foo *&alloc_ptr_ref(int n) { return x; }
int (*get_cb(void))(int) { return 0; }
int (&get_arr())[3] { return x; }
void (&get_fn())(int) { return f; }
int (*&get_ref())(int) { return f; }
char *(**get_ptrs(void))[3] { return 0; }
char *(&get_ptr_arr())[3] { return x; }
"#;
        // The parser takes what follows the macro for a parenthesized declarator, which holds the pointers and the
        // reference of the return type. A function that returns a function pointer, or a pointer or reference to an
        // array or function, has its name inside parentheses.
        assert_eq!(
            defs(Grammar::Cpp, src),
            [
                "function alloc_ptr 1:40 1-1",
                "function alloc_ref 2:39 2-2",
                "function alloc_ptrs 3:41 3-3",
                "function alloc_ptr_ref 4:40 4-4",
                "function get_cb 5:7 5-5",
                "function get_arr 6:7 6-6",
                "function get_fn 7:8 7-7",
                "function get_ref 8:8 8-8",
                "function get_ptrs 9:10 9-9",
                "function get_ptr_arr 10:9 10-10",
            ]
        );
    }

    #[test]
    fn cpp_template_arguments_are_no_part_of_a_name() {
        let src = r#"void f() {
    auto p = std::make_unique<Foo>();
    std::forward<ActTypes>(args);
    auto q = a::b::make<int, long>(1);
    auto r = a::b::c::get<0>(x);
    T::template cast<int>(x);
    obj.template run<int>();
    obj.run<int>();
    ns::Foo<int>::create<int>();
    plain<int>(1);
}
template <> void Foo::bar<int>() {}
template <> void ns::Foo::bar<int>() {}
template <> void a::b::Foo::bar<int>() {}
template <> Foo *Foo::baz<int>() { return 0; }
template <> Foo &Foo::qux<int>() { return x; }
void g() {
    A::B::template get<0>(x);
    A::B::C::template get<0>(x);
}
"#;
        // Calls and method definitions are named by their identifier, under up to three levels of qualification, and
        // also when the template is a dependent name (`T::template cast<int>()`, `obj.template run<int>()`).
        assert_has(
            &refs(Grammar::Cpp, src),
            &[
                "call make_unique 2:19",
                "call forward 3:10",
                "call make 4:20",
                "call get 5:23",
                "call cast 6:17",
                "call run 7:18",
                "call run 8:9",
                "call create 9:19",
                "call plain 10:5",
                "call get 18:20",
                "call get 19:23",
            ],
        );
        assert_eq!(
            defs(Grammar::Cpp, src),
            [
                "function f 1:6 1-11",
                "method bar 12:23 12-12",
                "method bar 13:27 13-13",
                "method bar 14:29 14-14",
                "method baz 15:23 15-15",
                "method qux 16:23 16-16",
                "function g 17:6 17-20",
            ]
        );
        let found = tags(Grammar::Cpp, src);
        assert!(
            found.iter().all(|t| !t.name.contains(['<', '>'])),
            "{found:#?}"
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
    fn cpp_constructors_of_class_templates_are_constructors() {
        let src = r#"template <typename T> Foo<T>::Foo() {}
template <typename T> Foo<T>::Foo(const Foo &o) : v(o.v) {
}
template <typename T> Foo<T>::~Foo() {}
template <typename T> void Foo<T>::bar() {}
template <typename T> Foo<T>::Other() {}
ns::Foo<int>::Foo() {}
a::b::Foo<T>::Foo() {}
Outer<T>::Foo::Foo() {}
template <> Box<int *>::Box() {}
template <typename T> struct Box<T *> { Box() {} };
Foo::Foo() {}
"#;
        // The scope of a constructor is named like it, also when the scope is a template specialization. The
        // destructor and the methods that are not named like their class stay methods.
        assert_eq!(
            defs(Grammar::Cpp, src),
            [
                "constructor Foo 1:31 1-1",
                "constructor Foo 2:31 2-3",
                "method ~Foo 4:31 4-4",
                "method bar 5:36 5-5",
                "method Other 6:31 6-6",
                "constructor Foo 7:15 7-7",
                "constructor Foo 8:15 8-8",
                "constructor Foo 9:16 9-9",
                "constructor Box 10:25 10-10",
                "class Box 11:30 11-11",
                "constructor Box 11:41 11-11",
                "constructor Foo 12:6 12-12",
            ]
        );
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
    fn a_query_may_use_the_captures_of_the_tags_crate_but_no_others() {
        // `tree-sitter-tags` knows these captures; the queries that ship with a grammar can start to use them.
        let query = "(function_item name: (identifier) @name) @definition.function\n\
            (source_file) @local.scope\n\
            (let_declaration pattern: (identifier) @local.definition)\n\
            ((identifier) @local.reference (#eq? @local.reference \"x\"))\n\
            (line_comment) @doc\n\
            (block_comment) @ignore\n";
        let config = Config::compile(Grammar::Rust, query).unwrap();
        let names = config.query.capture_names();
        assert_eq!(names.len(), 7, "{names:?}");
        for (name, capture) in names.iter().zip(&config.captures) {
            match *name {
                "name" => assert!(matches!(capture, Capture::Name)),
                "definition.function" => assert!(matches!(capture, Capture::Tag { is_def: true })),
                _ => assert!(matches!(capture, Capture::Other), "@{name}"),
            }
        }

        // Any other capture is a mistake in the query and fails the grammar, as does a query that names nothing.
        let error = Config::compile(
            Grammar::Rust,
            "(function_item name: (identifier) @name) @definition.function\n(identifier) @unknown\n",
        )
        .err()
        .unwrap();
        assert!(error.contains("invalid capture @unknown"), "{error}");
        let error = Config::compile(Grammar::Rust, "(function_item) @definition.function")
            .err()
            .unwrap();
        assert!(error.contains("no @name capture"), "{error}");
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
        assert_eq!(ParseError::TimedOut.to_string(), "took too long to parse");
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

    /// A definition and then a run of `+` after an `=`: tree-sitter's query cursor needs time quadratic in the length of
    /// the run (half a minute for 500 000), the parser does not.
    fn slow_to_match() -> String {
        format!("function f() {{}}\nx = {}", "+".repeat(300_000))
    }

    /// Nested template arguments: the parser itself needs time quadratic in the depth (seconds for 150 000).
    fn slow_to_parse() -> String {
        let depth = 150_000;
        format!(
            "template<{}int> struct s;{}",
            "A<".repeat(depth),
            ">".repeat(depth)
        )
    }

    #[test]
    fn work_that_takes_too_long_is_given_up_on_and_says_so() {
        let limit = Duration::from_millis(200);
        for (grammar, src) in [
            (Grammar::JavaScript, slow_to_match()),
            (Grammar::Cpp, slow_to_parse()),
        ] {
            let started = Instant::now();
            // An error, not an empty file: the caller must not take it for what the file holds.
            assert_eq!(
                parse_within(grammar, src.as_bytes(), None, limit),
                Err(ParseError::TimedOut),
                "{grammar:?}"
            );
            let took = started.elapsed();
            assert!(took < Duration::from_secs(20), "{grammar:?} took {took:?}");
        }
        // The parser of the thread is fine afterwards, and real code does not come near the limit.
        assert_eq!(
            defs(Grammar::Cpp, "int f() { return 0; }"),
            ["function f 1:5 1-1"]
        );
        assert_eq!(
            defs(Grammar::JavaScript, "function g() {}"),
            ["function g 1:10 1-1"]
        );
    }

    #[test]
    fn a_cancel_flag_raised_while_working_stops_the_parse_and_the_matching() {
        for (grammar, src) in [
            (Grammar::JavaScript, slow_to_match()),
            (Grammar::Cpp, slow_to_parse()),
        ] {
            let cancel = AtomicUsize::new(0);
            let started = Instant::now();
            let found = std::thread::scope(|scope| {
                scope.spawn(|| {
                    std::thread::sleep(Duration::from_millis(100));
                    cancel.store(1, Ordering::SeqCst);
                });
                parse_within(
                    grammar,
                    src.as_bytes(),
                    Some(&cancel),
                    Duration::from_secs(600),
                )
            });
            assert_eq!(found, Err(ParseError::Cancelled), "{grammar:?}");
            let took = started.elapsed();
            assert!(took < Duration::from_secs(20), "{grammar:?} took {took:?}");
        }
        assert_eq!(
            defs(Grammar::JavaScript, "function g() {}"),
            ["function g 1:10 1-1"]
        );
    }

    #[test]
    fn the_loop_that_makes_the_tags_looks_at_the_deadline_too() {
        // A file this small is parsed and matched without tree-sitter looking at the clock even once, so only the
        // loop that turns what was found into tags can notice that the time is up.
        let src = b"pub fn one() {}\n";
        assert_eq!(
            parse_within(Grammar::Rust, src, None, Duration::ZERO),
            Err(ParseError::TimedOut)
        );
        let kept = parse_within(Grammar::Rust, src, None, Duration::from_secs(60)).unwrap();
        assert_eq!(kept.len(), 1);

        // A dense file, a tag in every four bytes: the loop looks again every `TAGS_PER_CHECK` tags (five times here)
        // and lets code through while there is time.
        let dense = "a();".repeat(5 * TAGS_PER_CHECK);
        assert_eq!(tags(Grammar::JavaScript, &dense).len(), 5 * TAGS_PER_CHECK);
    }

    #[test]
    fn nothing_is_tagged_below_the_depth_limit() {
        // The body of a function is a level of its own, so every function sits two levels below the one around it.
        let depth = MAX_DEPTH as usize / 2 + 2_000;
        let src = format!("{}{}", "function f() {\n".repeat(depth), "}".repeat(depth));
        let started = Instant::now();
        let found = tags(Grammar::JavaScript, &src);
        let took = started.elapsed();
        assert!(took < Duration::from_secs(20), "took {took:?}");
        let kept = MAX_DEPTH as usize / 2;
        assert!(
            (kept - 5..=kept + 5).contains(&found.len()),
            "{} tags",
            found.len()
        );
        // The outermost ones are kept.
        assert_eq!((found[0].line, found[0].scope_start), (1, 1));
        assert!(found.iter().all(|t| t.is_def && t.name == "f"));
        assert!(
            found
                .windows(2)
                .all(|pair| pair[0].line + 1 == pair[1].line)
        );
    }

    #[test]
    fn long_runs_of_comments_are_matched_in_linear_time() {
        // A pattern that collects the comments in front of a definition takes time quadratic in the length of the run.
        let lines = 100_000;
        for (grammar, comment) in [
            (Grammar::JavaScript, "// comment\n"),
            (Grammar::TypeScript, "/** comment */\n"),
            (Grammar::Tsx, "/* comment */ // comment\n"),
        ] {
            let src = format!("{}function f() {{}}\n", comment.repeat(lines));
            let started = Instant::now();
            let found = defs(grammar, &src);
            let took = started.elapsed();
            assert!(took < Duration::from_secs(20), "{grammar:?} took {took:?}");
            assert_eq!(
                found,
                [format!(
                    "function f {}:10 {}-{}",
                    lines + 1,
                    lines + 1,
                    lines + 1
                )]
            );
        }
    }

    #[test]
    fn comments_in_front_of_definitions_do_not_change_what_is_tagged() {
        let src = "/** The doc. */\nfunction a() {}\n// two\n// lines\nconst b = () => 1;\n/* x */ var c = function () {};\n\
                   /* y */\nclass D { /** m */ method() {} }\nexport default e = 1;\n";
        let expected = [
            "function a 2:10 2-2",
            "function b 5:7 5-5",
            "function c 6:13 6-6",
            "class D 8:7 8-8",
            "method method 8:20 8-8",
            "constant e 9:16 9-9",
        ];
        for grammar in [Grammar::JavaScript, Grammar::TypeScript, Grammar::Tsx] {
            assert_eq!(defs(grammar, src), expected, "{grammar:?}");
        }
    }

    #[test]
    fn garbage_and_truncated_source_do_not_panic() {
        // Running out of time is an answer too (a machine that is busy may not give a parse 1.5 seconds), not a panic.
        let parsed = |grammar: Grammar, src: &[u8]| match parse(grammar, src, None) {
            Ok(tags) => Some(tags),
            Err(ParseError::TimedOut) => None,
            Err(e) => panic!("{grammar:?}: {e}"),
        };
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
                parsed(grammar, &bytes);
            }
            parsed(grammar, "{(([<".repeat(3000).as_bytes());
            parsed(grammar, "\u{FEFF}".as_bytes());
        }
        for (grammar, src) in FIXTURES {
            for cut in (0..src.len()).step_by(11) {
                parsed(grammar, &src.as_bytes()[..cut]);
            }
        }
        // A name that is not UTF-8 is no name.
        let found = parsed(
            Grammar::C,
            b"int caf\xE9(void) { return 0; }\nint ok(void) { return 1; }\n",
        );
        let Some(found) = found else { return };
        assert!(found.iter().all(|t| t.name != "caf\u{FFFD}"));
        assert!(found.iter().any(|t| t.name == "ok" && t.is_def));
    }

    #[test]
    fn names_that_are_not_identifiers_are_left_out() {
        let long = "a".repeat(MAX_NAME_BYTES + 1);
        for grammar in Grammar::ALL {
            for name in [
                "foo",
                "Foo_1",
                "$scope",
                "_",
                "~Widget",
                "中文",
                "operator",
                "operators",
                "operator_",
                "operator1",
                "operatorFoo",
            ] {
                assert!(plausible_name(grammar, name), "{grammar:?} {name}");
            }
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
                // Only a C++ operator name can have punctuation or a space.
                "operators.Binary",
                "operator_.x",
                "operator1 x",
                // An operator may be written across a tab, not across a line break.
                "operator\n==",
                // A template function is named by its identifier, never with its template arguments.
                "make<int>",
                "forward<ActTypes>",
                "Foo<T>",
                "a>b",
                "operator_<int>",
                long.as_str(),
            ] {
                assert!(!plausible_name(grammar, name), "{grammar:?} {name:?}");
            }
            assert!(plausible_name(grammar, &"a".repeat(MAX_NAME_BYTES)));
        }
        for name in [
            "operator==",
            "operator<<",
            "operator->",
            "operator<",
            "operator<=",
            "operator<=>",
            "operator>",
            "operator>=",
            "operator>>=",
        ] {
            assert!(plausible_name(Grammar::Cpp, name), "{name}");
        }
        for name in [
            "operator()",
            "operator[]",
            "operator,",
            "operator co_await",
            "operator new",
            "operator delete[]",
            "operator\t==",
            "operator std::string",
        ] {
            assert!(plausible_name(Grammar::Cpp, name), "{name}");
            assert!(!plausible_name(Grammar::Go, name), "{name}");
        }
        assert!(!plausible_name(
            Grammar::Cpp,
            &format!("operator {}", "a".repeat(MAX_NAME_BYTES))
        ));
    }

    #[test]
    fn a_member_expression_is_no_name_even_when_it_starts_with_operator() {
        // The catch-all pattern for `new` takes whatever follows it as the name.
        let src = "new operators.Binary(1);\nnew Plain(2);\nnew ns.Thing(3);\n";
        assert_eq!(
            refs(Grammar::JavaScript, src),
            ["class Binary 1:15", "class Plain 2:5", "class Thing 3:8"]
        );
    }

    #[test]
    fn cpp_operators_are_methods_named_by_their_operator() {
        let src = "struct S {\n  void *operator new(size_t n) { return 0; }\n  void operator delete[](void *p) {}\n  bool operator()(int a) { return a; }\n  S operator+(S o) const { return o; }\n  bool operator==(const S &o) const { return true; }\n  bool operator<=>(const S &o) const = default;\n};\n";
        assert_eq!(
            defs(Grammar::Cpp, src),
            [
                "class S 1:8 1-8",
                "method operator new 2:9 2-2",
                "method operator delete[] 3:8 3-3",
                "method operator() 4:8 4-4",
                "method operator+ 5:5 5-5",
                "method operator== 6:8 6-6",
                "method operator<=> 7:8 7-7",
            ]
        );
        // A name that merely starts with `operator` is no operator name: not with a template argument list in it.
        let src = "void f() { ns::operator_<detail::op_pow,L,R>(); operatorFoo(1); }\n";
        let found = refs(Grammar::Cpp, src);
        assert_has(&found, &["call operatorFoo 1:49"]);
        assert_lacks(&found, &["operator_<detail::op_pow,L,R>"]);
    }

    #[test]
    fn cpp_operator_names_do_not_depend_on_the_spacing_of_the_source() {
        let src = r#"struct S {
    bool operator ()(int a) { return a; }
    bool operator  ==(const S &o) const { return true; }
    void *operator new [](size_t n) { return 0; }
    void operator delete   (void *p) {}
    void operator delete [] (void *p) {}
};
S S::operator  -(S o) { return o; }
bool operator   !=(const S &a, const S &b) { return false; }
long operator "" _mi(long double x) { return 0; }
long operator""_km(long double x) { return 0; }
"#;
        // Whitespace is written only where two words meet: `operator new`, but `operator()` and `operator delete[]`.
        assert_eq!(
            defs(Grammar::Cpp, src),
            [
                "class S 1:8 1-7",
                "method operator() 2:10 2-2",
                "method operator== 3:10 3-3",
                "method operator new[] 4:11 4-4",
                "method operator delete 5:10 5-5",
                "method operator delete[] 6:10 6-6",
                "method operator- 8:6 8-8",
                "method operator!= 9:6 9-9",
                "method operator\"\"_mi 10:6 10-10",
                "method operator\"\"_km 11:6 11-11",
            ]
        );
        // The columns are those of the name as written (`operator  ==` takes 12 units), not those of the tagged name.
        let found = tags(Grammar::Cpp, src);
        let operator = found.iter().find(|t| t.name == "operator==").unwrap();
        assert_eq!((operator.column, operator.end_column), (10, 22));
        assert_eq!(operator.snippet_column, 6);

        for (written, tagged) in [
            ("operator ()", "operator()"),
            ("operator new  []", "operator new[]"),
            ("operator   delete", "operator delete"),
            ("operator\t==", "operator=="),
            ("operator  co_await", "operator co_await"),
            ("operator \"\" _mi", "operator\"\"_mi"),
            ("operator<=>", "operator<=>"),
            ("operator", "operator"),
            ("operators.Binary", "operators.Binary"),
        ] {
            assert_eq!(tag_name(Grammar::Cpp, written.as_bytes()), tagged);
        }
        // Only a C++ operator is rewritten.
        assert_eq!(tag_name(Grammar::Go, b"operator  ()"), "operator  ()");
    }

    #[test]
    fn cpp_operators_may_be_written_across_a_tab_but_not_across_a_line_break() {
        let src = "struct S {\n  bool operator\t==(const S &o) const { return true; }\n  S &operator\t=(const S &o) { return *this; }\n  auto operator   co_await() { return 0; }\n  bool operator\n==(const S &o) const { return true; }\n};\n";
        assert_eq!(
            defs(Grammar::Cpp, src),
            [
                "class S 1:8 1-7",
                "method operator== 2:8 2-2",
                "method operator= 3:6 3-3",
                "method operator co_await 4:8 4-4",
            ]
        );
    }

    #[test]
    fn raw_identifiers_are_tagged_without_their_prefix() {
        let src = "fn r#type(r#match: u8) -> u8 { r#match }\nfn call() { r#type(1); let y: r#Foo = r#Foo::new(); }\n";
        let found = tags(Grammar::Rust, src);
        assert!(found.iter().all(|t| !t.name.contains('#')), "{found:#?}");
        let def = found.iter().find(|t| t.is_def && t.name == "type").unwrap();
        // The name starts after `r#`, in the line as in its snippet.
        assert_eq!((def.line, def.column, def.end_column), (1, 6, 10));
        assert_eq!(
            (def.snippet.as_str(), def.snippet_column),
            ("fn r#type(r#match: u8) -> u8 { r#match }", 6)
        );
        assert_has(
            &refs(Grammar::Rust, src),
            &["call type 2:15", "type Foo 2:33", "call new 2:46"],
        );
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
    fn line_snippets_are_those_of_line_snippet_for_every_place() {
        // A character that looks like a byte order mark starts the second line, and the first line has the real one.
        let src =
            "\u{FEFF}  fn a() {}\n\u{FEFF}fn b() {}\r\n\n\tfn c() {} // 😀 d\nlast".as_bytes();
        let places = [
            (1, 6),
            (2, 2),
            (2, 5),
            (3, 1),
            (4, 5),
            (4, 22),
            (5, 3),
            (5, 99),
            // Lines that do not exist.
            (6, 1),
            (0, 1),
            (1_000_000, 1),
        ];
        let got = line_snippets(src, &places);
        let expected: Vec<_> = places
            .iter()
            .map(|&(line, column)| line_snippet(src, line, column))
            .collect();
        assert_eq!(got, expected);
        // The mark of the file is ignored, the look-alike of the second line is part of it.
        assert_eq!(got[0], Some(("fn a() {}".to_string(), 4)));
        assert_eq!(got[1], Some(("\u{FEFF}fn b() {}".to_string(), 2)));
        assert_eq!(got[3], Some((String::new(), 1)));
        assert_eq!(got[8], None);
        assert!(line_snippets(src, &[]).is_empty());
        assert_eq!(
            line_snippets(b"", &[(1, 1), (2, 1)]),
            [Some((String::new(), 1)), None]
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
        // Indentation is not shown, so it does not push the name out: 80 units after 100 or after 1000 of indentation
        // are no further out than the 80, also when the indentation reaches beyond the bytes that are looked at.
        for indent in [100, 1000] {
            let line = format!("{}{} name() {{}}", " ".repeat(indent), "a".repeat(80));
            assert_eq!(
                line_snippet(line.as_bytes(), 1, indent as u32 + 82),
                Some((format!("{} name() {{}}", "a".repeat(80)), 82)),
                "{indent} spaces"
            );
        }
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

    #[test]
    fn a_snippet_does_not_start_with_the_whitespace_it_was_cut_at() {
        // The snippet starts 40 units before the name. Where that lands in whitespace (the text is `x x x ...`, so on
        // every other unit), it starts at the next token instead: the name is then 39 units in. 100 pairs fit in the
        // part of the line that is looked at, 800 do not (and 50 000 are a minified bundle).
        for pairs in [100, 800, 50_000] {
            let before = format!("{} ", "x ".repeat(pairs));
            let line = format!("{before}target(x) {}", "z".repeat(300));
            let name_column = before.len() as u32 + 1;
            let (snippet, column) = line_snippet(line.as_bytes(), 1, name_column).unwrap();
            assert_eq!(column, 40, "{pairs} pairs");
            assert!(snippet.starts_with("x x"), "{pairs} pairs: {snippet:?}");
            assert!(snippet[column as usize - 1..].starts_with("target(x)"));
            assert!(snippet.len() <= SNIPPET_UNITS);
        }
        // A run of whitespace longer than that leaves nothing in front of the name.
        let line = format!("x{}target(1) rest", " ".repeat(500));
        assert_eq!(
            line_snippet(line.as_bytes(), 1, 502),
            Some(("target(1) rest".to_string(), 1))
        );
        let found = tags(
            Grammar::JavaScript,
            &format!("var x = 1;{}function target() {{}}\n", " ".repeat(500)),
        );
        let target = found.iter().find(|t| t.name == "target").unwrap();
        assert_eq!(
            (target.snippet.as_str(), target.snippet_column),
            ("function target() {}", 10)
        );
    }
}
