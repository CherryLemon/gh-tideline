//! Code navigation: the symbols of a file, the index of a pull request's commits, and what it finds there.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::error::AppError;
use crate::git::parse::refused_oids;
use crate::git::{GitAuth, Repo, STALL_INDEX};
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

/// How long a file whose parse timed out is answered as too large without being parsed again. Taking too long says
/// something about that run, not about the file, so the index keeps nothing; but a file that used up the whole time
/// limit once will most likely do so again, and every try holds a thread of the blocking pool for that long.
const SLOW_PARSE_TTL: Duration = Duration::from_secs(600);

/// How many such files are remembered at most; the oldest are forgotten first.
const SLOW_PARSES_KEPT: usize = 256;

/// When the parse of a blob under a grammar last timed out.
pub(super) type SlowParses = Mutex<HashMap<(String, Grammar), Instant>>;

/// How long a lookup waits for the blobs it fetches for the lines of its references, or that the task of another lookup
/// fetches. The fetch goes on in a task of its own after that, for a later lookup to find the blobs there, and the lookup
/// answers without the lines it lacks: the repo lock, which a fetch needs, may be held for long by a build or a pull
/// request that is loading.
pub(super) const SNIPPET_FETCH_WAIT: Duration = Duration::from_secs(5);

/// How long a blob whose fetch failed is not asked for again by a lookup. A remote that cannot be reached, or does not
/// give a blob, will most likely do the same at once, and every try holds the repo lock for as long as it takes.
const FETCH_FAILURE_TTL: Duration = Duration::from_secs(120);

/// How many such blobs are remembered at most; the oldest are forgotten first.
const FETCH_FAILURES_KEPT: usize = 4096;

/// How long the task of a lookup goes on asking the remote for the blobs it does not refuse, once it has refused some. A
/// remote names the first blob it refuses and stops there, so the blobs it names are noted and the others asked for
/// again, and every ask costs a round trip while the task holds the repo lock. The task asks again as long as the last
/// ask gave up a blob and this much time has not passed since the first; the blobs it has not asked for by then are
/// given back, for a later lookup.
pub(super) const FETCH_RETRY_BUDGET: Duration = Duration::from_secs(10);

/// What the fetch of a blob for the lines of references has come to.
enum Fetch {
    /// A task fetches the blob, or waits for the repo lock to. The lookups that want the blob wait for its flight.
    InFlight(Arc<Flight>),
    /// The fetch failed at that time.
    Failed(Instant),
}

/// The fetch of a task, as the lookups that want its blobs see it: they wait for it to end, and take the blobs it fetched.
struct Flight {
    /// Holds the blobs the task fetched, once it has fetched them. The channel closes when the claim of the task ends,
    /// which lets go of the lookups that wait when the task fetched nothing, whatever the reason.
    done: watch::Receiver<Option<Vec<String>>>,
}

impl Flight {
    /// Waits until the task tells which blobs it fetched, or its claim ends, and returns the blobs it fetched: none when
    /// it fetched none. A lookup that comes after the task has told gets them at once, whether or not the claim has ended
    /// since.
    async fn fetched(&self) -> Vec<String> {
        let mut done = self.done.clone();
        match done.wait_for(Option::is_some).await {
            Ok(fetched) => fetched.clone().unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }
}

/// The blobs of the cache repos (by git dir) that tasks fetch for the lines of references, and those whose fetch failed
/// lately. A blob is fetched by one task at a time: a lookup that wants a blob that a task has in flight waits for that
/// fetch, and queues none of its own for the repo lock, which a build or a pull request that is loading may hold for
/// long, ahead of the loads that come after. Shared with the tasks, which outlive the lookups that start them.
#[derive(Clone, Default)]
pub(super) struct SnippetFetches {
    blobs: Arc<Mutex<HashMap<(PathBuf, String), Fetch>>>,
    /// Set when the service shuts down (see [`PrService::drain`]): the tasks that have waited for the repo lock then
    /// leave it without fetching, and none starts.
    shut_down: Arc<AtomicBool>,
    /// How many tasks lookups have spawned (tests tell by it whether a lookup queued a fetch).
    #[cfg(test)]
    spawned: Arc<AtomicUsize>,
    /// How many fetches under way lookups have begun to wait for (tests tell by it that a lookup waits).
    #[cfg(test)]
    awaited: Arc<AtomicUsize>,
}

impl SnippetFetches {
    fn lock(&self) -> MutexGuard<'_, HashMap<(PathBuf, String), Fetch>> {
        // A claim gives its blobs back when it is dropped, which a panic does too: the lock must not panic again then. A
        // panic while it was held leaves nothing half done.
        self.blobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Claims the blobs among `oids` of the repo at `git_dir` for a task to fetch: those that no task fetches and whose
    /// fetch did not fail less than `FETCH_FAILURE_TTL` before `now`. The flights of the tasks that fetch the others are
    /// left to the claim, once each, for the lookup to wait for.
    fn claim(&self, git_dir: &Path, oids: Vec<String>, now: Instant) -> Claim {
        let (done, receiver) = watch::channel(None);
        let flight = Arc::new(Flight { done: receiver });
        let mut under_way: Vec<Arc<Flight>> = Vec::new();
        let mut blobs = self.lock();
        let oids = oids
            .into_iter()
            .filter(|oid| {
                let key = (git_dir.to_path_buf(), oid.clone());
                let free = match blobs.get(&key) {
                    None => true,
                    Some(Fetch::InFlight(other)) => {
                        // The claim does not wait for itself: a blob that is repeated is in flight with it after its
                        // first occurrence.
                        let seen = |known: &Arc<Flight>| Arc::ptr_eq(known, other);
                        if !seen(&flight) && !under_way.iter().any(seen) {
                            under_way.push(other.clone());
                        }
                        false
                    }
                    Some(&Fetch::Failed(at)) => {
                        now.saturating_duration_since(at) >= FETCH_FAILURE_TTL
                    }
                };
                if free {
                    blobs.insert(key, Fetch::InFlight(flight.clone()));
                }
                free
            })
            .collect();
        Claim {
            fetches: self.clone(),
            git_dir: git_dir.to_path_buf(),
            oids,
            flight,
            done,
            under_way,
        }
    }

    /// Notes that the fetch of the blobs of the repo at `git_dir` failed at `now`. The notes that have expired are
    /// dropped, and when more than `FETCH_FAILURES_KEPT` are left, the oldest of them too. The blobs that are being
    /// fetched are not notes, and stay.
    fn note(&self, git_dir: &Path, oids: &[String], now: Instant) {
        let mut blobs = self.lock();
        blobs.retain(|_, fetch| match fetch {
            Fetch::InFlight(_) => true,
            Fetch::Failed(at) => now.saturating_duration_since(*at) < FETCH_FAILURE_TTL,
        });
        for oid in oids {
            blobs.insert((git_dir.to_path_buf(), oid.clone()), Fetch::Failed(now));
        }
        let noted = blobs
            .values()
            .filter(|fetch| matches!(fetch, Fetch::Failed(_)))
            .count();
        if noted > FETCH_FAILURES_KEPT {
            let mut by_age: Vec<(Instant, (PathBuf, String))> = blobs
                .iter()
                .filter_map(|(key, fetch)| match fetch {
                    Fetch::Failed(at) => Some((*at, key.clone())),
                    Fetch::InFlight(_) => None,
                })
                .collect();
            by_age.sort();
            for (_, key) in by_age.into_iter().take(noted - FETCH_FAILURES_KEPT) {
                blobs.remove(&key);
            }
        }
    }

    /// From now on the tasks that fetch do not start git (see [`PrService::drain`]).
    pub(super) fn shut_down(&self) {
        self.shut_down.store(true, Ordering::SeqCst);
    }

