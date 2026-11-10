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

//! Port of ADK-Go `artifact/gcsartifact/gcs_test.go`, against the in-memory [`FakeBucket`]
//! (Go's `fakeBucket`) with save retries that do not wait (Go `newGCSServiceForTesting`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use gadk_artifact::{
    ArtifactError, ArtifactService, ArtifactVersion, DeleteRequest, GetArtifactVersionRequest,
    LoadRequest, SaveRequest, VersionsRequest,
};
use gadk_genai::Part;
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

use crate::bucket::GcsBucket;
use crate::error::{GcsError, GoWrapped, backend_chain_any};
use crate::service::{
    MAX_SAVE_ATTEMPTS, SAVE_RETRY_MAX_DELAY, backoff_delay, build_blob_name,
    is_precondition_failed, sleep_context,
};
use crate::{FakeBucket, GcsArtifactService, is_object_not_exist, is_version_conflict};

/// Go `newGCSServiceForTesting`: the service and its fake bucket.
fn new_gcs_service_for_testing(bucket_name: &str) -> (GcsArtifactService, Arc<FakeBucket>) {
    let fake = Arc::new(FakeBucket::new());
    (
        GcsArtifactService::with_bucket_for_testing(bucket_name, fake.clone()),
        fake,
    )
}

/// Go `saveReq`: a minimal valid request for the fixed test artifact.
fn save_req() -> SaveRequest {
    SaveRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        session_id: "session".into(),
        file_name: "file".into(),
        part: Some(Part::from_bytes("data".into(), "text/plain")),
        ..Default::default()
    }
}

fn delete_req(session_id: &str, file_name: &str, version: i64) -> DeleteRequest {
    DeleteRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        session_id: session_id.into(),
        file_name: file_name.into(),
        version,
    }
}

fn get_req(session_id: &str, file_name: &str, version: i64) -> GetArtifactVersionRequest {
    GetArtifactVersionRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        session_id: session_id.into(),
        file_name: file_name.into(),
        version,
    }
}

fn versions_req() -> VersionsRequest {
    VersionsRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        session_id: "session".into(),
        file_name: "file".into(),
    }
}

/// The artifact version `get_artifact_version` returned.
async fn get_version(svc: &GcsArtifactService, req: GetArtifactVersionRequest) -> ArtifactVersion {
    svc.get_artifact_version(req)
        .await
        .unwrap()
        .artifact_version
        .unwrap()
}

#[tokio::test]
async fn test_gcs_artifact_service() {
    gadk_artifact::testsuite::run(|| async {
        Arc::new(new_gcs_service_for_testing("new").0) as Arc<dyn ArtifactService>
    })
    .await;
}

#[tokio::test]
async fn test_delete_missing_version() {
    let (svc, _) = new_gcs_service_for_testing("bucket");
    let err = svc
        .delete(delete_req("session", "file", 1))
        .await
        .unwrap_err();
    assert!(is_object_not_exist(&err), "Delete(missing version) = {err}");
    assert!(!matches!(err, ArtifactError::NotFound));
}

// parity: adapted: the "canceled" case has no counterpart (cancellation is dropping the future)
#[tokio::test]
async fn test_delete_version_errors() {
    type Hook = fn() -> GcsError;
    type Want = fn(&ArtifactError) -> bool;
    let cases: [(&str, Option<Hook>, Want); 4] = [
        ("success", None, |_| true),
        (
            "not found",
            Some(|| GcsError::ObjectNotExist),
            is_object_not_exist,
        ),
        (
            "wrapped not found",
            Some(|| GcsError::Other(Box::new(GoWrapped::new("delete", GcsError::ObjectNotExist)))),
            is_object_not_exist,
        ),
        (
            "permission denied",
            Some(|| {
                GcsError::Other(Box::new(std::io::Error::from(
                    std::io::ErrorKind::PermissionDenied,
                )))
            }),
            |err| {
                backend_chain_any(err, |e| {
                    e.downcast_ref::<std::io::Error>()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
                })
            },
        ),
    ];
    for (name, hook, want) in cases {
        let (svc, fake) = new_gcs_service_for_testing("bucket");
        svc.save(save_req()).await.unwrap();
        if let Some(hook) = hook {
            fake.set_delete_err(hook);
        }
        let got = svc.delete(delete_req("session", "file", 1)).await;
        match (hook, got) {
            (None, Ok(())) => {}
            (Some(_), Err(err)) => assert!(want(&err), "{name}: Delete() = {err}"),
            (_, got) => panic!("{name}: Delete() = {got:?}"),
        }
    }
}

