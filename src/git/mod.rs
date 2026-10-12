//! Cache-repo management and git plumbing calls.
//!
//! Every git process gets an isolated environment: a cleared environment plus an allowlist
//! (see [`PASS_ENV`]), no system/global config, no terminal prompts or askpass helpers,
//! literal pathspecs, and, when a token is available, an `http.https://<host>/.extraHeader`
//! injected via `GIT_CONFIG_COUNT/KEY/VALUE` so the token never appears on a command line.
//!
//! Mutating git commands always run under [`Repo::lock`]: the in-process tokio mutex plus an
//! exclusive `flock` on `<repo>.git/gh-tideline-mutex`, which every gh-tideline server takes before it
//! runs git that writes to the repo. Git processes are never killed when a request is
//! dropped; callers detach mutating work into a task that owns the lock guard instead.

pub mod parse;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub use parse::ChangedFile;

/// Failure of a git invocation, with its (trimmed) stderr.
#[derive(Debug, Clone)]
pub struct GitFailure {
    pub command: String,
    pub stderr: String,
}

impl std::fmt::Display for GitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "git {} failed: {}", self.command, self.stderr)
    }
}

fn io_failure(command: &str, e: std::io::Error) -> GitFailure {
    GitFailure {
        command: command.into(),
        stderr: e.to_string(),
    }
}

/// Credentials for one host, injected into git through the environment.
#[derive(Clone)]
pub struct GitAuth {
    pub host: String,
    pub token: String,
}

/// `true` for a full lowercase SHA-1 (40 hex) or SHA-256 (64 hex) object id.
pub fn is_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Variables passed through to git from the server's environment (plus every `LC_*` except
/// `LC_ALL`/`LC_MESSAGES`, see [`isolated_git`]). Everything else is cleared, in particular
/// `GIT_*` overrides and `SSH_ASKPASS`/`DISPLAY`, so no password dialog can ever appear.
const PASS_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "TMPDIR",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "GIT_SSL_CAINFO",
    "GIT_SSL_CAPATH",
];

/// Mode for every directory under the cache and config dirs.
pub const PRIVATE_DIR_MODE: u32 = 0o700;

/// Creates `dir` (and missing parents) with mode 0700, and chmods it to 0700 if it exists.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(dir)?;
    set_private(dir)
}

fn set_private(dir: &Path) -> std::io::Result<()> {
    let mode = std::fs::metadata(dir)?.permissions().mode() & 0o777;
    if mode != PRIVATE_DIR_MODE {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE))?;
    }
    Ok(())
}

