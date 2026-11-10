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

//! Port of Go `internal/originguard`: the origin and Host checks in front of every route and the
//! WebSocket upgrade's origin check.
//!
//! It answers two questions, in this order:
//!
//! 1. Was this request pointed at us by rebound DNS? A server bound to a loopback address can
//!    only be reached from this machine, so a Host naming anything but loopback (or an allowed
//!    origin's host) means the name resolved here without belonging here.
//! 2. Is the Origin one we serve? Absent, matching the request's own origin, or on the
//!    configured allowlist.
//!
//! Both rest on the server being reachable from this machine only; a server reachable over a
//! network cannot tell a rebound page from an ordinary remote browser. Authentication closes that.

// Wired into the server by plan 5a Task 13.
#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::ConnectInfo;
use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;
use http::StatusCode;
use http::request::Parts;
use tokio::net::TcpListener;

use super::controllers::CheckOrigin;
use super::http_error::http_error;
use super::routing::{Handler, Request, handler};

/// The error a refused request is answered with (Go `ErrHostNotAllowed`, `ErrOriginNotAllowed`).
/// Both are safe to show a caller: they name the check that failed and nothing about the
/// configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum GuardError {
    /// Go `ErrHostNotAllowed`: the Host names a host the server cannot legitimately be reached
    /// under, which is what DNS rebinding looks like from the server side.
    #[error("host not allowed")]
    HostNotAllowed,
    /// Go `ErrOriginNotAllowed`: the Origin is not one the server serves.
    #[error("origin not allowed")]
    OriginNotAllowed,
}

/// Port of Go `originguard.Config`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Config {
    /// The address the server binds (Go `BindHost`); a loopback address arms the rebinding
    /// check. Empty: not declared. A wildcard (`""`, `":8080"`, `"0.0.0.0"`, `"[::]"`) says no
    /// more than nothing.
    pub bind_host: String,
    /// Origins the server serves (`scheme://host[:port]`; a bare host or host:port is read as
    /// http). A single `"*"` turns both checks off.
    pub allowed_origins: Vec<String>,
}

/// The address a connection was accepted on (Go `http.LocalAddrContextKey`), which the origin
/// rule reads under a wildcard or undeclared bind.
///
/// Serve the router with `into_make_service_with_connect_info::<LocalAddr>()` to provide it; a
/// request without it is treated as Go treats a request that did not come from an
/// `http.Server` (no accepted address).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalAddr(pub SocketAddr);

impl Connected<IncomingStream<'_, TcpListener>> for LocalAddr {
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Self {
        // An unreadable socket address reads as the unspecified address, which is neither
        // loopback nor anything an Origin can match (Go: no accepted address).
        LocalAddr(
            stream
                .io()
                .local_addr()
                .unwrap_or_else(|_| SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))),
        )
    }
}

/// Go `maxOriginLen`: the longest Origin the guard parses.
pub(crate) const MAX_ORIGIN_LEN: usize = 2048;

/// Port of Go `originguard.Policy`: an immutable decision procedure built by [`Policy::new`].
#[derive(Debug, Clone, Default)]
pub(crate) struct Policy {
    /// An `"*"` entry: both checks off.
    allow_all: bool,
    /// A bind host was declared.
    bind_declared: bool,
    /// The declared bind host is a loopback address (arms the rebinding check).
    bound_to_loopback: bool,
    /// The declared bind host names every interface.
    bind_wildcard: bool,
    /// The allowed origins, normalized.
    origins: std::collections::HashSet<String>,
    /// The bare host of each allowed origin: listing an origin vouches for its host.
    hosts: std::collections::HashSet<String>,
}

impl Policy {
    /// Go `originguard.New`.
    pub(crate) fn new(cfg: &Config) -> Policy {
        let declared = !cfg.bind_host.is_empty();
        let mut p = Policy {
            bind_declared: declared,
            bound_to_loopback: declared && is_loopback_addr(&cfg.bind_host),
            bind_wildcard: declared && is_wildcard_addr(&cfg.bind_host),
            ..Policy::default()
        };
        for raw in &cfg.allowed_origins {
            let origin = raw.trim();
            if origin.is_empty() {
                continue;
            }
            if origin == "*" {
                p.allow_all = true;
                continue;
            }
            let origin = normalize_origin(origin);
            if let Some(u) = go_url::parse(&origin) {
                let hostname = go_url::hostname(&u.host);
                if !hostname.is_empty() {
                    p.hosts.insert(bare_host(hostname));
                }
            }
            p.origins.insert(origin);
        }
        p
    }

