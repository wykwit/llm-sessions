//! Who may connect: the bind/token rules checked before listening, and the
//! per-request token check.
//!
//! Default closed. A loopback bind still needs `--token` unless `--no-auth`
//! says otherwise, and `--no-auth` is refused outright off loopback: every
//! transcript holds code, paths and often secrets.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Query, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::router::{ApiError, App};

/// The token to require, or `None` for an unauthenticated loopback server.
pub fn admit(
    bind: SocketAddr,
    token: Option<String>,
    no_auth: bool,
) -> Result<Option<String>, String> {
    let loopback = bind.ip().to_canonical().is_loopback();
    match (token, no_auth) {
        (Some(_), true) => Err("--token and --no-auth contradict each other".into()),
        (Some(token), false) if token.trim().is_empty() => Err("--token must not be empty".into()),
        (Some(token), false) => Ok(Some(token)),
        (None, true) if loopback => Ok(None),
        (None, true) => Err(format!(
            "--no-auth is refused for {bind}: only a loopback address may serve without a token"
        )),
        (None, false) => Err(format!(
            "serving {bind} needs --token (or LLMS_TOKEN){}",
            if loopback {
                "; --no-auth serves loopback without one"
            } else {
                ""
            }
        )),
    }
}

/// Length is not secret; the bytes are compared without early exit.
pub(super) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `Authorization: Bearer <token>` only.
pub(super) async fn bearer(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    if authorized(&app, &request, false) {
        next.run(request).await
    } else {
        ApiError::unauthorized().into_response()
    }
}

/// The bearer header, or `?token=` for WebSocket clients that cannot set
/// headers. A query-string token lands in proxy logs, so no other route
/// accepts one.
pub(super) async fn bearer_or_query(
    State(app): State<Arc<App>>,
    request: Request,
    next: Next,
) -> Response {
    if authorized(&app, &request, true) {
        next.run(request).await
    } else {
        ApiError::unauthorized().into_response()
    }
}

fn authorized(app: &App, request: &Request, query: bool) -> bool {
    let Some(expected) = app.token.as_deref() else {
        return true;
    };
    let matches = |offered: &str| constant_time_eq(offered.as_bytes(), expected.as_bytes());
    let from_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    if from_header.is_some_and(matches) {
        return true;
    }
    query
        && Query::<TokenQuery>::try_from_uri(request.uri())
            .ok()
            .and_then(|Query(query)| query.token)
            .is_some_and(|offered| matches(&offered))
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(address: &str) -> SocketAddr {
        address.parse().unwrap()
    }

    #[test]
    fn a_token_is_required_unless_no_auth_is_explicit_on_loopback() {
        let token = || Some("s3cret".to_owned());
        assert_eq!(
            admit(at("0.0.0.0:8787"), token(), false),
            Ok(Some("s3cret".into()))
        );
        assert_eq!(admit(at("127.0.0.1:8787"), None, true), Ok(None));
        assert_eq!(admit(at("[::1]:8787"), None, true), Ok(None));
        assert_eq!(admit(at("[::ffff:127.0.0.1]:8787"), None, true), Ok(None));

        assert!(admit(at("127.0.0.1:8787"), None, false).is_err());
        assert!(admit(at("0.0.0.0:8787"), None, false).is_err());
        assert!(admit(at("0.0.0.0:8787"), None, true).is_err());
        assert!(admit(at("192.168.1.4:8787"), None, true).is_err());
        assert!(admit(at("[::]:8787"), None, true).is_err());
        assert!(admit(at("127.0.0.1:8787"), Some("  ".into()), false).is_err());
        assert!(admit(at("127.0.0.1:8787"), token(), true).is_err());
    }

    #[test]
    fn constant_time_eq_needs_every_byte() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
