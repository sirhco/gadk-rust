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

//! Ports of ADK-Go `internal/originguard/originguard_test.go`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::response::IntoResponse;
use http::request::Parts;

use crate::rest::originguard::{
    Config, GuardError, LocalAddr, MAX_ORIGIN_LEN, Policy, normalize_origin, strip_port,
};
use crate::rest::routing::{Request, handler};

/// Go `loopbackBind`: the config of a server bound to 127.0.0.1, which arms the rebinding
/// check.
fn loopback_bind() -> Config {
    config("127.0.0.1", &[])
}

fn config(bind_host: &str, origins: &[&str]) -> Config {
    Config {
        bind_host: bind_host.to_owned(),
        allowed_origins: origins.iter().map(|o| (*o).to_owned()).collect(),
    }
}

/// Go `on`: a request to `/list-apps` as it arrives on a connection accepted at `local_addr`
/// (port 8080) with Host `host`; an empty `local_addr` leaves the accepted address unset.
fn on_request(local_addr: &str, host: &str) -> Request {
    let mut b = http::Request::builder().method("GET").uri("/list-apps");
    if !host.is_empty() {
        b = b.header(http::header::HOST, host);
    }
    let mut r = b.body(Body::empty()).unwrap();
    if !local_addr.is_empty() {
        let ip: IpAddr = local_addr.parse().unwrap();
        r.extensions_mut()
            .insert(ConnectInfo(LocalAddr(SocketAddr::new(ip, 8080))));
    }
    r
}

fn on(local_addr: &str, host: &str) -> Parts {
    on_request(local_addr, host).into_parts().0
}

/// A `TestCheckOriginHeader` case: name, allowed origins, accepted address, Host, Origin, want.
type OriginCase = (
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
    &'static str,
    Result<(), GuardError>,
);

/// A `TestCheckForwardedHeaders` case: name, headers, want.
type ForwardedCase = (
    &'static str,
    &'static [(&'static str, &'static str)],
    Result<(), GuardError>,
);

fn set(r: &mut Parts, name: &'static str, value: &str) {
    r.headers.insert(name, value.parse().unwrap());
}

#[test]
fn test_check_dns_rebinding() {
    struct Case {
        name: &'static str,
        bind_host: &'static str,
        origins: &'static [&'static str],
        host: &'static str,
        want: Result<(), GuardError>,
    }
    const fn case(
        name: &'static str,
        bind_host: &'static str,
        origins: &'static [&'static str],
        host: &'static str,
        want: Result<(), GuardError>,
    ) -> Case {
        Case {
            name,
            bind_host,
            origins,
            host,
            want,
        }
    }
    use GuardError::HostNotAllowed;
    let cases = [
        // The attack from issue #1154: a page whose DNS flips to 127.0.0.1 sends no Origin on a
        // same-origin GET, leaving Host as the only tell.
        case(
            "rebound host",
            "127.0.0.1",
            &[],
            "evil.com:8080",
            Err(HostNotAllowed),
        ),
        case(
            "rebound host on an IPv6 loopback bind",
            "::1",
            &[],
            "evil.com:8080",
            Err(HostNotAllowed),
        ),
        case(
            "rebound host on a bind named localhost",
            "localhost",
            &[],
            "evil.com:8080",
            Err(HostNotAllowed),
        ),
        case("loopback host", "127.0.0.1", &[], "127.0.0.1:8080", Ok(())),
        case("localhost", "127.0.0.1", &[], "localhost:8080", Ok(())),
        case(
            "LocalHost with a root dot",
            "127.0.0.1",
            &[],
            "LocalHost.:8080",
            Ok(()),
        ),
        case(
            "any host on a routable bind",
            "0.0.0.0",
            &[],
            "adk.example.com",
            Ok(()),
        ),
        case("undeclared bind", "", &[], "adk.example.com", Ok(())),
        case(
            "configured host",
            "127.0.0.1",
            &["http://adk.internal"],
            "adk.internal",
            Ok(()),
        ),
        case(
            "unconfigured host with other origins configured",
            "127.0.0.1",
            &["http://adk.internal"],
            "evil.com",
            Err(HostNotAllowed),
        ),
        case(
            "wildcard turns the check off",
            "127.0.0.1",
            &["*"],
            "evil.com",
            Ok(()),
        ),
        case(
            "comma-separated host list",
            "127.0.0.1",
            &[],
            "localhost:8080,evil.com",
            Err(HostNotAllowed),
        ),
        case(
            "comma-separated list of allowed hosts",
            "127.0.0.1",
            &["http://adk.internal"],
            "adk.internal,evil.com",
            Err(HostNotAllowed),
        ),
        case("empty host", "127.0.0.1", &[], "", Ok(())),
        case(
            "loopback address as a subdomain label",
            "127.0.0.1",
            &[],
            "127.0.0.1:8080.evil.com",
            Err(HostNotAllowed),
        ),
        case(
            "loopback name as a subdomain label",
            "127.0.0.1",
            &[],
            "localhost.evil.com",
            Err(HostNotAllowed),
        ),
        case("bracketed IPv6 loopback", "::1", &[], "[::1]:8080", Ok(())),
    ];
    for tc in cases {
        // The connection arrives over loopback in every case, so the declared bind host is the
        // only thing that can arm the check.
        let got = Policy::new(&config(tc.bind_host, tc.origins)).check(&on("127.0.0.1", tc.host));
        assert_eq!(got, tc.want, "{}: Check()", tc.name);
    }
}

