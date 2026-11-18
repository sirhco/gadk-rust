# gadk-cloudtrace

A small read client for the Google Cloud Trace v1 REST API: list a project's traces in a time
window, and get one trace with all of its spans. `gadk-console` reads its traces with it, and
`gadk-eval-monitor` rebuilds production runs from it.

```rust
// Inside an async fn returning Result<_, gadk_cloudtrace::TraceError>.
use chrono::{TimeDelta, Utc};
use gadk_cloudtrace::{CloudTraceClient, Filter, ListRequest, View};

let client = CloudTraceClient::new("my-proj"); // Application Default Credentials, built on first use
let end = Utc::now();
let page = client
    .list(&ListRequest {
        start: end - TimeDelta::hours(1),
        end,
        view: View::RootSpan,
        filters: vec![Filter::Span("invoke_agent".into())],
        start_descending: true,
        page_size: 50,
        page_token: None,
    })
    .await?;
for summary in &page.traces {
    let trace = client.get(&summary.trace_id).await?;
    println!("{}: {} spans", trace.trace_id, trace.spans.len());
}
// page.next_page_token, when set, goes in the next request's `page_token`.
```

## Requests

`list` sends `GET /v1/projects/{project}/traces` with, in this order: `view` (`ROOTSPAN` for
`View::RootSpan`, `COMPLETE` for `View::Complete`), `orderBy=start desc` (only when
`start_descending`), `pageSize`, `startTime` and `endTime` (RFC 3339, UTC), `filter` (when a term
is left) and `pageToken` (when set and non-empty). Every value is percent-encoded. `get` sends
`GET /v1/projects/{project}/traces/{trace_id}`.

## Filters

| Filter | Sent as | Matches traces with |
|---|---|---|
| `Filter::Root(prefix)` | `root:"<prefix>"` | a root span whose name starts with `prefix` |
| `Filter::Span(prefix)` | `span:"<prefix>"` | any span whose name starts with `prefix` |
| `Filter::LabelExact(key, value)` | `+<key>:"<value>"` | a span whose label `key` is exactly `value` |

Values are double-quoted with `\` and `"` backslash-escaped. A term with an empty prefix or value
is not sent. Terms are joined by spaces: all must match.

## Spans

Span ids are 16 lowercase hex characters (Cloud Trace v1 sends decimal ids, which are
converted). A parent id that is absent, zero or not a decimal number is `None`. On `get` and on a
`View::Complete` list, a span id that is not a decimal number is a `TraceError::Decode`; a
`View::RootSpan` list keeps such an id as sent, so one odd span never fails a list page. Trace
ids are returned as Cloud Trace sent them; callers validate them.

## Errors

| `TraceError` | When |
|---|---|
| `Http { status, reason, excerpt }` | a non-2xx reply (a 404 from `list` too); `reason` is the `google.rpc.ErrorInfo` reason (such as `SERVICE_DISABLED`), `excerpt` at most 1,024 bytes of the body |
| `NotFound` | `get` got HTTP 404 |
| `Transport(detail)` | the request could not be built or sent, or the reply could not be read |
| `Decode(detail)` | the reply is not the expected JSON, or a strict span id is not decimal |
| `Credentials(detail)` | no request credentials |

## Credentials and access

Every request carries the headers of an `AuthHeaders` implementation and
`x-goog-user-project: <project>` (replacing any quota project the credentials carry).
`Adc::lazy()` builds Application Default Credentials (cloud-platform scope) on first use;
`Adc::resolve()` builds them at once and fails when there are none (call it inside a Tokio
runtime); `NoAuth` sends none; `CloudTraceClient::with_auth` and `with_parts` take any
implementation. Reading traces needs Cloud Trace User (`roles/cloudtrace.user`) on the project.
