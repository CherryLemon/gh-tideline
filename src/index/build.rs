//! The build of one commit's index: list its source files, fetch the blobs the index lacks, parse them, and record
//! which blob each path holds.
//!
//! A build can be stopped at any step and started again: what it stored stays, so the next one does only what is left.
//! It looks for a cancellation between its steps and inside the parser. It never kills git, and it holds the repo lock
//! only while one chunk of blobs is fetched, so pull requests that are opened meanwhile are not kept waiting.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinError, JoinHandle};

use super::db::{BlobStatus, IndexDb, NewBlob};
use super::lang::{Family, Grammar, MAX_BLOB_BYTES, skipped_path};
use super::tags::{self, ParseError};
use super::{Job, Phase};
use crate::git::parse::GITLINK_MODE;
use crate::git::{GitAuth, GitFailure, Repo, TreeEntry};

/// Regular files. Symlinks (120000) and gitlinks (160000) have no source to read.
const FILE_MODES: [&str; 2] = ["100644", "100755"];
/// Threads that parse at once: more would take the processors from the rest of the machine for little gain.
const MAX_WORKERS: usize = 4;

/// What a build does at most, or at a time.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// A commit with more source files is not indexed.
    pub max_files: usize,
    /// How many blobs one fetch asks for, with the repo free for others in between. None: a twentieth of what is to
    /// fetch, but 500 to 2000.
    pub fetch_chunk: Option<usize>,
    /// How many bytes of blobs are read and parsed at a time (one blob at least).
    pub group_bytes: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_files: 100_000,
            fetch_chunk: None,
            group_bytes: 16 << 20,
        }
    }
}

impl Limits {
    fn chunk(&self, to_fetch: usize) -> usize {
        self.fetch_chunk
            .unwrap_or_else(|| (to_fetch / 20).clamp(500, 2000))
            .max(1)
    }
}

/// Why a build ended before the commit was completely indexed.
#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    Cancelled,
    /// With what the user is told.
    Failed(String),
}

impl From<GitFailure> for Stop {
    fn from(e: GitFailure) -> Stop {
        Stop::Failed(e.to_string())
    }
}

impl From<String> for Stop {
    fn from(message: String) -> Stop {
        Stop::Failed(message)
    }
}

fn join_failed(e: JoinError) -> Stop {
    Stop::Failed(format!("the indexing task failed: {e}"))
}

/// The path, blob and grammar of every file a commit lists.
type Listing = Vec<(String, String, Grammar)>;

/// A blob the index lacks, with the source paths that hold it.
#[derive(Debug, PartialEq, Eq)]
struct Pending {
    oid: String,
    paths: Vec<String>,
    /// Known once the blob is local.
    size: u64,
}

/// The files of the tree that the index covers: regular files of a language it knows, outside the paths it leaves out.
fn select_files(entries: Vec<TreeEntry>, max_files: usize) -> Result<Vec<TreeEntry>, Stop> {
    let files: Vec<TreeEntry> = entries
        .into_iter()
        .filter(|entry| {
            FILE_MODES.contains(&entry.mode.as_str())
                && Family::for_path(&entry.path).is_some()
                && !skipped_path(&entry.path)
        })
        .collect();
    if files.len() > max_files {
        return Err(Stop::Failed(format!(
            "the commit has {} source files; the index covers at most {max_files}",
            files.len()
        )));
    }
    Ok(files)
}

/// The grammar under which the index holds a blob for `path`, if it does: the one the extension names. A `.h` file
/// has no such grammar, since it depends on the content, which is not at hand: any grammar of the C family will do.
fn held_grammar(path: &str, stored: &[Grammar]) -> Option<Grammar> {
    match Grammar::for_path_only(path) {
        Some(grammar) => stored.contains(&grammar).then_some(grammar),
        None => stored
            .iter()
            .copied()
            .filter(|grammar| grammar.family() == Family::C)
            .min(),
    }
}

