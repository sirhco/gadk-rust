# gadk-console-client

A typed async client for the read-only JSON API of [gadk-console](../gadk-console/README.md)
(`/api/v1/…`). Its types, query builders and methods are generated from the console's OpenAPI
document, so they follow the routes the console serves.

## Install

```toml
[dependencies]
gadk-console-client = "0.5"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Quick start

Start a console with the BigQuery trace source (it listens on `http://127.0.0.1:8765` by
default):

```sh
gadk-console --project my-billing-project \
  --traces bigquery://my-data-project.agent_analytics.agent_events
```

then:

```rust,no_run
use gadk_console_client::{
    Client, Error, GetUsageBreakdownQuery, ListSessionsQuery, Range, UsageRange,
};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let client = Client::new("http://127.0.0.1:8765")?;

    let usage = client
        .get_usage_breakdown(&GetUsageBreakdownQuery::new().range(UsageRange::D7))
        .await?;
    for cell in &usage.data.cells {
        println!("{} {}: {} calls", cell.calls.model, cell.agent, cell.calls.calls);
    }

    let sessions = client
        .list_sessions(&ListSessionsQuery::new().range(Range::H24).agent("triage"))
        .await?;
    println!("{} sessions, more: {}", sessions.data.len(), sessions.next.is_some());
    Ok(())
}
```

Each method takes its query by reference and returns the route's envelope: `data`, `truncated`
and `next`. A query's `new` takes the path parameters (unencoded: the client percent-encodes
each one as a single path segment) and its setters take the optional ones; a parameter you do not
set is not sent, so the console's default applies.

| Method | Route | Query setters | `data` |
|---|---|---|---|
| `get_usage_breakdown` | `/api/v1/usage/breakdown` | `range(UsageRange)` | `UsageBreakdown` |
| `get_usage_trend` | `/api/v1/usage/trend` | `range(UsageRange)` | `UsageTrend` |
| `get_usage_top_traces` | `/api/v1/usage/top-traces` | `range(UsageRange)` | `Vec<PricedTopTrace>` |
| `list_sessions` | `/api/v1/sessions` | `range`, `user`, `agent`, `after`, `after_id` | `Vec<PricedSessionRow>` |
| `get_session_turns` | `/api/v1/sessions/{id}/turns` | `range` | `SessionTurns` |
| `list_users` | `/api/v1/users` | `range`, `agent` | `Vec<PricedUserRow>` |
| `get_prompt_versions` | `/api/v1/prompts/{name}/versions` | `range` | `PromptVersions` |
| `get_trace_scores` | `/api/v1/traces/{id}/scores` | none | `TraceScores` |
| `list_pending_reviews` | `/api/v1/review/pending` | `score` or `rubric`, `prompt`, `version`, `flagged(bool)`, `range` | `Vec<PendingTrace>` |
| `get_open_api` | `/api/v1/openapi.json` | none | the document as `serde_json::Value` |

`range` takes a `Range` (`H1`, `H6`, `H24`, `D7`) everywhere except the usage routes, which take
a `UsageRange` (also `D30`). The console's [JSON API](../gadk-console/README.md#json-api) section
describes each route's defaults and fields.

Pass your own `reqwest::Client` with `Client::with_http(base_url, http)` for timeouts, a proxy or
extra headers such as `Authorization`. The base URL must be `http` or `https` with no path,
query, fragment or credentials (a user name or password in it is an `InvalidUrl` error).
`Client::new` does not follow redirects, so a 3xx reply is an error with its status; a client
you pass keeps its own redirect policy.

The public API exposes reqwest 0.13 types (`Error::Transport(reqwest::Error)` and the
`reqwest::Client` of `with_http`), so a reqwest major-version upgrade is a breaking release of
this crate.

## Errors

Every method returns `Result<T, gadk_console_client::Error>`:

| Variant | When |
|---|---|
| `Api { status, kind, message, hint }` | A non-2xx reply with the console's JSON error (`bad_request`, `needs_bigquery`, `not_found`, `method_not_allowed`, or a query failure's kind such as `bigquery`) |
| `Http { status, body }` | A non-2xx reply that is not the console's JSON error, such as a proxy's page; `body` is its first 1024 bytes |
| `Transport(reqwest::Error)` | The request could not be sent or the reply could not be read |
| `Decode { status, message }` | A 2xx body that does not decode into the route's type |
| `InvalidUrl(String)` | A bad base URL, a path parameter that is `.`, `..` or empty, or a paging link that leaves the console |

The struct variants are `#[non_exhaustive]`: match them with `..`, as in
`Error::Api { status, kind, .. }`.

`Error::is_needs_bigquery()` is true when the console is not using the BigQuery trace source,
and `Error::is_not_found()` for an unknown trace, a session without activity in the range or an
unknown route.

## Paging

`/api/v1/sessions` answers at most 100 sessions per page, with `next` set when more follow.
`list_sessions_pages` follows the links for you:

```rust,no_run
use gadk_console_client::{Client, Error, ListSessionsQuery, Range};

async fn every_session(client: &Client) -> Result<(), Error> {
    let mut pages = client.list_sessions_pages(&ListSessionsQuery::new().range(Range::D7));
    while let Some(page) = pages.next_page().await? {
        for s in &page.data {
            println!("{} {}", s.session_id, s.last);
        }
    }
    // Or collect the rows of at most 5 pages:
    let rows = client.list_sessions_all(&ListSessionsQuery::new(), 5).await?;
    println!("{} sessions", rows.len());
    Ok(())
}
```

After the page without `next`, or after a page fails (the error is returned once),
`next_page` returns `Ok(None)`.

## Test values

The response types are `#[non_exhaustive]` and have no constructors. Build values for your own
tests from JSON:

```rust
use gadk_console_client::SessionsResponse;

let page: SessionsResponse = serde_json::from_value(serde_json::json!({
    "data": [{
        "session_id": "s-1", "user": "u-1", "agent": "triage",
        "first": "2026-11-16T11:00:00Z", "last": "2026-11-16T11:05:00Z",
        "turns": 2, "errors": 0, "ups": 1, "downs": 0,
        "tokens": [["gemini-3.8-flash", {"input": 900, "cached": 0, "output": 120, "reasoning": 0}]],
        "cost_usd": null, "cost_partial": false
    }],
    "truncated": false,
    "next": null
}))
.unwrap();
assert_eq!(page.data[0].tokens[0].1.output, 120);
```

Unknown JSON fields are ignored, so a newer console that adds fields does not break the client.

## Example

```sh
cargo run -p gadk-console-client --example console_client -- http://127.0.0.1:8765
```

prints the usage breakdown and the first sessions page.

## Regenerating

`src/generated.rs` is generated; do not edit it. After changing the console's
`crates/gadk-console/src/web/api/openapi.json`, run from the repository root:

```sh
python3 scripts/gen-console-client.py          # rewrites src/generated.rs
python3 scripts/gen-console-client.py --check  # exit 1 when src/generated.rs is stale
```

The generator needs Python 3 and the workspace's `rustfmt`. It reads `components.schemas`,
`components.parameters` and each path's `get`, and stops with an error naming the schema or
parameter when the document uses a construct it does not support or a schema property has no
`description`. The console's API tests deserialize every 200 body they produce into these types,
so a change to the document that the client cannot read fails there too.
