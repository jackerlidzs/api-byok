//! Picks the API keys a model call may use, in the order to try them, and remembers how each
//! key fared.
//!
//! Each failed answer is sorted into an [`Outcome`] from its status, body and retry headers,
//! because providers report the same thing differently: OpenAI reports an empty balance as 429,
//! Anthropic its own spend limit as 400 and relays as 403. Keys that are out of credit or quota,
//! rate-limited or not accepted rest and are skipped. Keys whose provider or proxy failed for a
//! passing reason are tried only after the other keys. When every key rests, the one that
//! recovers first is still tried, so a key topped up without saving the settings is noticed.
use std::{
    collections::HashMap,
    sync::LazyLock,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use reqwest::{header::HeaderMap, StatusCode};

use crate::{
    model::ModelConfig,
    network::NetworkClients,
    store::{provider_host, KeyOrder, Store},
    Result,
};

/// Rest of a key that is out of credit or quota and gave no retry time, by refusals in a row.
const EXHAUSTED_RESTS: [Duration; 4] = [
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
    Duration::from_secs(30 * 60),
    Duration::from_secs(60 * 60),
];
/// Rest of a key the provider does not accept.
const INVALID_KEY_REST: Duration = Duration::from_secs(30 * 60);
/// First rest of a rate-limited key that gave no retry time; it doubles up to the maximum.
const RATE_LIMIT_REST: Duration = Duration::from_secs(1);
const RATE_LIMIT_MAX_REST: Duration = Duration::from_secs(5 * 60);
/// How long a key whose provider or proxy failed is first tried after the others; it doubles up
/// to the maximum.
const UNAVAILABLE_REST: Duration = Duration::from_secs(60);
const UNAVAILABLE_MAX_REST: Duration = Duration::from_secs(10 * 60);
/// Longest retry time taken from a provider.
const LONGEST_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Body text of a used-up balance, credit, spend limit or quota window, as providers document
/// it. Matched against the lowercased body.
const EXHAUSTED: &[&str] = &[
    "insufficient_quota",             // OpenAI, wiro
    "billing_error",                  // Anthropic, wiro
    "credit balance",                 // Anthropic 400 "Your credit balance is too low"
    "credit_balance",                 // OpenAI credit_balance_exhausted
    "credits are depleted",           // Gemini Prepay
    "insufficient credits",           // OpenRouter
    "insufficient balance",           // DeepSeek
    "spend_limit",                    // OpenAI *_spend_limit_exceeded, Anthropic spend cap
    "usage_limit_exceeded",           // OpenAI organization_usage_limit_exceeded
    "api usage limits",               // Anthropic spend limits
    "budget limit exceeded",          // OpenRouter budgets
    "insufficient_user_quota",        // new-api relays
    "pre_consume_token_quota_failed", // new-api relays
    "arrearage",                      // Alibaba Model Studio
    "欠费",                           // Zhipu GLM 1113, Alibaba Model Studio
    "余额不足",
    "额度不足",     // new-api relays
    "使用上限",     // Zhipu GLM usage caps
    "调用次数限额", // Zhipu GLM daily limit
    "套餐已到期",   // Zhipu GLM plan expired
];
/// Short throttles that some providers word like a used-up quota.
const THROTTLED: &[&str] = &[
    "throttling",               // Alibaba Model Studio Throttling.*
    "allocated quota exceeded", // Alibaba Model Studio 429 insufficient_quota
    "perminute",                // Gemini per-minute quotaId
];
/// A 400 that means the key itself is not valid (Gemini).
const INVALID_KEY: &[&str] = &["api_key_invalid", "api key not valid", "invalid_api_key"];
/// A 403 about the request's content rather than the key (moderation, guardrails).
const BLOCKED_REQUEST: &[&str] = &[
    "moderation",
    "flagged",
    "guardrail",
    "request blocked",
    "content_policy",
];

/// Key-list state per provider host. In memory only: a restart starts every list fresh.
static POOLS: LazyLock<Mutex<HashMap<String, Pool>>> = LazyLock::new(Default::default);

/// One API key and the HTTP client (with that key's proxy) that carries its requests.
#[derive(Clone)]
pub struct KeyRoute {
    pub key: String,
    pub client: reqwest::Client,
    /// Provider host whose key list the key comes from; `None` for a model's own key.
    pub pool: Option<String>,
    /// Whether the call starts with this key; later keys are tried after it fails.
    pub first: bool,
}

/// The keys to try for one call, in order.
///
/// With "multiple API keys" on and keys saved for the model's provider, the provider's enabled
/// keys are used in the configured order, skipping keys that rest. Otherwise the model's own key
/// is used through the shared provider client.
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
            first: true,
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
    for (position, index) in order.into_iter().enumerate() {
        routes.push(KeyRoute {
            key: keys[index].key.clone(),
            client: clients.key_client(&keys[index].proxy, timeout).await?,
            pool: Some(pool.clone()),
            first: position == 0,
        });
    }
    Ok(routes)
}

