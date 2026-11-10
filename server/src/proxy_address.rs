//! Reads proxy addresses written the ways proxy sellers list them.
//!
//! Accepted, each with an optional `scheme://` prefix (http when absent):
//! - `host:port`
//! - `user:pass@host:port` and `host:port@user:pass`
//! - `host:port:user:pass` and `user:pass:host:port`
//! - `host:port user pass`, `host port user pass`, `host:port user:pass` and `host port`
//!
//! Passwords may contain `:` (and `@` in the `@` forms). Credentials of an address with an
//! explicit scheme are percent-decoded, as in a URL. Error messages never repeat the input,
//! which usually contains a password.
use crate::{Error, Result};

const UNRECOGNIZED: &str = "unrecognized proxy format; use scheme://user:pass@host:port, \
                            host:port:user:pass or host:port user pass";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedProxy {
    /// `scheme://host:port`, without credentials.
    pub address: String,
    pub credentials: Option<Credentials>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

struct Endpoint {
    host: String,
    port: u16,
}

pub fn parse(input: &str) -> Result<ParsedProxy> {
    let input = input.trim();
    if input.is_empty() {
        return Err(invalid("the address is empty"));
    }
    let (scheme, rest, explicit_scheme) = match input.split_once("://") {
        Some((scheme, rest)) => (scheme_name(scheme)?, rest.trim_end_matches('/'), true),
        None => ("http", input, false),
    };
    let (endpoint, credentials) = endpoint_and_credentials(rest)?;
    let credentials = credentials.map(|mut credentials| {
        if explicit_scheme {
            credentials.username = percent_decode(&credentials.username);
            credentials.password = percent_decode(&credentials.password);
        }
        credentials
    });
    let address = format!("{scheme}://{}:{}", endpoint.host, endpoint.port);
    reqwest::Proxy::all(&address)?;
    Ok(ParsedProxy {
        address,
        credentials,
    })
}

fn scheme_name(scheme: &str) -> Result<&'static str> {
    match scheme.to_ascii_lowercase().as_str() {
        "http" => Ok("http"),
        "https" => Ok("https"),
        "socks" | "socks5" => Ok("socks5"),
        "socks5h" => Ok("socks5h"),
        _ => Err(invalid("the scheme must be http, https, socks5 or socks5h")),
    }
}

fn endpoint_and_credentials(rest: &str) -> Result<(Endpoint, Option<Credentials>)> {
    let tokens = rest.split_whitespace().collect::<Vec<_>>();
    match tokens.as_slice() {
        [token] => single_token(token),
        [host, port] if port_number(port).is_some() && !host.contains(':') => {
            Ok((endpoint(host, port)?, None))
        }
        [address, credentials] => {
            Ok((endpoint_of(address)?, Some(user_and_password(credentials)?)))
        }
        [address, username, password] if address.contains(':') => Ok((
            endpoint_of(address)?,
            Some(credentials(username, password)?),
        )),
        [host, port, username, password] => Ok((
            endpoint(host, port)?,
            Some(credentials(username, password)?),
        )),
        _ => Err(invalid(UNRECOGNIZED)),
    }
}

fn single_token(token: &str) -> Result<(Endpoint, Option<Credentials>)> {
    if token.contains('@') {
        return at_form(token);
    }
    if token.starts_with('[') {
        return Ok((endpoint_of(token)?, None));
    }
    let parts = token.split(':').collect::<Vec<_>>();
    let count = parts.len();
    if count == 2 {
        return Ok((endpoint(parts[0], parts[1])?, None));
    }
    if count < 4 {
        return Err(invalid(UNRECOGNIZED));
    }
    let host_first = port_number(parts[1]).is_some();
    let host_last = port_number(parts[count - 1]).is_some();
    // Both readings fit when a password is a number: prefer the one whose host looks like one.
    let host_first = match (host_first, host_last) {
        (true, true) => looks_like_host(parts[0]) || !looks_like_host(parts[count - 2]),
        (first, last) if first || last => first,
        _ => return Err(invalid(UNRECOGNIZED)),
    };
    if host_first {
        let password = parts[3..].join(":");
        let endpoint = endpoint(parts[0], parts[1])?;
        return Ok((endpoint, Some(credentials(parts[2], &password)?)));
    }
    let password = parts[1..count - 2].join(":");
    let endpoint = endpoint(parts[count - 2], parts[count - 1])?;
    Ok((endpoint, Some(credentials(parts[0], &password)?)))
}

