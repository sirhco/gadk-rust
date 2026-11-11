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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::ConnectInfo;
use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;
use gadk_platform::{go_to_lower, go_url};
use http::StatusCode;
use http::request::Parts;
use tokio::net::TcpListener;

use super::controllers::CheckOrigin;
use super::http_error::http_error;
use crate::mux::{Handler, Request, handler};

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
            if let Ok(u) = go_url::parse(&origin) {
                let hostname = u.hostname();
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

/// Go `originguard.New(Config{BindHost, AllowedOrigins}).Middleware` as a handler wrapper.
///
/// A request the guard refuses (a rebound Host under a loopback `bind_host`, or an Origin that is
/// neither the request's own, a loopback page reaching a loopback server, nor on
/// `allowed_origins`) answers 403 `Forbidden: host not allowed` / `Forbidden: origin not allowed`
/// in `http.Error` framing; any other request reaches the wrapped handler. A single `"*"` in
/// `allowed_origins` turns both checks off. Serve with
/// `into_make_service_with_connect_info::<`[`LocalAddr`]`>()` so the guard knows the address each
/// connection was accepted on.
pub fn origin_guard_middleware(
    bind_host: &str,
    allowed_origins: &[String],
) -> Arc<dyn Fn(Handler) -> Handler + Send + Sync> {
    let policy = Arc::new(Policy::new(&Config {
        bind_host: bind_host.to_owned(),
        allowed_origins: allowed_origins.to_vec(),
    }));
    Arc::new(move |next: Handler| policy.middleware(next))
}

/// Go `originguard.NormalizeOrigin`: `addr` as an RFC 6454 origin (`scheme://host[:port]`,
/// lowercased); a bare host or host:port gets `http://`; `"*"` and `""` pass through, as does
/// anything that does not parse as a URL with a host.
pub fn normalize_origin(addr: &str) -> String {
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
        Ok(u) if !u.host.is_empty() => {
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
        .map(|u| u.hostname().to_owned())
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
    go_netip::parse_addr(&bare).is_some_and(|a| a.is_loopback())
}

/// Go `isWildcardAddr`: `host` (with or without a port) names every interface.
fn is_wildcard_addr(host: &str) -> bool {
    let bare = bare_host(host);
    if bare.is_empty() {
        return true;
    }
    go_netip::parse_addr(&bare).is_some_and(|a| a.is_unspecified())
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

/// Go `net/netip`'s `ParseAddr` with `IsLoopback`/`IsUnspecified`, as far as the guard reads
/// them (`net/url` is [`gadk_platform::go_url`]).
pub(crate) mod go_netip {
    use super::{IpAddr, Ipv4Addr, Ipv6Addr};

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
