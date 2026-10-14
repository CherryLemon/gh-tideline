//! A symbol index for code navigation: where names are defined and where they are used.
//!
//! The unit of work is a blob (the content of a file, named by its git OID). Each blob is parsed once with tree-sitter
//! into tags, the definitions and references of names it contains (`tags`), and every commit that contains the blob
//! shares those tags. A commit itself is only a listing of the blobs at its paths, its tree, so indexing a new commit
//! parses just the blobs not seen before. Lookups find tags by name within a language family (`lang`): a TypeScript
//! name sees JavaScript and TSX files, a C name sees C++ files, and no other language is mixed.
//!
//! The tags and the commits are kept in a SQLite file (`db`), which also answers the lookups. Their results are
//! ordered by how close they are to the file the lookup came from (`rank`).
//!
//! Indexing a commit is a job (`build`) that the `Indexer` starts, joins, reports on and cancels.

pub mod build;
pub mod db;
pub mod lang;
mod rank;
pub mod tags;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinSet;

use crate::git::{GitAuth, Repo};
use build::{Build, Limits, Stop};
use db::IndexDb;

const JOBS_POISONED: &str = "index jobs poisoned";
/// How many jobs that failed or were cancelled are kept for their outcome to be asked for. Past that the oldest are
/// forgotten, and a commit that was never built is `missing` all the same.
const FINISHED_JOBS_KEPT: usize = 64;

/// How far indexing a commit has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexState {
    /// Never built, and no job is building it.
    Missing,
    /// A job waits for the one that is building another commit.
    Queued,
    Building,
    /// Every file of the commit is indexed.
    Ready,
    /// The job stopped with an error, which `message` gives.
    Failed,
    Cancelled,
}

/// The step of a build that is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Listing,
    Fetching,
    Parsing,
    Saving,
}

/// What the client sees of the indexing of a commit. `done` and `total` count what the current phase works on: blobs
/// fetched while fetching, blobs parsed while parsing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub commit: String,
    pub state: IndexState,
    pub phase: Option<Phase>,
    pub done: u64,
    pub total: u64,
    /// Cancelled, but the job has not stopped yet: a fetch that has begun is finished first.
    pub cancelling: bool,
    /// Why the job failed.
    pub message: Option<String>,
}

impl IndexStatus {
    /// A commit no job is working on: `missing`, or `ready` from an earlier build.
    fn idle(commit: &str, state: IndexState) -> IndexStatus {
        IndexStatus {
            commit: commit.to_string(),
            state,
            phase: None,
            done: 0,
            total: 0,
            cancelling: false,
            message: None,
        }
    }
}

struct Progress {
    state: IndexState,
    phase: Option<Phase>,
    done: u64,
    total: u64,
    message: Option<String>,
    /// When the job finished.
    finished_at: Option<Instant>,
}

/// One build of one commit in one cache repo, and what it has done so far.
struct Job {
    commit: String,
    /// Raised by `cancel` and polled by the build between its steps and by tree-sitter while it parses.
    cancel: AtomicUsize,
    /// Wakes a job that waits for its turn when it is cancelled.
    wake: Notify,
    progress: Mutex<Progress>,
}

impl Job {
    fn new(commit: &str) -> Job {
        Job {
            commit: commit.to_string(),
            cancel: AtomicUsize::new(0),
            wake: Notify::new(),
            progress: Mutex::new(Progress {
                state: IndexState::Queued,
                phase: None,
                done: 0,
                total: 0,
                message: None,
                finished_at: None,
            }),
        }
    }

    fn progress(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress.lock().expect("index job poisoned")
    }

    fn status(&self) -> IndexStatus {
        let progress = self.progress();
        let running = matches!(progress.state, IndexState::Queued | IndexState::Building);
        IndexStatus {
            commit: self.commit.clone(),
            state: progress.state,
            phase: progress.phase,
            done: progress.done,
            total: progress.total,
            cancelling: running && self.cancelled(),
            message: progress.message.clone(),
        }
    }

    fn state(&self) -> IndexState {
        self.progress().state
    }

    /// Whether the job has not finished.
    fn active(&self) -> bool {
        matches!(self.state(), IndexState::Queued | IndexState::Building)
    }

    /// Whether a new job should take the place of this one: it failed or was cancelled, or it was cancelled and has not
    /// reached a check yet. Joining such a job would hand its outcome, `cancelled`, to someone who asked for a build.
    fn replaceable(&self) -> bool {
        match self.state() {
            IndexState::Failed | IndexState::Cancelled => true,
            IndexState::Queued | IndexState::Building => self.cancelled(),
            IndexState::Missing | IndexState::Ready => false,
        }
    }

    /// Asks the job to stop at its next check, and takes it out of the queue if it is waiting.
    fn cancel(&self) {
        self.cancel.store(1, Ordering::SeqCst);
        self.wake.notify_one();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst) != 0
    }

    fn begin(&self) {
        let mut progress = self.progress();
        progress.state = IndexState::Building;
        progress.phase = Some(Phase::Listing);
    }

    fn enter(&self, phase: Phase, total: usize) {
        let mut progress = self.progress();
        progress.phase = Some(phase);
        progress.done = 0;
        progress.total = total as u64;
    }

    fn advance(&self, count: usize) {
        self.progress().done += count as u64;
    }

    fn finish(&self, outcome: Result<(), Stop>) {
        let mut progress = self.progress();
        progress.phase = None;
        progress.finished_at = Some(Instant::now());
        match outcome {
            Ok(()) => progress.state = IndexState::Ready,
            Err(Stop::Cancelled) => progress.state = IndexState::Cancelled,
            Err(Stop::Failed(message)) => {
                progress.state = IndexState::Failed;
                progress.message = Some(message);
            }
        }
    }
}

/// Forgets the jobs whose outcome nobody needs: those that are ready (the database tells), and, when more than
/// `FINISHED_JOBS_KEPT` failed or were cancelled, the oldest of those. A job that has not finished is never forgotten.
fn forget_finished(jobs: &mut HashMap<(PathBuf, String), Arc<Job>>) {
    jobs.retain(|_, job| job.state() != IndexState::Ready);
    let mut finished: Vec<_> = jobs
        .iter()
        .filter(|(_, job)| !job.active())
        .map(|(key, job)| (job.progress().finished_at, key.clone()))
        .collect();
    if let Some(surplus) = finished.len().checked_sub(FINISHED_JOBS_KEPT) {
        finished.sort();
        for (_, key) in &finished[..surplus] {
            jobs.remove(key);
        }
    }
}

/// The index database and the jobs that fill it.
///
/// One commit is built at a time: builds compete for the network and the processors, and a second one would only slow
/// the first. The others are `queued`.
pub struct Indexer {
    db: IndexDb,
    limits: Limits,
    /// Held by the job that builds.
    turn: Arc<Semaphore>,
    /// By cache repo and commit. A job that failed or was cancelled stays until a new one replaces it, so its outcome
    /// can be asked for; one that is ready is forgotten when the next job starts, and the database tells. A job that was
    /// cancelled and is replaced by a new one is not here any more, but it goes on in its task until it stops.
    jobs: Mutex<HashMap<(PathBuf, String), Arc<Job>>>,
    tasks: Mutex<JoinSet<()>>,
}

