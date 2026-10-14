//! Opening a PR: GitHub metadata + cache-repo fetch + diff, with a short in-memory cache.

pub mod nav;
pub mod review;
pub mod target;
#[cfg(test)]
pub(crate) mod testing;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::AppError;
use crate::git::parse::GITLINK_MODE;
use crate::git::{
    ChangedFile, GitAuth, GitFailure, ObjectLookup, Repo, RepoGuard, Repos, STALL_PR, TreeEntry,
};
use crate::github::{Github, Tokens, ViewedState};
use crate::index::Indexer;
use crate::index::build::Limits;
use crate::index::db::IndexDb;
use crate::state::{PrKey, StateDb};

const CACHE_TTL: Duration = Duration::from_secs(30);
/// With `refresh`, a result whose computation started at most this long before the request
/// is still accepted: the SPA sends the summary and file-list requests of one refresh
/// concurrently, and they must share one pipeline run (and one snapshot).
const REFRESH_COALESCE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize)]
pub struct RefInfo {
    #[serde(rename = "ref")]
    pub name: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrSummary {
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub state: String,
    pub url: String,
    pub base: RefInfo,
    pub head: RefInfo,
    pub merge_base: String,
    pub node_id: String,
    pub viewer: String,
}

pub struct PrData {
    pub summary: PrSummary,
    pub files: Vec<ChangedFile>,
    /// The viewer's viewed marks, keyed by GitHub's own file paths (its rename detection
    /// may differ from ours). Updated in place after a successful `set_viewed`.
    viewed: Mutex<HashMap<String, ViewedState>>,
    /// Every file of the head tree, listed on first request (see [`PrService::tree`]).
    tree: tokio::sync::OnceCell<Arc<Vec<TreeEntry>>>,
}

/// A [`ChangedFile`] plus the viewer's viewed mark (only for paths GitHub lists).
#[derive(Serialize)]
pub struct FileWithViewed<'a> {
    #[serde(flatten)]
    file: &'a ChangedFile,
    #[serde(skip_serializing_if = "Option::is_none")]
    viewed: Option<ViewedState>,
}

impl PrData {
    pub fn files_with_viewed(&self) -> Vec<FileWithViewed<'_>> {
        let viewed = self.viewed.lock().expect("viewed map poisoned");
        self.files
            .iter()
            .map(|file| FileWithViewed {
                file,
                viewed: viewed.get(&file.path).copied(),
            })
            .collect()
    }

    fn fully_viewed(&self) -> bool {
        let marks = self.viewed.lock().expect("viewed map poisoned");
        fully_viewed(&self.files, &marks, &marks)
    }
}

/// Whether `marks` has every file the user can mark here as VIEWED: every file of ours that
/// GitHub listed in `markable`, except generated ones. Paths only GitHub lists (its rename
/// detection differs from ours) get no checkbox, so they cannot be required. A required
/// path missing from `marks` (e.g. a truncated list) counts as not viewed.
fn fully_viewed(
    files: &[ChangedFile],
    markable: &HashMap<String, ViewedState>,
    marks: &HashMap<String, ViewedState>,
) -> bool {
    files
        .iter()
        .filter(|f| !f.generated && markable.contains_key(&f.path))
        .all(|f| marks.get(&f.path) == Some(&ViewedState::Viewed))
}

/// Rejects a viewed-mark change the user cannot have meant: a head other than the one the
/// page shows (`client_head`) and the one `data` was built from (409, GitHub would also
/// mark changes the user has not seen), or a path GitHub does not list (400).
fn check_viewed_request(
    data: &PrData,
    path: &str,
    client_head: &str,
    current_head: &str,
) -> Result<(), AppError> {
    if current_head != client_head || current_head != data.summary.head.sha {
        return Err(AppError::stale_head(
            "the pull request has new commits; refresh before marking files as viewed",
        ));
    }
    check_github_path(data, path)
}

/// Rejects a path GitHub does not list for the PR (400): ours only (our rename detection may
/// differ), made up, or the old path of a renamed file (GitHub lists the new one; it takes
/// comments on the old one too, but files them there with lines that make no sense, spike
/// S12).
fn check_github_path(data: &PrData, path: &str) -> Result<(), AppError> {
    if !data
        .viewed
        .lock()
        .expect("viewed map poisoned")
        .contains_key(path)
    {
        return Err(AppError::bad_request(format!(
            "{path:?} is not a file of this pull request"
        )));
    }
    Ok(())
}

