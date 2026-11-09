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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The service end to end over the GCS JSON API: `GcsArtifactService::new` against an
//! in-process fake of the API on 127.0.0.1 (listing, metadata, download, conditional multipart
//! upload, delete), with authentication off through `STORAGE_EMULATOR_HOST`. Runs the artifact
//! conformance suite and concurrent saves through real HTTP, and checks the stored object names
//! and metadata are ADK-Go's.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gadk_artifact::{ArtifactService, GetArtifactVersionRequest, LoadRequest, SaveRequest};
use gadk_artifact_gcs::testutil::{multipart_parts, query_param};
use gadk_artifact_gcs::{GcsArtifactService, GcsConfig};
use gadk_genai::Part;
use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest};
use gadk_platform::env::MapEnv;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

/// One stored object.
struct Object {
    data: String,
    content_type: String,
    metadata: Value,
    created: String,
}

/// Bucket name → object name → object.
type Buckets = Mutex<HashMap<String, BTreeMap<String, Object>>>;

/// The fake API's state.
#[derive(Default)]
struct Gcs {
    buckets: Buckets,
    /// The 412 responses sent to uploads whose object already existed.
    precondition_failures: AtomicUsize,
    /// When set, the first `n` listings wait for each other before they are answered.
    listing_gate: Option<ListingGate>,
}

struct ListingGate {
    n: usize,
    arrived: AtomicUsize,
    barrier: tokio::sync::Barrier,
}

impl ListingGate {
    fn new(n: usize) -> Self {
        Self {
            n,
            arrived: AtomicUsize::new(0),
            barrier: tokio::sync::Barrier::new(n),
        }
    }
}

/// A JSON API URL path segment decoded, or `None` when it is not escaped as Go's
/// `googleapi.Expand` escapes it: every byte but ALPHA / DIGIT / "-._~" as upper-case `%XX`.
fn path_unescape(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b);
            i += 1;
            continue;
        }
        let hex = bytes.get(i + 1..i + 3)?;
        if b != b'%' || !hex.iter().all(|h| matches!(h, b'0'..=b'9' | b'A'..=b'F')) {
            return None;
        }
        let decoded = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
        if decoded.is_ascii_alphanumeric() || matches!(decoded, b'-' | b'.' | b'_' | b'~') {
            return None;
        }
        out.push(decoded);
        i += 3;
    }
    String::from_utf8(out).ok()
}

fn bad_path(segment: &str) -> FakeResponse {
    FakeResponse::json_status(
        400,
        json!({"error": {"code": 400, "message": format!("badly escaped path segment {segment:?}")}})
            .to_string(),
    )
}

