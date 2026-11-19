# gadk-openai

OpenAI models for gadk, on the Responses API or the Chat Completions API, over gadk's own REST
transport (`gadk_model::rest`). `new_model` takes a model name and a `ClientConfig` and returns
the model as an `Arc<dyn Llm>` for an LLM agent. `ClientConfig::api` selects the API:
`Api::Responses` (the default) or `Api::ChatCompletions`, the surface most OpenAI-compatible
servers (Ollama, LM Studio, vLLM, …) implement.

Every `GenerateContentConfig` field is translated, rejected with an error naming it, or
ignored, per API. Errors are `OpenAiError`, with an `ErrorKind`; an API failure carries an
`ApiError` with the HTTP status and response body.

Most applications use this crate through the `gadk` façade (`gadk::openai`, feature `openai`,
not on by default).

## Configuration

| Field | When empty |
|---|---|
| `api_key` | `OPENAI_API_KEY`; sent as `Authorization: Bearer <key>` |
| `base_url` | `OPENAI_BASE_URL`, else `DEFAULT_BASE_URL`; a `/` is appended when the path lacks one |
| `api` | `Api::Responses` |
| `http_client` | a `reqwest` client with a 10-minute response-header timeout |
| `headers` | none; sent after the environment's, and an `Authorization` here replaces the key's |

`env` replaces the environment source, for tests. `Api` parses from `"responses"` (or `""`) and
`"chat_completions"`.

## Example

```rust
use gadk_openai::{Api, ClientConfig, new_model};

// OpenAI, key from OPENAI_API_KEY.
let gpt = new_model("gpt-4o-mini", ClientConfig::default())?;

// A local OpenAI-compatible server on the Chat Completions API.
let local = new_model(
    "llama3.2",
    ClientConfig {
        base_url: "http://localhost:11434/v1".into(),
        api: Api::ChatCompletions,
        ..Default::default()
    },
)?;
```

`new_model` is synchronous and fails only on an empty model name; the environment is read when
it is called. Either model goes into `LlmAgentConfig::model`. The `gadk` crate's `openai`
example runs an agent with a function tool on either API.
