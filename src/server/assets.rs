//! Frontend assets embedded from `web/dist` (SPA: unknown paths serve `index.html`).

use axum::http::header;
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "web/dist"]
#[allow_missing = true]
struct Assets;

/// Content-Security-Policy for HTML pages. Review comments are GitHub-rendered HTML; even after
/// DOMPurify this keeps any slip from running script, posting forms or loading code elsewhere.
/// Monaco needs inline styles and same-origin (or blob:) workers; comment images come from
/// GitHub's https hosts.
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob: https:; font-src 'self' data:; worker-src 'self' blob:; \
    connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

const NOT_BUILT: &str = "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>tideline</title></head>\n<body style=\"font-family:system-ui;background:#FFF8ED;color:#546E7A;padding:2rem\">\n<h1>frontend not built</h1>\n<p>The web UI was not embedded in this binary. Run <code>make web</code> (or <code>make build</code>) and rebuild.</p>\n</body></html>\n";

pub fn serve(path: &str) -> Response {
    let rel = path.trim_start_matches('/');
    if !rel.is_empty()
        && let Some(file) = Assets::get(rel)
    {
        if rel.ends_with(".html") {
            return html(file.data.into_owned());
        }
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
        Some(index) => html(index.data.into_owned()),
        None => html(NOT_BUILT.as_bytes().to_vec()),
    }
}

/// HTML responses carry the CSP, and `no-referrer` so links opened from comments don't leak the
/// local URL (which names the PR) to other sites.
fn html(body: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    fn header_of(r: &Response, name: header::HeaderName) -> &str {
        r.headers().get(name).unwrap().to_str().unwrap()
    }

    #[test]
    fn html_pages_carry_csp_and_no_referrer() {
        // "/" and unknown paths fall back to index.html (or the not-built page); both are HTML.
        for path in ["/", "/github.com/o/r/pull/1", "/index.html"] {
            let r = serve(path);
            assert_eq!(r.status(), StatusCode::OK, "{path}");
            assert!(
                header_of(&r, header::CONTENT_TYPE).starts_with("text/html"),
                "{path}"
            );
            assert_eq!(
                header_of(&r, header::CONTENT_SECURITY_POLICY),
                CSP,
                "{path}"
            );
            assert_eq!(
                header_of(&r, header::REFERRER_POLICY),
                "no-referrer",
                "{path}"
            );
        }
        let r = html(NOT_BUILT.as_bytes().to_vec());
        assert_eq!(header_of(&r, header::CONTENT_SECURITY_POLICY), CSP);
    }

    #[test]
    fn csp_blocks_script_from_anywhere_but_self() {
        let directives: Vec<&str> = CSP.split(';').map(str::trim).collect();
        assert!(directives.contains(&"script-src 'self'"));
        assert!(directives.contains(&"form-action 'none'"));
        assert!(directives.contains(&"frame-ancestors 'none'"));
        assert!(!CSP.contains("unsafe-eval"));
        assert!(
            !CSP.contains("  "),
            "line continuations must not leave double spaces"
        );
    }
}
