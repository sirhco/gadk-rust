// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Proxy resolution and HTTP CONNECT tunnelling for the Live dial. Port of Go
//! `golang.org/x/net/http/httpproxy` (as reached through `http.ProxyFromEnvironment`, which
//! gorilla/websocket's `DefaultDialer` uses) and gorilla/websocket v1.5.3 `httpProxyDialer`.

use std::net::IpAddr;
use std::str::FromStr;

use ::url::Url;
use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The dial's environment lookup (process environment in production, a map in tests). The seam
/// keeps tests from mutating the process environment (`std::env::set_var` is `unsafe` in
/// edition 2024 and `unsafe_code` is forbidden).
pub(crate) type EnvLookup<'a> = &'a (dyn Fn(&str) -> Option<String> + Send + Sync);

/// Upper bound on a CONNECT response head.
const MAX_CONNECT_RESPONSE_HEAD: usize = 8192;

/// Go `getEnvAny`: the first non-empty value among `names`.
fn get_env_any(env: &dyn Fn(&str) -> Option<String>, names: &[&str]) -> String {
    names
        .iter()
        .find_map(|n| env(n).filter(|v| !v.is_empty()))
        .unwrap_or_default()
}

/// Port of Go httpproxy `(*config).proxyForURL` for the URL gorilla passes to `Dialer.Proxy`
/// (`ws` rewritten to `http`, `wss` to `https`): the proxy to use for `target`, if any.
pub(crate) fn proxy_for_with(target: &Url, env: impl Fn(&str) -> Option<String>) -> Option<Url> {
    let env: &dyn Fn(&str) -> Option<String> = &env;
    let proxy = match target.scheme() {
        "wss" | "https" => parse_proxy(&get_env_any(env, &["HTTPS_PROXY", "https_proxy"])),
        "ws" | "http" => {
            let p = parse_proxy(&get_env_any(env, &["HTTP_PROXY", "http_proxy"]));
            // Go refuses HTTP_PROXY when REQUEST_METHOD is set (golang.org/s/cgihttpproxy) and
            // fails the dial; returning no proxy here dials direct instead (Option return type).
            if p.is_some() && env("REQUEST_METHOD").is_some_and(|v| !v.is_empty()) {
                return None;
            }
            p
        }
        _ => None,
    }?;
    let no_proxy = NoProxy::parse(&get_env_any(env, &["NO_PROXY", "no_proxy"]));
    let host = target
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = target
        .port_or_known_default()
        .map(|p| p.to_string())
        .unwrap_or_default();
    no_proxy.use_proxy(host, &port).then_some(proxy)
}

/// Go `parseProxy`: a value with no scheme or no host is retried as `http://<value>`; an
/// unparseable value means no proxy (Go `init` ignores the error).
fn parse_proxy(proxy: &str) -> Option<Url> {
    if proxy.is_empty() {
        return None;
    }
    match Url::parse(proxy) {
        Ok(u) if u.host_str().is_some_and(|h| !h.is_empty()) => Some(u),
        _ => Url::parse(&format!("http://{proxy}")).ok(),
    }
}

/// Go httpproxy `config.ipMatchers` / `domainMatchers`.
#[derive(Debug, Default)]
struct NoProxy {
    all: bool,
    ips: Vec<IpMatcher>,
    domains: Vec<DomainMatch>,
}

#[derive(Debug)]
enum IpMatcher {
    /// Go `cidrMatch`.
    Cidr(IpAddr, u8),
    /// Go `ipMatch`: an empty port matches every port.
    Ip(IpAddr, String),
}

/// Go `domainMatch`: `host` always starts with `.`; `match_host` also matches the bare name.
#[derive(Debug)]
struct DomainMatch {
    host: String,
    port: String,
    match_host: bool,
}