fn not_found() -> FakeResponse {
    FakeResponse::json_status(404, r#"{"error":{"code":404,"message":"No such object"}}"#)
}

/// The fake GCS JSON API's answer to `req`.
fn handle(gcs: &Gcs, req: &RecordedRequest) -> FakeResponse {
    let mut buckets = gcs.buckets.lock().unwrap();
    if let Some(rest) = req.path.strip_prefix("/upload/storage/v1/b/") {
        let segment = rest.trim_end_matches("/o");
        let Some(bucket) = path_unescape(segment) else {
            return bad_path(segment);
        };
        assert_eq!(query_param(req, "uploadType").as_deref(), Some("multipart"));
        assert_eq!(query_param(req, "ifGenerationMatch").as_deref(), Some("0"));
        let parts = multipart_parts(req);
        let meta: Value = serde_json::from_str(&parts[0].1).unwrap();
        let data = parts[1].1.clone();
        let name = meta["name"].as_str().unwrap().to_owned();
        let objects = buckets.entry(bucket).or_default();
        if objects.contains_key(&name) {
            gcs.precondition_failures.fetch_add(1, Ordering::SeqCst);
            return FakeResponse::json_status(
                412,
                r#"{"error":{"code":412,"message":"At least one of the pre-conditions you specified did not hold."}}"#,
            );
        }
        let object = Object {
            data,
            content_type: meta["contentType"].as_str().unwrap_or_default().to_owned(),
            metadata: meta.get("metadata").cloned().unwrap_or(json!({})),
            created: chrono::Utc::now().to_rfc3339(),
        };
        objects.insert(name.clone(), object);
        return FakeResponse::json(json!({"name": name, "generation": "1"}).to_string());
    }
    let rest = req.path.strip_prefix("/storage/v1/b/").unwrap();
    let (bucket, object) = rest.split_once("/o").unwrap();
    let Some(bucket) = path_unescape(bucket) else {
        return bad_path(bucket);
    };
    let objects = buckets.entry(bucket).or_default();
    let Some(object) = object.strip_prefix('/') else {
        let prefix = query_param(req, "prefix").unwrap_or_default();
        let items: Vec<Value> = objects
            .keys()
            .filter(|name| name.starts_with(&prefix))
            .map(|name| json!({"name": name}))
            .collect();
        return FakeResponse::json(json!({"items": items}).to_string());
    };
    let Some(name) = path_unescape(object) else {
        return bad_path(object);
    };
    match (req.method.as_str(), query_param(req, "alt").as_deref()) {
        ("DELETE", _) => match objects.remove(&name) {
            Some(_) => FakeResponse::new(204),
            None => not_found(),
        },
        ("GET", Some("media")) => match objects.get(&name) {
            Some(o) => FakeResponse::new(200).chunk(o.data.clone()),
            None => not_found(),
        },
        _ => match objects.get(&name) {
            Some(o) => FakeResponse::json(
                json!({
                    "name": name,
                    "contentType": o.content_type,
                    "metadata": o.metadata,
                    "timeCreated": o.created,
                    "mediaLink": format!("https://storage.googleapis.com/download/{name}"),
                })
                .to_string(),
            ),
            None => not_found(),
        },
    }
}

/// Whether `req` lists a bucket's objects.
fn is_listing(req: &RecordedRequest) -> bool {
    req.method == "GET"
        && req
            .path
            .strip_prefix("/storage/v1/b/")
            .is_some_and(|rest| rest.ends_with("/o"))
}

/// The fake API on 127.0.0.1 and its state.
fn fake_gcs() -> (FakeServer, Arc<Gcs>) {
    fake_gcs_with(Gcs::default())
}

fn fake_gcs_with(gcs: Gcs) -> (FakeServer, Arc<Gcs>) {
    let gcs = Arc::new(gcs);
    let state = gcs.clone();
    let server = FakeServer::start_async(move |req| {
        let state = state.clone();
        async move {
            if let Some(gate) = &state.listing_gate
                && is_listing(&req)
                && gate.arrived.fetch_add(1, Ordering::SeqCst) < gate.n
            {
                gate.barrier.wait().await;
            }
            handle(&state, &req)
        }
    });
    (server, gcs)
}

/// A service for `bucket` on `server`, authentication off.
fn service(server: &FakeServer, bucket: &str) -> GcsArtifactService {
    let env = MapEnv(HashMap::from([(
        "STORAGE_EMULATOR_HOST".to_owned(),
        server.addr().to_string(),
    )]));
    let config = GcsConfig {
        env: Arc::new(env),
        ..Default::default()
    };
    GcsArtifactService::new(bucket, config).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rest_service_passes_conformance_suite() {
    let (server, _) = fake_gcs();
    let server = Arc::new(server);
    let n = AtomicUsize::new(0);
    // A fresh bucket per scenario.
    gadk_artifact::testsuite::run(|| {
        let bucket = format!("bkt-{}", n.fetch_add(1, Ordering::SeqCst));
        let svc = service(&server, &bucket);
        async move { Arc::new(svc) as Arc<dyn ArtifactService> }
    })
    .await;
}

// The first round of listings is held until every saver has listed, so all of them pick
// version 1 and all but one lose the conditional upload with a 412 before retrying.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_saves_over_http_get_distinct_versions() {
    const SAVERS: usize = 5;
    let (server, gcs) = fake_gcs_with(Gcs {
        listing_gate: Some(ListingGate::new(SAVERS)),
        ..Gcs::default()
    });
    let svc = Arc::new(service(&server, "bkt"));
    let tasks: Vec<_> = (0..SAVERS)
        .map(|_| {
            let svc = svc.clone();
            tokio::spawn(async move {
                svc.save(SaveRequest {
                    app_name: "app".into(),
                    user_id: "user".into(),
                    session_id: "session".into(),
                    file_name: "file".into(),
                    part: Some(Part::from_text("data")),
                    ..Default::default()
                })
                .await
            })
        })
        .collect();
    let mut versions = Vec::new();
    for task in tasks {
        let saved = tokio::time::timeout(std::time::Duration::from_secs(60), task)
            .await
            .expect("saves finish");
        versions.push(saved.unwrap().unwrap().version);
    }
    versions.sort_unstable();
    assert_eq!(versions, (1..=SAVERS as i64).collect::<Vec<_>>());
    assert!(
        gcs.precondition_failures.load(Ordering::SeqCst) >= SAVERS - 1,
        "412s: {}",
        gcs.precondition_failures.load(Ordering::SeqCst)
    );
    let names: Vec<String> = gcs.buckets.lock().unwrap()["bkt"].keys().cloned().collect();
    assert_eq!(
        names,
        (1..=SAVERS)
            .map(|v| format!("app/user/session/file/{v}"))
            .collect::<Vec<_>>()
    );
}

// Object names with bytes url.PathEscape keeps but the JSON API's path escape does not survive
// a save / load round trip (the fake rejects any other encoding).
#[tokio::test]
async fn reserved_bytes_in_names_round_trip() {
    let (server, gcs) = fake_gcs();
    let svc = service(&server, "bkt");
    let file_name = "user:a+b=c@d$&.txt";
    let req = SaveRequest {
        app_name: "app".into(),
        user_id: "u".into(),
        session_id: "s".into(),
        file_name: file_name.into(),
        part: Some(Part::from_text("data")),
        ..Default::default()
    };
    assert_eq!(svc.save(req).await.unwrap().version, 1);
    let loaded = svc
        .load(LoadRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: "s".into(),
            file_name: file_name.into(),
            version: 0,
        })
        .await
        .unwrap();
    assert_eq!(
        loaded.part,
        Some(Part::from_bytes("data".into(), "text/plain"))
    );
    let paths: Vec<String> = server
        .requests()
        .iter()
        .filter(|r| r.path.starts_with("/storage/v1/b/bkt/o/"))
        .map(|r| r.path.clone())
        .collect();
    assert!(!paths.is_empty());
    for path in paths {
        assert_eq!(
            path,
            "/storage/v1/b/bkt/o/app%2Fu%2Fuser%2Fuser%3Aa%2Bb%3Dc%40d%24%26.txt%2F1"
        );
    }
    assert!(gcs.buckets.lock().unwrap()["bkt"].contains_key("app/u/user/user:a+b=c@d$&.txt/1"));
}

