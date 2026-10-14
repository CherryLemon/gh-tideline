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
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinSet;

use crate::git::{GitAuth, Repo};
use build::{Build, Limits, Stop};
use db::IndexDb;

const JOBS_POISONED: &str = "index jobs poisoned";

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

    /// Whether the job has not finished.
    fn active(&self) -> bool {
        matches!(
            self.progress().state,
            IndexState::Queued | IndexState::Building
        )
    }

    /// Whether the job ended in a way that a new one should replace.
    fn replaceable(&self) -> bool {
        matches!(
            self.progress().state,
            IndexState::Failed | IndexState::Cancelled
        )
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

/// The index database and the jobs that fill it.
///
/// One commit is built at a time: builds compete for the network and the processors, and a second one would only slow
/// the first. The others are `queued`.
pub struct Indexer {
    db: IndexDb,
    limits: Limits,
    /// Held by the job that builds.
    turn: Arc<Semaphore>,
    /// By cache repo and commit. A finished job stays until a new one replaces it, so its outcome can be asked for.
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
    /// the first one's). A job that failed or was cancelled is replaced. `auth` is for fetching blobs.
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
        let mut tasks = self.tasks.lock().expect("index tasks poisoned");
        while tasks.try_join_next().is_some() {}
        tasks.spawn(build.run(self.turn.clone()));
        Ok(job.status())
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

    /// Cancels every job and waits (up to `timeout`) for them to stop, so that the git processes they started finish
    /// before the server exits.
    pub async fn drain(&self, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        for job in self.jobs.lock().expect(JOBS_POISONED).values() {
            if job.active() {
                job.cancel();
            }
        }
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
    use crate::git::testing::{PartialClone, git_in};
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
            let deadline = Instant::now() + PATIENCE;
            loop {
                let status = self.status(commit).await;
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
            self.until(commit, |status| {
                !matches!(status.state, IndexState::Queued | IndexState::Building)
            })
            .await
        }

        /// Builds `commit` to the end.
        async fn build(&self, commit: &str) -> IndexStatus {
            self.start(commit).await;
            self.end(commit).await
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

    /// Holds back the fetches of the cache repo: each reports that it has begun, and waits for the gate to open. That
    /// lets a test act while a fetch is under way.
    struct Gate {
        begun: PathBuf,
        open: PathBuf,
    }

    impl Gate {
        async fn install(clone: &PartialClone) -> Gate {
            let begun = clone.dir.join("gate-begun");
            let open = clone.dir.join("gate-open");
            let script = clone.dir.join("gate.sh");
            // A test that fails leaves nothing behind: the loop also ends when its directory is removed.
            std::fs::write(
                &script,
                format!(
                    "touch '{}'\nwhile [ ! -e '{}' ] && [ -d '{}' ]; do sleep 0.01; done\nexec git upload-pack \"$@\"\n",
                    begun.display(),
                    open.display(),
                    clone.dir.display()
                ),
            )
            .unwrap();
            let command = format!("sh {}", script.display());
            git_in(
                &clone.repo.git_dir,
                &["config", "remote.origin.uploadpack", &command],
            )
            .await;
            Gate { begun, open }
        }

        /// Waits until a fetch has begun.
        async fn begun(&self) {
            let deadline = Instant::now() + PATIENCE;
            while !self.begun.exists() {
                assert!(Instant::now() < deadline, "no fetch began");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }

        fn open(&self) {
            std::fs::write(&self.open, "").unwrap();
        }
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

        // The job waits for its first turn on the runtime.
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
        let hits =
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
    async fn blobs_the_index_holds_are_neither_fetched_nor_parsed_again() {
        let s = Setup::new("index-held").await;
        let head = s.clone.head.clone();
        let helpers = s.clone.oid(&head, "src/util/helpers.rs").await;
        // A row the parser would not make: the build must leave it as it is.
        s.db.insert_blobs(vec![NewBlob {
            oid: helpers.clone(),
            grammar: Grammar::Rust,
            status: BlobStatus::Parsed,
            size: 17,
            tags: vec![kept_tag()],
        }])
        .unwrap();

        assert_eq!(s.build(&head).await.state, IndexState::Ready);
        let missing = s.clone.repo.missing_objects(&head).await.unwrap();
        assert!(missing.contains(&helpers));
        let stored = s.db.blob_symbols(&helpers, Grammar::Rust).unwrap().unwrap();
        let names: Vec<&str> = stored.symbols.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, ["kept"]);
        assert!(
            s.listing(&head)
                .contains(&("src/util/helpers.rs".to_string(), "rust".to_string()))
        );
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
        // Let the fetch that is left over end.
        gate.open();
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
                .into_iter()
                .map(|hit| hit.path)
                .collect()
        };
        // C and C++ see each other.
        assert_eq!(places("Shape", "src/add.c"), ["include/shape.h"]);
        assert_eq!(places("add", "src/add.c"), ["src/add.c", "src/add.cc"]);
    }
}