#[tokio::test]
async fn test_gcs_artifact_version_fields() {
    let first_create_time = Utc.with_ymd_and_hms(2026, 8, 31, 12, 34, 56).unwrap();
    let second_create_time = first_create_time + chrono::Duration::minutes(1);
    let recreate_time = second_create_time + chrono::Duration::minutes(1);
    let at = |t: DateTime<Utc>| move || t;

    for (name, file_name, save_session_id, get_session_id, uri_prefix) in [
        (
            "session scoped",
            "file.txt",
            "session",
            "session",
            "gs://bucket/app/user/session/file.txt/",
        ),
        (
            "user scoped",
            "user:file.txt",
            "save-session",
            "irrelevant-session",
            "gs://bucket/app/user/user/user:file.txt/",
        ),
    ] {
        let (svc, fake) = new_gcs_service_for_testing("bucket");
        let save = |part: Part, custom_metadata: Map<String, Value>| SaveRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: save_session_id.into(),
            file_name: file_name.into(),
            part: Some(part),
            custom_metadata,
            ..Default::default()
        };
        fake.set_now(at(first_create_time));
        let mut metadata = json!({"string": "value", "number": 42, "enabled": true})
            .as_object()
            .cloned()
            .unwrap();
        svc.save(save(
            Part::from_bytes("data".into(), "image/png"),
            metadata.clone(),
        ))
        .await
        .unwrap();
        // The saved version keeps the value it was saved with.
        metadata.insert("string".into(), json!("changed after save"));

        fake.set_now(at(second_create_time));
        svc.save(save(Part::from_text("text"), Map::new()))
            .await
            .unwrap();

        let latest = get_version(&svc, get_req(get_session_id, file_name, 0)).await;
        assert_eq!(
            latest,
            ArtifactVersion {
                version: 2,
                canonical_uri: format!("{uri_prefix}2"),
                custom_metadata: Map::new(),
                create_time: second_create_time,
                mime_type: "text/plain".into(),
            },
            "{name}: latest"
        );

        let want_first = ArtifactVersion {
            version: 1,
            canonical_uri: format!("{uri_prefix}1"),
            custom_metadata: json!({"string": "value", "number": "42", "enabled": "true"})
                .as_object()
                .cloned()
                .unwrap(),
            create_time: first_create_time,
            mime_type: "image/png".into(),
        };
        let mut first = get_version(&svc, get_req(get_session_id, file_name, 1)).await;
        assert_eq!(first, want_first, "{name}: v1");

        first
            .custom_metadata
            .insert("string".into(), json!("changed after read"));
        let again = get_version(&svc, get_req(get_session_id, file_name, 1)).await;
        assert_eq!(again, want_first, "{name}: second v1");

        svc.delete(delete_req(get_session_id, file_name, 0))
            .await
            .unwrap();
        fake.set_now(at(recreate_time));
        svc.save(save(
            Part::from_bytes("new".into(), "application/octet-stream"),
            Map::new(),
        ))
        .await
        .unwrap();
        let recreated = get_version(&svc, get_req(get_session_id, file_name, 0)).await;
        assert_eq!(
            recreated,
            ArtifactVersion {
                version: 1,
                canonical_uri: format!("{uri_prefix}1"),
                custom_metadata: Map::new(),
                create_time: recreate_time,
                mime_type: "application/octet-stream".into(),
            },
            "{name}: recreated"
        );
    }
}

/// Concurrent saves of the same artifact get distinct versions and none is overwritten.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_gcs_artifact_service_concurrent_save() {
    let (svc, _) = new_gcs_service_for_testing("concurrent");
    let svc = Arc::new(svc);
    // Must stay <= MAX_SAVE_ATTEMPTS: a maximally unlucky saver can lose one race per other
    // writer.
    const SAVERS: i64 = 10;
    let tasks: Vec<_> = (0..SAVERS)
        .map(|_| {
            let svc = svc.clone();
            tokio::spawn(async move { svc.save(save_req()).await })
        })
        .collect();
    let mut got = Vec::new();
    for (i, task) in tasks.into_iter().enumerate() {
        match task.await.unwrap() {
            Ok(resp) => got.push(resp.version),
            Err(err) => panic!("saver {i}: Save() failed: {err}"),
        }
    }
    got.sort_unstable();
    let want: Vec<i64> = (1..=SAVERS).collect();
    assert_eq!(got, want, "returned versions");

    let mut stored = svc.versions(versions_req()).await.unwrap().versions;
    stored.sort_unstable();
    assert_eq!(stored, want, "stored versions");
}