    fn is_shut_down(&self) -> bool {
        self.shut_down.load(Ordering::SeqCst)
    }
}

/// The blobs that one task fetches. Dropping the claim, whichever way the task ends (a lookup that is dropped before
/// it spawned the task, a panic, no repo lock to be had), gives back the ones it still holds, and lets go of the lookups
/// that wait for the task.
struct Claim {
    fetches: SnippetFetches,
    git_dir: PathBuf,
    /// The blobs that the task has not given up.
    oids: Vec<String>,
    /// The flight of the task, which the entries of its blobs hold.
    flight: Arc<Flight>,
    /// Tells the flight which blobs the task fetched.
    done: watch::Sender<Option<Vec<String>>>,
    /// The flights of the tasks that fetch the other blobs the lookup of the claim asked for, once each.
    under_way: Vec<Arc<Flight>>,
}

impl Claim {
    /// Gives up the blobs among `failed` that the claim holds, and notes that their fetch failed at `now`. Returns how
    /// many it gave up.
    fn fail(&mut self, failed: &[String], now: Instant) -> usize {
        let (given_up, kept): (Vec<String>, Vec<String>) = std::mem::take(&mut self.oids)
            .into_iter()
            .partition(|oid| failed.contains(oid));
        self.oids = kept;
        self.fetches.note(&self.git_dir, &given_up, now);
        given_up.len()
    }

