//! Request gate for every route (design.md §8): Host check, session cookie, same-origin
//! signal, Origin + `X-DR` for non-GET, and the `{host}` allowlist. Also the one-time
//! authorization tokens the CLI mints for `/__auth`, and the session store.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use hmac::{KeyInit, Mac};

use super::AppState;
use crate::error::AppError;

/// Session cookie name for a server port: `dr_session_<port>`. Cookies are not isolated by
/// port, so servers on different ports must not share a name.
pub fn cookie_name(port: u16) -> String {
    format!("dr_session_{port}")
}

pub struct Policy {
    pub port: u16,
    pub cookie_name: String,
    pub allowed_hosts: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub enum Decision {
    Allow,
    /// Host header is not ours (DNS rebinding) → 421.
    Misdirected,
    Reject(AppError),
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// How long an authorization token from the CLI stays valid.
pub const TOKEN_TTL_SECS: u64 = 60;
/// Tolerated clock difference for a token minted "in the future".
pub const CLOCK_SKEW_SECS: u64 = 5;

fn auth_mac(secret: &str, port: u16, ts: u64, rand: &str) -> hmac::Hmac<sha2::Sha256> {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key");
    mac.update(format!("dr-auth|{port}|{ts}|{rand}").as_bytes());
    mac
}

/// Self-contained authorization token `ts.rand.hex(HMAC_SHA256(secret, "dr-auth|port|ts|rand"))`.
/// The CLI mints it locally, so the secret itself never leaves this machine's files.
/// `rand` must be 32 lowercase hex characters.
pub fn mint_auth_token(secret: &str, port: u16, ts: u64, rand: &str) -> String {
    let mac = auth_mac(secret, port, ts, rand).finalize().into_bytes();
    format!("{ts}.{rand}.{}", hex::encode(mac))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    BadMac,
    Expired,
    Replayed,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TokenError::Malformed => "malformed authorization link",
            TokenError::BadMac => "invalid authorization link",
            TokenError::Expired => "expired authorization link",
            TokenError::Replayed => "authorization link already used",
        })
    }
}

/// Checks a token's format, MAC (constant time, bound to `port`) and age. Tokens minted
/// before `not_before` (the server's start) are rejected too: a process that squatted the
/// port earlier and captured a token cannot redeem it at a server started later.
/// Returns `(ts, rand)`.
pub fn verify_auth_token<'a>(
    secret: &str,
    port: u16,
    token: &'a str,
    now: u64,
    not_before: u64,
) -> Result<(u64, &'a str), TokenError> {
    let mut parts = token.split('.');
    let (Some(ts), Some(rand), Some(mac), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };
    let ts: u64 = (!ts.is_empty() && ts.len() <= 20 && ts.bytes().all(|b| b.is_ascii_digit()))
        .then(|| ts.parse().ok())
        .flatten()
        .ok_or(TokenError::Malformed)?;
    if rand.len() != 32 || !rand.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(TokenError::Malformed);
    }
    let mac = hex::decode(mac).map_err(|_| TokenError::Malformed)?;
    auth_mac(secret, port, ts, rand)
        .verify_slice(&mac)
        .map_err(|_| TokenError::BadMac)?;
    if ts > now.saturating_add(CLOCK_SKEW_SECS)
        || now > ts.saturating_add(TOKEN_TTL_SECS)
        || ts < not_before
    {
        return Err(TokenError::Expired);
    }
    Ok((ts, rand))
}

/// Verifies tokens for `/__auth` and remembers redeemed `rand` values until they expire.
pub struct AuthTokens {
    secret: String,
    port: u16,
    not_before: u64,
    used: Mutex<HashMap<String, u64>>,
}

impl AuthTokens {
    pub fn new(secret: String, port: u16, not_before: u64) -> Self {
        AuthTokens {
            secret,
            port,
            not_before,
            used: Default::default(),
        }
    }

