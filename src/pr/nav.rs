//! Code navigation: the symbols of a file, the index of a pull request's commits, and what it finds there.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::git::{GitAuth, Repo};
use crate::index::build::groups;
use crate::index::db::{BlobStatus, Hit, IndexDb, NewBlob};
use crate::index::lang::{Family, Grammar, MAX_BLOB_BYTES};
use crate::index::tags::{self, ParseError, Tag};
use crate::index::{IndexStatus, Indexer};
use crate::state::PrKey;

use super::PrService;

/// The outline of one file (`GET /blobs/{oid}/symbols`).
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSymbols {
    /// The id of the grammar; None for a file whose extension the index does not know.
    pub language: Option<&'static str>,
    /// Not parsed, so without symbols: the file is over `MAX_BLOB_BYTES`, or parsing it took too long.
    pub too_large: bool,
    pub symbols: Vec<Symbol>,
}

/// A definition in a file, or an `impl` block (kind `implementation`). Lines are 1-based and columns
/// 1-based UTF-16 code units, like Monaco's; `end_column` is exclusive.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    /// Where the name is.
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    /// The lines of the whole definition; `end_line` is the last one.
    pub start_line: u32,
    pub end_line: u32,
}

impl From<&Tag> for Symbol {
    fn from(tag: &Tag) -> Symbol {
        Symbol {
            name: tag.name.clone(),
            kind: tag.kind.clone(),
            line: tag.line,
            column: tag.column,
            end_column: tag.end_column,
            start_line: tag.scope_start,
            end_line: tag.scope_end,
        }
    }
}

/// The definitions and `implementation` tags among `tags`, sorted by (start line, column) like the
/// database returns them for a stored blob.
fn symbols_of(tags: &[Tag]) -> Vec<Symbol> {
    let mut symbols: Vec<Symbol> = tags
        .iter()
        .filter(|tag| tag.is_def || tag.kind == "implementation")
        .map(Symbol::from)
        .collect();
    symbols.sort_by_key(|symbol| (symbol.start_line, symbol.column));
    symbols
}

/// Runs a synchronous index call on the blocking pool.
async fn with_index<T: Send + 'static>(
    db: &IndexDb,
    call: impl FnOnce(&IndexDb) -> Result<T, String> + Send + 'static,
) -> Result<T, AppError> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || call(&db))
        .await
        .map_err(|e| AppError::index_unavailable(format!("the index task failed: {e}")))?
        .map_err(AppError::index_unavailable)
}

/// Which of the two commits of a pull request code navigation looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The head: the code the pull request proposes.
    Head,
    /// The merge base: the code it changes.
    Base,
}

/// `POST /pulls/{n}/index`: the commit to index.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexRequest {
    /// The head the client shows.
    pub head: String,
    pub side: Side,
}

/// How many bytes of blobs are read at once to cut the snippets of references out of them, by the sizes the index has
/// noted: a read holds its output in memory. A blob over it is read alone.
const SNIPPET_READ_BYTES: u64 = 16 << 20;

/// A definition or a reference found in the index (`GET /definitions` and `GET /references`). Lines are 1-based and
/// columns 1-based UTF-16 code units, like a [`Symbol`]'s.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavHit {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    pub kind: String,
    /// The trimmed source line and the column of the name inside it; empty when the line could not be read.
    pub snippet: String,
    pub snippet_column: u32,
    /// How close the file is to the one the lookup came from: `file`, `dir` or `other`.
    pub tier: &'static str,
}

impl From<Hit> for NavHit {
    fn from(hit: Hit) -> NavHit {
        NavHit {
            path: hit.path,
            line: hit.line,
            column: hit.column,
            end_column: hit.end_column,
            kind: hit.kind,
            snippet: hit.snippet,
            snippet_column: hit.snippet_column,
            tier: hit.tier.as_str(),
        }
    }
}

/// What a lookup found, closest to the file it came from first.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavResult {
    /// The id of the grammar of the file the lookup came from; None for a file of no language the index knows.
    pub language: Option<&'static str>,
    pub commit: String,
    pub name: String,
    pub hits: Vec<NavHit>,
    /// There are more references than were returned. Definitions are cut at the best 200 without saying so.
    pub truncated: bool,
}

impl NavResult {
    /// The answer for a file of no language the index knows: nothing to find.
    fn unsupported(commit: String, name: &str) -> NavResult {
        NavResult {
            language: None,
            commit,
            name: name.to_string(),
            hits: Vec::new(),
            truncated: false,
        }
    }
}

impl PrService {
    /// The symbols of the file `path` whose blob is `oid`. The bytes come through
    /// [`PrService::blob`], so an OID is served exactly where the blob endpoint serves it, whatever
    /// the index holds: it is shared by every repo. A blob is parsed once per grammar, and what is
    /// found is kept in the index, where a repository build finds it too.
    pub async fn symbols(
        &self,
        host: &str,
        owner: &str,
        repo_name: &str,
        oid: &str,
        path: &str,
    ) -> Result<FileSymbols, AppError> {
        let bytes = self.blob(host, owner, repo_name, oid).await?;
        let size = bytes.len() as u64;
        let too_large = size > MAX_BLOB_BYTES;
        // A header is C or C++ by its content, which is not looked at when the file is not parsed anyway (the index
        // holds such a header as C).
        let content: &[u8] = if too_large { &[] } else { &bytes };
        let Some(grammar) = Grammar::for_path(path, content) else {
            return Ok(FileSymbols {
                language: None,
                too_large: false,
                symbols: Vec::new(),
            });
        };
        let db = self.index.as_ref().map(Indexer::db);
        let outline = |symbols: Vec<Symbol>| FileSymbols {
            language: Some(grammar.id()),
            too_large,
            symbols,
        };

        if let Some(db) = db {
            let key = oid.to_string();
            let stored = with_index(db, move |db| db.blob_symbols(&key, grammar)).await?;
            match stored {
                Some(stored) if stored.status == BlobStatus::Parsed && !too_large => {
                    return Ok(outline(symbols_of(&stored.symbols)));
                }
                Some(stored) if stored.status == BlobStatus::TooLarge && too_large => {
                    return Ok(outline(Vec::new()));
                }
                // Not stored yet, or left unreadable by a build that could not fetch the blob or parse it in time. A
                // parse that works now replaces such a row.
                _ => {}
            }
        }

        let (status, tags) = if too_large {
            (BlobStatus::TooLarge, Vec::new())
        } else {
            let parsed = tokio::task::spawn_blocking(move || tags::parse(grammar, &bytes, None))
                .await
                .map_err(|e| AppError::index_unavailable(format!("parsing {path} failed: {e}")))?;
            match parsed {
                Ok(tags) => (BlobStatus::Parsed, tags),
                // Taking too long says something about this run, not about the file, so nothing is kept: a build, or
                // another request, tries again. The client is told what it is told of a file that is too large.
                Err(ParseError::TimedOut) => {
                    return Ok(FileSymbols {
                        language: Some(grammar.id()),
                        too_large: true,
                        symbols: Vec::new(),
                    });
                }
                Err(e) => {
                    return Err(AppError::index_unavailable(format!(
                        "parsing {path} failed: {e}"
                    )));
                }
            }
        };
        let symbols = symbols_of(&tags);
        if let Some(db) = db {
            let blob = NewBlob {
                oid: oid.to_string(),
                grammar,
                status,
                size,
                tags,
            };
            // The answer is at hand; failing to keep it only means parsing again next time.
            if let Err(e) = with_index(db, move |db| db.insert_blobs(vec![blob])).await {
                eprintln!("gh-tideline: keeping the symbols of {oid} failed: {e}");
            }
        }
        Ok(outline(symbols))
    }

