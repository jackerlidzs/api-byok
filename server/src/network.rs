//! Owns reusable outbound HTTP clients configured from persisted proxy settings.

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::RwLock;

use crate::{
    store::{KeyProxy, KeyProxyMode, ProxySettingsSecret, Store},
    Error, Result,
};

const LOCAL_NO_PROXY: &str = "localhost,127.0.0.0/8,::1";

#[derive(Clone)]
pub struct NetworkClients {
    store: Store,
    cache: Arc<RwLock<ClientCache>>,
}

#[derive(Default)]
struct ClientCache {
    default: Option<reqwest::Client>,
    cursor: Option<reqwest::Client>,
    provider: Option<(Duration, reqwest::Client)>,
    /// Provider clients for API keys with their own proxy, keyed by proxy and timeout.
    keys: HashMap<(KeyProxy, Duration), reqwest::Client>,
}

impl NetworkClients {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            cache: Arc::new(RwLock::new(ClientCache::default())),
        }
    }

    pub async fn default_client(&self) -> Result<reqwest::Client> {
        if let Some(client) = self.cache.read().await.default.clone() {
            return Ok(client);
        }
        let mut cache = self.cache.write().await;
        if let Some(client) = cache.default.clone() {
            return Ok(client);
        }
        let client = client_builder(&self.store).await?.build()?;
        cache.default = Some(client.clone());
        Ok(client)
    }

    pub async fn cursor_client(&self) -> Result<reqwest::Client> {
        if let Some(client) = self.cache.read().await.cursor.clone() {
            return Ok(client);
        }
        let mut cache = self.cache.write().await;
        if let Some(client) = cache.cursor.clone() {
            return Ok(client);
        }
        let client = client_builder(&self.store)
            .await?
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        cache.cursor = Some(client.clone());
        Ok(client)
    }

    pub async fn provider_client(&self, timeout: Duration) -> Result<reqwest::Client> {
        if let Some((_, client)) = self
            .cache
            .read()
            .await
            .provider
            .as_ref()
            .filter(|(cached_timeout, _)| *cached_timeout == timeout)
        {
            return Ok(client.clone());
        }
        let mut cache = self.cache.write().await;
        if let Some((_, client)) = cache
            .provider
            .as_ref()
            .filter(|(cached_timeout, _)| *cached_timeout == timeout)
        {
            return Ok(client.clone());
        }
        let client = client_builder(&self.store)
            .await?
            .timeout(timeout)
            .build()?;
        cache.provider = Some((timeout, client.clone()));
        Ok(client)
    }

    /// The provider client for one API key: the shared provider client when the key follows
    /// Proxy settings, otherwise a client that uses the key's own proxy or no proxy.
    pub async fn key_client(&self, proxy: &KeyProxy, timeout: Duration) -> Result<reqwest::Client> {
        if proxy.mode == KeyProxyMode::Inherit {
            return self.provider_client(timeout).await;
        }
        let cache_key = (proxy.clone(), timeout);
        if let Some(client) = self.cache.read().await.keys.get(&cache_key) {
            return Ok(client.clone());
        }
        let client = own_proxy_client(proxy, timeout)?;
        self.cache
            .write()
            .await
            .keys
            .insert(cache_key, client.clone());
        Ok(client)
    }

    /// Like `key_client`, but never caches a client for the key's own proxy, so testing
    /// unsaved proxies leaves nothing behind.
    pub async fn key_test_client(
        &self,
        proxy: &KeyProxy,
        timeout: Duration,
    ) -> Result<reqwest::Client> {
        if proxy.mode == KeyProxyMode::Inherit {
            return self.provider_client(timeout).await;
        }
        own_proxy_client(proxy, timeout)
    }

    pub async fn invalidate(&self) {
        *self.cache.write().await = ClientCache::default();
    }
}

