//! Helpers for tests of the pull request service.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::nav::Side;
use super::{Cached, PrData, PrService, PrSummary, RefInfo};
use crate::git::Repos;
use crate::git::testing::{PartialClone, bare_repo_with_blobs, temp_dir};
use crate::index::db::IndexDb;
use crate::index::{IndexState, IndexStatus};
use crate::state::PrKey;

pub(crate) use crate::git::testing::UNKNOWN;

/// A Rust file with a struct, an `impl` block with a method, and a function: four symbols.
pub(crate) const RUST: &str = "use std::fmt;

pub struct Parser {
    x: u32,
}

impl Parser {
    pub fn new() -> Self {
        Parser { x: 1 }
    }
}

fn helper() {
    Parser::new();
}
";

/// The cache repo of `github.com/o/r` holding some blobs, served by a service that has an index.
/// The directory is removed when the fixture is dropped.
pub(crate) struct Fixture {
    root: PathBuf,
    pub(crate) service: Arc<PrService>,
    pub(crate) db: IndexDb,
    /// The OIDs of the blobs, in the order they were given.
    pub(crate) oids: Vec<String>,
}

impl Fixture {
    pub(crate) async fn new(name: &str, contents: &[&[u8]]) -> Fixture {
        let root = temp_dir(name);
        let oids = bare_repo_with_blobs(&root.join("repos/github.com/o/r.git"), contents).await;
        let db = IndexDb::open(&root.join("index.sqlite")).unwrap();
        let service = Arc::new(PrService::new(
            Repos::new(root.join("repos")),
            None,
            Some(db.clone()),
        ));
        Fixture {
            root,
            service,
            db,
            oids,
        }
    }

    /// A service over the same repos that has no index.
    pub(crate) fn without_index(&self) -> Arc<PrService> {
        Arc::new(PrService::new(
            Repos::new(self.root.join("repos")),
            None,
            None,
        ))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Pull request 1 of `github.com/o/r`, whose cache repo is a [`PartialClone`]: base and head are its two commits. The
/// service has an index. GitHub is never asked: the pull request is put in the cache by hand ([`Pull::at`]) and the
/// token is made up. The directory is removed when the fixture is dropped.
pub(crate) struct Pull {
    pub(crate) clone: PartialClone,
    pub(crate) service: Arc<PrService>,
    pub(crate) db: IndexDb,
    pub(crate) key: PrKey,
}

impl Pull {
    /// A pull request at the clone's head, whose merge base is the clone's base.
    pub(crate) async fn new(name: &str) -> Pull {
        Pull::over(PartialClone::new(name).await).await
    }

    /// Like `new`, but the base and the head hold just these files.
    pub(crate) async fn with_files(
        name: &str,
        base: &[(&str, &str)],
        head: &[(&str, &str)],
    ) -> Pull {
        Pull::over(PartialClone::with_files(name, base, head).await).await
    }

    async fn over(clone: PartialClone) -> Pull {
        let db = IndexDb::open(&clone.dir.join("index.sqlite")).unwrap();
        let mut service =
            PrService::new(Repos::new(clone.dir.join("repos")), None, Some(db.clone()));
        // A fetch of a lookup takes what a loaded machine gives it, and so does each ask of its task when the remote
        // refuses blobs; the tests that watch the wait and the budget shorten them.
        service.snippet_fetch_wait = Duration::from_secs(60);
        service.snippet_fetch_budget = Duration::from_secs(60);
        service.tokens.insert("github.com", "unused");
        let service = Arc::new(service);
        let key = PrKey {
            host: "github.com".to_string(),
            owner: "o".to_string(),
            repo: "r".to_string(),
            number: 1,
        };
        let pull = Pull {
            clone,
            service,
            db,
            key,
        };
        pull.at(&pull.clone.head).await;
        pull
    }

    /// Makes the service think the pull request has the head `head`.
    pub(crate) async fn at(&self, head: &str) {
        let r = |name: &str, sha: &str| RefInfo {
            name: name.to_string(),
            sha: sha.to_string(),
        };
        let data = PrData {
            summary: PrSummary {
                host: self.key.host.clone(),
                owner: self.key.owner.clone(),
                repo: self.key.repo.clone(),
                number: self.key.number,
                title: "A pull request".to_string(),
                author: "someone".to_string(),
                state: "OPEN".to_string(),
                url: String::new(),
                base: r("main", &self.clone.base),
                head: r("topic", head),
                merge_base: self.clone.base.clone(),
                node_id: "PR_x".to_string(),
                viewer: "me".to_string(),
            },
            files: Vec::new(),
            viewed: Mutex::new(HashMap::new()),
            tree: Default::default(),
        };
        let now = Instant::now();
        *self.service.slot(&self.key).lock().await = Some(Cached {
            started: now,
            finished: now,
            data: Arc::new(data),
        });
    }

    /// Waits until the build of `side` has ended, and returns how it ended.
    pub(crate) async fn end(&self, head: &str, side: Side) -> IndexStatus {
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

    /// Builds the index of `side` to the end, and returns how the build ended.
    pub(crate) async fn build(&self, side: Side) -> IndexStatus {
        let head = self.clone.head.clone();
        let started = self.service.start_index(&self.key, &head, side).await;
        started.unwrap();
        self.end(&head, side).await
    }

    /// A service over the same repos that has no index.
    pub(crate) fn without_index(&self) -> Arc<PrService> {
        Arc::new(PrService::new(
            Repos::new(self.clone.dir.join("repos")),
            None,
            None,
        ))
    }
}