/// Splits the files by whether the index holds their blob (`stored` lists the grammars it holds each under): `listing`
/// has the files it does, with the grammar, and `pending` the blobs it lacks, each with its paths, in the order the
/// files first use them.
fn settle(files: &[TreeEntry], stored: &HashMap<String, Vec<Grammar>>) -> (Listing, Vec<Pending>) {
    let mut listing = Listing::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut at: HashMap<&str, usize> = HashMap::new();
    for file in files {
        let held = stored
            .get(&file.oid)
            .and_then(|grammars| held_grammar(&file.path, grammars));
        if let Some(grammar) = held {
            listing.push((file.path.clone(), file.oid.clone(), grammar));
            continue;
        }
        let i = *at.entry(&file.oid).or_insert_with(|| {
            pending.push(Pending {
                oid: file.oid.clone(),
                paths: Vec::new(),
                size: 0,
            });
            pending.len() - 1
        });
        pending[i].paths.push(file.path.clone());
    }
    (listing, pending)
}

/// The grammars a blob is stored under: one for each its paths name. Without the content (`&[]`) a `.h` file counts as C.
fn grammars_of(paths: &[String], content: &[u8]) -> Vec<Grammar> {
    let mut grammars: Vec<Grammar> = paths
        .iter()
        .filter_map(|path| Grammar::for_path(path, content))
        .collect();
    grammars.sort();
    grammars.dedup();
    grammars
}

/// Rows for a blob that is not parsed, so that later builds do not look at it again.
fn unparsed(blob: &Pending, status: BlobStatus) -> impl Iterator<Item = NewBlob> + '_ {
    grammars_of(&blob.paths, &[])
        .into_iter()
        .map(move |grammar| NewBlob {
            oid: blob.oid.clone(),
            grammar,
            status,
            size: blob.size,
            tags: Vec::new(),
        })
}

/// Splits the blobs by whether they are local (`missing` lists the objects that are not).
fn partition(pending: Vec<Pending>, missing: &HashSet<String>) -> (Vec<Pending>, Vec<Pending>) {
    pending
        .into_iter()
        .partition(|blob| !missing.contains(&blob.oid))
}

/// Sorts the blobs by what becomes of them: the `present` ones that are small enough are returned to be parsed, and
/// the others get a row without tags, `too_large` when over the limit and `unreadable` for those the remote did not
/// give (`lost`) or that cannot be sized.
fn classify(
    present: Vec<Pending>,
    lost: Vec<Pending>,
    sizes: &HashMap<String, u64>,
) -> (Vec<Pending>, Vec<NewBlob>) {
    let mut readable = Vec::new();
    let mut rows = Vec::new();
    for mut blob in present {
        match sizes.get(&blob.oid) {
            Some(&size) if size > MAX_BLOB_BYTES => {
                blob.size = size;
                rows.extend(unparsed(&blob, BlobStatus::TooLarge));
            }
            Some(&size) => {
                blob.size = size;
                readable.push(blob);
            }
            None => rows.extend(unparsed(&blob, BlobStatus::Unreadable)),
        }
    }
    for blob in &lost {
        rows.extend(unparsed(blob, BlobStatus::Unreadable));
    }
    (readable, rows)
}

/// Groups of blobs, in order, whose sizes add up to at most `limit` bytes. A blob over it makes a group of its own.
fn groups(blobs: Vec<Pending>, limit: u64) -> Vec<Vec<Pending>> {
    let mut groups: Vec<Vec<Pending>> = Vec::new();
    let mut bytes = 0;
    for blob in blobs {
        match groups.last_mut() {
            Some(group) if bytes + blob.size <= limit => {
                bytes += blob.size;
                group.push(blob);
            }
            _ => {
                bytes = blob.size;
                groups.push(vec![blob]);
            }
        }
    }
    groups
}

