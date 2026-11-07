//! Sends one Provider HTTP attempt, moving to the next API key when a key is rejected.

use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

use super::{keys, CallRecorder};

#[derive(Debug)]
pub(crate) enum Attempt {
    Response(reqwest::Response),
    Cancelled,
}

pub(crate) async fn send<F>(
    label: &str,
    api_keys: &[String],
    build: F,
    cancellation: &CancellationToken,
    recorder: Option<&CallRecorder>,
) -> Result<Attempt>
where
    F: Fn(&str) -> reqwest::RequestBuilder,
{
    for (index, api_key) in api_keys.iter().enumerate() {
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Ok(Attempt::Cancelled),
            response = build(api_key.as_str()).send() => response,
        }?;
        let status = response.status();
        if index + 1 < api_keys.len() && keys::rejected(status) {
            tracing::warn!(
                label,
                key_index = index + 1,
                key_count = api_keys.len(),
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

    fn one_key() -> Vec<String> {
        vec!["key".to_owned()]
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

    #[tokio::test]
    async fn non_success_status_is_one_failed_attempt() {
        let url =
            server(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\n\r\ndown").await;
        let client = reqwest::Client::new();
        let error = send(
            "test",
            &one_key(),
            |_| client.get(&url),
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
        let client = reqwest::Client::new();
        let error = send(
            "test",
            &one_key(),
            |_| client.get(&url),
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
        let client = reqwest::Client::new();
        let error = send(
            "test",
            &one_key(),
            |_| client.get(&url),
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
        let api_keys = ["exhausted".to_owned(), "fresh".to_owned()];
        let attempt = send(
            "test",
            &api_keys,
            |key| client.get(&url).bearer_auth(key),
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
        let api_keys = ["exhausted".to_owned(), "exhausted".to_owned()];
        let error = send(
            "test",
            &api_keys,
            |key| client.get(&url).bearer_auth(key),
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
        let client = reqwest::Client::new();
        let error = send(
            "test",
            &[],
            |_| client.get("http://127.0.0.1:9"),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Config(_)));
    }
}
