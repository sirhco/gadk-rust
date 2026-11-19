# gadk-vertexai

Vertex AI Agent Engine services for gadk:

- `session::VertexAiSessionService`: a `SessionService` that keeps sessions and their events in
  an Agent Engine (reasoning engine) session store.
- `memory::VertexAiMemoryService`: a `MemoryService` that generates memories from sessions and
  retrieves them with the engine's Memory Bank.
- `example_store::new_provider`: a few-shot example provider for the example tool in
  `gadk-tools`, backed by a Vertex AI Example Store.
- `reasoning_engine::ReasoningEngineClient`: creates and updates an Agent Engine and waits on
  its operations (the calls a deploy makes).

All of them call the Vertex AI `v1beta1` REST API with the resource names and request bodies
that ADK uses, so a gadk service and an ADK service can share one engine. Requests are
authenticated with Application Default Credentials (`cloud-platform` scope) unless the config
gives `credentials`, a custom `http_client`, or sets `without_authentication`.

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

## Notes

- `VertexAiSessionServiceConfig::reasoning_engine` may be `None`: the engine is then taken from
  each request's app name (a numeric ID, or a full `projects/…/reasoningEngines/{id}` name).
- With `VertexAiMemoryServiceConfig::state_key_session_last_update_time` set, only events newer
  than the time stored under that session-state key are sent for memory generation.
- Endpoints default to `https://aiplatform.googleapis.com` for sessions and to the regional
  `https://{location}-aiplatform.googleapis.com` for memory and the Example Store; each config
  has an `endpoint` override.