/// The rows of one blob, one for each grammar its paths name; None when `cancel` was raised.
fn parse_blob(blob: &Pending, content: &[u8], cancel: &AtomicUsize) -> Option<Vec<NewBlob>> {
    let mut rows = Vec::new();
    for grammar in grammars_of(&blob.paths, content) {
        let (status, tags) = match tags::parse(grammar, content, Some(cancel)) {
            Ok(tags) => (BlobStatus::Parsed, tags),
            Err(ParseError::Cancelled) => return None,
            Err(ParseError::Failed(message)) => {
                eprintln!(
                    "gh-tideline: parsing {} as {} failed: {message}",
                    blob.paths[0],
                    grammar.id()
                );
                (BlobStatus::Unreadable, Vec::new())
            }
        };
        rows.push(NewBlob {
            oid: blob.oid.clone(),
            grammar,
            status,
            size: blob.size,
            tags,
        });
    }
    Some(rows)
}

/// Parses the blobs on a few threads and returns their rows in the order of `sources`. Once `job` is cancelled it stops
/// early, and the result is `Err(Cancelled)`.
fn parse_blobs(job: &Job, sources: Vec<(Pending, Vec<u8>)>) -> Result<Vec<NewBlob>, Stop> {
    let next = AtomicUsize::new(0);
    let parsed: Mutex<Vec<(usize, Vec<NewBlob>)>> = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_WORKERS)
        .min(sources.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some((blob, content)) = sources.get(i) else {
                        break;
                    };
                    // The flag stops the other threads too: their parses fail at once.
                    let Some(rows) = parse_blob(blob, content, &job.cancel) else {
                        break;
                    };
                    parsed
                        .lock()
                        .expect("parsed blobs poisoned")
                        .push((i, rows));
                    job.advance(1);
                }
            });
        }
    });
    let mut parsed = parsed.into_inner().expect("parsed blobs poisoned");
    if parsed.len() < sources.len() {
        return Err(Stop::Cancelled);
    }
    parsed.sort_by_key(|(i, _)| *i);
    Ok(parsed.into_iter().flat_map(|(_, rows)| rows).collect())
}

/// Waits for the insert that was started in the background, if one was.
async fn wait_for_insert(insert: Option<JoinHandle<Result<(), String>>>) -> Result<(), Stop> {
    match insert {
        Some(handle) => Ok(handle.await.map_err(join_failed)??),
        None => Ok(()),
    }
}

/// One build: the job it reports to, and what it works with.
pub(super) struct Build {
    pub(super) job: Arc<Job>,
    pub(super) repo: Arc<Repo>,
    pub(super) auth: GitAuth,
    pub(super) db: IndexDb,
    pub(super) limits: Limits,
}

impl Drop for Build {
    /// A build whose task panicked or was aborted must not stay `building` for ever.
    fn drop(&mut self) {
        if self.job.active() {
            self.job
                .finish(Err(Stop::Failed("the indexing task stopped".to_string())));
        }
    }
}

impl Build {
    /// Waits for the turn (one commit is built at a time), builds, and records how it went. A job cancelled in the
    /// queue leaves it at once.
    pub(super) async fn run(self, turn: Arc<Semaphore>) {
        let outcome = match self.wait_for(turn).await {
            Ok(_turn) => {
                self.job.begin();
                self.build().await
            }
            Err(stop) => Err(stop),
        };
        self.job.finish(outcome);
    }

    async fn wait_for(&self, turn: Arc<Semaphore>) -> Result<OwnedSemaphorePermit, Stop> {
        tokio::select! {
            permit = turn.acquire_owned() => permit
                .map_err(|_| Stop::Failed("indexing is shutting down".to_string())),
            _ = self.job.wake.notified() => Err(Stop::Cancelled),
        }
    }

    fn alive(&self) -> Result<(), Stop> {
        if self.job.cancelled() {
            return Err(Stop::Cancelled);
        }
        Ok(())
    }