struct Cached {
    /// When the pipeline run that produced `data` started.
    started: Instant,
    finished: Instant,
    data: Arc<PrData>,
}

type CacheSlot = Arc<tokio::sync::Mutex<Option<Cached>>>;

pub struct PrService {
    repos: Repos,
    github: Github,
    tokens: Tokens,
    /// None if the state database could not be opened; viewed marks still work.
    state: Option<StateDb>,
    /// None if the index database could not be opened; a file's symbols are then parsed on demand,
    /// without a cache, and nothing else of code navigation works.
    index: Option<Indexer>,
    /// The files whose parse timed out lately, so that they are not parsed again at once (see [`PrService::symbols`]).
    slow_parses: nav::SlowParses,
    cache: Mutex<HashMap<PrKey, CacheSlot>>,
    /// Detached work after a response (see [`PrService::set_viewed`]), awaited on shutdown.
    bookkeeping: Mutex<tokio::task::JoinSet<()>>,
    /// One review write at a time per PR (see `PrService::review_write`).
    review_writes: Mutex<HashMap<PrKey, Arc<tokio::sync::Mutex<()>>>>,
}

fn git_err(e: GitFailure) -> AppError {
    AppError::git(e.to_string())
}

impl PrService {
    pub fn new(repos: Repos, state: Option<StateDb>, index: Option<IndexDb>) -> Self {
        PrService {
            repos,
            github: Github::new(),
            tokens: Tokens::default(),
            state,
            index: index.map(|db| Indexer::new(db, Limits::default())),
            slow_parses: Default::default(),
            cache: Default::default(),
            bookkeeping: Default::default(),
            review_writes: Default::default(),
        }
    }

    fn slot(&self, key: &PrKey) -> CacheSlot {
        self.cache
            .lock()
            .expect("PR cache poisoned")
            .entry(key.clone())
            .or_default()
            .clone()
    }

    /// The last loaded snapshot, however old (waits for a load in progress).
    async fn cached(&self, key: &PrKey) -> Option<Arc<PrData>> {
        self.slot(key).lock().await.as_ref().map(|c| c.data.clone())
    }

    /// Returns the PR summary and file list, reusing a result younger than 30 s unless
    /// `refresh` is set (then only a run started at or after the request, give or take
    /// [`REFRESH_COALESCE`], is reused). Concurrent requests for the same PR share one
    /// computation. The computation runs in a detached task that owns the slot lock, so a
    /// client disconnect never cancels git mid-fetch; waiters then see its result.
    pub async fn open(
        self: &Arc<Self>,
        host: &str,
        owner: &str,
        repo: &str,
        number: u64,
        refresh: bool,
    ) -> Result<Arc<PrData>, AppError> {
        let requested = Instant::now();
        let key = PrKey {
            host: host.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
        };
        let mut slot = self.slot(&key).lock_owned().await;
        if let Some(c) = slot.as_ref() {
            let usable = if refresh {
                c.started + REFRESH_COALESCE >= requested
            } else {
                c.finished.elapsed() < CACHE_TTL
            };
            if usable {
                return Ok(c.data.clone());
            }
        }
        let service = Arc::clone(self);
        let task = tokio::spawn(async move {
            let started = Instant::now();
            let data = Arc::new(
                service
                    .load(&key.host, &key.owner, &key.repo, key.number)
                    .await?,
            );
            *slot = Some(Cached {
                started,
                finished: Instant::now(),
                data: data.clone(),
            });
            Ok(data)
        });
        task.await
            .map_err(|e| AppError::git(format!("loading the pull request failed: {e}")))?
    }

