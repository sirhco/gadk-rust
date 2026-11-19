# gadk-session

Sessions and events for gadk: the data model a runner reads and writes as an agent converses,
and the storage interface behind it.

- `SessionService` is the async storage trait (`create`, `get`, `list`, `delete`,
  `append_event`); `Session` is one conversation (its ID, app, user, `State` and ordered
  events). `InMemorySessionService` is the in-process implementation. Persistent
  implementations live in `gadk-session-database` (relational databases) and `gadk-vertexai`
  (Vertex AI Agent Engine).
- `Event` is one interaction: a model response plus author, invocation ID, timestamp and
  `EventActions` (state delta, artifact delta, agent transfer, escalation, compaction).
- Session state keys are scoped by prefix: `app:` (shared by every user of an app), `user:`
  (shared by a user's sessions) and `temp:` (dropped after the current invocation).
- `compaction` summarizes older events so the prompt stays small as a conversation grows;
  `toolauth` holds the auth data model (`Scheme`, `Credential`, `Config`) in the JSON format
  ADK clients exchange; `state_util` has helpers shared by `SessionService` implementations.

Most applications reach these types through the `gadk` facade (`gadk::session`, and
`gadk::compaction`) and only depend on this crate directly to implement a `SessionService`.

## Example

```rust
use gadk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use serde_json::json;

let service = InMemorySessionService::new();

let mut state = serde_json::Map::new();
state.insert("topic".into(), json!("weather"));
let session = service
    .create(CreateRequest {
        app_name: "my_app".into(),
        user_id: "user-1".into(),
        session_id: None, // generated
        state: Some(state),
    })
    .await?;

let again = service
    .get(GetRequest {
        app_name: "my_app".into(),
        user_id: "user-1".into(),
        session_id: session.id().to_string(),
        ..Default::default()
    })
    .await?;
assert_eq!(again.state().get("topic")?, json!("weather"));
```

## Features

- `testsuite`: the `testsuite` module, a reusable conformance suite for `SessionService`
  implementations.
