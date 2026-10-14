//! GitHub access: per-host token resolution, hand-written GraphQL query and REST compare.

mod review;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{AppError, ErrorCode};

pub use review::{
    DiffSide, NewThread, PendingReview, ReviewComment, ReviewState, ReviewThread, ReviewThreads,
    SubmittedReview, ThreadTarget,
};

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
  viewer { login }
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
        nodes { path additions deletions changeType viewerViewedState }
      }
    }
  }
}
"#;

const HEAD_QUERY: &str = r#"
query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) { pullRequest(number: $number) { headRefOid } }
}
"#;

const MARK_VIEWED: &str = r#"
mutation($id: ID!, $path: String!) {
  markFileAsViewed(input: { pullRequestId: $id, path: $path }) { clientMutationId }
}
"#;

const UNMARK_VIEWED: &str = r#"
mutation($id: ID!, $path: String!) {
  unmarkFileAsViewed(input: { pullRequestId: $id, path: $path }) { clientMutationId }
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
    /// Login of the token's user.
    pub viewer: String,
    pub files: Vec<GhFile>,
}

/// The viewer's "viewed" mark on a file; DISMISSED means it changed after being viewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ViewedState {
    Viewed,
    Unviewed,
    Dismissed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    /// Absent if GitHub returns a value this version does not know.
    #[serde(default, deserialize_with = "lenient_viewed")]
    pub viewer_viewed_state: Option<ViewedState>,
}

fn lenient_viewed<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<ViewedState>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

#[derive(Deserialize)]
struct GqlResponse<T> {
    data: Option<T>,
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
    viewer: GqlActor,
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
            let data: GqlData = self
                .graphql(
                    host,
                    token,
                    PR_QUERY,
                    json!({ "owner": owner, "repo": repo, "number": number, "cursor": cursor }),
                )
                .await?;
            let viewer = data.viewer.login;
            let pr = data
                .repository
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
                viewer,
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

