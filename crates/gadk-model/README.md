# gadk-model

The LLM abstraction of gadk. The `Llm` trait is what an LLM agent calls: `generate_content`
takes an `LlmRequest` (model name, contents, `GenerateContentConfig`, HTTP options, tools) and
returns a stream of `LlmResponse`s, one final response or partial responses followed by the
final one; models with live (bidirectional streaming) support also implement `connect_live`.
`gadk-gemini`, `gadk-anthropic` and `gadk-openai` implement it.

The crate also holds what those providers share:

- `Registry`: an opt-in, name-based model registry. Register a factory under a model-name
  pattern (a regex such as `"^(?i)gemini-.*"`), then build a model from a name with `new_llm`;
  exactly one pattern must match. Provider crates do not register themselves.
- `HttpClient`: the HTTP seam every provider sends through (`ReqwestClient` by default), so tests
  and applications can supply their own client.
- `rest`: the REST transport of `gadk-anthropic` and `gadk-openai`: one `POST` with a retry
  policy, call timeouts and per-attempt headers, and the Google API error type.
- `sse`: the incremental server-sent-events parser.
- `StreamingResponseAggregator`, `ModelError` and the Gemini model-name helpers
  (`is_gemini_model`, `is_gemini2_or_above`, …).

Most applications use this crate through the `gadk` façade (`gadk::model`).

## Example

```rust
use std::sync::Arc;
use gadk_model::{Factory, ModelError, Registry};

// Inside an async fn returning Result.
let factory: Factory = Arc::new(|name: String| {
    Box::pin(async move {
        gadk_gemini::GeminiModel::new(&name, gadk_gemini::ClientConfig::default())
            .await
            .map_err(|e| ModelError::Other(e.into()))
    })
});
Registry::global().register("^(?i)gemini-.*", factory)?;
let model = Registry::global().new_llm("gemini-2.5-flash").await?;
```

## Features

- `gcp-auth`: `gcp_auth`, Google Application Default Credentials and the request headers they
  produce, shared by the Google Cloud clients.
- `test-util`: `rest::test_util`, a scripted in-process `HttpClient`, a 127.0.0.1 fake server
  and fixed-header Google credentials for the provider crates' tests.
