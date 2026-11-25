# gadk-analytics

Typed, parameterized BigQuery queries over the agent-analytics table (`agent_events`) that
[gadk](../../README.md) agents write with `gadk-agentanalytics`: token usage and cost, sessions
and users, prompt-version metrics, scores and review queues. It is the query layer of
`gadk-console`, as a library for your own dashboards, reports and jobs.

## Quick example

```rust,ignore
use chrono::{TimeDelta, Utc};
use gadk_analytics::{AnalyticsSource, BigQueryTable};
use gadk_bigquery::BigQueryClient;

let source = AnalyticsSource::new(
    BigQueryClient::new(),                    // Application Default Credentials
    "my-billing-project",                     // runs and pays for the queries
    BigQueryTable::parse_url("bigquery://my-project.agent_analytics.agent_events")?,
    1_073_741_824,                            // byte cap per query (1 GiB)
);
let end = Utc::now();
let breakdown = source.breakdown(end - TimeDelta::hours(24), end).await?;
for cell in &breakdown.cells {
    println!("{} {} {:?}: {} calls", cell.calls.model, cell.agent, cell.prompt, cell.calls.calls);
}
```

Every query returns `Result<_, SourceError>`; `SourceError::hint` gives a short fix for
permission and byte-cap failures. Every result type implements `serde::Serialize` (timestamps
as RFC 3339), so results can be served as JSON as they are.

## Modules

| Module | Queries on `AnalyticsSource` | Results |
|---|---|---|
| `usage` | `breakdown`, `trend`, `top_traces` | Calls and tokens per model, agent and prompt version (`Breakdown`), the same per time bucket (`Trend`; `UsageRange::bucket` gives the widths the console uses), the traces with the most tokens (`TopTrace`) |
| `sessions` | `sessions`, `users`, `session_turns` | Sessions (`SessionPage`, filtered and paged with `SessionQuery` and `SessionCursor`), users (`UserPage`, `UserQuery`), the turns of a session (`TurnRow`) |
| `prompts` | `version_metrics`, `version_feedback`, `versions_for_invocations` | Per-version calls, mean tokens and p50/p95 latency (`VersionMetrics`), per-version scores (`VersionFeedback`), the versions each invocation used (`InvocationVersions`) |
| `scores` | `trace_scores`, `trace_root`, `insert_score`, `insert_scores` | The scores of a trace (`TraceScores`), the identity a new score is written under (`TraceRoot`), and adding scores (`NewScore`) |
| `review` | `pending`, `next_pending` | The traces of a `ReviewQueue` still missing a console score for some of the given score names (`PendingTrace`, paged with `Cursor`) |
| `cost` | none | `Usage` (four token counts), `cost_of` (a usage priced with a `gadk_eval_monitor::PriceTable`), `CostSum` (a sum that remembers unpriced calls) |
| `row` | none | The row readers and query-parameter builders the queries share |
| `testutil` (feature `test-util`) | none | `FakeHttp`, a scripted HTTP client to build a `BigQueryClient` over, and row fixtures |

Results are grouped by model wherever tokens are counted, so they can be priced with
`cost::cost_of`.

## Costs and caps

Every query runs as a BigQuery job in the billing project given to `AnalyticsSource::new`, which
BigQuery bills, and stops with a `bytesBilledLimitExceeded` error past the byte cap. Every query
filters `timestamp` to its window, so a table partitioned by day on `timestamp` is read only for
the days asked for. Every value is a query parameter; no input is spliced into SQL.

Results are capped, and say when a cap cut them (`truncated`, `has_next`, `more`):

| Cap | Value |
|---|---|
| `usage::BREAKDOWN_CAP` | 5,000 groups |
| `usage::TREND_CAP` | 20,000 groups |
| `usage::TOP_TRACES` | 20 traces (the console's count; `top_traces` takes any limit) |
| `sessions::SESSION_CAP`, `USER_CAP` | 100 sessions or users per page (`CSV_CAP`: 5,000) |
| `sessions::ROW_CAP` | 5,000 session or user × model rows (`CSV_ROW_CAP`: 50,000) |
| `sessions::TURN_CAP` | 200 turns |
| `prompts::FEEDBACK_CAP` | 5,000 version × score × value rows |
| `scores::TRACE_SCORES_CAP` | 200 scores |
| `review::REVIEW_CAP` | 100 pending traces |

The credentials need `roles/bigquery.dataViewer` on the table and `roles/bigquery.jobUser` in
the billing project.

## Writes

`AnalyticsSource::insert_score` and `insert_scores` are the only writes. Each is one DML
statement that inserts `FEEDBACK` rows into the table (`insert_scores` inserts all its rows or
none). They need `roles/bigquery.dataEditor` on the table (`scores::BQ_EDITOR_HINT`), and
BigQuery bills every DML statement at least 10 MiB, so their byte cap is raised to
`scores::DML_MIN_BYTES` when it is lower. `NewScore::new` checks a score as the console's form
does: a non-blank name, a non-blank category, a finite number.

## Testing

With the `test-util` feature, `testutil::FakeHttp` answers each request with the next scripted
`jobs.query` reply (`testutil::reply`) and records what it was sent:

```rust,ignore
let http = gadk_analytics::testutil::FakeHttp::new(vec![gadk_analytics::testutil::reply(
    &gadk_analytics::testutil::TREND_FIELDS,
    vec![],
    false,
)]);
let client = gadk_bigquery::BigQueryClient::with_client(http.clone(), std::sync::Arc::new(gadk_bigquery::NoAuth));
```

## License

Apache-2.0.
