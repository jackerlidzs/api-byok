//! Persists the "multiple API keys per provider" setting and each provider's key list.
use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

use super::{now_ms, Store};

const API_KEY_SETTINGS_KEY: &str = "provider_api_keys";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct ApiKeySettings {
    /// Off by default; when off every model uses only its own API key and the global proxy.
    #[serde(default)]
    pub enabled: bool,
    /// Keys per provider host (lowercase `base_url` host), in order Key 1, Key 2, ...
    #[serde(default)]
    pub providers: BTreeMap<String, Vec<ProviderApiKey>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ProviderApiKey {
    pub key: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub proxy: KeyProxy,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Hash, Serialize)]
pub struct KeyProxy {
    #[serde(default)]
    pub mode: KeyProxyMode,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub auth_enabled: bool,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyProxyMode {
    /// Use System settings -> Proxy settings.
    #[default]
    Inherit,
    /// Use this key's own proxy.
    Custom,
    /// Connect directly, ignoring every proxy.
    Direct,
}

fn enabled_by_default() -> bool {
    true
}

impl ApiKeySettings {
    /// The enabled keys of the provider serving `base_url`, or none when the feature is off.
    pub fn enabled_keys(&self, base_url: &str) -> Vec<&ProviderApiKey> {
        if !self.enabled {
            return Vec::new();
        }
        provider_host(base_url)
            .and_then(|host| self.providers.get(&host))
            .map(|keys| keys.iter().filter(|key| key.enabled).collect())
            .unwrap_or_default()
    }
}

/// The provider a model belongs to: the lowercase host of its `base_url`.
pub fn provider_host(base_url: &str) -> Option<String> {
    url::Url::parse(base_url.trim())
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

impl Store {
    pub async fn api_key_settings(&self) -> Result<ApiKeySettings> {
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM service_settings WHERE setting_key = ?",
        )
        .bind(API_KEY_SETTINGS_KEY)
        .fetch_optional(&self.pool)
        .await?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .unwrap_or_else(|| Ok(ApiKeySettings::default()))
    }

    pub async fn set_api_key_settings(&self, settings: ApiKeySettings) -> Result<ApiKeySettings> {
        let settings = normalize_api_key_settings(settings)?;
        let value_json = serde_json::to_string(&settings)?;
        let _write = self.writes.lock().await;
        sqlx::query("INSERT INTO service_settings(setting_key, value_json, updated_at_ms) VALUES (?, ?, ?) ON CONFLICT(setting_key) DO UPDATE SET value_json = excluded.value_json, updated_at_ms = excluded.updated_at_ms")
            .bind(API_KEY_SETTINGS_KEY)
            .bind(value_json)
            .bind(now_ms())
            .execute(&self.pool)
            .await?;
        Ok(settings)
    }
}

/// Trims keys and hosts, rejects empty or duplicate keys, and normalizes each key's proxy.
pub(crate) fn normalize_api_key_settings(settings: ApiKeySettings) -> Result<ApiKeySettings> {
    let mut providers = BTreeMap::new();
    for (host, keys) in settings.providers {
        let host = host.trim().to_ascii_lowercase();
        if host.is_empty() || keys.is_empty() {
            continue;
        }
        let mut seen = HashSet::new();
        let mut normalized = Vec::with_capacity(keys.len());
        for (index, key) in keys.into_iter().enumerate() {
            let label = format!("{host} Key {}", index + 1);
            let value = key.key.trim().to_owned();
            if value.is_empty() {
                return Err(Error::Config(format!("{label}: API key cannot be empty")));
            }
            if !seen.insert(value.clone()) {
                return Err(Error::Config(format!("{label}: duplicate API key")));
            }
            normalized.push(ProviderApiKey {
                key: value,
                enabled: key.enabled,
                proxy: normalize_proxy(key.proxy, &label)?,
            });
        }
        providers.insert(host, normalized);
    }
    Ok(ApiKeySettings {
        enabled: settings.enabled,
        providers,
    })
}

/// Validates one key's proxy and drops the fields its mode does not use. The address may be
/// written in any format `proxy_address` reads; credentials found in it move to the auth fields.
pub(crate) fn normalize_proxy(proxy: KeyProxy, label: &str) -> Result<KeyProxy> {
    if proxy.mode != KeyProxyMode::Custom {
        return Ok(KeyProxy {
            mode: proxy.mode,
            ..KeyProxy::default()
        });
    }
    let parsed = crate::proxy_address::parse(&proxy.address).map_err(|error| match error {
        Error::Config(message) => Error::Config(format!("{label}: {message}")),
        error => Error::Config(format!("{label}: {error}")),
    })?;
    let (auth_enabled, username, password) = match parsed.credentials {
        Some(credentials) => (true, credentials.username, credentials.password),
        None => (
            proxy.auth_enabled,
            proxy.username.trim().to_owned(),
            proxy.password,
        ),
    };
    if auth_enabled && username.is_empty() {
        return Err(Error::Config(format!(
            "{label}: proxy username cannot be empty"
        )));
    }
    Ok(KeyProxy {
        mode: KeyProxyMode::Custom,
        address: parsed.address,
        auth_enabled,
        username: if auth_enabled {
            username
        } else {
            String::new()
        },
        password: if auth_enabled {
            password
        } else {
            String::new()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str, proxy: KeyProxy) -> ProviderApiKey {
        ProviderApiKey {
            key: value.into(),
            enabled: true,
            proxy,
        }
    }

    fn custom(address: &str) -> KeyProxy {
        KeyProxy {
            mode: KeyProxyMode::Custom,
            address: address.into(),
            ..KeyProxy::default()
        }
    }

    async fn store() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("test.db").display()
        ))
        .await
        .unwrap();
        (directory, store)
    }

