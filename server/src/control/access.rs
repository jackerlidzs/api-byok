//! Decides who may use the console and the control API.
//!
//! The control API reads and changes models, upstream API keys, and settings, so it must not be
//! open to the network. A server that listens only on loopback (the desktop app) keeps it open.
//! A server reachable from other machines requires `CURSOR_CONSOLE_PASSWORD` through HTTP Basic
//! authentication (any username), and turns the console off when no password is configured.
use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};

const CHALLENGE: &str = "Basic realm=\"Cursor BYOK console\", charset=\"UTF-8\"";
const PASSWORD_REQUIRED: &str = "console password required";
const CONSOLE_DISABLED: &str = "The console is disabled because this server is reachable from \
                                the network. Set CURSOR_CONSOLE_PASSWORD and restart the server.";

/// Applies the access policy to `console`, the router that serves the console and the control
/// API of a server listening on `listen_addr`.
pub fn guard(console: Router, listen_addr: SocketAddr, password: Option<&str>) -> Router {
    if let Some(password) = password {
        let password: Arc<str> = Arc::from(password);
        return console.layer(middleware::from_fn_with_state(password, require_password));
    }
    if listen_addr.ip().is_loopback() {
        return console;
    }
    console.layer(middleware::from_fn(disabled))
}

async fn require_password(
    State(password): State<Arc<str>>,
    request: Request,
    next: Next,
) -> Response {
    if authorized(request.headers(), &password) {
        return next.run(request).await;
    }
    let challenge = [(header::WWW_AUTHENTICATE, CHALLENGE)];
    (StatusCode::UNAUTHORIZED, challenge, PASSWORD_REQUIRED).into_response()
}

async fn disabled(_request: Request, _next: Next) -> Response {
    (StatusCode::FORBIDDEN, CONSOLE_DISABLED).into_response()
}

/// Compares digests so the time taken does not depend on how much of the password matches.
fn authorized(headers: &HeaderMap, password: &str) -> bool {
    let Some(supplied) = basic_password(headers) else {
        return false;
    };
    Sha256::digest(supplied.as_bytes()) == Sha256::digest(password.as_bytes())
}

fn basic_password(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = String::from_utf8(STANDARD.decode(encoded).ok()?).ok()?;
    let (_username, password) = decoded.split_once(':')?;
    Some(password.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::{body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    const CONSOLE_PATH: &str = "/__byok-api__/api/models";
    const HEALTH_PATH: &str = "/__byok-api__/healthz";

    /// A console router guarded for `listen_addr`, merged next to an unguarded health route
    /// the way the application merges the console next to the Cursor and external API routes.
    fn guarded(listen_addr: &str, password: Option<&str>) -> Router {
        let console = Router::new().route(CONSOLE_PATH, get(|| async { "models" }));
        let health = Router::new().route(HEALTH_PATH, get(|| async { "ok" }));
        health.merge(guard(console, listen_addr.parse().unwrap(), password))
    }

    async fn send(app: Router, path: &str, authorization: Option<&str>) -> Response {
        let mut request = Request::builder().uri(path);
        if let Some(authorization) = authorization {
            request = request.header(header::AUTHORIZATION, authorization);
        }
        let request = request.body(Body::empty()).unwrap();
        app.oneshot(request).await.unwrap()
    }

    async fn status(app: Router, path: &str, authorization: Option<&str>) -> StatusCode {
        send(app, path, authorization).await.status()
    }

    fn basic(username: &str, password: &str) -> String {
        let credentials = STANDARD.encode(format!("{username}:{password}"));
        format!("Basic {credentials}")
    }

    #[tokio::test]
    async fn loopback_server_keeps_the_console_open() {
        let app = guarded("127.0.0.1:3000", None);
        assert_eq!(status(app, CONSOLE_PATH, None).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn network_server_without_password_disables_only_the_console() {
        let app = guarded("0.0.0.0:3000", None);
        let console = status(app.clone(), CONSOLE_PATH, None).await;
        assert_eq!(console, StatusCode::FORBIDDEN);
        assert_eq!(status(app, HEALTH_PATH, None).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn network_server_with_password_requires_basic_authentication() {
        let app = guarded("0.0.0.0:3000", Some("secret"));

        let missing = send(app.clone(), CONSOLE_PATH, None).await;
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(missing.headers()[header::WWW_AUTHENTICATE], CHALLENGE);

        let wrong = basic("admin", "guess");
        let wrong = status(app.clone(), CONSOLE_PATH, Some(&wrong)).await;
        assert_eq!(wrong, StatusCode::UNAUTHORIZED);

        let bearer = status(app.clone(), CONSOLE_PATH, Some("Bearer secret")).await;
        assert_eq!(bearer, StatusCode::UNAUTHORIZED);

        let correct = basic("anyone", "secret");
        let correct = status(app.clone(), CONSOLE_PATH, Some(&correct)).await;
        assert_eq!(correct, StatusCode::OK);

        assert_eq!(status(app, HEALTH_PATH, None).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn password_also_guards_a_loopback_server() {
        let app = guarded("127.0.0.1:3000", Some("secret"));
        let missing = status(app, CONSOLE_PATH, None).await;
        assert_eq!(missing, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn passwords_may_contain_colons() {
        let mut headers = HeaderMap::new();
        let value = basic("user", "pa:ss").parse().unwrap();
        headers.insert(header::AUTHORIZATION, value);
        assert!(authorized(&headers, "pa:ss"));
        assert!(!authorized(&headers, "pa"));
    }
}
