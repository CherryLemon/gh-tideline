//! GitHub access: per-host token resolution, hand-written GraphQL query and REST compare.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::error::{AppError, ErrorCode};

const TOKEN_TTL: Duration = Duration::from_secs(600);

/// Resolves and briefly caches tokens per host, following gh's precedence rules.
#[derive(Default)]
pub struct Tokens {
    cache: std::sync::Mutex<HashMap<String, (Instant, String)>>,
}

impl Tokens {
    pub async fn get(&self, host: &str) -> Result<String, AppError> {
        if let Some((at, token)) = self.cache.lock().expect("token cache poisoned").get(host)
            && at.elapsed() < TOKEN_TTL
        {
            return Ok(token.clone());
        }
        let token = resolve_token(host).await?;
        self.cache
            .lock()
            .expect("token cache poisoned")
            .insert(host.to_string(), (Instant::now(), token.clone()));
        Ok(token)
    }
}

async fn resolve_token(host: &str) -> Result<String, AppError> {
    let vars: &[&str] = if host == "github.com" {
        &["GH_TOKEN", "GITHUB_TOKEN"]
    } else {
        &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]
    };
    for var in vars {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return Ok(v.trim().to_string());
        }
    }
    let no_token = || {
        AppError::new(
            ErrorCode::NoToken,
            format!("no GitHub token for {host}; run `gh auth login --hostname {host}`"),
        )
    };
    let out = tokio::process::Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|_| no_token())?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || token.is_empty() {
        return Err(no_token());
    }
    Ok(token)
}

fn graphql_url(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com/graphql".into()
    } else {
        format!("https://{host}/api/graphql")
    }
}

fn rest_base(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com".into()
    } else {
        format!("https://{host}/api/v3")
    }
}

const PR_QUERY: &str = r#"
query($owner: String!, $repo: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      id
      title
      state
      url
      author { login }
      baseRefName
      baseRefOid
      headRefName
      headRefOid
      files(first: 100, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes { path additions deletions changeType }
      }
    }
  }
}
"#;

#[derive(Debug, Clone)]
pub struct PullRequest {
    pub id: String,
    pub title: String,
    pub state: String,
    pub url: String,
    pub author: String,
    pub base_ref: String,
    pub base_oid: String,
    pub head_ref: String,
    pub head_oid: String,
    pub files: Vec<GhFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Deserialize)]
