//! Review thread and review-writing handlers.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde_json::json;

use super::{AppState, Inner, check_head, check_repo, parse_pr_number};
use crate::error::AppError;
use crate::pr::review::{DiscardRequest, ThreadRequest};
use crate::state::PrKey;

type PrPath = Path<(String, String, String, String)>;

fn pr_key(
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

fn parse_body<T: DeserializeOwned>(body: &Bytes) -> Result<T, AppError> {
    serde_json::from_slice(body)
        .map_err(|e| AppError::bad_request(format!("invalid request body: {e}")))
}

pub(super) async fn create_thread(
    State(state): State<AppState>,
    Path(params): PrPath,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = pr_key(&state, params)?;
    let req: ThreadRequest = parse_body(&body)?;
    check_head(&req.head)?;
    Ok(Json(state.pr.create_thread(&key, &req).await?).into_response())
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
    state.pr.discard_review(&key, &req.review_id).await?;
    Ok(Json(json!({ "pendingReview": null })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

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