fn isolated_git(git_dir: Option<&Path>, auth: Option<&GitAuth>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(dir) = git_dir {
        cmd.arg("--git-dir").arg(dir);
    }
    cmd.env_clear();
    for key in PASS_ENV {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    for (key, value) in std::env::vars_os() {
        let k = key.to_string_lossy();
        if k.starts_with("LC_") && k != "LC_ALL" && k != "LC_MESSAGES" {
            cmd.env(key, value);
        }
    }
    // English messages: some git errors are classified by their stderr text.
    cmd.env("LC_MESSAGES", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        // Paths derived from PR contents are never pathspec magic (e.g. a directory `:!x`).
        .env("GIT_LITERAL_PATHSPECS", "1");
    match auth {
        Some(auth) => {
            let basic = base64::engine::general_purpose::STANDARD
                .encode(format!("x-access-token:{}", auth.token));
            cmd.env("GIT_CONFIG_COUNT", "1")
                .env(
                    "GIT_CONFIG_KEY_0",
                    format!("http.https://{}/.extraHeader", auth.host),
                )
                .env(
                    "GIT_CONFIG_VALUE_0",
                    format!("Authorization: Basic {basic}"),
                );
        }
        None => {
            cmd.env("GIT_CONFIG_COUNT", "0");
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Runs git with the isolation environment and returns stdout.
pub async fn run_git<S: AsRef<OsStr>>(
    git_dir: Option<&Path>,
    auth: Option<&GitAuth>,
    args: &[S],
    stdin: Option<Vec<u8>>,
) -> Result<Vec<u8>, GitFailure> {
    // First two words for error messages, skipping `-c key=value` pairs.
    let mut words = Vec::new();
    let mut iter = args.iter().map(|a| a.as_ref().to_string_lossy());
    while let Some(word) = iter.next() {
        if word == "-c" {
            iter.next();
        } else if words.len() < 2 {
            words.push(word);
        }
    }
    let command = words.join(" ");
    let fail = |stderr: String| GitFailure {
        command: command.clone(),
        stderr,
    };
    let mut cmd = isolated_git(git_dir, auth);
    cmd.args(args);
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| fail(format!("cannot spawn git: {e}")))?;
    let writer = match (stdin, child.stdin.take()) {
        (Some(input), Some(mut pipe)) => Some(tokio::spawn(async move {
            let _ = pipe.write_all(&input).await;
            // Dropping the pipe closes git's stdin.
        })),
        _ => None,
    };
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| fail(format!("waiting for git: {e}")))?;
    if let Some(w) = writer {
        let _ = w.await;
    }
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(fail(if stderr.is_empty() {
            format!("exit status {}", output.status)
        } else {
            stderr
        }))
    }
}

/// Checks that the installed git is at least 2.40 (needed for `check-attr --source`).
pub async fn check_git_version() -> Result<(), String> {
    let out = run_git::<&str>(None, None, &["--version"], None)
        .await
        .map_err(|e| format!("git is required: {e}"))?;
    let text = String::from_utf8_lossy(&out);
    let version = text
        .split_whitespace()
        .nth(2)
        .ok_or_else(|| format!("cannot parse git version from {text:?}"))?;
    let mut nums = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (nums.next().unwrap_or(0), nums.next().unwrap_or(0));
    if (major, minor) < (2, 40) {
        return Err(format!(
            "git {version} is too old; tideline needs git >= 2.40"
        ));
    }
    Ok(())
}

/// Mutable per-repo state, guarded by the repo's fetch mutex.
#[derive(Default)]
pub struct RepoState {
    /// `(merge_base, head)` pairs whose changed blobs have been fetched by this process.
    pub blobs_fetched: HashSet<(String, String)>,
    /// Whether this process has applied [`Repo::harden`] to the repo.
    hardened: bool,
}

/// Per-repo state shared by every [`Repo`] handle for the same directory.
#[derive(Default)]
struct RepoShared {
    state: Arc<tokio::sync::Mutex<RepoState>>,
    /// OIDs listed in pull requests opened by this process: `true` for blobs, `false` for
    /// gitlinks (submodule commits). Only these are ever lazily fetched by the blob endpoint.
    known: std::sync::Mutex<HashMap<String, bool>>,
}

/// Registry of cache repos and their per-repo state.
pub struct Repos {
    root: PathBuf,
    shared: std::sync::Mutex<HashMap<PathBuf, Arc<RepoShared>>>,
}

impl Repos {
    pub fn new(root: PathBuf) -> Self {
        Repos {
            root,
            shared: Default::default(),
        }
    }

    /// Path of the bare cache repo. Callers must validate host/owner/repo beforehand.
    pub fn repo(&self, host: &str, owner: &str, repo: &str) -> Repo {
        let git_dir = self.root.join(host).join(owner).join(format!("{repo}.git"));
        let shared = self
            .shared
            .lock()
            .expect("repo registry poisoned")
            .entry(git_dir.clone())
            .or_default()
            .clone();
        Repo {
            git_dir,
            remote_url: format!("https://{host}/{owner}/{repo}.git"),
            shared,
        }
    }

    /// Chmods `repos/`, `repos/<host>/`, `repos/<host>/<owner>/` and every
    /// `<repo>.git` directory to 0700 (earlier versions created them with the umask).
    pub fn secure_existing_dirs(&self) -> std::io::Result<()> {
        ensure_private_dir(&self.root)?;
        let subdirs = |dir: &Path| -> Vec<PathBuf> {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path())
                .collect()
        };
        for host in subdirs(&self.root) {
            set_private(&host)?;
            for owner in subdirs(&host) {
                set_private(&owner)?;
                for repo in subdirs(&owner) {
                    set_private(&repo)?;
                }
            }
        }
        Ok(())
    }

    /// Waits (up to `timeout`) until no repo lock is held, so that git processes started
    /// by detached tasks finish before the server exits.
    pub async fn drain(&self, timeout: Duration) {
        let all: Vec<Arc<RepoShared>> = self
            .shared
            .lock()
            .expect("repo registry poisoned")
            .values()
            .cloned()
            .collect();
        let _ = tokio::time::timeout(timeout, async {
            for shared in all {
                drop(shared.state.lock().await);
            }
        })
        .await;
    }
}

/// Exclusive access to a cache repo for mutating git commands. Owned (`'static`) so it can
/// move into a detached task; dropping it releases both the tokio mutex and the flock.
pub struct RepoGuard {
    pub state: tokio::sync::OwnedMutexGuard<RepoState>,
    _flock: std::fs::File,
}

/// Name of the flock file inside every cache repo (deliberately not `*.lock`).
const FLOCK_FILE: &str = "gh-tideline-mutex";

/// Git lock files at the top of the git dir that a killed git can leave behind.
fn is_lock_file(name: &OsStr) -> bool {
    name.to_str().is_some_and(|n| n.ends_with(".lock"))
}

/// Removes git lock files (`shallow.lock`, `packed-refs.lock`, `HEAD.lock`, `config.lock`,
/// `index.lock`, ... at the top level and `refs/**/*.lock`) left by a git process that was
/// killed mid-operation; they would otherwise make every later fetch fail.
///
/// Only call this while holding the repo's exclusive flock ([`Repo::lock`]): every gh-tideline
/// server runs mutating git only under that flock, and the server waits for its git
/// children before exiting, so any lock file seen then is stale. (Running git by hand in
/// the cache repo while a gh-tideline server uses it is not supported.)
pub fn remove_stale_locks(git_dir: &Path) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut remove = |path: PathBuf| {
        if std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    };
    for entry in std::fs::read_dir(git_dir).into_iter().flatten().flatten() {
        if is_lock_file(&entry.file_name()) && entry.file_type().is_ok_and(|t| t.is_file()) {
            remove(entry.path());
        }
    }
    let mut stack = vec![git_dir.join("refs")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(t) if t.is_file() && is_lock_file(&entry.file_name()) => remove(entry.path()),
                _ => {}
            }
        }
    }
    removed
}