struct GqlResponse {
    data: Option<GqlData>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct GqlData {
    repository: Option<GqlRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlRepository {
    pull_request: Option<GqlPullRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPullRequest {
    id: String,
    title: String,
    state: String,
    url: String,
    author: Option<GqlActor>,
    base_ref_name: String,
    base_ref_oid: String,
    head_ref_name: String,
    head_ref_oid: String,
    files: Option<GqlFiles>,
}

#[derive(Deserialize)]
struct GqlActor {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlFiles {
    page_info: GqlPageInfo,
    nodes: Option<Vec<GhFile>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

pub struct Github {
    http: reqwest::Client,
}

impl Github {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("tideline/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("building HTTP client");
        Github { http }
    }

    /// Step 1: PR metadata plus every page of `files`.
    pub async fn pull_request(
        &self,
        host: &str,
        token: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<PullRequest, AppError> {
        let mut cursor: Option<String> = None;
        let mut result: Option<PullRequest> = None;
        loop {
            let body = json!({
                "query": PR_QUERY,
                "variables": { "owner": owner, "repo": repo, "number": number, "cursor": cursor },
            });
            let resp = self
                .http
                .post(graphql_url(host))
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|e| AppError::github(format!("GraphQL request to {host} failed: {e}")))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| AppError::github(format!("reading GraphQL response: {e}")))?;
            if !status.is_success() {
                return Err(http_error("GraphQL", status, &text));
            }
            let parsed: GqlResponse = serde_json::from_str(&text)
                .map_err(|e| AppError::github(format!("unexpected GraphQL response: {e}")))?;
            if let Some(errors) = parsed.errors.filter(|e| !e.is_empty()) {
                let message = errors
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(
                    if errors
                        .iter()
                        .any(|e| e.kind.as_deref() == Some("NOT_FOUND"))
                    {
                        AppError::not_found(message)
                    } else {
                        AppError::github(message)
                    },
                );
            }
            let pr = parsed
                .data
                .and_then(|d| d.repository)
                .and_then(|r| r.pull_request)
                .ok_or_else(|| {
                    AppError::not_found(format!("pull request {owner}/{repo}#{number} not found"))
                })?;
            for oid in [&pr.base_ref_oid, &pr.head_ref_oid] {
                check_oid(oid)?;
            }
            let (nodes, page_info) = match pr.files {
                Some(f) => (f.nodes.unwrap_or_default(), Some(f.page_info)),
                None => (Vec::new(), None),
            };
            let entry = result.get_or_insert_with(|| PullRequest {
                id: pr.id,
                title: pr.title,
                state: pr.state,
                url: pr.url,
                author: pr.author.map(|a| a.login).unwrap_or_else(|| "ghost".into()),
                base_ref: pr.base_ref_name,
                base_oid: pr.base_ref_oid,
                head_ref: pr.head_ref_name,
                head_oid: pr.head_ref_oid,
                files: Vec::new(),
            });
            entry.files.extend(nodes);
            match page_info {
                Some(p) if p.has_next_page && p.end_cursor.is_some() => cursor = p.end_cursor,
                _ => break,
            }
        }
        Ok(result.expect("loop runs at least once"))
    }

    /// Step 2: GitHub's merge base for the three-dot diff.
    pub async fn merge_base(
        &self,
        host: &str,
        token: &str,
        owner: &str,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<String, AppError> {
        #[derive(Deserialize)]
        struct Compare {
            merge_base_commit: Commit,
        }
        #[derive(Deserialize)]
        struct Commit {
            sha: String,
        }
        let url = format!(
            "{}/repos/{owner}/{repo}/compare/{base}...{head}?per_page=1",
            rest_base(host)
        );
        let resp = self
            .http
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| AppError::github(format!("compare request to {host} failed: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::github(format!("reading compare response: {e}")))?;
        if !status.is_success() {
            return Err(http_error("compare", status, &text));
        }
        let compare: Compare = serde_json::from_str(&text)
            .map_err(|e| AppError::github(format!("unexpected compare response: {e}")))?;
        check_oid(&compare.merge_base_commit.sha)?;
        Ok(compare.merge_base_commit.sha)
    }
}

/// Commit ids from GitHub end up in git arguments and refspecs; reject anything else.
fn check_oid(oid: &str) -> Result<(), AppError> {
    if crate::git::is_oid(oid) {
        Ok(())
    } else {
        Err(AppError::github(format!(
            "GitHub returned an invalid commit id {:?}",
            oid.chars().take(80).collect::<String>()
        )))
    }
}

fn http_error(what: &str, status: reqwest::StatusCode, body: &str) -> AppError {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or_else(|| body.chars().take(200).collect());
    let message = format!("GitHub {what} returned {status}: {detail}");
    if status == reqwest::StatusCode::NOT_FOUND {
        AppError::not_found(message)
    } else {
        AppError::github(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    #[test]
    fn commit_ids_from_github_are_validated() {
        assert!(check_oid("0123456789abcdef0123456789abcdef01234567").is_ok());
        assert!(check_oid(&"b".repeat(64)).is_ok());
        for bad in [
            "",
            "HEAD",
            "--upload-pack=x",
            "0123456789abcdef0123456789abcdef0123456",
            "0123456789ABCDEF0123456789abcdef01234567",
            "0123456789abcdef0123456789abcdef01234567:refs/heads/x",
        ] {
            assert_eq!(
                check_oid(bad).map_err(|e| e.code),
                Err(ErrorCode::GithubError),
                "{bad:?}"
            );
        }
    }
}
