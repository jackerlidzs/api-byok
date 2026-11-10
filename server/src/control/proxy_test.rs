//! Tests the outbound proxy settings before they are saved: the public IP seen through them,
//! and whether each configured provider answers through them.
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use futures_util::future::join_all;
use serde::Serialize;

use crate::{
    network,
    store::{ProxySettingsInput, Store},
    Result,
};

use super::key_test::{egress_ip, transport_error};

const TEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Providers checked per test; each is one request.
const MAX_PROVIDERS: usize = 8;

#[derive(Clone, Debug, Serialize)]
pub struct ProxyTestResult {
    /// The proxy as it will be saved: `scheme://host:port`, empty in `Default` mode.
    pub address: String,
    /// Whether the proxy authenticates, including credentials found in the pasted address.
    pub auth_enabled: bool,
    pub username: String,
    /// The public IP servers see through these settings.
    pub egress_ip: Option<String>,
    pub egress_error: Option<String>,
    pub duration_ms: u64,
    pub providers: Vec<ProviderReach>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderReach {
    /// `host` or `host:port` of a configured model's server address.
    pub host: String,
    /// The provider answered with an HTTP status (any status but the proxy's 407).
    pub reachable: bool,
    pub http_status: Option<u16>,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// `input` must already be normalized (`normalize_proxy_input`). An empty password with
/// authentication on means "keep the saved password", as when saving.
pub(super) async fn run(store: &Store, input: ProxySettingsInput) -> Result<ProxyTestResult> {
    let custom = input.mode.is_custom();
    let password = match input
        .password
        .clone()
        .filter(|password| !password.is_empty())
    {
        Some(password) => password,
        None if custom && input.auth_enabled => store.proxy_settings_secret().await?.password,
        None => String::new(),
    };
    let auth = (custom && input.auth_enabled).then_some((&input.username, &password));
    let proxy = custom.then_some(input.address.as_str());
    let client = network::proxy_test_client(proxy, auth, TEST_TIMEOUT)?;
    let targets = provider_targets(store).await?;

    let started = Instant::now();
    let (egress, providers) = tokio::join!(
        egress_ip(&client),
        join_all(targets.into_iter().map(|target| reach(&client, target)))
    );
    let (egress_ip, egress_error) = match egress {
        Ok(ip) => (Some(ip), None),
        Err(error) => (None, Some(error)),
    };
    Ok(ProxyTestResult {
        address: if custom { input.address } else { String::new() },
        auth_enabled: custom && input.auth_enabled,
        username: if custom && input.auth_enabled {
            input.username
        } else {
            String::new()
        },
        egress_ip,
        egress_error,
        duration_ms: elapsed_ms(started),
        providers,
    })
}

struct Target {
    host: String,
    url: String,
}

/// The distinct servers of the configured models, in model order.
async fn provider_targets(store: &Store) -> Result<Vec<Target>> {
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for model in store.models().await? {
        let Ok(url) = url::Url::parse(model.base_url.trim()) else {
            continue;
        };
        let Some(host) = url.host_str() else {
            continue;
        };
        let host = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        if seen.insert(host.clone()) {
            let url = format!("{}://{host}/", url.scheme());
            targets.push(Target { host, url });
        }
        if targets.len() == MAX_PROVIDERS {
            break;
        }
    }
    Ok(targets)
}

/// Any HTTP answer means the provider is reachable; the root path usually answers 404.
async fn reach(client: &reqwest::Client, target: Target) -> ProviderReach {
    let started = Instant::now();
    match client.get(&target.url).send().await {
        Ok(response) => {
            let status = response.status();
            let proxy_refused = status == reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED;
            ProviderReach {
                host: target.host,
                reachable: !proxy_refused,
                http_status: Some(status.as_u16()),
                duration_ms: elapsed_ms(started),
                error: proxy_refused.then(|| "the proxy rejected the credentials".to_owned()),
            }
        }
        Err(error) => ProviderReach {
            host: target.host,
            reachable: false,
            http_status: None,
            duration_ms: elapsed_ms(started),
            error: Some(transport_error(&error)),
        },
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::{http::StatusCode, routing::get, Router};

    use crate::store::ProxyMode;

    async fn store() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", directory.path().join("test.db").display());
        (directory, Store::connect(&url).await.unwrap())
    }

    async fn serve(status: StatusCode) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new().route("/", get(move || async move { status }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}/")
    }

    fn target(url: &str) -> Target {
        Target {
            host: "provider".into(),
            url: url.into(),
        }
    }

    #[tokio::test]
    async fn any_http_answer_but_407_counts_as_reachable() {
        let client = reqwest::Client::new();
        let answered = reach(&client, target(&serve(StatusCode::NOT_FOUND).await)).await;
        assert!(answered.reachable);
        assert_eq!(answered.http_status, Some(404));

        let refused = serve(StatusCode::PROXY_AUTHENTICATION_REQUIRED).await;
        let refused = reach(&client, target(&refused)).await;
        assert!(!refused.reachable);
        assert_eq!(refused.http_status, Some(407));

        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", closed.local_addr().unwrap());
        drop(closed);
        let unreachable = reach(&client, target(&url)).await;
        assert!(!unreachable.reachable);
        assert!(unreachable.error.unwrap().starts_with("connection failed"));
    }

    #[tokio::test]
    async fn an_unreachable_proxy_is_reported_without_failing() {
        let (_directory, store) = store().await;
        let input = crate::store::normalize_proxy_input(ProxySettingsInput {
            mode: ProxyMode::Custom,
            address: "127.0.0.1:9:alice:secret".into(),
            auth_enabled: false,
            username: String::new(),
            password: None,
        })
        .unwrap();

        let result = run(&store, input).await.unwrap();

        assert_eq!(result.address, "http://127.0.0.1:9");
        assert!(result.auth_enabled);
        assert_eq!(result.username, "alice");
        assert!(result.egress_ip.is_none());
        assert!(result.egress_error.is_some());
        assert!(result.providers.is_empty());
    }
}