/// A non-precondition write error is returned as it is and not retried.
#[tokio::test]
async fn test_save_returns_write_error_without_retry() {
    let (svc, fake) = new_gcs_service_for_testing("errtest");
    fake.set_write_err(|| GcsError::Http {
        status: 500,
        message: "boom".into(),
        body: String::new(),
    });
    let err = svc.save(save_req()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "failed to save artifact: failed to close blob writer: googleapi: Error 500: boom"
    );
    assert!(backend_chain_any(&err, |e| matches!(
        e.downcast_ref::<GcsError>(),
        Some(GcsError::Http { status: 500, .. })
    )));
    assert_eq!(
        fake.close_calls(),
        1,
        "no retry on a non-precondition error"
    );
}

/// Save gives up with the version-conflict error after MAX_SAVE_ATTEMPTS when every write hits
/// a precondition failure.
#[tokio::test]
async fn test_save_exhausts_retries_on_persistent_conflict() {
    let (svc, fake) = new_gcs_service_for_testing("errtest");
    fake.set_write_err(|| GcsError::PreconditionFailed {
        message: String::new(),
        body: String::new(),
    });
    let mut req = save_req();
    req.custom_metadata = json!({"key": "value"}).as_object().cloned().unwrap();

    let err = svc.save(req).await.unwrap_err();
    assert!(is_version_conflict(&err), "Save() err = {err}");
    assert_eq!(
        err.to_string(),
        "failed to save artifact \"file\" after 16 attempts (last error: failed to close blob \
         writer: googleapi: got HTTP response code 412 with body: ): artifact version conflict"
    );
    let attempts = usize::try_from(MAX_SAVE_ATTEMPTS).unwrap();
    assert_eq!(fake.close_calls(), attempts);
    let want = BTreeMap::from([("key".to_owned(), "value".to_owned())]);
    assert_eq!(fake.metadata_calls(), vec![want; attempts]);
}

/// An empty payload still creates a real, listable version.
#[tokio::test]
async fn test_save_zero_byte_artifact() {
    let (svc, _) = new_gcs_service_for_testing("zerobyte");
    let req = || SaveRequest {
        part: Some(Part::from_bytes(
            bytes::Bytes::new(),
            "application/octet-stream",
        )),
        ..save_req()
    };
    for want in 1..=2 {
        assert_eq!(svc.save(req()).await.unwrap().version, want);
    }
    let mut versions = svc.versions(versions_req()).await.unwrap().versions;
    versions.sort_unstable();
    assert_eq!(versions, [1, 2]);

    let loaded = svc
        .load(LoadRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            file_name: "file".into(),
            version: 0,
        })
        .await
        .unwrap();
    let data = loaded.part.unwrap().inline_data.unwrap().data;
    assert!(data.is_empty(), "Load() data = {data:?}, want empty");
}

/// Load and GetArtifactVersion recognize the object-does-not-exist error even when the storage
/// client wraps it (as Go's storage client does in production).
#[tokio::test]
async fn test_not_found_is_wrapped_sentinel() {
    for name in ["Load", "GetArtifactVersion"] {
        let (svc, fake) = new_gcs_service_for_testing("wrapped-not-exist");
        fake.set_attrs_err(|| {
            GcsError::Other(Box::new(GoWrapped::new(
                "googleapi: Error 404: Not Found",
                GcsError::ObjectNotExist,
            )))
        });
        let err = match name {
            "Load" => svc
                .load(LoadRequest {
                    app_name: "app".into(),
                    user_id: "user".into(),
                    session_id: "session".into(),
                    file_name: "file".into(),
                    version: 1,
                })
                .await
                .map(|_| ()),
            _ => svc
                .get_artifact_version(get_req("session", "file", 1))
                .await
                .map(|_| ()),
        }
        .unwrap_err();
        assert!(
            matches!(err, ArtifactError::NotFound),
            "{name}: err = {err}"
        );
    }
}