    /// Go `Policy.Check`: whether the request may be served.
    pub(crate) fn check(&self, r: &Parts) -> Result<(), GuardError> {
        if self.allow_all {
            return Ok(());
        }
        if self.is_dns_rebinding(r) {
            return Err(GuardError::HostNotAllowed);
        }
        // An absent Origin is a non-browser client, which this check cannot constrain.
        let origin = first_header_value(r, "origin");
        if !origin.is_empty() && !self.origin_allowed(r, &origin) {
            return Err(GuardError::OriginNotAllowed);
        }
        Ok(())
    }

    /// Go `Policy.CheckOrigin`: the WebSocket upgrade's origin check, by [`Policy::check`]'s
    /// rules.
    pub(crate) fn check_origin(&self, r: &Parts) -> bool {
        if self.allow_all {
            return true;
        }
        let origin = first_header_value(r, "origin");
        origin.is_empty() || self.origin_allowed(r, &origin)
    }

    /// [`Policy::check_origin`] as the runtime controller's
    /// [`CheckOrigin`](crate::rest::controllers::CheckOrigin) hook.
    pub(crate) fn check_origin_fn(self: &Arc<Self>) -> CheckOrigin {
        let policy = Arc::clone(self);
        Arc::new(move |parts: &Parts| policy.check_origin(parts))
    }

    /// Go `Policy.Middleware`: [`Policy::check`] on every request before `next` runs; a refused
    /// request answers 403 `Forbidden: <reason>` in `http.Error` framing.
    pub(crate) fn middleware(self: &Arc<Self>, next: Handler) -> Handler {
        let policy = Arc::clone(self);
        handler(move |req: Request| {
            let policy = Arc::clone(&policy);
            let next = Arc::clone(&next);
            async move {
                let (parts, body) = req.into_parts();
                if let Err(e) = policy.check(&parts) {
                    return http_error(StatusCode::FORBIDDEN, &format!("Forbidden: {e}"));
                }
                next(Request::from_parts(parts, body)).await
            }
        })
    }

    /// Go `originAllowed`.
    fn origin_allowed(&self, r: &Parts, raw_origin: &str) -> bool {
        if raw_origin.len() > MAX_ORIGIN_LEN {
            return false;
        }
        let origin = normalize_origin(raw_origin);
        if self.origins.contains(&origin) {
            return true;
        }
        // A server reached over loopback serves loopback pages only, whatever origins are
        // configured (configured ones matched above).
        if self.serves_only_this_machine(r) && !is_loopback_addr(&origin_host(&origin)) {
            return false;
        }
        let request_origin = effective_request_origin(r);
        !request_origin.is_empty() && origin == request_origin
    }

    /// Go `servesOnlyThisMachine`: a declared non-wildcard bind answers outright; otherwise the
    /// accepted connection stands in.
    fn serves_only_this_machine(&self, r: &Parts) -> bool {
        if self.bind_declared && !self.bind_wildcard {
            return self.bound_to_loopback;
        }
        is_loopback_addr(&server_host(r))
    }

    /// Go `isDNSRebinding`: only a declared loopback bind arms it, and only the real Host
    /// header counts (a page may set `X-Forwarded-Host` and `Forwarded` freely).
    fn is_dns_rebinding(&self, r: &Parts) -> bool {
        if !self.bound_to_loopback {
            return false;
        }
        let host = request_host(r);
        if host.is_empty() {
            return false;
        }
        // Host is a singleton: a list in it is smuggling.
        if host.contains(',') {
            return true;
        }
        if is_loopback_addr(&host) {
            return false;
        }
        !self.hosts.contains(&bare_host(&host))
    }
}

