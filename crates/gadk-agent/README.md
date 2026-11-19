# gadk-agent

The agent API of gadk: the `Agent` trait and the one agent implementation every agent type
builds on, the three context types (`ReadonlyContext`, `InvocationContext`, `Context`), the
`Tool` and `Toolset` traits, the agent, model and tool callbacks, plugins (`Plugin`,
`PluginConfig`), `RunConfig`, the live (bidirectional streaming) request types and the `Loader`
a server uses to find an agent by application name (`SingleLoader`, `MultiLoader`).

It also carries the execution-identity pieces the engine and the analytics plugin rely on:
fibers (`fiber`), cancel causes (`CancelCause`, with `DeadlineExceeded` for agent timeouts) and
the per-invocation teardown registry (`Teardowns`), through which work started by a component
finishes before the runner's after-run callbacks even when a consumer stops reading early.

This crate defines the API but runs nothing: the LLM agent, the workflow agents and the runner
are in `gadk-engine`. Most applications use it through the `gadk` façade (`gadk::agent`, and
the common types in `gadk::prelude`).

## Example

A custom agent whose behaviour is a function from the invocation context to a stream of events:

```rust
use std::sync::Arc;
use futures::stream;
use gadk_agent::{AgentConfig, AgentError, InvocationContext, new_agent};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_session::Event;

let greeter = new_agent(AgentConfig {
    name: "greeter".into(),
    description: "Says hello.".into(),
    run: Some(Arc::new(|ctx: InvocationContext| {
        let mut event = Event::new(ctx.platform(), ctx.invocation_id());
        event.response.content = Some(Content::from_text("Hello!", ROLE_MODEL));
        Box::pin(stream::once(async move { Ok::<Event, AgentError>(event) }))
    })),
    ..Default::default()
})?;
```

`new_agent` fails when the name is empty or a sub-agent is listed twice. An event with no author
gets the agent's name (or `user` for user-role content). Run the agent with
`gadk_engine::Runner` like any other agent.

## Tool helpers

- `filter_toolset` and `allowed_tools_predicate` / `string_predicate` restrict which of a
  toolset's tools an agent sees.
- `with_confirmation` wraps a toolset so its tools' calls need a Human-in-the-Loop confirmation,
  always or as a `ConfirmationProvider` decides per call.
- Neither toolset wrapper forwards the inner toolset's `process_request`.