/// A blob read from the cache repo.
pub enum ObjectLookup {
    /// The object exists: its type (`blob`, `tree`, `commit`, `tag`) and contents.
    Found { kind: String, data: Vec<u8> },
    /// Not available (locally, or from the promisor remote when lazy fetch was allowed).
    Missing,
}

pub struct Repo {
    pub git_dir: PathBuf,
    remote_url: String,
    shared: Arc<RepoShared>,
}

impl Repo {
    pub fn exists(&self) -> bool {
        self.git_dir.join("HEAD").is_file()
    }

    async fn git<S: AsRef<OsStr>>(
        &self,
        auth: Option<&GitAuth>,
        args: &[S],
        stdin: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, GitFailure> {
        run_git(Some(&self.git_dir), auth, args, stdin).await
    }

    /// Takes the repo lock (creating the repo if needed): the tokio mutex, then an exclusive
    /// flock shared with other gh-tideline processes. Then removes stale git lock files and, once
    /// per process, hardens the repo's permissions.
    pub async fn lock(&self) -> Result<RepoGuard, GitFailure> {
        let mut state = self.shared.state.clone().lock_owned().await;
        self.ensure_init().await?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.git_dir.join(FLOCK_FILE))
            .map_err(|e| io_failure("lock", e))?;
        let flock = tokio::task::spawn_blocking(move || file.lock().map(|()| file))
            .await
            .map_err(|e| io_failure("lock", std::io::Error::other(e)))?
            .map_err(|e| io_failure("lock", e))?;
        let dir = self.git_dir.clone();
        let removed = tokio::task::spawn_blocking(move || remove_stale_locks(&dir))
            .await
            .unwrap_or_default();
        for path in removed {
            eprintln!(
                "gh-tideline: removed stale git lock file {}",
                path.display()
            );
        }
        if !state.hardened {
            self.harden().await?;
            state.hardened = true;
        }
        Ok(RepoGuard {
            state,
            _flock: flock,
        })
    }

    /// Mode 0700 on the repo directory and `core.sharedRepository=0600`, so files git
    /// creates from now on are private regardless of the umask.
    async fn harden(&self) -> Result<(), GitFailure> {
        set_private(&self.git_dir).map_err(|e| io_failure("chmod", e))?;
        let current = self
            .git(None, &["config", "core.sharedRepository"], None)
            .await
            .unwrap_or_default();
        if current.trim_ascii() != b"0600" {
            self.git(None, &["config", "core.sharedRepository", "0600"], None)
                .await?;
        }
        Ok(())
    }