    /// Posts one GraphQL query and returns its `data`. Any reported error fails the call;
    /// a `NOT_FOUND` error becomes `not_found`.
    async fn graphql<T: DeserializeOwned>(
        &self,
        host: &str,
        token: &str,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T, AppError> {
        self.post_graphql(host, token, query, variables, Op::Read)
            .await
    }

    /// Like `graphql`, for mutations: GitHub refusing the write becomes `rejected` (422) with
    /// GitHub's message, so the UI can say why. Callers must not retry: the write may have
    /// landed.
    async fn mutate<T: DeserializeOwned>(
        &self,
        host: &str,
        token: &str,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T, AppError> {
        self.post_graphql(host, token, query, variables, Op::Write)
            .await
    }

    async fn post_graphql<T: DeserializeOwned>(
        &self,
        host: &str,
        token: &str,
        query: &str,
        variables: serde_json::Value,
        op: Op,
    ) -> Result<T, AppError> {
        let resp = self
            .http
            .post(graphql_url(host))
            .bearer_auth(token)
            .json(&json!({ "query": query, "variables": variables }))
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
        if op == Op::Write {
            return write_result(&text);
        }
        let parsed: GqlResponse<T> = serde_json::from_str(&text)
            .map_err(|e| AppError::github(format!("unexpected GraphQL response: {e}")))?;
        if let Some(errors) = parsed.errors.filter(|e| !e.is_empty()) {
            return Err(classify_gql_errors(&errors, op));
        }
        parsed
            .data
            .ok_or_else(|| AppError::github("GraphQL response has no data"))
    }

    /// The pull request's current head commit, read fresh (no files).
    pub async fn head_oid(
        &self,
        host: &str,
        token: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<String, AppError> {
        #[derive(Deserialize)]
        struct Data {
            repository: Option<Repository>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Repository {
            pull_request: Option<Pr>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Pr {
            head_ref_oid: String,
        }
        let data: Data = self
            .graphql(
                host,
                token,
                HEAD_QUERY,
                json!({ "owner": owner, "repo": repo, "number": number }),
            )
            .await?;
        let oid = data
            .repository
            .and_then(|r| r.pull_request)
            .ok_or_else(|| {
                AppError::not_found(format!("pull request {owner}/{repo}#{number} not found"))
            })?
            .head_ref_oid;
        check_oid(&oid)?;
        Ok(oid)
    }

    /// Marks (`viewed`) or unmarks a file as viewed for the token's user.
    pub async fn set_viewed(
        &self,
        host: &str,
        token: &str,
        pull_request_id: &str,
        path: &str,
        viewed: bool,
    ) -> Result<(), AppError> {
        let query = if viewed { MARK_VIEWED } else { UNMARK_VIEWED };
        let _: serde_json::Value = self
            .mutate(
                host,
                token,
                query,
                json!({ "id": pull_request_id, "path": path }),
            )
            .await?;
        Ok(())
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

    /// `text` rendered as GitHub renders a comment in `owner/repo` (`gfm`: `#12` and `@user`
    /// link there), for a preview. The HTML is GitHub's, sanitized by the client like a
    /// comment body.
    pub async fn render_markdown(
        &self,
        host: &str,
        token: &str,
        owner: &str,
        repo: &str,
        text: &str,
    ) -> Result<String, AppError> {
        let resp = self
            .http
            .post(format!("{}/markdown", rest_base(host)))
            .bearer_auth(token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&json!({ "text": text, "mode": "gfm", "context": format!("{owner}/{repo}") }))
            .send()
            .await
            .map_err(|e| AppError::github(format!("markdown request to {host} failed: {e}")))?;
        let status = resp.status();
        let html = resp
            .text()
            .await
            .map_err(|e| AppError::github(format!("reading markdown response: {e}")))?;
        if !status.is_success() {
            return Err(http_error("markdown", status, &html));
        }
        Ok(html)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Read,
    Write,
}

/// The error for a GraphQL response's `errors`. GitHub refusing a write (`UNPROCESSABLE`:
/// the comment's line isn't in the diff, approving your own PR, ...; `FORBIDDEN`: no
/// permission) is `rejected`; the same on a read is a `github_error` like any other.
/// What a mutation returned. A mutation whose payload came back happened, even when GitHub
/// also reports errors (about some field of the answer): those are logged, not returned, so
/// the client doesn't take a write that landed for one that failed. An answer that can't be
/// read is still an error (502): the client looks before trying again.
fn write_result<T: DeserializeOwned>(text: &str) -> Result<T, AppError> {
    let parsed: GqlResponse<serde_json::Value> = serde_json::from_str(text)
        .map_err(|e| AppError::github(format!("unexpected GraphQL response: {e}")))?;
    let landed = payload_returned(parsed.data.as_ref());
    if let Some(errors) = parsed.errors.filter(|e| !e.is_empty()) {
        if !landed {
            return Err(classify_gql_errors(&errors, Op::Write));
        }
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        eprintln!(
            "gh-tideline: GitHub made a change but reported: {}",
            messages.join("; ")
        );
    }
    let data = parsed
        .data
        .ok_or_else(|| AppError::github("GraphQL response has no data"))?;
    serde_json::from_value(data).map_err(|e| {
        let made = if landed {
            "GitHub made the change, but "
        } else {
            ""
        };
        AppError::github(format!("{made}its answer was unexpected: {e}"))
    })
}

/// Whether some mutation in `data` returned a payload with a value in it.
fn payload_returned(data: Option<&serde_json::Value>) -> bool {
    let Some(data) = data.and_then(serde_json::Value::as_object) else {
        return false;
    };
    data.values().any(|payload| {
        payload
            .as_object()
            .is_some_and(|p| p.values().any(|v| !v.is_null()))
    })
}

fn classify_gql_errors(errors: &[GqlError], op: Op) -> AppError {
    let mut message = errors
        .iter()
        .map(|e| e.message.trim())
        .filter(|m| !m.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    // As for a new review with neither comments nor a summary.
    if message.is_empty() {
        message = "GitHub gave no reason".into();
    }
    let has = |kind: &str| errors.iter().any(|e| e.kind.as_deref() == Some(kind));
    if has("NOT_FOUND") {
        AppError::not_found(message)
    } else if op == Op::Write && (has("UNPROCESSABLE") || has("FORBIDDEN")) {
        AppError::rejected(message)
    } else {
        AppError::github(message)
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

    fn errors(json: &str) -> Vec<GqlError> {
        serde_json::from_str::<GqlResponse<serde_json::Value>>(json)
            .unwrap()
            .errors
            .unwrap()
    }

    #[test]
    fn graphql_errors_are_classified_by_type_and_operation() {
        // As GitHub sends it for a comment on a line outside the diff (spike S12).
        let unprocessable = errors(
            r#"{"data": {"addPullRequestReviewThread": null}, "errors": [{"type": "UNPROCESSABLE",
                "path": ["addPullRequestReviewThread"], "locations": [{"line": 2, "column": 3}],
                "message": "Line could not be resolved"}]}"#,
        );
        let forbidden = errors(
            r#"{"errors": [{"type": "FORBIDDEN", "message": "Resource not accessible by integration"}]}"#,
        );
        let not_found = errors(
            r#"{"errors": [{"type": "NOT_FOUND", "message": "Could not resolve to a node"},
                {"type": "UNPROCESSABLE", "message": "x"}]}"#,
        );
        let untyped =
            errors(r#"{"errors": [{"message": "Something went wrong"}, {"message": "again"}]}"#);
        let silent = errors(r#"{"errors": [{"type": "UNPROCESSABLE", "message": ""}]}"#);

        let classify = |e: &[GqlError], op| {
            let err = classify_gql_errors(e, op);
            (err.code, err.message)
        };
        assert_eq!(
            classify(&unprocessable, Op::Write),
            (ErrorCode::Rejected, "Line could not be resolved".into())
        );
        assert_eq!(classify(&forbidden, Op::Write).0, ErrorCode::Rejected);
        assert_eq!(
            classify(&silent, Op::Write),
            (ErrorCode::Rejected, "GitHub gave no reason".into())
        );
        assert_eq!(classify(&unprocessable, Op::Read).0, ErrorCode::GithubError);
        assert_eq!(classify(&forbidden, Op::Read).0, ErrorCode::GithubError);
        for op in [Op::Read, Op::Write] {
            assert_eq!(classify(&not_found, op).0, ErrorCode::NotFound);
            assert_eq!(
                classify(&untyped, op),
                (ErrorCode::GithubError, "Something went wrong; again".into())
            );
        }
    }

    #[test]
    fn writes_that_returned_a_payload_landed() {
        #[derive(Deserialize, Debug)]
        #[serde(rename_all = "camelCase")]
        struct Data {
            delete_pull_request_review: Option<serde_json::Value>,
        }
        let refused = write_result::<Data>(
            r#"{"data": {"deletePullRequestReview": null}, "errors": [{"type": "UNPROCESSABLE",
                "message": "Can not delete"}]}"#,
        )
        .unwrap_err();
        assert_eq!(refused.code, ErrorCode::Rejected);
        // Done, with an error about some field of the answer: a success.
        let landed = write_result::<Data>(
            r#"{"data": {"deletePullRequestReview": {"pullRequestReview": {"id": "PRR_1"}}},
                "errors": [{"message": "Something went wrong reading a field"}]}"#,
        )
        .unwrap();
        assert!(landed.delete_pull_request_review.is_some());
        // An answer with nothing in it says nothing about the write.
        let empty = write_result::<Data>(
            r#"{"data": {"deletePullRequestReview": {"clientMutationId": null}},
                "errors": [{"type": "FORBIDDEN", "message": "no"}]}"#,
        )
        .unwrap_err();
        assert_eq!(empty.code, ErrorCode::Rejected);
        // Done, but the answer can't be read: an error that says the change was made.
        let odd = write_result::<u32>(
            r#"{"data": {"deletePullRequestReview": {"pullRequestReview": {"id": "PRR_1"}}}}"#,
        )
        .unwrap_err();
        assert_eq!(odd.code, ErrorCode::GithubError);
        assert!(
            odd.message.starts_with("GitHub made the change"),
            "{}",
            odd.message
        );
        assert_eq!(
            write_result::<u32>("not json").unwrap_err().code,
            ErrorCode::GithubError
        );
    }

    #[test]
    fn pull_request_response_with_viewer_and_viewed_states() {
        let body = r#"{"data": {
            "viewer": {"login": "me"},
            "repository": {"pullRequest": {
                "id": "PR_x", "title": "t", "state": "OPEN", "url": "https://github.com/o/r/pull/1",
                "author": null,
                "baseRefName": "main", "baseRefOid": "1111111111111111111111111111111111111111",
                "headRefName": "f", "headRefOid": "2222222222222222222222222222222222222222",
                "files": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": [
                    {"path": "a", "additions": 1, "deletions": 0, "changeType": "MODIFIED", "viewerViewedState": "VIEWED"},
                    {"path": "b", "additions": 0, "deletions": 2, "changeType": "MODIFIED", "viewerViewedState": "DISMISSED"},
                    {"path": "c", "additions": 0, "deletions": 0, "changeType": "ADDED", "viewerViewedState": null},
                    {"path": "d", "additions": 0, "deletions": 0, "changeType": "ADDED", "viewerViewedState": "SOMETHING_NEW"},
                    {"path": "e", "additions": 0, "deletions": 0, "changeType": "ADDED"}
                ]}
            }}
        }}"#;
        let resp: GqlResponse<GqlData> = serde_json::from_str(body).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.viewer.login, "me");
        let pr = data.repository.unwrap().pull_request.unwrap();
        assert!(pr.author.is_none());
        let states: Vec<Option<ViewedState>> = pr
            .files
            .unwrap()
            .nodes
            .unwrap()
            .into_iter()
            .map(|f| f.viewer_viewed_state)
            .collect();
        assert_eq!(
            states,
            [
                Some(ViewedState::Viewed),
                Some(ViewedState::Dismissed),
                None,
                None,
                None
            ]
        );
    }
}