/// Go `NormalizeOrigin`: `addr` as an RFC 6454 origin (`scheme://host[:port]`, lowercased); a
/// bare host or host:port gets `http://`; `"*"` and `""` pass through, as does anything that does
/// not parse as a URL with a host.
pub(crate) fn normalize_origin(addr: &str) -> String {
    let addr = addr.trim();
    if addr.is_empty() || addr == "*" {
        return addr.to_owned();
    }
    let addr = if addr.contains("://") {
        addr.to_owned()
    } else {
        format!("http://{addr}")
    };
    match go_url::parse(&addr) {
        Some(u) if !u.host.is_empty() => {
            format!("{}://{}", go_to_lower(&u.scheme), go_to_lower(&u.host))
        }
        _ => addr,
    }
}

/// Go `r.Host`: the request target's authority, else the Host header.
fn request_host(r: &Parts) -> String {
    if let Some(a) = r.uri.authority() {
        return a.as_str().to_owned();
    }
    r.headers
        .get(http::header::HOST)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default()
}

/// Go `Header.Get`: the first value of header `name`, or `""`.
fn header(r: &Parts, name: &str) -> String {
    r.headers
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default()
}

/// Go `serverHost`: the accepted address ([`LocalAddr`]), or `""`.
fn server_host(r: &Parts) -> String {
    r.extensions
        .get::<ConnectInfo<LocalAddr>>()
        .map(|ConnectInfo(LocalAddr(a))| a.to_string())
        .unwrap_or_default()
}

/// Go `originHost`: the host of an origin, or `""`.
fn origin_host(origin: &str) -> String {
    go_url::parse(origin)
        .map(|u| go_url::hostname(&u.host).to_owned())
        .unwrap_or_default()
}

/// Go `effectiveRequestOrigin`: the origin a browser compares against, honouring `Forwarded`
/// and `X-Forwarded-*` (lowercased).
fn effective_request_origin(r: &Parts) -> String {
    if let Some((proto, host)) = forwarded_proto_host(r) {
        return format!("{}://{}", origin_scheme(&proto), go_to_lower(&host));
    }
    let mut host = first_header_value(r, "x-forwarded-host");
    if host.is_empty() {
        host = request_host(r);
    }
    if host.is_empty() {
        return String::new();
    }
    let mut proto = first_header_value(r, "x-forwarded-proto");
    if proto.is_empty() {
        proto = request_scheme(r);
    }
    format!("{}://{}", origin_scheme(&proto), go_to_lower(&host))
}

/// Go `forwardedProtoHost`: proto and host of the first element of an RFC 7239 `Forwarded`
/// header; both must be present.
fn forwarded_proto_host(r: &Parts) -> Option<(String, String)> {
    let forwarded = header(r, "forwarded");
    if forwarded.is_empty() {
        return None;
    }
    let first = forwarded.split(',').next().unwrap_or_default();
    let (mut proto, mut host) = (String::new(), String::new());
    for element in first.split(';') {
        let Some((name, value)) = element.split_once('=') else {
            continue;
        };
        let value = unquote(value.trim());
        match go_to_lower(name.trim()).as_str() {
            "proto" => proto = value.to_owned(),
            "host" => host = value.to_owned(),
            _ => {}
        }
    }
    (!proto.is_empty() && !host.is_empty()).then_some((proto, host))
}

/// Go `requestScheme`: the request target's scheme, else `http`. (gadk has no TLS connection
/// state on a request; a TLS front end that wants `https` here sets it on the URI or forwards
/// `X-Forwarded-Proto`.)
fn request_scheme(r: &Parts) -> String {
    r.uri.scheme_str().unwrap_or("http").to_owned()
}

/// Go `originScheme`: a WebSocket scheme as the page's scheme.
fn origin_scheme(scheme: &str) -> String {
    match go_to_lower(scheme).as_str() {
        "ws" => "http".to_owned(),
        "wss" => "https".to_owned(),
        s => s.to_owned(),
    }
}