    async fn load(
        &self,
        host: &str,
        owner: &str,
        repo_name: &str,
        number: u64,
    ) -> Result<PrData, AppError> {
        let token = self.tokens.get(host).await?;
        // 1. GraphQL: PR metadata and the (paginated) file list. OIDs are validated there.
        let pr = self
            .github
            .pull_request(host, &token, owner, repo_name, number)
            .await?;
        // 2. REST compare: GitHub's merge base for the three-dot diff.
        let merge_base = self
            .github
            .merge_base(host, &token, owner, repo_name, &pr.base_oid, &pr.head_oid)
            .await?;
        let head = pr.head_oid.clone();
        let auth = GitAuth {
            host: host.to_string(),
            token,
        };

        let repo = self.repos.repo(host, owner, repo_name);
        let mut guard = repo.lock().await.map_err(git_err)?;

        // 3. Shallow, blob-less fetch of both commits into refs/dr/pull/<n>/{base,head}.
        if repo.pull_refs(number).await != Some((merge_base.clone(), head.clone())) {
            repo.fetch_commits(&guard, &auth, number, &merge_base, &head)
                .await
                .map_err(git_err)?;
        }

        // 4–7. Changed blobs, renames, line counts and generated classification.
        let mut files = range_files(&repo, &mut guard, &auth, &merge_base, &head).await?;
        drop(guard);

        let viewed: HashMap<String, ViewedState> = pr
            .files
            .iter()
            .map(|f| {
                (
                    f.path.clone(),
                    f.viewer_viewed_state.unwrap_or(ViewedState::Unviewed),
                )
            })
            .collect();
        let gh_counts: HashMap<&str, (u64, u64)> = pr
            .files
            .iter()
            .map(|f| (f.path.as_str(), (f.additions, f.deletions)))
            .collect();
        for f in &mut files {
            // numstat always reports text files; fall back to GitHub's counts if it did not.
            if !f.binary
                && f.additions.is_none()
                && let Some(&(a, d)) = gh_counts.get(f.path.as_str())
            {
                f.additions = Some(a);
                f.deletions = Some(d);
            }
        }

        Ok(PrData {
            summary: PrSummary {
                host: host.to_string(),
                owner: owner.to_string(),
                repo: repo_name.to_string(),
                number,
                title: pr.title,
                author: pr.author,
                state: pr.state,
                url: pr.url,
                base: RefInfo {
                    name: pr.base_ref,
                    sha: pr.base_oid,
                },
                head: RefInfo {
                    name: pr.head_ref,
                    sha: pr.head_oid,
                },
                merge_base,
                node_id: pr.id,
                viewer: pr.viewer,
            },
            files,
            viewed: Mutex::new(viewed),
            tree: Default::default(),
        })
    }

