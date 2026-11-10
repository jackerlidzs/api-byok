//! Sends one Provider HTTP attempt, moving to the next API key when a key is rejected.

use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

use super::{
    keys::{self, KeyRoute},
    CallRecorder,
};

#[derive(Debug)]
pub(crate) enum Attempt {
    Response(reqwest::Response),
    Cancelled,
}

/// `build` creates the request for one key on that key's client.
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
            Err(error) if has_next && keys::unreachable(&error) => {
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
        keys::record(route, status);
        if has_next && keys::rejected(status) {
            tracing::warn!(
                label,
                key_index = index + 1,
                key_count = routes.len(),
                %status,
                "provider rejected API key; trying the next key"
            );
            continue;
        }
        return accept(label, response, cancellation, recorder).await;
    }
    Err(Error::Config(format!("{label} has no API key")))
}

async fn accept(
    label: &str,
    response: reqwest::Response,
    cancellation: &CancellationToken,
    recorder: Option<&CallRecorder>,
) -> Result<Attempt> {
    if let Some(recorder) = recorder {
        recorder
            .response_headers(response.status().as_u16())
            .await?;
    }
    if response.status().is_success() {
        return Ok(Attempt::Response(response));
    }
    let status = response.status();
    let bytes = tokio::select! {
        _ = cancellation.cancelled() => return Ok(Attempt::Cancelled),
        bytes = response.bytes() => bytes,
    }?;
    Err(Error::Provider(format!(
        "{label} {status}: {}",
        String::from_utf8_lossy(&bytes)
    )))
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

    /// Answers 429 to the key `exhausted` and 200 to every other key.
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
                    b"HTTP/1.1 429 Busy\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbusy"
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
                };
                socket.write_all(response).await.unwrap();
            }
        });
        format!("http://{address}")
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
    async fn rejected_key_moves_to_the_next_key() {
        let url = key_server().await;
        let client = reqwest::Client::new();
        let routes = [route("exhausted", &client), route("fresh", &client)];
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
    async fn last_rejected_key_returns_its_failure() {
        let url = key_server().await;
        let client = reqwest::Client::new();
        let routes = [route("exhausted", &client), route("exhausted", &client)];
        let error = send(
            "test",
            &routes,
            |client, key| client.get(&url).bearer_auth(key),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        let Error::Provider(message) = error else {
            panic!("expected a provider error");
        };
        assert!(message.contains("429"));
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
