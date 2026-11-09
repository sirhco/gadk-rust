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

//! Saves, lists, loads and deletes an artifact in a Google Cloud Storage bucket through
//! `gadk-artifact-gcs` (the gadk counterpart of ADK-Go's `artifact/gcsartifact`), with
//! Application Default Credentials (`gcloud auth application-default login`).
//!
//! `GADK_GCS_BUCKET=my-bucket cargo run -p gadk --example gcs_artifacts --features gcs`
//!
//! Set `STORAGE_EMULATOR_HOST` to use a local emulator instead (no credentials).

use gadk::artifact::{
    ArtifactService, DeleteRequest, GetArtifactVersionRequest, ListRequest, LoadRequest,
    SaveRequest,
};
use gadk::artifact_gcs::{GcsArtifactService, GcsConfig};
use gadk::genai::Part;

const APP_NAME: &str = "gcs_artifacts_example";
const USER_ID: &str = "example_user";
const SESSION_ID: &str = "session_1";
const FILE_NAME: &str = "greeting.txt";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bucket = std::env::var("GADK_GCS_BUCKET")
        .map_err(|_| anyhow::anyhow!("set GADK_GCS_BUCKET to a bucket you can write"))?;
    let artifacts = GcsArtifactService::new(bucket, GcsConfig::default())?;

    let saved = artifacts
        .save(SaveRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: SESSION_ID.to_string(),
            file_name: FILE_NAME.to_string(),
            part: Some(Part::from_text("hello from gadk")),
            ..Default::default()
        })
        .await?;
    println!("saved {FILE_NAME} version {}", saved.version);

    let listed = artifacts
        .list(ListRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: SESSION_ID.to_string(),
        })
        .await?;
    println!("files in the session: {:?}", listed.file_names);

    let loaded = artifacts
        .load(LoadRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: SESSION_ID.to_string(),
            file_name: FILE_NAME.to_string(),
            version: 0,
        })
        .await?;
    if let Some(blob) = loaded.part.and_then(|p| p.inline_data) {
        println!(
            "loaded {} bytes ({}): {}",
            blob.data.len(),
            blob.mime_type,
            String::from_utf8_lossy(&blob.data)
        );
    }

    let version = artifacts
        .get_artifact_version(GetArtifactVersionRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: SESSION_ID.to_string(),
            file_name: FILE_NAME.to_string(),
            version: 0,
        })
        .await?;
    if let Some(v) = version.artifact_version {
        println!(
            "version {} is {} ({})",
            v.version, v.canonical_uri, v.create_time
        );
    }

    artifacts
        .delete(DeleteRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: SESSION_ID.to_string(),
            file_name: FILE_NAME.to_string(),
            version: 0,
        })
        .await?;
    println!("deleted every version of {FILE_NAME}");
    Ok(())
}