/// A client for a key with its own proxy (`Custom`) or no proxy at all (`Direct`).
fn own_proxy_client(proxy: &KeyProxy, timeout: Duration) -> Result<reqwest::Client> {
    let builder = reqwest::Client::builder().use_native_tls().timeout(timeout);
    let builder = match proxy.mode {
        KeyProxyMode::Custom => builder.proxy(custom_proxy(
            &proxy.address,
            proxy
                .auth_enabled
                .then_some((&proxy.username, &proxy.password)),
        )?),
        _ => builder.no_proxy(),
    };
    Ok(builder.build()?)
}

pub async fn client_builder(store: &Store) -> Result<reqwest::ClientBuilder> {
    let settings = store.proxy_settings_secret().await?;
    // Use the platform TLS stack for compatibility with provider gateways that
    // only offer legacy TLS 1.2 cipher suites unsupported by rustls.
    let mut builder = reqwest::Client::builder().use_native_tls();
    if settings.mode.is_custom() {
        builder = builder.proxy(global_proxy(&settings)?);
    }
    Ok(builder)
}

pub async fn client(store: &Store) -> Result<reqwest::Client> {
    Ok(client_builder(store).await?.build()?)
}

pub async fn blocking_client_builder(store: &Store) -> Result<reqwest::blocking::ClientBuilder> {
    let settings = store.proxy_settings_secret().await?;
    let mut builder = reqwest::blocking::Client::builder().use_native_tls();
    if settings.mode.is_custom() {
        builder = builder.proxy(global_proxy(&settings)?);
    }
    Ok(builder)
}

fn global_proxy(settings: &ProxySettingsSecret) -> Result<reqwest::Proxy> {
    custom_proxy(
        &settings.address,
        settings
            .auth_enabled
            .then_some((&settings.username, &settings.password)),
    )
}

fn custom_proxy(address: &str, auth: Option<(&String, &String)>) -> Result<reqwest::Proxy> {
    let mut proxy =
        reqwest::Proxy::all(address)?.no_proxy(reqwest::NoProxy::from_string(LOCAL_NO_PROXY));
    if let Some((username, password)) = auth {
        proxy = proxy.basic_auth(username, password);
    }
    Ok(proxy)
}

pub fn reject_self_proxy(address: &str, local_proxy_port: u16) -> Result<()> {
    if local_proxy_port == 0 {
        return Ok(());
    }
    let url = url::Url::parse(address)
        .map_err(|error| Error::Config(format!("invalid proxy address: {error}")))?;
    if url.port_or_known_default() == Some(local_proxy_port) && url_host_is_loopback(&url) {
        return Err(Error::Config(
            "proxy address cannot point to the Cursor BYOK local proxy".into(),
        ));
    }
    Ok(())
}

fn url_host_is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => {
            host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        }
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::store::ProxyMode;

    fn custom_settings(address: String) -> ProxySettingsSecret {
        ProxySettingsSecret {
            mode: ProxyMode::Custom,
            address,
            auth_enabled: false,
            username: String::new(),
            password: String::new(),
        }
    }

    #[test]
    fn rejects_only_own_loopback_proxy_port() {
        for address in [
            "http://localhost:15721",
            "http://localhost.:15721",
            "http://127.0.0.2:15721",
            "http://[::1]:15721",
        ] {
            assert!(reject_self_proxy(address, 15721).is_err(), "{address}");
        }
        assert!(reject_self_proxy("http://127.0.0.1:7890", 15721).is_ok());
        assert!(reject_self_proxy("http://192.168.1.2:15721", 15721).is_ok());
        assert!(reject_self_proxy("http://127.0.0.1:15721", 0).is_ok());
    }

    #[tokio::test]
    async fn custom_proxy_bypasses_loopback_destinations() {
        let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = proxy_listener.local_addr().unwrap();
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target_listener.local_addr().unwrap();
        let target = tokio::spawn(async move {
            let (mut socket, _) = target_listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
        });
        let settings = custom_settings(format!("http://{proxy_address}"));
        let client = reqwest::Client::builder()
            .proxy(global_proxy(&settings).unwrap())
            .build()
            .unwrap();

        let body = client
            .get(format!("http://{target_address}"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        assert_eq!(body, "ok");
        target.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), proxy_listener.accept())
                .await
                .is_err(),
            "loopback destination unexpectedly reached the configured proxy"
        );
    }
}