    #[tokio::test]
    async fn is_off_and_empty_by_default() {
        let (_directory, store) = store().await;
        assert_eq!(
            store.api_key_settings().await.unwrap(),
            ApiKeySettings::default()
        );
    }

    #[tokio::test]
    async fn round_trips_normalized_keys_per_provider() {
        let (_directory, store) = store().await;
        let mut stale = custom("http://ignored:1");
        stale.mode = KeyProxyMode::Direct;
        let saved = store
            .set_api_key_settings(ApiKeySettings {
                enabled: true,
                providers: BTreeMap::from([
                    (
                        " AgentRouter.org ".into(),
                        vec![
                            key(" k1 ", KeyProxy::default()),
                            key("k2", custom(" socks5://127.0.0.1:1080 ")),
                            key("k3", stale),
                        ],
                    ),
                    ("empty.example".into(), Vec::new()),
                ]),
            })
            .await
            .unwrap();
        let keys = &saved.providers["agentrouter.org"];
        assert_eq!(saved.providers.len(), 1);
        assert_eq!(keys[0].key, "k1");
        assert_eq!(keys[1].proxy.address, "socks5://127.0.0.1:1080");
        assert_eq!(
            keys[2].proxy,
            KeyProxy {
                mode: KeyProxyMode::Direct,
                ..KeyProxy::default()
            }
        );
        assert_eq!(store.api_key_settings().await.unwrap(), saved);
    }

    #[tokio::test]
    async fn rejects_empty_duplicate_keys_and_bad_proxies() {
        let (_directory, store) = store().await;
        for keys in [
            vec![key(" ", KeyProxy::default())],
            vec![
                key("same", KeyProxy::default()),
                key(" same ", KeyProxy::default()),
            ],
            vec![key("k", custom("ftp://proxy:21"))],
            vec![key(
                "k",
                KeyProxy {
                    auth_enabled: true,
                    ..custom("http://proxy:8080")
                },
            )],
        ] {
            let result = store
                .set_api_key_settings(ApiKeySettings {
                    enabled: true,
                    providers: BTreeMap::from([("example.com".into(), keys)]),
                })
                .await;
            assert!(matches!(result, Err(Error::Config(_))));
        }
    }

    #[test]
    fn a_pasted_proxy_with_credentials_fills_the_auth_fields() {
        let proxy = normalize_proxy(custom("gate.example.net:8080 alice s3cret"), "Key 1").unwrap();
        assert_eq!(proxy.address, "http://gate.example.net:8080");
        assert!(proxy.auth_enabled);
        assert_eq!(proxy.username, "alice");
        assert_eq!(proxy.password, "s3cret");

        let Err(Error::Config(message)) = normalize_proxy(custom("gate.example.net"), "Key 2")
        else {
            panic!("an address without a port must be rejected");
        };
        assert!(
            message.starts_with("Key 2: invalid proxy address: "),
            "{message}"
        );
    }

    #[test]
    fn enabled_keys_follow_the_switch_host_and_key_state() {
        let mut disabled = key("off", KeyProxy::default());
        disabled.enabled = false;
        let mut settings = ApiKeySettings {
            enabled: false,
            providers: BTreeMap::from([(
                "api.example.com".into(),
                vec![key("on", KeyProxy::default()), disabled],
            )]),
        };
        assert!(settings
            .enabled_keys("https://api.example.com/v1")
            .is_empty());
        settings.enabled = true;
        let keys = settings.enabled_keys("https://API.example.com/v1");
        assert_eq!(
            keys.iter().map(|key| key.key.as_str()).collect::<Vec<_>>(),
            ["on"]
        );
        assert!(settings
            .enabled_keys("https://other.example.com")
            .is_empty());
    }
}
