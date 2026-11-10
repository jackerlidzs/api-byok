//! Tests one API key and its proxy: which IP the provider sees, and whether the provider
//! accepts the key for a short request to a model of that provider.
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    model::{ModelConfig, ProviderType},
    network::NetworkClients,
    provider::{custom_headers, keys},
    store::{normalize_proxy, provider_host, ProviderApiKey, Store},
    Error, Result,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const IP_LOOKUP_URL: &str = "https://api.ipify.org";
const TEST_PROMPT: &str = "Reply with OK.";
const DETAIL_LIMIT: usize = 300;

#[derive(Clone, Debug, Deserialize)]
pub struct ApiKeyTestInput {
    /// Provider host the key belongs to, e.g. `api.anthropic.com`.
    pub provider: String,
    pub key: ProviderApiKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyTestStatus {
    /// The provider answered the test request successfully.
    Ok,
    /// The provider refused the key: 401, 402, 403, 429 or 5xx.
    Rejected,
    /// The provider was reached but refused the test request for another reason (e.g. 400).
    RequestFailed,
    /// The provider could not be reached through this key's connection.
    Unreachable,
    /// No model uses this provider, so only the connection was checked.
    NoModel,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApiKeyTestResult {
    pub status: KeyTestStatus,
    /// The model the test request was sent to.
    pub model: Option<String>,
    pub http_status: Option<u16>,
    pub detail: Option<String>,
    pub duration_ms: u64,
    /// The public IP the provider sees for this key's connection.
    pub egress_ip: Option<String>,
    pub egress_error: Option<String>,
}

pub(super) async fn run(
    store: &Store,
    clients: &NetworkClients,
    input: ApiKeyTestInput,
    local_proxy_port: u16,
) -> Result<ApiKeyTestResult> {
    let provider = input.provider.trim().to_ascii_lowercase();
    let api_key = input.key.key.trim().to_owned();
    if api_key.is_empty() {
        return Err(Error::Config("API key cannot be empty".into()));
    }
    let proxy = normalize_proxy(input.key.proxy, &provider)?;
    if proxy.mode == crate::store::KeyProxyMode::Custom {
        crate::network::reject_self_proxy(&proxy.address, local_proxy_port)?;
    }
    let client = clients.key_test_client(&proxy, TEST_TIMEOUT).await?;
    let model = store
        .models()
        .await?
        .into_iter()
        .find(|model| provider_host(&model.base_url).as_deref() == Some(provider.as_str()));

    let started = Instant::now();
    let (egress, outcome) = tokio::join!(egress_ip(&client), async {
        match &model {
            Some(model) => test_key(&client, model, &api_key).await,
            None => Ok(Outcome::only(KeyTestStatus::NoModel)),
        }
    });
    let outcome = outcome?;
    let (egress_ip, egress_error) = match egress {
        Ok(ip) => (Some(ip), None),
        Err(error) => (None, Some(error)),
    };
    Ok(ApiKeyTestResult {
        status: outcome.status,
        model: model.map(|model| model.display_name),
        http_status: outcome.http_status,
        detail: outcome.detail,
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        egress_ip,
        egress_error,
    })
}

struct Outcome {
    status: KeyTestStatus,
    http_status: Option<u16>,
    detail: Option<String>,
}

impl Outcome {
    fn only(status: KeyTestStatus) -> Self {
        Self {
            status,
            http_status: None,
            detail: None,
        }
    }
}

/// The public IP seen by servers reached through `client`.
pub(super) async fn egress_ip(client: &reqwest::Client) -> std::result::Result<String, String> {
    let response = client
        .get(IP_LOOKUP_URL)
        .send()
        .await
        .map_err(|error| transport_error(&error))?;
    if !response.status().is_success() {
        return Err(format!("IP lookup returned HTTP {}", response.status()));
    }
    let ip = response
        .text()
        .await
        .map_err(|error| transport_error(&error))?;
    Ok(ip.trim().to_owned())
}

/// Sends one short, non-streaming request with `api_key` to `model`.
async fn test_key(client: &reqwest::Client, model: &ModelConfig, api_key: &str) -> Result<Outcome> {
    let provider_type = model.provider_type();
    let mut headers = HeaderMap::new();
    if model.custom_headers_enabled {
        headers.extend(custom_headers(&model.custom_headers, "api-key-test")?);
    }
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let auth = match provider_type {
        ProviderType::Anthropic => {
            headers
                .entry("anthropic-version")
                .or_insert(HeaderValue::from_static("2023-06-01"));
            ("x-api-key", api_key.to_owned())
        }
        _ => ("authorization", format!("Bearer {api_key}")),
    };
    let value = HeaderValue::from_str(&auth.1)
        .map_err(|_| Error::Config("API key contains characters not allowed in a header".into()))?;
    headers.insert(auth.0, value);

    let response = client
        .post(model.request_url()?)
        .headers(headers)
        .json(&request_body(provider_type, &model.model_id))
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return Ok(Outcome {
                status: KeyTestStatus::Unreachable,
                http_status: None,
                detail: Some(transport_error(&error)),
            });
        }
    };
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Ok(Outcome {
        status: classify(status),
        http_status: Some(status.as_u16()),
        detail: (!status.is_success()).then(|| shorten(&body)),
    })
}

