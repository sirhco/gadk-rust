# gadk-gemini

Gemini models for gadk, on the Gemini Developer API or on Vertex AI. `GeminiModel::new` takes a
model name and a `ClientConfig` and returns the model as an `Arc<dyn Llm>` for an LLM agent
(`GeminiModel::new_concrete` returns the concrete type). It calls `generateContent` and
`streamGenerateContent`, and opens live (bidirectional streaming) connections over WebSocket
for `Runner::run_live`.

The `apigee` module serves Gemini through an Apigee proxy (`ApigeeModel`, model names
`apigee/[vertex_ai/|gemini/][vN/]<model>`).

Most applications use this crate through the `gadk` façade (`gadk::gemini`, feature `gemini`,
on by default).

## Configuration

`ClientConfig::default()` resolves everything from the environment:

| Field | When unset |
|---|---|
| `backend` | Vertex AI when `GOOGLE_GENAI_USE_ENTERPRISE` (or, if that is unset, `GOOGLE_GENAI_USE_VERTEXAI`) is true, else the Gemini API |
| `api_key` (Gemini API) | `GOOGLE_API_KEY`, then `GEMINI_API_KEY` |
| `project` (Vertex AI) | `GOOGLE_CLOUD_PROJECT` |
| `location` (Vertex AI) | `GOOGLE_CLOUD_LOCATION`, then `GOOGLE_CLOUD_REGION` |
| `credentials` (Vertex AI) | Application Default Credentials |

`http_options` sets the base URL, API version and extra headers; `http_client` replaces the HTTP
client (ADC is then not looked up); `env` replaces the environment source, for tests. Setting both
`credentials` and an API key is an error.

## Example

```rust
use gadk_gemini::{ClientConfig, GeminiModel};
use gadk_model::Backend;

// Inside an async fn returning Result.
// Gemini API, key from GOOGLE_API_KEY / GEMINI_API_KEY.
let flash = GeminiModel::new("gemini-2.5-flash", ClientConfig::default()).await?;

// Vertex AI with Application Default Credentials.
let pro = GeminiModel::new(
    "gemini-2.5-pro",
    ClientConfig {
        backend: Some(Backend::VertexAi),
        project: Some("my-project".into()),
        location: Some("us-central1".into()),
        ..Default::default()
    },
)
.await?;
```

Either model goes into `LlmAgentConfig::model`.