    /// The index and the commit that `side` of the pull request means to the page showing `head`. A page
    /// that is behind the pull request gets `stale_head`: its positions are in files that have changed.
    async fn index_target(
        self: &Arc<Self>,
        key: &PrKey,
        head: &str,
        side: Side,
    ) -> Result<(&Indexer, String), AppError> {
        let index = self.index.as_ref().ok_or_else(|| {
            AppError::index_unavailable(
                "code navigation is unavailable: its index database could not be opened",
            )
        })?;
        let data = self.open_at(key, head).await?;
        if data.summary.head.sha != head {
            return Err(AppError::stale_head(
                "the pull request has new commits; refresh before using code navigation",
            ));
        }
        let commit = match side {
            Side::Head => data.summary.head.sha.clone(),
            Side::Base => data.summary.merge_base.clone(),
        };
        Ok((index, commit))
    }

    /// Starts building the index of one commit of the pull request, joins the build that is running, or
    /// reports that the commit is indexed already. Fetching blobs needs the viewer's token.
    pub async fn start_index(
        self: &Arc<Self>,
        key: &PrKey,
        head: &str,
        side: Side,
    ) -> Result<IndexStatus, AppError> {
        let (index, commit) = self.index_target(key, head, side).await?;
        let auth = GitAuth {
            host: key.host.clone(),
            token: self.tokens.get(&key.host).await?,
        };
        let repo = Arc::new(self.repos.repo(&key.host, &key.owner, &key.repo));
        index
            .start(repo, auth, &commit)
            .await
            .map_err(AppError::index_unavailable)
    }

    /// How far the index of one commit of the pull request has come.
    pub async fn index_status(
        self: &Arc<Self>,
        key: &PrKey,
        head: &str,
        side: Side,
    ) -> Result<IndexStatus, AppError> {
        let (index, commit) = self.index_target(key, head, side).await?;
        let repo = self.repos.repo(&key.host, &key.owner, &key.repo);
        index
            .status(&repo, &commit)
            .await
            .map_err(AppError::index_unavailable)
    }

    /// Asks the build of one commit of the pull request to stop; the answer is its status, which says
    /// `cancelling` until it has.
    pub async fn cancel_index(
        self: &Arc<Self>,
        key: &PrKey,
        head: &str,
        side: Side,
    ) -> Result<IndexStatus, AppError> {
        let (index, commit) = self.index_target(key, head, side).await?;
        let repo = self.repos.repo(&key.host, &key.owner, &key.repo);
        index
            .cancel(&repo, &commit)
            .await
            .map_err(AppError::index_unavailable)
    }

    /// Where `name` is defined in the commit, as the file `path` of the commit sees it: the definitions of its
    /// family of languages, the nearest first. Asks the index only: it never fetches and never starts a build.
    pub async fn definitions(
        self: &Arc<Self>,
        key: &PrKey,
        head: &str,
        side: Side,
        path: &str,
        name: &str,
    ) -> Result<NavResult, AppError> {
        let (index, commit) = self.index_target(key, head, side).await?;
        let Some(family) = Family::for_path(path) else {
            return Ok(NavResult::unsupported(commit, name));
        };
        let found = {
            let (commit, name, path) = (commit.clone(), name.to_string(), path.to_string());
            with_index(index.db(), move |db| {
                db.definitions(&commit, family, &name, &path)
            })
            .await?
        };
        let hits = found.ok_or_else(|| not_indexed(&commit))?;
        Ok(NavResult {
            language: Some(language_of(index.db(), &commit, path).await?),
            commit,
            name: name.to_string(),
            hits: hits.into_iter().map(NavHit::from).collect(),
            truncated: false,
        })
    }

    /// Where `name` is used in the commit, as the file `path` of the commit sees it: the references of its family of
    /// languages, the nearest first. The index knows where they are; the lines come from the blobs the cache repo
    /// holds. Nothing is fetched and no build is started, so a reference whose blob is not local has no snippet.
    pub async fn references(
        self: &Arc<Self>,
        key: &PrKey,
        head: &str,
        side: Side,
        path: &str,
        name: &str,
    ) -> Result<NavResult, AppError> {
        let (index, commit) = self.index_target(key, head, side).await?;
        let Some(family) = Family::for_path(path) else {
            return Ok(NavResult::unsupported(commit, name));
        };
        let found = {
            let (commit, name, path) = (commit.clone(), name.to_string(), path.to_string());
            with_index(index.db(), move |db| {
                db.references(&commit, family, &name, &path)
            })
            .await?
        };
        let (found, truncated) = found.ok_or_else(|| not_indexed(&commit))?;
        let repo = self.repos.repo(&key.host, &key.owner, &key.repo);
        let hits = with_snippets(&repo, index.db(), &commit, found).await?;
        Ok(NavResult {
            language: Some(language_of(index.db(), &commit, path).await?),
            commit,
            name: name.to_string(),
            hits,
            truncated,
        })
    }
}

fn not_indexed(commit: &str) -> AppError {
    AppError::index_missing(format!(
        "the index of commit {commit} is not complete; build it first"
    ))
}

/// The language of the file `path` of the commit: the grammar its extension names, and for a `.h` file the one the
/// index holds it under, which went by its content (C when the commit does not list it).
async fn language_of(db: &IndexDb, commit: &str, path: &str) -> Result<&'static str, AppError> {
    if let Some(grammar) = Grammar::for_path_only(path) {
        return Ok(grammar.id());
    }
    let (commit, path) = (commit.to_string(), path.to_string());
    let held = with_index(db, move |db| db.path_grammar(&commit, &path)).await?;
    Ok(held.unwrap_or(Grammar::C).id())
}

/// A hit that wants its snippet: its index among the hits, and the line and column of its name.
type Wanted = (usize, u32, u32);