    /// Marks or unmarks `path` as viewed on GitHub. `head` is the head the client shows: if
    /// the PR has moved on, GitHub would also mark changes the user has not seen, so this
    /// fails with `stale_head` instead. Once every non-generated file is viewed, the head is
    /// remembered as seen (the fallback baseline for "since your last review").
    pub async fn set_viewed(
        self: &Arc<Self>,
        key: &PrKey,
        path: &str,
        viewed: bool,
        head: &str,
    ) -> Result<ViewedState, AppError> {
        let token = self.tokens.get(&key.host).await?;
        let current = self
            .github
            .head_oid(&key.host, &token, &key.owner, &key.repo, key.number)
            .await?;
        if current != head {
            return Err(AppError::stale_head(
                "the pull request has new commits; refresh before marking files as viewed",
            ));
        }
        let data = self.snapshot_for_write(key, &current).await?;
        check_viewed_request(&data, path, head, &current)?;
        self.github
            .set_viewed(&key.host, &token, &data.summary.node_id, path, viewed)
            .await?;
        let state = if viewed {
            ViewedState::Viewed
        } else {
            ViewedState::Unviewed
        };
        data.viewed
            .lock()
            .expect("viewed map poisoned")
            .insert(path.to_string(), state);

        // Bookkeeping that must not delay (or fail) the response.
        let service = Arc::clone(self);
        let key = key.clone();
        let path = path.to_string();
        let mut tasks = self.bookkeeping.lock().expect("bookkeeping poisoned");
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            if let Err(e) = service.after_set_viewed(&key, data, &path, state).await {
                eprintln!("gh-tideline: recording the viewed state failed: {e}");
            }
        });
        Ok(state)
    }

    async fn after_set_viewed(
        &self,
        key: &PrKey,
        data: Arc<PrData>,
        path: &str,
        state: ViewedState,
    ) -> Result<(), String> {
        // A refresh that ran concurrently may have read the mark from before the mutation,
        // and the marks of later requests land in the snapshot now cached.
        let target = match self.cached(key).await {
            Some(current) if current.summary.head.sha == data.summary.head.sha => current,
            _ => data,
        };
        target
            .viewed
            .lock()
            .expect("viewed map poisoned")
            .insert(path.to_string(), state);
        if state != ViewedState::Viewed || !target.fully_viewed() {
            return Ok(());
        }
        let Some(db) = &self.state else {
            return Ok(());
        };
        let head = &target.summary.head.sha;
        if db.seen_head(key).await?.as_ref() == Some(head) {
            return Ok(());
        }
        // Our marks may be stale (another tab, github.com). Recording a head the user has not
        // fully seen would hide changes later, so confirm with GitHub, once per head.
        let token = self.tokens.get(&key.host).await.map_err(|e| e.message)?;
        let fresh = self
            .github
            .pull_request(&key.host, &token, &key.owner, &key.repo, key.number)
            .await
            .map_err(|e| e.message)?;
        let marks: HashMap<String, ViewedState> = fresh
            .files
            .into_iter()
            .map(|f| {
                (
                    f.path,
                    f.viewer_viewed_state.unwrap_or(ViewedState::Unviewed),
                )
            })
            .collect();
        let markable = target.viewed.lock().expect("viewed map poisoned").clone();
        if fresh.head_oid != *head || !fully_viewed(&target.files, &markable, &marks) {
            return Ok(());
        }
        let repo = self.repos.repo(&key.host, &key.owner, &key.repo);
        let guard = repo.lock().await.map_err(|e| e.to_string())?;
        repo.protect_commit(&guard, key.number, head)
            .await
            .map_err(|e| e.to_string())?;
        drop(guard);
        db.set_seen_head(key, head, crate::clock::unix_now()).await
    }

    /// A snapshot of `head` (GitHub's current one) to check a write against. Any snapshot of
    /// it will do, past its TTL too: only its file list and node id are needed, and GitHub
    /// itself rejects a path that is no longer part of the PR. Without one, load the PR afresh
    /// (a merely recent snapshot may still have another head); that may return a newer head.
    async fn snapshot_for_write(
        self: &Arc<Self>,
        key: &PrKey,
        head: &str,
    ) -> Result<Arc<PrData>, AppError> {
        match self.cached(key).await {
            Some(data) if data.summary.head.sha == head => Ok(data),
            _ => {
                self.open(&key.host, &key.owner, &key.repo, key.number, true)
                    .await
            }
        }
    }

    /// The snapshot of `head`: a cached one however old (the listing of a head never
    /// changes), else a regular [`PrService::open`], which may return another head.
    pub async fn open_at(
        self: &Arc<Self>,
        key: &PrKey,
        head: &str,
    ) -> Result<Arc<PrData>, AppError> {
        match self.cached(key).await {
            Some(data) if data.summary.head.sha == head => Ok(data),
            _ => {
                self.open(&key.host, &key.owner, &key.repo, key.number, false)
                    .await
            }
        }
    }

    /// Every file of the head tree of `data` (the full project view), listed once per
    /// snapshot. Its OIDs become fetchable through [`PrService::blob`] like the changed files'.
    pub async fn tree(&self, data: &PrData) -> Result<Arc<Vec<TreeEntry>>, AppError> {
        let s = &data.summary;
        data.tree
            .get_or_try_init(|| async {
                let repo = self.repos.repo(&s.host, &s.owner, &s.repo);
                let entries = repo.ls_tree(&s.head.sha).await.map_err(git_err)?;
                repo.remember_oids(
                    entries
                        .iter()
                        .map(|e| (e.oid.as_str(), e.mode != GITLINK_MODE)),
                );
                Ok(Arc::new(entries))
            })
            .await
            .cloned()
    }

    /// Raw bytes of a blob from the cache repo. A local read comes first and takes no lock.
    /// Only a blob listed in a PR opened by this process (a changed file, or any file of its
    /// head tree once that was listed) is lazily fetched on a local miss,
    /// under the repo lock in a detached task (so a client disconnect neither kills git nor
    /// releases the lock early). Gitlinks, unknown OIDs and non-blob objects are 404.
    pub async fn blob(
        &self,
        host: &str,
        owner: &str,
        repo_name: &str,
        oid: &str,
    ) -> Result<Vec<u8>, AppError> {
        let repo = self.repos.repo(host, owner, repo_name);
        if !repo.exists() {
            return Err(AppError::not_found(format!(
                "{host}/{owner}/{repo_name} has not been opened yet"
            )));
        }
        let lookup = match repo.read_object(None, None, oid).await.map_err(git_err)? {
            ObjectLookup::Missing => match repo.known_oid(oid) {
                None => {
                    return Err(AppError::not_found(format!(
                        "object {oid} is not in any pull request opened in this session"
                    )));
                }
                Some(false) => {
                    return Err(AppError::not_found(format!(
                        "{oid} is a submodule commit; it has no file contents"
                    )));
                }
                Some(true) => {
                    let auth = self.tokens.get(host).await.ok().map(|token| GitAuth {
                        host: host.to_string(),
                        token,
                    });
                    let oid = oid.to_string();
                    tokio::spawn(async move {
                        let guard = repo.lock().await?;
                        repo.read_object(Some(&guard), auth.as_ref(), &oid).await
                    })
                    .await
                    .map_err(|e| AppError::git(format!("reading the blob failed: {e}")))?
                    .map_err(git_err)?
                }
            },
            found => found,
        };
        match lookup {
            ObjectLookup::Found { kind, data } if kind == "blob" => Ok(data),
            ObjectLookup::Found { kind, .. } => Err(AppError::not_found(format!(
                "object {oid} is a {kind}, not a blob"
            ))),
            ObjectLookup::Missing => Err(AppError::not_found(format!("blob {oid} not found"))),
        }
    }

    /// Tells the index builds to stop, waits for detached bookkeeping, then for the builds, then for
    /// in-flight git work (see [`Repos::drain`]). The builds are told first: one that waits for the
    /// repo lock that bookkeeping holds should not start fetching when it gets it. Bookkeeping still
    /// running at the deadline is cancelled, so it cannot start git work that the repo drain no
    /// longer waits for.
    pub async fn drain(&self, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        if let Some(index) = &self.index {
            index.shut_down();
        }
        let mut tasks =
            std::mem::take(&mut *self.bookkeeping.lock().expect("bookkeeping poisoned"));
        let _ = tokio::time::timeout_at(deadline, async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        tasks.shutdown().await;
        if let Some(index) = &self.index {
            index
                .drain(deadline.saturating_duration_since(tokio::time::Instant::now()))
                .await;
        }
        self.repos
            .drain(deadline.saturating_duration_since(tokio::time::Instant::now()))
            .await;
    }
}