impl NoProxy {
    /// Go httpproxy `(*config).init` (the NO_PROXY half).
    fn parse(no_proxy: &str) -> NoProxy {
        let mut np = NoProxy::default();
        for p in no_proxy.split(',') {
            let p = p.trim().to_lowercase();
            if p.is_empty() {
                continue;
            }
            if p == "*" {
                return NoProxy {
                    all: true,
                    ..Default::default()
                };
            }
            if let Some((net, bits)) = parse_cidr(&p) {
                np.ips.push(IpMatcher::Cidr(net, bits));
                continue;
            }
            let (mut phost, pport) = match split_host_port(&p) {
                Some((h, port)) => {
                    if h.is_empty() {
                        continue;
                    }
                    (h, port)
                }
                None => (p.clone(), String::new()),
            };
            if phost.len() >= 2 && phost.starts_with('[') && phost.ends_with(']') {
                phost = phost[1..phost.len() - 1].to_string();
            }
            if let Ok(ip) = IpAddr::from_str(&phost) {
                np.ips.push(IpMatcher::Ip(ip, pport));
                continue;
            }
            if phost.is_empty() {
                continue;
            }
            if let Some(rest) = phost.strip_prefix("*.") {
                phost = format!(".{rest}");
            }
            let match_host = !phost.starts_with('.');
            if match_host {
                phost = format!(".{phost}");
            }
            np.domains.push(DomainMatch {
                host: idna_ascii(&phost),
                port: pport,
                match_host,
            });
        }
        np
    }

    /// Go httpproxy `(*config).useProxy` for `host` (no brackets) and `port`.
    fn use_proxy(&self, host: &str, port: &str) -> bool {
        if host == "localhost" {
            return false;
        }
        let ip = IpAddr::from_str(host).ok();
        if ip.is_some_and(is_loopback) {
            return false;
        }
        if self.all {
            return false;
        }
        let host = host.trim().to_lowercase();
        if let Some(ip) = ip
            && self.ips.iter().any(|m| m.matches(ip, port))
        {
            return false;
        }
        !self
            .domains
            .iter()
            .any(|m| m.matches(&host, port, ip.is_some()))
    }
}

impl IpMatcher {
    fn matches(&self, ip: IpAddr, port: &str) -> bool {
        match self {
            IpMatcher::Cidr(net, bits) => cidr_contains(*net, *bits, ip),
            IpMatcher::Ip(m, p) => *m == ip && (p.is_empty() || p == port),
        }
    }
}

impl DomainMatch {
    fn matches(&self, host: &str, port: &str, is_ip: bool) -> bool {
        if is_ip {
            return false;
        }
        if host.ends_with(&self.host) || (self.match_host && host == &self.host[1..]) {
            return self.port.is_empty() || self.port == port;
        }
        false
    }
}

/// Go `net.IP.IsLoopback`, which also treats v4-mapped IPv6 loopback as loopback.
fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Go `net.ParseCIDR` (address and prefix length only).
fn parse_cidr(s: &str) -> Option<(IpAddr, u8)> {
    let (ip, bits) = s.split_once('/')?;
    let ip = IpAddr::from_str(ip).ok()?;
    let bits: u8 = bits.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    (bits <= max).then_some((ip, bits))
}

fn cidr_contains(net: IpAddr, bits: u8, ip: IpAddr) -> bool {
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) => {
            prefix_eq(u128::from(u32::from(n)), u128::from(u32::from(i)), bits, 32)
        }
        (IpAddr::V6(n), IpAddr::V6(i)) => prefix_eq(u128::from(n), u128::from(i), bits, 128),
        _ => false,
    }
}

fn prefix_eq(a: u128, b: u128, bits: u8, width: u32) -> bool {
    if bits == 0 {
        return true;
    }
    let shift = width - u32::from(bits);
    (a >> shift) == (b >> shift)
}

/// Go `net.SplitHostPort` for NO_PROXY entries: `host:port` or `[host]:port`; `None` when the
/// entry has no port (or too many colons, e.g. a bare IPv6 address).
fn split_host_port(s: &str) -> Option<(String, String)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = after.strip_prefix(':')?;
        return Some((host.to_string(), port.to_string()));
    }
    let (host, port) = s.rsplit_once(':')?;
    if host.contains(':') {
        return None;
    }
    Some((host.to_string(), port.to_string()))
}

