//! Picks the API keys a model call may use, in the order to try them, and remembers how each
//! key fared so later calls start with a key that works.
use std::{
    collections::HashMap,
    sync::LazyLock,
    time::{Duration, Instant},
};

use parking_lot::Mutex;

use crate::{
    model::ModelConfig,
    network::NetworkClients,
    store::{provider_host, KeyOrder, Store},
    Result,
};

/// How long a key the provider refused for good (invalid key, out of credit) is tried last.
const REFUSED_KEY_REST: Duration = Duration::from_secs(10 * 60);

/// Key-list state per provider host. In memory only: a restart starts every list fresh.
static POOLS: LazyLock<Mutex<HashMap<String, Pool>>> = LazyLock::new(Default::default);

/// One API key and the HTTP client (with that key's proxy) that carries its requests.
#[derive(Clone)]
pub struct KeyRoute {
    pub key: String,
    pub client: reqwest::Client,
    /// Provider host whose key list the key comes from; `None` for a model's own key.
    pub pool: Option<String>,
}

/// The keys to try for one call, in order.
///
/// With "multiple API keys" on and keys saved for the model's provider, the provider's enabled
/// keys are used in the configured order, keys refused recently last. Otherwise the model's own
/// key is used through the shared provider client.
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
            pool: None,
        }]);
    }
    let pool = provider_host(&model.base_url).unwrap_or_default();
    let names = keys.iter().map(|key| key.key.as_str()).collect::<Vec<_>>();
    let order = {
        let mut pools = POOLS.lock();
        let state = pools.entry(pool.clone()).or_default();
        state.order(&names, settings.order, Instant::now())
    };
    let mut routes = Vec::with_capacity(order.len());
    for index in order {
        routes.push(KeyRoute {
            key: keys[index].key.clone(),
            client: clients.key_client(&keys[index].proxy, timeout).await?,
            pool: Some(pool.clone()),
        });
    }
    Ok(routes)
}

/// Remembers the provider's answer to one key: a key that served a call is where "in order"
/// calls start; a key refused with 401, 402 or 403 is tried last for a while.
pub fn record(route: &KeyRoute, status: reqwest::StatusCode) {
    let Some(pool) = &route.pool else {
        return;
    };
    let mut pools = POOLS.lock();
    let state = pools.entry(pool.clone()).or_default();
    state.record(&route.key, status, Instant::now());
}

/// Forgets how every key fared, so edited key lists start fresh.
pub fn reset() {
    POOLS.lock().clear();
}

/// Whether the status means this key cannot serve the request, so the next key should be tried.
pub fn rejected(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 402 | 403 | 429) || status.is_server_error()
}

/// Whether the request never reached the provider, e.g. the key's proxy is unreachable.
pub fn unreachable(error: &reqwest::Error) -> bool {
    error.is_connect()
}

#[derive(Default)]
struct Pool {
    /// Round robin: where the next call starts.
    next: usize,
    /// In order: the key that last served a call.
    current: Option<String>,
    /// Keys refused with 401, 402 or 403, and until when they are tried last.
    resting: HashMap<String, Instant>,
}

impl Pool {
    /// Indexes into `keys` in the order to try them.
    fn order(&mut self, keys: &[&str], order: KeyOrder, now: Instant) -> Vec<usize> {
        let count = keys.len();
        if count == 0 {
            return Vec::new();
        }
        let start = match order {
            KeyOrder::RoundRobin => {
                let start = self.next % count;
                self.next = start + 1;
                start
            }
            KeyOrder::InOrder => self
                .current
                .as_deref()
                .and_then(|current| keys.iter().position(|key| *key == current))
                .unwrap_or(0),
        };
        self.resting.retain(|_, until| *until > now);
        let (ready, resting): (Vec<_>, Vec<_>) = (0..count)
            .map(|offset| (start + offset) % count)
            .partition(|&index| !self.resting.contains_key(keys[index]));
        ready.into_iter().chain(resting).collect()
    }

    fn record(&mut self, key: &str, status: reqwest::StatusCode, now: Instant) {
        if matches!(status.as_u16(), 401..=403) {
            self.resting.insert(key.to_owned(), now + REFUSED_KEY_REST);
        } else if !rejected(status) {
            // 429 and 5xx are passing (busy, outage) and leave the key where it is.
            self.resting.remove(key);
            self.current = Some(key.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: [&str; 3] = ["k1", "k2", "k3"];

    fn tried(pool: &mut Pool, order: KeyOrder, now: Instant) -> Vec<&'static str> {
        pool.order(&KEYS, order, now)
            .into_iter()
            .map(|index| KEYS[index])
            .collect()
    }

    fn status(code: u16) -> reqwest::StatusCode {
        reqwest::StatusCode::from_u16(code).unwrap()
    }

    #[test]
    fn round_robin_starts_one_key_later_each_call() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        let starts = (0..4)
            .map(|_| tried(&mut pool, KeyOrder::RoundRobin, now)[0])
            .collect::<Vec<_>>();
        assert_eq!(starts, ["k1", "k2", "k3", "k1"]);
        assert_eq!(
            tried(&mut pool, KeyOrder::RoundRobin, now),
            ["k2", "k3", "k1"]
        );
    }

    #[test]
    fn in_order_stays_on_the_key_that_last_worked() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, now), ["k1", "k2", "k3"]);
        pool.record("k1", status(200), now);
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, now), ["k1", "k2", "k3"]);
        // Key 1 runs out of credit and the call moves on to key 2.
        pool.record("k1", status(402), now);
        pool.record("k2", status(200), now);
        for _ in 0..3 {
            assert_eq!(tried(&mut pool, KeyOrder::InOrder, now), ["k2", "k3", "k1"]);
        }
        // Busy or failing servers do not move the list off key 2.
        pool.record("k2", status(429), now);
        pool.record("k2", status(503), now);
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, now)[0], "k2");
    }

    #[test]
    fn refused_keys_are_tried_last_until_their_rest_ends() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        pool.record("k1", status(401), now);
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, now), ["k2", "k3", "k1"]);
        let mut round_robin = Pool::default();
        round_robin.record("k1", status(402), now);
        let starts = (0..3)
            .map(|_| tried(&mut round_robin, KeyOrder::RoundRobin, now)[0])
            .collect::<Vec<_>>();
        assert_eq!(starts, ["k2", "k2", "k3"]);

        let later = now + REFUSED_KEY_REST + Duration::from_secs(1);
        assert_eq!(
            tried(&mut pool, KeyOrder::InOrder, later),
            ["k1", "k2", "k3"]
        );
    }

    #[test]
    fn a_key_that_works_again_is_no_longer_resting() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        pool.record("k1", status(402), now);
        pool.record("k1", status(200), now);
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, now), ["k1", "k2", "k3"]);
        pool.record("k2", status(429), now);
        pool.record("k3", status(500), now);
        assert!(pool.resting.is_empty());
    }

    #[test]
    fn a_removed_current_key_falls_back_to_the_first_key() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        pool.record("gone", status(200), now);
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, now), ["k1", "k2", "k3"]);
    }

    #[test]
    fn single_key_is_always_tried() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        pool.record("only", status(402), now);
        for order in [KeyOrder::RoundRobin, KeyOrder::InOrder] {
            assert_eq!(pool.order(&["only"], order, now), [0]);
        }
    }

    #[test]
    fn rejects_auth_quota_and_server_failures_only() {
        for code in [401, 402, 403, 429, 500, 503] {
            assert!(rejected(status(code)));
        }
        for code in [200, 400, 404, 422] {
            assert!(!rejected(status(code)));
        }
    }
}