    /// Tells the lookups that wait for the task that it has fetched the blobs it holds, and returns them.
    fn finish(&self) -> Vec<String> {
        self.done.send_replace(Some(self.oids.clone()));
        self.oids.clone()
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut blobs = self.fetches.lock();
        for oid in &self.oids {
            // What is no longer the claim's own stays: a note of a failed fetch, or the blob in flight with another task.
            let key = (self.git_dir.clone(), oid.clone());
            let own = match blobs.get(&key) {
                Some(Fetch::InFlight(flight)) => Arc::ptr_eq(flight, &self.flight),
                _ => false,
            };
            if own {
                blobs.remove(&key);
            }
        }
    }
}

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
    /// There are more definitions (than the nearest 200) or references (than the first 1000) than were returned.
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
        // What a file that was not parsed in time is told: what is told of a file that is too large.
        let timed_out = || FileSymbols {
            language: Some(grammar.id()),
            too_large: true,
            symbols: Vec::new(),
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
                // Not stored yet, or left unreadable by a build that could not fetch the blob or parse it in time (a
                // parse that works now replaces such a row). Or held by a newer version, whose tags are not known, so
                // not served: the row stays as it is (see `IndexDb::insert_blobs`), and every request for the outline
                // parses the blob again. That costs a parse of at most `MAX_BLOB_BYTES` each, for a blob that a newer
                // version of the server stored in the database they share; one that takes too long is not tried again
                // at once (see `SLOW_PARSE_TTL`).
                _ => {}
            }
        }

        let (status, tags) = if too_large {
            (BlobStatus::TooLarge, Vec::new())
        } else {
            // A file that timed out a moment ago is not worth another thread of the pool for as long.
            if self.parse_was_slow(oid, grammar, Instant::now()) {
                return Ok(timed_out());
            }
            let parsed = tokio::task::spawn_blocking(move || tags::parse(grammar, &bytes, None))
                .await
                .map_err(|e| AppError::index_unavailable(format!("parsing {path} failed: {e}")))?;
            match parsed {
                Ok(tags) => (BlobStatus::Parsed, tags),
                // Taking too long says something about this run, not about the file, so nothing is kept in the index:
                // a build tries again, and so does a request once the time of `SLOW_PARSE_TTL` has gone by. The client
                // is told what it is told of a file that is too large.
                Err(ParseError::TimedOut) => {
                    self.note_slow_parse(oid, grammar, Instant::now());
                    return Ok(timed_out());
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

    /// Whether parsing the blob under the grammar timed out less than `SLOW_PARSE_TTL` before `now`.
    fn parse_was_slow(&self, oid: &str, grammar: Grammar, now: Instant) -> bool {
        let slow = self.slow_parses.lock().expect("slow parses poisoned");
        slow.get(&(oid.to_string(), grammar))
            .is_some_and(|&at| now.saturating_duration_since(at) < SLOW_PARSE_TTL)
    }

    /// Notes that parsing the blob under the grammar timed out at `now`. The notes that have expired are dropped, and
    /// when `SLOW_PARSES_KEPT` are left, the oldest of them too.
    fn note_slow_parse(&self, oid: &str, grammar: Grammar, now: Instant) {
        let mut slow = self.slow_parses.lock().expect("slow parses poisoned");
        slow.retain(|_, at| now.saturating_duration_since(*at) < SLOW_PARSE_TTL);
        let key = (oid.to_string(), grammar);
        if slow.len() >= SLOW_PARSES_KEPT && !slow.contains_key(&key) {
            let oldest = slow
                .iter()
                .min_by_key(|&(_, &at)| at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                slow.remove(&oldest);
            }
        }
        slow.insert(key, now);
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
        let (hits, truncated) = found.ok_or_else(|| not_indexed(&commit))?;
        Ok(NavResult {
            language: Some(language_of(index.db(), &commit, path).await?),
            commit,
            name: name.to_string(),
            hits: hits.into_iter().map(NavHit::from).collect(),
            truncated,
        })
    }

    /// Where `name` is used in the commit, as the file `path` of the commit sees it: the references of its family of
    /// languages, the nearest first. The index knows where they are; the lines come from the blobs of the cache repo.
    /// The index may hold a commit that another clone of the repository built, so the blobs of the references that the
    /// repo lacks are fetched, as far as the remote gives them and for as long as the lookup can wait (see
    /// `SNIPPET_FETCH_WAIT`): a reference whose blob cannot be had in time has no snippet. No build is started.
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
        let hits = self.with_snippets(key, index.db(), &commit, found).await?;
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

impl PrService {
    /// The references with the line each is on, cut out of the blob that holds it (`found` has the OID of the blob with
    /// each hit). The index holds the references of blobs that other clones of the repository fetched, so the blobs of the
    /// hits that the cache repo lacks are fetched first (see `fetch_for_snippets`). The blobs are read a few at a time (see
    /// `SNIPPET_READ_BYTES`), and a hit whose blob could not be had, or cannot be read, has no snippet.
    async fn with_snippets(
        &self,
        key: &PrKey,
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
        let repo = self.repos.repo(&key.host, &key.owner, &key.repo);
        // Reading a blob the repo lacks is slow: git scans its promisor packs before it gives up. So the blobs the repo
        // lacks are told first, with one listing, and those of the hits are fetched.
        let mut missing = match repo.missing_objects(commit).await {
            Ok(missing) => missing,
            Err(e) => {
                eprintln!("gh-tideline: listing the blobs the repo lacks failed: {e}");
                return Ok(hits);
            }
        };
        let lacking: Vec<String> = in_blob
            .keys()
            .filter(|oid| missing.contains(*oid))
            .cloned()
            .collect();
        if !lacking.is_empty() {
            // A git fetch of blobs by their OIDs gives all it asks for or fails, so the blobs a task reports it fetched
            // are local and need no second listing of the objects the repo lacks (a build lists them again, since it goes
            // on after a fetch of blobs that failed). The others stay missing: those whose fetch failed lately, and those
            // that did not come in time, whether a task of this lookup or of another one fetches them.
            for oid in self.fetch_for_snippets(key, lacking).await {
                missing.remove(&oid);
            }
        }
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
                    eprintln!(
                        "gh-tideline: reading blobs for the snippets of references failed: {e}"
                    );
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

    /// Fetches blobs the cache repo lacks, for the snippets of a lookup, and returns those that came in time (the blobs
    /// that the tasks it waited for fetched, which may include others than it asked for). It is best effort: a lookup
    /// without lines is better than none, so no token, a remote that cannot be reached or one that does not give the
    /// blobs only costs the lines (and is told on stderr), and so does a blob whose fetch failed lately (see
    /// `FETCH_FAILURE_TTL`). One task fetches a blob at a time: a task of this lookup fetches the blobs that no task has
    /// in flight, and for the others the lookup waits for the task that has them, which the client may have given up on
    /// when it asked again. The lookup waits `snippet_fetch_wait` at most, for all the tasks together: they go on after
    /// that, so that a later lookup finds the blobs. They need the repo lock, which a build or a pull request that is
    /// loading may hold for long, and give up on a transfer that stays under 1 kB/s for `STALL_INDEX`.
    ///
    /// A window is left open: the entries of a task go when its claim ends, so a lookup that listed the blobs the repo
    /// lacks just before the task fetched them claims them again, and its task finds them local. That costs one git fetch
    /// queued for the repo lock that transfers nothing (git starts no upload-pack for objects it has), and ends with the
    /// blobs reported as fetched, which they are.
    async fn fetch_for_snippets(&self, key: &PrKey, oids: Vec<String>) -> Vec<String> {
        let repo = self.repos.repo(&key.host, &key.owner, &key.repo);
        let mut claim = self
            .snippet_fetches
            .claim(&repo.git_dir, oids, Instant::now());
        let under_way = std::mem::take(&mut claim.under_way);
        let task = self.spawn_fetch(key, repo, claim).await;
        #[cfg(test)]
        self.snippet_fetches
            .awaited
            .fetch_add(under_way.len(), Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + self.snippet_fetch_wait;
        let mut fetched = Vec::new();
        let mut in_time = true;
        if let Some(task) = task {
            match tokio::time::timeout_at(deadline, task).await {
                Ok(Ok(done)) => fetched = done,
                Ok(Err(e)) => {
                    eprintln!(
                        "gh-tideline: the fetch of the blobs for the snippets of references failed: {e}"
                    );
                }
                Err(_) => in_time = false,
            }
        }
        // The tasks run on their own, so that waiting for them one after the other under one deadline takes as long as
        // waiting for all of them at once.
        for flight in under_way {
            match tokio::time::timeout_at(deadline, flight.fetched()).await {
                Ok(done) => fetched.extend(done),
                Err(_) => in_time = false,
            }
        }
        if !in_time {
            eprintln!(
                "gh-tideline: the blobs for the snippets of references did not come within {:?}; the lookup answers without their lines",
                self.snippet_fetch_wait
            );
        }
        fetched
    }

    /// Spawns the task that fetches the blobs of `claim`. None, with the claim given back, when it holds none, the
    /// viewer has no token (told on stderr) or the service is shutting down.
    async fn spawn_fetch(
        &self,
        key: &PrKey,
        repo: Repo,
        claim: Claim,
    ) -> Option<JoinHandle<Vec<String>>> {
        if claim.oids.is_empty() {
            return None;
        }
        let token = match self.tokens.get(&key.host).await {
            Ok(token) => token,
            Err(e) => {
                eprintln!(
                    "gh-tideline: no token to fetch the blobs for the snippets of references: {e}"
                );
                return None;
            }
        };
        // The task would find the service shutting down as well, once it had the repo lock (see `fetch_under_lock`).
        if self.snippet_fetches.is_shut_down() {
            return None;
        }
        let auth = GitAuth {
            host: key.host.clone(),
            token,
        };
        #[cfg(test)]
        self.snippet_fetches.spawned.fetch_add(1, Ordering::SeqCst);
        Some(tokio::spawn(fetch_under_lock(
            repo,
            auth,
            claim,
            self.snippet_fetch_budget,
        )))
    }
}

/// The task that fetches the blobs of a claim for `PrService::fetch_for_snippets`, and returns the ones it fetched. It
/// owns the repo lock, so that a lookup that is dropped, or has stopped waiting, does not leave git running without it.
/// When the fetch fails, the blobs that the remote names as refused are noted, and the others are asked for again for as
/// long as the last ask gave up a blob and `budget` has not passed since the first one (a remote names the first blob it
/// refuses and stops there, so with several of them the others come last); if it names none, they are all noted. The
/// claim gives back the blobs that are not noted when the task ends.
async fn fetch_under_lock(
    repo: Repo,
    auth: GitAuth,
    mut claim: Claim,
    budget: Duration,
) -> Vec<String> {
    let guard = match repo.lock().await {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!(
                "gh-tideline: locking the repo to fetch the blobs for the snippets of references failed: {e}"
            );
            return Vec::new();
        }
    };
    let began = Instant::now();
    loop {
        // The lock may have been held for a long time, by a build or a pull request that was loading, and the service
        // may be shutting down by now: like a chunk of a build (see `Build::fetch_chunk`), the task does not start git
        // then. It looks again before each ask that follows a refusal, of which there may be many.
        if claim.fetches.is_shut_down() {
            return Vec::new();
        }
        let failure = match repo
            .fetch_blobs(&guard, &auth, &claim.oids, STALL_INDEX)
            .await
        {
            Ok(()) => return claim.finish(),
            Err(failure) => failure,
        };
        eprintln!(
            "gh-tideline: fetching the blobs for the snippets of references failed: {failure}"
        );
        if claim.fail(&refused_oids(&failure.stderr), Instant::now()) == 0 {
            // A remote that does not say which blobs it refuses gives no blob that is known to be fine.
            let all = claim.oids.clone();
            claim.fail(&all, Instant::now());
        }
        if claim.oids.is_empty() || began.elapsed() >= budget {
            return Vec::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::error::ErrorCode;
    use crate::git::Repos;
    use crate::git::testing::{Gate, git_in};
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
    async fn a_file_that_takes_too_long_to_parse_is_answered_as_too_large_and_not_kept_in_the_index()
     {
        // A definition and then a long run of `+` after an `=`: matching it takes tree-sitter far longer than a file is
        // given (see the tests of `tags`).
        let slow = format!("function f() {{}}\nx = {}", "+".repeat(300_000));
        assert!((slow.len() as u64) < MAX_BLOB_BYTES);
        let f = Fixture::new("nav-slow", &[slow.as_bytes(), b"function g() {}\n"]).await;
        let started = Instant::now();
        let got = f.symbols(&f.oids[0], "slow.js").await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(30));
        // Not a failure, and not a file without symbols: one that was not parsed.
        let not_parsed = FileSymbols {
            language: Some("javascript"),
            too_large: true,
            symbols: Vec::new(),
        };
        assert_eq!(got, not_parsed);
        // How long it took says something about this run, not about the file: nothing is kept in the index, so that
        // a build of the commit tries again.
        assert!(
            f.db.blob_symbols(&f.oids[0], Grammar::JavaScript)
                .unwrap()
                .is_none()
        );
        // The service remembers it for a while, so that the next request does not spend the time again.
        let slow_for = |grammar| {
            f.service
                .parse_was_slow(&f.oids[0], grammar, Instant::now())
        };
        assert!(slow_for(Grammar::JavaScript));
        assert_eq!(f.symbols(&f.oids[0], "slow.js").await.unwrap(), not_parsed);
        // The thread that parsed it is fine afterwards.
        let fine = f.symbols(&f.oids[1], "fine.js").await.unwrap();
        assert_eq!(fine.symbols, [symbol("g", "function", (1, 10, 11), (1, 1))]);
    }

    #[tokio::test]
    async fn a_file_that_timed_out_lately_is_not_parsed_until_the_note_of_it_expires() {
        let f = Fixture::new("nav-slow-noted", &[b"function g() {}\n"]).await;
        let oid = &f.oids[0];
        // The note is all the lookup goes by: this file parses in no time, and is not parsed.
        f.service
            .note_slow_parse(oid, Grammar::JavaScript, Instant::now());
        let got = f.symbols(oid, "g.js").await.unwrap();
        assert_eq!(
            got,
            FileSymbols {
                language: Some("javascript"),
                too_large: true,
                symbols: Vec::new(),
            }
        );
        let held = |grammar| f.db.blob_symbols(oid, grammar).unwrap();
        assert!(held(Grammar::JavaScript).is_none());
        // The same bytes under another grammar are another parse.
        let got = f.symbols(oid, "g.ts").await.unwrap();
        assert_eq!(got.symbols, [symbol("g", "function", (1, 10, 11), (1, 1))]);
        assert!(held(Grammar::TypeScript).is_some());
    }

    #[test]
    fn the_notes_of_slow_parses_expire_and_are_capped() {
        let service = PrService::new(Repos::new(std::env::temp_dir()), None, None);
        let oid = |n: usize| format!("{n:040x}");
        let was_slow = |n, grammar, at| service.parse_was_slow(&oid(n), grammar, at);
        let now = Instant::now();
        service.note_slow_parse(&oid(1), Grammar::C, now);
        assert!(was_slow(1, Grammar::C, now));
        // The same bytes under another grammar are another parse, and another blob is another file.
        assert!(!was_slow(1, Grammar::Cpp, now));
        assert!(!was_slow(2, Grammar::C, now));
        // The note holds for `SLOW_PARSE_TTL`.
        let almost = now + SLOW_PARSE_TTL - Duration::from_secs(1);
        assert!(was_slow(1, Grammar::C, almost));
        let expired = now + SLOW_PARSE_TTL;
        assert!(!was_slow(1, Grammar::C, expired));

        // Noting another file drops the notes that have expired.
        service.note_slow_parse(&oid(2), Grammar::C, expired);
        assert_eq!(service.slow_parses.lock().unwrap().len(), 1);

        // At most `SLOW_PARSES_KEPT` are kept, and the oldest go first.
        let at = |n: usize| expired + Duration::from_millis(n as u64);
        for n in 0..SLOW_PARSES_KEPT + 10 {
            service.note_slow_parse(&oid(100 + n), Grammar::C, at(n));
        }
        assert_eq!(service.slow_parses.lock().unwrap().len(), SLOW_PARSES_KEPT);
        let last = SLOW_PARSES_KEPT + 9;
        assert!(!was_slow(2, Grammar::C, at(last)));
        assert!(!was_slow(100, Grammar::C, at(last)));
        assert!(was_slow(110, Grammar::C, at(last)));
        assert!(was_slow(100 + last, Grammar::C, at(last)));

        // Noting a file again only renews its note: nothing else goes.
        service.note_slow_parse(&oid(110), Grammar::C, at(last + 1));
        assert_eq!(service.slow_parses.lock().unwrap().len(), SLOW_PARSES_KEPT);
        assert!(was_slow(111, Grammar::C, at(last + 1)));
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

        // The single definition of a name with many references is not cut.
        let got = p.definitions(Side::Head, "gen.rs", "over").await.unwrap();
        assert_eq!(places(&got), [("gen.rs", 3, 4, "file")]);
        assert!(!got.truncated);
    }

    #[tokio::test]
    async fn definitions_stop_at_two_hundred_and_say_so() {
        let generated = format!(
            "{}{}",
            "fn exact() {}\n".repeat(200),
            "fn over() {}\n".repeat(201)
        );
        let p = Pull::with_files(
            "nav-truncated-definitions",
            &[("old.rs", "fn old() {}\n")],
            &[("gen.rs", generated.as_str())],
        )
        .await;
        assert_eq!(p.build(Side::Head).await.state, IndexState::Ready);

        // Exactly 200 is not more than that.
        let got = p.definitions(Side::Head, "gen.rs", "exact").await.unwrap();
        assert!(!got.truncated);
        assert_eq!(got.hits.len(), 200);
        assert_eq!((got.hits[0].line, got.hits[199].line), (1, 200));

        let got = p.definitions(Side::Head, "gen.rs", "over").await.unwrap();
        assert!(got.truncated);
        assert_eq!(got.hits.len(), 200);
        assert_eq!(
            (got.hits[0].line, got.hits[199].line),
            (201, 400),
            "the 200 nearest are kept, the last one is cut"
        );

        // A name nobody defines is not cut either.
        let got = p.definitions(Side::Head, "gen.rs", "nobody").await.unwrap();
        assert!(!got.truncated);
        assert!(got.hits.is_empty());
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
    async fn a_lookup_fetches_the_blobs_of_its_references_that_the_repo_lacks_and_no_others() {
        let sources = [
            TARGET_SOURCES[0],
            TARGET_SOURCES[1],
            ("c.rs", "fn third() {\n    target();\n}\n"),
            ("d.rs", "fn unrelated() {}\n"),
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
            HashSet::from([files[0].1.clone(), files[2].1.clone(), files[3].1.clone()])
        );

        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(
            got.hits,
            [
                hit("target", "a.rs", (4, 5), "call", ("target();", 1), "file"),
                hit("target", "b.rs", (2, 5), "call", ("target();", 1), "dir"),
                hit("target", "c.rs", (2, 5), "call", ("target();", 1), "dir"),
            ]
        );
        // The blobs of a.rs and c.rs were fetched; that of d.rs has no reference to `target`, and stays where it is.
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert_eq!(after, HashSet::from([files[3].1.clone()]));
    }

    #[tokio::test]
    async fn a_lookup_in_a_new_clone_gets_the_lines_of_a_commit_another_clone_indexed() {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files("nav-no-blobs", &[("old.rs", "fn old() {}\n")], &sources).await;
        let head = p.clone.head.clone();
        // The index of the commit is complete, but the cache repo never fetched its blobs: a build in another clone of
        // the repository stored them, and no build here has anything to parse.
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| before.contains(oid)));

        // The definitions come from the index alone, with their lines, and fetch nothing.
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
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);

        // The references have to be cut out of the blobs, which the lookup fetches.
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(
            got.hits,
            [
                hit("target", "a.rs", (4, 5), "call", ("target();", 1), "file"),
                hit("target", "b.rs", (2, 5), "call", ("target();", 1), "dir"),
            ]
        );
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| !after.contains(oid)));
    }

    /// The references to `target` in the files of `TARGET_SOURCES`, with the line of each or, if `lines` is false,
    /// without.
    fn target_hits(lines: bool) -> [NavHit; 2] {
        let line = if lines { ("target();", 1) } else { ("", 0) };
        [
            hit("target", "a.rs", (4, 5), "call", line, "file"),
            hit("target", "b.rs", (2, 5), "call", line, "dir"),
        ]
    }

    /// How many blobs are being fetched, and how many have a note of a fetch that failed.
    fn in_flight_and_failed(fetches: &SnippetFetches) -> (usize, usize) {
        let blobs = fetches.lock();
        let in_flight = blobs
            .values()
            .filter(|fetch| matches!(fetch, Fetch::InFlight(_)))
            .count();
        (in_flight, blobs.len() - in_flight)
    }

    /// How many tasks lookups have spawned to fetch blobs.
    fn spawned(fetches: &SnippetFetches) -> usize {
        fetches.spawned.load(Ordering::SeqCst)
    }

    /// How many fetches under way lookups have begun to wait for.
    fn awaited(fetches: &SnippetFetches) -> usize {
        fetches.awaited.load(Ordering::SeqCst)
    }

    /// Waits until `reached` holds. A test that never gets there fails, it does not hang.
    async fn until(what: &str, reached: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(60), async {
            while !reached() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what} did not happen"));
    }

    /// Removes the blob `oid`, a loose object, from the source repo of `p`, which then refuses a fetch that asks for it.
    /// Returns where the object was and its bytes, to put it back.
    fn lose_blob(p: &Pull, oid: &str) -> (PathBuf, Vec<u8>) {
        let loose = (p.clone.origin.join(".git/objects"))
            .join(&oid[..2])
            .join(&oid[2..]);
        let lost = std::fs::read(&loose).unwrap();
        std::fs::remove_file(&loose).unwrap();
        (loose, lost)
    }

    #[tokio::test]
    async fn a_lookup_that_cannot_fetch_the_blobs_answers_without_lines_and_does_not_try_again_for_a_while()
     {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files("nav-fetch-fails", &[("old.rs", "fn old() {}\n")], &sources).await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| before.contains(oid)));

        // The remote cannot be reached.
        let url = |path: &std::path::Path| format!("file://{}", path.display());
        let gone = url(&p.clone.dir.join("gone"));
        let git_dir = &p.clone.repo.git_dir;
        git_in(git_dir, &["config", "remote.origin.url", &gone]).await;
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        // Not an error: the references are where the index says, without their lines. A remote that does not name the
        // blobs it refuses (this one is not there at all) leaves none of them known to be fine.
        assert_eq!(got.hits, target_hits(false));
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);
        assert_eq!(in_flight_and_failed(&p.service.snippet_fetches), (0, 2));

        // The failure is remembered: with the remote back, the next lookup does not ask for the blobs again, so it does
        // not hold the repo for a fetch that most likely fails again.
        git_in(
            git_dir,
            &["config", "remote.origin.url", &url(&p.clone.origin)],
        )
        .await;
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(false));
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);

        // When the notes have expired, the next lookup tries again, and has the lines.
        p.service.snippet_fetches.lock().clear();
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(true));
        assert_eq!(in_flight_and_failed(&p.service.snippet_fetches), (0, 0));
    }

    #[test]
    fn the_notes_of_failed_fetches_expire_and_are_capped() {
        let fetches = SnippetFetches::default();
        let (repo, other) = (Path::new("/cache/a.git"), Path::new("/cache/b.git"));
        let oid = |n: usize| format!("{n:040x}");
        // Whether a task can claim the blob at `at`, which it cannot while its fetch has failed lately. The claim is
        // dropped at once, so that a blob that was free stays so.
        let free = |repo: &Path, n: usize, at: Instant| {
            !fetches.claim(repo, vec![oid(n)], at).oids.is_empty()
        };
        let len = || fetches.lock().len();
        let now = Instant::now();
        fetches.note(repo, &[oid(1), oid(2)], now);
        assert!(!free(repo, 1, now) && !free(repo, 2, now));
        // Another blob has not failed, and neither has the same blob of another repo.
        assert!(free(repo, 3, now));
        assert!(free(other, 1, now));
        assert_eq!(
            fetches.claim(repo, vec![oid(3), oid(1), oid(4)], now).oids,
            [oid(3), oid(4)]
        );
        assert_eq!(len(), 2);
        // The note holds for `FETCH_FAILURE_TTL`.
        let almost = now + FETCH_FAILURE_TTL - Duration::from_secs(1);
        assert!(!free(repo, 1, almost));
        let expired = now + FETCH_FAILURE_TTL;
        assert!(free(repo, 1, expired));

        // Noting another blob drops the notes that have expired.
        fetches.note(repo, &[oid(3)], expired);
        assert_eq!(len(), 1);

        // At most `FETCH_FAILURES_KEPT` are kept, and the oldest go first, whichever repo they are of.
        let at = |n: usize| expired + Duration::from_millis(n as u64);
        for n in 0..FETCH_FAILURES_KEPT + 10 {
            fetches.note(other, &[oid(100 + n)], at(n));
        }
        assert_eq!(len(), FETCH_FAILURES_KEPT);
        let last = FETCH_FAILURES_KEPT + 9;
        assert!(free(repo, 3, at(last)));
        assert!(free(other, 100, at(last)));
        assert!(!free(other, 110, at(last)));
        assert!(!free(other, 100 + last, at(last)));

        // Noting a blob again only renews its note: nothing else goes. And the blobs of one fetch are capped together.
        fetches.note(other, &[oid(110)], at(last + 1));
        assert_eq!(len(), FETCH_FAILURES_KEPT);
        assert!(!free(other, 111, at(last + 1)));
        let batch: Vec<String> = (0..FETCH_FAILURES_KEPT + 10)
            .map(|n| oid(10_000 + n))
            .collect();
        fetches.note(other, &batch, at(last + 2));
        assert_eq!(len(), FETCH_FAILURES_KEPT);
    }

    #[test]
    fn a_blob_is_claimed_by_one_task_at_a_time_and_given_back_when_the_claim_ends() {
        let fetches = SnippetFetches::default();
        let (repo, other) = (Path::new("/cache/a.git"), Path::new("/cache/b.git"));
        let oid = |n: usize| format!("{n:040x}");
        let now = Instant::now();

        let mut first = fetches.claim(repo, vec![oid(1), oid(2)], now);
        assert_eq!(first.oids, [oid(1), oid(2)]);
        // What a task fetches is left to it: another claim gets the blobs that are not in flight, once each. The same
        // blob of another repo is another blob.
        let second = fetches.claim(repo, vec![oid(2), oid(3), oid(3)], now);
        assert_eq!(second.oids, [oid(3)]);
        let third = fetches.claim(other, vec![oid(1)], now);
        assert_eq!(third.oids, [oid(1)]);
        assert_eq!(in_flight_and_failed(&fetches), (4, 0));
        assert!(fetches.claim(repo, vec![oid(1)], now).oids.is_empty());

        // A claim gives its blobs back when it is dropped, whatever became of them.
        drop(second);
        assert_eq!(in_flight_and_failed(&fetches), (3, 0));
        assert_eq!(fetches.claim(repo, vec![oid(3)], now).oids, [oid(3)]);

        // A blob the task gave up because its fetch failed is a note, and stays one when the claim ends.
        assert_eq!(first.fail(&[oid(1)], now), 1);
        assert_eq!(first.oids, [oid(2)]);
        assert_eq!(in_flight_and_failed(&fetches), (2, 1));
        drop(first);
        assert_eq!(in_flight_and_failed(&fetches), (1, 1));
        assert!(fetches.claim(repo, vec![oid(1)], now).oids.is_empty());

        // The blobs in flight are not notes: they neither expire nor count against the cap, and are not forgotten when
        // notes are.
        let later = now + FETCH_FAILURE_TTL;
        let batch: Vec<String> = (0..FETCH_FAILURES_KEPT + 10)
            .map(|n| oid(10_000 + n))
            .collect();
        fetches.note(repo, &batch, later);
        assert_eq!(
            in_flight_and_failed(&fetches),
            (1, FETCH_FAILURES_KEPT),
            "the note of blob 1 has expired, and the blob in flight stays"
        );
        assert!(fetches.claim(other, vec![oid(1)], later).oids.is_empty());
        drop(third);
        assert_eq!(in_flight_and_failed(&fetches), (0, FETCH_FAILURES_KEPT));
    }

    #[test]
    fn a_task_that_panics_gives_back_its_blobs_even_when_it_poisoned_the_lock() {
        let fetches = SnippetFetches::default();
        let repo = Path::new("/cache/a.git");
        let oid = |n: usize| format!("{n:040x}");
        let now = Instant::now();

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _claim = fetches.claim(repo, vec![oid(1), oid(2)], now);
            assert_eq!(in_flight_and_failed(&fetches), (2, 0));
            // The panic comes while the lock is held, which poisons it; the claim is dropped while it unwinds, and
            // must not panic in turn.
            let _blobs = fetches.lock();
            panic!("the task panics");
        }));
        assert!(panicked.is_err());
        assert_eq!(in_flight_and_failed(&fetches), (0, 0));
        assert_eq!(
            fetches.claim(repo, vec![oid(1), oid(2)], now).oids,
            [oid(1), oid(2)]
        );
    }

    #[test]
    fn a_claim_leaves_the_flights_of_the_tasks_that_have_the_other_blobs_to_wait_for() {
        let fetches = SnippetFetches::default();
        let (repo, other) = (Path::new("/cache/a.git"), Path::new("/cache/b.git"));
        let oid = |n: usize| format!("{n:040x}");
        let now = Instant::now();

        // A blob that is repeated is claimed once, and a claim does not wait for itself.
        let first = fetches.claim(repo, vec![oid(1), oid(2), oid(1)], now);
        assert_eq!(first.oids, [oid(1), oid(2)]);
        assert!(first.under_way.is_empty());
        // The task that has two of the blobs is waited for once.
        let second = fetches.claim(repo, vec![oid(3), oid(1), oid(2), oid(3), oid(1)], now);
        assert_eq!(second.oids, [oid(3)]);
        assert_eq!(second.under_way.len(), 1);
        assert!(Arc::ptr_eq(&second.under_way[0], &first.flight));
        // So are the tasks that have the others, once each, in the order the blobs come.
        let third = fetches.claim(repo, vec![oid(3), oid(2), oid(4), oid(3), oid(1)], now);
        assert_eq!(third.oids, [oid(4)]);
        assert_eq!(third.under_way.len(), 2);
        assert!(Arc::ptr_eq(&third.under_way[0], &second.flight));
        assert!(Arc::ptr_eq(&third.under_way[1], &first.flight));

        // No task fetches a blob whose fetch failed, so there is nothing to wait for; and the same blob of another repo is
        // another blob.
        fetches.note(repo, &[oid(5)], now);
        let failed = fetches.claim(repo, vec![oid(5)], now);
        assert!(failed.oids.is_empty() && failed.under_way.is_empty());
        let elsewhere = fetches.claim(other, vec![oid(1)], now);
        assert_eq!(elsewhere.oids, [oid(1)]);
        assert!(elsewhere.under_way.is_empty());
    }

    /// What the flight gives, if it ends within `wait`.
    async fn fetched_within(
        flight: Arc<Flight>,
        wait: Duration,
    ) -> Result<Vec<String>, tokio::time::error::Elapsed> {
        tokio::time::timeout(wait, flight.fetched()).await
    }

    #[tokio::test]
    async fn a_flight_ends_with_the_blobs_its_task_fetched_or_with_none() {
        let fetches = SnippetFetches::default();
        let repo = Path::new("/cache/a.git");
        let oid = |n: usize| format!("{n:040x}");
        let now = Instant::now();
        let (patience, moment) = (Duration::from_secs(60), Duration::from_millis(50));

        // The lookup that wants blob 2 gets the flight of the task that has blobs 1 and 2.
        let task = fetches.claim(repo, vec![oid(1), oid(2)], now);
        let flight = fetches.claim(repo, vec![oid(2)], now).under_way.remove(0);
        // The task has not ended, and the lookup waits.
        assert!(fetched_within(flight.clone(), moment).await.is_err());
        let waiting = tokio::spawn(fetched_within(flight.clone(), patience));
        tokio::task::yield_now().await;
        // The lookup is woken when the task tells which blobs it fetched, and gets all of them, not only blob 2.
        assert_eq!(task.finish(), [oid(1), oid(2)]);
        assert_eq!(
            waiting.await.unwrap().expect("the flight ended"),
            [oid(1), oid(2)]
        );
        // The task is told for good: a lookup that finds the flight when the claim has ended gets the blobs at once.
        drop(task);
        assert_eq!(
            fetched_within(flight, patience)
                .await
                .expect("the flight ended"),
            [oid(1), oid(2)]
        );

        // A task that fetched nothing ends its claim without a word, which lets go of the lookups that wait for it.
        let task = fetches.claim(repo, vec![oid(3)], now);
        let flight = fetches.claim(repo, vec![oid(3)], now).under_way.remove(0);
        let waiting = tokio::spawn(fetched_within(flight.clone(), patience));
        tokio::task::yield_now().await;
        drop(task);
        assert!(waiting.await.unwrap().expect("the flight ended").is_empty());
        assert!(
            fetched_within(flight, patience)
                .await
                .expect("the flight ended")
                .is_empty()
        );
    }

    #[test]
    fn a_claim_that_gives_up_blobs_notes_only_those_it_holds() {
        let fetches = SnippetFetches::default();
        let repo = Path::new("/cache/a.git");
        let oid = |n: usize| format!("{n:040x}");
        let now = Instant::now();

        let mut claim = fetches.claim(repo, vec![oid(1), oid(2), oid(3)], now);
        // Blob 4 is none of the claim's, and blob 2 is named twice: one blob is given up, and it is the one noted.
        assert_eq!(claim.fail(&[oid(2), oid(4), oid(2)], now), 1);
        assert_eq!(claim.oids, [oid(1), oid(3)]);
        assert_eq!(in_flight_and_failed(&fetches), (2, 1));
        assert_eq!(fetches.claim(repo, vec![oid(4)], now).oids, [oid(4)]);
        // A blob that is given up is not the claim's any more.
        assert_eq!(claim.fail(&[oid(2), oid(4)], now), 0);
        assert_eq!(in_flight_and_failed(&fetches), (2, 1));
    }

    #[test]
    fn a_claim_that_ends_leaves_the_entries_that_are_no_longer_its_own() {
        let fetches = SnippetFetches::default();
        let repo = Path::new("/cache/a.git");
        let oid = |n: usize| format!("{n:040x}");
        let now = Instant::now();

        // The note of a failed fetch stays when the claim that held the blob ends.
        let claim = fetches.claim(repo, vec![oid(1), oid(2)], now);
        fetches.note(repo, &[oid(1)], now);
        drop(claim);
        assert_eq!(in_flight_and_failed(&fetches), (0, 1));

        // So does the blob that another task has in flight, when the note has expired and that task claimed it.
        let later = now + FETCH_FAILURE_TTL;
        let old = fetches.claim(repo, vec![oid(3)], now);
        fetches.note(repo, &[oid(3)], now);
        let new = fetches.claim(repo, vec![oid(3)], later);
        assert_eq!(new.oids, [oid(3)]);
        drop(old);
        assert_eq!(in_flight_and_failed(&fetches), (1, 1));
        let waiting = fetches.claim(repo, vec![oid(3)], later);
        assert!(waiting.oids.is_empty());
        assert!(Arc::ptr_eq(&waiting.under_way[0], &new.flight));
        drop(new);
        assert_eq!(in_flight_and_failed(&fetches), (0, 1));
    }

    #[tokio::test]
    async fn a_lookup_does_not_wait_longer_than_a_moment_for_the_blobs_it_fetches() {
        let sources = TARGET_SOURCES;
        let mut p =
            Pull::with_files("nav-fetch-wait", &[("old.rs", "fn old() {}\n")], &sources).await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| before.contains(oid)));
        Arc::get_mut(&mut p.service).unwrap().snippet_fetch_wait = Duration::from_millis(100);

        // A pull request that is loading holds the repo, and the fetch of the lookup waits for it.
        let repo = p.service.repos.repo("github.com", "o", "r");
        let guard = repo.lock().await.unwrap();
        let lookup = p.references(Side::Head, "a.rs", "target");
        let got = tokio::time::timeout(Duration::from_secs(60), lookup)
            .await
            .expect("the lookup waited for the repo")
            .unwrap();
        assert_eq!(got.hits, target_hits(false));
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);
        // A fetch that has not come yet has not failed: the blobs are in flight, and noted as nothing else.
        assert_eq!(in_flight_and_failed(&p.service.snippet_fetches), (2, 0));

        // The task of the lookup goes on: when the repo is free, it gets the blobs for a later lookup.
        drop(guard);
        drop(repo.lock().await.unwrap());
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| !after.contains(oid)));
        assert_eq!(in_flight_and_failed(&p.service.snippet_fetches), (0, 0));
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(true));
    }

    #[tokio::test]
    async fn lookups_for_blobs_that_are_in_flight_start_no_second_fetch_for_the_repo_lock() {
        let sources = TARGET_SOURCES;
        let mut p =
            Pull::with_files("nav-fetch-merged", &[("old.rs", "fn old() {}\n")], &sources).await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        Arc::get_mut(&mut p.service).unwrap().snippet_fetch_wait = Duration::from_millis(100);
        let fetches = &p.service.snippet_fetches;

        // A pull request that is loading holds the repo, and the fetch of the first lookup waits for it. The lookup
        // stops waiting for it: the task that fetches is alive and queued for the lock.
        let repo = p.service.repos.repo("github.com", "o", "r");
        let guard = repo.lock().await.unwrap();
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(false));
        assert_eq!(spawned(fetches), 1);
        assert_eq!(awaited(fetches), 0);
        assert_eq!(in_flight_and_failed(fetches), (2, 0));

        // The lookups that come meanwhile find the blobs in flight, and wait for that fetch for as long as they can,
        // which is not until the repo is free. They queue no fetch of their own, which would wait for the same lock ahead
        // of the loads that come after.
        let (second, third) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(
                p.references(Side::Head, "a.rs", "target"),
                p.references(Side::Head, "a.rs", "target"),
            )
        })
        .await
        .expect("the lookups waited for the repo");
        assert_eq!(second.unwrap().hits, target_hits(false));
        assert_eq!(third.unwrap().hits, target_hits(false));
        assert_eq!(spawned(fetches), 1);
        assert_eq!(awaited(fetches), 2);
        assert_eq!(in_flight_and_failed(fetches), (2, 0));

        // The one fetch gets both blobs when the repo is free, and the blobs are not in flight any more.
        drop(guard);
        drop(repo.lock().await.unwrap());
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| !after.contains(oid)));
        assert_eq!(spawned(fetches), 1);
        assert_eq!(in_flight_and_failed(fetches), (0, 0));
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(true));
        assert_eq!(spawned(fetches), 1);
    }

    #[tokio::test]
    async fn a_lookup_waits_for_the_fetch_of_a_lookup_that_the_client_gave_up_on() {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files(
            "nav-fetch-awaited",
            &[("old.rs", "fn old() {}\n")],
            &sources,
        )
        .await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let fetches = &p.service.snippet_fetches;

        // A pull request that is loading holds the repo, and the fetch of the first lookup waits for it. The client
        // asks again, which makes it give up on the first request: the lookup is dropped, and its task goes on.
        let repo = p.service.repos.repo("github.com", "o", "r");
        let guard = repo.lock().await.unwrap();
        {
            let mut first = Box::pin(p.references(Side::Head, "a.rs", "target"));
            tokio::select! {
                _ = &mut first => panic!("the first lookup ended before its fetch was queued"),
                _ = until("the fetch of the first lookup", || spawned(fetches) == 1) => {}
            }
        }
        assert_eq!(in_flight_and_failed(fetches), (2, 0));

        // The second lookup finds the blobs in flight, and waits for that fetch: it queues none of its own, and has the
        // lines when the repo is free and the task has fetched the blobs.
        let second = p.references(Side::Head, "a.rs", "target");
        let release = async {
            until("the second lookup waiting for the fetch", || {
                awaited(fetches) == 1
            })
            .await;
            drop(guard);
        };
        let (got, ()) = tokio::join!(second, release);
        assert_eq!(got.unwrap().hits, target_hits(true));
        assert_eq!(spawned(fetches), 1);
        drop(repo.lock().await.unwrap());
        assert_eq!(in_flight_and_failed(fetches), (0, 0));
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| !after.contains(oid)));
    }

    #[tokio::test]
    async fn a_lookup_takes_the_blobs_of_its_own_task_and_of_the_task_it_waits_for() {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files("nav-fetch-union", &[("old.rs", "fn old() {}\n")], &sources).await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let fetches = &p.service.snippet_fetches;

        // A pull request that is loading holds the repo. The first lookup finds the blob of a.rs in flight with a claim
        // that ends later, and its task fetches the blob of b.rs. The client gives up on the lookup, and the task goes on.
        let repo = p.service.repos.repo("github.com", "o", "r");
        let guard = repo.lock().await.unwrap();
        let other = fetches.claim(
            &p.clone.repo.git_dir,
            vec![files[0].1.clone()],
            Instant::now(),
        );
        {
            let mut first = Box::pin(p.references(Side::Head, "a.rs", "target"));
            tokio::select! {
                _ = &mut first => panic!("the first lookup ended before its fetch was queued"),
                _ = until("the fetch of the first lookup", || spawned(fetches) == 1) => {}
            }
        }
        drop(other);
        assert_eq!(in_flight_and_failed(fetches), (1, 0));

        // The second lookup fetches the blob of a.rs in a task of its own, and waits for the task of the first one, which
        // has the blob of b.rs: it has the lines of both when the repo is free.
        let second = p.references(Side::Head, "a.rs", "target");
        let release = async {
            until("the second lookup waiting for the fetch", || {
                awaited(fetches) == 2
            })
            .await;
            drop(guard);
        };
        let (got, ()) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(second, release)
        })
        .await
        .expect("the second lookup got the blobs");
        assert_eq!(got.unwrap().hits, target_hits(true));
        assert_eq!(spawned(fetches), 2);
        drop(repo.lock().await.unwrap());
        assert_eq!(in_flight_and_failed(fetches), (0, 0));
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| !after.contains(oid)));
    }

    #[tokio::test]
    async fn a_lookup_waits_for_all_the_fetches_it_needs_no_longer_than_the_wait() {
        let sources = TARGET_SOURCES;
        let mut p = Pull::with_files(
            "nav-fetch-one-deadline",
            &[("old.rs", "fn old() {}\n")],
            &sources,
        )
        .await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let wait = Duration::from_millis(800);
        Arc::get_mut(&mut p.service).unwrap().snippet_fetch_wait = wait;
        let fetches = &p.service.snippet_fetches;

        // The blob of b.rs is in flight with a task that does not end, and the repo is held, so the task of the lookup
        // does not get the blob of a.rs either.
        let other = fetches.claim(
            &p.clone.repo.git_dir,
            vec![files[1].1.clone()],
            Instant::now(),
        );
        let repo = p.service.repos.repo("github.com", "o", "r");
        let guard = repo.lock().await.unwrap();
        let began = Instant::now();
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        let took = began.elapsed();
        assert_eq!(got.hits, target_hits(false));
        assert_eq!((spawned(fetches), awaited(fetches)), (1, 1));
        // It waited for the one, and then the other had no wait left: a wait for each would take twice as long.
        assert!(took >= wait, "the lookup waited {took:?} only");
        assert!(took < 2 * wait, "the lookup waited {took:?}");

        drop(other);
        drop(guard);
        drop(repo.lock().await.unwrap());
        assert_eq!(in_flight_and_failed(fetches), (0, 0));
    }

    #[tokio::test]
    async fn a_lookup_that_waits_for_a_fetch_that_fails_answers_without_lines_at_once() {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files(
            "nav-fetch-awaited-fails",
            &[("old.rs", "fn old() {}\n")],
            &sources,
        )
        .await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        let fetches = &p.service.snippet_fetches;
        let gone = format!("file://{}", p.clone.dir.join("gone").display());
        git_in(
            &p.clone.repo.git_dir,
            &["config", "remote.origin.url", &gone],
        )
        .await;

        let repo = p.service.repos.repo("github.com", "o", "r");
        let guard = repo.lock().await.unwrap();
        {
            let mut first = Box::pin(p.references(Side::Head, "a.rs", "target"));
            tokio::select! {
                _ = &mut first => panic!("the first lookup ended before its fetch was queued"),
                _ = until("the fetch of the first lookup", || spawned(fetches) == 1) => {}
            }
        }

        // The remote cannot be reached, and the second lookup does not wait for the 60 s it could: the task ends, and the
        // lookup is let go.
        let second = p.references(Side::Head, "a.rs", "target");
        let release = async {
            until("the second lookup waiting for the fetch", || {
                awaited(fetches) == 1
            })
            .await;
            drop(guard);
        };
        let (got, ()) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(second, release)
        })
        .await
        .expect("the lookup was let go when the fetch failed");
        assert_eq!(got.unwrap().hits, target_hits(false));
        assert_eq!(spawned(fetches), 1);
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);
        assert_eq!(in_flight_and_failed(fetches), (0, 2));
    }

    #[tokio::test]
    async fn a_blob_the_remote_refuses_is_noted_alone_and_the_others_are_fetched() {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files(
            "nav-fetch-refused",
            &[("old.rs", "fn old() {}\n")],
            &sources,
        )
        .await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let (a_rs, b_rs) = (files[0].1.clone(), files[1].1.clone());
        let fetches = &p.service.snippet_fetches;
        let git_dir = &p.clone.repo.git_dir;

        // The source repo lost the blob of a.rs, a loose object, and says so when a fetch asks for it.
        let (loose, lost) = lose_blob(&p, &a_rs);

        // Only b.rs has its line: the fetch for both blobs fails, and the lookup does not give up on the other blob,
        // which it asks for again.
        let [from_a, mut from_b] = target_hits(false);
        from_b.snippet = "target();".to_string();
        from_b.snippet_column = 1;
        let without_a = [from_a, from_b];
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, without_a);
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert_eq!(after, HashSet::from([a_rs.clone()]));
        // The blob that was refused is the only one noted.
        assert_eq!(in_flight_and_failed(fetches), (0, 1));
        let claim = fetches.claim(git_dir, vec![a_rs.clone(), b_rs.clone()], Instant::now());
        assert_eq!(claim.oids, [b_rs]);
        drop(claim);

        // The note holds: with the blob back, the next lookup does not ask for it.
        std::fs::write(&loose, lost).unwrap();
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, without_a);
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), after);

        // When the note has expired, it does.
        fetches.lock().clear();
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(true));
        assert_eq!(in_flight_and_failed(fetches), (0, 0));
    }

    /// Files whose head has `target` defined in the first and used in all of them. Their blobs differ.
    const MANY_SOURCES: [(&str, &str); 5] = [
        TARGET_SOURCES[0],
        TARGET_SOURCES[1],
        ("c.rs", "fn third() {\n    target();\n}\n"),
        ("d.rs", "fn fourth() {\n    target();\n}\n"),
        ("e.rs", "fn fifth() {\n    target();\n}\n"),
    ];

    /// The references to `target` in the files of `MANY_SOURCES`, with the line of each in the files `with_lines` and
    /// without in the others.
    fn many_hits(with_lines: &[&str]) -> Vec<NavHit> {
        MANY_SOURCES
            .iter()
            .map(|&(path, _)| {
                let (at, tier) = if path == "a.rs" {
                    ((4, 5), "file")
                } else {
                    ((2, 5), "dir")
                };
                let line = if with_lines.contains(&path) {
                    ("target();", 1)
                } else {
                    ("", 0)
                };
                hit("target", path, at, "call", line, tier)
            })
            .collect()
    }

    /// A pull request over `MANY_SOURCES` whose index holds the files, as a build in another clone left it, and whose
    /// source repo has lost the blobs of a.rs, c.rs and e.rs: a fetch that asks for them is refused. Returns the OIDs
    /// of the files, and of those blobs.
    async fn pull_that_lost_three_blobs(name: &str) -> (Pull, Vec<String>, [String; 3]) {
        let p = Pull::with_files(name, &[("old.rs", "fn old() {}\n")], &MANY_SOURCES).await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &MANY_SOURCES).await;
        p.db.save_commit(&head, &files).unwrap();
        let oids: Vec<String> = files.into_iter().map(|(_, oid, _)| oid).collect();
        let lost = [0, 2, 4].map(|n| oids[n].clone());
        for oid in &lost {
            lose_blob(&p, oid);
        }
        (p, oids, lost)
    }

    #[tokio::test]
    async fn blobs_the_remote_refuses_one_after_the_other_do_not_keep_the_others_from_coming() {
        let (p, _, lost) = pull_that_lost_three_blobs("nav-fetch-many-refused").await;
        let head = p.clone.head.clone();
        let fetches = &p.service.snippet_fetches;

        // The remote names the first blob it refuses and stops there, so it takes a fourth ask, after three that gave up
        // a blob each, to get the two blobs that it has.
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, many_hits(&["b.rs", "d.rs"]));
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert_eq!(after, HashSet::from(lost));
        // The three blobs that were refused are noted, and no other.
        assert_eq!(in_flight_and_failed(fetches), (0, 3));
    }

    #[tokio::test]
    async fn a_task_that_has_no_time_to_ask_again_gives_back_the_blobs_it_did_not_ask_for() {
        let (mut p, oids, _) = pull_that_lost_three_blobs("nav-fetch-out-of-time").await;
        let head = p.clone.head.clone();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        Arc::get_mut(&mut p.service).unwrap().snippet_fetch_budget = Duration::ZERO;
        let fetches = &p.service.snippet_fetches;

        // The first ask gave up a blob, and the time for a second one is up: no blob came.
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, many_hits(&[]));
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);
        // Only the blob that the remote named is noted; the other four are free for a later lookup.
        assert_eq!(in_flight_and_failed(fetches), (0, 1));
        let claim = fetches.claim(&p.clone.repo.git_dir, oids, Instant::now());
        assert_eq!(claim.oids.len(), 4);
    }

    #[tokio::test]
    async fn a_fetch_does_not_ask_again_once_the_service_shuts_down() {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files(
            "nav-fetch-shutdown-asking",
            &[("old.rs", "fn old() {}\n")],
            &sources,
        )
        .await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        // The first ask is refused for a.rs, and a second one would get b.rs.
        lose_blob(&p, &files[0].1);
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        let gate = Gate::install(&p.clone).await;
        let fetches = &p.service.snippet_fetches;

        // The service shuts down while the first ask is under way.
        let lookup = p.references(Side::Head, "a.rs", "target");
        let shut_down = async {
            gate.begun().await;
            fetches.shut_down();
            gate.open();
        };
        let (got, ()) = tokio::join!(lookup, shut_down);
        assert_eq!(got.unwrap().hits, target_hits(false));
        // The blob that was refused is noted. The task did not ask for the other, and gave it back.
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);
        assert_eq!(in_flight_and_failed(fetches), (0, 1));
    }

    #[tokio::test]
    async fn a_lookup_that_comes_after_the_service_shut_down_spawns_no_fetch() {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files(
            "nav-fetch-after-shutdown",
            &[("old.rs", "fn old() {}\n")],
            &sources,
        )
        .await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        let fetches = &p.service.snippet_fetches;

        fetches.shut_down();
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(false));
        assert_eq!(spawned(fetches), 0);
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);
        assert_eq!(in_flight_and_failed(fetches), (0, 0));
    }

    #[tokio::test]
    async fn a_fetch_that_waited_for_the_repo_does_not_start_git_once_the_service_shuts_down() {
        let sources = TARGET_SOURCES;
        let mut p = Pull::with_files(
            "nav-fetch-shutdown",
            &[("old.rs", "fn old() {}\n")],
            &sources,
        )
        .await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| before.contains(oid)));
        Arc::get_mut(&mut p.service).unwrap().snippet_fetch_wait = Duration::from_millis(100);

        // The lookup stops waiting for the repo, which is held; its fetch of both blobs waits for it.
        let repo = p.service.repos.repo("github.com", "o", "r");
        let guard = repo.lock().await.unwrap();
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(false));
        assert_eq!(in_flight_and_failed(&p.service.snippet_fetches), (2, 0));

        // The server shuts down: it tells the fetches, and waits for the repo, which is held.
        let service = p.service.clone();
        let drain = tokio::spawn(async move { service.drain(Duration::from_secs(60)).await });
        tokio::time::timeout(Duration::from_secs(60), async {
            while !p.service.snippet_fetches.is_shut_down() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the drain told the fetches that the service shuts down");

        // The fetch gets the repo, and leaves it as it was.
        drop(guard);
        tokio::time::timeout(Duration::from_secs(60), drain)
            .await
            .expect("the service drained")
            .unwrap();
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);
        assert_eq!(in_flight_and_failed(&p.service.snippet_fetches), (0, 0));
    }

    #[tokio::test]
    async fn a_lookup_that_is_dropped_while_it_fetches_leaves_the_fetch_to_end_under_the_repo_lock()
    {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files(
            "nav-fetch-dropped",
            &[("old.rs", "fn old() {}\n")],
            &sources,
        )
        .await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let gate = Gate::install(&p.clone).await;

        // The client goes away while the fetch of the lookup is under way: its request is dropped.
        {
            let mut lookup = Box::pin(p.references(Side::Head, "a.rs", "target"));
            tokio::select! {
                _ = &mut lookup => panic!("the lookup ended before its fetch began"),
                _ = gate.begun() => {}
            }
        }
        // The task that fetches still owns the repo lock, so nothing else touches the repo while git runs, and the blobs
        // it fetches are still its own.
        let repo = p.service.repos.repo("github.com", "o", "r");
        let waited = tokio::time::timeout(Duration::from_millis(200), repo.lock()).await;
        assert!(
            waited.is_err(),
            "the repo was free while a fetch was under way"
        );
        assert_eq!(in_flight_and_failed(&p.service.snippet_fetches), (2, 0));

        // The fetch ends, and the lock is let go, and the blobs with it.
        gate.open();
        drop(repo.lock().await.unwrap());
        let after = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| !after.contains(oid)));
        assert_eq!(in_flight_and_failed(&p.service.snippet_fetches), (0, 0));
        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(true));
    }

    #[tokio::test]
    async fn a_lookup_without_a_token_answers_without_lines_and_notes_no_failure() {
        let sources = TARGET_SOURCES;
        let p = Pull::with_files("nav-no-token", &[("old.rs", "fn old() {}\n")], &sources).await;
        let head = p.clone.head.clone();
        let files = stored_by_another_clone(&p, &sources).await;
        p.db.save_commit(&head, &files).unwrap();
        let before = p.clone.repo.missing_objects(&head).await.unwrap();
        assert!(files.iter().all(|(_, oid, _)| before.contains(oid)));

        // The same cache repo, for a host the viewer has no token for: the test service only has one for github.com.
        std::os::unix::fs::symlink("github.com", p.clone.dir.join("repos/ghe.invalid")).unwrap();
        let key = PrKey {
            host: "ghe.invalid".to_string(),
            ..p.key.clone()
        };
        let p = Pull { key, ..p };
        p.at(&head).await;
        // Whatever the environment and `gh` hold for the host.
        p.service.tokens.deny("ghe.invalid");

        let got = p.references(Side::Head, "a.rs", "target").await.unwrap();
        assert_eq!(got.hits, target_hits(false));
        assert_eq!(p.clone.repo.missing_objects(&head).await.unwrap(), before);
        // Having no token is no failure of a blob, and leaves none of them claimed.
        assert_eq!(in_flight_and_failed(&p.service.snippet_fetches), (0, 0));
    }
}
