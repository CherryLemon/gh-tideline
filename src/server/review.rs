//! Review thread and review-writing handlers.

use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};

use super::{AppState, Inner, check_repo, parse_pr_number};
use crate::error::AppError;
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