#[test]
fn test_check_origin_header() {
    use GuardError::OriginNotAllowed;
    // (name, origins, local, host, origin, want)
    let cases: [OriginCase; 11] = [
        (
            "absent origin",
            &[],
            "127.0.0.1",
            "localhost:8080",
            "",
            Ok(()),
        ),
        (
            "same origin",
            &[],
            "127.0.0.1",
            "localhost:8080",
            "http://localhost:8080",
            Ok(()),
        ),
        (
            "cross origin with nothing configured",
            &[],
            "127.0.0.1",
            "localhost:8080",
            "http://localhost:4200",
            Err(OriginNotAllowed),
        ),
        (
            "cross origin that is configured",
            &["http://localhost:4200"],
            "127.0.0.1",
            "localhost:8080",
            "http://localhost:4200",
            Ok(()),
        ),
        (
            "bare host:port configured is read as http",
            &["localhost:4200"],
            "127.0.0.1",
            "localhost:8080",
            "http://localhost:4200",
            Ok(()),
        ),
        (
            "configured origin with a mixed-case host",
            &["http://LocalHost:4200"],
            "127.0.0.1",
            "localhost:8080",
            "http://localhost:4200",
            Ok(()),
        ),
        (
            "mixed-case host in a same-origin request",
            &[],
            "127.0.0.1",
            "LocalHost:8080",
            "http://localhost:8080",
            Ok(()),
        ),
        (
            "configured origin over https does not match http",
            &["https://localhost:4200"],
            "127.0.0.1",
            "localhost:8080",
            "http://localhost:4200",
            Err(OriginNotAllowed),
        ),
        (
            "wildcard allows any origin",
            &["*"],
            "127.0.0.1",
            "localhost:8080",
            "http://evil.com",
            Ok(()),
        ),
        (
            "cross origin on a routable connection",
            &[],
            "192.168.1.5",
            "adk.example.com",
            "http://evil.com",
            Err(OriginNotAllowed),
        ),
        (
            "same origin on a routable connection",
            &[],
            "192.168.1.5",
            "adk.example.com",
            "http://adk.example.com",
            Ok(()),
        ),
    ];
    for (name, origins, local, host, origin, want) in cases {
        let mut r = on(local, host);
        if !origin.is_empty() {
            set(&mut r, "origin", origin);
        }
        let p = Policy::new(&config("", origins));
        assert_eq!(p.check(&r), want, "{name}: Check()");
        // CheckOrigin is the WebSocket upgrader's view of the same question.
        assert_eq!(p.check_origin(&r), want.is_ok(), "{name}: CheckOrigin()");
    }
}

#[test]
fn test_check_origin_rebinding_without_host_guard() {
    let mut r = on("127.0.0.1", "evil.com:8080");
    set(&mut r, "origin", "http://evil.com:8080");

    assert!(
        !Policy::new(&Config::default()).check_origin(&r),
        "CheckOrigin() = true for a rebound origin, want false"
    );
    // Configuring the origin is the operator saying they meant it.
    assert!(
        Policy::new(&config("", &["http://evil.com:8080"])).check_origin(&r),
        "CheckOrigin() = false for a configured origin, want true"
    );
}

