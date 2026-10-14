//! Code navigation handlers: the symbols of a file, and the index of a commit.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use super::review::{PrPath, parse_body, pr_key};
use super::{AppState, check_head, check_repo};
use crate::error::AppError;
use crate::git::is_oid;
use crate::pr::nav::{IndexRequest, Side};

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

/// An answer that is never cached: what the index holds changes as builds go on.
fn uncached(body: impl Serialize) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
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
    Ok(uncached(symbols))
}

/// The commit a request is about: the head the page shows, and which of the pull request's two commits.
#[derive(Deserialize)]
pub(super) struct CommitQuery {
    head: Option<String>,
    side: Option<String>,
}

impl CommitQuery {
    fn parts(&self) -> Result<(&str, Side), AppError> {
        let head = self.head.as_deref().unwrap_or_default();
        check_head(head)?;
        let side = match self.side.as_deref() {
            Some("head") => Side::Head,
            Some("base") => Side::Base,
            _ => {
                return Err(AppError::bad_request("side must be \"head\" or \"base\""));
            }
        };
        Ok((head, side))
    }
}

/// `POST /api/{host}/{owner}/{repo}/pulls/{n}/index {head, side}`: starts building the index of the head or of the
/// merge base, joins the build that is running, or answers `ready` at once. The build goes on without the request.
pub(super) async fn start_index(
    State(state): State<AppState>,
    Path(params): PrPath,
    body: Bytes,
) -> Result<Response, AppError> {
    let key = pr_key(&state, params)?;
    let req: IndexRequest = parse_body(&body)?;
    check_head(&req.head)?;
    Ok(uncached(
        state.pr.start_index(&key, &req.head, req.side).await?,
    ))
}

/// `GET /api/{host}/{owner}/{repo}/pulls/{n}/index?head=&side=`: how far the build has come.
pub(super) async fn index_status(
    State(state): State<AppState>,
    Path(params): PrPath,
    Query(q): Query<CommitQuery>,
) -> Result<Response, AppError> {
    let key = pr_key(&state, params)?;
    let (head, side) = q.parts()?;
    Ok(uncached(state.pr.index_status(&key, head, side).await?))
}