/// What a provider's answer says about the key that sent the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The key served the request.
    Served,
    /// Out of credit, or a spend limit or longer quota window is used up; with the provider's
    /// retry time, if it gave one.
    Exhausted(Option<Duration>),
    /// A short rate or concurrency limit; with the provider's retry time, if it gave one.
    RateLimited(Option<Duration>),
    /// The provider does not accept the key: invalid, revoked, without permission, or blocked in
    /// the region its proxy is in.
    InvalidKey,
    /// The provider, or the key's proxy, failed for a passing reason.
    Unavailable,
    /// The request itself was refused, so any other key would get the same answer.
    RequestError,
}

impl Outcome {
    /// Whether the same request should move on to the next key.
    pub fn fails_over(self) -> bool {
        !matches!(self, Self::Served | Self::RequestError)
    }

    /// How long the provider asked to wait.
    pub fn retry_after(self) -> Option<Duration> {
        match self {
            Self::Exhausted(retry_after) | Self::RateLimited(retry_after) => retry_after,
            _ => None,
        }
    }
}

/// Sorts a provider's answer by what it says about the key.
pub fn classify(status: StatusCode, headers: &HeaderMap, body: &[u8]) -> Outcome {
    if status.is_success() {
        return Outcome::Served;
    }
    let text = String::from_utf8_lossy(body).to_lowercase();
    let mentions = |markers: &[&str]| markers.iter().any(|marker| text.contains(marker));
    let retry_after = || retry_after(headers, &text);
    match status.as_u16() {
        401 => Outcome::InvalidKey,
        402 => Outcome::Exhausted(retry_after()),
        // A daily quota outlasts the per-minute limit it may be reported with.
        429 if text.contains("perday") => Outcome::Exhausted(retry_after()),
        429 if mentions(THROTTLED) => Outcome::RateLimited(retry_after()),
        429 if mentions(EXHAUSTED) => Outcome::Exhausted(retry_after()),
        429 => Outcome::RateLimited(retry_after()),
        400 | 403 if mentions(EXHAUSTED) => Outcome::Exhausted(retry_after()),
        403 if mentions(BLOCKED_REQUEST) => Outcome::RequestError,
        400 if mentions(INVALID_KEY) => Outcome::InvalidKey,
        403 => Outcome::InvalidKey,
        408 | 500..=599 => Outcome::Unavailable,
        _ => Outcome::RequestError,
    }
}

/// How long the provider asks to wait: `retry-after-ms`, `retry-after` in seconds, or Gemini's
/// `RetryInfo.retryDelay` (such as "34s") in the lowercased body.
fn retry_after(headers: &HeaderMap, body: &str) -> Option<Duration> {
    let header = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<f64>().ok();
    let seconds = header("retry-after-ms")
        .map(|milliseconds| milliseconds / 1000.0)
        .or_else(|| header("retry-after"))
        .or_else(|| retry_delay(body))
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)?;
    Some(Duration::from_secs_f64(
        seconds.min(LONGEST_RETRY_AFTER.as_secs_f64()),
    ))
}