#[test]
fn test_check_forwarded_headers() {
    use GuardError::OriginNotAllowed;
    let cases: [ForwardedCase; 7] = [
        (
            "same origin behind an X-Forwarded proxy",
            &[
                ("origin", "https://adk.internal"),
                ("x-forwarded-host", "adk.internal"),
                ("x-forwarded-proto", "https"),
            ],
            Ok(()),
        ),
        (
            "same origin behind an RFC 7239 proxy",
            &[
                ("origin", "https://adk.internal"),
                (
                    "forwarded",
                    "for=192.0.2.1;host=\"adk.internal\";proto=https",
                ),
            ],
            Ok(()),
        ),
        (
            "forwarded headers naming a different origin",
            &[
                ("origin", "https://evil.com"),
                ("x-forwarded-host", "adk.internal"),
            ],
            Err(OriginNotAllowed),
        ),
        (
            "Forwarded with no proto falls back",
            &[
                ("origin", "https://adk.internal"),
                ("forwarded", "for=192.0.2.1"),
                ("x-forwarded-host", "adk.internal"),
                ("x-forwarded-proto", "https"),
            ],
            Ok(()),
        ),
        (
            "ws proto compares as http",
            &[
                ("origin", "http://adk.internal"),
                ("x-forwarded-host", "adk.internal"),
                ("x-forwarded-proto", "ws"),
            ],
            Ok(()),
        ),
        (
            "wss proto compares as https",
            &[
                ("origin", "https://adk.internal"),
                ("x-forwarded-host", "adk.internal"),
                ("x-forwarded-proto", "wss"),
            ],
            Ok(()),
        ),
        (
            "first element of a forwarded list wins",
            &[
                ("origin", "https://adk.internal"),
                ("x-forwarded-host", "adk.internal, internal-lb"),
                ("x-forwarded-proto", "https, http"),
            ],
            Ok(()),
        ),
    ];
    for (name, headers, want) in cases {
        // No origins configured: the same-origin comparison is the only thing that can accept
        // these. Host is the internal address the proxy dialed.
        let mut r = on("192.168.1.5", "10.0.0.7:8080");
        for (h, v) in headers {
            set(&mut r, h, v);
        }
        assert_eq!(
            Policy::new(&Config::default()).check(&r),
            want,
            "{name}: Check()"
        );
    }
}

#[test]
fn test_loopback_bind_behind_local_proxy() {
    let mut r = on("127.0.0.1", "adk.internal");
    set(&mut r, "origin", "https://adk.internal");
    set(&mut r, "x-forwarded-host", "adk.internal");
    set(&mut r, "x-forwarded-proto", "https");

    assert_eq!(
        Policy::new(&config("127.0.0.1", &["https://adk.internal"])).check(&r),
        Ok(())
    );
    assert_eq!(
        Policy::new(&loopback_bind()).check(&r),
        Err(GuardError::HostNotAllowed),
        "Check() without the proxy origin"
    );
}

#[test]
fn test_forwarded_headers_cannot_defeat_host_guard() {
    for header in ["X-Forwarded-Host", "Forwarded"] {
        let mut r = on("127.0.0.1", "evil.com:8080");
        if header == "Forwarded" {
            set(&mut r, "forwarded", "host=localhost;proto=http");
        } else {
            set(&mut r, "x-forwarded-host", "localhost");
        }
        assert_eq!(
            Policy::new(&loopback_bind()).check(&r),
            Err(GuardError::HostNotAllowed),
            "{header}: Check()"
        );
    }
}

#[test]
fn test_declared_bind_overrides_the_connection() {
    use GuardError::{HostNotAllowed, OriginNotAllowed};
    let cases = [
        (
            "routable bind, loopback connection",
            "192.168.1.5",
            "127.0.0.1",
            Ok(()),
        ),
        (
            "wildcard bind, loopback connection",
            "0.0.0.0",
            "127.0.0.1",
            Err(OriginNotAllowed),
        ),
        (
            "wildcard bind written as a bare port, loopback connection",
            ":8080",
            "127.0.0.1",
            Err(OriginNotAllowed),
        ),
        (
            "IPv6 wildcard bind, loopback connection",
            "[::]",
            "127.0.0.1",
            Err(OriginNotAllowed),
        ),
        (
            "wildcard bind, routable connection",
            "0.0.0.0",
            "192.168.1.5",
            Ok(()),
        ),
        (
            "loopback bind, routable connection",
            "127.0.0.1",
            "192.168.1.5",
            Err(HostNotAllowed),
        ),
        (
            "nothing declared, loopback connection",
            "",
            "127.0.0.1",
            Err(OriginNotAllowed),
        ),
        (
            "nothing declared, routable connection",
            "",
            "192.168.1.5",
            Ok(()),
        ),
    ];
    for (name, bind_host, local, want) in cases {
        // A page claiming a non-loopback origin, same-origin by the browser's reckoning, which
        // is what rebinding produces.
        let mut r = on(local, "adk.example.com");
        set(&mut r, "origin", "http://adk.example.com");
        assert_eq!(
            Policy::new(&config(bind_host, &[])).check(&r),
            want,
            "{name}: Check()"
        );
    }
}

