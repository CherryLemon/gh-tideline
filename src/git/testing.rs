//! Helpers for tests that need real git repositories.

use std::path::{Path, PathBuf};

use super::run_git;

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
