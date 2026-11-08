//! Picks the API keys a model call may use and rotates them across calls.
use std::{collections::HashMap, sync::LazyLock, time::Duration};

use parking_lot::Mutex;

use crate::{
    model::ModelConfig,
    network::NetworkClients,
    store::{provider_host, Store},
    Result,
};

static NEXT_KEY: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// One API key and the HTTP client (with that key's proxy) that carries its requests.
#[derive(Clone)]
pub struct KeyRoute {
    pub key: String,
    pub client: reqwest::Client,
}

/// The keys to try for one call, in order.
///
/// With "multiple API keys" on and keys saved for the model's provider, the provider's enabled
/// keys are used, starting one key later on every call. Otherwise the model's own key is used
/// through the shared provider client.
pub async fn routes(
    store: &Store,
    clients: &NetworkClients,
    model: &ModelConfig,
    timeout: Duration,
) -> Result<Vec<KeyRoute>> {
    let settings = store.api_key_settings().await?;
    let keys = settings.enabled_keys(&model.base_url);
    if keys.is_empty() {
        return Ok(vec![KeyRoute {
            key: model.api_key.clone(),
            client: clients.provider_client(timeout).await?,
        }]);
    }
    let pool = provider_host(&model.base_url).unwrap_or_default();
    let start = next_start(&pool, keys.len());
    let mut routes = Vec::with_capacity(keys.len());
    for key in keys.iter().cycle().skip(start).take(keys.len()) {
        routes.push(KeyRoute {
            key: key.key.clone(),
            client: clients.key_client(&key.proxy, timeout).await?,
        });
    }
    Ok(routes)
}

/// Round-robin start index for `pool`, advancing by one on every call.
fn next_start(pool: &str, count: usize) -> usize {
    if count <= 1 {
        return 0;
    }
    let mut next = NEXT_KEY.lock();
    let slot = next.entry(pool.to_owned()).or_default();
    let start = *slot % count;
    *slot = start + 1;
    start
}

/// Whether the status means this key cannot serve the request, so the next key should be tried.
pub fn rejected(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 402 | 403 | 429) || status.is_server_error()
}

/// Whether the request never reached the provider, e.g. the key's proxy is unreachable.
pub fn unreachable(error: &reqwest::Error) -> bool {
    error.is_connect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_the_starting_key_for_each_call() {
        let pool = "rotates_the_starting_key_for_each_call";
        assert_eq!(
            (0..4).map(|_| next_start(pool, 3)).collect::<Vec<_>>(),
            [0, 1, 2, 0]
        );
    }

    #[test]
    fn single_key_is_never_rotated() {
        let pool = "single_key_is_never_rotated";
        assert_eq!(next_start(pool, 1), 0);
        assert_eq!(next_start(pool, 1), 0);
    }

    #[test]
    fn rejects_auth_quota_and_server_failures_only() {
        for status in [401, 402, 403, 429, 500, 503] {
            assert!(rejected(reqwest::StatusCode::from_u16(status).unwrap()));
        }
        for status in [200, 400, 404, 422] {
            assert!(!rejected(reqwest::StatusCode::from_u16(status).unwrap()));
        }
    }
}
