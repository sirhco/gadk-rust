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

//! Port of ADK-Go `artifact/gcsartifact/gcs_client_test.go`: the REST client against a
//! 127.0.0.1 fake server, with authentication off (Go `option.WithoutAuthentication`) through
//! `STORAGE_EMULATOR_HOST`.

use std::collections::BTreeMap;

use bytes::Bytes;
use gadk_model::rest::test_util::{FakeResponse, FakeServer};
use pretty_assertions::assert_eq;

use crate::bucket::GcsBucket;
use crate::config::GcsConfig;
use crate::rest::RestBucket;
use crate::rest::wire_tests::env;
use crate::testutil::{multipart_parts, query_param};

/// Bucket `bucket` on `server`, authentication off.
fn bucket_on(server: &FakeServer) -> RestBucket {
    let cfg = GcsConfig {
        env: env(&[("STORAGE_EMULATOR_HOST", &server.addr().to_string())]),
        ..Default::default()
    };
    RestBucket::from_config("bucket", cfg).unwrap()
}

// parity: adapted: gadk has no writer object, so the metadata is checked in the upload's JSON part
#[tokio::test]
async fn test_gcs_writer_wrapper_set_metadata() {
    let server = FakeServer::json(r#"{"bucket":"bucket","name":"obj","generation":"1"}"#);
    let want = BTreeMap::from([("key".to_owned(), "value".to_owned())]);
    bucket_on(&server)
        .insert_if_absent("obj", "text/plain", &want, Bytes::from_static(b"data"))
        .await
        .unwrap();
    let parts = multipart_parts(&server.last_request());
    let json: serde_json::Value = serde_json::from_str(&parts[0].1).unwrap();
    assert_eq!(json["metadata"], serde_json::json!({"key": "value"}));
}

// parity: adapted: every upload is conditional, so the unconditional case checks that no other call carries ifGenerationMatch
#[tokio::test]
async fn test_object_wrapper_if_not_exist() {
    let server = FakeServer::start(|req| {
        let query = req.query.as_deref().unwrap_or_default();
        match req.method.as_str() {
            "POST" => FakeResponse::json(r#"{"bucket":"bucket","name":"obj","generation":"1"}"#),
            "DELETE" => FakeResponse::new(204),
            _ if query.starts_with("prefix=") => FakeResponse::json(r#"{"items":[]}"#),
            _ if query == "alt=media" => FakeResponse::new(200).chunk("data"),
            _ => FakeResponse::json(r#"{"name":"obj","contentType":"text/plain"}"#),
        }
    });
    let bucket = bucket_on(&server);
    bucket
        .insert_if_absent(
            "obj",
            "text/plain",
            &BTreeMap::new(),
            Bytes::from_static(b"data"),
        )
        .await
        .unwrap();
    bucket.list("o", false).await.unwrap();
    bucket.attrs("obj").await.unwrap();
    bucket.read("obj").await.unwrap();
    bucket.delete("obj").await.unwrap();

    let reqs = server.requests();
    assert_eq!(reqs.len(), 5);
    let (uploads, others): (Vec<_>, Vec<_>) = reqs
        .iter()
        .partition(|req| req.path.starts_with("/upload/"));
    for (name, reqs, want) in [("conditional", uploads, "0"), ("unconditional", others, "")] {
        assert!(!reqs.is_empty(), "{name}: no requests");
        for req in reqs {
            let got = query_param(req, "ifGenerationMatch").unwrap_or_default();
            assert_eq!(
                got,
                want,
                "{name}: ifGenerationMatch of {} {}",
                req.method,
                req.path_and_query()
            );
        }
    }
}
