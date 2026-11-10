//! Sends one Provider HTTP attempt, moving to the next API key when the provider refuses a key.

use std::time::Duration;

use reqwest::StatusCode;
use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

use super::{
    keys::{self, KeyRoute, Outcome},
    CallRecorder,
};

#[derive(Debug)]
pub(crate) enum Attempt {
    Response(reqwest::Response),
    Cancelled,
}

/// `build` creates the request for one key on that key's client.
///
/// A key the provider refuses (out of credit or quota, rate-limited, not accepted, failing) hands
/// the same request to the next key. A refused request itself, or the last key's refusal, is the
/// error, carrying the provider's status and retry time.
pub(crate) async fn send<F>(
    label: &str,
    routes: &[KeyRoute],
    build: F,
    cancellation: &CancellationToken,
    recorder: Option<&CallRecorder>,
) -> Result<Attempt>
where
    F: Fn(&reqwest::Client, &str) -> reqwest::RequestBuilder,
{
    for (index, route) in routes.iter().enumerate() {
        let has_next = index + 1 < routes.len();
        let sent = tokio::select! {
            _ = cancellation.cancelled() => return Ok(Attempt::Cancelled),
            response = build(&route.client, &route.key).send() => response,
        };
        let response = match sent {
            Err(error) if keys::unreachable(&error) => {
                keys::record(route, Outcome::Unavailable);
                if !has_next {
                    return Err(error.into());
                }
                tracing::warn!(
                    label,
                    key_index = index + 1,
                    key_count = routes.len(),
                    %error,
                    "provider unreachable with this API key; trying the next key"
                );
                continue;
            }
            sent => sent?,
        };
        let status = response.status();
        if status.is_success() {
            keys::record(route, Outcome::Served);
            if let Some(recorder) = recorder {
                recorder.response_headers(status.as_u16()).await?;
            }
            return Ok(Attempt::Response(response));
        }
        let headers = response.headers().clone();
        let body = tokio::select! {
            _ = cancellation.cancelled() => return Ok(Attempt::Cancelled),
            body = response.bytes() => body,
        }?;
        let outcome = keys::classify(status, &headers, &body);
        keys::record(route, outcome);
        if has_next && outcome.fails_over() {
            tracing::warn!(
                label,
                key_index = index + 1,
                key_count = routes.len(),
                %status,
                ?outcome,
                "provider refused this API key; trying the next key"
            );
            continue;
        }
        if let Some(recorder) = recorder {
            recorder.response_headers(status.as_u16()).await?;
        }
        return Err(refused(
            label,
            status,
            outcome.retry_after(),
            &String::from_utf8_lossy(&body),
        ));
    }
    Err(Error::Config(format!("{label} has no API key")))
}

/// The error for a provider's HTTP refusal: `<label> <status>[ (retry after <n>s)]: <body>`.
/// [`refused_status`] reads the status and retry time back.
fn refused(label: &str, status: StatusCode, retry_after: Option<Duration>, body: &str) -> Error {
    let retry = retry_after.map_or_else(String::new, |after| {
        format!(" (retry after {}s)", after.as_secs_f64().ceil())
    });
    Error::Provider(format!("{label} {status}{retry}: {body}"))
}