#[test]
fn test_overlong_origin_is_refused_unparsed() {
    let mut r = on("192.168.1.5", "adk.example.com");
    let long = format!("http://adk.example.com/{}", "a".repeat(MAX_ORIGIN_LEN));
    set(&mut r, "origin", &long);
    assert_eq!(
        Policy::new(&Config::default()).check(&r),
        Err(GuardError::OriginNotAllowed)
    );
    // The same origin within the bound is the one this server serves.
    set(&mut r, "origin", "http://adk.example.com");
    assert_eq!(Policy::new(&Config::default()).check(&r), Ok(()));
}

// parity: adapted: gadk requests carry no TLS connection state; the request target's https scheme stands for Go's r.TLS
#[test]
fn test_check_over_tls() {
    let cases = [
        ("https origin over TLS", "https://adk.internal", Ok(())),
        (
            "http origin over TLS",
            "http://adk.internal",
            Err(GuardError::OriginNotAllowed),
        ),
    ];
    for (name, origin, want) in cases {
        let mut r = on("192.168.1.5", "adk.internal");
        r.uri = "https://adk.internal/list-apps".parse().unwrap();
        set(&mut r, "origin", origin);
        assert_eq!(
            Policy::new(&Config::default()).check(&r),
            want,
            "{name}: Check()"
        );
    }
}

#[test]
fn test_new_ignores_empty_origins() {
    let mut r = on("127.0.0.1", "evil.com:8080");
    set(&mut r, "origin", "http://evil.com:8080");
    assert!(
        !Policy::new(&config("", &["", "  "])).check_origin(&r),
        "CheckOrigin() = true for a rebound origin, want false"
    );
}

#[tokio::test]
async fn test_middleware() {
    let cases = [
        (
            "allowed request reaches the handler",
            "localhost:8080",
            200,
            "served",
        ),
        (
            "rebound request is refused",
            "evil.com:8080",
            403,
            "Forbidden: host not allowed\n",
        ),
    ];
    for (name, host, want_status, want_body) in cases {
        let next = handler(|_req: Request| async { "served".into_response() });
        let h = Arc::new(Policy::new(&loopback_bind())).middleware(next);
        let resp = h(on_request("127.0.0.1", host)).await;
        assert_eq!(resp.status().as_u16(), want_status, "{name}: status");
        if want_status == 403 {
            assert_eq!(
                resp.headers()["content-type"],
                "text/plain; charset=utf-8",
                "{name}: Content-Type"
            );
            assert_eq!(
                resp.headers()["x-content-type-options"],
                "nosniff",
                "{name}: X-Content-Type-Options"
            );
        }
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body, want_body.as_bytes(), "{name}: body");
    }
}

#[test]
fn test_normalize_origin() {
    for (addr, want) in [
        ("", ""),
        ("*", "*"),
        ("  localhost:8080  ", "http://localhost:8080"),
        ("localhost", "http://localhost"),
        ("http://localhost:8080/", "http://localhost:8080"),
        ("https://ui.example.com/app?x=1", "https://ui.example.com"),
        ("://nonsense", "://nonsense"),
    ] {
        assert_eq!(normalize_origin(addr), want, "NormalizeOrigin({addr:?})");
    }
}

#[test]
fn test_strip_port() {
    for (host, want) in [
        ("localhost:8080", "localhost"),
        ("localhost", "localhost"),
        ("[::1]:8080", "::1"),
        ("[::1]", "::1"),
        ("::1", "::1"),
        // A malformed authority comes back whole.
        ("127.0.0.1:8080.evil.com", "127.0.0.1:8080.evil.com"),
        ("[::1", "[::1"),
        ("localhost:", "localhost:"),
        ("localhost:http", "localhost:http"),
    ] {
        assert_eq!(strip_port(host), want, "stripPort({host:?})");
    }
}
