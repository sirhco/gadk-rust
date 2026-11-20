# gadk-vertexai

Vertex AI services for gadk:

- `session::VertexAiSessionService`: a `SessionService` that keeps sessions and their events in
  an Agent Engine (reasoning engine) session store.
- `memory::VertexAiMemoryService`: a `MemoryService` that generates memories from sessions and
  retrieves them with the engine's Memory Bank.
- `memory::VertexAiRagMemoryService`: a `MemoryService` that stores each session as a file of
  a Vertex AI RAG Engine corpus and searches them with RAG retrieval.
- `example_store::new_provider`: a few-shot example provider for the example tool in
  `gadk-tools`, backed by a Vertex AI Example Store.
- `reasoning_engine::ReasoningEngineClient`: creates and updates an Agent Engine and waits on
  its operations (the calls a deploy makes).

The Agent Engine services call the Vertex AI `v1beta1` REST API with the resource names and
request bodies that ADK uses, so a gadk service and an ADK service can share one engine; the RAG
memory service calls the RAG Engine `v1` API. Requests are authenticated with Application
Default Credentials (`cloud-platform` scope) unless the config gives `credentials`, a custom
`http_client`, or sets `without_authentication`.

With the facade, enable the `vertexai` feature of `gadk` and use `gadk::vertexai`.

## Example

```rust
// Inside an async fn returning Result.
use std::sync::Arc;
use gadk_vertexai::AgentEngineData;
use gadk_vertexai::memory::{VertexAiMemoryService, VertexAiMemoryServiceConfig};
use gadk_vertexai::session::{VertexAiSessionService, VertexAiSessionServiceConfig};

let engine = AgentEngineData {
    project_id: "my-project".into(),
    location: "us-central1".into(),
    reasoning_engine: "1234567890".into(),
};
let sessions = Arc::new(VertexAiSessionService::new(VertexAiSessionServiceConfig {
    project_id: engine.project_id.clone(),
    location: engine.location.clone(),
    reasoning_engine: Some(engine.reasoning_engine.clone()),
    ..Default::default()
})?);
let memory = Arc::new(VertexAiMemoryService::new(VertexAiMemoryServiceConfig {
    engine,
    wait_for_completion: true,
    ..Default::default()
})?);
// Pass them as `RunnerConfig::session_service` and `RunnerConfig::memory_service`.
```

## RAG memory

`VertexAiRagMemoryService` keeps memory in a RAG Engine corpus you create once. RAG Engine is
regional: use a region such as `us-central1` (not `us` or `global`). The caller needs the
Vertex AI User role (`roles/aiplatform.user`) on the project.

```sh
curl -X POST -H "Authorization: Bearer $(gcloud auth print-access-token)" \
  -H "Content-Type: application/json" \
  "https://us-central1-aiplatform.googleapis.com/v1/projects/MY_PROJECT/locations/us-central1/ragCorpora" \
  -d '{"displayName": "gadk-memory"}'
# The reply is an operation; once done, list the corpora to read the corpus name:
curl -H "Authorization: Bearer $(gcloud auth print-access-token)" \
  "https://us-central1-aiplatform.googleapis.com/v1/projects/MY_PROJECT/locations/us-central1/ragCorpora"
```

```rust
// Inside an async fn returning Result.
use std::sync::Arc;
use gadk_vertexai::memory::{VertexAiRagMemoryConfig, VertexAiRagMemoryService};

let memory = Arc::new(VertexAiRagMemoryService::new(
    VertexAiRagMemoryConfig::new("projects/MY_PROJECT/locations/us-central1/ragCorpora/123")
        .with_similarity_top_k(10),
)?);
// Pass it as `RunnerConfig::memory_service`.
```

How it behaves:

- Each session is one RAG file named `{app_name}.{user_id}.{session_id}`, with the app name and
  user id percent-encoded (see below), holding one JSON line
  `{"author", "timestamp", "text"}` per event with text (thoughts, images and tool calls are
  left out). Adding a session again deletes its earlier file and uploads a new one; the two
  steps are not atomic, so a failure between them leaves the session out of memory until the
  next add. App names and user ids must be non-empty; `%` and `.` in them are
  percent-encoded in the file name (`alice@example.com` → `alice@example%2Ecom`), so any id
  works.
- Each add lists every file in the corpus to find the session's file, so adds slow down as the
  corpus grows.
- Indexing is asynchronous: a session may not be searchable for a while after it is added.
- Search retrieves from the whole corpus, then keeps the requesting app and user's lines, so
  other users' files can take retrieval slots: raise `with_similarity_top_k` for a shared
  corpus. A line split across RAG chunks is skipped. File display names have a length limit,
  so very long ids make the upload fail.
- Incremental event adds (`add_events_to_memory`) are not supported.

### Cost and teardown

RAG Engine stores vectors in RagManagedDb, a Spanner instance billed **per hour, per project
and region**, from the first corpus until the project's RAG Engine config is set to
Unprovisioned (the Basic tier is 100 processing units). Deleting a corpus does **not** stop
that billing. To stop it when you are done:

```sh
TOKEN=$(gcloud auth print-access-token)
BASE=https://us-central1-aiplatform.googleapis.com/v1/projects/MY_PROJECT/locations/us-central1
# 1. Delete the corpus and its files.
curl -X DELETE -H "Authorization: Bearer $TOKEN" "$BASE/ragCorpora/123?force=true"
# 2. Unprovision the RAG database. This DELETES EVERY RAG corpus's data in this project and
#    region, and cannot be undone.
curl -X PATCH -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  "$BASE/ragEngineConfig" \
  -d '{"ragManagedDbConfig": {"unprovisioned": {}}}'
# 3. Confirm: the config shows "unprovisioned" (after the PATCH operation finishes).
curl -H "Authorization: Bearer $TOKEN" "$BASE/ragEngineConfig"
```

## Notes

- `VertexAiSessionServiceConfig::reasoning_engine` may be `None`: the engine is then taken from
  each request's app name (a numeric ID, or a full `projects/…/reasoningEngines/{id}` name).
- With `VertexAiMemoryServiceConfig::state_key_session_last_update_time` set, only events newer
  than the time stored under that session-state key are sent for memory generation.
- Endpoints default to `https://aiplatform.googleapis.com` for sessions and to the regional
  `https://{location}-aiplatform.googleapis.com` for memory and the Example Store; each config
  has an `endpoint` override.