/// The provider's HTTP status and retry time in an error made by `send`, so an API that relays
/// the call can answer with the same status. `None` for every other error, including errors
/// that arrive inside a stream after the provider accepted the request.
pub(crate) fn refused_status(error: &Error) -> Option<(StatusCode, Option<Duration>)> {
    let Error::Provider(message) = error else {
        return None;
    };
    let (head, _) = message.split_once(": ")?;
    let (head, retry_after) = match head.strip_suffix("s)") {
        Some(rest) => {
            let (head, seconds) = rest.rsplit_once(" (retry after ")?;
            (head, Some(Duration::from_secs(seconds.parse().ok()?)))
        }
        None => (head, None),
    };
    head.match_indices(' ').find_map(|(space, _)| {
        let rest = &head[space + 1..];
        let code = rest.get(..3)?.parse::<u16>().ok()?;
        let status = StatusCode::from_u16(code).ok()?;
        let reason = status.canonical_reason().unwrap_or("<unknown status code>");
        (rest[3..] == format!(" {reason}")
            && (status.is_client_error() || status.is_server_error()))
        .then_some((status, retry_after))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn route(key: &str, client: &reqwest::Client) -> KeyRoute {
        KeyRoute {
            key: key.to_owned(),
            client: client.clone(),
            pool: None,
            first: true,
        }
    }

    fn one_key() -> Vec<KeyRoute> {
        vec![route("key", &reqwest::Client::new())]
    }

    async fn server(response: &'static [u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await;
            socket.write_all(response).await.unwrap();
        });
        format!("http://{address}")
    }

    /// Answers by key: `exhausted` 429 with `retry-after: 7`, `credit` Anthropic's 400 for an
    /// empty balance, `bad-request` an ordinary 400, every other key 200.
    async fn key_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let read = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                let response: &[u8] = if request.contains("bearer exhausted") {
                    b"HTTP/1.1 429 Busy\r\nRetry-After: 7\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbusy"
                } else if request.contains("bearer credit") {
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 45\r\nConnection: close\r\n\r\n{\"message\":\"Your credit balance is too low.\"}"
                } else if request.contains("bearer bad-request") {
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 15\r\nConnection: close\r\n\r\nprompt too long"
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                };
                socket.write_all(response).await.unwrap();
            }
        });
        format!("http://{address}")
    }

    async fn send_keys(keys: &[&str]) -> Result<String> {
        let url = key_server().await;
        let client = reqwest::Client::new();
        let routes = keys
            .iter()
            .map(|key| route(key, &client))
            .collect::<Vec<_>>();
        let attempt = send(
            "test",
            &routes,
            |client, key| client.get(&url).bearer_auth(key),
            &CancellationToken::new(),
            None,
        )
        .await?;
        let Attempt::Response(response) = attempt else {
            panic!("expected a response");
        };
        Ok(response.text().await.unwrap())
    }

    /// An HTTP proxy address where nothing listens.
    async fn dead_proxy_client() -> reqwest::Client {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(format!("http://{address}")).unwrap())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn non_success_status_is_one_failed_attempt() {
        let url =
            server(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\n\r\ndown").await;
        let error = send(
            "test",
            &one_key(),
            |client, _| client.get(&url),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, Error::Provider(message) if message.contains("503") && message.contains("down"))
        );
    }

    #[tokio::test]
    async fn response_body_transport_failure_is_one_failed_attempt() {
        let url =
            server(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 100\r\n\r\nshort").await;
        let error = send(
            "test",
            &one_key(),
            |client, _| client.get(&url),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Http(_)));
    }

    #[tokio::test]
    async fn request_transport_failure_is_one_failed_attempt() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{address}");
        let error = send(
            "test",
            &one_key(),
            |client, _| client.get(&url),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Http(_)));
    }

    #[tokio::test]
    async fn refused_keys_move_to_the_next_key() {
        assert_eq!(send_keys(&["exhausted", "fresh"]).await.unwrap(), "ok");
        // Anthropic reports an empty balance as 400; only the body tells it from a bad request.
        assert_eq!(send_keys(&["credit", "fresh"]).await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn a_refused_request_is_not_sent_with_other_keys() {
        let error = send_keys(&["bad-request", "fresh"]).await.unwrap_err();
        assert_eq!(
            refused_status(&error),
            Some((StatusCode::BAD_REQUEST, None)),
            "{error}"
        );
    }

    #[tokio::test]
    async fn unreachable_key_proxy_moves_to_the_next_key() {
        let url = key_server().await;
        let routes = [
            route("behind-dead-proxy", &dead_proxy_client().await),
            route(
                "direct",
                &reqwest::Client::builder().no_proxy().build().unwrap(),
            ),
        ];
        let attempt = send(
            "test",
            &routes,
            |client, key| client.get(&url).bearer_auth(key),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        let Attempt::Response(response) = attempt else {
            panic!("expected a response");
        };
        assert_eq!(response.text().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn last_rejected_key_returns_its_status_and_retry_time() {
        let error = send_keys(&["exhausted", "exhausted"]).await.unwrap_err();
        let Error::Provider(message) = &error else {
            panic!("expected a provider error");
        };
        assert_eq!(message, "test 429 Too Many Requests (retry after 7s): busy");
        assert_eq!(
            refused_status(&error),
            Some((StatusCode::TOO_MANY_REQUESTS, Some(Duration::from_secs(7))))
        );
    }

    #[test]
    fn only_http_refusals_carry_a_status() {
        let round_trip = |label: &str, code: u16, retry_after: Option<Duration>| {
            let status = StatusCode::from_u16(code).unwrap();
            refused_status(&refused(
                label,
                status,
                retry_after,
                r#"{"error": {"message": "x"}}"#,
            ))
        };
        assert_eq!(
            round_trip("Anthropic", 402, None),
            Some((StatusCode::PAYMENT_REQUIRED, None))
        );
        assert_eq!(
            round_trip("OpenAI Chat", 529, Some(Duration::from_millis(1500))),
            Some((
                StatusCode::from_u16(529).unwrap(),
                Some(Duration::from_secs(2))
            ))
        );
        for message in [
            "Anthropic error 400 (invalid_request_error): The model provider rejected this request.",
            "Anthropic error: Overloaded",
            "Anthropic declined to answer (stop_reason: refusal)",
            "provider stream idle timeout: no events received for 600 seconds (10 minutes)",
            "test 200 OK: fine",
        ] {
            assert_eq!(refused_status(&Error::Provider(message.into())), None, "{message}");
        }
        assert_eq!(refused_status(&Error::Config("x".into())), None);
    }

    #[tokio::test]
    async fn missing_key_is_a_configuration_error() {
        let error = send(
            "test",
            &[],
            |client, _| client.get("http://127.0.0.1:9"),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Config(_)));
    }
}
