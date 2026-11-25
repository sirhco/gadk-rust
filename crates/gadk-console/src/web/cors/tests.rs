// Copyright 2026 Chris Olson
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

//! Tests of `--api-origin` and the CORS layer of the JSON API.

use std::sync::Arc;

use axum::body::Body;
use http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_MAX_AGE, CONTENT_TYPE, HOST, ORIGIN, VARY,
};
use http::{HeaderMap, Method, Request, StatusCode};

use super::is_origin;
use crate::web::AppState;
use crate::web::testutil::{FakeSource, PORT, send, state};
use crate::{Cli, ConsoleConfig, ConsoleError, parse_cli};

const APP: &str = "http://localhost:5173";

fn with_origins(origins: &[&str]) -> AppState {
    AppState {
        api_origins: origins.iter().map(|o| (*o).to_owned()).collect(),
        ..state(Arc::new(FakeSource::new()), None)
    }
}

/// `method path` with the guard's Host and the given `Origin` (None: no header).
async fn request(
    st: AppState,
    method: Method,
    path: &str,
    origin: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, format!("127.0.0.1:{PORT}"));
    if let Some(origin) = origin {
        req = req.header(ORIGIN, origin);
    }
    send(st, req.body(Body::empty()).unwrap()).await
}

fn no_cors(headers: &HeaderMap) -> bool {
    !headers.contains_key(ACCESS_CONTROL_ALLOW_ORIGIN)
        && !headers.contains_key(ACCESS_CONTROL_ALLOW_METHODS)
        && !headers.contains_key(ACCESS_CONTROL_MAX_AGE)
        && !headers.contains_key(ACCESS_CONTROL_ALLOW_CREDENTIALS)
}

#[test]
fn origins_must_be_exact_browser_origins() {
    for ok in [
        "http://localhost:5173",
        "http://localhost",
        "https://localhost",
        "http://127.0.0.1:3000",
        "https://app.example.com",
        "https://my-app.example.com:8443",
        "http://[::1]:5173",
        "http://[::1]",
        "http://localhost:443",
        "https://localhost:80",
    ] {
        assert!(is_origin(ok), "{ok}");
    }
    for bad in [
        "",
        "localhost:5173",
        "ftp://localhost",
        "HTTP://localhost",
        "http://LOCALHOST:5173",
        "http://localhost:5173/",
        "http://localhost/app",
        "http://localhost?x=1",
        "http://localhost#x",
        "http://user@localhost",
        "http://localhost:80",
        "https://localhost:443",
        "http://localhost:",
        "http://localhost:05173",
        "http://localhost:65536",
        "http://localhost:x",
        "http://",
        "http://.localhost",
        "http://local..host",
        "http://-app.example",
        "http://app-.example",
        "http://app_1.example",
        "http://256.0.0.1",
        "http://[::1",
        "http://[::g]",
        "http://[::1]x",
        "http://[FE80::1]",
        "http://::1",
        "*",
        "null",
    ] {
        assert!(!is_origin(bad), "{bad}");
    }
}

#[tokio::test]
async fn an_allowed_origin_gets_its_allow_origin_and_vary() {
    let (status, headers, _) =
        request(with_origins(&[APP]), Method::GET, "/api/v1/nope", Some(APP)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "errors carry CORS too");
    assert_eq!(headers[CONTENT_TYPE], "application/json");
    assert_eq!(headers[ACCESS_CONTROL_ALLOW_ORIGIN], APP);
    assert_eq!(headers.get_all(VARY).iter().collect::<Vec<_>>(), ["Origin"]);
    assert!(!headers.contains_key(ACCESS_CONTROL_ALLOW_CREDENTIALS));
    assert!(!headers.contains_key(ACCESS_CONTROL_ALLOW_METHODS));
}

