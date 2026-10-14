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

pub use parse::{ChangedFile, TreeEntry};

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

    /// Every file of `commit`'s tree (`ls-tree -r`). Trees are local in the blob-less clone,
    /// so this needs no lock; `protocol.allow=never` makes it fail rather than fetch if the
    /// commit were missing.
    pub async fn ls_tree(&self, commit: &str) -> Result<Vec<TreeEntry>, GitFailure> {
        debug_assert!(is_oid(commit));
        let out = self
            .git(
                None,
                &[
                    "-c",
                    "protocol.allow=never",
                    "ls-tree",
                    "-r",
                    "-z",
                    "--full-tree",
                    "--end-of-options",
                    commit,
                ],
                None,
            )
            .await?;
        parse::parse_ls_tree(&out).map_err(|e| GitFailure {
            command: "ls-tree".into(),
            stderr: e,
        })
    }

    /// Objects of `commit`'s tree that are missing locally, i.e. its blobs that were not fetched
    /// yet. `rev-list --objects --no-walk --missing=print` prints them as `?<oid>` and never
    /// fetches by itself (`protocol.allow=never` is a second safeguard); it does not descend
    /// into gitlinks, so submodules are never listed. Needs no lock. An unknown `commit` is an
    /// error.
    #[allow(dead_code)] // The index build uses it (not wired in yet).
    pub async fn missing_objects(&self, commit: &str) -> Result<HashSet<String>, GitFailure> {
        debug_assert!(is_oid(commit));
        let out = self
            .git(
                None,
                &[
                    "-c",
                    "protocol.allow=never",
                    "rev-list",
                    "--objects",
                    "--no-walk",
                    "--missing=print",
                    "--no-object-names",
                    "--end-of-options",
                    commit,
                ],
                None,
            )
            .await?;
        parse::parse_missing_objects(&out).map_err(|e| GitFailure {
            command: "rev-list".into(),
            stderr: e,
        })
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

    /// Sizes of the objects among `oids` that are local (`cat-file --batch-check`); the others
    /// are left out of the map. Needs no lock and never fetches. `oids` must satisfy [`is_oid`];
    /// an unfetched blob among them costs two more git runs (see `cat_file_local_with`).
    #[allow(dead_code)] // The index build uses it (not wired in yet).
    pub async fn object_sizes(&self, oids: &[String]) -> Result<HashMap<String, u64>, GitFailure> {
        let out = self
            .cat_file_local("--batch-check=%(objectname) %(objectsize)", oids)
            .await?;
        parse::parse_object_sizes(&out).map_err(|e| GitFailure {
            command: "cat-file --batch-check".into(),
            stderr: e,
        })
    }

    /// Contents of the blobs among `oids` that are local (`cat-file --batch`), in `oids` order;
    /// missing objects and objects of other types are left out. Needs no lock and never
    /// fetches. The output is buffered, so the caller bounds the total size (see
    /// [`Repo::object_sizes`]). `oids` must satisfy [`is_oid`]; an unfetched blob among them
    /// costs two more git runs (see `cat_file_local_with`).
    #[allow(dead_code)] // The index build uses it (not wired in yet).
    pub async fn read_blobs(&self, oids: &[String]) -> Result<Vec<(String, Vec<u8>)>, GitFailure> {
        let out = self.cat_file_local("--batch", oids).await?;
        parse::parse_batch_blobs(&out).map_err(|e| GitFailure {
            command: "cat-file --batch".into(),
            stderr: e,
        })
    }

    /// Runs `cat-file <mode>` over `oids` without a lock and returns its stdout.
    async fn cat_file_local(&self, mode: &str, oids: &[String]) -> Result<Vec<u8>, GitFailure> {
        cat_file_local_with(mode, oids, |args, stdin| async move {
            self.git(None, &args, stdin).await
        })
        .await
    }
}

