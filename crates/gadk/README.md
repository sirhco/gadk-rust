# gadk

The façade crate of gadk, an agent development kit for Rust modelled on Google's
[Agent Development Kit](https://google.github.io/adk-docs/) (ADK). Depend on this crate rather
than on the member crates: it re-exports each of them as a module (`gadk::agent`,
`gadk::engine`, `gadk::model`, `gadk::session`, …), puts the optional ones behind features, and
gathers the common types for building and running agents in `gadk::prelude`.

## Quick start

```toml
[dependencies]
gadk = "0.4"   # default features: gemini, gcp, mcp
tokio = { version = "1", features = ["full"] }
futures = "0.3"
```

```rust
use futures::StreamExt;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Gemini API key from GOOGLE_API_KEY / GEMINI_API_KEY, or Vertex AI with
    // GOOGLE_GENAI_USE_VERTEXAI, GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION (`us` or `global`
    // for newer Gemini models) and ADC.
    let model = GeminiModel::new("gemini-3.8-flash", ClientConfig::default()).await?;
    let agent = llm_agent::new(LlmAgentConfig {
        name: "helper".into(),
        description: "Answers questions.".into(),
        model: Some(model),
        instruction: "Answer briefly.".into(),
        ..Default::default()
    })?;

    // In-memory session, artifact and memory services.
    let runner = Runner::in_memory("quickstart", agent)?;
    let msg = Content::from_text("What is the capital of France?", gadk::genai::ROLE_USER);
    let mut events = runner.run("user", "session-1", Some(msg), RunConfig::default(), RunOptions::default());
    while let Some(event) = events.next().await {
        if let Some(content) = &event?.response.content {
            for part in &content.parts {
                print!("{}", part.text);
            }
        }
    }
    runner.close().await?;
    Ok(())
}
```

`gadk::prelude` also brings in the agent and tool traits, tools (`FunctionTool`, `AgentTool`,
`StreamingFunctionTool`, `exit_loop`), workflow agents (`sequential_agent`, `parallel_agent`,
`loop_agent`), graph workflows (`workflow_agent`, `function_node`, `Edge`, `START`) and the
in-memory session, artifact and memory services. More programs are in `examples/` (`cargo run -p
gadk --example workflow` needs no model or credentials).

## Modules and features

Always present: `agent`, `artifact`, `auth`, `compaction`, `engine`, `genai`, `memory`, `model`,
`platform`, `session` and `tools`. The rest are behind features:

| Feature | Default | Adds |
|---|---|---|
| `gemini` | yes | Gemini API and Vertex AI models (`gadk::gemini`) |
| `gcp` | yes | Google Cloud credentials in `gadk::auth` |
| `mcp` | yes | Model Context Protocol toolsets (`gadk::mcp`) |
| `anthropic` | | Claude models, Anthropic API and Vertex AI (`gadk::anthropic`) |
| `openai` | | OpenAI Responses and Chat Completions models (`gadk::openai`) |
| `database` | | Relational-database sessions: sqlite, or PostgreSQL (`gadk::session_database`) |
| `gcs` | | Artifacts in Google Cloud Storage (`gadk::artifact_gcs`) |
| `vertexai` | | Sessions and memory in Vertex AI Agent Engine (`gadk::vertexai`) |
| `prompts` | | Versioned, labelled prompt registry (`gadk::prompts`) |
| `prompts-firestore` | | The prompt registry's Firestore store (`gadk::prompts_firestore`); enables `prompts` |
| `server` | | ADK REST API server with authn/authz (`gadk::server`) |
| `a2a` | | Agent2Agent protocol: serve agents and call remote agents (`gadk::a2a`) |
| `launcher` | | Console and web launchers (REST, A2A, web UI, triggers) (`gadk::launcher`); enables `telemetry` |
| `agentengine` | | Agent Engine server and launcher (`gadk::agentengine`); enables `launcher` |
| `telemetry` | | OpenTelemetry export (`gadk::telemetry`) |
| `agentregistry` | | Google Cloud Agent Registry client (`gadk::agentregistry`) |
| `agentanalytics` | | BigQuery Agent Analytics plugin (`gadk::agentanalytics`) |
| `bigquery` | | BigQuery toolset: list datasets and tables, read metadata, run SQL (`gadk::bigquery`) |
| `openapi` | | OpenAPI toolset: one tool per operation of an OpenAPI 3.0 or 3.1 document (`gadk::openapi`) |
| `eval` | | Offline agent evaluation: eval sets, scorers, LLM judge (`gadk::eval`) |

For PostgreSQL sessions, also enable the `postgres` feature of `gadk-session-database` in your
own manifest; `database` alone gives sqlite.

See the [repository README](https://github.com/sirhco/gadk-rust#crates) for the full list of
crates and binaries.
