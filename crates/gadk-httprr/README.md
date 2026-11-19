# gadk-httprr

HTTP record/replay for deterministic tests of code that talks to model APIs. `RecordReplay` is
a `gadk_model::HttpClient` backed by a trace file: in record mode it forwards each request to a
real client and appends the exchange to the trace; in replay mode it answers from the trace and
never touches the network. The trace format is `httprr trace v1`, the format ADK's test
fixtures use, so those fixtures replay unmodified.

This is a test utility: add it as a dev-dependency.

## Example

```rust
use std::sync::Arc;
use gadk_httprr::RecordReplay;
use gadk_model::ReqwestClient;

let rr = Arc::new(RecordReplay::open(
    "tests/testdata/my_test.httprr",
    Arc::new(ReqwestClient::default()),
)?);
// Use `rr` as the model's HTTP client, e.g. `ClientConfig { http_client: Some(rr.clone()), .. }`.
// ... run the test ...
rr.close()?; // in record mode, flushes the trace and reports any write error
```

## Recording

`RecordReplay::open` records when the environment variable `GADK_HTTPRR_RECORD` holds a regex
matching the trace path, and replays otherwise:

```sh
GADK_HTTPRR_RECORD='my_test' cargo test -p my-crate my_test
```

Recording truncates the file (its directory must exist). `open_with` takes the pattern as an
argument instead, and `RecordReplay::recording(path)` reports whether a path would be recorded,
for example to choose between a real API key and a fake one.

## Matching and scrubbing

Replay matches on method, URL (ignoring any `key` query parameter) and `canonical_body`, so
bodies that differ only in JSON key order, whitespace or integral-float spelling still match.
Headers are not compared. A miss fails with `HttprrError::NoMatch`, which includes a diff
against the closest logged body with the same method and URL.

The logged copy of each request has the `x-goog-api-key` and `Authorization` headers and any
`key` query parameter removed. Add more with `scrub_req` (runs on a copy of the request, also
before replay lookup) and `scrub_resp` (runs on the logged response bytes).
