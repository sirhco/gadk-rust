# gadk-bigquery

A BigQuery toolset for [gadk](../../README.md) agents. It gives an agent five tools:

| Tool | Arguments | Returns |
|---|---|---|
| `list_dataset_ids` | `project_id` | `{"status":"SUCCESS","dataset_ids":[…]}` |
| `get_dataset_info` | `project_id`, `dataset_id` | `{"status":"SUCCESS","info":{…}}`: reference, description, location, labels, times, default table expiration |
| `list_table_ids` | `project_id`, `dataset_id` | `{"status":"SUCCESS","table_ids":[…]}` |
| `get_table_info` | `project_id`, `dataset_id`, `table_id` | `{"status":"SUCCESS","info":{…}}`: reference, type, schema, row and byte counts, partitioning, clustering, labels and, for a view, `view.query` |
| `execute_sql` | `project_id`, `query` | `{"status":"SUCCESS","rows":[{…}],"result_is_likely_truncated":true?}` |

A BigQuery error (bad SQL, a missing table, a permission problem) comes back as
`{"status":"ERROR","error_details":"…"}` so the model can correct itself. A credentials failure
fails the tool call. Lists stop at 1,000 items or 100 pages.

## Usage

```rust,ignore
use std::sync::Arc;
use gadk::bigquery::{BigQueryToolConfig, BigQueryToolset, WriteMode};

let bigquery = Arc::new(BigQueryToolset::new(
    BigQueryToolConfig::new()
        .with_write_mode(WriteMode::Blocked)
        .with_max_query_result_rows(50),
)?);
// LlmAgentConfig { toolsets: vec![bigquery], .. }
```

Enable it in the `gadk` crate with the `bigquery` feature (`gadk::bigquery`), or depend on
`gadk-bigquery` directly. See `crates/gadk/examples/bigquery_agent.rs` for a full agent on
`gemini-3.8-flash` (Vertex AI, location `us`).

To expose only some tools, wrap the toolset with `gadk_agent::toolset::filter_toolset`; to ask
the user before a call runs, wrap it with `gadk_agent::toolset::with_confirmation`.

`BigQueryToolset::new` and `with_client` return `ToolError::InvalidArgument` for an invalid
`compute_project` or a `max_query_result_rows` of 0.

## Write modes

| `WriteMode` | `execute_sql` runs |
|---|---|
| `Blocked` (default) | Only `SELECT` statements. A dry run checks the statement type first; anything else is refused without running. |
| `Protected` | `SELECT` statements, and writes to the temporary tables of a BigQuery session that belongs to the agent session (`CREATE TEMP TABLE …`). The session id is kept in session state under `gadk_bigquery_session`; an expired session is replaced and the call retried once. A BigQuery session is bound to one location, so set `location` when the data is outside the `US` multi-region. |
| `Allowed` | Any statement the credentials allow, without a dry run. |

## Configuration

| `BigQueryToolConfig` field | Default | Meaning |
|---|---|---|
| `write_mode` | `Blocked` | See above |
| `max_query_result_rows` | 50 | Rows returned to the model per query; at least 1 |
| `compute_project` | none | The project that runs and pays for query jobs (a project id, `[A-Za-z0-9.:_-]+`); none uses the `project_id` argument |
| `location` | none | Job location, such as `US`; needed in `Protected` mode when the data is outside the `US` multi-region |
| `maximum_bytes_billed` | none | Cost cap per query in bytes |
| `job_labels` | empty | Labels on every job; the toolset adds `gadk-bigquery-tool: execute_sql` |

## Query results

Each row is a JSON object. INT64 values are numbers up to 2^53 and strings beyond; FLOAT64 values
are numbers (NaN and infinities are strings); NUMERIC and BIGNUMERIC values are strings; BOOL
values are booleans; TIMESTAMP values are RFC 3339 strings; RECORD values are objects; REPEATED
values are arrays; NULL is `null`; every other type is the string BigQuery returns.

## Credentials and roles

`BigQueryToolset::new` uses Application Default Credentials (`gcloud auth application-default
login`, a service account, or the metadata server). Pass your own credentials with
`BigQueryClient::with_auth` and `BigQueryToolset::with_client`.

The credentials need **BigQuery Data Viewer** on the data and **BigQuery Job User** on the
compute project; **BigQuery Data Editor** too when writes are allowed.

## Live test

```sh
GADK_BIGQUERY_LIVE_PROJECT=<project> cargo test -p gadk-bigquery --test live -- --ignored --nocapture
```

It reads the `gadk_examples` dataset and creates no datasets or tables apart from one temporary
table in a BigQuery session's anonymous dataset (the `Protected` check), which BigQuery drops with
the session.

## License

Apache-2.0.
