//! HTTP server: routes, auth flow, API handlers and static assets.

mod assets;
mod nav;
mod review;
pub mod security;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::clock::unix_now;
use crate::error::AppError;
use crate::git::{TreeEntry, is_oid};
use crate::pr::PrService;
use crate::pr::target::valid_name;
use crate::state::PrKey;
use security::{AuthTokens, Policy, Sessions};

/// Lifetime of a browser session (server-side and cookie `Max-Age`).
const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub struct Inner {
    pub policy: Policy,
    tokens: AuthTokens,
    sessions: Sessions,
    pr: Arc<PrService>,
}

pub type AppState = Arc<Inner>;

/// `started_at` (Unix seconds) is the server start: older authorization tokens are refused.
pub fn state(
    port: u16,
    secret: String,
    started_at: u64,
    allowed_hosts: Vec<String>,
    pr: Arc<PrService>,
) -> AppState {
    Arc::new(Inner {
        policy: Policy {
            port,
            cookie_name: security::cookie_name(port),
            allowed_hosts,
        },
        tokens: AuthTokens::new(secret, port, started_at),
        sessions: Sessions::new(SESSION_TTL),
        pr,
    })
}

/// Request bodies: a comment of GitHub's 65536 characters, JSON-escaped, with room to spare.
const BODY_LIMIT: usize = 512 * 1024;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/__auth", get(auth))
        .route("/api/{host}/{owner}/{repo}/pulls/{n}", get(pull))
        .route(
            "/api/{host}/{owner}/{repo}/pulls/{n}/files",
            get(pull_files),
        )
        .route("/api/{host}/{owner}/{repo}/pulls/{n}/tree", get(pull_tree))
        .route(
            "/api/{host}/{owner}/{repo}/pulls/{n}/viewed",
            put(set_viewed),
        )
        .route(
            "/api/{host}/{owner}/{repo}/pulls/{n}/threads",
            get(review::threads).post(review::create_thread),
        )
        .route(
            "/api/{host}/{owner}/{repo}/pulls/{n}/review",
            post(review::submit_review).delete(review::discard_review),
        )
        .route(
            "/api/{host}/{owner}/{repo}/pulls/{n}/threads/{t}/replies",
            post(review::reply),
        )
        .route(
            "/api/{host}/{owner}/{repo}/pulls/{n}/threads/{t}/resolved",
            put(review::set_resolved),
        )
        .route(
            "/api/{host}/{owner}/{repo}/pulls/{n}/threads/{t}/comments/{c}",
            patch(review::edit_comment).delete(review::delete_comment),
        )
        .route("/api/{host}/{owner}/{repo}/blobs/{oid}", get(blob))
        .route(
            "/api/{host}/{owner}/{repo}/blobs/{oid}/symbols",
            get(nav::symbols),
        )
        .route(
            "/api/{host}/{owner}/{repo}/markdown",
            post(review::markdown),
        )
        .fallback(fallback)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            security::middleware,
        ))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "app": "tideline", "version": env!("CARGO_PKG_VERSION") }))
}

#[derive(Deserialize)]
struct AuthQuery {
    t: Option<String>,
    next: Option<String>,
}

/// Redeems a one-time token minted by the CLI and sets a fresh random session cookie.
async fn auth(
    State(state): State<AppState>,
    Query(q): Query<AuthQuery>,
) -> Result<Response, AppError> {
    let next = q.next.unwrap_or_else(|| "/".into());
    if !security::valid_next(&next) {
        return Err(AppError::bad_request(
            "next must be a local path starting with a single /",
        ));
    }
    state
        .tokens
        .redeem(q.t.as_deref().unwrap_or_default(), unix_now())
        .map_err(|e| {
            AppError::unauthorized(format!("{e}; run `gh-tideline` again for a new one"))
        })?;
    let cookie = format!(
        "{}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        state.policy.cookie_name,
        state.sessions.create(),
        state.sessions.ttl().as_secs()
    );
    Ok((
        StatusCode::FOUND,
        [
            (header::SET_COOKIE, cookie),
            (header::LOCATION, next),
            (header::CACHE_CONTROL, "no-store".into()),
            (header::REFERRER_POLICY, "no-referrer".into()),
        ],
    )
        .into_response())
}

/// Re-validates decoded path parameters (the middleware sees the raw path).
fn check_repo(state: &Inner, host: &str, owner: &str, repo: &str) -> Result<(), AppError> {
    if !state.policy.allowed_hosts.iter().any(|h| h == host) {
        return Err(AppError::forbidden_host(host));
    }
    if !valid_name(owner) || !valid_name(repo) {
        return Err(AppError::bad_request("invalid owner or repo name"));
    }
    Ok(())
}

fn check_head(head: &str) -> Result<(), AppError> {
    if is_oid(head) {
        Ok(())
    } else {
        Err(AppError::bad_request(
            "head must be 40 (or 64) lowercase hex characters",
        ))
    }
}