/// The URI handed to a consumer is the gs:// form even though the object has a media link.
#[tokio::test]
async fn test_get_artifact_version_canonical_uri() {
    for (name, file_name, want_path) in [
        ("session scoped", "file", "app/user/session/file"),
        ("user namespaced", "user:file", "app/user/user/user:file"),
    ] {
        let (svc, fake) = new_gcs_service_for_testing("demo-bucket");
        let saved = svc
            .save(SaveRequest {
                file_name: file_name.into(),
                ..save_req()
            })
            .await
            .unwrap();

        // The assertion below only tells the two forms apart while the stored object has a
        // media link to be preferred over, so pin that.
        let blob_name = build_blob_name("app", "user", "session", file_name, saved.version);
        let attrs = fake.attrs(&blob_name).await.unwrap();
        assert!(!attrs.media_link.is_empty(), "{name}: no media link");

        let got = get_version(&svc, get_req("session", file_name, 0)).await;
        assert_eq!(
            got.canonical_uri,
            format!("gs://demo-bucket/{want_path}/{}", saved.version),
            "{name}"
        );
    }
}

#[tokio::test]
async fn test_get_artifact_version_canonical_uri_is_stored_object_name() {
    for (name, app_name, user_id, session_id, file_name) in [
        (
            "session scoped special characters",
            "app one",
            "user/é",
            "session%two",
            "report 100%.txt",
        ),
        (
            "user scoped special characters",
            "app/one",
            "user two",
            "session three",
            "user:résumé %.txt",
        ),
    ] {
        let (svc, fake) = new_gcs_service_for_testing("demo-bucket");
        let saved = svc
            .save(SaveRequest {
                app_name: app_name.into(),
                user_id: user_id.into(),
                session_id: session_id.into(),
                file_name: file_name.into(),
                part: Some(Part::from_text("data")),
                ..Default::default()
            })
            .await
            .unwrap();
        let resp = svc
            .get_artifact_version(GetArtifactVersionRequest {
                app_name: app_name.into(),
                user_id: user_id.into(),
                session_id: session_id.into(),
                file_name: file_name.into(),
                version: 0,
            })
            .await
            .unwrap();
        let blob_name = build_blob_name(app_name, user_id, session_id, file_name, saved.version);
        assert_eq!(
            fake.object_names(),
            std::slice::from_ref(&blob_name),
            "{name}"
        );
        assert_eq!(
            resp.artifact_version.unwrap().canonical_uri,
            format!("gs://demo-bucket/{blob_name}"),
            "{name}: CanonicalURI does not name the stored object"
        );
    }
}

/// The jittered backoff stays within [0, SAVE_RETRY_MAX_DELAY].
#[test]
fn test_backoff_delay_bounds() {
    for attempt in 0..MAX_SAVE_ATTEMPTS {
        let d = backoff_delay(attempt);
        assert!(
            d <= SAVE_RETRY_MAX_DELAY,
            "backoff_delay({attempt}) = {d:?}, want within [0, {SAVE_RETRY_MAX_DELAY:?}]"
        );
    }
    // Attempts past the shift range stay capped.
    for attempt in [63, 64, u32::MAX] {
        assert!(backoff_delay(attempt) <= SAVE_RETRY_MAX_DELAY);
    }
}

// parity: adapted: Rust has no nil error, and the gRPC cases have no counterpart (JSON API only)
#[test]
fn test_is_precondition_failed() {
    let http = |status: u16| GcsError::Http {
        status,
        message: String::new(),
        body: String::new(),
    };
    let precondition = || GcsError::PreconditionFailed {
        message: String::new(),
        body: String::new(),
    };
    let cases: [(&str, GcsError, bool); 6] = [
        ("generic", GcsError::Other("boom".into()), false),
        ("http 412", precondition(), true),
        ("http 412 status", http(412), true),
        (
            "http 412 wrapped",
            GcsError::Other(Box::new(GoWrapped::new("save", precondition()))),
            true,
        ),
        ("http 500", http(500), false),
        ("not found", GcsError::ObjectNotExist, false),
    ];
    for (name, err, want) in cases {
        assert_eq!(is_precondition_failed(&err), want, "{name}: {err}");
    }
    let wrapped = GoWrapped::new("failed to close blob writer", precondition());
    assert!(is_precondition_failed(&wrapped));
}

// parity: adapted: the canceled-context case has no counterpart (cancellation is dropping the future)
#[tokio::test(start_paused = true)]
async fn test_sleep_context() {
    let start = tokio::time::Instant::now();
    sleep_context(Duration::ZERO).await;
    assert_eq!(start.elapsed(), Duration::ZERO, "sleep_context(0) waited");

    sleep_context(Duration::from_millis(1)).await;
    assert!(start.elapsed() >= Duration::from_millis(1));
}
