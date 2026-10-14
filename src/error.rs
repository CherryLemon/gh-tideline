use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Error codes from the M0–M3 contracts; each maps to a fixed HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Unauthorized,
    ForbiddenHost,
    NotFound,
    NoToken,
    GithubError,
    GitError,
    BadRequest,
    StaleHead,
    Rejected,
    PendingReviewExists,
    ReviewChanged,
    IndexUnavailable,
    IndexMissing,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::ForbiddenHost => "forbidden_host",
            ErrorCode::NotFound => "not_found",
            ErrorCode::NoToken => "no_token",
            ErrorCode::GithubError => "github_error",
            ErrorCode::GitError => "git_error",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::StaleHead => "stale_head",
            ErrorCode::Rejected => "rejected",
            ErrorCode::PendingReviewExists => "pending_review_exists",
            ErrorCode::ReviewChanged => "review_changed",
            ErrorCode::IndexUnavailable => "index_unavailable",
            ErrorCode::IndexMissing => "index_missing",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::ForbiddenHost => StatusCode::FORBIDDEN,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::NoToken => StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::GithubError => StatusCode::BAD_GATEWAY,
            ErrorCode::GitError => StatusCode::BAD_GATEWAY,
            ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
            ErrorCode::StaleHead => StatusCode::CONFLICT,
            ErrorCode::Rejected => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::PendingReviewExists => StatusCode::CONFLICT,
            ErrorCode::ReviewChanged => StatusCode::CONFLICT,
            ErrorCode::IndexUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::IndexMissing => StatusCode::CONFLICT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        AppError {
            code,
            message: message.into(),
        }
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message)
    }
    pub fn forbidden_host(host: &str) -> Self {
        Self::new(
            ErrorCode::ForbiddenHost,
            format!("host {host:?} is not github.com or a configured ghe_hosts entry"),
        )
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }
    pub fn github(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::GithubError, message)
    }
    pub fn git(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::GitError, message)
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }
    pub fn stale_head(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::StaleHead, message)
    }
    /// GitHub refused a write; the message is GitHub's.
    pub fn rejected(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Rejected, message)
    }
    /// A single comment while the viewer has a pending review (GitHub allows one per PR).
    pub fn pending_review_exists(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PendingReviewExists, message)
    }
    /// The viewer's pending review on GitHub is not the one the client acts on (submitted,
    /// discarded or started elsewhere).
    pub fn review_changed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ReviewChanged, message)
    }
    /// Code navigation cannot answer: its index database could not be opened or failed.
    pub fn index_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::IndexUnavailable, message)
    }
    /// A lookup in a commit whose index is not complete: it has to be built first.
    pub fn index_missing(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::IndexMissing, message)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = json!({ "error": { "code": self.code.as_str(), "message": self.message } });
        (self.code.status(), Json(body)).into_response()
    }
}
