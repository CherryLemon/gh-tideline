//! Helpers for tests that need real git repositories.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{GitAuth, Repo, STALL_INDEX, run_git};

/// A valid OID that no repo holds.
pub(crate) const UNKNOWN: &str = "1234567890123456789012345678901234567891";

/// A new, empty directory in the system temp dir. The test removes it.
pub(crate) fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "gh-tideline-test-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs git in `dir` and returns its trimmed output.
pub(crate) async fn git_in(dir: &Path, args: &[&str]) -> String {
    let mut full = vec!["-C", dir.to_str().unwrap()];
    full.extend_from_slice(args);
    let out = run_git(None, None, &full, None).await.unwrap();
    String::from_utf8(out).unwrap().trim().to_string()
}

/// Creates a bare repo at `git_dir` that holds `contents` as blobs, and returns their OIDs.
pub(crate) async fn bare_repo_with_blobs(git_dir: &Path, contents: &[&[u8]]) -> Vec<String> {
    std::fs::create_dir_all(git_dir).unwrap();
    git_in(git_dir, &["init", "--bare", "-q"]).await;
    let mut oids = Vec::new();
    for content in contents {
        let out = run_git(
            Some(git_dir),
            None,
            &["hash-object", "-w", "--stdin"],
            Some(content.to_vec()),
        )
        .await
        .unwrap();
        oids.push(String::from_utf8(out).unwrap().trim().to_string());
    }
    oids
}

/// Commits everything in the work tree `dir` and returns the commit.
pub(crate) async fn commit_all(dir: &Path, message: &str) -> String {
    git_in(dir, &["add", "-A"]).await;
    git_in(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            message,
        ],
    )
    .await;
    git_in(dir, &["rev-parse", "HEAD"]).await
}

fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }
}

/// Creates a work-tree repo with the given files committed; returns the head commit.
pub(crate) async fn make_repo(dir: &Path, files: &[(&str, &str)]) -> String {
    git_in(dir, &["init", "-q"]).await;
    write_files(dir, files);
    commit_all(dir, "init").await
}

pub(crate) fn repo_at(git_dir: PathBuf, remote_url: String) -> Repo {
    Repo {
        git_dir,
        remote_url,
        shared: Default::default(),
    }
}

pub(crate) const LIB_V1: &str = "pub fn one() {}\n";
pub(crate) const LIB_V2: &str = "pub fn one() {}\npub fn two() {}\n";
pub(crate) const HELPERS: &str = "pub fn help() {}\n";
pub(crate) const BINARY: &[u8] = b"\0\x01\xff\xfe\r\n\n\0tail";

/// Over 1 MiB, the size from which the index leaves a file out.
pub(crate) fn big_rs() -> String {
    "// filler line\n".repeat(80_000)
}

/// A source repo with two commits and the blob-less cache repo a pull request from the first
/// (`base`) to the second (`head`) leaves behind: both commits and their trees, no blobs.
/// Besides plain files the commits hold a path with a space, a subdirectory, two paths with
/// one content, a binary file, a file over 1 MiB and a gitlink.
///
/// The cache repo is where `Repos::new(dir/repos)` looks for `github.com/o/r`, and it fetches from the
/// source repo. The directory is removed when the value is dropped.
pub(crate) struct PartialClone {
    pub(crate) dir: PathBuf,
    pub(crate) origin: PathBuf,
    pub(crate) repo: Arc<Repo>,
    pub(crate) auth: GitAuth,
    pub(crate) base: String,
    pub(crate) head: String,
}

