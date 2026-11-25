// Copyright 2026 Chris Olson
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The row readers and query-parameter builders the queries share, and the error hints of
//! BigQuery failures.
//!
//! Rows are the JSON objects of [`gadk_bigquery::QueryResult::rows`]; parameters are
//! `jobs.query` `queryParameters` entries.

use chrono::{DateTime, Utc};
use gadk_bigquery::BigQueryError;
use serde_json::{Map, Value, json};

use crate::SourceError;

/// The error-banner hint for a BigQuery permission failure.
pub const BQ_ROLES_HINT: &str = "needs roles/bigquery.dataViewer and roles/bigquery.jobUser";
/// The console's error-banner hint for a query stopped by the byte cap: it names the console's
/// `--bigquery-max-bytes` flag, as the console shows it.
pub const BQ_BYTES_HINT: &str = "raise --bigquery-max-bytes";

/// A `gadk-bigquery` error as a trace-source error.
pub fn from_bigquery(e: BigQueryError) -> SourceError {
    match e {
        BigQueryError::Api {
            status,
            message,
            reason,
        } => SourceError::BigQuery {
            status: Some(status),
            reason,
            detail: message,
        },
        BigQueryError::Credentials(text) => SourceError::Credentials(text),
        BigQueryError::Http(text) | BigQueryError::Job(text) | BigQueryError::Decode(text) => {
            SourceError::BigQuery {
                status: None,
                reason: None,
                detail: text,
            }
        }
        other => SourceError::BigQuery {
            status: None,
            reason: None,
            detail: other.to_string(),
        },
    }
}

/// A named query parameter; `None` is SQL NULL (`parameterValue` without `value`).
pub fn param(name: &str, ty: &str, value: Option<String>) -> Value {
    let value = match value {
        Some(v) => json!({ "value": v }),
        None => json!({}),
    };
    json!({ "name": name, "parameterType": { "type": ty }, "parameterValue": value })
}

/// A TIMESTAMP parameter value: `%Y-%m-%d %H:%M:%S%.6f+00:00`.
pub fn ts_param(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.6f+00:00").to_string()
}

/// A non-empty STRING cell.
pub fn text(row: &Map<String, Value>, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A TIMESTAMP cell (RFC 3339 text from `gadk-bigquery`).
pub fn timestamp(row: &Map<String, Value>, key: &str) -> Result<DateTime<Utc>, SourceError> {
    row.get(key)
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(|| SourceError::BigQuery {
            status: None,
            reason: None,
            detail: format!("row without a valid {key}"),
        })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn bigquery_errors_map_to_source_errors() {
        assert_eq!(
            from_bigquery(BigQueryError::Api {
                status: 403,
                message: "Access Denied".into(),
                reason: Some("accessDenied".into()),
            }),
            SourceError::BigQuery {
                status: Some(403),
                reason: Some("accessDenied".into()),
                detail: "Access Denied".into(),
            }
        );
        assert_eq!(
            from_bigquery(BigQueryError::Credentials("no ADC".into())),
            SourceError::Credentials("no ADC".into())
        );
        for e in [
            BigQueryError::Http("reset".into()),
            BigQueryError::Job(
                "Query exceeded limit for bytes billed: 10. 20 or higher required.".into(),
            ),
            BigQueryError::Decode("no rows".into()),
        ] {
            let text = match &e {
                BigQueryError::Http(t) | BigQueryError::Job(t) | BigQueryError::Decode(t) => {
                    t.clone()
                }
                _ => unreachable!(),
            };
            assert_eq!(
                from_bigquery(e),
                SourceError::BigQuery {
                    status: None,
                    reason: None,
                    detail: text
                }
            );
        }
    }
}
