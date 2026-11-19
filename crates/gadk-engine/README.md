# gadk-engine

The gadk runtime. It implements the agents defined by `gadk-agent` and runs them:

- `llm`: the LLM agent (`llm_agent::new` with an `LlmAgentConfig`: model, instruction or
  instruction provider, tools and toolsets, sub-agents and transfer, input and output schemas,
  callbacks) and the flow that drives model calls, tool calls and agent transfer.
- `agents`: workflow agents: `sequential_agent`, `parallel_agent`, `loop_agent` and
  `workflow_agent`, which runs a graph from `workflow` (function, agent, join and parallel-worker
  nodes, `Edge`s from `START`, routes, retries, human input requests).
- `runner`: `Runner` runs an agent against a session service (plus optional artifact and memory
  services, plugins and context compaction), appends its events to the session and streams them
  back; `run_live` runs a live (bidirectional streaming) session.
- `tool`: `FunctionTool` (a typed async function; input and output schemas come from the
  argument and result types), `StreamingFunctionTool`, `AgentTool` (an agent called as a tool),
  and `exit_loop`.
- `plugin`: built-in plugins: `logging_plugin`, `new_retry_and_reflect_plugin` and
  `new_function_call_modifier_plugin`.

Dropping a runner stream early unwinds rather than aborts: started work (tool calls, parallel
branches, workflow nodes) finishes before the after-run callbacks. Most applications use this
crate through the `gadk` façade (`gadk::engine`, and the common types in `gadk::prelude`).

## Example

```rust
use futures::StreamExt;
use gadk_agent::{Context, RunConfig, ToolError};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_engine::{RunOptions, Runner, llm_agent};
use gadk_genai::{Content, ROLE_USER};

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct WeatherInput { city: String }

#[derive(serde::Serialize, schemars::JsonSchema)]
struct WeatherOutput { report: String }

async fn get_weather(_ctx: Context, input: WeatherInput) -> Result<WeatherOutput, ToolError> {
    Ok(WeatherOutput { report: format!("Sunny in {}.", input.city) })
}

// Inside an async fn returning Result; `model` is any `Arc<dyn gadk_model::Llm>`.
let weather = FunctionTool::new(
    FunctionToolConfig::new("get_weather", "Returns the weather in a city."),
    get_weather,
)?;
let agent = llm_agent::new(LlmAgentConfig {
    name: "weather".into(),
    description: "Answers weather questions.".into(),
    model: Some(model),
    instruction: "Use get_weather to answer.".into(),
    tools: vec![weather],
    ..Default::default()
})?;

let runner = Runner::in_memory("weather_app", agent)?;
let msg = Content::from_text("Weather in Paris?", ROLE_USER);
let mut events = runner.run("user", "s1", Some(msg), RunConfig::default(), RunOptions::default());
while let Some(event) = events.next().await {
    let event = event?;
    // event.author, event.response.content, …
}
runner.close().await?;
```

`Runner::in_memory` uses in-memory session, artifact and memory services and creates the
session on its first run; `Runner::new` takes a `RunnerConfig` with your own services.

## Features

- `testutil`: test support (`gadk_engine::testutil`): a mock model, stream collection helpers,
  Gemini record/replay helpers and a test agent runner.
