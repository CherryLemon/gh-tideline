//! Frontend assets embedded from `web/dist` (SPA: unknown paths serve `index.html`).

use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "web/dist"]
#[allow_missing = true]
struct Assets;

const NOT_BUILT: &str = "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>tideline</title></head>\n<body style=\"font-family:system-ui;background:#FFF8ED;color:#546E7A;padding:2rem\">\n<h1>frontend not built</h1>\n<p>The web UI was not embedded in this binary. Run <code>make web</code> (or <code>make build</code>) and rebuild.</p>\n</body></html>\n";

pub fn serve(path: &str) -> Response {
    let rel = path.trim_start_matches('/');
    if !rel.is_empty()
        && let Some(file) = Assets::get(rel)
    {
        let mime = mime_guess::from_path(rel).first_or_octet_stream();
        let cache = if rel.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            [
                (header::CONTENT_TYPE, mime.as_ref().to_string()),
                (header::CACHE_CONTROL, cache.to_string()),
            ],
            file.data.into_owned(),
        )
            .into_response();
    }
    match Assets::get("index.html") {
        Some(index) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            index.data.into_owned(),
        )
            .into_response(),
        None => (StatusCode::OK, Html(NOT_BUILT)).into_response(),
    }
}