    /// Creates the bare partial-clone cache repo if needed. It is set up in a temporary
    /// directory and renamed into place so a half-initialised repo is never observed.
    async fn ensure_init(&self) -> Result<(), GitFailure> {
        if self.exists() {
            return Ok(());
        }
        let parent = self.git_dir.parent().expect("repo path has a parent");
        ensure_private_dir(parent).map_err(|e| io_failure("init", e))?;
        let tmp = self
            .git_dir
            .with_extension(format!("git.tmp-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        // --shared=0600: directories 0700, files 0600, whatever the umask.
        run_git(
            None,
            None,
            &[
                OsStr::new("init"),
                OsStr::new("--bare"),
                OsStr::new("-q"),
                OsStr::new("--shared=0600"),
                OsStr::new("--"),
                tmp.as_os_str(),
            ],
            None,
        )
        .await?;
        for (key, value) in [
            ("remote.origin.url", self.remote_url.as_str()),
            ("gc.auto", "0"),
            ("core.repositoryformatversion", "1"),
            ("extensions.partialClone", "origin"),
            ("remote.origin.promisor", "true"),
            ("remote.origin.partialCloneFilter", "blob:none"),
        ] {
            run_git(Some(&tmp), None, &["config", key, value], None).await?;
        }
        match tokio::fs::rename(&tmp, &self.git_dir).await {
            Ok(()) => Ok(()),
            // Another process won the race; its repo is equally valid.
            Err(_) if self.exists() => {
                let _ = tokio::fs::remove_dir_all(&tmp).await;
                Ok(())
            }
            Err(e) => Err(io_failure("init", e)),
        }
    }

    /// Returns the commits currently stored under `refs/dr/pull/<n>/{base,head}`.
    pub async fn pull_refs(&self, number: u64) -> Option<(String, String)> {
        let base = format!("refs/dr/pull/{number}/base");
        let head = format!("refs/dr/pull/{number}/head");
        let out = self
            .git(None, &["rev-parse", "--verify", "-q", &base], None)
            .await
            .ok()?;
        let out_head = self
            .git(None, &["rev-parse", "--verify", "-q", &head], None)
            .await
            .ok()?;
        Some((
            String::from_utf8_lossy(&out).trim().to_string(),
            String::from_utf8_lossy(&out_head).trim().to_string(),
        ))
    }

    /// Step 3: shallow, blob-less fetch of the merge base and head into PR refs.
    /// `merge_base` and `head` must have been validated with [`is_oid`].
    pub async fn fetch_commits(
        &self,
        _guard: &RepoGuard,
        auth: &GitAuth,
        number: u64,
        merge_base: &str,
        head: &str,
    ) -> Result<(), GitFailure> {
        let base_spec = format!("+{merge_base}:refs/dr/pull/{number}/base");
        let head_spec = format!("+{head}:refs/dr/pull/{number}/head");
        self.git(
            Some(auth),
            &[
                "fetch",
                "--depth=1",
                "--filter=blob:none",
                "--no-tags",
                "--no-write-fetch-head",
                "--recurse-submodules=no",
                "--end-of-options",
                "origin",
                &base_spec,
                &head_spec,
            ],
            None,
        )
        .await
        .map(|_| ())
    }

    /// Pins a local commit with `refs/dr/pull/<n>/c/<sha>` so a future gc keeps it.
    /// `protocol.allow=never`: if the commit were missing, `update-ref` would otherwise
    /// lazily fetch it with its whole history.
    pub async fn protect_commit(
        &self,
        _guard: &RepoGuard,
        number: u64,
        sha: &str,
    ) -> Result<(), GitFailure> {
        debug_assert!(is_oid(sha));
        let name = format!("refs/dr/pull/{number}/c/{sha}");
        self.git(
            None,
            &["-c", "protocol.allow=never", "update-ref", &name, sha],
            None,
        )
        .await
        .map(|_| ())
    }

    /// Runs `diff-tree -r -z --raw --no-abbrev` (with `extra` flags) and parses it.
    pub async fn diff_tree(
        &self,
        auth: &GitAuth,
        extra: &[&str],
        from: &str,
        to: &str,
    ) -> Result<Vec<ChangedFile>, GitFailure> {
        let mut args = vec!["diff-tree", "-r", "-z", "--raw", "--no-abbrev"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--end-of-options", from, to]);
        let out = self.git(Some(auth), &args, None).await?;
        parse::parse_diff_tree(&out).map_err(|e| GitFailure {
            command: "diff-tree".into(),
            stderr: e,
        })
    }

    /// OIDs of every regular-file `.gitattributes` in `head` that can affect `paths`: the
    /// root plus each ancestor directory. `paths` must exist in `head`, so every ancestor is
    /// a tree; trees are local in the blob-less clone, so `cat-file --batch` on
    /// `<head>:<dir>` (fed over stdin, no argv limit) never triggers a lazy fetch.
    pub async fn gitattributes_oids(
        &self,
        head: &str,
        paths: &[&str],
    ) -> Result<Vec<String>, GitFailure> {
        let mut dirs: BTreeSet<&str> = BTreeSet::from([""]);
        for path in paths {
            let mut dir = *path;
            while let Some((parent, _)) = dir.rsplit_once('/') {
                dirs.insert(parent);
                dir = parent;
            }
        }
        let mut input = String::new();
        // Input is line-based; a directory name containing a newline cannot be looked up.
        for dir in dirs.into_iter().filter(|d| !d.contains('\n')) {
            input.push_str(head);
            input.push(':');
            input.push_str(dir);
            input.push('\n');
        }
        let out = self
            .git(None, &["cat-file", "--batch"], Some(input.into_bytes()))
            .await?;
        parse::gitattributes_in_trees(&out).map_err(|e| GitFailure {
            command: "cat-file --batch".into(),
            stderr: e,
        })
    }

    /// Step 5: batch-fetch blobs by OID, fed over stdin.
    pub async fn fetch_blobs(
        &self,
        _guard: &RepoGuard,
        auth: &GitAuth,
        oids: &[String],
    ) -> Result<(), GitFailure> {
        if oids.is_empty() {
            return Ok(());
        }
        let mut input = oids.join("\n");
        input.push('\n');
        self.git(
            Some(auth),
            &[
                "-c",
                "fetch.negotiationAlgorithm=noop",
                "fetch",
                "origin",
                "--no-tags",
                "--no-write-fetch-head",
                "--recurse-submodules=no",
                "--filter=blob:none",
                "--stdin",
            ],
            Some(input.into_bytes()),
        )
        .await
        .map(|_| ())
    }

    /// Step 7: `linguist-generated` / `linguist-vendored` attributes as of `head`.
    pub async fn check_attr(
        &self,
        auth: &GitAuth,
        head: &str,
        paths: &[&str],
    ) -> Result<HashMap<String, Vec<(String, String)>>, GitFailure> {
        if paths.is_empty() {
            return Ok(HashMap::new());
        }
        let source = format!("--source={head}");
        let mut input = Vec::new();
        for p in paths {
            input.extend_from_slice(p.as_bytes());
            input.push(0);
        }
        let out = self
            .git(
                Some(auth),
                &[
                    "check-attr",
                    &source,
                    "-z",
                    "--stdin",
                    "linguist-generated",
                    "linguist-vendored",
                ],
                Some(input),
            )
            .await?;
        Ok(parse::parse_check_attr(&out))
    }

    /// Records the OIDs of a pull request's files (blob vs gitlink) for [`Repo::known_oid`].
    pub fn remember_oids<'a>(&self, oids: impl IntoIterator<Item = (&'a str, bool)>) {
        let mut known = self.shared.known.lock().expect("known OIDs poisoned");
        for (oid, is_blob) in oids {
            known.insert(oid.to_string(), is_blob);
        }
    }

    /// `Some(true)` for a blob and `Some(false)` for a gitlink listed in a pull request
    /// opened by this process; `None` if no such pull request lists `oid`.
    pub fn known_oid(&self, oid: &str) -> Option<bool> {
        self.shared
            .known
            .lock()
            .expect("known OIDs poisoned")
            .get(oid)
            .copied()
    }

    /// Reads one object with `cat-file --batch`. `oid` must satisfy [`is_oid`].
    ///
    /// Without `lazy` (`guard` = `None`) the read is purely local and needs no lock: git
    /// 2.43 has no `--no-lazy-fetch`, so `protocol.allow=never` makes the promisor fetch
    /// that git would start for a missing object fail at once, before it touches the repo.
    /// With a guard, a missing object is fetched lazily from the promisor remote.
    pub async fn read_object(
        &self,
        guard: Option<&RepoGuard>,
        auth: Option<&GitAuth>,
        oid: &str,
    ) -> Result<ObjectLookup, GitFailure> {
        let mut args = Vec::new();
        if guard.is_none() {
            args.extend(["-c", "protocol.allow=never"]);
        }
        args.extend(["cat-file", "--batch"]);
        match self
            .git(auth, &args, Some(format!("{oid}\n").into_bytes()))
            .await
        {
            Ok(out) => parse::parse_batch_object(&out).map_err(|e| GitFailure {
                command: "cat-file --batch".into(),
                stderr: e,
            }),
            // Network trouble is a real failure; anything else the promisor remote refused
            // ("not our ref", "could not fetch ... from promisor remote") means "no such object".
            Err(e) if e.stderr.contains("unable to access") => Err(e),
            Err(e)
                if e.stderr.contains("from promisor remote")
                    || e.stderr.contains("not our ref") =>
            {
                Ok(ObjectLookup::Missing)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
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

    async fn git_in(dir: &Path, args: &[&str]) -> String {
        let mut full = vec!["-C", dir.to_str().unwrap()];
        full.extend_from_slice(args);
        let out = run_git(None, None, &full, None).await.unwrap();
        String::from_utf8(out).unwrap().trim().to_string()
    }

    /// Creates a work-tree repo with the given files committed; returns the head commit.
    async fn make_repo(dir: &Path, files: &[(&str, &str)]) -> String {
        git_in(dir, &["init", "-q"]).await;
        for (path, content) in files {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, content).unwrap();
        }
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
                "init",
            ],
        )
        .await;
        git_in(dir, &["rev-parse", "HEAD"]).await
    }

    fn repo_at(git_dir: PathBuf, remote_url: String) -> Repo {
        Repo {
            git_dir,
            remote_url,
            shared: Default::default(),
        }
    }

    #[test]
    fn oid_validation() {
        assert!(is_oid("0123456789abcdef0123456789abcdef01234567"));
        assert!(is_oid(&"a".repeat(64)));
        assert!(!is_oid("0123456789ABCDEF0123456789abcdef01234567"));
        assert!(!is_oid("0123456789abcdef0123456789abcdef0123456"));
        assert!(!is_oid("0123456789abcdef0123456789abcdef012345678"));
        assert!(!is_oid(&"a".repeat(63)));
        assert!(!is_oid("../../../../etc/passwd/aaaaaaaaaaaaaaaaaaaa"));
        assert!(!is_oid("--upload-pack=touch${IFS}x;aaaaaaaaaaaaaaa"));
        assert!(!is_oid("HEAD"));
        assert!(!is_oid(""));
    }

    #[tokio::test]
    async fn git_environment_is_an_allowlist() {
        // `!env` runs in git's environment (as git sets it up for a shell alias).
        let out = run_git::<&str>(None, None, &["-c", "alias.e=!env", "e"], None)
            .await
            .unwrap();
        let env = String::from_utf8(out).unwrap();
        let keys: Vec<&str> = env
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, _)| k)
            .collect();
        for want in [
            "GIT_TERMINAL_PROMPT",
            "GIT_LITERAL_PATHSPECS",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_GLOBAL",
        ] {
            assert!(keys.contains(&want), "{want} missing");
        }
        for banned in [
            "SSH_ASKPASS",
            "SSH_ASKPASS_REQUIRE",
            "GIT_ASKPASS",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
            "GIT_DIR",
            "LC_ALL",
        ] {
            assert!(!keys.contains(&banned), "{banned} leaked");
        }
        assert!(env.lines().any(|l| l == "LC_MESSAGES=C"));
    }

    #[test]
    fn stale_lock_cleanup() {
        let dir = temp_dir("locks");
        let touch = |rel: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        };
        for rel in [
            "shallow.lock",
            "packed-refs.lock",
            "HEAD.lock",
            "config.lock",
            "refs/dr/pull/1/head.lock",
            "refs/heads/main.lock",
        ] {
            touch(rel);
        }
        for rel in [
            "config",
            "shallow",
            FLOCK_FILE,
            "refs/dr/pull/1/base",
            "objects/pack/pack-x.lock",
            "hooks/pre-x.lock/keep",
        ] {
            touch(rel);
        }
        let mut removed: Vec<String> = remove_stale_locks(&dir)
            .iter()
            .map(|p| p.strip_prefix(&dir).unwrap().display().to_string())
            .collect();
        removed.sort();
        assert_eq!(
            removed,
            vec![
                "HEAD.lock",
                "config.lock",
                "packed-refs.lock",
                "refs/dr/pull/1/head.lock",
                "refs/heads/main.lock",
                "shallow.lock",
            ]
        );
        for rel in [
            "config",
            "shallow",
            FLOCK_FILE,
            "refs/dr/pull/1/base",
            "objects/pack/pack-x.lock",
        ] {
            assert!(dir.join(rel).exists(), "{rel}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn private_dirs() {
        let dir = temp_dir("perms");
        let nested = dir.join("a/b");
        ensure_private_dir(&nested).unwrap();
        for d in [dir.join("a"), nested.clone()] {
            assert_eq!(
                std::fs::metadata(&d).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o775)).unwrap();
        ensure_private_dir(&nested).unwrap();
        assert_eq!(
            std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn gitattributes_lookup_is_literal() {
        let dir = temp_dir("attrs");
        let head = make_repo(
            &dir,
            &[
                (".gitattributes", "*.gen linguist-generated\n"),
                (":!x/.gitattributes", "*.txt linguist-generated\n"),
                (":!x/a.txt", "a\n"),
                ("b/c.txt", "c\n"),
                ("d/.gitattributes", "* linguist-vendored\n"),
                ("d/e.txt", "e\n"),
                ("sp ace/.gitattributes/f.txt", "dir named .gitattributes\n"),
            ],
        )
        .await;
        let want = |path: &str| {
            let dir = dir.clone();
            let path = path.to_string();
            async move { git_in(&dir, &["rev-parse", &format!("HEAD:{path}")]).await }
        };
        let repo = repo_at(dir.join(".git"), String::new());
        let mut got = repo
            .gitattributes_oids(
                &head,
                &[":!x/a.txt", "b/c.txt", "sp ace/.gitattributes/f.txt"],
            )
            .await
            .unwrap();
        got.sort();
        let mut expected = vec![
            want(".gitattributes").await,
            want(":!x/.gitattributes").await,
        ];
        expected.sort();
        assert_eq!(got, expected);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Partial clone over file://: a local read never lazily fetches, a locked read does.
    #[tokio::test]
    async fn local_read_does_not_lazy_fetch() {
        let dir = temp_dir("lazy");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let head = make_repo(&src, &[("a.txt", "hello\n")]).await;
        git_in(&src, &["config", "uploadpack.allowFilter", "true"]).await;
        git_in(&src, &["config", "uploadpack.allowAnySHA1InWant", "true"]).await;
        let blob = git_in(&src, &["rev-parse", "HEAD:a.txt"]).await;

        let repo = repo_at(
            dir.join("cache/o/r.git"),
            format!("file://{}", src.display()),
        );
        let guard = repo.lock().await.unwrap();
        assert_eq!(
            std::fs::metadata(&repo.git_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let auth = GitAuth {
            host: "example.invalid".into(),
            token: "unused".into(),
        };
        repo.fetch_commits(&guard, &auth, 1, &head, &head)
            .await
            .unwrap();
        assert!(!repo.git_dir.join("shallow.lock").exists());

        let missing = "1234567890123456789012345678901234567890";
        for oid in [blob.as_str(), missing] {
            let started = std::time::Instant::now();
            let got = repo.read_object(None, None, oid).await.unwrap();
            assert!(matches!(got, ObjectLookup::Missing), "{oid}");
            assert!(started.elapsed() < Duration::from_secs(5));
        }
        match repo.read_object(None, None, &head).await.unwrap() {
            ObjectLookup::Found { kind, .. } => assert_eq!(kind, "commit"),
            ObjectLookup::Missing => panic!("head commit must be local"),
        }
        match repo.read_object(Some(&guard), None, &blob).await.unwrap() {
            ObjectLookup::Found { kind, data } => {
                assert_eq!(kind, "blob");
                assert_eq!(data, b"hello\n");
            }
            ObjectLookup::Missing => panic!("lazy fetch should have found the blob"),
        }
        assert!(matches!(
            repo.read_object(Some(&guard), None, missing).await.unwrap(),
            ObjectLookup::Missing
        ));
        // Now local without a fetch.
        assert!(matches!(
            repo.read_object(None, None, &blob).await.unwrap(),
            ObjectLookup::Found { .. }
        ));
        drop(guard);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
