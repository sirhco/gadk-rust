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

//! Live round trip against a real bucket: set `GADK_TEST_GCS_BUCKET` to a bucket the
//! Application Default Credentials can write (`gcloud auth application-default login`). The
//! test writes under a random app name and deletes what it wrote; unset, it is skipped.
//!
//! `GADK_TEST_GCS_BUCKET=my-bucket cargo test -p gadk-artifact-gcs --test live_gcs -- --nocapture`

use std::sync::Arc;

use gadk_artifact::{
    ArtifactError, ArtifactService, DeleteRequest, GetArtifactVersionRequest, ListRequest,
    LoadRequest, SaveRequest, VersionsRequest,
};
use gadk_artifact_gcs::{GcsArtifactService, GcsConfig};
use gadk_genai::Part;
use gadk_platform::env::MapEnv;
use serde_json::json;

const USER: &str = "live-user";
const SESSION: &str = "live-session";

#[tokio::test]
async fn live_gcs_round_trip() {
    let Some(bucket) = std::env::var("GADK_TEST_GCS_BUCKET")
        .ok()
        .filter(|b| !b.is_empty())
    else {
        println!("skipping live_gcs_round_trip: GADK_TEST_GCS_BUCKET is not set");
        return;
    };
    // An empty injected environment: no emulator, and credentials from google-cloud-auth's
    // own Application Default Credentials lookup.
    let config = GcsConfig {
        env: Arc::new(MapEnv::default()),
        ..Default::default()
    };
    let svc = GcsArtifactService::new(bucket.clone(), config).unwrap();
    let app = format!("gadk-live-{:016x}", rand::random::<u64>());
    let save = |file_name: &str, part: Part| SaveRequest {
        app_name: app.clone(),
        user_id: USER.into(),
        session_id: SESSION.into(),
        file_name: file_name.into(),
        part: Some(part),
        custom_metadata: json!({"n": 1}).as_object().cloned().unwrap(),
        ..Default::default()
    };
    let load = |file_name: &str, version: i64| LoadRequest {
        app_name: app.clone(),
        user_id: USER.into(),
        session_id: SESSION.into(),
        file_name: file_name.into(),
        version,
    };
    let delete = |file_name: &str, version: i64| DeleteRequest {
        app_name: app.clone(),
        user_id: USER.into(),
        session_id: SESSION.into(),
        file_name: file_name.into(),
        version,
    };
    let versions = |file_name: &str| VersionsRequest {
        app_name: app.clone(),
        user_id: USER.into(),
        session_id: SESSION.into(),
        file_name: file_name.into(),
    };
    let list = ListRequest {
        app_name: app.clone(),
        user_id: USER.into(),
        session_id: SESSION.into(),
    };

    let v1 = svc
        .save(save("f.txt", Part::from_text("one")))
        .await
        .unwrap();
    let v2 = svc
        .save(save("f.txt", Part::from_bytes("two".into(), "image/png")))
        .await
        .unwrap();
    assert_eq!((v1.version, v2.version), (1, 2));
    svc.save(save("user:u.txt", Part::from_text("shared")))
        .await
        .unwrap();

    let latest = svc.load(load("f.txt", 0)).await.unwrap().part.unwrap();
    let blob = latest.inline_data.unwrap();
    assert_eq!(
        (blob.data.as_ref(), blob.mime_type.as_str()),
        (&b"two"[..], "image/png")
    );
    let first = svc.load(load("f.txt", 1)).await.unwrap().part.unwrap();
    let blob = first.inline_data.unwrap();
    assert_eq!(
        (blob.data.as_ref(), blob.mime_type.as_str()),
        (&b"one"[..], "text/plain")
    );

    assert_eq!(
        svc.list(list.clone()).await.unwrap().file_names,
        ["f.txt", "user:u.txt"]
    );
    let mut got = svc.versions(versions("f.txt")).await.unwrap().versions;
    got.sort_unstable();
    assert_eq!(got, [1, 2]);

    let av = svc
        .get_artifact_version(GetArtifactVersionRequest {
            app_name: app.clone(),
            user_id: USER.into(),
            session_id: SESSION.into(),
            file_name: "f.txt".into(),
            version: 1,
        })
        .await
        .unwrap()
        .artifact_version
        .unwrap();
    assert_eq!(
        av.canonical_uri,
        format!("gs://{bucket}/{app}/{USER}/{SESSION}/f.txt/1")
    );
    assert_eq!(av.mime_type, "text/plain");
    assert_eq!(av.custom_metadata, *json!({"n": "1"}).as_object().unwrap());

    svc.delete(delete("f.txt", 2)).await.unwrap();
    svc.delete(delete("f.txt", 0)).await.unwrap();
    svc.delete(delete("user:u.txt", 0)).await.unwrap();
    assert!(matches!(
        svc.versions(versions("f.txt")).await,
        Err(ArtifactError::NotFound)
    ));
    assert!(svc.list(list).await.unwrap().file_names.is_empty());
}