fn request_body(provider_type: ProviderType, model_id: &str) -> Value {
    match provider_type {
        ProviderType::Anthropic => json!({
            "model": model_id,
            "max_tokens": 16,
            "messages": [{ "role": "user", "content": TEST_PROMPT }],
        }),
        ProviderType::OpenAiResponses => json!({
            "model": model_id,
            "input": TEST_PROMPT,
        }),
        _ => json!({
            "model": model_id,
            "messages": [{ "role": "user", "content": TEST_PROMPT }],
        }),
    }
}

fn classify(status: reqwest::StatusCode) -> KeyTestStatus {
    if status.is_success() {
        KeyTestStatus::Ok
    } else if keys::rejected(status) {
        KeyTestStatus::Rejected
    } else {
        KeyTestStatus::RequestFailed
    }
}

/// A readable reason for a request that got no HTTP response.
pub(super) fn transport_error(error: &reqwest::Error) -> String {
    let kind = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    let mut root: &dyn std::error::Error = error;
    while let Some(source) = root.source() {
        root = source;
    }
    format!("{kind}: {root}")
}

fn shorten(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(DETAIL_LIMIT) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_provider_answers() {
        let status = |code| reqwest::StatusCode::from_u16(code).unwrap();
        assert_eq!(classify(status(200)), KeyTestStatus::Ok);
        for code in [401, 402, 403, 429, 500, 503] {
            assert_eq!(classify(status(code)), KeyTestStatus::Rejected);
        }
        for code in [400, 404, 422] {
            assert_eq!(classify(status(code)), KeyTestStatus::RequestFailed);
        }
    }

    #[test]
    fn builds_a_request_for_each_protocol() {
        let anthropic = request_body(ProviderType::Anthropic, "claude");
        assert_eq!(anthropic["max_tokens"], 16);
        assert_eq!(anthropic["messages"][0]["content"], TEST_PROMPT);
        assert_eq!(
            request_body(ProviderType::OpenAiResponses, "gpt")["input"],
            TEST_PROMPT
        );
        let chat = request_body(ProviderType::OpenAiChat, "gpt");
        assert_eq!(chat["model"], "gpt");
        assert!(chat.get("stream").is_none());
    }

    #[test]
    fn shortens_long_error_bodies() {
        assert_eq!(shorten("  short  "), "short");
        let long = "é".repeat(DETAIL_LIMIT + 5);
        assert_eq!(shorten(&long).chars().count(), DETAIL_LIMIT + 1);
    }

    #[tokio::test]
    async fn reports_an_unreachable_proxy_without_failing() {
        let client = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("socks5h://127.0.0.1:9").unwrap())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        assert!(egress_ip(&client).await.is_err());
    }
}