/// Go `firstHeaderValue`: the first comma-separated element of a header, trimmed.
fn first_header_value(r: &Parts, name: &str) -> String {
    let value = header(r, name);
    value
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// Go `unquote`: strips one pair of wrapping double quotes.
fn unquote(value: &str) -> &str {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Go `isLoopbackAddr`: `host` (with or without a port) names a loopback address.
fn is_loopback_addr(host: &str) -> bool {
    let bare = bare_host(host);
    if bare.is_empty() {
        return false;
    }
    if bare == "localhost" {
        return true;
    }
    go_url::parse_addr(&bare).is_some_and(|a| a.is_loopback())
}

/// Go `isWildcardAddr`: `host` (with or without a port) names every interface.
fn is_wildcard_addr(host: &str) -> bool {
    let bare = bare_host(host);
    if bare.is_empty() {
        return true;
    }
    go_url::parse_addr(&bare).is_some_and(|a| a.is_unspecified())
}

/// Go `bareHost`: lowercased, port and root dot stripped; an unparsable host comes back whole.
fn bare_host(host: &str) -> String {
    go_to_lower(strip_port(host))
        .trim_end_matches('.')
        .to_owned()
}

/// Go `stripPort`: `host` without a valid port, else unchanged.
pub(crate) fn strip_port(host: &str) -> &str {
    let (bare, suffix) = if let Some(rest) = host.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return host;
        };
        (&rest[..end], &rest[end + 1..])
    } else if host.matches(':').count() == 1 {
        let i = host.find(':').unwrap_or(host.len());
        (&host[..i], &host[i..])
    } else {
        return host;
    };
    if suffix.is_empty() {
        return bare;
    }
    match suffix.strip_prefix(':') {
        Some(port) if !port.is_empty() && port.bytes().all(|c| c.is_ascii_digit()) => bare,
        _ => host,
    }
}

/// Go `strings.ToLower`: ASCII fast path; otherwise invalid UTF-8 is already U+FFFD (the header
/// was read lossily) and each rune maps through Go's `unicode.ToLower`, which differs from
/// Rust's full lowercase mapping only for U+0130.
fn go_to_lower(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    s.chars()
        .map(|c| {
            let mut l = c.to_lowercase();
            match (l.next(), l.next()) {
                (Some(one), None) => one,
                _ => 'i', // U+0130 LATIN CAPITAL LETTER I WITH DOT ABOVE → U+0069 in Go
            }
        })
        .collect()
}

/// The subset of Go `net/url` and `net/netip` the guard and the router rely on, byte for byte:
/// `url.Parse`'s acceptance rules, its lowercased scheme and unescaped Host, `URL.Hostname`,
/// path unescaping, escaping and `validEncoded`, and `netip.ParseAddr` with
/// `IsLoopback`/`IsUnspecified`.
pub(crate) mod go_url {
    use super::{IpAddr, Ipv4Addr, Ipv6Addr};

