//! Opening a PR: GitHub metadata + cache-repo fetch + diff, with a short in-memory cache.

pub mod target;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::AppError;
use crate::git::parse::GITLINK_MODE;
use crate::git::{ChangedFile, GitAuth, GitFailure, ObjectLookup, Repos};
use crate::github::{Github, Tokens};

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
}

pub struct PrData {
    pub summary: PrSummary,
    pub files: Vec<ChangedFile>,
}

type PrKey = (String, String, String, u64);

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
    cache: std::sync::Mutex<HashMap<PrKey, CacheSlot>>,
}

fn git_err(e: GitFailure) -> AppError {
    AppError::git(e.to_string())
}

impl PrService {
    pub fn new(repos: Repos) -> Self {
        PrService {
            repos,
            github: Github::new(),
            tokens: Tokens::default(),
            cache: Default::default(),
        }
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
        let key = (
            host.to_string(),
            owner.to_string(),
            repo.to_string(),
            number,
        );
        let slot = self
            .cache
            .lock()
            .expect("PR cache poisoned")
            .entry(key.clone())
            .or_default()
            .clone();
        let mut slot = slot.lock_owned().await;
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
            let (host, owner, repo, number) = key;
            let started = Instant::now();
            let data = Arc::new(service.load(&host, &owner, &repo, number).await?);
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

        // 4–5. Collect changed blob OIDs (+ relevant .gitattributes) and batch-fetch them.
        let pair = (merge_base.clone(), head.clone());
        if !guard.state.blobs_fetched.contains(&pair) {
            let raw = repo
                .diff_tree(&auth, &["--no-renames"], &merge_base, &head)
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
            // Paths present in head: all their ancestors are trees there.
            let paths: Vec<&str> = raw
                .iter()
                .filter(|f| f.new_oid.is_some())
                .map(|f| f.path.as_str())
                .collect();
            oids.extend(
                repo.gitattributes_oids(&head, &paths)
                    .await
                    .map_err(git_err)?,
            );
            oids.sort();
            oids.dedup();
            repo.fetch_blobs(&guard, &auth, &oids)
                .await
                .map_err(git_err)?;
            guard.state.blobs_fetched.insert(pair);
        }

        // 6. Rename detection and line counts, now that blobs are local.
        let mut files = repo
            .diff_tree(&auth, &["-M", "--numstat"], &merge_base, &head)
            .await
            .map_err(git_err)?;

        // 7. Generated / vendored classification.
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        let attrs = repo
            .check_attr(&auth, &head, &paths)
            .await
            .map_err(git_err)?;
        drop(guard);

        repo.remember_oids(files.iter().flat_map(|f| {
            [
                (f.old_oid.as_deref(), f.old_mode != GITLINK_MODE),
                (f.new_oid.as_deref(), f.new_mode != GITLINK_MODE),
            ]
            .into_iter()
            .filter_map(|(oid, is_blob)| oid.map(|o| (o, is_blob)))
        }));

        let gh_counts: HashMap<&str, (u64, u64)> = pr
            .files
            .iter()
            .map(|f| (f.path.as_str(), (f.additions, f.deletions)))
            .collect();
        for f in &mut files {
            f.generated = is_generated(&f.path, attrs.get(&f.path).map(Vec::as_slice));
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
            },
            files,
        })
    }

    /// Raw bytes of a blob from the cache repo. A local read comes first and takes no lock.
    /// Only a blob listed in a PR opened by this process is lazily fetched on a local miss,
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

    /// Waits for in-flight git work (see [`Repos::drain`]).
    pub async fn drain(&self, timeout: Duration) {
        self.repos.drain(timeout).await;
    }
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
}