/// The references with the line each is on, cut out of the blob that holds it (`found` has the OID of the blob with
/// each hit). Only the blobs `repo` holds for `commit` are read, a few at a time (see `SNIPPET_READ_BYTES`); nothing is
/// fetched, and a hit whose blob is not local, or cannot be read, has no snippet.
async fn with_snippets(
    repo: &Repo,
    db: &IndexDb,
    commit: &str,
    found: Vec<(Hit, String)>,
) -> Result<Vec<NavHit>, AppError> {
    let mut hits: Vec<NavHit> = Vec::with_capacity(found.len());
    let mut in_blob: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (hit, oid) in found {
        in_blob.entry(oid).or_default().push(hits.len());
        hits.push(hit.into());
    }
    if hits.is_empty() {
        return Ok(hits);
    }
    // Reading a blob the repo lacks is slow: git scans its promisor packs before it gives up. The index holds the
    // references of blobs that other clones of the repository fetched, so those are told first, with one listing.
    let missing = match repo.missing_objects(commit).await {
        Ok(missing) => missing,
        Err(e) => {
            eprintln!("gh-tideline: listing the blobs the repo lacks failed: {e}");
            return Ok(hits);
        }
    };
    let local: Vec<String> = in_blob
        .keys()
        .filter(|oid| !missing.contains(*oid))
        .cloned()
        .collect();
    let sizes = {
        let local = local.clone();
        with_index(db, move |db| db.blob_sizes(&local)).await
    }
    .unwrap_or_else(|e| {
        eprintln!(
            "gh-tideline: reading the sizes of blobs for the snippets of references failed: {e}"
        );
        HashMap::new()
    });
    // A blob of unknown size counts as the largest the index parses.
    let size = |oid: &String| sizes.get(oid).copied().unwrap_or(MAX_BLOB_BYTES);
    for group in groups(local, size, SNIPPET_READ_BYTES) {
        let blobs = match repo.read_blobs(&group).await {
            Ok(blobs) => blobs,
            Err(e) => {
                eprintln!("gh-tideline: reading blobs for the snippets of references failed: {e}");
                continue;
            }
        };
        let cut: Vec<(Vec<u8>, Vec<Wanted>)> = blobs
            .into_iter()
            .filter_map(|(oid, text)| {
                let at = in_blob.get(&oid)?;
                let wanted = at.iter().map(|&i| (i, hits[i].line, hits[i].column));
                Some((text, wanted.collect()))
            })
            .collect();
        let snippets = tokio::task::spawn_blocking(move || {
            let mut snippets = Vec::new();
            for (text, wanted) in cut {
                let places: Vec<(u32, u32)> = wanted
                    .iter()
                    .map(|&(_, line, column)| (line, column))
                    .collect();
                let found = tags::line_snippets(&text, &places);
                for (&(i, _, _), found) in wanted.iter().zip(found) {
                    if let Some((snippet, at)) = found {
                        snippets.push((i, snippet, at));
                    }
                }
            }
            snippets
        })
        .await
        .map_err(|e| AppError::index_unavailable(format!("reading snippets failed: {e}")))?;
        for (i, snippet, column) in snippets {
            hits[i].snippet = snippet;
            hits[i].snippet_column = column;
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use super::*;
    use crate::error::ErrorCode;
    use crate::index::{IndexState, Phase};
    use crate::pr::testing::{Fixture, Pull, RUST, UNKNOWN};

    impl Fixture {
        async fn symbols(&self, oid: &str, path: &str) -> Result<FileSymbols, AppError> {
            self.service
                .symbols("github.com", "o", "r", oid, path)
                .await
        }
    }

    impl Pull {
        /// The definitions of `name` for a click in `path` of the commit on `side`, as the page showing the
        /// current head asks for them.
        async fn definitions(
            &self,
            side: Side,
            path: &str,
            name: &str,
        ) -> Result<NavResult, AppError> {
            let head = self.clone.head.clone();
            self.service
                .definitions(&self.key, &head, side, path, name)
                .await
        }

        /// Like `definitions`, for the references.
        async fn references(
            &self,
            side: Side,
            path: &str,
            name: &str,
        ) -> Result<NavResult, AppError> {
            let head = self.clone.head.clone();
            self.service
                .references(&self.key, &head, side, path, name)
                .await
        }
    }

    fn symbol(name: &str, kind: &str, at: (u32, u32, u32), lines: (u32, u32)) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind: kind.to_string(),
            line: at.0,
            column: at.1,
            end_column: at.2,
            start_line: lines.0,
            end_line: lines.1,
        }
    }

    fn tag(name: &str, kind: &str, is_def: bool, column: u32, scope: (u32, u32)) -> Tag {
        Tag {
            name: name.to_string(),
            kind: kind.to_string(),
            is_def,
            line: scope.0,
            column,
            end_column: column + name.len() as u32,
            scope_start: scope.0,
            scope_end: scope.1,
            snippet: String::new(),
            snippet_column: 0,
        }
    }

    fn rust_symbols() -> Vec<Symbol> {
        vec![
            symbol("Parser", "class", (3, 12, 18), (3, 5)),
            symbol("Parser", "implementation", (7, 6, 12), (7, 11)),
            symbol("new", "method", (8, 12, 15), (8, 10)),
            symbol("helper", "function", (13, 4, 10), (13, 15)),
        ]
    }

    #[test]
    fn definitions_and_impl_blocks_are_symbols_in_source_order() {
        let tags = [
            tag("b", "function", true, 9, (5, 6)),
            tag("call", "call", false, 1, (5, 5)),
            tag("a", "function", true, 3, (5, 9)),
            tag("T", "implementation", false, 6, (2, 4)),
            tag("Ty", "type", false, 1, (1, 1)),
        ];
        let got: Vec<_> = symbols_of(&tags)
            .into_iter()
            .map(|s| (s.name, s.start_line, s.column))
            .collect();
        let want = [("T", 2, 6), ("a", 5, 3), ("b", 5, 9)];
        assert_eq!(got, want.map(|(n, l, c)| (n.to_string(), l, c)));
    }

    #[tokio::test]
    async fn a_file_of_a_language_the_index_does_not_know_has_no_symbols() {
        let f = Fixture::new("nav-unknown", &[b"# Title\n", b"fn f() {}\n"]).await;
        let none = FileSymbols {
            language: None,
            too_large: false,
            symbols: Vec::new(),
        };
        assert_eq!(f.symbols(&f.oids[0], "README.md").await.unwrap(), none);
        // The extension decides, not the content.
        assert_eq!(f.symbols(&f.oids[1], "src/main.zig").await.unwrap(), none);
        assert_eq!(f.symbols(&f.oids[1], "Makefile").await.unwrap(), none);
    }

    #[tokio::test]
    async fn a_rust_file_has_its_definitions_with_lines_and_columns() {
        let f = Fixture::new("nav-rust", &[RUST.as_bytes()]).await;
        let got = f.symbols(&f.oids[0], "src/parser.rs").await.unwrap();
        assert_eq!(got.language, Some("rust"));
        assert!(!got.too_large);
        assert_eq!(got.symbols, rust_symbols());
    }

    #[tokio::test]
    async fn columns_count_utf16_code_units() {
        // The emoji is two UTF-16 code units and four bytes.
        let f = Fixture::new(
            "nav-utf16",
            &["const A: &str = \"😀\"; fn g() {}\n".as_bytes()],
        )
        .await;
        let got = f.symbols(&f.oids[0], "a.rs").await.unwrap();
        let g = got.symbols.iter().find(|s| s.name == "g").unwrap();
        assert_eq!((g.line, g.column, g.end_column), (1, 26, 27));
    }

    #[tokio::test]
    async fn a_header_is_c_or_cpp_by_its_content() {
        let f = Fixture::new(
            "nav-header",
            &[
                b"int add(int a, int b);\n",
                b"class Shape { public: int sides; };\n",
            ],
        )
        .await;
        let c = f.symbols(&f.oids[0], "include/add.h").await.unwrap();
        assert_eq!(c.language, Some("c"));
        let cpp = f.symbols(&f.oids[1], "include/shape.h").await.unwrap();
        assert_eq!(cpp.language, Some("cpp"));
        assert!(cpp.symbols.iter().any(|s| s.name == "Shape"));
        // The same bytes are C under a `.c` name: the grammar is part of the cache key.
        let as_c = f.symbols(&f.oids[1], "shape.c").await.unwrap();
        assert_eq!(as_c.language, Some("c"));
        assert!(
            f.db.blob_symbols(&f.oids[1], Grammar::Cpp)
                .unwrap()
                .is_some()
        );
        assert!(f.db.blob_symbols(&f.oids[1], Grammar::C).unwrap().is_some());
    }

    #[tokio::test]
    async fn a_file_over_a_mebibyte_is_too_large_and_not_parsed() {
        let big = "// filler line\n".repeat(80_000);
        assert!(big.len() as u64 > MAX_BLOB_BYTES);
        let f = Fixture::new("nav-big", &[big.as_bytes()]).await;
        let want = FileSymbols {
            language: Some("rust"),
            too_large: true,
            symbols: Vec::new(),
        };
        assert_eq!(f.symbols(&f.oids[0], "big.rs").await.unwrap(), want);
        // The verdict is kept, without tags; asking again gives the same.
        let stored =
            f.db.blob_symbols(&f.oids[0], Grammar::Rust)
                .unwrap()
                .unwrap();
        assert_eq!(stored.status, BlobStatus::TooLarge);
        assert!(stored.symbols.is_empty());
        assert_eq!(f.symbols(&f.oids[0], "big.rs").await.unwrap(), want);
    }

    #[tokio::test]
    async fn what_is_parsed_is_kept_and_read_back() {
        let f = Fixture::new("nav-cache", &[RUST.as_bytes(), b"fn other() {}\n"]).await;
        assert!(
            f.db.blob_symbols(&f.oids[0], Grammar::Rust)
                .unwrap()
                .is_none()
        );
        let first = f.symbols(&f.oids[0], "a.rs").await.unwrap();
        let stored =
            f.db.blob_symbols(&f.oids[0], Grammar::Rust)
                .unwrap()
                .unwrap();
        assert_eq!(stored.status, BlobStatus::Parsed);
        assert_eq!(symbols_of(&stored.symbols), first.symbols);
        assert_eq!(f.symbols(&f.oids[0], "a.rs").await.unwrap(), first);

        // A stored blob is answered from the index, not parsed again: its row says something else.
        let blob = NewBlob {
            oid: f.oids[1].clone(),
            grammar: Grammar::Rust,
            status: BlobStatus::Parsed,
            size: 14,
            tags: vec![tag("kept", "function", true, 4, (1, 1))],
        };
        f.db.insert_blobs(vec![blob]).unwrap();
        let got = f.symbols(&f.oids[1], "other.rs").await.unwrap();
        assert_eq!(got.symbols, [symbol("kept", "function", (1, 4, 8), (1, 1))]);
    }

    #[tokio::test]
    async fn an_unreadable_row_does_not_hide_a_blob_that_can_be_read() {
        let f = Fixture::new("nav-unreadable", &[RUST.as_bytes()]).await;
        let blob = NewBlob {
            oid: f.oids[0].clone(),
            grammar: Grammar::Rust,
            status: BlobStatus::Unreadable,
            size: 0,
            tags: Vec::new(),
        };
        f.db.insert_blobs(vec![blob]).unwrap();
        let got = f.symbols(&f.oids[0], "a.rs").await.unwrap();
        assert_eq!(got.symbols, rust_symbols());
        // The parse that worked takes the place of the row that said the blob was unreadable, so that lookups find it.
        let stored =
            f.db.blob_symbols(&f.oids[0], Grammar::Rust)
                .unwrap()
                .unwrap();
        assert_eq!(stored.status, BlobStatus::Parsed);
        assert_eq!(symbols_of(&stored.symbols), got.symbols);
    }

    #[tokio::test]
    async fn a_file_that_takes_too_long_to_parse_is_answered_as_too_large_and_not_remembered() {
        // A definition and then a long run of `+` after an `=`: matching it takes tree-sitter far longer than a file is
        // given (see the tests of `tags`).
        let slow = format!("function f() {{}}\nx = {}", "+".repeat(300_000));
        assert!((slow.len() as u64) < MAX_BLOB_BYTES);
        let f = Fixture::new("nav-slow", &[slow.as_bytes(), b"function g() {}\n"]).await;
        let started = std::time::Instant::now();
        let got = f.symbols(&f.oids[0], "slow.js").await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(30));
        // Not a failure, and not a file without symbols: one that was not parsed.
        assert_eq!(
            got,
            FileSymbols {
                language: Some("javascript"),
                too_large: true,
                symbols: Vec::new(),
            }
        );
        // How long it took says something about this run, not about the file: nothing is kept, so that a build of the
        // commit, or another request, tries again.
        assert!(
            f.db.blob_symbols(&f.oids[0], Grammar::JavaScript)
                .unwrap()
                .is_none()
        );
        // The thread that parsed it is fine afterwards.
        let fine = f.symbols(&f.oids[1], "fine.js").await.unwrap();
        assert_eq!(fine.symbols, [symbol("g", "function", (1, 10, 11), (1, 1))]);
    }

    #[tokio::test]
    async fn a_header_too_large_to_parse_is_not_read_for_its_language() {
        // The content says C++, but a file that is not parsed is not worth the time it takes to look: the index holds
        // such a header as C, like the build does.
        let big = format!("class A {{}};\n{}", "// filler line\n".repeat(80_000));
        assert!(big.len() as u64 > MAX_BLOB_BYTES);
        let f = Fixture::new("nav-big-header", &[big.as_bytes()]).await;
        let got = f.symbols(&f.oids[0], "big.h").await.unwrap();
        assert_eq!(
            got,
            FileSymbols {
                language: Some("c"),
                too_large: true,
                symbols: Vec::new(),
            }
        );
        let held = |grammar| f.db.blob_symbols(&f.oids[0], grammar).unwrap();
        assert_eq!(held(Grammar::C).unwrap().status, BlobStatus::TooLarge);
        assert!(held(Grammar::Cpp).is_none());
    }

    #[tokio::test]
    async fn without_an_index_symbols_are_parsed_on_demand() {
        let f = Fixture::new("nav-no-index", &[RUST.as_bytes()]).await;
        let service = f.without_index();
        let got = service
            .symbols("github.com", "o", "r", &f.oids[0], "a.rs")
            .await
            .unwrap();
        assert_eq!(got.symbols, rust_symbols());
        // Nothing was stored anywhere.
        assert!(
            f.db.blob_symbols(&f.oids[0], Grammar::Rust)
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_index_does_not_open_a_door_the_blob_endpoint_keeps_shut() {
        let f = Fixture::new("nav-gate", &[RUST.as_bytes()]).await;
        // Symbols of another repo's blob are in the shared index, but this repo does not have the
        // blob and no pull request lists it.
        let blob = NewBlob {
            oid: UNKNOWN.to_string(),
            grammar: Grammar::Rust,
            status: BlobStatus::Parsed,
            size: 1,
            tags: vec![tag("secret", "function", true, 1, (1, 1))],
        };
        f.db.insert_blobs(vec![blob]).unwrap();
        let code = |r: Result<FileSymbols, AppError>| r.map(|_| ()).map_err(|e| e.code);
        assert_eq!(
            code(f.symbols(UNKNOWN, "a.rs").await),
            Err(ErrorCode::NotFound)
        );
        // A repo this process has not opened is unknown too.
        let other = f
            .service
            .symbols("github.com", "o", "never-opened", &f.oids[0], "a.rs")
            .await;
        assert_eq!(code(other), Err(ErrorCode::NotFound));
        // A known blob is served.
        assert!(f.symbols(&f.oids[0], "a.rs").await.is_ok());
    }

    #[tokio::test]
    async fn the_head_and_the_merge_base_are_indexed_apart() {
        let p = Pull::new("nav-sides").await;
        let (base, head) = (p.clone.base.clone(), p.clone.head.clone());

        let started = p
            .service
            .start_index(&p.key, &head, Side::Head)
            .await
            .unwrap();
        assert_eq!(
            (started.commit.as_str(), started.state),
            (head.as_str(), IndexState::Queued)
        );
        let ended = p.end(&head, Side::Head).await;
        assert_eq!(
            (ended.commit.as_str(), ended.state),
            (head.as_str(), IndexState::Ready)
        );
        assert!(p.db.commit_complete(&head).unwrap());

        // The merge base is another commit, which nobody has built.
        let other = p.service.index_status(&p.key, &head, Side::Base).await;
        let other = other.unwrap();
        assert_eq!(
            (other.commit.as_str(), other.state),
            (base.as_str(), IndexState::Missing)
        );
        assert!(!p.db.commit_complete(&base).unwrap());
        p.service
            .start_index(&p.key, &head, Side::Base)
            .await
            .unwrap();
        let ended = p.end(&head, Side::Base).await;
        assert_eq!(
            (ended.commit.as_str(), ended.state),
            (base.as_str(), IndexState::Ready)
        );
        assert!(p.db.commit_complete(&base).unwrap());

        // Built already: ready at once.
        let again = p.service.start_index(&p.key, &head, Side::Head).await;
        assert_eq!(again.unwrap().state, IndexState::Ready);
    }

    #[tokio::test]
    async fn a_build_is_cancelled_through_the_service() {
        let p = Pull::new("nav-cancel").await;
        let head = p.clone.head.clone();

        let nothing = p.service.cancel_index(&p.key, &head, Side::Head).await;
        assert_eq!(nothing.unwrap().state, IndexState::Missing);

        p.service
            .start_index(&p.key, &head, Side::Head)
            .await
            .unwrap();
        let cancelling = p
            .service
            .cancel_index(&p.key, &head, Side::Head)
            .await
            .unwrap();
        assert!(cancelling.cancelling);
        let ended = p.end(&head, Side::Head).await;
        assert_eq!(
            (ended.state, ended.cancelling),
            (IndexState::Cancelled, false)
        );
        assert!(!p.db.commit_complete(&head).unwrap());
    }

    /// Polls the status of the index of the head of `p` until `reached` holds for it.
    async fn index_until(p: &Pull, reached: impl Fn(&IndexStatus) -> bool) -> IndexStatus {
        let head = p.clone.head.clone();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let status = p.service.index_status(&p.key, &head, Side::Head).await;
            let status = status.unwrap();
            if reached(&status) {
                return status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out; the status is {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn draining_tells_the_builds_to_stop_before_it_waits_for_bookkeeping() {
        let p = Pull::new("nav-drain").await;
        let head = p.clone.head.clone();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();

        // A build waits for the repo, which something else holds.
        let guard = p.clone.repo.lock().await.unwrap();
        p.service
            .start_index(&p.key, &head, Side::Head)
            .await
            .unwrap();
        index_until(&p, |status| status.phase == Some(Phase::Fetching)).await;

        // Bookkeeping that does not end until it is let go.
        let (release, held) = tokio::sync::oneshot::channel::<()>();
        p.service.bookkeeping.lock().unwrap().spawn(async move {
            let _ = held.await;
        });
        let service = p.service.clone();
        let drain = tokio::spawn(async move { service.drain(Duration::from_secs(60)).await });

        // The build is cancelled at once, though the drain has to wait for the bookkeeping.
        index_until(&p, |status| status.cancelling).await;
        assert!(!drain.is_finished());

        // It ends when it gets the repo and finds that it was cancelled, without fetching.
        release.send(()).unwrap();
        drop(guard);
        drain.await.unwrap();
        let ended = index_until(&p, |status| status.state != IndexState::Building).await;
        assert_eq!(
            (ended.state, ended.cancelling, ended.done),
            (IndexState::Cancelled, false, 0)
        );
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);
    }

    #[tokio::test]
    async fn a_page_behind_the_pull_request_is_told_to_refresh() {
        let p = Pull::new("nav-stale").await;
        // A commit that is not the head the service knows.
        let behind = p.clone.base.clone();
        let code = |r: Result<IndexStatus, AppError>| r.map_err(|e| e.code);
        for side in [Side::Head, Side::Base] {
            assert_eq!(
                code(p.service.start_index(&p.key, &behind, side).await),
                Err(ErrorCode::StaleHead)
            );
            assert_eq!(
                code(p.service.index_status(&p.key, &behind, side).await),
                Err(ErrorCode::StaleHead)
            );
            assert_eq!(
                code(p.service.cancel_index(&p.key, &behind, side).await),
                Err(ErrorCode::StaleHead)
            );
        }
        // Neither does a lookup, whatever file it comes from.
        let lookup = |r: Result<NavResult, AppError>| r.map(|_| ()).map_err(|e| e.code);
        for side in [Side::Head, Side::Base] {
            for path in ["src/lib.rs", "README.md"] {
                let found = p.service.definitions(&p.key, &behind, side, path, "one");
                assert_eq!(lookup(found.await), Err(ErrorCode::StaleHead));
                let found = p.service.references(&p.key, &behind, side, path, "one");
                assert_eq!(lookup(found.await), Err(ErrorCode::StaleHead));
            }
        }
        // Nothing was started.
        let head = p.clone.head.clone();
        let status = p.service.index_status(&p.key, &head, Side::Head).await;
        assert_eq!(status.unwrap().state, IndexState::Missing);
    }

    #[tokio::test]
    async fn without_an_index_code_navigation_is_unavailable() {
        let p = Pull::new("nav-no-index-service").await;
        let service = p.without_index();
        let head = p.clone.head.clone();
        let code = |r: Result<IndexStatus, AppError>| r.map_err(|e| e.code);
        assert_eq!(
            code(service.start_index(&p.key, &head, Side::Head).await),
            Err(ErrorCode::IndexUnavailable)
        );
        assert_eq!(
            code(service.index_status(&p.key, &head, Side::Head).await),
            Err(ErrorCode::IndexUnavailable)
        );
        assert_eq!(
            code(service.cancel_index(&p.key, &head, Side::Head).await),
            Err(ErrorCode::IndexUnavailable)
        );
        let lookup = |r: Result<NavResult, AppError>| r.map(|_| ()).map_err(|e| e.code);
        for path in ["src/lib.rs", "README.md"] {
            let found = service.definitions(&p.key, &head, Side::Head, path, "one");
            assert_eq!(lookup(found.await), Err(ErrorCode::IndexUnavailable));
            let found = service.references(&p.key, &head, Side::Head, path, "one");
            assert_eq!(lookup(found.await), Err(ErrorCode::IndexUnavailable));
        }
    }

    /// `(path, line, column, tier)` of each hit.
    fn places(result: &NavResult) -> Vec<(&str, u32, u32, &str)> {
        result
            .hits
            .iter()
            .map(|hit| (hit.path.as_str(), hit.line, hit.column, hit.tier))
            .collect()
    }

    /// A hit on `name` at `at` (line, column) of `path`, on a line whose trimmed text is `snippet` with the name at
    /// its `snippet_column`. The names of the tests are ASCII, so the end column is the column plus the length.
    fn hit(
        name: &str,
        path: &str,
        at: (u32, u32),
        kind: &str,
        snippet: (&str, u32),
        tier: &'static str,
    ) -> NavHit {
        NavHit {
            path: path.to_string(),
            line: at.0,
            column: at.1,
            end_column: at.1 + name.len() as u32,
            kind: kind.to_string(),
            snippet: snippet.0.to_string(),
            snippet_column: snippet.1,
            tier,
        }
    }

    /// The files of `lookup_pull`'s head. `parse` is defined at line 5 of the first and used at line 10.
    const MAIN_RS: &str = "use std::fmt;

struct Local;

fn parse(input: u32) -> usize {
    input as usize
}

fn main() {
    let first = parse(1);
}
";
    const OTHER_RS: &str = "pub fn parse(buf: &[u8]) -> usize {
    buf.len()
}

pub fn caller() -> usize {
    parse(&[1])
}
";
    const MORE_RS: &str = "pub fn parse() {}

pub fn again() {
    parse();
}
";
    const TWICE_RS: &str = "pub fn parse() {}

pub fn twice() {
    parse();
    parse();
}
";
    const SCRIPT_PY: &str = "def parse(text):
    return parse(text)
";

    /// A pull request whose head defines `parse` in five Rust files and a Python one, and uses it in four Rust files
    /// and the Python one; its merge base has a different file in the first directory and another `parse`.
    async fn lookup_pull(name: &str) -> Pull {
        Pull::with_files(
            name,
            &[
                (
                    "src/a/main.rs",
                    "fn only_in_base() {}\n\nfn run() {\n    only_in_base();\n}\n",
                ),
                ("lib/parse.rs", "pub fn parse() {}\n"),
            ],
            &[
                ("src/a/main.rs", MAIN_RS),
                ("src/a/other.rs", OTHER_RS),
                ("src/a/deep/more.rs", MORE_RS),
                ("src/a/script.py", SCRIPT_PY),
                ("src/b/parse.rs", "pub fn parse() {}\n"),
                ("lib/parse.rs", TWICE_RS),
                ("README.md", "# parse\n"),
            ],
        )
        .await
    }

    #[tokio::test]
    async fn definitions_come_nearest_first_within_the_language_of_the_click() {
        let p = lookup_pull("nav-defs").await;
        let head = p.clone.head.clone();
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);

        let got = p
            .definitions(Side::Head, "src/a/main.rs", "parse")
            .await
            .unwrap();
        let func = |path, at, snippet, tier| hit("parse", path, at, "function", snippet, tier);
        assert_eq!(
            got,
            NavResult {
                language: Some("rust"),
                commit: head.clone(),
                name: "parse".to_string(),
                hits: vec![
                    // The same file, then its directory, then the rest by the directories shared with the file.
                    func(
                        "src/a/main.rs",
                        (5, 4),
                        ("fn parse(input: u32) -> usize {", 4),
                        "file"
                    ),
                    func(
                        "src/a/other.rs",
                        (1, 8),
                        ("pub fn parse(buf: &[u8]) -> usize {", 8),
                        "dir"
                    ),
                    func(
                        "src/a/deep/more.rs",
                        (1, 8),
                        ("pub fn parse() {}", 8),
                        "other"
                    ),
                    func("src/b/parse.rs", (1, 8), ("pub fn parse() {}", 8), "other"),
                    func("lib/parse.rs", (1, 8), ("pub fn parse() {}", 8), "other"),
                ],
                truncated: false,
            }
        );

        // From another file the order is another: no directory is shared, so the paths decide.
        let got = p
            .definitions(Side::Head, "lib/parse.rs", "parse")
            .await
            .unwrap();
        assert_eq!(
            places(&got),
            [
                ("lib/parse.rs", 1, 8, "file"),
                ("src/a/deep/more.rs", 1, 8, "other"),
                ("src/a/main.rs", 5, 4, "other"),
                ("src/a/other.rs", 1, 8, "other"),
                ("src/b/parse.rs", 1, 8, "other"),
            ]
        );

        // The Python file defines `parse` too, but a Rust click does not see it, and the other way round.
        let got = p
            .definitions(Side::Head, "src/a/script.py", "parse")
            .await
            .unwrap();
        assert_eq!(got.language, Some("python"));
        assert_eq!(
            got.hits,
            [hit(
                "parse",
                "src/a/script.py",
                (1, 5),
                "function",
                ("def parse(text):", 5),
                "file"
            )]
        );

        // Names are exact, and a name nobody defines has no hits.
        for name in ["Parse", "pars", "parse_all"] {
            let got = p
                .definitions(Side::Head, "src/a/main.rs", name)
                .await
                .unwrap();
            assert_eq!(got.language, Some("rust"), "{name}");
            assert_eq!(got.name, name);
            assert!(got.hits.is_empty(), "{name}: {:?}", got.hits);
            assert!(!got.truncated, "{name}");
        }
        let got = p
            .definitions(Side::Head, "src/a/main.rs", "caller")
            .await
            .unwrap();
        assert_eq!(places(&got), [("src/a/other.rs", 5, 8, "dir")]);
    }

    #[tokio::test]
    async fn references_have_their_lines_and_come_nearest_first() {
        let p = lookup_pull("nav-refs").await;
        let head = p.clone.head.clone();
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);

        let got = p
            .references(Side::Head, "src/a/main.rs", "parse")
            .await
            .unwrap();
        let call = |path, at, snippet, tier| hit("parse", path, at, "call", snippet, tier);
        assert_eq!(
            got,
            NavResult {
                language: Some("rust"),
                commit: head,
                name: "parse".to_string(),
                hits: vec![
                    call(
                        "src/a/main.rs",
                        (10, 17),
                        ("let first = parse(1);", 13),
                        "file"
                    ),
                    call("src/a/other.rs", (6, 5), ("parse(&[1])", 1), "dir"),
                    // The rest by path and line.
                    call("lib/parse.rs", (4, 5), ("parse();", 1), "other"),
                    call("lib/parse.rs", (5, 5), ("parse();", 1), "other"),
                    call("src/a/deep/more.rs", (4, 5), ("parse();", 1), "other"),
                ],
                truncated: false,
            }
        );

        let got = p
            .references(Side::Head, "lib/parse.rs", "parse")
            .await
            .unwrap();
        assert_eq!(
            places(&got),
            [
                ("lib/parse.rs", 4, 5, "file"),
                ("lib/parse.rs", 5, 5, "file"),
                ("src/a/deep/more.rs", 4, 5, "other"),
                ("src/a/main.rs", 10, 17, "other"),
                ("src/a/other.rs", 6, 5, "other"),
            ]
        );

        // Rust does not see the call in the Python file, and Python does not see the Rust ones.
        let got = p
            .references(Side::Head, "src/a/script.py", "parse")
            .await
            .unwrap();
        assert_eq!(got.language, Some("python"));
        assert_eq!(
            got.hits,
            [hit(
                "parse",
                "src/a/script.py",
                (2, 12),
                "call",
                ("return parse(text)", 8),
                "file"
            )]
        );

        // A definition is not a reference to its name, and a name nobody uses has no hits.
        for name in ["Parse", "main", "Local", "caller"] {
            let got = p
                .references(Side::Head, "src/a/main.rs", name)
                .await
                .unwrap();
            assert_eq!(got.name, name);
            assert!(got.hits.is_empty(), "{name}: {:?}", got.hits);
            assert!(!got.truncated, "{name}");
        }
    }

    #[tokio::test]
    async fn the_base_side_looks_in_the_merge_base() {
        let p = lookup_pull("nav-lookup-sides").await;
        let (base, head) = (p.clone.base.clone(), p.clone.head.clone());
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);
        assert_eq!(p.build(Side::Base).await.state, IndexState::Ready);

        let got = p
            .definitions(Side::Base, "src/a/main.rs", "only_in_base")
            .await
            .unwrap();
        assert_eq!(got.commit, base);
        assert_eq!(places(&got), [("src/a/main.rs", 1, 4, "file")]);
        let got = p
            .definitions(Side::Head, "src/a/main.rs", "only_in_base")
            .await
            .unwrap();
        assert_eq!(got.commit, head);
        assert!(got.hits.is_empty());

        let got = p
            .references(Side::Base, "src/a/main.rs", "only_in_base")
            .await
            .unwrap();
        assert_eq!(got.commit, base);
        assert_eq!(places(&got), [("src/a/main.rs", 4, 5, "file")]);
        assert_eq!(got.hits[0].snippet, "only_in_base();");
        assert_eq!(got.hits[0].snippet_column, 1);
        let got = p
            .references(Side::Head, "src/a/main.rs", "only_in_base")
            .await
            .unwrap();
        assert!(got.hits.is_empty());

        // `parse` is defined by one file of the merge base and five of the head, and used by none at the base.
        let got = p
            .definitions(Side::Base, "lib/parse.rs", "parse")
            .await
            .unwrap();
        assert_eq!(places(&got), [("lib/parse.rs", 1, 8, "file")]);
        let got = p
            .definitions(Side::Head, "lib/parse.rs", "parse")
            .await
            .unwrap();
        assert_eq!(got.hits.len(), 5);
        let got = p
            .references(Side::Base, "lib/parse.rs", "parse")
            .await
            .unwrap();
        assert!(got.hits.is_empty());
    }

    #[tokio::test]
    async fn references_stop_at_a_thousand_and_say_so() {
        let calls = format!(
            "{}{}",
            "    exact();\n".repeat(1000),
            "    over();\n".repeat(1001)
        );
        let generated = format!("fn exact() {{}}\n\nfn over() {{}}\n\nfn caller() {{\n{calls}}}\n");
        let p = Pull::with_files(
            "nav-truncated",
            &[("old.rs", "fn old() {}\n")],
            &[("gen.rs", generated.as_str())],
        )
        .await;
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);

        // Exactly a thousand is not more than that.
        let got = p.references(Side::Head, "gen.rs", "exact").await.unwrap();
        assert!(!got.truncated);
        assert_eq!(got.hits.len(), 1000);
        assert_eq!(
            (got.hits[0].line, got.hits[999].line),
            (6, 1005),
            "the references are in the order of the file"
        );

        let got = p.references(Side::Head, "gen.rs", "over").await.unwrap();
        assert!(got.truncated);
        assert_eq!(got.hits.len(), 1000);
        assert_eq!(
            (got.hits[0].line, got.hits[999].line),
            (1006, 2005),
            "the thousand nearest are kept, the last one is cut"
        );
        let line = |h: &NavHit| (h.column, h.snippet.clone(), h.snippet_column, h.tier);
        let want = (5, "over();".to_string(), 1, "file");
        assert!(got.hits.iter().all(|h| line(h) == want));

        // A definition is never reported as cut.
        let got = p.definitions(Side::Head, "gen.rs", "over").await.unwrap();
        assert_eq!(places(&got), [("gen.rs", 3, 4, "file")]);
        assert!(!got.truncated);
    }

    #[tokio::test]
    async fn every_reference_gets_its_snippet_however_many_blobs_hold_them() {
        let blobs = 40;
        let sources: Vec<(String, String)> = (0..blobs)
            .map(|i| {
                let source = format!("fn f{i}() {{\n    needle();\n}}\n");
                (format!("src/m{i:02}.rs"), source)
            })
            .collect();
        let head: Vec<(&str, &str)> = sources
            .iter()
            .map(|(path, source)| (path.as_str(), source.as_str()))
            .collect();
        let p = Pull::with_files("nav-many-blobs", &[("old.rs", "fn old() {}\n")], &head).await;
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);
        // The blobs are read in groups by the sizes the index has noted, which are made up here: two blobs to a group,
        // and one that is over what a group holds, which has one to itself.
        let alone = p.clone.oid(&p.clone.head, "src/m07.rs").await;
        let conn = rusqlite::Connection::open(p.clone.dir.join("index.sqlite")).unwrap();
        conn.execute("UPDATE blobs SET size = ?1", [6 << 20])
            .unwrap();
        conn.execute(
            "UPDATE blobs SET size = ?1 WHERE oid = ?2",
            rusqlite::params![(SNIPPET_READ_BYTES + 1) as i64, alone],
        )
        .unwrap();
        drop(conn);

        let got = p
            .references(Side::Head, "src/m07.rs", "needle")
            .await
            .unwrap();
        assert!(!got.truncated);
        let want: Vec<&str> = std::iter::once("src/m07.rs")
            .chain(
                sources
                    .iter()
                    .map(|(path, _)| path.as_str())
                    .filter(|path| *path != "src/m07.rs"),
            )
            .collect();
        let paths: Vec<&str> = got.hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(paths, want);
        assert_eq!(got.hits.len(), blobs);
        assert_eq!(got.hits[0].tier, "file");
        assert!(got.hits[1..].iter().all(|h| h.tier == "dir"));
        for h in &got.hits {
            let seen = (h.line, h.column, h.snippet.as_str(), h.snippet_column);
            assert_eq!(seen, (2, 5, "needle();", 1), "{}", h.path);
        }
    }

    #[tokio::test]
    async fn a_file_of_no_known_language_finds_nothing_and_starts_nothing() {
        let p = lookup_pull("nav-unsupported").await;
        let (base, head) = (p.clone.base.clone(), p.clone.head.clone());
        let nothing = |commit: &str| NavResult {
            language: None,
            commit: commit.to_string(),
            name: "parse".to_string(),
            hits: Vec::new(),
            truncated: false,
        };

        // No index is built, and none is asked for: the answer does not depend on one.
        for (side, commit) in [(Side::Head, &head), (Side::Base, &base)] {
            for path in ["README.md", "Makefile", "dir.rs/file", "src/.rs"] {
                let got = p.definitions(side, path, "parse").await.unwrap();
                assert_eq!(got, nothing(commit), "{path}");
                let got = p.references(side, path, "parse").await.unwrap();
                assert_eq!(got, nothing(commit), "{path}");
            }
        }
        for side in [Side::Head, Side::Base] {
            let status = p.service.index_status(&p.key, &head, side).await;
            assert_eq!(status.unwrap().state, IndexState::Missing);
        }

        // With the index, the name is defined and used elsewhere, which a click in such a file does not see.
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);
        let got = p.definitions(Side::Head, "README.md", "parse").await;
        assert_eq!(got.unwrap(), nothing(&head));
        let got = p.references(Side::Head, "README.md", "parse").await;
        assert_eq!(got.unwrap(), nothing(&head));
    }

    #[tokio::test]
    async fn a_commit_that_is_not_indexed_is_asked_to_be_built_first() {
        let p = lookup_pull("nav-not-built").await;
        let (base, head) = (p.clone.base.clone(), p.clone.head.clone());
        let lookup = |r: Result<NavResult, AppError>| r.map(|_| ()).map_err(|e| e.code);

        for side in [Side::Head, Side::Base] {
            let found = p.definitions(side, "src/a/main.rs", "parse").await;
            assert_eq!(lookup(found), Err(ErrorCode::IndexMissing));
            let found = p.references(side, "src/a/main.rs", "parse").await;
            assert_eq!(lookup(found), Err(ErrorCode::IndexMissing));
        }
        let error = p
            .definitions(Side::Head, "src/a/main.rs", "parse")
            .await
            .unwrap_err();
        assert!(error.message.contains(&head), "{}", error.message);
        let error = p
            .references(Side::Base, "src/a/main.rs", "parse")
            .await
            .unwrap_err();
        assert!(error.message.contains(&base), "{}", error.message);
        // Asking did not start a build.
        for side in [Side::Head, Side::Base] {
            let status = p.service.index_status(&p.key, &head, side).await;
            assert_eq!(status.unwrap().state, IndexState::Missing);
        }

        // The head is built; the merge base is another commit and still is not.
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);
        assert!(
            p.definitions(Side::Head, "src/a/main.rs", "parse")
                .await
                .is_ok()
        );
        assert!(
            p.references(Side::Head, "src/a/main.rs", "parse")
                .await
                .is_ok()
        );
        let found = p.definitions(Side::Base, "src/a/main.rs", "parse").await;
        assert_eq!(lookup(found), Err(ErrorCode::IndexMissing));
        let found = p.references(Side::Base, "src/a/main.rs", "parse").await;
        assert_eq!(lookup(found), Err(ErrorCode::IndexMissing));
        let status = p.service.index_status(&p.key, &head, Side::Base).await;
        assert_eq!(status.unwrap().state, IndexState::Missing);
    }

    #[tokio::test]
    async fn a_lookup_only_sees_the_languages_that_share_names_with_its_file() {
        let p = Pull::with_files(
            "nav-families",
            &[("old.rs", "fn old() {}\n")],
            &[
                ("web/app.ts", "export function main() {\n    render();\n}\n"),
                ("web/view.ts", "function render(): void {}\n"),
                (
                    "web/view.tsx",
                    "function render() {\n    return <div />;\n}\n",
                ),
                ("web/legacy.js", "function render() {}\n"),
                ("lib/other.ts", "function render(): void {}\n"),
                ("tool/render.py", "def render():\n    pass\n\nrender()\n"),
                (
                    "src/render.rs",
                    "fn render() {}\n\nfn run() {\n    render();\n}\n",
                ),
                (
                    "cmd/render.go",
                    "package main\n\nfunc render() {}\n\nfunc run() {\n\trender()\n}\n",
                ),
            ],
        )
        .await;
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);

        // TypeScript, TSX and JavaScript share names; the language is the one of the file of the click.
        let cases = [
            (
                "web/app.ts",
                "typescript",
                vec![
                    ("web/legacy.js", 1, 10, "dir"),
                    ("web/view.ts", 1, 10, "dir"),
                    ("web/view.tsx", 1, 10, "dir"),
                    ("lib/other.ts", 1, 10, "other"),
                ],
            ),
            (
                "web/view.tsx",
                "tsx",
                vec![
                    ("web/view.tsx", 1, 10, "file"),
                    ("web/legacy.js", 1, 10, "dir"),
                    ("web/view.ts", 1, 10, "dir"),
                    ("lib/other.ts", 1, 10, "other"),
                ],
            ),
            (
                "web/legacy.js",
                "javascript",
                vec![
                    ("web/legacy.js", 1, 10, "file"),
                    ("web/view.ts", 1, 10, "dir"),
                    ("web/view.tsx", 1, 10, "dir"),
                    ("lib/other.ts", 1, 10, "other"),
                ],
            ),
            (
                "tool/render.py",
                "python",
                vec![("tool/render.py", 1, 5, "file")],
            ),
            (
                "src/render.rs",
                "rust",
                vec![("src/render.rs", 1, 4, "file")],
            ),
            ("cmd/render.go", "go", vec![("cmd/render.go", 3, 6, "file")]),
        ];
        for (path, language, want) in cases {
            let got = p.definitions(Side::Head, path, "render").await.unwrap();
            assert_eq!(got.language, Some(language), "{path}");
            assert_eq!(places(&got), want, "{path}");
        }

        // The same for the uses.
        let got = p
            .references(Side::Head, "web/view.ts", "render")
            .await
            .unwrap();
        assert_eq!(got.language, Some("typescript"));
        assert_eq!(places(&got), [("web/app.ts", 2, 5, "dir")]);
        let got = p
            .references(Side::Head, "tool/render.py", "render")
            .await
            .unwrap();
        assert_eq!(got.language, Some("python"));
        assert_eq!(places(&got), [("tool/render.py", 4, 1, "file")]);
        let got = p
            .references(Side::Head, "src/render.rs", "render")
            .await
            .unwrap();
        assert_eq!(got.language, Some("rust"));
        assert_eq!(places(&got), [("src/render.rs", 4, 5, "file")]);
        let got = p
            .references(Side::Head, "cmd/render.go", "render")
            .await
            .unwrap();
        assert_eq!(got.language, Some("go"));
        assert_eq!(places(&got), [("cmd/render.go", 6, 2, "file")]);
    }

    #[tokio::test]
    async fn c_and_cpp_share_names_and_a_header_is_whichever_its_content_says() {
        let same_source =
            "int add(int a, int b) { return a + b; }\nint twice(int x) { return add(x, x); }\n";
        let p = Pull::with_files(
            "nav-c-family",
            &[("old.c", "int old(void) { return 0; }\n")],
            &[
                ("include/add.h", "int add(int a, int b);\n"),
                ("include/shape.h", "class Shape { public: int sides; };\n"),
                // One blob under two names, which are two grammars of the family.
                ("src/add.c", same_source),
                ("src/add.cc", same_source),
            ],
        )
        .await;
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);

        // The language of a `.h` file is the one the index holds it under, C when the commit does not list it.
        let cases = [
            ("include/add.h", "c"),
            ("include/shape.h", "cpp"),
            ("include/ghost.h", "c"),
            ("src/add.c", "c"),
            ("src/add.cc", "cpp"),
        ];
        for (path, language) in cases {
            let got = p.definitions(Side::Head, path, "add").await.unwrap();
            assert_eq!(got.language, Some(language), "{path}");
            let got = p.references(Side::Head, path, "add").await.unwrap();
            assert_eq!(got.language, Some(language), "{path}");
        }

        // A prototype is no definition; both bodies are, whatever grammar the click goes by.
        let want = [("src/add.c", 1, 5, "other"), ("src/add.cc", 1, 5, "other")];
        for path in ["include/add.h", "include/shape.h", "include/ghost.h"] {
            let got = p.definitions(Side::Head, path, "add").await.unwrap();
            assert_eq!(places(&got), want, "{path}");
        }
        let got = p
            .definitions(Side::Head, "include/shape.h", "Shape")
            .await
            .unwrap();
        assert_eq!(
            got.hits,
            [hit(
                "Shape",
                "include/shape.h",
                (1, 7),
                "class",
                ("class Shape { public: int sides; };", 7),
                "file"
            )]
        );

        // Two files hold the same blob, and each of their uses has its line.
        let got = p.references(Side::Head, "src/add.c", "add").await.unwrap();
        let line = "int twice(int x) { return add(x, x); }";
        assert_eq!(
            got.hits,
            [
                hit("add", "src/add.c", (2, 27), "call", (line, 27), "file"),
                hit("add", "src/add.cc", (2, 27), "call", (line, 27), "dir"),
            ]
        );
    }

    /// Files whose head has `target` defined in the first and used in both.
    const TARGET_SOURCES: [(&str, &str); 2] = [
        ("a.rs", "fn target() {}\n\nfn run() {\n    target();\n}\n"),
        ("b.rs", "fn other() {\n    target();\n}\n"),
    ];

    /// Stores the tags of the files of the head of `p` in the index, as a build in another clone of the repository
    /// would have: the cache repo of `p` holds none of the blobs. Returns the listing of the files.
    async fn stored_by_another_clone(
        p: &Pull,
        sources: &[(&str, &str)],
    ) -> Vec<(String, String, Grammar)> {
        let head = p.clone.head.clone();
        let mut blobs = Vec::new();
        let mut files = Vec::new();
        for &(path, source) in sources {
            let oid = p.clone.oid(&head, path).await;
            blobs.push(NewBlob {
                oid: oid.clone(),
                grammar: Grammar::Rust,
                status: BlobStatus::Parsed,
                size: source.len() as u64,
                tags: tags::parse(Grammar::Rust, source.as_bytes(), None).unwrap(),
            });
            files.push((path.to_string(), oid, Grammar::Rust));
        }
        p.db.insert_blobs(blobs).unwrap();
        files
    }

    #[tokio::test]
    async fn a_build_fetches_the_blobs_the_index_holds_so_that_their_references_have_lines() {
        let p = Pull::with_files("nav-held", &[("old.rs", "fn old() {}\n")], &TARGET_SOURCES).await;
        let head = p.clone.head.clone();
        // The index holds the tags of both files, from a build in another clone; this repo lacks the blobs.
        let files = stored_by_another_clone(&p, &TARGET_SOURCES).await;
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| before.contains(oid)));

        // There is nothing to parse, and the build fetches the blobs all the same: it is where their lines come from.
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| !after.contains(oid)));

        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(
            got.hits,
            [
                hit("target", "a.rs", (4, 5), "call", ("target();", 1), "file"),
                hit("target", "b.rs", (2, 5), "call", ("target();", 1), "dir"),
            ]
        );
    }

    #[tokio::test]
    async fn only_the_references_in_blobs_the_repo_holds_have_lines_and_nothing_is_fetched() {
        let sources = [
            TARGET_SOURCES[0],
            TARGET_SOURCES[1],
            ("c.rs", "fn third() {\n    target();\n}\n"),
        ];
        let p = Pull::with_files("nav-some-blobs", &[("old.rs", "fn old() {}\n")], &sources).await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        // This repo holds the blob of b.rs, and no other.
        p.clone.fetch(&[files[1].1.as_str()]).await;
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        assert_eq!(
            before,
            HashSet::from([files[0].1.clone(), files[2].1.clone()])
        );

        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(
            got.hits,
            [
                hit("target", "a.rs", (4, 5), "call", ("", 0), "file"),
                hit("target", "b.rs", (2, 5), "call", ("target();", 1), "dir"),
                hit("target", "c.rs", (2, 5), "call", ("", 0), "dir"),
            ]
        );
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn references_of_blobs_the_cache_repo_does_not_hold_have_no_snippets() {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files("nav-no-blobs", &[("old.rs", "fn old() {}\n")], &sources).await;
        let head = p.clone.head.clone();
        // The index of the commit is complete, but the cache repo never fetched its blobs: they were evicted, or
        // another repo's build stored them.
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| before.contains(oid)));

        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(
            got.hits,
            [
                hit("target", "a.rs", (4, 5), "call", ("", 0), "file"),
                hit("target", "b.rs", (2, 5), "call", ("", 0), "dir"),
            ]
        );
        // The definitions come from the index alone, with their lines.
        let got = p.definitions(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(
            got.hits,
            [hit(
                "target",
                "a.rs",
                (1, 4),
                "function",
                ("fn target() {}", 4),
                "file"
            )]
        );
        // Nothing was fetched to answer.
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert_eq!(after, before);
    }
}
