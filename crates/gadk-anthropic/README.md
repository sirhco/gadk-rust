# gadk-anthropic

Claude models for gadk, on the Anthropic Messages API or on Google Cloud Vertex AI, over gadk's
own REST transport (`gadk_model::rest`) with the Anthropic SDK's retry policy.
`AnthropicModel::new` takes a model name (`claude-…`, `anthropic/claude-…`, or a Vertex
resource name) and a `ClientConfig` and returns the model as an `Arc<dyn Llm>` for an LLM agent
(`AnthropicModel::new_concrete` returns the concrete type). It supports streaming, function
tools and tool choice, images, thinking, effort, structured output and prompt caching. Each
request's config is validated: an option Claude cannot honour is either rejected with an
`AnthropicError` or ignored.

Most applications use this crate through the `gadk` façade (`gadk::anthropic`, feature
`anthropic`, not on by default).

## Configuration

`ClientConfig::default()` uses the Anthropic backend and resolves credentials from the
environment:

| Backend | Credentials and endpoint when unset in `ClientConfig` |
|---|---|
| `Backend::Anthropic` (default) | `api_key` from `ANTHROPIC_API_KEY`, else `auth_token` from `ANTHROPIC_AUTH_TOKEN`; base URL from `ANTHROPIC_BASE_URL` |
| `Backend::Vertex` | project from the resource name, `GOOGLE_CLOUD_PROJECT`, then `CLOUD_ML_PROJECT_ID`; location from the resource name, `GOOGLE_CLOUD_LOCATION`, then `CLOUD_ML_REGION`; Application Default Credentials |

With a custom `base_url` the environment's credentials are never sent: set `api_key` /
`auth_token` (Anthropic) or `credentials` (Vertex) explicitly. Other fields: `max_tokens`
(used when the request sets no `max_output_tokens`; default `DEFAULT_MAX_TOKENS`, 8192),
`max_retries` (default 2), `effort` (`Effort::Low` … `Effort::Max`), `prompt_cache`
(ephemeral prompt caching with a 5-minute or 1-hour TTL), `headers`, `http_client` and `env`.

## Example

```rust
use gadk_anthropic::{AnthropicModel, Backend, ClientConfig};

// Inside an async fn returning Result.
// Anthropic API, key from ANTHROPIC_API_KEY.
let claude = AnthropicModel::new("claude-sonnet-4-5-20250929", ClientConfig::default()).await?;

// Vertex AI with Application Default Credentials (Vertex AI serves no `-latest` aliases).
let on_vertex = AnthropicModel::new(
    "claude-sonnet-4-5-20250929",
    ClientConfig {
        backend: Backend::Vertex,
        project: Some("my-project".into()),
        location: Some("us-east5".into()),
        ..Default::default()
    },
)
.await?;
```

Either model goes into `LlmAgentConfig::model`. The `gadk` crate's `anthropic` example runs an
agent with a function tool and an inline image on either backend.
