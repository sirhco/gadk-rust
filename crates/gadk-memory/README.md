# gadk-memory

Long-term memory for gadk agents. A `MemoryService` ingests whole sessions
(`add_session_to_memory`) and answers searches over a user's memories (`search_memory`),
returning `MemoryEntry` values (content, author, timestamp, metadata). The `load_memory` and
`preload_memory` tools in `gadk-tools` search the runner's memory service and put matching
memories in front of the model.

`InMemoryMemoryService` is the process-local implementation: memories are scoped per app and
user, a search returns at most ten entries ranked by the number of distinct query words they
match, and thought parts are neither searched nor returned. Re-adding a session replaces what
was stored for it. Other backends live in their own crates, for example the Vertex AI Memory
Bank service in `gadk-vertexai`.

Most applications use this crate through the `gadk` facade (`gadk::memory`; the prelude
re-exports `MemoryService` and `InMemoryMemoryService`).

## Example

```rust
// Inside an async fn returning Result; `previous` is an `Arc<dyn gadk_session::Session>`.
use gadk_memory::{InMemoryMemoryService, MemoryService, SearchRequest};

let memory = InMemoryMemoryService::new();
memory.add_session_to_memory(previous.as_ref()).await?;

let found = memory
    .search_memory(SearchRequest {
        query: "trip to Tokyo".into(),
        user_id: "u1".into(),
        app_name: "my_app".into(),
    })
    .await?;
for entry in found.memories {
    println!("{}: {:?}", entry.author, entry.content);
}
```

Pass the service to `RunnerConfig::memory_service` to make it available to an agent's memory
tools.

## Incremental ingestion (experimental)

A service can also accept a batch of new events without re-ingesting the whole session through
the `EventAdder` capability. Call `add_events_to_memory(&svc, AddEventsRequest { .. })`; it
returns `MemoryError::AddEventsUnsupported` when the service has no adder.
`InMemoryMemoryService` supports it and skips events whose ID is already stored in the same
session bucket. This API is experimental and may change.