fn retry_delay(body: &str) -> Option<f64> {
    const FIELD: &str = "\"retrydelay\"";
    let rest = &body[body.find(FIELD)? + FIELD.len()..];
    let value = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    value.split_once('s')?.0.parse().ok()
}

/// Remembers how the provider answered one key.
pub fn record(route: &KeyRoute, outcome: Outcome) {
    let Some(pool) = &route.pool else {
        return;
    };
    let mut pools = POOLS.lock();
    let state = pools.entry(pool.clone()).or_default();
    state.record(&route.key, route.first, outcome, Instant::now());
}

/// Forgets how every key fared, so edited or topped-up keys are used again at once.
pub fn reset() {
    POOLS.lock().clear();
}

/// Whether the status alone says the provider refused this key. Used to report a key test;
/// calls use [`classify`], which also reads the body.
pub fn rejected(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 402 | 403 | 429) || status.is_server_error()
}

/// Whether the request never reached the provider, e.g. the key's proxy is unreachable.
pub fn unreachable(error: &reqwest::Error) -> bool {
    error.is_connect()
}

#[derive(Default)]
struct Pool {
    /// Round robin: the key handed out last; the next call starts at the ready key after it.
    last: Option<String>,
    /// In order: the key that last served a call, kept while it is only busy or failing for a
    /// moment.
    current: Option<String>,
    /// Keys that failed since they last served a call.
    failures: HashMap<String, Failure>,
}

struct Failure {
    kind: FailureKind,
    /// Failures of this kind in a row; each one rests the key longer.
    strikes: u32,
    until: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    Exhausted,
    RateLimited,
    InvalidKey,
    Unavailable,
}

impl FailureKind {
    fn rest(self, strikes: u32) -> Duration {
        let step = strikes.saturating_sub(1).min(20);
        let doubling = |first: Duration, max: Duration| first.saturating_mul(1 << step).min(max);
        match self {
            Self::Exhausted => EXHAUSTED_RESTS[(step as usize).min(EXHAUSTED_RESTS.len() - 1)],
            Self::InvalidKey => INVALID_KEY_REST,
            Self::RateLimited => doubling(RATE_LIMIT_REST, RATE_LIMIT_MAX_REST),
            Self::Unavailable => doubling(UNAVAILABLE_REST, UNAVAILABLE_MAX_REST),
        }
    }
}

impl Pool {
    /// Indexes into `keys` in the order to try them: the keys ready to serve, starting where the
    /// key order says, then the keys whose provider or proxy failed lately. When every key
    /// rests, only the one that recovers first is tried.
    fn order(&mut self, keys: &[&str], order: KeyOrder, now: Instant) -> Vec<usize> {
        let count = keys.len();
        if count == 0 {
            return Vec::new();
        }
        let anchor = match order {
            KeyOrder::RoundRobin => self.last.as_deref(),
            KeyOrder::InOrder => self.current.as_deref(),
        }
        .and_then(|anchor| keys.iter().position(|key| *key == anchor));
        let start = match (order, anchor) {
            (KeyOrder::RoundRobin, Some(index)) => index + 1,
            (KeyOrder::InOrder, Some(index)) => index,
            (_, None) => 0,
        };
        let resting = |index: usize| {
            self.failures
                .get(keys[index])
                .filter(|failure| failure.until > now)
        };
        let cycle = (0..count).map(|offset| (start + offset) % count);
        let mut tried = cycle
            .clone()
            .filter(|&index| resting(index).is_none())
            .collect::<Vec<_>>();
        tried.extend(cycle.filter(|&index| {
            resting(index).is_some_and(|failure| failure.kind == FailureKind::Unavailable)
        }));
        if tried.is_empty() {
            tried.extend(
                (0..count).min_by_key(|&index| resting(index).map(|failure| failure.until)),
            );
        }
        if order == KeyOrder::RoundRobin {
            self.last = Some(keys[tried[0]].to_owned());
        }
        tried
    }