#[tokio::test]
async fn stored_objects_are_gos() {
    let (server, gcs) = fake_gcs();
    let svc = service(&server, "my-bucket");
    for (session_id, file_name, part) in [
        ("s 1", "report 100%.txt", Part::from_text("hello")),
        (
            "s 1",
            "user:avatar.png",
            Part::from_bytes("png".into(), "image/png"),
        ),
    ] {
        svc.save(SaveRequest {
            app_name: "app/one".into(),
            user_id: "u".into(),
            session_id: session_id.into(),
            file_name: file_name.into(),
            part: Some(part),
            custom_metadata: json!({"n": 42, "ok": true, "s": "x"})
                .as_object()
                .cloned()
                .unwrap(),
            ..Default::default()
        })
        .await
        .unwrap();
    }
    {
        let buckets = gcs.buckets.lock().unwrap();
        let objects = &buckets["my-bucket"];
        let names: Vec<&String> = objects.keys().collect();
        assert_eq!(
            names,
            [
                "app/one/u/s 1/report 100%.txt/1",
                "app/one/u/user/user:avatar.png/1"
            ]
        );
        let text = &objects["app/one/u/s 1/report 100%.txt/1"];
        assert_eq!(text.content_type, "text/plain");
        assert_eq!(text.data, "hello");
        assert_eq!(text.metadata, json!({"n": "42", "ok": "true", "s": "x"}));
        assert_eq!(
            objects["app/one/u/user/user:avatar.png/1"].content_type,
            "image/png"
        );
    }
    let version = svc
        .get_artifact_version(GetArtifactVersionRequest {
            app_name: "app/one".into(),
            user_id: "u".into(),
            session_id: "s 1".into(),
            file_name: "report 100%.txt".into(),
            version: 0,
        })
        .await
        .unwrap()
        .artifact_version
        .unwrap();
    assert_eq!(
        version.canonical_uri,
        "gs://my-bucket/app/one/u/s 1/report 100%.txt/1"
    );
    assert_eq!(version.mime_type, "text/plain");
}