/// `user:pass@host:port` (split at the last `@`) or `host:port@user:pass` (split at the first
/// `@`), so an `@` in the password stays in it.
fn at_form(token: &str) -> Result<(Endpoint, Option<Credentials>)> {
    let user_first = token
        .rsplit_once('@')
        .and_then(|(user_info, address)| Some((endpoint_of(address).ok()?, user_info)));
    let host_first = token
        .split_once('@')
        .and_then(|(address, user_info)| Some((endpoint_of(address).ok()?, user_info)));
    let (endpoint, user_info) = match (user_first, host_first) {
        // `host:port@user:1234` also reads as user-first with host `user`.
        (Some(user_first), Some(host_first)) => {
            if looks_like_host(&host_first.0.host) && !looks_like_host(&user_first.0.host) {
                host_first
            } else {
                user_first
            }
        }
        (Some(user_first), None) => user_first,
        (None, Some(host_first)) => host_first,
        (None, None) => return Err(invalid(UNRECOGNIZED)),
    };
    Ok((endpoint, Some(user_and_password(user_info)?)))
}

/// `host:port` or `[ipv6]:port`.
fn endpoint_of(address: &str) -> Result<Endpoint> {
    if let Some(rest) = address.strip_prefix('[') {
        let (host, port) = rest
            .split_once("]:")
            .ok_or_else(|| invalid("missing port"))?;
        return endpoint(&format!("[{host}]"), port);
    }
    let (host, port) = address
        .rsplit_once(':')
        .ok_or_else(|| invalid("missing port"))?;
    endpoint(host, port)
}

fn endpoint(host: &str, port: &str) -> Result<Endpoint> {
    let bracketed = host.starts_with('[') && host.ends_with(']');
    let forbidden = |ch: char| ch.is_whitespace() || matches!(ch, '/' | '@' | '?' | '#');
    if host.is_empty() || host.contains(forbidden) || (!bracketed && host.contains(':')) {
        return Err(invalid("the host is not valid"));
    }
    let port = port_number(port).ok_or_else(|| invalid("the port must be 1-65535"))?;
    Ok(Endpoint {
        host: host.to_owned(),
        port,
    })
}

fn port_number(text: &str) -> Option<u16> {
    if text.is_empty() || text.len() > 5 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|port| *port > 0)
}

fn looks_like_host(text: &str) -> bool {
    text.contains('.') || text.eq_ignore_ascii_case("localhost")
}

/// `user:pass` (the first `:` separates them) or a username alone.
fn user_and_password(text: &str) -> Result<Credentials> {
    let (username, password) = text.split_once(':').unwrap_or((text, ""));
    credentials(username, password)
}

fn credentials(username: &str, password: &str) -> Result<Credentials> {
    if username.is_empty() {
        return Err(invalid("the username is empty"));
    }
    Ok(Credentials {
        username: username.to_owned(),
        password: password.to_owned(),
    })
}

/// Decodes `%XX` sequences; text that does not decode to UTF-8 is kept as written.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = bytes.get(index + 1..index + 3).and_then(|hex| {
            std::str::from_utf8(hex)
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        });
        match (bytes[index], escaped) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                index += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| text.to_owned())
}