    fn record(&mut self, key: &str, first: bool, outcome: Outcome, now: Instant) {
        let (kind, retry_after) = match outcome {
            Outcome::Served => {
                // In order, a key that is only busy or failing for a moment keeps its place and
                // takes calls back once it recovers; a key out of credit gives its place up.
                let keeps_place = self.current.as_deref().is_some_and(|current| {
                    current != key
                        && self.failures.get(current).is_some_and(|failure| {
                            matches!(
                                failure.kind,
                                FailureKind::RateLimited | FailureKind::Unavailable
                            )
                        })
                });
                if !keeps_place {
                    self.current = Some(key.to_owned());
                }
                self.failures.remove(key);
                if !first {
                    // The call moved on to this key, so the next call starts after it.
                    self.last = Some(key.to_owned());
                }
                return;
            }
            Outcome::RequestError => return,
            Outcome::Exhausted(retry_after) => (FailureKind::Exhausted, retry_after),
            Outcome::RateLimited(retry_after) => (FailureKind::RateLimited, retry_after),
            Outcome::InvalidKey => (FailureKind::InvalidKey, None),
            Outcome::Unavailable => (FailureKind::Unavailable, None),
        };
        let strikes = match self.failures.get(key) {
            Some(failure) if failure.kind == kind => failure.strikes.saturating_add(1),
            _ => 1,
        };
        let rest = retry_after.unwrap_or_else(|| kind.rest(strikes));
        self.failures.insert(
            key.to_owned(),
            Failure {
                kind,
                strikes,
                until: now + rest,
            },
        );
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

    /// Serves one call: the keys tried in order, each answering with the next outcome, until one
    /// is served or the outcomes run out. Returns the keys tried.
    fn call(
        pool: &mut Pool,
        order: KeyOrder,
        now: Instant,
        outcomes: &[(&str, Outcome)],
    ) -> Vec<&'static str> {
        let keys = tried(pool, order, now);
        for (position, key) in keys.iter().enumerate() {
            let outcome = outcomes
                .iter()
                .find(|(name, _)| name == key)
                .map_or(Outcome::Served, |(_, outcome)| *outcome);
            pool.record(key, position == 0, outcome, now);
            if !outcome.fails_over() {
                return keys[..=position].to_vec();
            }
        }
        keys
    }

    fn status(code: u16) -> StatusCode {
        StatusCode::from_u16(code).unwrap()
    }

    fn classified(code: u16, body: &str) -> Outcome {
        classify(status(code), &HeaderMap::new(), body.as_bytes())
    }

