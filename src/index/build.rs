//! The build of one commit's index: list its source files, fetch the blobs the index lacks, parse them, and record
//! which blob each path holds.
//!
//! A build can be stopped at any step and started again: what it stored stays, so the next one does only what is left.
//! It looks for a cancellation between its steps and inside the parser. It never kills git, and it holds the repo lock
//! only while one chunk of blobs is fetched, so pull requests that are opened meanwhile are not kept waiting.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinError, JoinHandle};

use super::db::{BlobStatus, IndexDb, NewBlob};
use super::lang::{Family, Grammar, MAX_BLOB_BYTES, skipped_path};
use super::tags::{self, ParseError};
use super::{Job, Phase};
use crate::git::parse::GITLINK_MODE;
use crate::git::{GitAuth, GitFailure, Repo, STALL_INDEX, TreeEntry};

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

/// The grammars the index stores each blob under, with how it went with each (see `IndexDb::stored_blobs`).
type Stored = HashMap<String, Vec<(Grammar, BlobStatus)>>;

/// A blob the index lacks, with the source paths that hold it.
#[derive(Debug, PartialEq, Eq)]
struct Pending {
    oid: String,
    paths: Vec<String>,
    /// Known once the blob is local.
    size: u64,
}

/// The files of the tree that the index covers: regular files of a language it knows, outside the paths it leaves out.
fn select_files(entries: &[TreeEntry], max_files: usize) -> Result<Vec<TreeEntry>, Stop> {
    let files: Vec<TreeEntry> = entries
        .iter()
        .filter(|entry| {
            FILE_MODES.contains(&entry.mode.as_str())
                && Family::for_path(&entry.path).is_some()
                && !skipped_path(&entry.path)
        })
        .cloned()
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
/// has no such grammar, since it depends on the content, which is not at hand: any grammar of the C family will do, one
/// whose row has tags before one that is `unreadable` (a stale one), and C before C++.
fn held_grammar(path: &str, stored: &[(Grammar, BlobStatus)]) -> Option<Grammar> {
    match Grammar::for_path_only(path) {
        Some(grammar) => stored
            .iter()
            .any(|&(held, _)| held == grammar)
            .then_some(grammar),
        None => stored
            .iter()
            .filter(|(grammar, _)| grammar.family() == Family::C)
            .map(|&(grammar, status)| (status == BlobStatus::Unreadable, grammar))
            .min()
            .map(|(_, grammar)| grammar),
    }
}

/// Splits the files by whether the index holds their blob (`stored` lists the rows it holds each under): `listing` has
/// the files it does, with the grammar, and `pending` the blobs it lacks, each with its paths, in the order the files
/// first use them.
fn settle(files: &[TreeEntry], stored: &Stored) -> (Listing, Vec<Pending>) {
    let mut listing = Listing::new();
    let mut pending: Vec<Pending> = Vec::new();
    let mut at: HashMap<&str, usize> = HashMap::new();
    for file in files {
        let held = stored
            .get(&file.oid)
            .and_then(|rows| held_grammar(&file.path, rows));
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

/// The rows of `stored` that are not `unreadable`: what a build has no reason to do again.
fn without_unreadable(stored: &Stored) -> Stored {
    stored
        .iter()
        .map(|(oid, rows)| {
            let read = rows
                .iter()
                .copied()
                .filter(|&(_, status)| status != BlobStatus::Unreadable)
                .collect();
            (oid.clone(), read)
        })
        .collect()
}

/// The blobs among `oids` that the repo lacks (`missing`) and a build fetches, in two lists: those it has to parse
/// (`pending`), and those the index holds with their tags. A repo needs the latter for the lines of the references it
/// finds in them, and a second clone of a repository, or one made anew, lacks what the index holds from the first. A
/// blob too large to be parsed has no tags, so no references to read lines for.
fn to_fetch(
    oids: &[String],
    pending: &[Pending],
    stored: &Stored,
    missing: &HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let pending: HashSet<&str> = pending.iter().map(|blob| blob.oid.as_str()).collect();
    let parsed = |oid: &String| {
        stored
            .get(oid)
            .is_some_and(|rows| rows.iter().any(|&(_, status)| status == BlobStatus::Parsed))
    };
    oids.iter()
        .filter(|oid| missing.contains(*oid) && (pending.contains(oid.as_str()) || parsed(oid)))
        .cloned()
        .partition(|oid| pending.contains(oid.as_str()))
}

/// The `.h` files of `listing` that the content of their blob has to place: those whose blob was parsed under more than
/// one grammar of the C family, since only then does the choice matter (a blob that was not parsed has no tags). By
/// blob, with the places of its files in `listing`.
fn header_choices(listing: &Listing, stored: &Stored) -> BTreeMap<String, Vec<usize>> {
    let parsed_as_c = |oid: &str| {
        stored.get(oid).map_or(0, |rows| {
            rows.iter()
                .filter(|&&(grammar, status)| {
                    grammar.family() == Family::C && status == BlobStatus::Parsed
                })
                .count()
        })
    };
    let mut choices: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, (path, oid, _)) in listing.iter().enumerate() {
        if Grammar::for_path_only(path).is_none() && parsed_as_c(oid) > 1 {
            choices.entry(oid.clone()).or_default().push(i);
        }
    }
    choices
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

/// Groups of items, in order, whose sizes (`size` gives them) add up to at most `limit` bytes. An item over it makes a
/// group of its own.
pub(crate) fn groups<T>(items: Vec<T>, size: impl Fn(&T) -> u64, limit: u64) -> Vec<Vec<T>> {
    let mut groups: Vec<Vec<T>> = Vec::new();
    let mut bytes = 0;
    for item in items {
        let item_bytes = size(&item);
        match groups.last_mut() {
            Some(group) if bytes + item_bytes <= limit => {
                bytes += item_bytes;
                group.push(item);
            }
            _ => {
                bytes = item_bytes;
                groups.push(vec![item]);
            }
        }
    }
    groups
}

/// The rows of one blob, one for each grammar its paths name; None when `cancel` was raised. A blob that cannot be
/// parsed, or not in time, gets `unreadable` rows: the next build that lists it tries again.
fn parse_blob(blob: &Pending, content: &[u8], cancel: &AtomicUsize) -> Option<Vec<NewBlob>> {
    let mut rows = Vec::new();
    for grammar in grammars_of(&blob.paths, content) {
        let (status, tags) = match tags::parse(grammar, content, Some(cancel)) {
            Ok(tags) => (BlobStatus::Parsed, tags),
            Err(ParseError::Cancelled) => return None,
            Err(e @ (ParseError::TimedOut | ParseError::Failed(_))) => {
                eprintln!(
                    "gh-tideline: parsing {} as {} failed: {e}",
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

    /// A job that was cancelled before its turn has come ends cancelled, even if the indexer is shutting down too
    /// (`Indexer::shut_down` closes the turn and cancels the jobs).
    async fn wait_for(&self, turn: Arc<Semaphore>) -> Result<OwnedSemaphorePermit, Stop> {
        tokio::select! {
            biased;
            _ = self.job.wake.notified() => Err(Stop::Cancelled),
            permit = turn.acquire_owned() => permit
                .map_err(|_| Stop::Failed("indexing is shutting down".to_string())),
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

    async fn stored(&self, oids: Vec<String>) -> Result<Stored, Stop> {
        self.with_db(move |db| db.stored_blobs(&oids)).await
    }

    async fn build(&self) -> Result<(), Stop> {
        let commit = self.job.commit.as_str();
        self.alive()?;
        let entries = self.repo.ls_tree(commit).await?;
        // From now on the blob endpoint may fetch any file of the commit, as it may those of a pull request. A job that
        // finds the commit complete (below) has this to do as well: its clone may not have listed the commit yet.
        self.repo.remember_oids(
            entries
                .iter()
                .map(|entry| (entry.oid.as_str(), entry.mode != GITLINK_MODE)),
        );
        // Another job may have completed the commit while this one waited for its turn.
        let complete = self.with_db({
            let commit = commit.to_string();
            move |db| db.commit_complete(&commit)
        });
        if complete.await? {
            return Ok(());
        }
        let files = select_files(&entries, self.limits.max_files)?;
        let mut oids: Vec<String> = files.iter().map(|file| file.oid.clone()).collect();
        oids.sort();
        oids.dedup();

        // A blob that was left unreadable (it could not be fetched, or parsed in time) is tried again.
        let stored = self.stored(oids.clone()).await?;
        let (_, pending) = settle(&files, &without_unreadable(&stored));
        let missing = self.index_blobs(&oids, &stored, pending).await?;

        self.job.enter(Phase::Saving, 0);
        let stored = self.stored(oids).await?;
        // A blob that is unreadable still is no reason not to list the commit.
        let (mut listing, pending) = settle(&files, &stored);
        if !pending.is_empty() {
            return Err(Stop::Failed(format!(
                "{} blobs are still missing from the index after the build",
                pending.len()
            )));
        }
        self.headers_by_content(&mut listing, &stored, &missing)
            .await?;
        self.alive()?;
        self.with_db({
            let commit = commit.to_string();
            move |db| db.save_commit(&commit, &listing)
        })
        .await
    }

    /// A `.h` file is C or C++ by its content, but the index only knows which grammars its blob is stored under, and
    /// `settle` takes the first of the C family. Where there is a choice (the same content is parsed as C and as C++,
    /// because it has other names) the content decides, if the grammar it names has the tags. The blobs are read a group
    /// at a time, and only those that are local (`missing` lists the objects that are not, since the remote may not have
    /// given a blob, or the fetch of a blob the index holds may have failed), whose size the index knows, and which are no
    /// larger than it parses.
    async fn headers_by_content(
        &self,
        listing: &mut Listing,
        stored: &Stored,
        missing: &HashSet<String>,
    ) -> Result<(), Stop> {
        let choices = header_choices(listing, stored);
        let oids: Vec<String> = choices
            .keys()
            .filter(|oid| !missing.contains(*oid))
            .cloned()
            .collect();
        if oids.is_empty() {
            return Ok(());
        }
        let sizes = self
            .with_db({
                let oids = oids.clone();
                move |db| db.blob_sizes(&oids)
            })
            .await?;
        let readable: Vec<String> = oids
            .into_iter()
            .filter(|oid| sizes.get(oid).is_some_and(|&size| size <= MAX_BLOB_BYTES))
            .collect();
        let has_tags = |oid: &str, wanted: Grammar| {
            stored[oid]
                .iter()
                .any(|&(held, status)| held == wanted && status == BlobStatus::Parsed)
        };
        for group in groups(readable, |oid| sizes[oid], self.limits.group_bytes) {
            self.alive()?;
            for (oid, content) in self.repo.read_blobs(&group).await? {
                for &i in choices.get(&oid).into_iter().flatten() {
                    let (path, _, grammar) = &mut listing[i];
                    let by_content = Grammar::for_path(path, &content);
                    if let Some(wanted) = by_content.filter(|&wanted| has_tags(&oid, wanted)) {
                        *grammar = wanted;
                    }
                }
            }
        }
        Ok(())
    }

    /// Gets the blobs into the index: fetches the blobs the index lacks that the repo lacks too and, after them, the
    /// blobs the index holds that the repo lacks (see `to_fetch`), then parses and stores the former (`pending`). The
    /// phases follow one another and none comes back: fetching, which counts all the blobs it fetches, then parsing.
    /// Returns the objects the repo lacks by then.
    async fn index_blobs(
        &self,
        oids: &[String],
        stored: &Stored,
        pending: Vec<Pending>,
    ) -> Result<HashSet<String>, Stop> {
        let commit = self.job.commit.as_str();
        let mut missing = self.repo.missing_objects(commit).await?;
        let (to_parse, held) = to_fetch(oids, &pending, stored, &missing);
        if !to_parse.is_empty() || !held.is_empty() {
            self.fetch(to_parse, held).await?;
            // What the remote did not give stays missing.
            missing = self.repo.missing_objects(commit).await?;
        }
        if !pending.is_empty() {
            self.parse_and_store(pending, &missing).await?;
        }
        Ok(missing)
    }

    /// Parses and stores the blobs the index lacks. Those the repo lacks (`missing`), because the remote did not give
    /// them, get an `unreadable` row.
    async fn parse_and_store(
        &self,
        pending: Vec<Pending>,
        missing: &HashSet<String>,
    ) -> Result<(), Stop> {
        let (present, lost) = partition(pending, missing);
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
            .parse(
                groups(readable, |blob| blob.size, self.limits.group_bytes),
                &mut inserting,
            )
            .await;
        // The insert in the background is over before the build is.
        let inserted = wait_for_insert(inserting).await;
        parsed.and(inserted)
    }

    /// Fetches the blobs to parse and then the blobs the index holds, in chunks, in one phase that counts them all. The
    /// build cannot go on without the first, so a fetch of them that fails ends it. The others are fetched for the lines
    /// of the references in them: the commit does not need them to be indexed, and a lookup fetches the ones it needs, so
    /// a fetch of them that fails is told and the build goes on, without the chunks after it. A cancellation ends the
    /// build either way.
    async fn fetch(&self, to_parse: Vec<String>, held: Vec<String>) -> Result<(), Stop> {
        self.job.enter(Phase::Fetching, to_parse.len() + held.len());
        self.fetch_chunks(to_parse).await?;
        match self.fetch_chunks(held).await {
            Err(Stop::Failed(message)) => {
                eprintln!(
                    "gh-tideline: fetching the indexed blobs of {} failed: {message}",
                    self.job.commit
                );
                Ok(())
            }
            result => result,
        }
    }

    async fn fetch_chunks(&self, oids: Vec<String>) -> Result<(), Stop> {
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
            repo.fetch_blobs(&guard, &auth, &oids, STALL_INDEX).await?;
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
    use crate::git::testing::{repo_at, temp_dir};

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

    fn stored_as(rows: &[(u32, &[(Grammar, BlobStatus)])]) -> Stored {
        rows.iter()
            .map(|&(n, held)| (oid(n), held.to_vec()))
            .collect()
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
        let got: Vec<String> = select_files(&entries, 100)
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
        assert_eq!(select_files(&entries, 3).unwrap().len(), 3);
        assert_eq!(
            select_files(&entries, 2).unwrap_err(),
            Stop::Failed("the commit has 3 source files; the index covers at most 2".to_string())
        );
    }

    #[test]
    fn a_blob_is_held_under_the_grammar_its_extension_names() {
        let parsed = |grammars: &[Grammar]| -> Vec<(Grammar, BlobStatus)> {
            grammars.iter().map(|&g| (g, BlobStatus::Parsed)).collect()
        };
        assert_eq!(
            held_grammar("a.rs", &parsed(&[Grammar::Rust])),
            Some(Grammar::Rust)
        );
        assert_eq!(held_grammar("a.rs", &[]), None);
        // Stored under another grammar only: the index lacks it for this name.
        assert_eq!(held_grammar("a.cc", &parsed(&[Grammar::C])), None);
        assert_eq!(held_grammar("a.c", &parsed(&[Grammar::Cpp])), None);
        // A `.h` file follows its content, which is not at hand: any grammar of its family will do, C first.
        assert_eq!(
            held_grammar("a.h", &parsed(&[Grammar::Cpp])),
            Some(Grammar::Cpp)
        );
        assert_eq!(
            held_grammar("a.h", &parsed(&[Grammar::C, Grammar::Cpp])),
            Some(Grammar::C)
        );
        assert_eq!(held_grammar("a.h", &parsed(&[Grammar::Rust])), None);
    }

    #[test]
    fn a_header_is_held_under_a_grammar_that_was_read_before_one_that_was_not() {
        let unreadable_c = (Grammar::C, BlobStatus::Unreadable);
        let parsed_cpp = (Grammar::Cpp, BlobStatus::Parsed);
        // A stale `unreadable` row of C must not hide the tags the blob has as C++.
        assert_eq!(
            held_grammar("a.h", &[unreadable_c, parsed_cpp]),
            Some(Grammar::Cpp)
        );
        // With nothing better, the row is held all the same: a build lists a blob it could not read.
        assert_eq!(held_grammar("a.h", &[unreadable_c]), Some(Grammar::C));
        // A name that tells the grammar is held under that, whatever the other rows say.
        assert_eq!(
            held_grammar("a.c", &[unreadable_c, parsed_cpp]),
            Some(Grammar::C)
        );
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
        let stored = stored_as(&[(1, &[(Grammar::Rust, BlobStatus::Parsed)])]);
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
    fn an_unreadable_blob_is_pending_before_the_build_and_listed_after_it() {
        let files = vec![file("a.rs", 1), file("b.rs", 2), file("c.rs", 3)];
        let stored = stored_as(&[
            (1, &[(Grammar::Rust, BlobStatus::Parsed)]),
            (2, &[(Grammar::Rust, BlobStatus::Unreadable)]),
            (3, &[(Grammar::Rust, BlobStatus::TooLarge)]),
        ]);
        // A blob that could not be read is one to do again, ...
        let (listing, pending_blobs) = settle(&files, &without_unreadable(&stored));
        assert_eq!(
            listing,
            [
                ("a.rs".to_string(), oid(1), Grammar::Rust),
                ("c.rs".to_string(), oid(3), Grammar::Rust),
            ]
        );
        assert_eq!(pending_blobs, [pending(2, &["b.rs"])]);
        // ... and when the build has done what it can, a blob it could not read is listed all the same.
        let (listing, pending_blobs) = settle(&files, &stored);
        assert_eq!(listing.len(), 3);
        assert!(pending_blobs.is_empty());
    }

    #[test]
    fn blobs_the_repo_lacks_are_fetched_if_they_are_to_be_parsed_or_have_tags() {
        let oids: Vec<String> = (1..=7).map(oid).collect();
        let stored = stored_as(&[
            (2, &[(Grammar::Rust, BlobStatus::Parsed)]),
            (3, &[(Grammar::Rust, BlobStatus::TooLarge)]),
            (4, &[(Grammar::Rust, BlobStatus::Unreadable)]),
            (
                5,
                &[
                    (Grammar::C, BlobStatus::TooLarge),
                    (Grammar::Cpp, BlobStatus::Parsed),
                ],
            ),
            (6, &[(Grammar::Rust, BlobStatus::Parsed)]),
            // Stored by a version that knows more: nothing is known of its tags.
            (7, &[(Grammar::Rust, BlobStatus::Held)]),
        ]);
        // The index lacks 1, and 4 is to be tried again.
        let pending_blobs = vec![pending(1, &["a.rs"]), pending(4, &["d.rs"])];
        // The repo lacks all but 6.
        let missing: HashSet<String> = [1, 2, 3, 4, 5, 7].map(oid).into_iter().collect();
        // The blobs to parse come apart from those the index holds, which are fetched last and for their lines only. Not
        // 3, which is too large to have tags, nor 7, whose tags are not known to be there.
        assert_eq!(
            to_fetch(&oids, &pending_blobs, &stored, &missing),
            (vec![oid(1), oid(4)], vec![oid(2), oid(5)])
        );
        // A blob that is to be parsed for another name is no blob that is only held.
        let stored_as_go = stored_as(&[(1, &[(Grammar::Go, BlobStatus::Parsed)])]);
        assert_eq!(
            to_fetch(&oids, &pending_blobs, &stored_as_go, &missing),
            (vec![oid(1), oid(4)], vec![])
        );
        let nothing = (Vec::<String>::new(), Vec::<String>::new());
        assert_eq!(
            to_fetch(&oids, &pending_blobs, &stored, &HashSet::new()),
            nothing
        );
        assert_eq!(to_fetch(&[], &pending_blobs, &stored, &missing), nothing);
    }

    #[test]
    fn the_headers_the_content_has_to_place_are_those_parsed_under_two_grammars_of_c() {
        let listing: Listing = vec![
            ("a.h".to_string(), oid(1), Grammar::C),
            // Another name of the same blob.
            ("b/a.h".to_string(), oid(1), Grammar::C),
            // A name that tells its grammar needs no content.
            ("a.c".to_string(), oid(1), Grammar::C),
            // Too large to be parsed: both rows say so, and there are no tags to choose between.
            ("big.h".to_string(), oid(2), Grammar::C),
            // One of the rows is a stale `unreadable` one.
            ("stale.h".to_string(), oid(3), Grammar::Cpp),
            ("lone.h".to_string(), oid(4), Grammar::C),
            ("other.h".to_string(), oid(5), Grammar::C),
            // One of the rows is held under a status this version does not know: it is not known to have tags.
            ("held.h".to_string(), oid(6), Grammar::C),
        ];
        let both = |status: BlobStatus| [(Grammar::C, status), (Grammar::Cpp, status)];
        let stored = stored_as(&[
            (1, &both(BlobStatus::Parsed)),
            (2, &both(BlobStatus::TooLarge)),
            (
                3,
                &[
                    (Grammar::C, BlobStatus::Unreadable),
                    (Grammar::Cpp, BlobStatus::Parsed),
                ],
            ),
            (4, &[(Grammar::C, BlobStatus::Parsed)]),
            // Parsed under two grammars, but not of the same family.
            (
                5,
                &[
                    (Grammar::C, BlobStatus::Parsed),
                    (Grammar::Rust, BlobStatus::Parsed),
                ],
            ),
            (
                6,
                &[
                    (Grammar::C, BlobStatus::Parsed),
                    (Grammar::Cpp, BlobStatus::Held),
                ],
            ),
        ]);
        assert_eq!(
            header_choices(&listing, &stored),
            BTreeMap::from([(oid(1), vec![0, 1])])
        );
    }

    #[test]
    fn a_blob_stored_under_a_status_this_version_does_not_know_is_listed_and_not_parsed_again() {
        let held = [(Grammar::Rust, BlobStatus::Held)];
        assert_eq!(held_grammar("a.rs", &held), Some(Grammar::Rust));
        let files = vec![file("a.rs", 1), file("b.py", 1)];
        let stored = stored_as(&[(1, &held)]);
        // The file the row is for is listed, as one that was parsed is. The row says nothing of the other name of the
        // blob, which is to be parsed, as a blob that was not stored under it would be.
        let (listing, pending_blobs) = settle(&files, &without_unreadable(&stored));
        assert_eq!(listing, [("a.rs".to_string(), oid(1), Grammar::Rust)]);
        assert_eq!(pending_blobs, [pending(1, &["b.py"])]);
    }

    #[tokio::test]
    async fn a_header_is_read_for_its_language_only_if_it_is_local_and_its_size_is_known() {
        let dir = temp_dir("build-headers");
        // The repo does not exist, so that a read of any blob fails.
        let build = Build {
            job: Arc::new(Job::new(&oid(100))),
            repo: Arc::new(repo_at(dir.join("none.git"), String::new())),
            auth: GitAuth {
                host: "example.invalid".to_string(),
                token: "unused".to_string(),
            },
            db: IndexDb::open(&dir.join("index.sqlite")).unwrap(),
            limits: Limits::default(),
        };
        // Headers 1, 2 and 3 are parsed as C and as C++, so that their content has to place them. The index has the
        // size of 1 and 3, as a row that is stored has one.
        let sized = [1, 3].into_iter().flat_map(|n| {
            [Grammar::C, Grammar::Cpp].map(|grammar| row(n, grammar, BlobStatus::Parsed, 10))
        });
        build.db.insert_blobs(sized.collect()).unwrap();
        let both = [
            (Grammar::C, BlobStatus::Parsed),
            (Grammar::Cpp, BlobStatus::Parsed),
        ];
        let stored = stored_as(&[(1, &both), (2, &both), (3, &both)]);
        let mut listing: Listing = (1..=3)
            .map(|n| (format!("h{n}.h"), oid(n), Grammar::C))
            .collect();

        // The repo lacks 1, which the remote did not give, and the size of 2 is not known: neither is read.
        let missing = HashSet::from([oid(1)]);
        let mut skipped = listing[..2].to_vec();
        build
            .headers_by_content(&mut skipped, &stored, &missing)
            .await
            .unwrap();
        assert_eq!(skipped, listing[..2]);

        // Header 3 is local and sized: it is read, which fails here.
        let stop = build
            .headers_by_content(&mut listing, &stored, &missing)
            .await
            .unwrap_err();
        assert!(matches!(stop, Stop::Failed(_)), "{stop:?}");
        std::fs::remove_dir_all(&dir).unwrap();
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
            sizes(groups(blobs, |blob| blob.size, 10)),
            [vec![4, 4], vec![4], vec![10], vec![1]]
        );
        // A blob over the limit makes a group of its own.
        assert_eq!(
            sizes(groups(
                vec![sized(1, 50), sized(2, 1)],
                |blob| blob.size,
                10
            )),
            [vec![50], vec![1]]
        );
        assert!(groups(Vec::<Pending>::new(), |blob| blob.size, 10).is_empty());
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
    fn a_file_that_takes_too_long_to_parse_is_stored_unreadable_for_a_later_build_to_try_again() {
        // A definition and then a long run of `+` after an `=`: matching it takes tree-sitter far longer than a file is
        // given. That says nothing about the file, only about this run, so it must not be kept as a file with no tags.
        let content = format!("function f() {{}}\nx = {}", "+".repeat(300_000));
        let slow = Pending {
            size: content.len() as u64,
            ..pending(1, &["slow.js"])
        };
        let cancel = AtomicUsize::new(0);
        let started = std::time::Instant::now();
        let rows = parse_blob(&slow, content.as_bytes(), &cancel).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        assert_eq!(
            rows,
            [row(
                1,
                Grammar::JavaScript,
                BlobStatus::Unreadable,
                content.len() as u64
            )]
        );

        // The next parse on the thread goes well.
        let fine = parse_blob(&pending(2, &["fine.js"]), b"function g() {}\n", &cancel).unwrap();
        assert_eq!(fine[0].status, BlobStatus::Parsed);
        assert_eq!(fine[0].tags.len(), 1);
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