impl Indexer {
    pub fn new(db: IndexDb, limits: Limits) -> Indexer {
        Indexer {
            db,
            limits,
            turn: Arc::new(Semaphore::new(1)),
            jobs: Default::default(),
            tasks: Default::default(),
        }
    }

    pub fn db(&self) -> &IndexDb {
        &self.db
    }

    async fn complete(&self, commit: &str) -> Result<bool, String> {
        let (db, commit) = (self.db.clone(), commit.to_string());
        tokio::task::spawn_blocking(move || db.commit_complete(&commit))
            .await
            .map_err(|e| format!("the index task failed: {e}"))?
    }

    fn job(&self, repo: &Repo, commit: &str) -> Option<Arc<Job>> {
        let key = (repo.git_dir.clone(), commit.to_string());
        self.jobs.lock().expect(JOBS_POISONED).get(&key).cloned()
    }

    /// Starts building the index of `commit`, unless it is complete already, or a job builds it (a second tab joins
    /// the first one's). A job that failed or was cancelled is replaced, and so is one that is being cancelled: the new
    /// job waits for its turn, which the old one gives up when it stops. `auth` is for fetching blobs.
    pub async fn start(
        &self,
        repo: Arc<Repo>,
        auth: GitAuth,
        commit: &str,
    ) -> Result<IndexStatus, String> {
        if self.complete(commit).await? {
            return Ok(IndexStatus::idle(commit, IndexState::Ready));
        }
        let job = {
            let mut jobs = self.jobs.lock().expect(JOBS_POISONED);
            let key = (repo.git_dir.clone(), commit.to_string());
            if let Some(job) = jobs.get(&key).filter(|job| !job.replaceable()) {
                return Ok(job.status());
            }
            forget_finished(&mut jobs);
            let job = Arc::new(Job::new(commit));
            jobs.insert(key, job.clone());
            job
        };
        let build = Build {
            job: job.clone(),
            repo,
            auth,
            db: self.db.clone(),
            limits: self.limits,
        };
        // The answer is the job as it was made, `queued`, whether or not the runtime has run its task by the time we ask.
        let status = job.status();
        let mut tasks = self.tasks.lock().expect("index tasks poisoned");
        while tasks.try_join_next().is_some() {}
        tasks.spawn(build.run(self.turn.clone()));
        Ok(status)
    }

    /// How far the index of `commit` has come: the job's word while it works or after it failed, else what the database
    /// knows.
    pub async fn status(&self, repo: &Repo, commit: &str) -> Result<IndexStatus, String> {
        let job = self.job(repo, commit);
        if let Some(job) = job.as_ref().filter(|job| job.active()) {
            return Ok(job.status());
        }
        if let Some(job) = &job
            && job.status().state == IndexState::Ready
        {
            return Ok(job.status());
        }
        if self.complete(commit).await? {
            return Ok(IndexStatus::idle(commit, IndexState::Ready));
        }
        Ok(match job {
            Some(job) => job.status(),
            None => IndexStatus::idle(commit, IndexState::Missing),
        })
    }

    /// Asks the job that builds `commit` to stop. The job ends at its next check, `cancelling` until then; what it has
    /// stored stays, so the next build goes on from there.
    pub async fn cancel(&self, repo: &Repo, commit: &str) -> Result<IndexStatus, String> {
        if let Some(job) = self.job(repo, commit).filter(|job| job.active()) {
            job.cancel();
            return Ok(job.status());
        }
        self.status(repo, commit).await
    }

    /// Cancels every job and refuses new ones: a job that waits for its turn leaves the queue, and one that starts from
    /// now on fails at once, saying that indexing is shutting down. The jobs end at their next check; `drain` waits for
    /// that.
    pub fn shut_down(&self) {
        // The jobs are cancelled before the turn is closed, so that one that waits for it leaves the queue cancelled
        // rather than failed. The lock is held throughout: a job that starts later finds the turn closed.
        let jobs = self.jobs.lock().expect(JOBS_POISONED);
        for job in jobs.values().filter(|job| job.active()) {
            job.cancel();
        }
        self.turn.close();
    }