/// Steps 4–7 of opening a PR, for any two local tree-ishes `from` and `to`: batch-fetch
/// the changed blobs (plus the `.gitattributes` in `to` that can affect them) once per
/// pair, then detect renames, count lines and classify generated files. The listed OIDs
/// are registered so the blob endpoint may fetch them lazily.
async fn range_files(
    repo: &Repo,
    guard: &mut RepoGuard,
    auth: &GitAuth,
    from: &str,
    to: &str,
) -> Result<Vec<ChangedFile>, AppError> {
    let pair = (from.to_string(), to.to_string());
    if !guard.state.blobs_fetched.contains(&pair) {
        let raw = repo
            .diff_tree(auth, &["--no-renames"], from, to)
            .await
            .map_err(git_err)?;
        let mut oids: Vec<String> = Vec::new();
        for f in &raw {
            if f.old_mode != GITLINK_MODE {
                oids.extend(f.old_oid.clone());
            }
            if f.new_mode != GITLINK_MODE {
                oids.extend(f.new_oid.clone());
            }
        }
        // Paths present in `to`: all their ancestors are trees there.
        let paths: Vec<&str> = raw
            .iter()
            .filter(|f| f.new_oid.is_some())
            .map(|f| f.path.as_str())
            .collect();
        oids.extend(repo.gitattributes_oids(to, &paths).await.map_err(git_err)?);
        oids.sort();
        oids.dedup();
        repo.fetch_blobs(guard, auth, &oids, STALL_PR)
            .await
            .map_err(git_err)?;
        guard.state.blobs_fetched.insert(pair);
    }

    let mut files = repo
        .diff_tree(auth, &["-M", "--numstat"], from, to)
        .await
        .map_err(git_err)?;

    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let attrs = repo.check_attr(auth, to, &paths).await.map_err(git_err)?;
    for f in &mut files {
        f.generated = is_generated(&f.path, attrs.get(&f.path).map(Vec::as_slice));
    }

    repo.remember_oids(files.iter().flat_map(|f| {
        [
            (f.old_oid.as_deref(), f.old_mode != GITLINK_MODE),
            (f.new_oid.as_deref(), f.new_mode != GITLINK_MODE),
        ]
        .into_iter()
        .filter_map(|(oid, is_blob)| oid.map(|o| (o, is_blob)))
    }));
    Ok(files)
}

