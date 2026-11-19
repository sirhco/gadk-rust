# gadk-auth

Authentication for the outbound HTTP calls gadk tools make. A `Credential` (`Bearer`, `ApiKey`,
`Basic`, `OAuth2`) writes itself onto a request; a `CredentialProvider` resolves one for the
current invocation from an `AuthContext` (the invocation's identity, if any); and `Transport`, a
`reqwest_middleware` middleware, applies a provider to every request. When a request is rejected
with 401 or 403 and the provider can refresh (`RefreshingProvider`), the transport refreshes the
credential and retries once, provided the request body can be replayed.

Built-in providers: `static_token` (bearer token), `api_key` (a header), `token_source` (any
`TokenSource`), `adc` (Application Default Credentials) and `service_account` (a JSON key or
ADC, for access or ID tokens). `ProviderFn` adapts a closure. Token exchange and refresh are
delegated to `google-cloud-auth`. A provider that has to reach the network can cache what it
resolved in a `CredentialStore`; `InMemoryCredentialStore` is the process-local one.

The `gcp` module (feature `gcp`, on by default) provides per-user Google Cloud credentials from
the Agent Identity and IAM Connector credential services: `gcp::new_provider` returns a provider
that acts for the end user named in the `AuthContext`. When the user must consent first, the
provider fails with a `ConsentRequiredError`; recover it with `find_consent` or
`find_consent_in`.

The `gadk` facade re-exports this crate as `gadk::auth`. An MCP toolset authenticates its HTTP
requests with a provider set in `gadk_mcp::Config::auth`.

## Example

```rust
// Inside an async fn returning Result.
use gadk_auth::{AuthContext, adc, client};

// Application Default Credentials, cloud-platform scope.
let http = client(reqwest::Client::new(), adc(&[]), AuthContext::anonymous());
let resp = http
    .get("https://storage.googleapis.com/storage/v1/b?project=my-project")
    .send()
    .await?;
println!("{}", resp.status());
```

A `Transport` fixes its `AuthContext` at construction, so for a per-user provider (such as the
`gcp` one) build a client per invocation identity and do not share it across users. Inside a
tool, `AuthContext::from(&ctx)` takes the identity from the tool's context.

For callers that build requests themselves, `apply_credential`, `refresh_credential` and
`is_auth_rejected` are the same steps the transport runs.

## Features

| Feature | Default | Adds |
|---|---|---|
| `gcp` | yes | `gcp`: Agent Identity and IAM Connector per-user credentials |