    /// Shuts down (if that has not been done) and waits (up to `timeout`) for the jobs to stop, so that the git
    /// processes they started finish before the server exits.
    pub async fn drain(&self, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        self.shut_down();
        let mut tasks = std::mem::take(&mut *self.tasks.lock().expect("index tasks poisoned"));
        let _ = tokio::time::timeout_at(deadline, async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        tasks.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::time::Instant;

    use super::*;
    use crate::git::testing::{Gate, PartialClone, git_in};
    use db::{BlobStatus, Hit, NewBlob, Tier};
    use lang::{Family, Grammar};
    use tags::Tag;

    /// The longest a test waits for a job or a fetch.
    const PATIENCE: Duration = Duration::from_secs(60);

    /// A cache repo with two commits, an index database beside it, and an indexer over both.
    struct Setup {
        clone: PartialClone,
        indexer: Arc<Indexer>,
        db: IndexDb,
    }

    impl Setup {
        async fn new(name: &str) -> Setup {
            Setup::with_limits(name, Limits::default()).await
        }

        async fn with_limits(name: &str, limits: Limits) -> Setup {
            Setup::over(PartialClone::new(name).await, limits).await
        }

        async fn over(clone: PartialClone, limits: Limits) -> Setup {
            // Git unpacks a small fetch into loose objects; the tests count packs to count fetches.
            git_in(&clone.repo.git_dir, &["config", "fetch.unpackLimit", "1"]).await;
            let db = IndexDb::open(&clone.dir.join("index.sqlite")).unwrap();
            let indexer = Arc::new(Indexer::new(db.clone(), limits));
            Setup { clone, indexer, db }
        }

        async fn start(&self, commit: &str) -> IndexStatus {
            let (repo, auth) = (self.clone.repo.clone(), self.clone.auth.clone());
            self.indexer.start(repo, auth, commit).await.unwrap()
        }

        async fn status(&self, commit: &str) -> IndexStatus {
            self.indexer.status(&self.clone.repo, commit).await.unwrap()
        }

        async fn cancel(&self, commit: &str) -> IndexStatus {
            self.indexer.cancel(&self.clone.repo, commit).await.unwrap()
        }

        /// Polls the status of `commit` until `reached` holds for it.
        async fn until(&self, commit: &str, reached: impl Fn(&IndexStatus) -> bool) -> IndexStatus {
            self.until_in(&self.clone.repo, commit, reached).await
        }

        /// Like `until`, in another cache repo.
        async fn until_in(
            &self,
            repo: &Repo,
            commit: &str,
            reached: impl Fn(&IndexStatus) -> bool,
        ) -> IndexStatus {
            let deadline = Instant::now() + PATIENCE;
            loop {
                let status = self.indexer.status(repo, commit).await.unwrap();
                if reached(&status) {
                    return status;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out; the status is {status:?}"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }

        /// Waits for the job that was started for `commit` to end, and returns how it ended.
        async fn end(&self, commit: &str) -> IndexStatus {
            self.until(commit, ended).await
        }

        /// Builds `commit` to the end.
        async fn build(&self, commit: &str) -> IndexStatus {
            self.start(commit).await;
            self.end(commit).await
        }

        /// Builds `commit` to the end in another cache repo over the same source repo.
        async fn build_in(&self, repo: &Arc<Repo>, commit: &str) -> IndexStatus {
            let auth = self.clone.auth.clone();
            self.indexer
                .start(repo.clone(), auth, commit)
                .await
                .unwrap();
            self.until_in(repo, commit, ended).await
        }

        /// How many packs the cache repo holds and how many objects are in them. Every fetch of blobs adds a pack.
        async fn packs(&self) -> (u64, u64) {
            let out = git_in(&self.clone.repo.git_dir, &["count-objects", "-v"]).await;
            let count = |key: &str| -> u64 {
                out.lines()
                    .find_map(|line| line.strip_prefix(key))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap()
            };
            (count("packs:"), count("in-pack:"))
        }

        /// The files of `commit` as the database lists them, with the grammar of their blobs, in path order.
        fn listing(&self, commit: &str) -> Vec<(String, String)> {
            let conn = rusqlite::Connection::open(self.clone.dir.join("index.sqlite")).unwrap();
            let mut query = conn
                .prepare(
                    "SELECT te.path, b.grammar FROM tree_entries te
                     JOIN blobs b ON b.id = te.blob_id
                     JOIN commits c ON c.id = te.commit_id
                     WHERE c.oid = ?1 AND c.complete = 1 ORDER BY te.path",
                )
                .unwrap();
            query
                .query_map([commit], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        }

        fn jobs(&self) -> usize {
            self.indexer.jobs.lock().unwrap().len()
        }
    }

    /// Whether a job has stopped, one way or another.
    fn ended(status: &IndexStatus) -> bool {
        !matches!(status.state, IndexState::Queued | IndexState::Building)
    }

    fn files(listing: &[(&str, &str)]) -> Vec<(String, String)> {
        listing
            .iter()
            .map(|(path, grammar)| (path.to_string(), grammar.to_string()))
            .collect()
    }

    fn limits_of_chunk(chunk: usize) -> Limits {
        Limits {
            fetch_chunk: Some(chunk),
            ..Limits::default()
        }
    }

    /// Makes the remote of `repo` serve the first fetch it gets and refuse every one after it.
    async fn serve_one_fetch_only(clone: &PartialClone, repo: &Repo) {
        let served = clone.dir.join("fetch-served");
        let script = clone.dir.join("serve-one-fetch.sh");
        std::fs::write(
            &script,
            format!(
                "if [ -e '{0}' ]; then echo 'the remote is gone' >&2; exit 1; fi\ntouch '{0}'\nexec git upload-pack \"$@\"\n",
                served.display()
            ),
        )
        .unwrap();
        let command = format!("sh {}", script.display());
        git_in(
            &repo.git_dir,
            &["config", "remote.origin.uploadpack", &command],
        )
        .await;
    }

    fn kept_tag() -> Tag {
        Tag {
            name: "kept".to_string(),
            kind: "function".to_string(),
            is_def: true,
            line: 1,
            column: 4,
            end_column: 8,
            scope_start: 1,
            scope_end: 1,
            snippet: "fn kept() {}".to_string(),
            snippet_column: 4,
        }
    }

    #[tokio::test]
    async fn a_build_indexes_the_source_files_of_a_commit() {
        let s = Setup::new("index-build").await;
        let head = s.clone.head.clone();
        assert_eq!(
            s.status(&head).await,
            IndexStatus::idle(&head, IndexState::Missing)
        );

        // `start` answers for the job as it was made, whether or not the runtime has run it yet.
        assert_eq!(
            s.start(&head).await,
            IndexStatus::idle(&head, IndexState::Queued)
        );
        assert_eq!(
            s.end(&head).await,
            IndexStatus::idle(&head, IndexState::Ready)
        );

        // The files with a source, in path order: not the text and binary files, nor the gitlink.
        assert_eq!(
            s.listing(&head),
            files(&[
                ("big.rs", "rust"),
                ("dir with space/a b.py", "python"),
                ("src/lib.rs", "rust"),
                ("src/util/helpers.rs", "rust"),
            ])
        );
        let (hits, _) =
            s.db.definitions(&head, Family::Rust, "two", "src/lib.rs")
                .unwrap()
                .unwrap();
        assert_eq!(
            hits,
            [Hit {
                path: "src/lib.rs".to_string(),
                line: 2,
                column: 8,
                end_column: 11,
                kind: "function".to_string(),
                snippet: "pub fn two() {}".to_string(),
                snippet_column: 8,
                tier: Tier::File,
            }]
        );

        // A file over a mebibyte is fetched and noted, not parsed.
        let big = s.clone.oid(&head, "big.rs").await;
        let stored = s.db.blob_symbols(&big, Grammar::Rust).unwrap().unwrap();
        assert_eq!(
            (stored.status, stored.symbols.len()),
            (BlobStatus::TooLarge, 0)
        );

        // Files that are no sources are not even fetched.
        let text = s.clone.oid(&head, "same-1.txt").await;
        let binary = s.clone.oid(&head, "data.bin").await;
        assert!(
            s.db.stored_blobs(&[text.clone(), binary.clone()])
                .unwrap()
                .is_empty()
        );
        let missing = s.clone.repo.missing_objects(&head).await.unwrap();
        assert_eq!(missing, HashSet::from([text.clone(), binary]));

        // The blob endpoint may now serve any file of the commit.
        assert_eq!(s.clone.repo.known_oid(&text), Some(true));
        let gitlink = "1234567890123456789012345678901234567890";
        assert_eq!(s.clone.repo.known_oid(gitlink), Some(false));
    }

    #[tokio::test]
    async fn blobs_the_index_holds_are_fetched_for_their_lines_but_not_parsed_again() {
        let s = Setup::new("index-held").await;
        let head = s.clone.head.clone();
        let helpers = s.clone.oid(&head, "src/util/helpers.rs").await;
        let big = s.clone.oid(&head, "big.rs").await;
        // Rows the parser would not make: the build must leave them as they are.
        s.db.insert_blobs(vec![
            NewBlob {
                oid: helpers.clone(),
                grammar: Grammar::Rust,
                status: BlobStatus::Parsed,
                size: 17,
                tags: vec![kept_tag()],
            },
            NewBlob {
                oid: big.clone(),
                grammar: Grammar::Rust,
                status: BlobStatus::TooLarge,
                size: 1 << 21,
                tags: Vec::new(),
            },
        ])
        .unwrap();
        let before = s.packs().await;

        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        // The repo must hold a blob the index has tags of, for the lines of the references found in it, but not one
        // that was too large to be parsed. The two sources the index lacked are fetched first, then the helpers in a
        // fetch of their own, so that a failure of it can be told apart and does not stop the build. The two sources
        // are parsed after both fetches.
        let missing = s.clone.repo.missing_objects(&head).await.unwrap();
        assert!(!missing.contains(&helpers));
        assert!(missing.contains(&big));
        let after = s.packs().await;
        assert_eq!((after.0 - before.0, after.1 - before.1), (2, 3));
        let stored = s.db.blob_symbols(&helpers, Grammar::Rust).unwrap().unwrap();
        let names: Vec<&str> = stored.symbols.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, ["kept"]);
        let listing = s.listing(&head);
        assert!(listing.contains(&("src/util/helpers.rs".to_string(), "rust".to_string())));
        assert!(listing.contains(&("big.rs".to_string(), "rust".to_string())));
    }

    #[tokio::test]
    async fn the_phases_of_a_build_follow_one_another_and_the_fetching_counts_every_blob_it_fetches()
     {
        let s = Setup::with_limits("index-phases", limits_of_chunk(1)).await;
        // The fetches of the two blobs to parse go through, and that of the blob the index holds is held back.
        let gate = Gate::after(&s.clone, 2).await;
        let head = s.clone.head.clone();
        let helpers = s.clone.oid(&head, "src/util/helpers.rs").await;
        let big = s.clone.oid(&head, "big.rs").await;
        let lib = s.clone.oid(&head, "src/lib.rs").await;
        let python = s.clone.oid(&head, "dir with space/a b.py").await;
        let held = |oid: &String, status, tags| NewBlob {
            oid: oid.clone(),
            grammar: Grammar::Rust,
            status,
            size: 17,
            tags,
        };
        s.db.insert_blobs(vec![
            held(&helpers, BlobStatus::Parsed, vec![kept_tag()]),
            held(&big, BlobStatus::TooLarge, Vec::new()),
        ])
        .unwrap();

        s.start(&head).await;
        gate.begun().await;
        // One phase counts the blobs to parse and the blob held, and the third fetch is the last one: nothing is
        // parsed before it, so that the phase does not go back to fetching when parsing has begun.
        assert_eq!(
            s.status(&head).await,
            IndexStatus {
                commit: head.clone(),
                state: IndexState::Building,
                phase: Some(Phase::Fetching),
                done: 2,
                total: 3,
                cancelling: false,
                message: None,
            }
        );
        let missing = s.clone.repo.missing_objects(&head).await.unwrap();
        assert!(!missing.contains(&lib) && !missing.contains(&python));
        assert!(missing.contains(&helpers));
        assert!(s.db.blob_symbols(&lib, Grammar::Rust).unwrap().is_none());
        assert!(
            s.db.blob_symbols(&python, Grammar::Python)
                .unwrap()
                .is_none()
        );

        gate.open();
        assert_eq!(
            s.end(&head).await,
            IndexStatus::idle(&head, IndexState::Ready)
        );
        assert!(s.db.blob_symbols(&lib, Grammar::Rust).unwrap().is_some());
        assert!(
            !s.clone
                .repo
                .missing_objects(&head)
                .await
                .unwrap()
                .contains(&helpers)
        );
    }

    #[tokio::test]
    async fn a_blob_that_was_unreadable_is_parsed_by_the_next_build() {
        let s = Setup::new("index-unreadable").await;
        let head = s.clone.head.clone();
        let helpers = s.clone.oid(&head, "src/util/helpers.rs").await;
        // What a build leaves of a blob it could not fetch, or could not parse in time.
        s.db.insert_blobs(vec![NewBlob {
            oid: helpers.clone(),
            grammar: Grammar::Rust,
            status: BlobStatus::Unreadable,
            size: 0,
            tags: Vec::new(),
        }])
        .unwrap();

        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        let stored = s.db.blob_symbols(&helpers, Grammar::Rust).unwrap().unwrap();
        assert_eq!(stored.status, BlobStatus::Parsed);
        let names: Vec<&str> = stored.symbols.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, ["help"]);
        let listing = s.listing(&head);
        assert!(listing.contains(&("src/util/helpers.rs".to_string(), "rust".to_string())));
        assert_eq!(listing.len(), 4);
    }

    #[tokio::test]
    async fn a_build_in_a_clone_made_anew_fetches_the_blobs_the_index_already_holds() {
        let s = Setup::new("index-clones").await;
        let (base, head) = (s.clone.base.clone(), s.clone.head.clone());
        let helpers = s.clone.oid(&head, "src/util/helpers.rs").await;
        let python = s.clone.oid(&head, "dir with space/a b.py").await;
        // The helpers and the Python file are blobs of the base too: its build parsed them, in the first clone.
        assert_eq!(s.build(&base).await.state, IndexState::Ready);
        let held = (
            s.db.blob_symbols(&helpers, Grammar::Rust).unwrap(),
            s.db.blob_symbols(&python, Grammar::Python).unwrap(),
        );
        assert!(held.0.is_some() && held.1.is_some());

        // A second clone, as one made anew is, has the commits and none of the blobs.
        let other = s.clone.another_cache().await;
        let lacking = other.missing_objects(&head).await.unwrap();
        assert!(lacking.contains(&helpers) && lacking.contains(&python));

        // Its references must have their lines, so the build fetches all the sources of the head, those it needs to
        // parse and those the index holds. The text and the binary file are no sources.
        assert_eq!(s.build_in(&other, &head).await.state, IndexState::Ready);
        let text = s.clone.oid(&head, "same-1.txt").await;
        let binary = s.clone.oid(&head, "data.bin").await;
        assert_eq!(
            other.missing_objects(&head).await.unwrap(),
            HashSet::from([text, binary])
        );
        assert_eq!(
            (
                s.db.blob_symbols(&helpers, Grammar::Rust).unwrap(),
                s.db.blob_symbols(&python, Grammar::Python).unwrap()
            ),
            held
        );
        assert_eq!(s.listing(&head).len(), 4);
    }

    #[tokio::test]
    async fn a_build_is_ready_if_the_blobs_the_index_holds_cannot_be_fetched() {
        let s = Setup::new("index-held-fails").await;
        let (base, head) = (s.clone.base.clone(), s.clone.head.clone());
        let helpers = s.clone.oid(&head, "src/util/helpers.rs").await;
        let python = s.clone.oid(&head, "dir with space/a b.py").await;
        assert_eq!(s.build(&base).await.state, IndexState::Ready);
        // A clone made anew, whose remote serves the one fetch for the blobs to parse (the new lib.rs and big.rs) and
        // then goes away, before the fetch for the helpers and the Python file that the index holds.
        let other = s.clone.another_cache().await;
        serve_one_fetch_only(&s.clone, &other).await;

        // The lines of their references are all those blobs are fetched for, which the lookups can do on their own:
        // the commit is indexed all the same.
        assert_eq!(
            s.build_in(&other, &head).await,
            IndexStatus::idle(&head, IndexState::Ready)
        );
        assert_eq!(s.listing(&head).len(), 4);
        let text = s.clone.oid(&head, "same-1.txt").await;
        let binary = s.clone.oid(&head, "data.bin").await;
        assert_eq!(
            other.missing_objects(&head).await.unwrap(),
            HashSet::from([text, binary, helpers, python])
        );
    }

    #[tokio::test]
    async fn a_cancel_while_the_blobs_the_index_holds_are_fetched_ends_the_build_all_the_same() {
        let s = Setup::with_limits("index-held-cancel", limits_of_chunk(1)).await;
        let gate = Gate::install(&s.clone).await;
        let head = s.clone.head.clone();
        // The index holds every source of the head, from builds in other clones, but has not listed the commit.
        let mut rows = Vec::new();
        for (path, grammar, status) in [
            ("big.rs", Grammar::Rust, BlobStatus::TooLarge),
            ("dir with space/a b.py", Grammar::Python, BlobStatus::Parsed),
            ("src/lib.rs", Grammar::Rust, BlobStatus::Parsed),
            ("src/util/helpers.rs", Grammar::Rust, BlobStatus::Parsed),
        ] {
            rows.push(NewBlob {
                oid: s.clone.oid(&head, path).await,
                grammar,
                status,
                size: 17,
                tags: if status == BlobStatus::Parsed {
                    vec![kept_tag()]
                } else {
                    Vec::new()
                },
            });
        }
        s.db.insert_blobs(rows).unwrap();
        let before = s.packs().await;

        // Nothing is to be parsed, so the job only fetches: the three blobs with tags, one at a time.
        s.start(&head).await;
        gate.begun().await;
        let fetching = IndexStatus {
            commit: head.clone(),
            state: IndexState::Building,
            phase: Some(Phase::Fetching),
            done: 0,
            total: 3,
            cancelling: false,
            message: None,
        };
        assert_eq!(s.status(&head).await, fetching);
        assert!(s.cancel(&head).await.cancelling);

        gate.open();
        assert_eq!(
            s.end(&head).await,
            IndexStatus {
                state: IndexState::Cancelled,
                phase: None,
                done: 1,
                ..fetching
            }
        );
        let after = s.packs().await;
        assert_eq!((after.0 - before.0, after.1 - before.1), (1, 1));
        assert!(!s.db.commit_complete(&head).unwrap());

        // The next build fetches the two that are left, and lists the commit.
        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        let after_all = s.packs().await;
        assert_eq!(
            (after_all.0 - after.0, after_all.1 - after.1),
            (2, 2),
            "only the blobs still missing are fetched"
        );
        assert_eq!(s.listing(&head).len(), 4);
    }

    #[tokio::test]
    async fn blobs_that_are_local_are_not_fetched() {
        let s = Setup::new("index-local").await;
        let head = s.clone.head.clone();
        let all: Vec<String> = s.clone.blobs(&head).await.into_iter().collect();
        s.clone
            .fetch(&all.iter().map(String::as_str).collect::<Vec<_>>())
            .await;
        let packs = s.packs().await;

        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        assert_eq!(s.packs().await, packs);
        assert_eq!(s.listing(&head).len(), 4);
    }

    #[tokio::test]
    async fn a_second_commit_fetches_only_the_blobs_the_first_lacked() {
        let s = Setup::new("index-second").await;
        let (base, head) = (s.clone.base.clone(), s.clone.head.clone());
        let before = s.packs().await;

        assert_eq!(s.build(&base).await.state, IndexState::Ready);
        // lib.rs, helpers.rs and a b.py, in one fetch: the readme and the text files are no sources.
        let after_base = s.packs().await;
        assert_eq!((after_base.0 - before.0, after_base.1 - before.1), (1, 3));
        assert_eq!(
            s.listing(&base),
            files(&[
                ("dir with space/a b.py", "python"),
                ("src/lib.rs", "rust"),
                ("src/util/helpers.rs", "rust"),
            ])
        );

        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        // The new lib.rs and big.rs: the other sources are the base's blobs.
        let after_head = s.packs().await;
        assert_eq!(
            (after_head.0 - after_base.0, after_head.1 - after_base.1),
            (1, 2)
        );
        assert_eq!(s.listing(&head).len(), 4);
        // The commits are told apart: lib.rs has one definition in the base and two in the head.
        let one = |commit: &str, name: &str| {
            s.db.definitions(commit, Family::Rust, name, "src/lib.rs")
                .unwrap()
                .unwrap()
                .0
                .len()
        };
        assert_eq!((one(&base, "one"), one(&base, "two")), (1, 0));
        assert_eq!((one(&head, "one"), one(&head, "two")), (1, 1));
    }

    #[tokio::test]
    async fn blobs_are_fetched_in_chunks() {
        let s = Setup::with_limits("index-chunks", limits_of_chunk(2)).await;
        let head = s.clone.head.clone();
        let before = s.packs().await;

        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        // Four sources to fetch, two at a time.
        let after = s.packs().await;
        assert_eq!((after.0 - before.0, after.1 - before.1), (2, 4));
    }

    #[tokio::test]
    async fn cancelling_waits_for_the_fetch_that_has_begun_and_stops_before_the_next() {
        let s = Setup::with_limits("index-cancel", limits_of_chunk(1)).await;
        let gate = Gate::install(&s.clone).await;
        let head = s.clone.head.clone();
        let before = s.packs().await;

        s.start(&head).await;
        gate.begun().await;
        let fetching = IndexStatus {
            commit: head.clone(),
            state: IndexState::Building,
            phase: Some(Phase::Fetching),
            done: 0,
            total: 4,
            cancelling: false,
            message: None,
        };
        assert_eq!(s.status(&head).await, fetching);

        // The fetch cannot be taken back: the job says it is stopping, and goes on until it ends.
        let cancelling = IndexStatus {
            cancelling: true,
            ..fetching.clone()
        };
        assert_eq!(s.cancel(&head).await, cancelling);
        assert_eq!(s.status(&head).await, cancelling);

        gate.open();
        let ended = s.end(&head).await;
        assert_eq!(
            ended,
            IndexStatus {
                state: IndexState::Cancelled,
                phase: None,
                done: 1,
                cancelling: false,
                ..fetching
            }
        );
        let after = s.packs().await;
        assert_eq!((after.0 - before.0, after.1 - before.1), (1, 1));

        // A cancelled job is replaced, and the new one goes on from what is there.
        assert_eq!(
            s.start(&head).await,
            IndexStatus::idle(&head, IndexState::Queued)
        );
        assert_eq!(
            s.end(&head).await,
            IndexStatus::idle(&head, IndexState::Ready)
        );
        let after_all = s.packs().await;
        assert_eq!(
            (after_all.0 - after.0, after_all.1 - after.1),
            (3, 3),
            "only the blobs still missing are fetched"
        );
        assert_eq!(s.jobs(), 1);
    }

    #[tokio::test]
    async fn a_start_after_a_cancel_gets_a_new_job_while_the_old_one_is_still_stopping() {
        let s = Setup::with_limits("index-restart", limits_of_chunk(1)).await;
        let gate = Gate::install(&s.clone).await;
        let head = s.clone.head.clone();
        let before = s.packs().await;

        s.start(&head).await;
        gate.begun().await;
        assert!(s.cancel(&head).await.cancelling);

        // The old job waits for the fetch that has begun and will end cancelled. A start does not join it, which would
        // answer a request for a build with `cancelled`, but makes a job that waits for the turn the old one gives up.
        assert_eq!(
            s.start(&head).await,
            IndexStatus::idle(&head, IndexState::Queued)
        );
        assert_eq!(
            s.status(&head).await,
            IndexStatus::idle(&head, IndexState::Queued)
        );
        assert_eq!(s.jobs(), 1);

        // The end of the old job leaves the new one alone, which goes on from what the old one fetched.
        gate.open();
        assert_eq!(
            s.end(&head).await,
            IndexStatus::idle(&head, IndexState::Ready)
        );
        let after = s.packs().await;
        assert_eq!((after.0 - before.0, after.1 - before.1), (4, 4));
        assert_eq!(s.listing(&head).len(), 4);
    }

    #[tokio::test]
    async fn a_job_that_finds_the_commit_complete_when_its_turn_comes_is_ready_without_fetching() {
        let s = Setup::new("index-complete-meanwhile").await;
        let gate = Gate::install(&s.clone).await;
        let head = s.clone.head.clone();
        let other = s.clone.another_cache().await;
        let lacking = other.missing_objects(&head).await.unwrap();
        let text = s.clone.oid(&head, "same-1.txt").await;
        let gitlink = "1234567890123456789012345678901234567890";
        assert_eq!(other.known_oid(&text), None);

        // A build of the commit in one clone is under way, and another clone asks for the same commit: it waits.
        s.start(&head).await;
        gate.begun().await;
        let auth = s.clone.auth.clone();
        let queued = s.indexer.start(other.clone(), auth, &head).await.unwrap();
        assert_eq!(queued, IndexStatus::idle(&head, IndexState::Queued));

        // When the first build has listed the commit, the second has nothing left to do, not even to fetch.
        gate.open();
        assert_eq!(s.end(&head).await.state, IndexState::Ready);
        let second = s.until_in(&other, &head, ended).await;
        assert_eq!(second, IndexStatus::idle(&head, IndexState::Ready));
        assert_eq!(other.missing_objects(&head).await.unwrap(), lacking);

        // The blob endpoint of the second clone may serve the files of the commit all the same: its job listed them.
        assert_eq!(other.known_oid(&text), Some(true));
        assert_eq!(other.known_oid(gitlink), Some(false));
    }

    #[tokio::test]
    async fn cancelling_while_a_group_is_being_stored_ends_the_build_and_keeps_what_was_stored() {
        let sources = ["a", "b", "c", "d"]
            .map(|name| (format!("src/{name}.rs"), format!("pub fn {name}() {{}}\n")));
        let head: Vec<(&str, &str)> = sources
            .iter()
            .map(|(path, source)| (path.as_str(), source.as_str()))
            .collect();
        let clone =
            PartialClone::with_files("index-cancel-insert", &[("README.md", "r\n")], &head).await;
        // Every blob is a group of its own, so the build stores one group while it parses the next.
        let limits = Limits {
            group_bytes: 1,
            ..Limits::default()
        };
        let s = Setup::over(clone, limits).await;
        let head = s.clone.head.clone();

        // Another connection holds the write lock of the database, so the first insert waits for it.
        let writer = rusqlite::Connection::open(s.clone.dir.join("index.sqlite")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        s.start(&head).await;
        let parsing = s
            .until(&head, |status| {
                status.phase == Some(Phase::Parsing) && status.done >= 2
            })
            .await;
        // Two groups are parsed, and the build waits for the insert of the first before it goes on to a third.
        assert_eq!((parsing.done, parsing.total), (2, 4));
        assert!(s.cancel(&head).await.cancelling);

        // The insert gets through when the lock is let go, well within the time it waits for it.
        writer.execute_batch("ROLLBACK").unwrap();
        let ended = s.end(&head).await;
        assert_eq!(
            (ended.state, ended.phase, ended.cancelling, ended.message),
            (IndexState::Cancelled, None, false, None)
        );
        let oids = [
            s.clone.oid(&head, "src/a.rs").await,
            s.clone.oid(&head, "src/b.rs").await,
            s.clone.oid(&head, "src/c.rs").await,
            s.clone.oid(&head, "src/d.rs").await,
        ];
        let stored = s.db.stored_blobs(&oids).unwrap();
        assert!(stored.contains_key(&oids[0]) && stored.contains_key(&oids[1]));
        assert!(!stored.contains_key(&oids[2]) && !stored.contains_key(&oids[3]));
        assert!(!s.db.commit_complete(&head).unwrap());

        // The next build goes on from there.
        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        assert_eq!(s.listing(&head).len(), 4);
    }

    #[tokio::test]
    async fn a_job_cancelled_while_it_waits_for_the_repo_fetches_nothing() {
        let s = Setup::with_limits("index-cancel-lock", limits_of_chunk(1)).await;
        let head = s.clone.head.clone();
        let before = s.packs().await;

        let guard = s.clone.repo.lock().await.unwrap();
        s.start(&head).await;
        s.until(&head, |status| status.phase == Some(Phase::Fetching))
            .await;
        let cancelling = s.cancel(&head).await;
        assert_eq!(
            (cancelling.state, cancelling.cancelling),
            (IndexState::Building, true)
        );

        // The job ends when it gets the repo and finds that it was cancelled.
        drop(guard);
        let ended = s.end(&head).await;
        assert_eq!(
            (ended.state, ended.cancelling, ended.done),
            (IndexState::Cancelled, false, 0)
        );
        assert_eq!(s.packs().await, before);
    }

    #[tokio::test]
    async fn starts_join_the_job_that_builds_the_commit() {
        let s = Setup::new("index-join").await;
        let head = s.clone.head.clone();

        let first = s.start(&head).await;
        assert_eq!(first, IndexStatus::idle(&head, IndexState::Queued));
        // The job may have begun by now; it is the same one.
        let second = s.start(&head).await;
        assert_eq!(second.commit, head);
        assert!(matches!(
            second.state,
            IndexState::Queued | IndexState::Building
        ));
        assert_eq!(s.jobs(), 1);
        assert_eq!(s.end(&head).await.state, IndexState::Ready);

        // A complete commit is ready at once, without a job.
        assert_eq!(
            s.start(&head).await,
            IndexStatus::idle(&head, IndexState::Ready)
        );
        assert_eq!(s.jobs(), 1);

        // A server that starts later knows it from the database.
        let later = Indexer::new(s.db.clone(), Limits::default());
        assert_eq!(
            later.status(&s.clone.repo, &head).await.unwrap(),
            IndexStatus::idle(&head, IndexState::Ready)
        );
        assert_eq!(
            later.cancel(&s.clone.repo, &head).await.unwrap(),
            IndexStatus::idle(&head, IndexState::Ready)
        );
        // The other commit was never built.
        let base = s.clone.base.clone();
        assert_eq!(
            s.status(&base).await,
            IndexStatus::idle(&base, IndexState::Missing)
        );
    }

    #[tokio::test]
    async fn a_job_that_is_ready_is_forgotten_when_the_next_one_starts() {
        let s = Setup::new("index-forget").await;
        let (base, head) = (s.clone.base.clone(), s.clone.head.clone());

        assert_eq!(s.build(&base).await.state, IndexState::Ready);
        assert_eq!(s.jobs(), 1);
        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        // The base's job is gone, and the database tells that the base is ready.
        assert_eq!(s.jobs(), 1);
        assert_eq!(
            s.status(&base).await,
            IndexStatus::idle(&base, IndexState::Ready)
        );
    }

    #[test]
    fn finished_jobs_are_forgotten_if_they_are_ready_or_there_are_too_many_to_keep() {
        let key = |n: usize| (PathBuf::from("repo.git"), format!("commit-{n:03}"));
        let made = |outcome: Option<Result<(), Stop>>| {
            let job = Job::new("commit");
            if let Some(outcome) = outcome {
                job.finish(outcome);
            }
            Arc::new(job)
        };
        let failed = || Some(Err(Stop::Failed("no".to_string())));
        let kept = |jobs: &HashMap<(PathBuf, String), Arc<Job>>| -> Vec<usize> {
            let mut kept: Vec<usize> = jobs
                .keys()
                .map(|(_, commit)| commit.trim_start_matches("commit-").parse().unwrap())
                .collect();
            kept.sort();
            kept
        };

        // Jobs that have not finished are kept, and so are a few that failed or were cancelled; a ready one is not.
        let mut jobs = HashMap::new();
        jobs.insert(key(0), made(None));
        jobs.insert(key(1), made(Some(Ok(()))));
        jobs.insert(key(2), made(failed()));
        jobs.insert(key(3), made(Some(Err(Stop::Cancelled))));
        let building = made(None);
        building.begin();
        jobs.insert(key(4), building);
        forget_finished(&mut jobs);
        assert_eq!(kept(&jobs), [0, 2, 3, 4]);

        // Up to a limit of them stay...
        for n in 10..10 + FINISHED_JOBS_KEPT - 2 {
            jobs.insert(key(n), made(failed()));
        }
        forget_finished(&mut jobs);
        let finished = |jobs: &HashMap<(PathBuf, String), Arc<Job>>| {
            jobs.values().filter(|job| !job.active()).count()
        };
        assert_eq!(finished(&jobs), FINISHED_JOBS_KEPT);
        // ...and past it the oldest go, whatever their commits are called, while the newest and the others stay.
        jobs.insert(key(9), made(failed()));
        forget_finished(&mut jobs);
        assert_eq!(finished(&jobs), FINISHED_JOBS_KEPT);
        assert!(!jobs.contains_key(&key(2)));
        assert!(jobs.contains_key(&key(9)) && jobs.contains_key(&key(3)));
        jobs.insert(key(8), made(failed()));
        jobs.insert(key(7), made(failed()));
        forget_finished(&mut jobs);
        assert_eq!(finished(&jobs), FINISHED_JOBS_KEPT);
        assert!(!jobs.contains_key(&key(3)) && !jobs.contains_key(&key(10)));
        assert!(jobs.contains_key(&key(7)) && jobs.contains_key(&key(11)));
        assert!(jobs.contains_key(&key(0)) && jobs.contains_key(&key(4)));
    }

    #[tokio::test]
    async fn a_failed_job_says_why_and_is_replaced_by_the_next_start() {
        let s = Setup::new("index-failed").await;
        let head = s.clone.head.clone();
        let gone = s.clone.dir.join("origin-gone");
        std::fs::rename(&s.clone.origin, &gone).unwrap();

        s.start(&head).await;
        let failed = s.end(&head).await;
        assert_eq!((failed.state, failed.phase), (IndexState::Failed, None));
        let message = failed.message.clone().unwrap();
        assert!(message.starts_with("git fetch origin failed"), "{message}");
        // It is reported until a new job replaces it, and cancelling it changes nothing.
        assert_eq!(s.status(&head).await, failed);
        assert_eq!(s.cancel(&head).await, failed);
        assert!(!s.db.commit_complete(&head).unwrap());

        std::fs::rename(&gone, &s.clone.origin).unwrap();
        assert_eq!(
            s.start(&head).await,
            IndexStatus::idle(&head, IndexState::Queued)
        );
        assert_eq!(
            s.end(&head).await,
            IndexStatus::idle(&head, IndexState::Ready)
        );
        assert_eq!(s.jobs(), 1);
    }

    #[tokio::test]
    async fn a_commit_with_too_many_source_files_fails_before_fetching_anything() {
        let limits = Limits {
            max_files: 3,
            ..Limits::default()
        };
        let s = Setup::with_limits("index-too-many", limits).await;
        let head = s.clone.head.clone();
        let before = s.packs().await;

        let failed = s.build(&head).await;
        assert_eq!(failed.state, IndexState::Failed);
        assert_eq!(
            failed.message.as_deref(),
            Some("the commit has 4 source files; the index covers at most 3")
        );
        assert_eq!(s.packs().await, before);
        assert!(!s.db.commit_complete(&head).unwrap());
    }

    #[tokio::test]
    async fn a_queued_job_leaves_the_queue_when_it_is_cancelled() {
        let s = Setup::new("index-queue").await;
        let gate = Gate::install(&s.clone).await;
        let (base, head) = (s.clone.base.clone(), s.clone.head.clone());

        // The head holds the turn, in a fetch that does not end.
        s.start(&head).await;
        gate.begun().await;
        assert_eq!(
            s.start(&base).await,
            IndexStatus::idle(&base, IndexState::Queued)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            s.status(&base).await,
            IndexStatus::idle(&base, IndexState::Queued)
        );

        let cancelling = s.cancel(&base).await;
        assert_eq!(
            cancelling,
            IndexStatus {
                cancelling: true,
                ..IndexStatus::idle(&base, IndexState::Queued)
            }
        );
        let ended = s.end(&base).await;
        assert_eq!(
            (ended.state, ended.cancelling),
            (IndexState::Cancelled, false)
        );
        // It did not wait for the turn, and the job that has it is untouched.
        assert_eq!(s.status(&head).await.state, IndexState::Building);

        gate.open();
        assert_eq!(s.end(&head).await.state, IndexState::Ready);
    }

    #[tokio::test]
    async fn draining_cancels_the_jobs_and_waits_for_the_fetch_that_has_begun() {
        let s = Setup::with_limits("index-drain", limits_of_chunk(1)).await;
        let gate = Gate::install(&s.clone).await;
        let head = s.clone.head.clone();
        let before = s.packs().await;

        s.start(&head).await;
        gate.begun().await;
        let indexer = s.indexer.clone();
        let drain = tokio::spawn(async move { indexer.drain(PATIENCE).await });
        s.until(&head, |status| status.cancelling).await;
        // The fetch is under way: the server must not exit yet.
        assert!(!drain.is_finished());

        gate.open();
        drain.await.unwrap();
        let ended = s.status(&head).await;
        assert_eq!(
            (ended.state, ended.cancelling, ended.done),
            (IndexState::Cancelled, false, 1)
        );
        let after = s.packs().await;
        assert_eq!((after.0 - before.0, after.1 - before.1), (1, 1));
    }

    #[tokio::test]
    async fn draining_stops_a_job_that_does_not_end_in_time() {
        let s = Setup::new("index-drain-late").await;
        let gate = Gate::install(&s.clone).await;
        let head = s.clone.head.clone();

        s.start(&head).await;
        gate.begun().await;
        s.indexer.drain(Duration::from_millis(50)).await;
        let ended = s.status(&head).await;
        assert_eq!(ended.state, IndexState::Failed);
        assert_eq!(ended.message.as_deref(), Some("the indexing task stopped"));
        // Let the fetch that is left over end, and wait for the task that owns the repo lock to let it go.
        gate.open();
        drop(s.clone.repo.lock().await.unwrap());
    }

    #[tokio::test]
    async fn a_shut_down_cancels_the_jobs_and_fails_the_ones_that_start_after_it() {
        let s = Setup::new("index-shut-down").await;
        let gate = Gate::install(&s.clone).await;
        let (base, head) = (s.clone.base.clone(), s.clone.head.clone());

        // The head holds the turn, in a fetch that does not end, and the base waits for it.
        s.start(&head).await;
        gate.begun().await;
        s.start(&base).await;

        s.indexer.shut_down();
        // The job in the queue leaves it cancelled, though the turn is closed too; the other one stops at its next check.
        let queued = s.end(&base).await;
        assert_eq!(
            (queued.state, queued.cancelling, queued.message),
            (IndexState::Cancelled, false, None)
        );
        assert!(s.status(&head).await.cancelling);

        // A job that starts now is refused, with its reason, instead of waiting for a turn that does not come.
        s.start(&base).await;
        let refused = s.end(&base).await;
        assert_eq!(refused.state, IndexState::Failed);
        assert_eq!(
            refused.message.as_deref(),
            Some("indexing is shutting down")
        );

        gate.open();
        s.indexer.drain(PATIENCE).await;
        assert_eq!(s.status(&head).await.state, IndexState::Cancelled);
        assert!(!s.db.commit_complete(&head).unwrap());
    }

    #[tokio::test]
    async fn headers_and_blobs_with_two_names_are_indexed_for_each_name() {
        let add = "int add(int a, int b) { return a + b; }\n";
        let clone = PartialClone::with_files(
            "index-names",
            &[("README.md", "readme\n")],
            &[
                ("include/add.h", "int add(int a, int b);\n"),
                ("include/shape.h", "class Shape { public: int sides; };\n"),
                ("src/add.c", add),
                ("src/add.cc", add),
                ("web/node_modules/dep/index.js", "function dep() {}\n"),
                ("web/app.min.js", "function minified() {}\n"),
                ("web/app.js", "function app() {}\n"),
            ],
        )
        .await;
        let s = Setup::over(clone, Limits::default()).await;
        let head = s.clone.head.clone();

        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        // A `.h` file is C or C++ by its content, a blob with two names is stored once for each grammar, and the
        // paths the index leaves out are not there.
        assert_eq!(
            s.listing(&head),
            files(&[
                ("include/add.h", "c"),
                ("include/shape.h", "cpp"),
                ("src/add.c", "c"),
                ("src/add.cc", "cpp"),
                ("web/app.js", "javascript"),
            ])
        );
        let places = |name: &str, from: &str| -> Vec<String> {
            s.db.definitions(&head, Family::C, name, from)
                .unwrap()
                .unwrap()
                .0
                .into_iter()
                .map(|hit| hit.path)
                .collect()
        };
        // C and C++ see each other.
        assert_eq!(places("Shape", "src/add.c"), ["include/shape.h"]);
        assert_eq!(places("add", "src/add.c"), ["src/add.c", "src/add.cc"]);
    }

    #[tokio::test]
    async fn a_header_stored_as_c_and_as_cpp_is_listed_by_its_content() {
        let cpp = "class Shape { public: int sides; };\n";
        let c = "int sides;\n";
        // Each header has the content of a `.c` and of a `.cc` file, so its blob is stored under both grammars.
        let clone = PartialClone::with_files(
            "index-header-content",
            &[("README.md", "readme\n")],
            &[
                ("a.c", cpp),
                ("b.cc", cpp),
                ("c.c", c),
                ("d.cc", c),
                ("include/plain.h", c),
                ("include/shape.h", cpp),
            ],
        )
        .await;
        // Each header is read in a group of its own to be looked at.
        let limits = Limits {
            group_bytes: 1,
            ..Limits::default()
        };
        let s = Setup::over(clone, limits).await;
        let head = s.clone.head.clone();

        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        assert_eq!(
            s.listing(&head),
            files(&[
                ("a.c", "c"),
                ("b.cc", "cpp"),
                ("c.c", "c"),
                ("d.cc", "cpp"),
                ("include/plain.h", "c"),
                ("include/shape.h", "cpp"),
            ])
        );
    }

    #[tokio::test]
    async fn a_file_that_takes_too_long_to_parse_does_not_keep_the_commit_from_being_indexed() {
        // A definition and then a long run of `+` after an `=`, which takes tree-sitter far longer than a file is given.
        let slow = format!("function f() {{}}\nx = {}", "+".repeat(300_000));
        let clone = PartialClone::with_files(
            "index-slow",
            &[("README.md", "readme\n")],
            &[("fine.js", "function g() {}\n"), ("slow.js", &slow)],
        )
        .await;
        let s = Setup::over(clone, Limits::default()).await;
        let head = s.clone.head.clone();

        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        assert_eq!(
            s.listing(&head),
            files(&[("fine.js", "javascript"), ("slow.js", "javascript")])
        );
        // The slow file is noted as unreadable, which the next build tries again, and not as a file without tags.
        let slow_oid = s.clone.oid(&head, "slow.js").await;
        let fine_oid = s.clone.oid(&head, "fine.js").await;
        let slow =
            s.db.blob_symbols(&slow_oid, Grammar::JavaScript)
                .unwrap()
                .unwrap();
        assert_eq!(
            (slow.status, slow.symbols.len()),
            (BlobStatus::Unreadable, 0)
        );
        let fine =
            s.db.blob_symbols(&fine_oid, Grammar::JavaScript)
                .unwrap()
                .unwrap();
        assert_eq!((fine.status, fine.symbols.len()), (BlobStatus::Parsed, 1));
    }
}
