//! Code navigation: the symbols of a file, and the index of a pull request's commits.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::git::GitAuth;
use crate::index::db::{BlobStatus, IndexDb, NewBlob};
use crate::index::lang::{Grammar, MAX_BLOB_BYTES};
use crate::index::tags::{self, Tag};
use crate::index::{IndexStatus, Indexer};
use crate::state::PrKey;

use super::PrService;

/// The outline of one file (`GET /blobs/{oid}/symbols`).
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSymbols {
    /// The id of the grammar; None for a file whose extension the index does not know.
    pub language: Option<&'static str>,
    /// Over `MAX_BLOB_BYTES`: not parsed, so without symbols.
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
        let Some(grammar) = Grammar::for_path(path, &bytes) else {
            return Ok(FileSymbols {
                language: None,
                too_large: false,
                symbols: Vec::new(),
            });
        };
        let size = bytes.len() as u64;
        let too_large = size > MAX_BLOB_BYTES;
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
                // Not stored yet, or left unreadable by a build that could not fetch the blob.
                _ => {}
            }
        }

        let (status, tags) = if too_large {
            (BlobStatus::TooLarge, Vec::new())
        } else {
            let tags = tokio::task::spawn_blocking(move || tags::parse(grammar, &bytes, None))
                .await
                .map_err(|e| e.to_string())
                .and_then(|parsed| parsed.map_err(|e| e.to_string()))
                .map_err(|e| AppError::index_unavailable(format!("parsing {path} failed: {e}")))?;
            (BlobStatus::Parsed, tags)
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
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::error::ErrorCode;
    use crate::index::IndexState;
    use crate::pr::testing::{Fixture, Pull, RUST, UNKNOWN};

    impl Fixture {
        async fn symbols(&self, oid: &str, path: &str) -> Result<FileSymbols, AppError> {
            self.service
                .symbols("github.com", "o", "r", oid, path)
                .await
        }
    }

    impl Pull {
        /// Waits until the build of `side` has ended, and returns how it ended.
        async fn end(&self, head: &str, side: Side) -> IndexStatus {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let status = self.service.index_status(&self.key, head, side).await;
                let status = status.unwrap();
                if !matches!(status.state, IndexState::Queued | IndexState::Building) {
                    return status;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out; the status is {status:?}"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
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
    }
}