/// `DELETE /api/{host}/{owner}/{repo}/pulls/{n}/index?head=&side=`: asks the build to stop.
pub(super) async fn cancel_index(
    State(state): State<AppState>,
    Path(params): PrPath,
    Query(q): Query<CommitQuery>,
) -> Result<Response, AppError> {
    let key = pr_key(&state, params)?;
    let (head, side) = q.parts()?;
    Ok(uncached(state.pr.cancel_index(&key, head, side).await?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::time::{Duration, Instant};

    use axum::http::HeaderMap;
    use reqwest::Method;
    use serde_json::{Value, json};

    use super::*;
    use crate::pr::PrService;
    use crate::pr::testing::{Fixture, Pull, RUST, UNKNOWN};

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

        /// Sends a request the way the page does: the status, the headers and the JSON body of the answer.
        async fn send(
            &self,
            method: Method,
            path: &str,
            body: Option<&str>,
        ) -> (u16, HeaderMap, Value) {
            let mut request = self
                .client
                .request(method, format!("http://127.0.0.1:{}{path}", self.port))
                .header("cookie", &self.cookie)
                .header("origin", format!("http://127.0.0.1:{}", self.port))
                .header("x-dr", "1");
            if let Some(body) = body {
                request = request.body(body.to_string());
            }
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            (status, headers, response.json().await.unwrap())
        }

        async fn get(&self, path: &str) -> (u16, HeaderMap, Value) {
            self.send(Method::GET, path, None).await
        }

        async fn post(&self, path: &str, body: &str) -> (u16, HeaderMap, Value) {
            self.send(Method::POST, path, Some(body)).await
        }

        async fn delete(&self, path: &str) -> (u16, HeaderMap, Value) {
            self.send(Method::DELETE, path, None).await
        }

        /// Polls `path` until the body it answers with satisfies `reached`.
        async fn until(&self, path: &str, reached: impl Fn(&Value) -> bool) -> Value {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let (status, _, body) = self.get(path).await;
                assert_eq!(status, 200, "{body}");
                if reached(&body) {
                    return body;
                }
                assert!(Instant::now() < deadline, "timed out; the answer is {body}");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }

    fn error_code(body: &Value) -> &str {
        body["error"]["code"].as_str().unwrap()
    }

    /// The index of pull request 1 of `github.com/o/r`.
    const INDEX: &str = "/api/github.com/o/r/pulls/1/index";

    /// The status of a commit no job is working on.
    fn idle(commit: &str, state: &str) -> Value {
        json!({
            "commit": commit, "state": state, "phase": null, "done": 0, "total": 0,
            "cancelling": false, "message": null
        })
    }

    /// Whether a status says that the build is over.
    fn ended(status: &Value) -> bool {
        !matches!(status["state"].as_str(), Some("queued" | "building"))
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

    #[tokio::test]
    async fn an_index_is_built_by_posting_and_polling() {
        let p = Pull::new("srv-index").await;
        let server = TestServer::start(p.service.clone()).await;
        let (base, head) = (p.clone.base.clone(), p.clone.head.clone());
        let head_query = format!("{INDEX}?head={head}&side=head");

        let (status, headers, body) = server.get(&head_query).await;
        assert_eq!((status, &body), (200, &idle(&head, "missing")));
        assert_eq!(headers["cache-control"], "no-store");

        // The build is queued, and goes on without the request.
        let start_head = json!({ "head": head, "side": "head" }).to_string();
        let (status, headers, body) = server.post(INDEX, &start_head).await;
        assert_eq!((status, &body), (200, &idle(&head, "queued")));
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(server.until(&head_query, ended).await, idle(&head, "ready"));
        assert!(p.db.commit_complete(&head).unwrap());

        // Built: asking to stop changes nothing, and starting again is answered at once.
        let (status, headers, body) = server.delete(&head_query).await;
        assert_eq!((status, &body), (200, &idle(&head, "ready")));
        assert_eq!(headers["cache-control"], "no-store");
        let (status, _, body) = server.post(INDEX, &start_head).await;
        assert_eq!((status, &body), (200, &idle(&head, "ready")));

        // The merge base is a commit of its own.
        let base_query = format!("{INDEX}?head={head}&side=base");
        let (status, _, body) = server.get(&base_query).await;
        assert_eq!((status, &body), (200, &idle(&base, "missing")));
        assert!(!p.db.commit_complete(&base).unwrap());
        let start_base = json!({ "head": head, "side": "base" }).to_string();
        let (status, _, body) = server.post(INDEX, &start_base).await;
        assert_eq!((status, &body), (200, &idle(&base, "queued")));
        assert_eq!(server.until(&base_query, ended).await, idle(&base, "ready"));
        assert!(p.db.commit_complete(&base).unwrap());
    }

    #[tokio::test]
    async fn a_build_is_cancelled_over_http() {
        let p = Pull::new("srv-index-cancel").await;
        let server = TestServer::start(p.service.clone()).await;
        let head = p.clone.head.clone();
        let query = format!("{INDEX}?head={head}&side=head");

        // While the repo is held, the build waits for it with its blobs counted and none fetched.
        let guard = p.clone.repo.lock().await.unwrap();
        let start = json!({ "head": head, "side": "head" }).to_string();
        server.post(INDEX, &start).await;
        server
            .until(&query, |status| status["phase"] == "fetching")
            .await;
        let (status, headers, body) = server.delete(&query).await;
        assert_eq!(status, 200);
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(
            body,
            json!({
                "commit": head, "state": "building", "phase": "fetching", "done": 0, "total": 4,
                "cancelling": true, "message": null
            })
        );

        // The build stops when it gets the repo.
        drop(guard);
        let over = server.until(&query, ended).await;
        assert_eq!(over["state"], "cancelled");
        assert_eq!(over["cancelling"], false);
        assert_eq!(over["done"], 0);
        assert!(!p.db.commit_complete(&head).unwrap());
    }

    #[tokio::test]
    async fn an_index_request_with_bad_parts_is_a_bad_request() {
        let p = Pull::new("srv-index-400").await;
        let server = TestServer::start(p.service.clone()).await;
        let head = p.clone.head.clone();
        let upper = head.to_uppercase();
        let short = &head[..39];

        let bodies = [
            String::new(),
            "not json".to_string(),
            "{}".to_string(),
            json!({ "side": "head" }).to_string(),
            json!({ "head": head }).to_string(),
            json!({ "head": head, "side": "both" }).to_string(),
            json!({ "head": head, "side": "HEAD" }).to_string(),
            json!({ "head": head, "side": 1 }).to_string(),
            json!({ "head": upper, "side": "head" }).to_string(),
            json!({ "head": short, "side": "head" }).to_string(),
            json!({ "head": "z".repeat(40), "side": "head" }).to_string(),
            json!({ "head": 7, "side": "head" }).to_string(),
            json!({ "head": head, "side": "head", "also": true }).to_string(),
        ];
        for body in bodies {
            let (status, _, answer) = server.post(INDEX, &body).await;
            assert_eq!(
                (status, error_code(&answer)),
                (400, "bad_request"),
                "{body}"
            );
        }

        let queries = [
            String::new(),
            "?side=head".to_string(),
            "?head=&side=head".to_string(),
            format!("?head={upper}&side=head"),
            format!("?head={short}&side=head"),
            format!("?head={head}"),
            format!("?head={head}&side="),
            format!("?head={head}&side=both"),
            format!("?head={head}&side=HEAD"),
        ];
        for query in queries {
            let url = format!("{INDEX}{query}");
            for (status, _, answer) in [server.get(&url).await, server.delete(&url).await] {
                assert_eq!((status, error_code(&answer)), (400, "bad_request"), "{url}");
            }
        }

        // Neither a pull request nor a repository is named.
        let start = json!({ "head": head, "side": "head" }).to_string();
        let query = format!("?head={head}&side=head");
        for path in [
            "/api/github.com/o/r/pulls/0/index",
            "/api/github.com/o/r/pulls/x/index",
            "/api/github.com/o/r/pulls/-1/index",
            "/api/github.com/bad%20owner/r/pulls/1/index",
        ] {
            let answers = [
                server.post(path, &start).await,
                server.get(&format!("{path}{query}")).await,
                server.delete(&format!("{path}{query}")).await,
            ];
            for (status, _, answer) in answers {
                assert_eq!(
                    (status, error_code(&answer)),
                    (400, "bad_request"),
                    "{path}"
                );
            }
        }

        // Only the configured hosts.
        let path = "/api/evil.example/o/r/pulls/1/index";
        let answers = [
            server.post(path, &start).await,
            server.get(&format!("{path}{query}")).await,
            server.delete(&format!("{path}{query}")).await,
        ];
        for (status, _, answer) in answers {
            assert_eq!((status, error_code(&answer)), (403, "forbidden_host"));
        }

        // None of it started anything.
        let (_, _, body) = server.get(&format!("{INDEX}{query}")).await;
        assert_eq!(body, idle(&head, "missing"));
    }

    #[tokio::test]
    async fn a_page_behind_the_pull_request_is_told_to_refresh_over_http() {
        let p = Pull::new("srv-index-stale").await;
        let server = TestServer::start(p.service.clone()).await;
        // A commit that is not the head the service knows.
        let behind = p.clone.base.clone();
        for side in ["head", "base"] {
            let query = format!("{INDEX}?head={behind}&side={side}");
            let start = json!({ "head": behind, "side": side }).to_string();
            let answers = [
                server.post(INDEX, &start).await,
                server.get(&query).await,
                server.delete(&query).await,
            ];
            for (status, _, answer) in answers {
                assert_eq!((status, error_code(&answer)), (409, "stale_head"), "{side}");
            }
        }

        // Nothing was started.
        let head = p.clone.head.clone();
        let (_, _, body) = server.get(&format!("{INDEX}?head={head}&side=head")).await;
        assert_eq!(body, idle(&head, "missing"));
    }

    #[tokio::test]
    async fn without_an_index_the_index_endpoints_are_unavailable() {
        let p = Pull::new("srv-index-503").await;
        let server = TestServer::start(p.without_index()).await;
        let head = p.clone.head.clone();
        let query = format!("{INDEX}?head={head}&side=head");
        let start = json!({ "head": head, "side": "head" }).to_string();
        let answers = [
            server.post(INDEX, &start).await,
            server.get(&query).await,
            server.delete(&query).await,
        ];
        for (status, _, answer) in answers {
            assert_eq!((status, error_code(&answer)), (503, "index_unavailable"));
        }
    }
}