/// Runs `git cat-file <mode>` over `oids` (one per line on stdin) by calling `run(args, stdin)`
/// and returns its stdout. Every run carries `-c protocol.allow=never`: git 2.43 has no
/// `--no-lazy-fetch`, and this stops it from fetching a missing object.
///
/// Git prints `<oid> missing` for an object it knows nothing about, but for a blob that a tree
/// of the partial clone promises and that was never fetched it dies with `could not fetch <oid>
/// from promisor remote`, and [`run_git`] drops the output of a failed run. After such a
/// failure the local objects are listed once (`--batch-all-objects` names only what is in the
/// repo and never fetches), the OIDs that are not among them are dropped as missing, and the
/// run is repeated on the rest; if that fails too, its error is returned. That is at most three
/// runs however many OIDs are unfetched. The failed run pays git's scan of the promisor packs
/// (0.6 s in a clone of microsoft/vscode) and the listing grows with the object store, so
/// callers should still pass fetched OIDs ([`Repo::missing_objects`] lists the others).
async fn cat_file_local_with<F, Fut>(
    mode: &str,
    oids: &[String],
    mut run: F,
) -> Result<Vec<u8>, GitFailure>
where
    F: FnMut(Vec<String>, Option<Vec<u8>>) -> Fut,
    Fut: Future<Output = Result<Vec<u8>, GitFailure>>,
{
    debug_assert!(oids.iter().all(|oid| is_oid(oid)));
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let cat_file = |tail: &[&str]| -> Vec<String> {
        let mut args = ["-c", "protocol.allow=never", "cat-file"]
            .map(String::from)
            .to_vec();
        args.extend(tail.iter().map(|arg| arg.to_string()));
        args
    };
    let lines = |list: &[String]| Some(format!("{}\n", list.join("\n")).into_bytes());
    // Success and every other failure are the result; only an unfetched blob goes on below.
    match run(cat_file(&[mode]), lines(oids)).await {
        Err(e) if parse::is_unfetched_object_error(&e.stderr) => {}
        result => return result,
    }
    let listing = run(
        cat_file(&[
            "--batch-check=%(objectname)",
            "--batch-all-objects",
            "--unordered",
        ]),
        None,
    )
    .await?;
    let local = parse::listed_oids(oids, &listing).map_err(|e| GitFailure {
        command: "cat-file --batch-all-objects".into(),
        stderr: e,
    })?;
    if local.is_empty() {
        return Ok(Vec::new());
    }
    run(cat_file(&[mode]), lines(&local)).await
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

    #[tokio::test]
    async fn ls_tree_lists_a_blobless_head_without_fetching() {
        let dir = temp_dir("lstree");
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        make_repo(&src, &[("a.txt", "a\n"), ("deep/er/b.rs", "b\n")]).await;
        let sub = "1234567890123456789012345678901234567890";
        git_in(
            &src,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{sub},vendor/lib"),
            ],
        )
        .await;
        git_in(
            &src,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "commit",
                "-qm",
                "submodule",
            ],
        )
        .await;
        let head = git_in(&src, &["rev-parse", "HEAD"]).await;
        git_in(&src, &["config", "uploadpack.allowFilter", "true"]).await;
        git_in(&src, &["config", "uploadpack.allowAnySHA1InWant", "true"]).await;
        let blob = git_in(&src, &["rev-parse", "HEAD:deep/er/b.rs"]).await;

        let repo = repo_at(
            dir.join("cache/o/r.git"),
            format!("file://{}", src.display()),
        );
        let guard = repo.lock().await.unwrap();
        let auth = GitAuth {
            host: "example.invalid".into(),
            token: "unused".into(),
        };
        repo.fetch_commits(&guard, &auth, 1, &head, &head)
            .await
            .unwrap();
        drop(guard);

        let started = std::time::Instant::now();
        let entries = repo.ls_tree(&head).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        let got: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| (e.path.as_str(), e.mode.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a.txt", "100644"),
                ("deep/er/b.rs", "100644"),
                ("vendor/lib", "160000")
            ]
        );
        assert_eq!(entries[1].oid, blob);
        assert_eq!(entries[2].oid, sub);
        // Listing did not fetch any blob.
        assert!(matches!(
            repo.read_object(None, None, &blob).await.unwrap(),
            ObjectLookup::Missing
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const LIB_V1: &str = "pub fn one() {}\n";
    const LIB_V2: &str = "pub fn one() {}\npub fn two() {}\n";
    const HELPERS: &str = "pub fn help() {}\n";
    const BINARY: &[u8] = b"\0\x01\xff\xfe\r\n\n\0tail";
    const UNKNOWN: &str = "1234567890123456789012345678901234567891";

    /// Over 1 MiB, the size from which the index leaves a file out.
    fn big_rs() -> String {
        "// filler line\n".repeat(80_000)
    }

    /// A source repo with two commits and the blob-less cache repo a pull request from the first
    /// (`base`) to the second (`head`) leaves behind: both commits and their trees, no blobs.
    /// Besides plain files the commits hold a path with a space, a subdirectory, two paths with
    /// one content, a binary file, a file over 1 MiB and a gitlink.
    struct PartialClone {
        dir: PathBuf,
        origin: PathBuf,
        repo: Repo,
        auth: GitAuth,
        base: String,
        head: String,
    }

    impl PartialClone {
        async fn new(name: &str) -> PartialClone {
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
            git_in(&origin, &["config", "uploadpack.allowFilter", "true"]).await;
            git_in(
                &origin,
                &["config", "uploadpack.allowAnySHA1InWant", "true"],
            )
            .await;

            let repo = repo_at(
                dir.join("cache/o/r.git"),
                format!("file://{}", origin.display()),
            );
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

        /// The OID of `path` in `commit`, from the source repo.
        async fn oid(&self, commit: &str, path: &str) -> String {
            git_in(&self.origin, &["rev-parse", &format!("{commit}:{path}")]).await
        }

        /// The blobs of `commit`'s tree (not its gitlinks), from the source repo.
        async fn blobs(&self, commit: &str) -> HashSet<String> {
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
        async fn fetch(&self, oids: &[&str]) {
            let oids: Vec<String> = oids.iter().map(|oid| oid.to_string()).collect();
            let guard = self.repo.lock().await.unwrap();
            self.repo
                .fetch_blobs(&guard, &self.auth, &oids)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn missing_objects_lists_the_unfetched_blobs() {
        let pc = PartialClone::new("missing-objects").await;
        let (base, head) = (pc.base.clone(), pc.head.clone());
        // Five blobs in base (README, lib, helpers, a b.py, `same` for two paths); six in head
        // (no README; the new lib, big.rs and data.bin). The gitlink is no blob.
        let (base_blobs, head_blobs) = (pc.blobs(&base).await, pc.blobs(&head).await);
        assert_eq!((base_blobs.len(), head_blobs.len()), (5, 6));

        // The blob-less clone has none of them.
        assert_eq!(pc.repo.missing_objects(&base).await.unwrap(), base_blobs);
        assert_eq!(pc.repo.missing_objects(&head).await.unwrap(), head_blobs);

        // Fetching shrinks the set of every commit whose tree has the blob.
        let lib_v1 = pc.oid(&base, "src/lib.rs").await;
        let big = pc.oid(&head, "big.rs").await;
        pc.fetch(&[&lib_v1, &big]).await;
        let (mut want_base, mut want_head) = (base_blobs.clone(), head_blobs.clone());
        want_base.remove(&lib_v1);
        want_head.remove(&big);
        assert_eq!(pc.repo.missing_objects(&base).await.unwrap(), want_base);
        assert_eq!(pc.repo.missing_objects(&head).await.unwrap(), want_head);

        let helpers = pc.oid(&head, "src/util/helpers.rs").await;
        assert_eq!(helpers, pc.oid(&base, "src/util/helpers.rs").await);
        pc.fetch(&[&helpers]).await;
        want_base.remove(&helpers);
        want_head.remove(&helpers);
        assert_eq!(pc.repo.missing_objects(&base).await.unwrap(), want_base);
        assert_eq!(pc.repo.missing_objects(&head).await.unwrap(), want_head);

        // With every blob present, nothing is missing.
        let all: Vec<&str> = base_blobs.union(&head_blobs).map(String::as_str).collect();
        pc.fetch(&all).await;
        assert!(pc.repo.missing_objects(&base).await.unwrap().is_empty());
        assert!(pc.repo.missing_objects(&head).await.unwrap().is_empty());

        // A commit the repo does not have is an error, not a fetch.
        assert!(pc.repo.missing_objects(UNKNOWN).await.is_err());
        std::fs::remove_dir_all(&pc.dir).unwrap();
    }

    #[tokio::test]
    async fn object_sizes_and_blobs_skip_missing_objects() {
        let pc = PartialClone::new("blob-reads").await;
        let (base, head) = (pc.base.clone(), pc.head.clone());
        let big_rs = big_rs();
        assert!(big_rs.len() > 1 << 20);
        let helpers = pc.oid(&head, "src/util/helpers.rs").await;
        let lib_v2 = pc.oid(&head, "src/lib.rs").await;
        let big = pc.oid(&head, "big.rs").await;
        let bin = pc.oid(&head, "data.bin").await;
        pc.fetch(&[&helpers, &lib_v2, &big, &bin]).await;
        // In trees of the clone but never fetched, so git cannot read them.
        let readme = pc.oid(&base, "README.md").await;
        let lib_v1 = pc.oid(&base, "src/lib.rs").await;
        let same = pc.oid(&head, "same-1.txt").await;
        let tree = git_in(&pc.origin, &["rev-parse", &format!("{head}^{{tree}}")]).await;
        let present: Vec<(&String, &[u8])> = vec![
            (&helpers, HELPERS.as_bytes()),
            (&big, big_rs.as_bytes()),
            (&bin, BINARY),
            (&lib_v2, LIB_V2.as_bytes()),
        ];

        // Unfetched blobs first, last, repeated and next to each other; an object git has never
        // heard of; and a tree, which is local but no blob.
        let oids: Vec<String> = [
            &readme, &helpers, &big, &lib_v1, &same, UNKNOWN, &bin, &readme, &tree, &lib_v2, &same,
        ]
        .map(String::from)
        .into();
        let mut sizes = pc.repo.object_sizes(&oids).await.unwrap();
        let blobs = pc.repo.read_blobs(&oids).await.unwrap();
        assert!(sizes.remove(&tree).is_some_and(|size| size > 0));
        let want_sizes: HashMap<String, u64> = present
            .iter()
            .map(|(oid, data)| (oid.to_string(), data.len() as u64))
            .collect();
        assert_eq!(sizes, want_sizes);
        // In `oids` order, byte for byte (the comparison avoids printing a megabyte on failure).
        let got: Vec<&str> = blobs.iter().map(|(oid, _)| oid.as_str()).collect();
        let want: Vec<&str> = present.iter().map(|(oid, _)| oid.as_str()).collect();
        assert_eq!(got, want);
        for ((oid, data), (_, want_data)) in blobs.iter().zip(&present) {
            assert!(data == want_data, "contents of {oid}");
        }

        // Nothing local at all.
        let none = vec![
            readme.clone(),
            UNKNOWN.to_string(),
            lib_v1.clone(),
            same.clone(),
        ];
        assert!(pc.repo.object_sizes(&none).await.unwrap().is_empty());
        assert!(pc.repo.read_blobs(&none).await.unwrap().is_empty());

        // Git dies on the first of 50,000 lines of input, long before it has read them all.
        let mut many = vec![readme.clone()];
        many.extend(std::iter::repeat_n(helpers.clone(), 50_000));
        let sizes = pc.repo.object_sizes(&many).await.unwrap();
        assert_eq!(sizes.len(), 1);
        assert_eq!(sizes[&helpers], HELPERS.len() as u64);

        // An empty list does not even start git: the repo does not exist.
        let nowhere = repo_at(pc.dir.join("nowhere.git"), String::new());
        assert!(nowhere.object_sizes(&[]).await.unwrap().is_empty());
        assert!(nowhere.read_blobs(&[]).await.unwrap().is_empty());
        assert!(nowhere.object_sizes(&none).await.is_err());
        std::fs::remove_dir_all(&pc.dir).unwrap();
    }

    #[tokio::test]
    async fn blob_reads_never_fetch() {
        let pc = PartialClone::new("no-fetch").await;
        let (base, head) = (pc.base.clone(), pc.head.clone());
        let helpers = pc.oid(&head, "src/util/helpers.rs").await;
        let readme = pc.oid(&base, "README.md").await;
        let same = pc.oid(&head, "same-1.txt").await;
        pc.fetch(&[&helpers]).await;
        let mut want_missing = pc.blobs(&head).await;
        want_missing.remove(&helpers);
        let stats = git_in(&pc.repo.git_dir, &["count-objects", "-v"]).await;
        let oids = vec![readme, helpers.clone(), same];

        // First while the source repo can serve the missing blobs, so a lazy fetch would
        // succeed and show; then with the source gone, so that it would fail.
        for reachable in [true, false] {
            if !reachable {
                std::fs::rename(&pc.origin, pc.dir.join("origin-moved")).unwrap();
            }
            let missing = pc.repo.missing_objects(&head).await.unwrap();
            assert_eq!(missing, want_missing, "reachable: {reachable}");
            let sizes = pc.repo.object_sizes(&oids).await.unwrap();
            let want = HashMap::from([(helpers.clone(), HELPERS.len() as u64)]);
            assert_eq!(sizes, want, "reachable: {reachable}");
            let blobs = pc.repo.read_blobs(&oids).await.unwrap();
            let want = vec![(helpers.clone(), HELPERS.as_bytes().to_vec())];
            assert_eq!(blobs, want, "reachable: {reachable}");
            let now = git_in(&pc.repo.git_dir, &["count-objects", "-v"]).await;
            assert_eq!(now, stats, "reachable: {reachable}");
        }
        std::fs::remove_dir_all(&pc.dir).unwrap();
    }

    fn failure(command: &str, stderr: &str) -> GitFailure {
        GitFailure {
            command: command.into(),
            stderr: stderr.into(),
        }
    }

    /// Runs `cat_file_local_with` against canned git replies, in order. Returns its result and
    /// the arguments and stdin of every run it asked for.
    async fn fake_cat_file(
        oids: &[String],
        replies: Vec<Result<Vec<u8>, GitFailure>>,
    ) -> (
        Result<Vec<u8>, GitFailure>,
        Vec<(Vec<String>, Option<String>)>,
    ) {
        let mut replies = replies.into_iter();
        let mut runs = Vec::new();
        let result = cat_file_local_with("--batch", oids, |args, stdin| {
            runs.push((args, stdin.map(|bytes| String::from_utf8(bytes).unwrap())));
            std::future::ready(replies.next().expect("one run too many"))
        })
        .await;
        (result, runs)
    }

    #[tokio::test]
    async fn local_cat_file_runs_at_most_three_times() {
        let [a, b, c, d] = ["a", "b", "c", "d"].map(|digit| digit.repeat(40));
        let oids: Vec<String> = [&a, &b, &c, &b, &d, &a].into_iter().cloned().collect();
        let lines = |list: &[&String]| Some(list.iter().map(|oid| format!("{oid}\n")).collect());
        let args = |tail: &[&str]| -> Vec<String> {
            ["-c", "protocol.allow=never", "cat-file"]
                .iter()
                .chain(tail)
                .map(|arg| arg.to_string())
                .collect()
        };
        let listing_args = args(&[
            "--batch-check=%(objectname)",
            "--batch-all-objects",
            "--unordered",
        ]);
        let died = failure(
            "cat-file --batch",
            &format!(
                "fatal: transport 'file' not allowed\nfatal: could not fetch {b} from promisor remote"
            ),
        );
        // Of these only `b` is not local, whatever the order and repeats of the listing.
        let listing = format!("{d}\n{a}\n{c}\n{a}\n").into_bytes();

        // Nothing unfetched: one run, whose output is the result.
        let (result, runs) = fake_cat_file(&oids, vec![Ok(b"all".to_vec())]).await;
        assert_eq!(result.unwrap(), b"all");
        assert_eq!(
            runs,
            [(args(&["--batch"]), lines(&[&a, &b, &c, &b, &d, &a]))]
        );

        // An unfetched blob: the failed run, the listing, then the run again on the OIDs that
        // are local, in their order and with their repeats.
        let replies = vec![Err(died.clone()), Ok(listing.clone()), Ok(b"rest".to_vec())];
        let (result, runs) = fake_cat_file(&oids, replies).await;
        assert_eq!(result.unwrap(), b"rest");
        assert_eq!(
            runs,
            [
                (args(&["--batch"]), lines(&[&a, &b, &c, &b, &d, &a])),
                (listing_args.clone(), None),
                (args(&["--batch"]), lines(&[&a, &c, &d, &a])),
            ]
        );

        // The run again fails too, with the same kind of error: no fourth run (the fake panics
        // on one) and the error of the last run.
        let last = failure(
            "cat-file --batch",
            &format!("fatal: could not fetch {d} from promisor remote"),
        );
        let replies = vec![Err(died.clone()), Ok(listing.clone()), Err(last.clone())];
        let (result, runs) = fake_cat_file(&oids, replies).await;
        assert_eq!(result.unwrap_err().stderr, last.stderr);
        assert_eq!(runs.len(), 3);

        // The listing fails or is no list of OIDs: no run after it.
        let broken = failure(
            "cat-file --batch-check=%(objectname)",
            "fatal: listing failed",
        );
        let (result, runs) = fake_cat_file(&oids, vec![Err(died.clone()), Err(broken)]).await;
        assert_eq!(result.unwrap_err().stderr, "fatal: listing failed");
        assert_eq!(runs.len(), 2);
        let replies = vec![Err(died.clone()), Ok(b"garbage\n".to_vec())];
        let (result, runs) = fake_cat_file(&oids, replies).await;
        assert_eq!(result.unwrap_err().command, "cat-file --batch-all-objects");
        assert_eq!(runs.len(), 2);

        // None of the OIDs is local: nothing left to read.
        let (result, runs) = fake_cat_file(&oids, vec![Err(died), Ok(Vec::new())]).await;
        assert!(result.unwrap().is_empty());
        assert_eq!(runs.len(), 2);

        // Any other failure is the result, without a listing.
        let other = failure(
            "cat-file --batch",
            "fatal: not a git repository: 'nowhere.git'",
        );
        let (result, runs) = fake_cat_file(&oids, vec![Err(other.clone())]).await;
        assert_eq!(result.unwrap_err().stderr, other.stderr);
        assert_eq!(runs.len(), 1);

        // No OIDs, no git.
        let (result, runs) = fake_cat_file(&[], Vec::new()).await;
        assert!(result.unwrap().is_empty());
        assert!(runs.is_empty());
    }
}
