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

//! Port of Go `server/adkrest/controllers/version_test.go`, plus the shape check against the
//! ADK-Go wire fixture `testdata/wire/main_03_version.json` (compared by key set: the values
//! are gadk's).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::body::Body;
use gadk_server::rest::controllers::{VersionApiController, VersionResponse};
use http::StatusCode;
use serde_json::{Map, Value};

/// Go `versionBody`: serves GET /version; status, Content-Type and body.
async fn version_body() -> (StatusCode, String, String) {
    let req = http::Request::builder()
        .uri("/version")
        .body(Body::empty())
        .unwrap();
    let resp = VersionApiController::new().version_handler(req).await;
    let status = resp.status();
    let content_type = resp.headers()["content-type"].to_str().unwrap().to_owned();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        content_type,
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

/// The JSON field names are the cross-implementation contract the web UI reads.
#[tokio::test]
async fn test_version_handler_field_names() {
    let (status, _, body) = version_body().await;
    assert_eq!(status, StatusCode::OK);
    let got: Map<String, Value> = serde_json::from_str(&body).unwrap();
    let mut keys: Vec<&str> = got.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["language", "language_version", "version"]);
}

// parity: adapted: gadk reports its own values (ledger header): language "rust", the gadk
// release, and the rustc release it was built with, where Go reports "go", version.Version and
// runtime.Version().
#[tokio::test]
async fn test_version_handler_values() {
    let (status, content_type, body) = version_body().await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.contains("application/json"),
        "Content-Type = {content_type:?}"
    );
    let got: VersionResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(got.language, "rust");
    assert_eq!(got.version, env!("CARGO_PKG_VERSION"));
    assert!(
        !got.language_version.is_empty(),
        "language_version is empty, want the toolchain version"
    );
    let header = gadk_platform::tracking_header();
    assert!(
        header.ends_with(&format!(" rust/{}", got.language_version)),
        "language_version {:?} is not the rustc release in {header:?}",
        got.language_version
    );
}

/// Same status, headers and key order as ADK-Go's `main_03_version` (its values are Go's).
#[tokio::test]
async fn version_matches_go_wire_fixture_shape() {
    let path = format!(
        "{}/testdata/wire/main_03_version.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let f: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let (status, content_type, body) = version_body().await;
    assert_eq!(u64::from(status.as_u16()), f["response"]["status"]);
    assert_eq!(content_type, f["response"]["headers"][0][1]);
    let want_body = f["response"]["body"].as_str().unwrap();
    // The key set (Go's compare mode `json_key_set`) and Go's field order.
    let keys = |s: &str| -> Vec<String> {
        let mut k: Vec<String> = serde_json::from_str::<Map<String, Value>>(s)
            .unwrap()
            .keys()
            .cloned()
            .collect();
        k.sort();
        k
    };
    assert_eq!(keys(&body), keys(want_body));
    let order = |s: &str| -> Vec<usize> {
        ["\"version\"", "\"language\"", "\"language_version\""]
            .iter()
            .map(|k| s.find(k).unwrap())
            .collect()
    };
    assert!(order(want_body).is_sorted());
    assert!(order(&body).is_sorted(), "key order of {body:?}");
    assert!(body.ends_with('\n'));
}