    /// What `url.Parse` yields that the guard reads.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub(super) struct Url {
        /// Lowercased scheme.
        pub scheme: String,
        /// Unescaped `Host` (`host[:port]`, IPv6 in brackets).
        pub host: String,
    }

    /// Go's `encoding` modes the guard needs.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Path,
        Host,
        Zone,
        UserPassword,
        Fragment,
    }

    /// Go `url.Parse(raw)`; `None` where Go returns an error.
    pub(super) fn parse(raw: &str) -> Option<Url> {
        let (u, frag) = match raw.split_once('#') {
            Some((u, f)) => (u, f),
            None => (raw, ""),
        };
        let url = parse_no_fragment(u)?;
        if !frag.is_empty() {
            unescape(frag, Mode::Fragment)?;
        }
        Some(url)
    }

    /// Go `parse(rawURL, viaRequest=false)`.
    fn parse_no_fragment(raw: &str) -> Option<Url> {
        if raw.bytes().any(|b| b < b' ' || b == 0x7f) {
            return None;
        }
        if raw == "*" {
            return Some(Url::default());
        }
        let (scheme, rest) = get_scheme(raw)?;
        let scheme = scheme.to_ascii_lowercase();
        let rest = if rest.ends_with('?') && rest.matches('?').count() == 1 {
            &rest[..rest.len() - 1]
        } else {
            rest.split_once('?').map_or(rest, |(r, _)| r)
        };
        if !rest.starts_with('/') {
            if !scheme.is_empty() {
                // A rootless path is opaque: no host.
                return Some(Url {
                    scheme,
                    host: String::new(),
                });
            }
            let segment = rest.split_once('/').map_or(rest, |(s, _)| s);
            if segment.contains(':') {
                return None;
            }
        }
        let mut host = String::new();
        let mut path = rest;
        if (!scheme.is_empty() || !rest.starts_with("///")) && rest.starts_with("//") {
            let after = &rest[2..];
            let (authority, p) = match after.find('/') {
                Some(i) => (&after[..i], &after[i..]),
                None => (after, ""),
            };
            host = parse_authority(&scheme, authority)?;
            path = p;
        }
        unescape(path, Mode::Path)?;
        Some(Url { scheme, host })
    }

    /// Go `getScheme`.
    fn get_scheme(raw: &str) -> Option<(&str, &str)> {
        for (i, c) in raw.bytes().enumerate() {
            match c {
                b'a'..=b'z' | b'A'..=b'Z' => {}
                b'0'..=b'9' | b'+' | b'-' | b'.' => {
                    if i == 0 {
                        return Some(("", raw));
                    }
                }
                b':' => {
                    if i == 0 {
                        return None; // "missing protocol scheme"
                    }
                    return Some((&raw[..i], &raw[i + 1..]));
                }
                _ => return Some(("", raw)),
            }
        }
        Some(("", raw))
    }

    /// Go `parseAuthority`: the unescaped host; the userinfo is validated and dropped.
    fn parse_authority(scheme: &str, authority: &str) -> Option<String> {
        let at = authority.rfind('@');
        let host = parse_host(scheme, at.map_or(authority, |i| &authority[i + 1..]))?;
        if let Some(i) = at {
            let userinfo = &authority[..i];
            if !valid_userinfo(userinfo) {
                return None;
            }
            match userinfo.split_once(':') {
                None => {
                    unescape(userinfo, Mode::UserPassword)?;
                }
                Some((user, password)) => {
                    unescape(user, Mode::UserPassword)?;
                    unescape(password, Mode::UserPassword)?;
                }
            }
        }
        Some(host)
    }

    /// Go `parseHost` (with `urlstrictcolons=1`, Go 1.26+'s default).
    fn parse_host(scheme: &str, host: &str) -> Option<String> {
        if let Some(open) = host.rfind('[') {
            if open > 0 {
                return None; // "invalid IP-literal"
            }
            let close = host.rfind(']')?;
            let colon_port = &host[close + 1..];
            if !valid_optional_port(colon_port) {
                return None;
            }
            let colon_port = unescape(colon_port, Mode::Host)?;
            let hostname = &host[1..close];
            let unescaped = match hostname.find("%25") {
                Some(z) => {
                    let mut h = unescape(&hostname[..z], Mode::Host)?;
                    h.push_str(&unescape(&hostname[z..], Mode::Zone)?);
                    h
                }
                None => unescape(hostname, Mode::Host)?,
            };
            let addr = parse_addr(&unescaped)?;
            if matches!(addr, Addr::V4(_)) {
                return None;
            }
            return Some(format!("[{unescaped}]{colon_port}"));
        }
        if let Some(first) = host.find(':') {
            let last = host.rfind(':').unwrap_or(first);
            let i = if last != first && !(scheme == "http" || scheme == "https") {
                last
            } else {
                first
            };
            if !valid_optional_port(&host[i..]) {
                return None;
            }
        }
        unescape(host, Mode::Host)
    }

    /// Go `validOptionalPort`: empty or `:` followed by digits.
    fn valid_optional_port(port: &str) -> bool {
        if port.is_empty() {
            return true;
        }
        match port.strip_prefix(':') {
            Some(digits) => digits.bytes().all(|b| b.is_ascii_digit()),
            None => false,
        }
    }

    /// Go `validUserinfo`.
    fn valid_userinfo(s: &str) -> bool {
        s.chars().all(|r| {
            r.is_ascii_alphanumeric()
                || matches!(
                    r,
                    '-' | '.'
                        | '_'
                        | ':'
                        | '~'
                        | '!'
                        | '$'
                        | '&'
                        | '\''
                        | '('
                        | ')'
                        | '*'
                        | '+'
                        | ','
                        | ';'
                        | '='
                        | '%'
                        | '@'
                )
        })
    }

    /// Go `URL.Hostname()`: the host without a valid port and without IPv6 brackets.
    pub(super) fn hostname(host: &str) -> &str {
        let mut h = host;
        if let Some(colon) = h.rfind(':')
            && valid_optional_port(&h[colon..])
        {
            h = &h[..colon];
        }
        if h.len() >= 2 && h.starts_with('[') && h.ends_with(']') {
            h = &h[1..h.len() - 1];
        }
        h
    }

    /// Go `shouldEscape(c, mode)` for the modes above.
    fn should_escape(c: u8, mode: Mode) -> bool {
        if c.is_ascii_alphanumeric() {
            return false;
        }
        if matches!(mode, Mode::Host | Mode::Zone)
            && matches!(
                c,
                b'!' | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'['
                    | b']'
                    | b'<'
                    | b'>'
                    | b'"'
            )
        {
            return false;
        }
        match c {
            b'-' | b'_' | b'.' | b'~' => return false,
            b'$' | b'&' | b'+' | b',' | b'/' | b':' | b';' | b'=' | b'?' | b'@' => match mode {
                Mode::Path => return c == b'?',
                Mode::UserPassword => return matches!(c, b'@' | b'/' | b'?' | b':'),
                Mode::Fragment => return false,
                Mode::Host | Mode::Zone => {}
            },
            _ => {}
        }
        if mode == Mode::Fragment && matches!(c, b'!' | b'(' | b')' | b'*') {
            return false;
        }
        true
    }

    fn unhex(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }

    /// Go `unescape(s, mode)`; `None` on Go's `EscapeError`/`InvalidHostError`. Bytes that are
    /// not UTF-8 after decoding become U+FFFD.
    fn unescape(s: &str, mode: Mode) -> Option<String> {
        unescape_bytes(s, mode).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// Go `unescape(s, encodePath)` (how `URL.Path` is decoded); `None` on an invalid escape.
    pub(crate) fn unescape_path(s: &str) -> Option<Vec<u8>> {
        unescape_bytes(s, Mode::Path)
    }

    /// Go `escape(s, encodePath)`, upper-case hex.
    pub(crate) fn escape_path(s: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut out = String::with_capacity(s.len());
        for &c in s {
            if should_escape(c, Mode::Path) {
                out.push('%');
                out.push(char::from(HEX[usize::from(c >> 4)]));
                out.push(char::from(HEX[usize::from(c & 15)]));
            } else {
                out.push(char::from(c));
            }
        }
        out
    }

    /// Go `validEncoded(s, encodePath)`: `s` holds no byte that escaping would change.
    pub(crate) fn valid_encoded_path(s: &str) -> bool {
        s.bytes().all(|c| {
            matches!(
                c,
                b'!' | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
                    | b'['
                    | b']'
                    | b'%'
            ) || !should_escape(c, Mode::Path)
        })
    }

    fn unescape_bytes(s: &str, mode: Mode) -> Option<Vec<u8>> {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'%' => {
                    let hi = b.get(i + 1).copied().and_then(unhex)?;
                    let lo = b.get(i + 2).copied().and_then(unhex)?;
                    if mode == Mode::Host && hi < 8 && &s[i..i + 3] != "%25" {
                        return None;
                    }
                    let v = (hi << 4) | lo;
                    if mode == Mode::Zone
                        && &s[i..i + 3] != "%25"
                        && v != b' '
                        && should_escape(v, Mode::Host)
                    {
                        return None;
                    }
                    out.push(v);
                    i += 3;
                }
                c => {
                    if matches!(mode, Mode::Host | Mode::Zone) && c < 0x80 && should_escape(c, mode)
                    {
                        return None;
                    }
                    out.push(c);
                    i += 1;
                }
            }
        }
        Some(out)
    }

    /// A Go `netip.Addr`, as far as the guard reads it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Addr {
        V4(Ipv4Addr),
        /// The address and whether it carried a zone.
        V6(Ipv6Addr, bool),
    }

    impl Addr {
        /// Go `Addr.IsLoopback` (an IPv4-mapped address is unmapped first).
        pub(super) fn is_loopback(self) -> bool {
            match self {
                Addr::V4(a) => IpAddr::V4(a).is_loopback(),
                Addr::V6(a, _) => match a.to_ipv4_mapped() {
                    Some(v4) => v4.is_loopback(),
                    None => a.is_loopback(),
                },
            }
        }

        /// Go `Addr.IsUnspecified` (exact comparison: a zone or a mapped address is not).
        pub(super) fn is_unspecified(self) -> bool {
            match self {
                Addr::V4(a) => a.is_unspecified(),
                Addr::V6(a, zoned) => !zoned && a.is_unspecified(),
            }
        }
    }

    /// Go `netip.ParseAddr`; `None` on error.
    pub(super) fn parse_addr(s: &str) -> Option<Addr> {
        for c in s.bytes() {
            match c {
                b'.' => return s.parse::<Ipv4Addr>().ok().map(Addr::V4),
                b':' => {
                    let (addr, zoned) = match s.split_once('%') {
                        Some((_, "")) => return None,
                        Some((a, _)) => (a, true),
                        None => (s, false),
                    };
                    return addr.parse::<Ipv6Addr>().ok().map(|a| Addr::V6(a, zoned));
                }
                b'%' => return None,
                _ => {}
            }
        }
        None
    }

    #[cfg(test)]
    #[allow(clippy::unwrap_used)]
    mod tests {
        use super::*;

        /// Hosts and errors of Go 1.27 `url.Parse` on the inputs the guard sees.
        #[test]
        fn parse_matches_go() {
            let cases: &[(&str, Option<(&str, &str)>)] = &[
                ("http://localhost:8080", Some(("http", "localhost:8080"))),
                ("HTTP://LocalHost:80/", Some(("http", "LocalHost:80"))),
                (
                    "https://ui.example.com/app?x=1",
                    Some(("https", "ui.example.com")),
                ),
                ("http://user:pw@host:1", Some(("http", "host:1"))),
                ("http://[::1]:8080", Some(("http", "[::1]:8080"))),
                ("http://[fe80::1%25en0]", Some(("http", "[fe80::1%en0]"))),
                ("http://ex%C3%A4mple.com", Some(("http", "exämple.com"))),
                ("mailto:x@y", Some(("mailto", ""))),
                ("http:///path", Some(("http", ""))),
                ("://nonsense", None),
                ("http://a:b:80", None),
                ("http://[1.2.3.4]", None),
                ("http://x[::1]", None),
                ("http://host:port", None),
                ("http://%41.com", None),
                ("http://a b", None),
                ("http://x/%zz", None),
                ("http://x/#%zz", None),
                ("http://x\n", None),
                ("http://bad user@x", None),
            ];
            for (raw, want) in cases {
                let got = parse(raw).map(|u| (u.scheme, u.host));
                let want = want.map(|(s, h)| (s.to_owned(), h.to_owned()));
                assert_eq!(got, want, "url.Parse({raw:?})");
            }
        }

        #[test]
        fn hostname_matches_go() {
            for (host, want) in [
                ("localhost:8080", "localhost"),
                ("[::1]:80", "::1"),
                ("[::1]", "::1"),
                ("example.com", "example.com"),
                ("example.com:", "example.com"),
            ] {
                assert_eq!(hostname(host), want, "Hostname({host:?})");
            }
        }

        #[test]
        fn parse_addr_matches_netip() {
            assert!(parse_addr("127.0.0.2").unwrap().is_loopback());
            assert!(parse_addr("::1").unwrap().is_loopback());
            assert!(parse_addr("::1%lo0").unwrap().is_loopback());
            assert!(parse_addr("::ffff:127.0.0.1").unwrap().is_loopback());
            assert!(parse_addr("0.0.0.0").unwrap().is_unspecified());
            assert!(parse_addr("::").unwrap().is_unspecified());
            assert!(!parse_addr("::%eth0").unwrap().is_unspecified());
            assert!(!parse_addr("::ffff:0.0.0.0").unwrap().is_unspecified());
            assert_eq!(parse_addr("localhost"), None);
            assert_eq!(parse_addr("01.2.3.4"), None);
            assert_eq!(parse_addr("fe80::1%"), None);
            assert_eq!(parse_addr("%x"), None);
        }
    }
}
