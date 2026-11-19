# gadk-artifact-gcs

An `ArtifactService` (from `gadk-artifact`) that stores artifacts in a Google Cloud Storage
bucket. `GcsArtifactService` saves, loads, lists, versions and deletes artifacts through the GCS
JSON API; `GcsConfig` sets the credentials, HTTP client, endpoint and extra headers.

Objects are named `{app}/{user}/{session}/{file}/{version}`, or
`{app}/{user}/user/{file}/{version}` for a `user:`-scoped file, with the same content types
and metadata that ADK's GCS artifact service writes, so gadk and ADK services can share one
bucket.

With the facade, enable the `gcs` feature of `gadk` and use `gadk::artifact_gcs`.

## Example

```rust
// Inside an async fn returning Result.
use gadk_artifact::{ArtifactService, SaveRequest};
use gadk_artifact_gcs::{GcsArtifactService, GcsConfig};
use gadk_genai::Part;

let artifacts = GcsArtifactService::new("my-bucket", GcsConfig::default())?;
let saved = artifacts
    .save(SaveRequest {
        app_name: "my_app".into(),
        user_id: "u1".into(),
        session_id: "s1".into(),
        file_name: "greeting.txt".into(),
        part: Some(Part::from_text("hello")),
        ..Default::default()
    })
    .await?;
println!("saved version {}", saved.version);
```

Pass the service to `RunnerConfig::artifact_service` to give agents and the artifact tools
access to it.

## Configuration

- **Credentials.** `GcsConfig::credentials`, or Application Default Credentials (scopes
  `devstorage.full_control` and `cloud-platform`) when `None`. Resolving ADC needs a running
  Tokio runtime, so call `GcsArtifactService::new` from inside one.
- **Endpoint.** `GcsConfig::endpoint`, else `STORAGE_EMULATOR_HOST` when set (authentication
  is then turned off), else `https://storage.googleapis.com`.

## Versions

GCS cannot list and write atomically, so `save` writes version `max + 1` with a
does-not-exist precondition and, when another writer took that version first, backs off and
retries (at most 16 attempts). If every attempt loses, it fails with an error that
`is_version_conflict` detects; it never overwrites a version. This only holds when every writer
to the bucket uses the precondition.
