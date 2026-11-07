//! Splits a model's API key field into several keys and rotates them across calls.
use std::{collections::HashMap, sync::LazyLock};

use parking_lot::Mutex;

static NEXT_KEY: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Splits the stored API key field on commas and whitespace.
pub fn split(raw: &str) -> Vec<String> {
    raw.split(|character: char| character == ',' || character.is_whitespace())
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Returns every key of `raw`, starting at the next key in round-robin order for `pool`.
pub fn rotated(pool: &str, raw: &str) -> Vec<String> {
    let mut keys = split(raw);
    if keys.len() > 1 {
        let mut next = NEXT_KEY.lock();
        let slot = next.entry(pool.to_owned()).or_default();
        let start = *slot % keys.len();
        *slot = start + 1;
        keys.rotate_left(start);
    }
    keys
}

/// Whether the status means this key cannot serve the request, so the next key should be tried.
pub fn rejected(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 402 | 403 | 429) || status.is_server_error()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_keys_on_commas_and_whitespace() {
        assert_eq!(split(" a, b\nc  ,,d "), ["a", "b", "c", "d"]);
        assert_eq!(split("single"), ["single"]);
        assert!(split(" , ").is_empty());
    }

    #[test]
    fn rotates_the_starting_key_for_each_call() {
        let pool = "rotates_the_starting_key_for_each_call";
        assert_eq!(rotated(pool, "a,b,c"), ["a", "b", "c"]);
        assert_eq!(rotated(pool, "a,b,c"), ["b", "c", "a"]);
        assert_eq!(rotated(pool, "a,b,c"), ["c", "a", "b"]);
        assert_eq!(rotated(pool, "a,b,c"), ["a", "b", "c"]);
    }

    #[test]
    fn single_key_is_never_rotated() {
        let pool = "single_key_is_never_rotated";
        assert_eq!(rotated(pool, "only"), ["only"]);
        assert_eq!(rotated(pool, "only"), ["only"]);
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
