//! Helpers for tests of the pull request service.

use std::path::PathBuf;
use std::sync::Arc;

use super::PrService;
use crate::git::Repos;
use crate::git::testing::{bare_repo_with_blobs, temp_dir};
use crate::index::db::IndexDb;

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

/// A blob nobody has: a valid OID that no repo holds.
pub(crate) const UNKNOWN: &str = "1234567890123456789012345678901234567891";

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