    #[test]
    fn round_robin_spreads_calls_over_the_ready_keys() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        let starts = (0..4)
            .map(|_| call(&mut pool, KeyOrder::RoundRobin, now, &[])[0])
            .collect::<Vec<_>>();
        assert_eq!(starts, ["k1", "k2", "k3", "k1"]);
    }

    #[test]
    fn round_robin_continues_after_the_key_a_call_moved_on_to() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        let broke = [("k1", Outcome::Exhausted(None))];
        assert_eq!(
            call(&mut pool, KeyOrder::RoundRobin, now, &broke),
            ["k1", "k2"]
        );
        let served = (0..4)
            .map(|_| call(&mut pool, KeyOrder::RoundRobin, now, &broke))
            .collect::<Vec<_>>();
        assert_eq!(served, [["k3"], ["k2"], ["k3"], ["k2"]]);
    }

    #[test]
    fn in_order_stays_on_the_key_that_last_worked() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        assert_eq!(call(&mut pool, KeyOrder::InOrder, now, &[]), ["k1"]);
        assert_eq!(call(&mut pool, KeyOrder::InOrder, now, &[]), ["k1"]);
        let broke = [("k1", Outcome::Exhausted(None))];
        assert_eq!(
            call(&mut pool, KeyOrder::InOrder, now, &broke),
            ["k1", "k2"]
        );
        for _ in 0..3 {
            assert_eq!(call(&mut pool, KeyOrder::InOrder, now, &broke), ["k2"]);
        }
        // A rate-limited key hands calls over until its rest ends, then takes them back.
        let busy = [("k2", Outcome::RateLimited(Some(Duration::from_secs(5))))];
        assert_eq!(call(&mut pool, KeyOrder::InOrder, now, &busy), ["k2", "k3"]);
        assert_eq!(call(&mut pool, KeyOrder::InOrder, now, &[]), ["k3"]);
        let later = now + Duration::from_secs(6);
        assert_eq!(call(&mut pool, KeyOrder::InOrder, later, &[]), ["k2"]);
    }

    #[test]
    fn failing_keys_are_tried_after_the_ready_keys() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        pool.record("k1", true, Outcome::Unavailable, now);
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, now), ["k2", "k3", "k1"]);
        // Once the others are out of credit, the failing key is used again.
        pool.record("k2", true, Outcome::Exhausted(None), now);
        pool.record("k3", false, Outcome::InvalidKey, now);
        assert_eq!(tried(&mut pool, KeyOrder::RoundRobin, now), ["k1"]);
    }

    #[test]
    fn when_every_key_rests_the_one_that_recovers_first_is_tried() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        pool.record("k1", true, Outcome::Exhausted(None), now);
        let soon = Outcome::RateLimited(Some(Duration::from_secs(10)));
        pool.record("k2", true, soon, now);
        pool.record("k3", true, Outcome::InvalidKey, now);
        for order in [KeyOrder::RoundRobin, KeyOrder::InOrder] {
            assert_eq!(tried(&mut pool, order, now), ["k2"]);
        }
        let later = now + Duration::from_secs(11);
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, later), ["k2"]);
    }

    #[test]
    fn rests_grow_with_each_failure_and_follow_the_provider_retry_time() {
        let minutes = |minutes: u64| Duration::from_secs(minutes * 60);
        let rests = |kind: FailureKind| {
            (1..=6)
                .map(|strikes| kind.rest(strikes))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            rests(FailureKind::Exhausted),
            [5, 15, 30, 60, 60, 60].map(minutes)
        );
        assert_eq!(
            rests(FailureKind::RateLimited),
            [1, 2, 4, 8, 16, 32].map(Duration::from_secs)
        );
        assert_eq!(FailureKind::RateLimited.rest(30), RATE_LIMIT_MAX_REST);
        assert_eq!(
            rests(FailureKind::Unavailable),
            [1, 2, 4, 8, 10, 10].map(minutes)
        );
        assert_eq!(FailureKind::InvalidKey.rest(9), minutes(30));

        let (mut pool, now) = (Pool::default(), Instant::now());
        pool.record("k1", true, Outcome::Exhausted(None), now);
        pool.record("k1", true, Outcome::Exhausted(None), now);
        assert_eq!(pool.failures["k1"].until, now + minutes(15));
        // Another kind of failure starts over.
        pool.record("k1", true, Outcome::RateLimited(None), now);
        assert_eq!(pool.failures["k1"].until, now + Duration::from_secs(1));
        let hint = Outcome::Exhausted(Some(Duration::from_secs(90)));
        pool.record("k1", true, hint, now);
        assert_eq!(pool.failures["k1"].until, now + Duration::from_secs(90));
    }

    #[test]
    fn serving_clears_a_failure_and_request_errors_change_nothing() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        pool.record("k1", true, Outcome::Exhausted(None), now);
        pool.record("k1", true, Outcome::RequestError, now);
        assert!(pool.failures.contains_key("k1"));
        pool.record("k1", true, Outcome::Served, now);
        assert!(pool.failures.is_empty());
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, now), ["k1", "k2", "k3"]);
    }

    #[test]
    fn a_single_or_removed_key_is_still_tried() {
        let (mut pool, now) = (Pool::default(), Instant::now());
        pool.record("only", true, Outcome::Exhausted(None), now);
        for order in [KeyOrder::RoundRobin, KeyOrder::InOrder] {
            assert_eq!(pool.order(&["only"], order, now), [0]);
        }
        pool.record("gone", true, Outcome::Served, now);
        assert_eq!(tried(&mut pool, KeyOrder::InOrder, now), ["k1", "k2", "k3"]);
    }

    #[test]
    fn out_of_credit_and_quota_answers_are_exhausted() {
        for (code, body) in [
            // wiro
            (
                402,
                r#"{"type":"error","error":{"type":"billing_error","message":"The project has insufficient quota for this request."}}"#,
            ),
            // OpenAI reports an empty balance and spend limits as 429.
            (
                429,
                r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#,
            ),
            (
                429,
                r#"{"error":{"message":"Your organization has no prepaid credits remaining.","type":"insufficient_quota","code":"credit_balance_exhausted"}}"#,
            ),
            (
                429,
                r#"{"error":{"message":"Project spend limit reached.","type":"insufficient_quota","code":"project_spend_limit_exceeded"}}"#,
            ),
            // Anthropic: credit and own spend limit as 400, the tier spend cap as 429.
            (
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."}}"#,
            ),
            (
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"You have reached your specified API usage limits. You will regain access on 2026-12-01 at 00:00 UTC."}}"#,
            ),
            (
                429,
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"You have reached your API usage limits.","details":{"error_code":"enforced_spend_limit_reached"}}}"#,
            ),
            // Gemini Prepay and daily quota
            (
                402,
                r#"{"error":{"code":402,"message":"Your prepayment credits are depleted.","status":"FAILED_PRECONDITION"}}"#,
            ),
            (
                429,
                r#"{"error":{"code":429,"message":"Your prepayment credits are depleted.","status":"RESOURCE_EXHAUSTED"}}"#,
            ),
            // DeepSeek, OpenRouter
            (
                402,
                r#"{"error":{"message":"Insufficient Balance","type":"unknown_error"}}"#,
            ),
            (
                403,
                r#"{"error":{"code":403,"message":"Budget limit exceeded (weekly limit). $52.00 of $50.00 used; resets 2026-10-12T00:00:00.000Z.","metadata":{"limit_source":"openrouter_guardrail_budget"}}}"#,
            ),
            // Relays and Chinese providers
            (
                403,
                r#"{"error":{"message":"用户额度不足, 剩余额度: ＄0.000000","type":"new_api_error","code":"insufficient_user_quota"}}"#,
            ),
            (
                403,
                r#"{"error":{"message":"token quota is not enough","type":"new_api_error","code":"pre_consume_token_quota_failed"}}"#,
            ),
            (
                403,
                r#"{"code":"Arrearage","message":"Access denied, please make sure your account is in good standing.","type":"Arrearage"}"#,
            ),
            (
                429,
                r#"{"error":{"code":"1113","message":"您的账户已欠费，请充值后重试"}}"#,
            ),
            (
                429,
                r#"{"error":{"code":"1310","message":"您已达到每周/每月使用上限，您的限额将在 2026-11-17 00:00:00 重置"}}"#,
            ),
        ] {
            assert!(
                matches!(classified(code, body), Outcome::Exhausted(_)),
                "{code} {body}"
            );
        }
    }

    #[test]
    fn short_limits_are_rate_limits() {
        for (code, body) in [
            // wiro's concurrency limit mentions the balance but is not a used-up balance.
            (
                429,
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"You have reached your concurrent task limit. With your current balance of $4.27, you can run up to 1 tasks at the same time. Add funds to increase your limit."}}"#,
            ),
            (
                429,
                r#"{"error":{"message":"Rate limit reached for gpt-x in organization org-1 on requests per min (RPM): Limit 3, Used 3, Requested 1.","type":"requests","code":"rate_limit_exceeded"}}"#,
            ),
            (
                429,
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"}}"#,
            ),
            // Alibaba words a throttle like OpenAI's empty balance.
            (
                429,
                r#"{"error":{"message":"Allocated quota exceeded, please increase your quota limit.","type":"Throttling.AllocationQuota","code":"insufficient_quota"}}"#,
            ),
            (
                429,
                r#"{"error":{"code":"1302","message":"您的账户已达到速率限制，请您控制请求频率"}}"#,
            ),
            (429, ""),
        ] {
            assert!(
                matches!(classified(code, body), Outcome::RateLimited(_)),
                "{code} {body}"
            );
        }
    }

    #[test]
    fn gemini_quota_windows_follow_the_quota_id_and_retry_delay() {
        let quota = |quota_id: &str, delay: &str| {
            format!(
                r#"{{"error":{{"code":429,"message":"You exceeded your current quota, please check your plan and billing details.","status":"RESOURCE_EXHAUSTED","details":[{{"@type":"type.googleapis.com/google.rpc.QuotaFailure","violations":[{{"quotaId":"{quota_id}"}}]}},{{"@type":"type.googleapis.com/google.rpc.RetryInfo","retryDelay":"{delay}"}}]}}}}"#
            )
        };
        assert_eq!(
            classified(
                429,
                &quota("GenerateRequestsPerDayPerProjectPerModel-FreeTier", "9663s")
            ),
            Outcome::Exhausted(Some(Duration::from_secs(9663)))
        );
        assert_eq!(
            classified(
                429,
                &quota("GenerateRequestsPerMinutePerProjectPerModel", "34.5s")
            ),
            Outcome::RateLimited(Some(Duration::from_millis(34_500)))
        );
    }

    #[test]
    fn retry_headers_set_the_rest() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "7".parse().unwrap());
        assert_eq!(
            classify(status(429), &headers, b"{}"),
            Outcome::RateLimited(Some(Duration::from_secs(7)))
        );
        headers.insert("retry-after-ms", "1500".parse().unwrap());
        assert_eq!(
            classify(status(429), &headers, b"{}").retry_after(),
            Some(Duration::from_millis(1500))
        );
        let mut huge = HeaderMap::new();
        huge.insert("retry-after", "1e300".parse().unwrap());
        assert_eq!(
            classify(status(402), &huge, b"").retry_after(),
            Some(LONGEST_RETRY_AFTER)
        );
        let mut date = HeaderMap::new();
        date.insert(
            "retry-after",
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(classify(status(429), &date, b"").retry_after(), None);
    }

    #[test]
    fn keys_the_provider_does_not_accept_are_invalid() {
        for (code, body) in [
            (
                401,
                r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            ),
            (
                400,
                r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"reason":"API_KEY_INVALID"}]}}"#,
            ),
            (
                403,
                r#"{"type":"error","error":{"type":"permission_error","message":"Your API key does not have permission to use the specified resource."}}"#,
            ),
            (
                403,
                r#"{"error":{"code":"unsupported_country_region_territory","message":"Country, region, or territory not supported"}}"#,
            ),
            (403, "<html>Forbidden</html>"),
        ] {
            assert_eq!(classified(code, body), Outcome::InvalidKey, "{code} {body}");
        }
    }

    #[test]
    fn passing_failures_are_unavailable_and_bad_requests_do_not_fail_over() {
        for code in [408, 500, 502, 503, 504, 520, 529] {
            assert_eq!(classified(code, "{}"), Outcome::Unavailable, "{code}");
        }
        for (code, body) in [
            (
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#,
            ),
            (
                404,
                r#"{"type":"error","error":{"type":"not_found_error","message":"model: claude-x"}}"#,
            ),
            (
                413,
                r#"{"type":"error","error":{"type":"request_too_large","message":"Request exceeds the maximum size"}}"#,
            ),
            (422, r#"{"error":{"message":"Invalid Parameters"}}"#),
            (
                403,
                r#"{"error":{"code":403,"message":"Input was flagged","metadata":{"reasons":["violence"]}}}"#,
            ),
            (
                403,
                r#"{"error":{"code":403,"message":"Request blocked: prompt injection patterns detected"}}"#,
            ),
        ] {
            let outcome = classified(code, body);
            assert_eq!(outcome, Outcome::RequestError, "{code} {body}");
            assert!(!outcome.fails_over());
        }
        assert_eq!(classified(200, ""), Outcome::Served);
        assert!(!Outcome::Served.fails_over());
    }

    #[test]
    fn a_key_test_reports_the_statuses_that_refuse_a_key() {
        for code in [401, 402, 403, 429, 500, 503] {
            assert!(rejected(status(code)));
        }
        for code in [200, 400, 404, 422] {
            assert!(!rejected(status(code)));
        }
    }
}
