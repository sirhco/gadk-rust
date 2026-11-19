# gadk-artifact

Artifact storage for gadk agents. An artifact is a named file (a `genai::Part` holding text or
inline bytes with a MIME type) scoped to an app, a user and a session. Every save creates a new
version; earlier versions stay readable until deleted.

`ArtifactService` is the storage trait: `save`, `load`, `delete`, `list`, `versions` and
`get_artifact_version`, each taking a request struct (`SaveRequest`, `LoadRequest`, …) whose
`validate` method checks the required fields and rejects a file name containing `/` or `\`.
A `version` of 0 means the latest version on load and every version on delete. File names that
start with `user:` are user-scoped: they are shared by all of the user's sessions and listed with
each session's own files. Errors are `ArtifactError`: `NotFound`, `InvalidRequest` or `Backend`.

`InMemoryArtifactService` keeps every version in process memory (versions start at 1). It is the
only implementation in this crate; `gadk-artifact-gcs` stores artifacts in Google Cloud Storage.
Give a service to the runner through `RunnerConfig::artifact_service` so agents and tools (such
as `load_artifacts`) can use it. The `gadk` facade re-exports this crate as `gadk::artifact`.

## Example

```rust
// Inside an async fn returning Result.
use std::sync::Arc;
use gadk::artifact::{ArtifactService, InMemoryArtifactService, LoadRequest, SaveRequest};
use gadk::genai::Part;

let artifacts = Arc::new(InMemoryArtifactService::new());
let saved = artifacts
    .save(SaveRequest {
        app_name: "my_app".into(),
        user_id: "user".into(),
        session_id: "session".into(),
        file_name: "haiku.txt".into(),
        part: Some(Part::from_text("An old silent pond...")),
        ..Default::default()
    })
    .await?;
assert_eq!(saved.version, 1);

let loaded = artifacts
    .load(LoadRequest {
        app_name: "my_app".into(),
        user_id: "user".into(),
        session_id: "session".into(),
        file_name: "haiku.txt".into(),
        version: 0, // the latest
    })
    .await?;
```

## Features

- `testsuite`: enables `gadk_artifact::testsuite::run`, a conformance suite for
  `ArtifactService` implementations. Call it from a backend's tests with a factory that returns
  a fresh `Arc<dyn ArtifactService>`; it panics on the first failing check.
