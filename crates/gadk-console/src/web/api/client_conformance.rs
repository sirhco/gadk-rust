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

//! Client conformance: every 200 body the API tests produce must also deserialize into the
//! `gadk-console-client` type of its route.

use gadk_console_client as client;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::openapi::document;

/// Deserializes a body into one client type.
type Decoder = fn(&Value) -> Result<(), String>;

/// Deserializes `body` into `T`, or the serde error.
fn decode<T: DeserializeOwned>(body: &Value) -> Result<(), String> {
    serde_json::from_value::<T>(body.clone())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The client decoder of the 200 body of the document path `path_template`, if it has one.
fn decoder(path_template: &str) -> Option<Decoder> {
    Some(match path_template {
        "/api/v1/usage/breakdown" => decode::<client::UsageBreakdownResponse>,
        "/api/v1/usage/trend" => decode::<client::UsageTrendResponse>,
        "/api/v1/usage/top-traces" => decode::<client::UsageTopTracesResponse>,
        "/api/v1/sessions" => decode::<client::SessionsResponse>,
        "/api/v1/sessions/{id}/turns" => decode::<client::SessionTurnsResponse>,
        "/api/v1/users" => decode::<client::UsersResponse>,
        "/api/v1/prompts/{name}/versions" => decode::<client::PromptVersionsResponse>,
        "/api/v1/traces/{id}/scores" => decode::<client::TraceScoresResponse>,
        "/api/v1/review/pending" => decode::<client::PendingReviewsResponse>,
        "/api/v1/openapi.json" => decode::<Value>,
        _ => return None,
    })
}

/// Deserializes the 200 `body` of the document path `path_template` into the client's type for
/// that route: the serde error, or an error for a path without a client type.
pub(super) fn decode_as_client(path_template: &str, body: &Value) -> Result<(), String> {
    match decoder(path_template) {
        Some(decode) => decode(body),
        None => Err(format!("{path_template}: no client type")),
    }
}

#[test]
fn every_document_path_has_a_client_type() {
    let doc = document();
    let paths = doc["paths"].as_object().unwrap();
    assert_eq!(paths.len(), 10);
    for path in paths.keys() {
        assert!(decoder(path).is_some(), "{path} has no client type");
    }
    assert_eq!(
        decode_as_client("/api/v1/nope", &Value::Null),
        Err("/api/v1/nope: no client type".to_owned())
    );
}

#[test]
fn a_body_the_client_cannot_read_names_the_serde_error() {
    let e = decode_as_client("/api/v1/users", &serde_json::json!({ "data": 1 })).unwrap_err();
    assert!(e.contains("invalid type"), "{e}");
}