/// Go `idnaASCII` for a NO_PROXY domain that starts with `.`: non-ASCII labels become punycode
/// (the `url` crate's IDNA); on failure the entry is kept as written.
fn idna_ascii(v: &str) -> String {
    if v.is_ascii() {
        return v.to_string();
    }
    let (dot, rest) = match v.strip_prefix('.') {
        Some(r) => (".", r),
        None => ("", v),
    };
    match ::url::Host::parse(rest) {
        Ok(::url::Host::Domain(d)) => format!("{dot}{d}"),
        _ => v.to_string(),
    }
}

/// `host:port` of `u` with its scheme's default port (gorilla `hostPortNoPort`); IPv6 hosts
/// keep their brackets.
fn host_port(u: &Url) -> Result<String, String> {
    let host = u
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| format!("missing host in {u}"))?;
    let port = u
        .port_or_known_default()
        .ok_or_else(|| format!("missing port in {u}"))?;
    Ok(format!("{host}:{port}"))
}

/// Percent-decodes URL userinfo (Go `url.Userinfo` holds it decoded).
fn percent_decode(s: &str) -> String {
    fn hex(c: u8) -> Option<u8> {
        char::from(c)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Dials `target` (`host:port`, the scheme's default port) directly, or through an HTTP CONNECT
/// tunnel to `proxy` (port of gorilla `httpProxyDialer.Dial`). The error is the bare cause; the
/// caller adds the `Connect to <url> failed` context.
pub(crate) async fn dial(target: &Url, proxy: Option<&Url>) -> Result<TcpStream, String> {
    let addr = host_port(target)?;
    let Some(proxy) = proxy else {
        return TcpStream::connect(addr.as_str())
            .await
            .map_err(|e| e.to_string());
    };
    if proxy.scheme() != "http" {
        return Err(format!(
            "proxy: unsupported proxy scheme {}",
            proxy.scheme()
        ));
    }
    let mut conn = TcpStream::connect(host_port(proxy)?.as_str())
        .await
        .map_err(|e| e.to_string())?;
    let mut req = format!("CONNECT {addr} HTTP/1.1\r\nHost: {addr}\r\n");
    // gorilla sets the header only when the userinfo has a password (`passwordSet`).
    if let Some(password) = proxy.password() {
        let credential = base64::engine::general_purpose::STANDARD.encode(format!(
            "{}:{}",
            percent_decode(proxy.username()),
            percent_decode(password)
        ));
        req.push_str(&format!("Proxy-Authorization: Basic {credential}\r\n"));
    }
    req.push_str("\r\n");
    conn.write_all(req.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let head = read_response_head(&mut conn).await?;
    let status_line = head.split("\r\n").next().unwrap_or_default();
    let (code, text) = parse_status_line(status_line)?;
    if code != "200" {
        // gorilla: errors.New(strings.SplitN(resp.Status, " ", 2)[1]).
        return Err(if text.is_empty() { code } else { text });
    }
    Ok(conn)
}

/// Reads the CONNECT response head (through the blank line) one byte at a time, so no byte of
/// the tunnelled stream is consumed.
async fn read_response_head(conn: &mut TcpStream) -> Result<String, String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_CONNECT_RESPONSE_HEAD {
            return Err("proxy: CONNECT response head too large".into());
        }
        let n = conn.read(&mut byte).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("unexpected EOF".into());
        }
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// Go `http.ReadResponse` status-line parsing: `(code, text)`.
fn parse_status_line(line: &str) -> Result<(String, String), String> {
    let malformed = || format!("malformed HTTP response {line:?}");
    let (proto, status) = line.split_once(' ').ok_or_else(malformed)?;
    if !proto.starts_with("HTTP/") {
        return Err(malformed());
    }
    let status = status.trim_start();
    let (code, text) = status.split_once(' ').unwrap_or((status, ""));
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("malformed HTTP status code {code:?}"));
    }
    Ok((code.to_string(), text.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use ::url::Url;
    use base64::Engine;
    use futures::{SinkExt, StreamExt};
    use gadk_genai::LiveConnectConfig;
    use gadk_model::{Backend, HttpOptions, LlmResponse};
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    use super::{dial, proxy_for_with};
    use crate::config::{Auth, ResolvedConfig};
    use crate::error::GeminiError;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + Send + Sync + 'static {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    /// (name, env, target, expected proxy).
    type ProxyCase<'a> = (&'a str, Vec<(&'a str, &'a str)>, &'a str, Option<&'a str>);

    /// Go httpproxy proxyForURLTests, with the request scheme mapped as gorilla maps it
    /// (http → ws, https → wss). The default request is Go's `http://example.com`.
    #[test]
    fn proxy_for_url_table() {
        let cases: Vec<ProxyCase<'_>> = vec![
            (
                "host:port",
                vec![("HTTP_PROXY", "127.0.0.1:8080")],
                "ws://example.com/",
                Some("http://127.0.0.1:8080"),
            ),
            (
                "domain:port",
                vec![("HTTP_PROXY", "cache.corp.example.com:1234")],
                "ws://example.com/",
                Some("http://cache.corp.example.com:1234"),
            ),
            (
                "domain",
                vec![("HTTP_PROXY", "cache.corp.example.com")],
                "ws://example.com/",
                Some("http://cache.corp.example.com"),
            ),
            (
                "single label",
                vec![("HTTP_PROXY", "localhost")],
                "ws://example.com/",
                Some("http://localhost"),
            ),
            (
                "https proxy",
                vec![("HTTP_PROXY", "https://cache.corp.example.com")],
                "ws://example.com/",
                Some("https://cache.corp.example.com"),
            ),
            (
                "http ip",
                vec![("HTTP_PROXY", "http://127.0.0.1:8080")],
                "ws://example.com/",
                Some("http://127.0.0.1:8080"),
            ),
            (
                "https ip",
                vec![("HTTP_PROXY", "https://127.0.0.1:8080")],
                "ws://example.com/",
                Some("https://127.0.0.1:8080"),
            ),
            (
                "socks5 kept",
                vec![("HTTP_PROXY", "socks5://127.0.0.1")],
                "ws://example.com/",
                Some("socks5://127.0.0.1"),
            ),
            (
                "unknown scheme kept",
                vec![("HTTP_PROXY", "foo://host")],
                "ws://example.com/",
                Some("foo://host"),
            ),
            (
                "ws uses HTTP_PROXY",
                vec![
                    ("HTTP_PROXY", "http.proxy.tld"),
                    ("HTTPS_PROXY", "secure.proxy.tld"),
                ],
                "ws://insecure.tld/",
                Some("http://http.proxy.tld"),
            ),
            (
                "wss uses HTTPS_PROXY",
                vec![
                    ("HTTP_PROXY", "http.proxy.tld"),
                    ("HTTPS_PROXY", "secure.proxy.tld"),
                ],
                "wss://secure.tld/",
                Some("http://secure.proxy.tld"),
            ),
            (
                "wss https proxy",
                vec![
                    ("HTTP_PROXY", "http.proxy.tld"),
                    ("HTTPS_PROXY", "https://secure.proxy.tld"),
                ],
                "wss://secure.tld/",
                Some("https://secure.proxy.tld"),
            ),
            (
                "wss without HTTPS_PROXY",
                vec![("HTTP_PROXY", "http.proxy.tld")],
                "wss://secure.tld/",
                None,
            ),
            (
                "other scheme",
                vec![("HTTP_PROXY", "http.proxy.tld")],
                "ftp://insecure.tld/",
                None,
            ),
            // Go returns an error here ("refusing to use HTTP_PROXY value in CGI environment"); the
            // Option-returning port goes direct instead.
            (
                "CGI ignores HTTP_PROXY",
                vec![
                    ("HTTP_PROXY", "http://10.1.2.3:8080"),
                    ("REQUEST_METHOD", "GET"),
                ],
                "ws://example.com/",
                None,
            ),
            (
                "CGI keeps HTTPS_PROXY",
                vec![
                    ("HTTPS_PROXY", "https://secure.proxy.tld"),
                    ("REQUEST_METHOD", "GET"),
                ],
                "wss://secure.tld/",
                Some("https://secure.proxy.tld"),
            ),
            ("empty env", vec![], "ws://example.com/", None),
            (
                "no_proxy exact",
                vec![("NO_PROXY", "example.com"), ("HTTP_PROXY", "proxy")],
                "ws://example.com/",
                None,
            ),
            (
                "no_proxy leading dot",
                vec![("NO_PROXY", ".example.com"), ("HTTP_PROXY", "proxy")],
                "ws://example.com/",
                Some("http://proxy"),
            ),
            (
                "no_proxy partial label",
                vec![("NO_PROXY", "ample.com"), ("HTTP_PROXY", "proxy")],
                "ws://example.com/",
                Some("http://proxy"),
            ),
            (
                "no_proxy subdomain",
                vec![("NO_PROXY", "example.com"), ("HTTP_PROXY", "proxy")],
                "ws://foo.example.com/",
                None,
            ),
            (
                "no_proxy other domain",
                vec![("NO_PROXY", ".foo.com"), ("HTTP_PROXY", "proxy")],
                "ws://example.com/",
                Some("http://proxy"),
            ),
            (
                "idn dot entry",
                vec![("NO_PROXY", ".示例.com"), ("HTTP_PROXY", "proxy")],
                "ws://www.示例.com/",
                None,
            ),
            (
                "punycode entry",
                vec![("NO_PROXY", "xn--fsq092h.com"), ("HTTP_PROXY", "proxy")],
                "ws://www.示例.com/",
                None,
            ),
            (
                "idn entry, punycode target",
                vec![("NO_PROXY", "示例.com"), ("HTTP_PROXY", "proxy")],
                "ws://www.xn--fsq092h.com/",
                None,
            ),
            // Go TestFromEnvironment*: upper case wins; an empty upper-case value falls back.
            (
                "upper wins",
                vec![("HTTP_PROXY", "upper"), ("http_proxy", "lower")],
                "ws://example.com/",
                Some("http://upper"),
            ),
            (
                "lower case",
                vec![("http_proxy", "lower")],
                "ws://example.com/",
                Some("http://lower"),
            ),
            (
                "empty upper falls back",
                vec![("HTTP_PROXY", ""), ("http_proxy", "lower")],
                "ws://example.com/",
                Some("http://lower"),
            ),
            (
                "lower no_proxy",
                vec![("HTTP_PROXY", "proxy"), ("no_proxy", "example.com")],
                "ws://example.com/",
                None,
            ),
        ];
        for (name, env, target, want) in cases {
            assert_eq!(
                proxy_for_with(&url(target), env_of(&env)),
                want.map(url),
                "{name}"
            );
        }
    }

    const NO_PROXY: &str = "foobar.com, .barbaz.net, *.wildcard.io, 192.168.1.1, 192.168.1.2:81, 192.168.1.3:80, 10.0.0.0/30, 2001:db8::52:0:1, [2001:db8::52:0:2]:443, [2001:db8::52:0:3]:80, 2002:db8:a::45/64";

    /// Go httpproxy UseProxyTests (host, proxied?), each dialled on port 80.
    const USE_PROXY: &[(&str, bool)] = &[
        ("localhost", false),
        ("127.0.0.1", false),
        ("127.0.0.2", false),
        ("[::1]", false),
        ("[::2]", true),
        ("192.168.1.1", false),
        ("192.168.1.2", true),
        ("192.168.1.3", false),
        ("192.168.1.4", true),
        ("10.0.0.2", false),
        ("[2001:db8::52:0:1]", false),
        ("[2001:db8::52:0:2]", true),
        ("[2001:db8::52:0:3]", false),
        ("[2002:db8:a::123]", false),
        ("[fe80::424b:c8be:1643:a1b6]", true),
        ("barbaz.net", true),
        ("www.barbaz.net", false),
        ("foobar.com", false),
        ("www.foobar.com", false),
        ("foofoobar.com", true),
        ("baz.com", true),
        ("localhost.net", true),
        ("local.localhost", true),
        ("barbarbaz.net", true),
        ("wildcard.io", true),
        ("nested.wildcard.io", false),
        ("awildcard.io", true),
    ];

    #[test]
    fn use_proxy_table() {
        for &(host, proxied) in USE_PROXY {
            let got = proxy_for_with(
                &url(&format!("ws://{host}:80/")),
                env_of(&[("HTTP_PROXY", "proxy"), ("NO_PROXY", NO_PROXY)]),
            );
            assert_eq!(got.is_some(), proxied, "{host}");
        }
    }

    /// Go TestAllNoProxy: `*` disables the proxy for every host.
    #[test]
    fn all_no_proxy() {
        for &(host, _) in USE_PROXY {
            let got = proxy_for_with(
                &url(&format!("ws://{host}:80/")),
                env_of(&[("HTTP_PROXY", "proxy"), ("NO_PROXY", "*")]),
            );
            assert_eq!(got, None, "{host}");
        }
    }

    /// Go TestInvalidNoProxy: an entry with an empty host is skipped, not a panic.
    #[test]
    fn invalid_no_proxy_entry_is_ignored() {
        let got = proxy_for_with(
            &url("ws://example.com:80/"),
            env_of(&[("HTTP_PROXY", "proxy"), ("NO_PROXY", ":1")]),
        );
        assert_eq!(got, Some(url("http://proxy")));
    }

    /// Loopback targets are never proxied, whatever NO_PROXY says (spec R4).
    #[test]
    fn loopback_is_never_proxied() {
        for target in [
            "ws://localhost:9/",
            "ws://127.0.0.1:9/",
            "wss://127.9.9.9/",
            "ws://[::1]:9/",
            "ws://[::ffff:127.0.0.1]:9/",
        ] {
            let got = proxy_for_with(
                &url(target),
                env_of(&[("HTTP_PROXY", "http://p:1"), ("HTTPS_PROXY", "http://p:1")]),
            );
            assert_eq!(got, None, "{target}");
        }
    }

    /// A fake Live server on 127.0.0.1: upgrades one connection, consumes the setup frame,
    /// replies `{"setupComplete":{}}`, then reads until the client goes away.
    async fn fake_live_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let (mut sink, mut stream) = ws.split();
            let _setup = stream.next().await;
            sink.send(Message::text(r#"{"setupComplete":{}}"#))
                .await
                .unwrap();
            while let Some(Ok(_)) = stream.next().await {}
        });
        port
    }

    struct FakeProxy {
        port: u16,
        /// The CONNECT request head the proxy received.
        seen: Arc<Mutex<Option<String>>>,
    }

    /// A fake HTTP proxy: accepts one connection, records the request head, answers `reply`,
    /// and (when `upstream` is set) tunnels to `127.0.0.1:<upstream>` whatever host was asked for.
    async fn fake_proxy(upstream: Option<u16>, reply: &'static str) -> FakeProxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(None));
        let task_seen = seen.clone();
        tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if client.read(&mut byte).await.unwrap() == 0 {
                    return;
                }
                head.push(byte[0]);
            }
            *task_seen.lock() = Some(String::from_utf8(head).unwrap());
            client.write_all(reply.as_bytes()).await.unwrap();
            if let Some(up) = upstream {
                let mut server = tokio::net::TcpStream::connect(("127.0.0.1", up))
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
            }
        });
        FakeProxy { port, seen }
    }

    fn resolved(base_url: &str) -> ResolvedConfig {
        ResolvedConfig {
            backend: Backend::GeminiApi,
            project: None,
            location: None,
            base_url: base_url.into(),
            api_version: "v1beta".into(),
            auth: Some(Auth::ApiKey("k".into())),
            needs_adc: false,
            http_options: HttpOptions::default(),
        }
    }

    /// Spec R4 tunnel test: `gemini.test` resolves nowhere, so the dial can only succeed through
    /// the proxy; the proxy forwards the tunnel to the fake Live server.
    #[tokio::test]
    async fn live_dial_tunnels_through_http_proxy() {
        let live_port = fake_live_server().await;
        let proxy = fake_proxy(
            Some(live_port),
            "HTTP/1.1 200 Connection established\r\n\r\n",
        )
        .await;
        let env = env_of(&[(
            "HTTP_PROXY",
            &format!("http://user:p%40ss@127.0.0.1:{}", proxy.port),
        )]);
        let conn = crate::live::connect_with_env(
            &resolved("ws://gemini.test:443/"),
            "gemini-2.0-flash-live-001",
            &LiveConnectConfig::default(),
            &env,
        )
        .await
        .unwrap();
        let head = proxy.seen.lock().clone().unwrap();
        assert_eq!(
            head.split("\r\n").next(),
            Some("CONNECT gemini.test:443 HTTP/1.1")
        );
        assert!(head.contains("\r\nHost: gemini.test:443\r\n"), "{head}");
        let credential = base64::engine::general_purpose::STANDARD.encode("user:p@ss");
        assert!(
            head.contains(&format!("\r\nProxy-Authorization: Basic {credential}\r\n")),
            "{head}"
        );
        // The buffered setupComplete proves the Live handshake ran through the tunnel.
        assert_eq!(conn.recv().await.unwrap(), Some(LlmResponse::default()));
    }

    /// A proxy URL without a password sends no Proxy-Authorization (gorilla `passwordSet`).
    #[tokio::test]
    async fn no_password_sends_no_proxy_authorization() {
        let live_port = fake_live_server().await;
        let proxy = fake_proxy(Some(live_port), "HTTP/1.1 200 OK\r\n\r\n").await;
        let p = url(&format!("http://127.0.0.1:{}", proxy.port));
        let _tcp = dial(&url("ws://gemini.test:443/"), Some(&p)).await.unwrap();
        let head = proxy.seen.lock().clone().unwrap();
        assert!(!head.contains("Proxy-Authorization"), "{head}");
    }

    /// Any CONNECT status other than exactly 200 is a dial error carrying the status text
    /// (gorilla proxy.go).
    #[tokio::test]
    async fn non_200_connect_is_a_dial_error() {
        for (reply, want) in [
            (
                "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic\r\n\r\n",
                "Proxy Authentication Required",
            ),
            ("HTTP/1.1 204 No Content\r\n\r\n", "No Content"),
        ] {
            let proxy = fake_proxy(None, reply).await;
            let p = url(&format!("http://127.0.0.1:{}", proxy.port));
            let err = dial(&url("wss://gemini.test/"), Some(&p))
                .await
                .unwrap_err();
            assert_eq!(err, want);
            let head = proxy.seen.lock().clone().unwrap();
            assert!(
                head.starts_with("CONNECT gemini.test:443 HTTP/1.1\r\n"),
                "{head}"
            );
        }
        let proxy = fake_proxy(None, "HTTP/1.1 403 Forbidden\r\n\r\n").await;
        let env = env_of(&[("HTTPS_PROXY", &format!("127.0.0.1:{}", proxy.port))]);
        let err = crate::live::connect_with_env(
            &resolved("wss://gemini.test/"),
            "m",
            &LiveConnectConfig::default(),
            &env,
        )
        .await
        .unwrap_err();
        let GeminiError::Live(e) = err else {
            panic!("{err:?}")
        };
        assert!(
            e.message.starts_with("Connect to wss://gemini.test/"),
            "{}",
            e.message
        );
        assert!(e.message.ends_with(" failed: Forbidden"), "{}", e.message);
    }

    /// socks5:// and https:// proxies are rejected (spec R4).
    #[tokio::test]
    async fn unsupported_proxy_schemes_fail_the_dial() {
        for scheme in ["socks5", "https"] {
            let p = url(&format!("{scheme}://127.0.0.1:1"));
            let err = dial(&url("wss://gemini.test/"), Some(&p))
                .await
                .unwrap_err();
            assert_eq!(err, format!("proxy: unsupported proxy scheme {scheme}"));
        }
        let env = env_of(&[("HTTPS_PROXY", "socks5://127.0.0.1:1")]);
        let err = crate::live::connect_with_env(
            &resolved("wss://gemini.test/"),
            "m",
            &LiveConnectConfig::default(),
            &env,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .ends_with("failed: proxy: unsupported proxy scheme socks5"),
            "{err}"
        );
    }

    /// A loopback Live endpoint is dialled directly even when a (dead) proxy is configured.
    #[tokio::test]
    async fn loopback_endpoint_bypasses_the_proxy() {
        let live_port = fake_live_server().await;
        let env = env_of(&[("HTTP_PROXY", "http://127.0.0.1:1")]);
        let conn = crate::live::connect_with_env(
            &resolved(&format!("ws://127.0.0.1:{live_port}/")),
            "m",
            &LiveConnectConfig::default(),
            &env,
        )
        .await
        .unwrap();
        assert_eq!(conn.recv().await.unwrap(), Some(LlmResponse::default()));
    }
}