fn invalid(detail: &str) -> Error {
    Error::Config(format!("invalid proxy address: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(input: &str) -> (String, Option<(String, String)>) {
        let proxy = parse(input).unwrap_or_else(|error| panic!("{input}: {error}"));
        let credentials = proxy
            .credentials
            .map(|credentials| (credentials.username, credentials.password));
        (proxy.address, credentials)
    }

    fn with_login(
        address: &str,
        username: &str,
        password: &str,
    ) -> (String, Option<(String, String)>) {
        (address.into(), Some((username.into(), password.into())))
    }

    #[test]
    fn reads_addresses_without_credentials() {
        let plain = |address: &str| (address.to_owned(), None);
        assert_eq!(parsed("127.0.0.1:7890"), plain("http://127.0.0.1:7890"));
        assert_eq!(
            parsed(" http://127.0.0.1:7890/ "),
            plain("http://127.0.0.1:7890")
        );
        assert_eq!(
            parsed("HTTPS://proxy.example.com:443"),
            plain("https://proxy.example.com:443")
        );
        assert_eq!(
            parsed("socks://127.0.0.1:1080"),
            plain("socks5://127.0.0.1:1080")
        );
        assert_eq!(
            parsed("socks5h://[::1]:1080"),
            plain("socks5h://[::1]:1080")
        );
        assert_eq!(
            parsed("proxy.example.com 8080"),
            plain("http://proxy.example.com:8080")
        );
    }

    #[test]
    fn reads_the_formats_proxy_sellers_use() {
        let fr = "http://fr.example.net:8080";
        for input in [
            "fr.example.net:8080:alice:s3cret",
            "alice:s3cret@fr.example.net:8080",
            "http://alice:s3cret@fr.example.net:8080",
            "fr.example.net:8080@alice:s3cret",
            "alice:s3cret:fr.example.net:8080",
            "fr.example.net:8080 alice s3cret",
            "fr.example.net 8080 alice s3cret",
            "fr.example.net:8080\talice:s3cret",
        ] {
            assert_eq!(parsed(input), with_login(fr, "alice", "s3cret"), "{input}");
        }
        assert_eq!(
            parsed("socks5://bob:pw@10.0.0.5:1080"),
            with_login("socks5://10.0.0.5:1080", "bob", "pw")
        );
        assert_eq!(
            parsed("gate.example.info:59781:user3proxy:huc_An-oc7o87"),
            with_login(
                "http://gate.example.info:59781",
                "user3proxy",
                "huc_An-oc7o87"
            )
        );
    }

    #[test]
    fn keeps_separators_inside_passwords() {
        assert_eq!(
            parsed("fr.example.net:8080:alice:pa:ss"),
            with_login("http://fr.example.net:8080", "alice", "pa:ss")
        );
        assert_eq!(
            parsed("alice:p@ss@fr.example.net:8080"),
            with_login("http://fr.example.net:8080", "alice", "p@ss")
        );
        assert_eq!(
            parsed("fr.example.net:8080@alice:p@ss"),
            with_login("http://fr.example.net:8080", "alice", "p@ss")
        );
        assert_eq!(
            parsed("fr.example.net:8080@alice:1234"),
            with_login("http://fr.example.net:8080", "alice", "1234")
        );
        assert_eq!(
            parsed("http://alice:p%40ss%3A1@fr.example.net:8080"),
            with_login("http://fr.example.net:8080", "alice", "p@ss:1")
        );
        // Without a scheme the password is taken literally.
        assert_eq!(
            parsed("fr.example.net:8080:alice:100%25"),
            with_login("http://fr.example.net:8080", "alice", "100%25")
        );
    }

    #[test]
    fn tells_host_first_from_user_first_when_numbers_are_ambiguous() {
        assert_eq!(
            parsed("alice:12345:10.0.0.5:3128"),
            with_login("http://10.0.0.5:3128", "alice", "12345")
        );
        assert_eq!(
            parsed("10.0.0.5:3128:alice:12345"),
            with_login("http://10.0.0.5:3128", "alice", "12345")
        );
        assert_eq!(
            parsed("proxyhost:8080:alice:pass"),
            with_login("http://proxyhost:8080", "alice", "pass")
        );
    }

    #[test]
    fn rejects_what_it_cannot_read_without_echoing_the_password() {
        for input in [
            "",
            "ftp://proxy.example.com:21",
            "proxy.example.com",
            "proxy.example.com:99999",
            "proxy.example.com:0",
            "proxy.example.com:port",
            "proxy.example.com:8080:alice",
            ":8080:alice:topsecret",
            "proxy.example.com:8080::topsecret",
            "a b c d e topsecret",
        ] {
            let Err(Error::Config(message)) = parse(input) else {
                panic!("{input:?} must be rejected");
            };
            assert!(message.starts_with("invalid proxy address: "), "{message}");
            assert!(!message.contains("topsecret"), "{message}");
        }
    }
}
