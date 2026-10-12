use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Error codes from the M0 contract; each maps to a fixed HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Unauthorized,
    ForbiddenHost,
    NotFound,
    NoToken,
    GithubError,
    GitError,
    BadRequest,
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
