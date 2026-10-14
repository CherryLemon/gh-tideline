//! Code navigation handlers: the symbols of a file.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::{AppState, check_repo};
use crate::error::AppError;
use crate::git::is_oid;

/// The longest path accepted, in bytes (Linux's `PATH_MAX`).
const MAX_PATH_BYTES: usize = 4096;

/// A path in the repository: 1 to 4096 bytes without control characters.
fn check_path(path: Option<&str>) -> Result<&str, AppError> {
    match path {
        Some(path)
            if (1..=MAX_PATH_BYTES).contains(&path.len())
                && !path.chars().any(char::is_control) =>
        {
            Ok(path)
        }
        _ => Err(AppError::bad_request(
            "path must be 1 to 4096 bytes without control characters",
        )),
    }
}

#[derive(Deserialize)]
pub(super) struct SymbolsQuery {
    path: Option<String>,
}

/// `GET /api/{host}/{owner}/{repo}/blobs/{oid}/symbols?path=`: the definitions in a file, for the
/// sticky header and the outline. `path` names the language.
pub(super) async fn symbols(
    State(state): State<AppState>,
    Path((host, owner, repo, oid)): Path<(String, String, String, String)>,
    Query(q): Query<SymbolsQuery>,
) -> Result<Response, AppError> {
    check_repo(&state, &host, &owner, &repo)?;
    if !is_oid(&oid) {
        return Err(AppError::bad_request(
            "oid must be 40 (or 64) lowercase hex characters",
        ));
    }
    let path = check_path(q.path.as_deref())?;
    let symbols = state.pr.symbols(&host, &owner, &repo, &oid, path).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(symbols)).into_response())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::HeaderMap;
    use serde_json::{Value, json};

    use super::*;
    use crate::pr::PrService;
    use crate::pr::testing::{Fixture, RUST, UNKNOWN};

    /// The router served on a local port to a browser that is logged in.
    struct TestServer {
        port: u16,
        cookie: String,
        client: reqwest::Client,
    }

    impl TestServer {
        async fn start(pr: Arc<PrService>) -> TestServer {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let port = listener.local_addr().unwrap().port();
            let state = crate::server::state(
                port,
                "secret".to_string(),
                0,
                vec!["github.com".to_string()],
                pr,
            );
            let cookie = format!("{}={}", state.policy.cookie_name, state.sessions.create());
            let app = crate::server::router(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            TestServer {
                port,
                cookie,
                client: reqwest::Client::builder().no_proxy().build().unwrap(),
            }
        }

        /// GETs `path`: the status, the headers and the JSON body.
        async fn get(&self, path: &str) -> (u16, HeaderMap, Value) {
            let response = self
                .client
                .get(format!("http://127.0.0.1:{}{path}", self.port))
                .header("cookie", &self.cookie)
                .header("x-dr", "1")
                .send()
                .await
                .unwrap();
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            (status, headers, response.json().await.unwrap())
        }
    }

    fn error_code(body: &Value) -> &str {
        body["error"]["code"].as_str().unwrap()
    }

    #[test]
    fn a_path_is_one_to_4096_bytes_without_control_characters() {
        for path in [
            "a",
            "src/main.rs",
            "dir with space/é ü.rs",
            &"a".repeat(4096),
        ] {
            assert_eq!(check_path(Some(path)).unwrap(), path);
        }
        for path in [
            None,
            Some(""),
            Some("a\nb.rs"),
            Some("a\tb.rs"),
            Some("a\u{7f}.rs"),
            Some("a\u{85}.rs"),
            Some("nul\0.rs"),
            Some(&"a".repeat(4097)),
        ] {
            assert!(check_path(path).is_err(), "{path:?}");
        }
    }

    #[tokio::test]
    async fn symbols_are_served_as_json_and_never_cached() {
        let f = Fixture::new("srv-symbols", &[RUST.as_bytes()]).await;
        let server = TestServer::start(f.service.clone()).await;
        let url = format!(
            "/api/github.com/o/r/blobs/{}/symbols?path=src/a.rs",
            f.oids[0]
        );
        let (status, headers, body) = server.get(&url).await;
        assert_eq!(status, 200);
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(body["language"], "rust");
        assert_eq!(body["tooLarge"], false);
        assert_eq!(body["symbols"].as_array().unwrap().len(), 4);
        assert_eq!(
            body["symbols"][3],
            json!({
                "name": "helper", "kind": "function", "line": 13, "column": 4, "endColumn": 10,
                "startLine": 13, "endLine": 15
            })
        );
    }

    #[tokio::test]
    async fn a_path_of_no_known_language_has_a_null_language() {
        let f = Fixture::new("srv-no-language", &[b"text\n"]).await;
        let server = TestServer::start(f.service.clone()).await;
        let url = format!(
            "/api/github.com/o/r/blobs/{}/symbols?path=notes.txt",
            f.oids[0]
        );
        let (status, headers, body) = server.get(&url).await;
        assert_eq!(status, 200);
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(
            body,
            json!({ "language": null, "tooLarge": false, "symbols": [] })
        );
    }

    #[tokio::test]
    async fn a_symbols_request_with_bad_parts_is_a_bad_request() {
        let f = Fixture::new("srv-symbols-400", &[RUST.as_bytes()]).await;
        let server = TestServer::start(f.service.clone()).await;
        let oid = &f.oids[0];
        let long_path = "a".repeat(4097);
        let urls = [
            format!(
                "/api/github.com/o/r/blobs/{}/symbols?path=a.rs",
                oid.to_uppercase()
            ),
            format!("/api/github.com/o/r/blobs/{}/symbols?path=a.rs", &oid[..39]),
            "/api/github.com/o/r/blobs/HEAD/symbols?path=a.rs".to_string(),
            format!("/api/github.com/o/r/blobs/{oid}/symbols"),
            format!("/api/github.com/o/r/blobs/{oid}/symbols?path="),
            format!("/api/github.com/o/r/blobs/{oid}/symbols?path=a%0Ab.rs"),
            format!("/api/github.com/o/r/blobs/{oid}/symbols?path={long_path}"),
            format!("/api/github.com/bad%20owner/r/blobs/{oid}/symbols?path=a.rs"),
        ];
        for url in urls {
            let (status, _, body) = server.get(&url).await;
            assert_eq!((status, error_code(&body)), (400, "bad_request"), "{url}");
        }
        // The longest path allowed is served.
        let longest = format!("{}.rs", "a".repeat(4093));
        let url = format!("/api/github.com/o/r/blobs/{oid}/symbols?path={longest}");
        assert_eq!(server.get(&url).await.0, 200);
        // Only the configured hosts.
        let url = format!("/api/evil.example/o/r/blobs/{oid}/symbols?path=a.rs");
        let (status, _, body) = server.get(&url).await;
        assert_eq!((status, error_code(&body)), (403, "forbidden_host"));
    }

    #[tokio::test]
    async fn a_blob_the_blob_endpoint_would_not_serve_is_not_found() {
        let f = Fixture::new("srv-symbols-404", &[RUST.as_bytes()]).await;
        let server = TestServer::start(f.service.clone()).await;
        let blob = format!("/api/github.com/o/r/blobs/{UNKNOWN}");
        let (status, _, body) = server.get(&format!("{blob}/symbols?path=a.rs")).await;
        assert_eq!((status, error_code(&body)), (404, "not_found"));
        // Like the blob itself.
        let (status, _, body) = server.get(&blob).await;
        assert_eq!((status, error_code(&body)), (404, "not_found"));
    }

    #[tokio::test]
    async fn symbols_work_without_an_index() {
        let f = Fixture::new("srv-symbols-no-index", &[RUST.as_bytes()]).await;
        let server = TestServer::start(f.without_index()).await;
        let url = format!("/api/github.com/o/r/blobs/{}/symbols?path=a.rs", f.oids[0]);
        let (status, _, body) = server.get(&url).await;
        assert_eq!(status, 200);
        assert_eq!(body["symbols"].as_array().unwrap().len(), 4);
    }
}
