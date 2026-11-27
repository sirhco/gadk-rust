# gadk-openapi

An OpenAPI toolset for [gadk](../../README.md) agents. It turns an OpenAPI 3.0 or 3.1 document
(JSON or YAML) into tools, one per operation, so an agent can call any REST API that has a spec.

## Quick start

```rust,ignore
use std::sync::Arc;
use gadk::openapi::{OpenApiConfig, OpenApiSpec, OpenApiToolset};

// From a string (`text.parse()`), a file (`OpenApiSpec::from_file`) or a URL:
let spec = OpenApiSpec::fetch("https://petstore3.swagger.io/api/v3/openapi.json").await?;
let petstore = Arc::new(OpenApiToolset::new(
    &spec,
    OpenApiConfig::new()
        .prefix("petstore")
        .auth(gadk::auth::static_token(std::env::var("PETSTORE_TOKEN")?)),
)?);
for skipped in petstore.skipped_operations() {
    eprintln!("skipped {} {}: {}", skipped.method, skipped.path, skipped.reason);
}
// LlmAgentConfig { toolsets: vec![petstore], .. }
```

Enable it in the `gadk` crate with the `openapi` feature (`gadk::openapi`), or depend on
`gadk-openapi` directly. See `crates/gadk/examples/openapi_agent.rs` for a full agent over an
embedded Open-Meteo forecast spec on `gemini-3.8-flash` (Vertex AI, location `us`).

## Tools

- **Name:** the snake_case of `operationId` (`listPets` → `list_pets`, `getHTTPResponse` →
  `get_http_response`, `2fa` → `op_2fa`). Without an `operationId` the name comes from the
  method and path (`GET /pets/{petId}` → `get_pets_pet_id`). Names are cut to 64 bytes; two
  operations with the same name fail `OpenApiToolset::new` with `Error::DuplicateName`.
- **Description:** `summary` and `description` separated by a blank line, or whichever exists,
  or `<METHOD> <path>`; at most 1,024 characters.
- **Arguments:** one property per path, query and header parameter (named after the parameter,
  with characters outside `[A-Za-z0-9_]` turned into `_`), plus `body` for an
  `application/json` (or `application/<x>+json`) request body. When two parameters map to the
  same property, or one maps to `body`, each gets a `_<in>` suffix (`id_path`, `id_query`).
  Two properties that still clash after the suffix fail `OpenApiToolset::new` with
  `Error::DuplicateName` (named `<tool>.<property>`).
- **Schemas:** local `$ref`s (`#/components/…`) are expanded inline. A `$ref` that repeats
  on its own path (recursion), is more than 8 levels deep, or comes after a schema has grown
  past 10,000 nodes becomes `{}`, so large and recursive specs still load. `nullable`,
  `discriminator`, `xml`, `example` and `externalDocs` are removed, and in a 3.0 document
  `nullable: true` becomes `type: [T, "null"]`.
- **Skipped operations:** an operation with a cookie parameter, one with a parameter whose `in`
  is unknown (reason `parameter <name> has unsupported location "<in>"`), one whose request
  body has no JSON content type, and every operation of a path item that is itself a `$ref`.
  They are listed by `OpenApiToolset::skipped_operations()`.

## Configuration

| `OpenApiConfig` method | Default | Effect |
|---|---|---|
| `base_url(url)` | `servers[0].url` | Base of every request URL. Server variables take their defaults; a relative server URL is resolved against the URL a fetched spec came from. The base must be absolute and have no query or fragment, else `new` fails with `Error::NoBaseUrl`. |
| `auth(provider)` | none | Sends every request through `gadk_auth::client` with this credential provider. |
| `query_api_key(name, value)` | none | Appends `name=value` to every query string. |
| `prefix(prefix)` | `""` | Exposes each tool as `<prefix>_<name>` (at most 64 bytes, else `Error::NameTooLong`). |
| `tool_filter(predicate)` | all tools | Keeps the tools the predicate accepts; it sees the unprefixed names. |
| `require_confirmation(mode)` | `Mutating` | `Mutating`, `Always` or `Never`. |
| `confirmation_provider(provider)` | none | Decides per call, overriding the mode; called with the exposed name and the args. |
| `header(name, value)` | none | Adds a header to every request. |
| `timeout(duration)` | 30 s | Limit on one whole call, body read included. |
| `max_response_bytes(n)` | 262,144 | Longer bodies are cut. |

A large spec makes many declarations. Keep only the operations the agent needs with an
allow-list:

```rust,ignore
use gadk::agent::toolset::allowed_tools_predicate;

let config = OpenApiConfig::new()
    .tool_filter(allowed_tools_predicate(&["find_pets_by_status", "get_pet_by_id"]));
```

## Auth

- `auth(provider)` takes any `gadk_auth` credential provider: `static_token` (a bearer token),
  `api_key(name, value)` (a header), `token_source`, `adc` or `service_account`. With a
  provider that can refresh its credential (`adc` and `service_account`), a request rejected
  with 401 or 403 is retried once with a refreshed credential; `static_token`, `api_key` and
  `token_source` are not retried.
- `query_api_key(name, value)` puts an API key in the query string, for APIs that want it there.
- Both may be set. With neither, requests carry no credential.

Credentials are static per toolset: they are not chosen from the document's `securitySchemes`,
and there is no interactive per-user OAuth consent.

## Results

A call returns, for any HTTP status:

```json
{"status": 404, "body": {"error": "no such pet"}}
```

`body` is the parsed JSON when the response `Content-Type` is JSON and the bytes parse, else the
text (lossy UTF-8); an empty body is `null`. A body over `max_response_bytes` is cut, returned as
text, and the result gains `"truncated": true`. Redirects are not followed: a 3xx comes back as a
normal result.

A missing required argument, a non-object `args`, an object or array for a path or header
value, an object or a nested array for a query value (an array of scalars repeats the key), an
empty path value, or a path value that makes a `.` or `..` segment (alone or together with the
segment's other text, as `{x}{y}` with `.` and `.`) is `ToolError::InvalidArgument`, and no
request is sent. A transport failure is
`ToolError::Execution` holding `Error::Timeout`, `Error::Cancelled` (the invocation was
cancelled) or `Error::Request` (connection, DNS, the credential provider, or the body read).

## Confirmation

By default (`RequireConfirmation::Mutating`) calls of `post`, `put`, `patch` and `delete`
operations need the user's confirmation. Such a call requests confirmation and returns
`ToolError::ConfirmationRequired` without sending anything; once the user confirms, the call is
sent, and a rejected call returns `ToolError::ConfirmationRejected`. `Always` confirms every call
and `Never` none; a `confirmation_provider` overrides the mode per call.

## Limits

Not supported: Swagger 2.0, remote `$ref`s, cookie parameters, form and multipart bodies,
choosing credentials from `securitySchemes`, interactive per-user OAuth, Google Discovery
documents, response schemas in the declaration, and streaming responses. Fetched documents are
limited to 10 MiB and 30 s.

## License

Apache-2.0.
