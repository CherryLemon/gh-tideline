//! Review thread and review-writing handlers.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::future::Future;

use super::{AppState, Inner, check_head, check_repo, parse_pr_number};
use crate::error::AppError;
use crate::pr::review::{
    DiscardRequest, EditRequest, MarkdownRequest, ReplyRequest, ResolveRequest, SubmitRequest,
    ThreadRequest,
};
use crate::state::PrKey;

pub(super) type PrPath = Path<(String, String, String, String)>;
/// A PR path plus a thread id.
type ThreadPath = Path<(String, String, String, String, String)>;
/// A PR path plus a thread id and one of its comments' ids.
type CommentPath = Path<(String, String, String, String, String, String)>;

pub(super) fn pr_key(
    state: &Inner,
    (host, owner, repo, n): (String, String, String, String),
) -> Result<PrKey, AppError> {
    check_repo(state, &host, &owner, &repo)?;
    let number = parse_pr_number(&n)?;
    Ok(PrKey {
        host,
        owner,
        repo,
        number,
    })
}

pub(super) async fn threads(
    State(state): State<AppState>,
    Path(params): PrPath,
) -> Result<Response, AppError> {
    let key = pr_key(&state, params)?;
    Ok(Json(state.pr.threads(&key).await?).into_response())
}

/// `POST /api/{host}/{owner}/{repo}/markdown {text}` → `{html}`: a comment's preview, as
/// GitHub renders it in that repository. Reads nothing and writes nothing on GitHub.
pub(super) async fn markdown(
    State(state): State<AppState>,
    Path((host, owner, repo)): Path<(String, String, String)>,
    body: Bytes,
) -> Result<Response, AppError> {
    check_repo(&state, &host, &owner, &repo)?;
    let req: MarkdownRequest = parse_body(&body)?;
    let html = state
        .pr
        .render_markdown(&host, &owner, &repo, &req.text)
        .await?;
    Ok(Json(json!({ "html": html })).into_response())
}

pub(super) fn parse_body<T: DeserializeOwned>(body: &Bytes) -> Result<T, AppError> {
    serde_json::from_slice(body)
        .map_err(|e| AppError::bad_request(format!("invalid request body: {e}")))
}

/// Runs a write to its end even if the client goes away meanwhile. Hyper drops a handler when
/// its connection closes (a reload while sending), which would stop a write of several GitHub
/// calls halfway: a pending review with no comment, or a single comment left pending.
async fn to_the_end<T: Send + 'static>(
    write: impl Future<Output = Result<T, AppError>> + Send + 'static,
) -> Result<T, AppError> {
    tokio::spawn(write).await.unwrap_or_else(|e| {
        eprintln!("review write task failed: {e}");
        // Like a GitHub failure: the client looks for what landed before sending again.
        Err(AppError::github(
            "the write stopped unexpectedly, possibly after GitHub made the change",
        ))
    })
}

pub(super) async fn create_thread(
    State(state): State<AppState>,
    Path(params): PrPath,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = pr_key(&state, params)?;
    let req: ThreadRequest = parse_body(&body)?;
    check_head(&req.head)?;
    let pr = state.pr.clone();
    let created = to_the_end(async move { pr.create_thread(&key, &req).await }).await?;
    Ok(Json(created).into_response())
}

/// GraphQL node ids (`PRR_kwDO…`); only ever passed to GitHub as variables.
fn check_node_id(id: &str) -> Result<(), AppError> {
    let valid = (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_=+/-".contains(&b));
    if valid {
        Ok(())
    } else {
        Err(AppError::bad_request("invalid id"))
    }
}

pub(super) async fn discard_review(
    State(state): State<AppState>,
    Path(params): PrPath,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = pr_key(&state, params)?;
    let req: DiscardRequest = parse_body(&body)?;
    check_node_id(&req.review_id)?;
    let pr = state.pr.clone();
    to_the_end(async move { pr.discard_review(&key, &req.review_id).await }).await?;
    Ok(Json(json!({ "pendingReview": null })).into_response())
}

pub(super) async fn submit_review(
    State(state): State<AppState>,
    Path(params): PrPath,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = pr_key(&state, params)?;
    let req: SubmitRequest = parse_body(&body)?;
    check_head(&req.head)?;
    if let Some(id) = &req.review_id {
        check_node_id(id)?;
    }
    let pr = state.pr.clone();
    let submitted = to_the_end(async move { pr.submit_review(&key, &req).await }).await?;
    Ok(Json(submitted).into_response())
}

pub(super) async fn reply(
    State(state): State<AppState>,
    Path((host, owner, repo, n, thread)): ThreadPath,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = pr_key(&state, (host, owner, repo, n))?;
    check_node_id(&thread)?;
    let req: ReplyRequest = parse_body(&body)?;
    check_head(&req.head)?;
    let pr = state.pr.clone();
    let replied = to_the_end(async move { pr.reply(&key, &thread, &req).await }).await?;
    Ok(Json(replied).into_response())
}

pub(super) async fn set_resolved(
    State(state): State<AppState>,
    Path((host, owner, repo, n, thread)): ThreadPath,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = pr_key(&state, (host, owner, repo, n))?;
    check_node_id(&thread)?;
    let req: ResolveRequest = parse_body(&body)?;
    let thread = state.pr.set_resolved(&key, &thread, req.resolved).await?;
    Ok(Json(json!({ "thread": thread })).into_response())
}

pub(super) async fn edit_comment(
    State(state): State<AppState>,
    Path((host, owner, repo, n, thread, comment)): CommentPath,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = pr_key(&state, (host, owner, repo, n))?;
    check_node_id(&thread)?;
    check_node_id(&comment)?;
    let req: EditRequest = parse_body(&body)?;
    let comment = state.pr.edit_comment(&key, &comment, &req.body).await?;
    Ok(Json(json!({ "comment": comment })).into_response())
}

pub(super) async fn delete_comment(
    State(state): State<AppState>,
    Path((host, owner, repo, n, thread, comment)): CommentPath,
) -> Result<Response, AppError> {
    let key = pr_key(&state, (host, owner, repo, n))?;
    check_node_id(&thread)?;
    check_node_id(&comment)?;
    // It may drop the pending review: a create waiting for the lock must not read it first.
    let pr = state.pr.clone();
    let deleted =
        to_the_end(async move { pr.delete_comment(&key, &thread, &comment).await }).await?;
    Ok(Json(deleted).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_finish_when_the_client_goes_away() {
        let (done, landed) = tokio::sync::oneshot::channel();
        let write = to_the_end(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = done.send(());
            Ok(())
        });
        // The handler is dropped before the write is done, as when the connection closes.
        let gone = tokio::time::timeout(std::time::Duration::from_millis(5), write).await;
        assert!(gone.is_err());
        tokio::time::timeout(std::time::Duration::from_secs(2), landed)
            .await
            .expect("the write went on")
            .expect("the write finished");
    }

    #[tokio::test]
    async fn a_failed_write_task_may_have_landed() {
        let r: Result<(), AppError> = to_the_end(async { panic!("boom") }).await;
        assert_eq!(r.unwrap_err().code, crate::error::ErrorCode::GithubError);
    }

    #[test]
    fn node_ids_are_plain() {
        for id in ["PRR_kwDOABCD12", "PRRT_kwDO-x_y=", "a/b+c"] {
            assert!(check_node_id(id).is_ok(), "{id}");
        }
        let long = "x".repeat(129);
        for id in ["", "PRR 1", "PRR_1\"", "a\nb", "é", long.as_str()] {
            assert!(check_node_id(id).is_err(), "{id:?}");
        }
    }
}