    /// Accepts each valid token once. `now` is the current Unix time in seconds.
    pub fn redeem(&self, token: &str, now: u64) -> Result<(), TokenError> {
        let (ts, rand) = verify_auth_token(&self.secret, self.port, token, now, self.not_before)?;
        let mut used = self.used.lock().expect("token store poisoned");
        used.retain(|_, expires| *expires >= now);
        if used.insert(rand.to_string(), ts + TOKEN_TTL_SECS).is_some() {
            return Err(TokenError::Replayed);
        }
        Ok(())
    }
}

/// Random per-browser session values minted by `/__auth`, kept in memory only (a server
/// restart logs every browser out; `gh-tideline` authorizes it again).
pub struct Sessions {
    ttl: Duration,
    live: Mutex<Vec<(String, Instant)>>,
}

impl Sessions {
    pub fn new(ttl: Duration) -> Self {
        Sessions {
            ttl,
            live: Default::default(),
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Creates a session and returns its cookie value.
    pub fn create(&self) -> String {
        let value = hex::encode(rand::random::<[u8; 32]>());
        let now = Instant::now();
        let mut live = self.live.lock().expect("session store poisoned");
        live.retain(|(_, expires)| *expires > now);
        live.push((value.clone(), now + self.ttl));
        value
    }

    /// Whether `value` is a live session (constant-time comparison against each one).
    pub fn is_valid(&self, value: &str) -> bool {
        let now = Instant::now();
        self.live
            .lock()
            .expect("session store poisoned")
            .iter()
            .filter(|(_, expires)| *expires > now)
            .fold(false, |found, (v, _)| {
                found | constant_time_eq(v.as_bytes(), value.as_bytes())
            })
    }
}

fn header_str(headers: &HeaderMap, name: impl header::AsHeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Every value of cookie `name` across all `Cookie` headers. Another page on `localhost`
/// can plant ("toss") a cookie with the same name, so all of them must be considered.
fn cookie_values<'a>(headers: &'a HeaderMap, name: &'a str) -> impl Iterator<Item = &'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .filter(move |(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn is_local_authority(value: &str, port: u16) -> bool {
    value == format!("localhost:{port}") || value == format!("127.0.0.1:{port}")
}

/// Decides whether a request may proceed. Pure function of method, path, headers, policy
/// and the session check.
pub fn decide(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    policy: &Policy,
    session_ok: &dyn Fn(&str) -> bool,
) -> Decision {
    match header_str(headers, header::HOST) {
        Some(host) if is_local_authority(host, policy.port) => {}
        _ => return Decision::Misdirected,
    }

    if method != Method::GET {
        let origin_ok = header_str(headers, header::ORIGIN).is_some_and(|o| {
            o.strip_prefix("http://")
                .is_some_and(|rest| is_local_authority(rest, policy.port))
        });
        if !origin_ok || header_str(headers, "x-dr") != Some("1") {
            return Decision::Reject(AppError::unauthorized(
                "non-GET requests need a same-origin Origin header and X-DR: 1",
            ));
        }
    }

    let Some(api_path) = path.strip_prefix("/api/") else {
        return if path == "/api" {
            Decision::Reject(AppError::unauthorized("missing session"))
        } else {
            Decision::Allow
        };
    };
    if path == "/api/health" {
        return Decision::Allow;
    }

    if !cookie_values(headers, &policy.cookie_name).any(session_ok) {
        return Decision::Reject(AppError::unauthorized(
            "this browser is not authorized (or the server restarted); run `gh-tideline` in a terminal to authorize it",
        ));
    }
    let same_origin = header_str(headers, "sec-fetch-site") == Some("same-origin")
        || header_str(headers, "x-dr") == Some("1");
    if !same_origin {
        return Decision::Reject(AppError::unauthorized(
            "API requests need Sec-Fetch-Site: same-origin or X-DR: 1",
        ));
    }

    let host = api_path.split('/').next().unwrap_or_default();
    if !policy.allowed_hosts.iter().any(|h| h == host) {
        return Decision::Reject(AppError::forbidden_host(host));
    }
    Decision::Allow
}

/// `next` for `/__auth` must be a local absolute path: starts with `/`, not `//`, and
/// contains no backslash or control characters (browsers treat `/\` like `//`).
pub fn valid_next(next: &str) -> bool {
    next.starts_with('/')
        && !next.starts_with("//")
        && !next.contains('\\')
        && !next.chars().any(|c| c.is_control())
}

pub async fn middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let session_ok = |v: &str| state.sessions.is_valid(v);
    match decide(
        req.method(),
        req.uri().path(),
        req.headers(),
        &state.policy,
        &session_ok,
    ) {
        Decision::Allow => next.run(req).await,
        Decision::Misdirected => (
            StatusCode::MISDIRECTED_REQUEST,
            "misdirected request: unexpected Host header\n",
        )
            .into_response(),
        Decision::Reject(err) => err.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use axum::http::HeaderValue;

    const PORT: u16 = 7457;
    const GOOD: &str = "good-session";

    fn policy() -> Policy {
        Policy {
            port: PORT,
            cookie_name: cookie_name(PORT),
            allowed_hosts: vec!["github.com".into(), "ghe.example.com".into()],
        }
    }

    fn decide(method: &Method, path: &str, headers: &HeaderMap, policy: &Policy) -> Decision {
        super::decide(method, path, headers, policy, &|v: &str| v == GOOD)
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn cookie() -> String {
        format!("other=1; dr_session_{PORT}={GOOD}")
    }

    fn code(d: Decision) -> Option<ErrorCode> {
        match d {
            Decision::Reject(e) => Some(e.code),
            _ => None,
        }
    }

    const API: &str = "/api/github.com/cli/cli/pulls/1";
    const SECRET: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const RAND: &str = "0123456789abcdef0123456789abcdef";
    const NOW: u64 = 1_800_000_000;

    #[test]
    fn host_header_must_be_local_with_port() {
        let p = policy();
        for host in ["localhost:7457", "127.0.0.1:7457"] {
            let h = headers(&[("host", host)]);
            assert_eq!(decide(&Method::GET, "/", &h, &p), Decision::Allow, "{host}");
        }
        for host in [
            "evil.com",
            "localhost",
            "127.0.0.1",
            "localhost:7458",
            "evil.com:7457",
            "localhost.evil.com:7457",
            "[::1]:7457",
        ] {
            let h = headers(&[("host", host)]);
            assert_eq!(
                decide(&Method::GET, "/", &h, &p),
                Decision::Misdirected,
                "{host}"
            );
        }
        assert_eq!(
            decide(&Method::GET, "/", &HeaderMap::new(), &p),
            Decision::Misdirected
        );
    }

    #[test]
    fn only_health_needs_no_cookie() {
        let p = policy();
        let h = headers(&[("host", "localhost:7457")]);
        assert_eq!(decide(&Method::GET, "/api/health", &h, &p), Decision::Allow);
        let h = headers(&[
            ("host", "127.0.0.1:7457"),
            ("origin", "http://127.0.0.1:7457"),
            ("x-dr", "1"),
        ]);
        // The old secret-for-nonce endpoint is gone and gets no exemption.
        assert_eq!(
            code(decide(&Method::POST, "/api/auth/nonce", &h, &p)),
            Some(ErrorCode::Unauthorized)
        );
    }

    #[test]
    fn api_requires_valid_cookie() {
        let p = policy();
        let h = headers(&[("host", "localhost:7457"), ("x-dr", "1")]);
        assert_eq!(
            code(decide(&Method::GET, API, &h, &p)),
            Some(ErrorCode::Unauthorized)
        );
        for wrong in [
            format!("dr_session_{PORT}=bad"),
            // Right value under another port's (or the old, port-less) cookie name.
            format!("dr_session_7458={GOOD}"),
            format!("dr_session={GOOD}"),
        ] {
            let h = headers(&[
                ("host", "localhost:7457"),
                ("x-dr", "1"),
                ("cookie", &wrong),
            ]);
            assert_eq!(
                code(decide(&Method::GET, API, &h, &p)),
                Some(ErrorCode::Unauthorized),
                "{wrong}"
            );
        }
        let h = headers(&[
            ("host", "localhost:7457"),
            ("x-dr", "1"),
            ("cookie", &cookie()),
        ]);
        assert_eq!(decide(&Method::GET, API, &h, &p), Decision::Allow);
        // Cookie split across multiple headers is still found.
        let c2 = format!("dr_session_{PORT}={GOOD}");
        let h = headers(&[
            ("host", "localhost:7457"),
            ("x-dr", "1"),
            ("cookie", "a=b"),
            ("cookie", &c2),
        ]);
        assert_eq!(decide(&Method::GET, API, &h, &p), Decision::Allow);
    }

    #[test]
    fn any_cookie_with_our_name_may_match() {
        // A tossed cookie with the same name (sent first, e.g. with a longer Path) must not
        // hide the real one, and vice versa.
        let p = policy();
        for c in [
            format!("dr_session_{PORT}=tossed; dr_session_{PORT}={GOOD}"),
            format!("dr_session_{PORT}={GOOD}; dr_session_{PORT}=tossed"),
        ] {
            let h = headers(&[("host", "localhost:7457"), ("x-dr", "1"), ("cookie", &c)]);
            assert_eq!(decide(&Method::GET, API, &h, &p), Decision::Allow, "{c}");
        }
        let h = headers(&[
            ("host", "localhost:7457"),
            ("x-dr", "1"),
            ("cookie", &format!("dr_session_{PORT}=tossed")),
            ("cookie", &format!("dr_session_{PORT}={GOOD}")),
        ]);
        assert_eq!(decide(&Method::GET, API, &h, &p), Decision::Allow);
        let c = format!("dr_session_{PORT}=x; dr_session_{PORT}=y");
        let h = headers(&[("host", "localhost:7457"), ("x-dr", "1"), ("cookie", &c)]);
        assert_eq!(
            code(decide(&Method::GET, API, &h, &p)),
            Some(ErrorCode::Unauthorized)
        );
    }

    #[test]
    fn api_requires_same_origin_signal() {
        let p = policy();
        let c = cookie();
        for site in ["cross-site", "same-site", "none"] {
            let h = headers(&[
                ("host", "localhost:7457"),
                ("cookie", &c),
                ("sec-fetch-site", site),
            ]);
            assert_eq!(
                code(decide(&Method::GET, API, &h, &p)),
                Some(ErrorCode::Unauthorized),
                "{site}"
            );
        }
        let h = headers(&[("host", "localhost:7457"), ("cookie", &c)]);
        assert_eq!(
            code(decide(&Method::GET, API, &h, &p)),
            Some(ErrorCode::Unauthorized)
        );
        let h = headers(&[("host", "localhost:7457"), ("cookie", &c), ("x-dr", "0")]);
        assert_eq!(
            code(decide(&Method::GET, API, &h, &p)),
            Some(ErrorCode::Unauthorized)
        );
        let h = headers(&[
            ("host", "localhost:7457"),
            ("cookie", &c),
            ("sec-fetch-site", "same-origin"),
        ]);
        assert_eq!(decide(&Method::GET, API, &h, &p), Decision::Allow);
    }

    #[test]
    fn non_get_requires_origin_and_x_dr() {
        let p = policy();
        let c = cookie();
        let ok = headers(&[
            ("host", "localhost:7457"),
            ("cookie", &c),
            ("origin", "http://localhost:7457"),
            ("x-dr", "1"),
        ]);
        // Every write method of the API (viewed marks, threads, comments, the review).
        let writes = [Method::PUT, Method::POST, Method::PATCH, Method::DELETE];
        for m in &writes {
            assert_eq!(decide(m, API, &ok, &p), Decision::Allow, "{m}");
        }
        for origin in [
            "http://evil.com",
            "https://localhost:7457",
            "http://localhost:7458",
            "null",
        ] {
            let h = headers(&[
                ("host", "localhost:7457"),
                ("cookie", &c),
                ("origin", origin),
                ("x-dr", "1"),
            ]);
            assert_eq!(
                code(decide(&Method::PUT, API, &h, &p)),
                Some(ErrorCode::Unauthorized),
                "{origin}"
            );
        }
        let no_xdr = headers(&[
            ("host", "localhost:7457"),
            ("cookie", &c),
            ("origin", "http://localhost:7457"),
            ("sec-fetch-site", "same-origin"),
        ]);
        let no_origin = headers(&[("host", "localhost:7457"), ("cookie", &c), ("x-dr", "1")]);
        for m in &writes {
            assert_eq!(
                code(decide(m, API, &no_xdr, &p)),
                Some(ErrorCode::Unauthorized),
                "{m} without X-DR"
            );
            assert_eq!(
                code(decide(m, API, &no_origin, &p)),
                Some(ErrorCode::Unauthorized),
                "{m} without Origin"
            );
        }
        // CORS preflights from other origins are rejected as well.
        let preflight = headers(&[("host", "localhost:7457"), ("origin", "http://evil.com")]);
        assert_eq!(
            code(decide(&Method::OPTIONS, API, &preflight, &p)),
            Some(ErrorCode::Unauthorized)
        );
    }

    #[test]
    fn path_host_allowlist() {
        let p = policy();
        let c = cookie();
        let h = headers(&[("host", "localhost:7457"), ("cookie", &c), ("x-dr", "1")]);
        assert_eq!(decide(&Method::GET, API, &h, &p), Decision::Allow);
        assert_eq!(
            decide(&Method::GET, "/api/ghe.example.com/o/r/blobs/x", &h, &p),
            Decision::Allow
        );
        for path in [
            "/api/evil.com/o/r/pulls/1",
            "/api/github.com.evil.com/o/r/pulls/1",
            "/api/GitHub.com/o/r/pulls/1",
            "/api/github%2Ecom/o/r/pulls/1",
            "/api//o/r/pulls/1",
            "/api/auth/other",
        ] {
            assert_eq!(
                code(decide(&Method::GET, path, &h, &p)),
                Some(ErrorCode::ForbiddenHost),
                "{path}"
            );
        }
    }

    #[test]
    fn static_paths_need_no_cookie() {
        let p = policy();
        let h = headers(&[("host", "localhost:7457")]);
        for path in [
            "/",
            "/github.com/cli/cli/pull/1",
            "/assets/index.js",
            "/__auth",
            "/apifoo",
        ] {
            assert_eq!(
                decide(&Method::GET, path, &h, &p),
                Decision::Allow,
                "{path}"
            );
        }
        assert_eq!(
            code(decide(&Method::GET, "/api", &h, &p)),
            Some(ErrorCode::Unauthorized)
        );
    }

    #[test]
    fn next_validation() {
        for ok in ["/", "/github.com/cli/cli/pull/1", "/x?file=a%2Fb", "/a//b"] {
            assert!(valid_next(ok), "{ok}");
        }
        for bad in [
            "",
            "//evil.com",
            "///evil.com",
            "/\\evil.com",
            "https://evil.com",
            "evil.com",
            "javascript:alert(1)",
            "/foo\nbar",
            "/foo\tbar",
        ] {
            assert!(!valid_next(bad), "{bad:?}");
        }
    }

    #[test]
    fn auth_token_roundtrip() {
        let t = mint_auth_token(SECRET, PORT, NOW, RAND);
        assert!(t.starts_with(&format!("{NOW}.{RAND}.")));
        assert_eq!(t.len(), 10 + 1 + 32 + 1 + 64);
        assert_eq!(verify_auth_token(SECRET, PORT, &t, NOW, 0), Ok((NOW, RAND)));
        // Still valid at the edge of the window and with small forward skew.
        assert!(verify_auth_token(SECRET, PORT, &t, NOW + TOKEN_TTL_SECS, 0).is_ok());
        assert!(verify_auth_token(SECRET, PORT, &t, NOW - CLOCK_SKEW_SECS, 0).is_ok());
    }

    #[test]
    fn auth_token_rejections() {
        let t = mint_auth_token(SECRET, PORT, NOW, RAND);
        let verify = |token: &str, port: u16, now: u64| {
            verify_auth_token(SECRET, port, token, now, 0).map(|_| ())
        };
        // Expired, and too far in the future.
        assert_eq!(
            verify(&t, PORT, NOW + TOKEN_TTL_SECS + 1),
            Err(TokenError::Expired)
        );
        assert_eq!(
            verify(&t, PORT, NOW - CLOCK_SKEW_SECS - 1),
            Err(TokenError::Expired)
        );
        // Minted before the server started (e.g. captured by a port squatter).
        assert_eq!(
            verify_auth_token(SECRET, PORT, &t, NOW, NOW + 1),
            Err(TokenError::Expired)
        );
        // Bound to the port.
        assert_eq!(verify(&t, PORT + 1, NOW), Err(TokenError::BadMac));
        // Other secret, tampered fields, tampered MAC.
        let other = mint_auth_token(&SECRET.replace('0', "1"), PORT, NOW, RAND);
        assert_eq!(verify(&other, PORT, NOW), Err(TokenError::BadMac));
        let mac = t.rsplit('.').next().unwrap();
        let shifted_ts = format!("{}.{RAND}.{mac}", NOW + 1);
        assert_eq!(verify(&shifted_ts, PORT, NOW), Err(TokenError::BadMac));
        let other_rand = format!("{NOW}.{}.{mac}", "f".repeat(32));
        assert_eq!(verify(&other_rand, PORT, NOW), Err(TokenError::BadMac));
        let mut bad_mac = t.clone();
        let last = bad_mac.pop().unwrap();
        bad_mac.push(if last == '0' { '1' } else { '0' });
        assert_eq!(verify(&bad_mac, PORT, NOW), Err(TokenError::BadMac));
        let short_mac = &t[..t.len() - 2];
        assert_eq!(verify(short_mac, PORT, NOW), Err(TokenError::BadMac));
        // Malformed.
        for bad in [
            String::new(),
            "x".into(),
            format!("{NOW}.{RAND}"),
            format!("{t}.x"),
            format!("-1.{RAND}.{mac}"),
            format!("+{NOW}.{RAND}.{mac}"),
            format!("{NOW}.{}.{mac}", RAND.to_uppercase()),
            format!("{NOW}.{}.{mac}", &RAND[..31]),
            format!("{NOW}.{RAND}.zz"),
            format!("{}.{RAND}.{mac}", "9".repeat(21)),
            SECRET.to_string(),
        ] {
            assert_eq!(verify(&bad, PORT, NOW), Err(TokenError::Malformed), "{bad}");
        }
    }

    #[test]
    fn auth_tokens_are_single_use() {
        let store = AuthTokens::new(SECRET.into(), PORT, NOW - 10);
        let t = mint_auth_token(SECRET, PORT, NOW, RAND);
        assert_eq!(store.redeem(&t, NOW), Ok(()));
        assert_eq!(store.redeem(&t, NOW + 1), Err(TokenError::Replayed));
        // A different token is unaffected; after expiry the replay is still refused.
        let t2 = mint_auth_token(SECRET, PORT, NOW, &"a".repeat(32));
        assert_eq!(store.redeem(&t2, NOW + 2), Ok(()));
        assert_eq!(
            store.redeem(&t, NOW + TOKEN_TTL_SECS + 1),
            Err(TokenError::Expired)
        );
        // Old token rejected by the server-start bound.
        let early = mint_auth_token(SECRET, PORT, NOW - 11, &"b".repeat(32));
        assert_eq!(store.redeem(&early, NOW), Err(TokenError::Expired));
        // Failed attempts do not burn a valid token.
        let t3 = mint_auth_token(SECRET, PORT, NOW, &"c".repeat(32));
        assert_eq!(store.redeem(&t3, NOW + 100), Err(TokenError::Expired));
        assert_eq!(store.redeem(&t3, NOW), Ok(()));
    }

    #[test]
    fn sessions_are_random_and_expire() {
        let sessions = Sessions::new(Duration::from_secs(3600));
        let a = sessions.create();
        let b = sessions.create();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert!(sessions.is_valid(&a) && sessions.is_valid(&b));
        assert!(!sessions.is_valid("x"));
        assert!(!sessions.is_valid(""));
        let expired = Sessions::new(Duration::ZERO);
        let c = expired.create();
        assert!(!expired.is_valid(&c));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