fn parse_pr_number(n: &str) -> Result<u64, AppError> {
    n.parse::<u64>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| AppError::bad_request(format!("invalid pull request number {n:?}")))
}

#[derive(Deserialize)]
struct RefreshQuery {
    refresh: Option<String>,
}

async fn load_pr(
    state: &Inner,
    (host, owner, repo, n): (String, String, String, String),
    q: RefreshQuery,
) -> Result<Arc<crate::pr::PrData>, AppError> {
    check_repo(state, &host, &owner, &repo)?;
    let number = parse_pr_number(&n)?;
    let refresh = q.refresh.as_deref() == Some("1");
    state.pr.open(&host, &owner, &repo, number, refresh).await
}

async fn pull(
    State(state): State<AppState>,
    Path(params): Path<(String, String, String, String)>,
    Query(q): Query<RefreshQuery>,
) -> Result<Response, AppError> {
    let data = load_pr(&state, params, q).await?;
    Ok(Json(&data.summary).into_response())
}

async fn pull_files(
    State(state): State<AppState>,
    Path(params): Path<(String, String, String, String)>,
    Query(q): Query<RefreshQuery>,
) -> Result<Response, AppError> {
    let data = load_pr(&state, params, q).await?;
    Ok(Json(data.files_with_viewed()).into_response())
}

#[derive(Serialize)]
struct TreeResponse<'a> {
    head: &'a str,
    entries: &'a [TreeEntry],
}

#[derive(Deserialize)]
struct TreeQuery {
    head: Option<String>,
}

/// Every file of the head tree, with the head it belongs to (serialized from the cached
/// listing without copying it: it can hold 100k entries). With `?head=`, a cached snapshot
/// of that head is used past its TTL, so the listing never reloads the PR from GitHub
/// just because the page has been open for a while.
async fn pull_tree(
    State(state): State<AppState>,
    Path((host, owner, repo, n)): Path<(String, String, String, String)>,
    Query(q): Query<TreeQuery>,
) -> Result<Response, AppError> {
    check_repo(&state, &host, &owner, &repo)?;
    let number = parse_pr_number(&n)?;
    let data = match q.head.as_deref() {
        Some(head) => {
            if !is_oid(head) {
                return Err(AppError::bad_request(
                    "head must be 40 (or 64) lowercase hex characters",
                ));
            }
            let key = PrKey {
                host,
                owner,
                repo,
                number,
            };
            state.pr.open_at(&key, head).await?
        }
        None => state.pr.open(&host, &owner, &repo, number, false).await?,
    };
    // The client asked for a head the server has moved on from: it only needs to know that.
    if q.head
        .as_deref()
        .is_some_and(|h| h != data.summary.head.sha)
    {
        return Ok(Json(TreeResponse {
            head: &data.summary.head.sha,
            entries: &[],
        })
        .into_response());
    }
    let entries = state.pr.tree(&data).await?;
    Ok(Json(TreeResponse {
        head: &data.summary.head.sha,
        entries: &entries,
    })
    .into_response())
}

#[derive(Deserialize)]
struct ViewedBody {
    path: String,
    viewed: bool,
    head: String,
}

async fn set_viewed(
    State(state): State<AppState>,
    Path((host, owner, repo, n)): Path<(String, String, String, String)>,
    body: Bytes,
) -> Result<Response, AppError> {
    check_repo(&state, &host, &owner, &repo)?;
    let number = parse_pr_number(&n)?;
    let body: ViewedBody = serde_json::from_slice(&body)
        .map_err(|e| AppError::bad_request(format!("invalid request body: {e}")))?;
    check_head(&body.head)?;
    let key = PrKey {
        host,
        owner,
        repo,
        number,
    };
    let viewed = state
        .pr
        .set_viewed(&key, &body.path, body.viewed, &body.head)
        .await?;
    Ok(Json(json!({ "path": body.path, "viewed": viewed })).into_response())
}

async fn blob(
    State(state): State<AppState>,
    Path((host, owner, repo, oid)): Path<(String, String, String, String)>,
) -> Result<Response, AppError> {
    check_repo(&state, &host, &owner, &repo)?;
    if !is_oid(&oid) {
        return Err(AppError::bad_request(
            "oid must be 40 (or 64) lowercase hex characters",
        ));
    }
    let bytes = state.pr.blob(&host, &owner, &repo, &oid).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CONTENT_SECURITY_POLICY, "sandbox"),
            (
                header::CACHE_CONTROL,
                "private, max-age=31536000, immutable",
            ),
        ],
        bytes,
    )
        .into_response())
}

async fn fallback(uri: Uri) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return AppError::not_found(format!("no API route for {path}")).into_response();
    }
    assets::serve(path)
}
