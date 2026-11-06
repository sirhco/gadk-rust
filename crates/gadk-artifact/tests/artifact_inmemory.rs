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

//! Parity target for Go artifact/inmemory_test.go.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, TimeZone, Utc};
use gadk_artifact::{
    ArtifactService, ArtifactVersion, GetArtifactVersionRequest, InMemoryArtifactService,
    LoadRequest, SaveRequest,
};
use gadk_genai::Part;
use gadk_platform::Platform;
use serde_json::{Map, Value, json};

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

/// A settable clock: Go passes a time provider per call through ctx; Rust sets it on the
/// service's Platform and moves the shared instant between calls.
fn clocked() -> (InMemoryArtifactService, Arc<Mutex<DateTime<Utc>>>) {
    let now = Arc::new(Mutex::new(Utc.timestamp_opt(0, 0).unwrap()));
    let n = now.clone();
    let svc = InMemoryArtifactService::with_platform(
        Platform::default().with_time(move || *n.lock().unwrap()),
    );
    (svc, now)
}

#[tokio::test]
async fn test_in_memory_artifact_service() {
    gadk_artifact::testsuite::run(|| async {
        Arc::new(InMemoryArtifactService::new()) as Arc<dyn ArtifactService>
    })
    .await;
}

// parity: adapted: Go injects the create time per call via ctx; Rust uses one service Platform whose clock the test moves
#[tokio::test]
async fn test_in_memory_artifact_version_fields() {
    let first_create = Utc.with_ymd_and_hms(2026, 8, 31, 12, 34, 56).unwrap();
    let second_create = first_create + Duration::minutes(1);
    let read_time = second_create + Duration::hours(1);
    for (name, file_name, save_sid, get_sid, uri_prefix) in [
        (
            "session scoped",
            "file.txt",
            "session",
            "session",
            "memory://apps/app/users/user/sessions/session/artifacts/file.txt/versions/",
        ),
        (
            "user scoped",
            "user:file.txt",
            "save-session",
            "irrelevant-session",
            "memory://apps/app/users/user/artifacts/user:file.txt/versions/",
        ),
    ] {
        let (srv, clock) = clocked();
        let mut metadata = obj(json!({"key": "value", "count": 42, "enabled": true}));
        *clock.lock().unwrap() = first_create;
        srv.save(SaveRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: save_sid.into(),
            file_name: file_name.into(),
            part: Some(Part::from_bytes(b"data".to_vec().into(), "image/png")),
            custom_metadata: metadata.clone(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("{name}: Save(v1) failed: {e}"));
        metadata.insert("key".into(), json!("changed after save"));
        *clock.lock().unwrap() = second_create;
        srv.save(SaveRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: save_sid.into(),
            file_name: file_name.into(),
            part: Some(Part::from_text("text")),
            ..Default::default()
        })
        .await
        .unwrap();
        *clock.lock().unwrap() = read_time;
        let get = |version: i64| GetArtifactVersionRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: get_sid.into(),
            file_name: file_name.into(),
            version,
        };
        let latest = srv.get_artifact_version(get(0)).await.unwrap();
        assert_eq!(
            latest.artifact_version,
            Some(ArtifactVersion {
                version: 2,
                canonical_uri: format!("{uri_prefix}2"),
                custom_metadata: Map::new(),
                create_time: second_create,
                mime_type: "text/plain".into(),
            }),
            "{name}"
        );
        let want_first = ArtifactVersion {
            version: 1,
            canonical_uri: format!("{uri_prefix}1"),
            custom_metadata: obj(json!({"key": "value", "count": "42", "enabled": "true"})),
            create_time: first_create,
            mime_type: "image/png".into(),
        };
        let first = srv.get_artifact_version(get(1)).await.unwrap();
        assert_eq!(first.artifact_version.as_ref(), Some(&want_first), "{name}");
        let loaded = srv
            .load(LoadRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                session_id: get_sid.into(),
                file_name: file_name.into(),
                version: 1,
            })
            .await
            .unwrap();
        assert_eq!(
            loaded.part.unwrap().inline_data.unwrap().mime_type,
            want_first.mime_type
        );
        let mut mutated = first.artifact_version.unwrap();
        mutated
            .custom_metadata
            .insert("key".into(), json!("changed after read"));
        let again = srv.get_artifact_version(get(1)).await.unwrap();
        assert_eq!(again.artifact_version, Some(want_first), "{name}");
    }
}

#[tokio::test]
async fn test_in_memory_canonical_uri_escapes_segments() {
    for (name, file_name, want_uri) in [
        (
            "session scoped",
            "report?old#draft",
            "memory://apps/app%2Fone/users/user%2Ftwo/sessions/session%2Fthree/artifacts/report%3Fold%23draft/versions/1",
        ),
        (
            "user scoped",
            "user:report?old#draft",
            "memory://apps/app%2Fone/users/user%2Ftwo/artifacts/user:report%3Fold%23draft/versions/1",
        ),
        (
            "session scoped percent and space",
            "report 100%.txt",
            "memory://apps/app%2Fone/users/user%2Ftwo/sessions/session%2Fthree/artifacts/report%20100%25.txt/versions/1",
        ),
        (
            "user scoped percent and space",
            "user:report 100%.txt",
            "memory://apps/app%2Fone/users/user%2Ftwo/artifacts/user:report%20100%25.txt/versions/1",
        ),
    ] {
        let srv = InMemoryArtifactService::new();
        srv.save(SaveRequest {
            app_name: "app/one".into(),
            user_id: "user/two".into(),
            session_id: "session/three".into(),
            file_name: file_name.into(),
            part: Some(Part::from_text("data")),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("{name}: Save() failed: {e}"));
        let resp = srv
            .get_artifact_version(GetArtifactVersionRequest {
                app_name: "app/one".into(),
                user_id: "user/two".into(),
                session_id: "session/three".into(),
                file_name: file_name.into(),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{name}: GetArtifactVersion() failed: {e}"));
        let got = resp.artifact_version.unwrap().canonical_uri;
        assert_eq!(got, want_uri, "{name}");
        // Go parses the URI and checks it has no query and no fragment.
        assert!(
            !got.contains('?') && !got.contains('#'),
            "{name}: CanonicalURI {got:?} has a query or fragment"
        );
    }
}