    /// Runs a synchronous database call on the blocking pool.
    async fn with_db<T: Send + 'static>(
        &self,
        call: impl FnOnce(&IndexDb) -> Result<T, String> + Send + 'static,
    ) -> Result<T, Stop> {
        let db = self.db.clone();
        Ok(tokio::task::spawn_blocking(move || call(&db))
            .await
            .map_err(join_failed)??)
    }

    async fn stored(&self, oids: Vec<String>) -> Result<HashMap<String, Vec<Grammar>>, Stop> {
        self.with_db(move |db| db.stored_blobs(&oids)).await
    }

    async fn build(&self) -> Result<(), Stop> {
        let commit = self.job.commit.as_str();
        self.alive()?;
        let entries = self.repo.ls_tree(commit).await?;
        // From now on the blob endpoint may fetch any file of the commit, as it may those of a pull request.
        self.repo.remember_oids(
            entries
                .iter()
                .map(|entry| (entry.oid.as_str(), entry.mode != GITLINK_MODE)),
        );
        let files = select_files(entries, self.limits.max_files)?;
        let mut oids: Vec<String> = files.iter().map(|file| file.oid.clone()).collect();
        oids.sort();
        oids.dedup();

        let (_, pending) = settle(&files, &self.stored(oids.clone()).await?);
        if !pending.is_empty() {
            self.index_blobs(pending).await?;
        }

        self.job.enter(Phase::Saving, 0);
        let (listing, pending) = settle(&files, &self.stored(oids).await?);
        if !pending.is_empty() {
            return Err(Stop::Failed(format!(
                "{} blobs are still missing from the index after the build",
                pending.len()
            )));
        }
        self.alive()?;
        self.with_db({
            let commit = commit.to_string();
            move |db| db.save_commit(&commit, &listing)
        })
        .await
    }

    /// Gets the blobs into the index: fetches those that are not local, then parses and stores them.
    async fn index_blobs(&self, pending: Vec<Pending>) -> Result<(), Stop> {
        let commit = self.job.commit.as_str();
        let missing = self.repo.missing_objects(commit).await?;
        let to_fetch: Vec<String> = pending
            .iter()
            .filter(|blob| missing.contains(&blob.oid))
            .map(|blob| blob.oid.clone())
            .collect();
        let missing = if to_fetch.is_empty() {
            missing
        } else {
            self.fetch(to_fetch).await?;
            // What the remote did not give stays missing.
            self.repo.missing_objects(commit).await?
        };
        let (present, lost) = partition(pending, &missing);
        // Sizing a blob that is not local would cost git a scan of the promisor packs.
        let present_oids: Vec<String> = present.iter().map(|blob| blob.oid.clone()).collect();
        let sizes = self.repo.object_sizes(&present_oids).await?;
        let (readable, rows) = classify(present, lost, &sizes);

        self.job.enter(Phase::Parsing, readable.len());
        if !rows.is_empty() {
            self.with_db(move |db| db.insert_blobs(rows)).await?;
        }
        let mut inserting = None;
        let parsed = self
            .parse(groups(readable, self.limits.group_bytes), &mut inserting)
            .await;
        // The insert in the background is over before the build is.
        let inserted = wait_for_insert(inserting).await;
        parsed.and(inserted)
    }

    async fn fetch(&self, oids: Vec<String>) -> Result<(), Stop> {
        self.job.enter(Phase::Fetching, oids.len());
        for chunk in oids.chunks(self.limits.chunk(oids.len())) {
            self.alive()?;
            self.fetch_chunk(chunk.to_vec()).await?;
            self.job.advance(chunk.len());
        }
        Ok(())
    }

    /// Fetches in a detached task that owns the repo lock: if the build is dropped, git is not left running without it.
    async fn fetch_chunk(&self, oids: Vec<String>) -> Result<(), Stop> {
        let (repo, job, auth) = (self.repo.clone(), self.job.clone(), self.auth.clone());
        tokio::spawn(async move {
            let guard = repo.lock().await?;
            // The lock may have been held for a long time, by a pull request that was loading.
            if job.cancelled() {
                return Err(Stop::Cancelled);
            }
            repo.fetch_blobs(&guard, &auth, &oids).await?;
            Ok(())
        })
        .await
        .map_err(join_failed)?
    }

    /// Reads, parses and stores the groups of blobs one after the other. A group is stored in the background while the
    /// next one is read and parsed; `inserting` holds that insert for the caller to wait for.
    async fn parse(
        &self,
        groups: Vec<Vec<Pending>>,
        inserting: &mut Option<JoinHandle<Result<(), String>>>,
    ) -> Result<(), Stop> {
        for group in groups {
            self.alive()?;
            let oids: Vec<String> = group.iter().map(|blob| blob.oid.clone()).collect();
            let mut contents: HashMap<String, Vec<u8>> =
                self.repo.read_blobs(&oids).await?.into_iter().collect();
            let mut sources = Vec::new();
            let mut rows = Vec::new();
            for blob in group {
                match contents.remove(&blob.oid) {
                    Some(content) => sources.push((blob, content)),
                    None => {
                        rows.extend(unparsed(&blob, BlobStatus::Unreadable));
                        self.job.advance(1);
                    }
                }
            }
            let job = self.job.clone();
            let parsed = tokio::task::spawn_blocking(move || parse_blobs(&job, sources))
                .await
                .map_err(join_failed)??;
            rows.extend(parsed);
            wait_for_insert(inserting.take()).await?;
            let db = self.db.clone();
            *inserting = Some(tokio::task::spawn_blocking(move || db.insert_blobs(rows)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u32) -> String {
        format!("{n:040x}")
    }

    fn entry(path: &str, mode: &str, n: u32) -> TreeEntry {
        TreeEntry {
            path: path.to_string(),
            mode: mode.to_string(),
            oid: oid(n),
        }
    }

    fn file(path: &str, n: u32) -> TreeEntry {
        entry(path, "100644", n)
    }

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    fn pending(n: u32, at: &[&str]) -> Pending {
        Pending {
            oid: oid(n),
            paths: paths(at),
            size: 0,
        }
    }

    fn sized(n: u32, size: u64) -> Pending {
        Pending {
            size,
            ..pending(n, &["a.rs"])
        }
    }

    fn row(n: u32, grammar: Grammar, status: BlobStatus, size: u64) -> NewBlob {
        NewBlob {
            oid: oid(n),
            grammar,
            status,
            size,
            tags: Vec::new(),
        }
    }

    #[test]
    fn only_regular_files_of_a_known_language_outside_skipped_paths_are_indexed() {
        let entries = vec![
            file("src/lib.rs", 1),
            entry("bin/run.py", "100755", 2),
            entry("link.rs", "120000", 3),
            entry("sub.rs", "160000", 4),
            file("README.md", 5),
            file("web/node_modules/x/index.js", 6),
            file("web/app.min.js", 7),
            file("include/api.h", 8),
            file("web/app.js", 9),
        ];
        let got: Vec<String> = select_files(entries, 100)
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert_eq!(
            got,
            paths(&["src/lib.rs", "bin/run.py", "include/api.h", "web/app.js"])
        );
    }

    #[test]
    fn a_commit_with_more_files_than_the_limit_is_not_indexed() {
        let entries: Vec<TreeEntry> = (0..3).map(|n| file(&format!("f{n}.rs"), n)).collect();
        assert_eq!(select_files(entries.clone(), 3).unwrap().len(), 3);
        assert_eq!(
            select_files(entries, 2).unwrap_err(),
            Stop::Failed("the commit has 3 source files; the index covers at most 2".to_string())
        );
    }

    #[test]
    fn a_blob_is_held_under_the_grammar_its_extension_names() {
        assert_eq!(held_grammar("a.rs", &[Grammar::Rust]), Some(Grammar::Rust));
        assert_eq!(held_grammar("a.rs", &[]), None);
        // Stored under another grammar only: the index lacks it for this name.
        assert_eq!(held_grammar("a.cc", &[Grammar::C]), None);
        assert_eq!(held_grammar("a.c", &[Grammar::Cpp]), None);
        // A `.h` file follows its content, which is not at hand: any grammar of its family will do, C first.
        assert_eq!(held_grammar("a.h", &[Grammar::Cpp]), Some(Grammar::Cpp));
        assert_eq!(
            held_grammar("a.h", &[Grammar::C, Grammar::Cpp]),
            Some(Grammar::C)
        );
        assert_eq!(held_grammar("a.h", &[Grammar::Rust]), None);
    }

    #[test]
    fn files_of_stored_blobs_are_listed_and_the_rest_are_pending_by_blob() {
        let files = vec![
            file("a.rs", 1),
            file("b.rs", 2),
            file("c.rs", 2),
            file("d.py", 3),
            // Blob 1 is stored as Rust, and this name needs it as Python.
            file("e.py", 1),
        ];
        let stored = HashMap::from([(oid(1), vec![Grammar::Rust])]);
        let (listing, pending_blobs) = settle(&files, &stored);
        assert_eq!(listing, [("a.rs".to_string(), oid(1), Grammar::Rust)]);
        assert_eq!(
            pending_blobs,
            [
                pending(2, &["b.rs", "c.rs"]),
                pending(3, &["d.py"]),
                pending(1, &["e.py"]),
            ]
        );
    }

    #[test]
    fn a_blob_is_stored_once_for_each_grammar_its_paths_name() {
        assert_eq!(grammars_of(&paths(&["a.rs", "b.rs"]), b""), [Grammar::Rust]);
        assert_eq!(
            grammars_of(&paths(&["b.cc", "a.c"]), b""),
            [Grammar::C, Grammar::Cpp]
        );
        // A `.h` file is C without its content, and by its content otherwise.
        assert_eq!(grammars_of(&paths(&["a.h"]), b""), [Grammar::C]);
        assert_eq!(
            grammars_of(&paths(&["a.h"]), b"class A {};"),
            [Grammar::Cpp]
        );
    }

    #[test]
    fn blobs_are_split_by_whether_they_are_local() {
        let missing = HashSet::from([oid(2)]);
        let (present, lost) =
            partition(vec![pending(1, &["a.rs"]), pending(2, &["b.rs"])], &missing);
        assert_eq!(present, [pending(1, &["a.rs"])]);
        assert_eq!(lost, [pending(2, &["b.rs"])]);
    }

    #[test]
    fn blobs_that_are_not_parsed_get_a_row_that_says_why() {
        let sizes = HashMap::from([
            (oid(1), 10),
            (oid(2), MAX_BLOB_BYTES),
            (oid(3), MAX_BLOB_BYTES + 1),
        ]);
        let present = vec![
            pending(1, &["a.rs"]),
            pending(2, &["b.rs"]),
            pending(3, &["c.rs", "d.go"]),
            // Local, but its size could not be had.
            pending(4, &["e.rs"]),
        ];
        let lost = vec![pending(5, &["f.py"])];
        let (readable, rows) = classify(present, lost, &sizes);
        assert_eq!(
            readable,
            [
                Pending {
                    size: 10,
                    ..pending(1, &["a.rs"])
                },
                Pending {
                    size: MAX_BLOB_BYTES,
                    ..pending(2, &["b.rs"])
                },
            ]
        );
        assert_eq!(
            rows,
            [
                row(3, Grammar::Rust, BlobStatus::TooLarge, MAX_BLOB_BYTES + 1),
                row(3, Grammar::Go, BlobStatus::TooLarge, MAX_BLOB_BYTES + 1),
                row(4, Grammar::Rust, BlobStatus::Unreadable, 0),
                row(5, Grammar::Python, BlobStatus::Unreadable, 0),
            ]
        );
    }

    #[test]
    fn blobs_are_grouped_in_order_up_to_a_size() {
        let sizes = |groups: Vec<Vec<Pending>>| -> Vec<Vec<u64>> {
            groups
                .iter()
                .map(|group| group.iter().map(|blob| blob.size).collect())
                .collect()
        };
        let blobs = vec![
            sized(1, 4),
            sized(2, 4),
            sized(3, 4),
            sized(4, 10),
            sized(5, 1),
        ];
        assert_eq!(
            sizes(groups(blobs, 10)),
            [vec![4, 4], vec![4], vec![10], vec![1]]
        );
        // A blob over the limit makes a group of its own.
        assert_eq!(
            sizes(groups(vec![sized(1, 50), sized(2, 1)], 10)),
            [vec![50], vec![1]]
        );
        assert!(groups(Vec::new(), 10).is_empty());
    }

    #[test]
    fn a_blob_is_parsed_for_each_grammar_it_is_stored_under() {
        let job = Job::new(&oid(100));
        let sources = vec![
            (pending(1, &["a.rs", "b.py"]), b"pub fn one() {}\n".to_vec()),
            (
                pending(2, &["c.go"]),
                b"package p\n\nfunc Two() {}\n".to_vec(),
            ),
        ];
        let rows = parse_blobs(&job, sources).unwrap();
        let got: Vec<_> = rows
            .iter()
            .map(|row| (row.oid.clone(), row.grammar, row.status))
            .collect();
        assert_eq!(
            got,
            [
                (oid(1), Grammar::Rust, BlobStatus::Parsed),
                (oid(1), Grammar::Python, BlobStatus::Parsed),
                (oid(2), Grammar::Go, BlobStatus::Parsed),
            ]
        );
        assert!(
            rows[0]
                .tags
                .iter()
                .any(|tag| tag.name == "one" && tag.is_def)
        );
        assert!(
            rows[2]
                .tags
                .iter()
                .any(|tag| tag.name == "Two" && tag.is_def)
        );
        assert_eq!(job.status().done, 2);
    }

    #[test]
    fn rows_come_back_in_source_order_however_many_threads_parse() {
        let job = Job::new(&oid(100));
        let sources: Vec<_> = (0..40)
            .map(|n| {
                (
                    pending(n, &["a.rs"]),
                    format!("pub fn f{n}() {{}}\n").into_bytes(),
                )
            })
            .collect();
        let rows = parse_blobs(&job, sources).unwrap();
        let got: Vec<String> = rows.iter().map(|row| row.oid.clone()).collect();
        assert_eq!(got, (0..40).map(oid).collect::<Vec<_>>());
        assert!(rows.iter().all(|row| row.status == BlobStatus::Parsed));
        assert!(rows.iter().all(|row| row.tags.len() == 1));
        assert_eq!(job.status().done, 40);
        assert!(parse_blobs(&job, Vec::new()).unwrap().is_empty());
    }

    #[test]
    fn a_cancelled_job_parses_nothing() {
        let job = Job::new(&oid(100));
        job.cancel();
        let sources = vec![(pending(1, &["a.rs"]), b"pub fn one() {}\n".to_vec())];
        assert_eq!(parse_blobs(&job, sources).unwrap_err(), Stop::Cancelled);
        assert_eq!(job.status().done, 0);
    }

    #[test]
    fn a_fetch_chunk_is_a_twentieth_of_what_is_to_fetch_within_500_and_2000() {
        let limits = Limits::default();
        assert_eq!(limits.chunk(1), 500);
        assert_eq!(limits.chunk(10_000), 500);
        assert_eq!(limits.chunk(20_000), 1000);
        assert_eq!(limits.chunk(1_000_000), 2000);
        let fixed = Limits {
            fetch_chunk: Some(3),
            ..limits
        };
        assert_eq!(fixed.chunk(1_000_000), 3);
        // Never an empty chunk, which would make no progress.
        let zero = Limits {
            fetch_chunk: Some(0),
            ..limits
        };
        assert_eq!(zero.chunk(5), 1);
    }
}