const BUILTIN_GENERATED: &[&str] = &[
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "go.sum",
    "Cargo.lock",
    "*.min.js",
    "*.min.css",
    "*.map",
    "*.snap",
];

/// Built-in generated-file rules, matched against the file's basename.
pub fn builtin_generated(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    BUILTIN_GENERATED
        .iter()
        .any(|pat| match pat.strip_prefix('*') {
            Some(suffix) => name.len() > suffix.len() && name.ends_with(suffix),
            None => name == *pat,
        })
}

/// Combines `.gitattributes` with the built-in rules. An explicit
/// `linguist-generated`/`linguist-vendored` setting wins; `-linguist-generated` or
/// `linguist-generated=false` opts a file out of the built-in rules.
pub fn is_generated(path: &str, attrs: Option<&[(String, String)]>) -> bool {
    let mut explicit_false = false;
    for (attr, value) in attrs.unwrap_or_default() {
        match value.as_str() {
            "set" | "true" => return true,
            "unset" | "false" if attr == "linguist-generated" => explicit_false = true,
            _ => {}
        }
    }
    !explicit_false && builtin_generated(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_patterns() {
        for path in [
            "Cargo.lock",
            "sub/dir/Cargo.lock",
            "Gemfile.lock",
            "web/package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "go.sum",
            "dist/app.min.js",
            "dist/app.min.css",
            "dist/app.js.map",
            "__snapshots__/x.test.ts.snap",
        ] {
            assert!(builtin_generated(path), "{path}");
        }
        for path in [
            "src/lock.rs",
            "lock",
            ".lock",
            "go.sum.txt",
            "app.js",
            "min.js",
            "styles.css",
            "Cargo.toml",
            "package.json",
            "snap/README.md",
            "sitemap.xml",
        ] {
            assert!(!builtin_generated(path), "{path}");
        }
    }

    fn attrs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, v)| (a.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn gitattributes_override_builtin_rules() {
        let unspecified = attrs(&[
            ("linguist-generated", "unspecified"),
            ("linguist-vendored", "unspecified"),
        ]);
        assert!(is_generated("Cargo.lock", Some(&unspecified)));
        assert!(!is_generated("src/main.rs", Some(&unspecified)));
        assert!(!is_generated("src/main.rs", None));

        let generated = attrs(&[("linguist-generated", "set")]);
        assert!(is_generated("api/gen.pb.go", Some(&generated)));
        let generated_true = attrs(&[("linguist-generated", "true")]);
        assert!(is_generated("api/gen.pb.go", Some(&generated_true)));
        let vendored = attrs(&[
            ("linguist-generated", "unspecified"),
            ("linguist-vendored", "set"),
        ]);
        assert!(is_generated("vendor/lib.c", Some(&vendored)));

        let opted_out = attrs(&[("linguist-generated", "unset")]);
        assert!(!is_generated("Cargo.lock", Some(&opted_out)));
        let opted_out_false = attrs(&[("linguist-generated", "false")]);
        assert!(!is_generated("yarn.lock", Some(&opted_out_false)));
    }

    fn file(path: &str, generated: bool) -> ChangedFile {
        ChangedFile {
            path: path.to_string(),
            old_path: None,
            status: "M".to_string(),
            old_oid: None,
            new_oid: None,
            old_mode: "100644".to_string(),
            new_mode: "100644".to_string(),
            additions: None,
            deletions: None,
            binary: false,
            generated,
        }
    }

    fn marks(entries: &[(&str, ViewedState)]) -> HashMap<String, ViewedState> {
        entries.iter().map(|(p, s)| (p.to_string(), *s)).collect()
    }

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";

    fn data(files: Vec<ChangedFile>, viewed: HashMap<String, ViewedState>) -> PrData {
        let r = |sha: &str| RefInfo {
            name: "x".to_string(),
            sha: sha.to_string(),
        };
        PrData {
            summary: PrSummary {
                host: "github.com".to_string(),
                owner: "o".to_string(),
                repo: "r".to_string(),
                number: 1,
                title: String::new(),
                author: String::new(),
                state: "OPEN".to_string(),
                url: String::new(),
                base: r(OTHER),
                head: r(HEAD),
                merge_base: OTHER.to_string(),
                node_id: "PR_x".to_string(),
                viewer: "me".to_string(),
            },
            files,
            viewed: Mutex::new(viewed),
            tree: Default::default(),
        }
    }

    #[test]
    fn fully_viewed_needs_every_markable_non_generated_file() {
        use ViewedState::*;
        let files = vec![file("a", false), file("b", false), file("Cargo.lock", true)];
        let own = |m: &HashMap<String, ViewedState>| fully_viewed(&files, m, m);
        // One file viewed is not enough.
        assert!(!own(&marks(&[("a", Viewed), ("b", Unviewed)])));
        // Dismissed means changed since it was viewed.
        assert!(!own(&marks(&[("a", Viewed), ("b", Dismissed)])));
        // Generated files are not required.
        assert!(own(&marks(&[
            ("a", Viewed),
            ("b", Viewed),
            ("Cargo.lock", Unviewed)
        ])));
        // Paths only GitHub lists (a rename it sees as delete + add) have no checkbox here.
        assert!(own(&marks(&[
            ("a", Viewed),
            ("b", Viewed),
            ("old/b", Unviewed)
        ])));
        // Our files GitHub does not list cannot be marked either.
        assert!(own(&marks(&[("a", Viewed)])));
        // Checked against a fresh list: a markable path it lacks (truncated) is not viewed.
        let markable = marks(&[("a", Viewed), ("b", Viewed)]);
        assert!(!fully_viewed(&files, &markable, &marks(&[("a", Viewed)])));
        assert!(!fully_viewed(&files, &markable, &HashMap::new()));
        assert!(fully_viewed(
            &files,
            &markable,
            &marks(&[("a", Viewed), ("b", Viewed)])
        ));
    }

    #[test]
    fn viewed_requests_are_checked_against_the_head_and_github_paths() {
        use crate::error::ErrorCode;
        let d = data(
            vec![file("a", false)],
            marks(&[("a", ViewedState::Unviewed)]),
        );
        let code = |r: Result<(), AppError>| r.map_err(|e| e.code);
        assert_eq!(code(check_viewed_request(&d, "a", HEAD, HEAD)), Ok(()));
        // The page shows an older head, or GitHub has moved on since the snapshot.
        assert_eq!(
            code(check_viewed_request(&d, "a", OTHER, HEAD)),
            Err(ErrorCode::StaleHead)
        );
        assert_eq!(
            code(check_viewed_request(&d, "a", OTHER, OTHER)),
            Err(ErrorCode::StaleHead)
        );
        // A path GitHub does not list (ours only, or made up).
        assert_eq!(
            code(check_viewed_request(&d, "../a", HEAD, HEAD)),
            Err(ErrorCode::BadRequest)
        );
    }

    #[tokio::test]
    async fn open_at_serves_a_snapshot_of_that_head_past_its_ttl() {
        // Never touched: a cache hit reads neither the repo nor GitHub.
        let root = std::env::temp_dir().join("tideline-open-at-unused");
        let service = Arc::new(PrService::new(Repos::new(root), None, None));
        let key = PrKey {
            host: "github.com".to_string(),
            owner: "o".to_string(),
            repo: "r".to_string(),
            number: 1,
        };
        let old = Instant::now()
            .checked_sub(CACHE_TTL * 2)
            .expect("monotonic clock older than a minute");
        let snapshot = Arc::new(data(vec![file("a", false)], HashMap::new()));
        *service.slot(&key).lock().await = Some(Cached {
            started: old,
            finished: old,
            data: snapshot.clone(),
        });
        let got = service.open_at(&key, HEAD).await.expect("cached snapshot");
        assert!(Arc::ptr_eq(&got, &snapshot));
    }
}