#[tokio::test]
async fn other_origins_or_none_get_only_vary() {
    let st = with_origins(&[APP, "https://app.example.com"]);
    for origin in [
        Some("http://localhost:5174"),
        Some("http://LOCALHOST:5173"),
        Some("http://localhost:5173/"),
        Some("null"),
        None,
    ] {
        let (_, headers, _) = request(st.clone(), Method::GET, "/api/v1/nope", origin).await;
        assert!(no_cors(&headers), "{origin:?}");
        assert_eq!(headers[VARY], "Origin", "{origin:?}");
    }
    let (_, headers, _) = request(
        st,
        Method::GET,
        "/api/v1/nope",
        Some("https://app.example.com"),
    )
    .await;
    assert_eq!(
        headers[ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://app.example.com"
    );
}

#[tokio::test]
async fn without_an_allow_list_there_are_no_cors_headers() {
    let (status, headers, _) =
        request(with_origins(&[]), Method::GET, "/api/v1/nope", Some(APP)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(no_cors(&headers));
    assert!(!headers.contains_key(VARY));
}

#[tokio::test]
async fn a_preflight_from_an_allowed_origin_is_204_with_the_cors_headers() {
    let (status, headers, body) = request(
        with_origins(&[APP]),
        Method::OPTIONS,
        "/api/v1/nope",
        Some(APP),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty(), "{body}");
    assert_eq!(headers[ACCESS_CONTROL_ALLOW_ORIGIN], APP);
    assert_eq!(headers[VARY], "Origin");
    assert_eq!(headers[ACCESS_CONTROL_ALLOW_METHODS], "GET");
    assert_eq!(headers[ACCESS_CONTROL_MAX_AGE], "600");
    assert!(!headers.contains_key(ACCESS_CONTROL_ALLOW_CREDENTIALS));
}

#[tokio::test]
async fn a_preflight_from_another_origin_or_none_is_204_without_cors_headers() {
    for (origins, origin) in [
        (&[APP][..], Some("http://localhost:5174")),
        (&[APP][..], None),
        (&[][..], Some(APP)),
    ] {
        let (status, headers, _) =
            request(with_origins(origins), Method::OPTIONS, "/api", origin).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{origins:?} {origin:?}");
        assert!(no_cors(&headers), "{origins:?} {origin:?}");
    }
}

#[tokio::test]
async fn page_routes_never_get_cors_headers() {
    for (method, path) in [
        (Method::GET, "/traces"),
        (Method::GET, "/nope"),
        (Method::GET, "/apix"),
        (Method::OPTIONS, "/traces"),
    ] {
        let (_, headers, _) = request(with_origins(&[APP]), method.clone(), path, Some(APP)).await;
        assert!(no_cors(&headers), "{method} {path}");
        assert!(!headers.contains_key(VARY), "{method} {path}");
    }
}

#[tokio::test]
async fn the_host_guard_comes_before_cors() {
    let req = Request::get("/api/v1/nope")
        .header(HOST, "evil.example:8765")
        .header(ORIGIN, APP)
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(with_origins(&[APP]), req).await;
    assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
    assert!(no_cors(&headers));
}

#[test]
fn parse_cli_reads_repeated_api_origins() {
    let cli = parse_cli([
        "gadk-console",
        "--project",
        "p",
        "--api-origin",
        APP,
        "--api-origin",
        "https://app.example.com",
    ])
    .unwrap();
    assert_eq!(cli.api_origins, [APP, "https://app.example.com"]);
    assert_eq!(
        cli,
        Cli::new(crate::parse_args(["gadk-console", "--project", "p"]).unwrap())
            .with_api_origin(APP)
            .with_api_origin("https://app.example.com")
    );
    let cli = parse_cli(["gadk-console", "--project", "p"]).unwrap();
    assert!(cli.api_origins.is_empty());
    let help = parse_cli(["gadk-console", "--help"])
        .unwrap_err()
        .render()
        .to_string();
    assert!(help.contains("--api-origin"), "{help}");
}

#[tokio::test]
async fn a_bad_api_origin_stops_startup_before_binding() {
    let cfg = ConsoleConfig {
        project: "p".into(),
        store: None,
        reports: None,
        port: 0,
        log_id: "gadk".into(),
    };
    let mut out = Vec::new();
    let err = crate::run_cli(
        Cli::new(cfg)
            .with_api_origin(APP)
            .with_api_origin("http://localhost:5173/"),
        &mut out,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ConsoleError::ApiOrigin { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "--api-origin: invalid origin http://localhost:5173/ (expected scheme://host[:port])"
    );
    assert!(out.is_empty(), "no URL is printed");
}