impl PartialClone {
    pub(crate) async fn new(name: &str) -> PartialClone {
        let dir = temp_dir(name);
        let origin = dir.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        let base = make_repo(
            &origin,
            &[
                ("README.md", "readme\n"),
                ("src/lib.rs", LIB_V1),
                ("src/util/helpers.rs", HELPERS),
                ("dir with space/a b.py", "print('a b')\n"),
                ("same-1.txt", "same\n"),
                ("same-2.txt", "same\n"),
            ],
        )
        .await;
        std::fs::write(origin.join("src/lib.rs"), LIB_V2).unwrap();
        std::fs::write(origin.join("big.rs"), big_rs()).unwrap();
        std::fs::write(origin.join("data.bin"), BINARY).unwrap();
        std::fs::remove_file(origin.join("README.md")).unwrap();
        git_in(&origin, &["add", "-A"]).await;
        let sub = "1234567890123456789012345678901234567890";
        git_in(
            &origin,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sub},vendor/lib"),
            ],
        )
        .await;
        git_in(
            &origin,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-qm",
                "second",
            ],
        )
        .await;
        let head = git_in(&origin, &["rev-parse", "HEAD"]).await;
        PartialClone::cache(dir, origin, base, head).await
    }

    /// Like `new`, but the two commits hold just these files.
    pub(crate) async fn with_files(
        name: &str,
        base: &[(&str, &str)],
        head: &[(&str, &str)],
    ) -> PartialClone {
        let dir = temp_dir(name);
        let origin = dir.join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        let base_commit = make_repo(&origin, base).await;
        for entry in std::fs::read_dir(&origin).unwrap() {
            let path = entry.unwrap().path();
            if path.ends_with(".git") {
                continue;
            }
            if path.is_dir() {
                std::fs::remove_dir_all(path).unwrap();
            } else {
                std::fs::remove_file(path).unwrap();
            }
        }
        write_files(&origin, head);
        let head_commit = commit_all(&origin, "second").await;
        PartialClone::cache(dir, origin, base_commit, head_commit).await
    }

    /// The cache repo for the commits `base` and `head` of the source repo `origin`.
    async fn cache(dir: PathBuf, origin: PathBuf, base: String, head: String) -> PartialClone {
        git_in(&origin, &["config", "uploadpack.allowFilter", "true"]).await;
        git_in(
            &origin,
            &["config", "uploadpack.allowAnySHA1InWant", "true"],
        )
        .await;

        let repo = Arc::new(repo_at(
            dir.join("repos/github.com/o/r.git"),
            format!("file://{}", origin.display()),
        ));
        let auth = GitAuth {
            host: "example.invalid".into(),
            token: "unused".into(),
        };
        let guard = repo.lock().await.unwrap();
        repo.fetch_commits(&guard, &auth, 1, &base, &head)
            .await
            .unwrap();
        drop(guard);
        PartialClone {
            dir,
            origin,
            repo,
            auth,
            base,
            head,
        }
    }

    /// A second cache repo over the same source repo, as a clone made anew has: both commits and their trees, no blobs.
    pub(crate) async fn another_cache(&self) -> Arc<Repo> {
        let repo = Arc::new(repo_at(
            self.dir.join("repos/github.com/o/fork.git"),
            format!("file://{}", self.origin.display()),
        ));
        let guard = repo.lock().await.unwrap();
        repo.fetch_commits(&guard, &self.auth, 1, &self.base, &self.head)
            .await
            .unwrap();
        drop(guard);
        repo
    }

    /// The OID of `path` in `commit`, from the source repo.
    pub(crate) async fn oid(&self, commit: &str, path: &str) -> String {
        git_in(&self.origin, &["rev-parse", &format!("{commit}:{path}")]).await
    }

    /// The blobs of `commit`'s tree (not its gitlinks), from the source repo.
    pub(crate) async fn blobs(&self, commit: &str) -> HashSet<String> {
        let listing = git_in(
            &self.origin,
            &[
                "ls-tree",
                "-r",
                "--format=%(objecttype) %(objectname)",
                commit,
            ],
        )
        .await;
        listing
            .lines()
            .filter_map(|line| line.strip_prefix("blob "))
            .map(String::from)
            .collect()
    }

    /// Fetches blobs the way the index build does.
    pub(crate) async fn fetch(&self, oids: &[&str]) {
        let oids: Vec<String> = oids.iter().map(|oid| oid.to_string()).collect();
        let guard = self.repo.lock().await.unwrap();
        self.repo
            .fetch_blobs(&guard, &self.auth, &oids, STALL_INDEX)
            .await
            .unwrap();
    }
}

impl Drop for PartialClone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
