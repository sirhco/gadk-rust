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

//! The BigQuery trace source: traces read from the agent-analytics table (`agent_events`).
//!
//! Rows that share a `trace_id` are one trace; rows that share a `span_id` are one span; every
//! row is also one log entry of its span. Rows are never joined to Cloud Logging.

use gadk_bigquery::BigQueryError;

use super::SourceError;

/// The URL form `--traces` accepts.
const URL_FORM: &str = "bigquery://<project>.<dataset>.<table>";

/// A BigQuery table: `project.dataset.table`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BigQueryTable {
    /// The project id, optionally domain-scoped (`example.com:proj`).
    pub project: String,
    /// The dataset id.
    pub dataset: String,
    /// The table id.
    pub table: String,
}

impl BigQueryTable {
    /// A table reference (not validated here; queries validate it).
    pub fn new(
        project: impl Into<String>,
        dataset: impl Into<String>,
        table: impl Into<String>,
    ) -> Self {
        Self {
            project: project.into(),
            dataset: dataset.into(),
            table: table.into(),
        }
    }

    /// Parses `bigquery://<project>.<dataset>.<table>`, splitting from the right so a
    /// domain-scoped project (`example.com:proj`) keeps its dots.
    ///
    /// # Errors
    ///
    /// A message naming the expected form when the scheme, the parts or their characters are
    /// wrong.
    pub fn parse_url(s: &str) -> Result<Self, String> {
        let bad = || format!("expected {URL_FORM}, got {s:?}");
        let rest = s.strip_prefix("bigquery://").ok_or_else(bad)?;
        let mut parts = rest.rsplitn(3, '.');
        let (Some(table), Some(dataset), Some(project)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        let t = Self::new(project, dataset, table);
        if t.is_valid() { Ok(t) } else { Err(bad()) }
    }

    /// Whether every part matches the allowed characters (see `is_project`, `is_dataset_or_table`).
    fn is_valid(&self) -> bool {
        is_project(&self.project)
            && is_dataset_or_table(&self.dataset)
            && is_dataset_or_table(&self.table)
    }

    /// The backquoted SQL path `` `project.dataset.table` ``.
    #[allow(dead_code)] // used by BigQueryTraceSource (Tasks 3-4)
    pub(crate) fn sql_path(&self) -> Result<String, SourceError> {
        if self.is_valid() {
            Ok(format!(
                "`{}.{}.{}`",
                self.project, self.dataset, self.table
            ))
        } else {
            Err(SourceError::BigQuery {
                status: None,
                reason: None,
                detail: format!(
                    "invalid table {}.{}.{}",
                    self.project, self.dataset, self.table
                ),
            })
        }
    }
}

/// `[a-z][a-z0-9-]{4,28}[a-z0-9]`, optionally prefixed by a domain `[a-z0-9.-]+:`.
fn is_project(s: &str) -> bool {
    let id = match s.rsplit_once(':') {
        Some((domain, id)) => {
            if domain.is_empty()
                || !domain
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            {
                return false;
            }
            id
        }
        None => s,
    };
    let b = id.as_bytes();
    (6..=30).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && (b[b.len() - 1].is_ascii_lowercase() || b[b.len() - 1].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `[A-Za-z0-9_]{1,1024}`.
fn is_dataset_or_table(s: &str) -> bool {
    (1..=1024).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether an event type names a span: `USER_MESSAGE_RECEIVED` or the `INVOCATION_`, `AGENT_`,
/// `LLM_` and `TOOL_` families.
#[allow(dead_code)] // used by BigQueryTraceSource (Tasks 3-4)
pub(crate) fn is_named_family(event_type: &str) -> bool {
    event_type == "USER_MESSAGE_RECEIVED"
        || ["INVOCATION_", "AGENT_", "LLM_", "TOOL_"]
            .iter()
            .any(|p| event_type.starts_with(p))
}

/// The span name for a row: `invocation`, `invoke_agent <agent>`, `call_llm <model>`,
/// `execute_tool <tool>`, else the event type lowercased. A missing or empty suffix is dropped.
#[allow(dead_code)] // used by BigQueryTraceSource (Tasks 3-4)
pub(crate) fn span_name(
    event_type: &str,
    agent: Option<&str>,
    model: Option<&str>,
    tool: Option<&str>,
) -> String {
    let (base, suffix) =
        if event_type == "USER_MESSAGE_RECEIVED" || event_type.starts_with("INVOCATION_") {
            ("invocation", None)
        } else if event_type.starts_with("AGENT_") {
            ("invoke_agent", agent)
        } else if event_type.starts_with("LLM_") {
            ("call_llm", model)
        } else if event_type.starts_with("TOOL_") {
            ("execute_tool", tool)
        } else {
            return event_type.to_ascii_lowercase();
        };
    match suffix.filter(|s| !s.is_empty()) {
        Some(s) => format!("{base} {s}"),
        None => base.to_owned(),
    }
}

/// A `gadk-bigquery` error as a trace-source error.
#[allow(dead_code)] // used by BigQueryTraceSource (Tasks 3-4)
pub(crate) fn from_bigquery(e: BigQueryError) -> SourceError {
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

#[cfg(test)]
mod tests {
    use gadk_bigquery::BigQueryError;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::traces::SourceError;

    #[test]
    fn parse_url_accepts_plain_and_domain_scoped_projects() {
        assert_eq!(
            BigQueryTable::parse_url("bigquery://sirhco-apps.agent_analytics.agent_events"),
            Ok(BigQueryTable::new(
                "sirhco-apps",
                "agent_analytics",
                "agent_events"
            ))
        );
        assert_eq!(
            BigQueryTable::parse_url("bigquery://example.com:proj-1.ds.tbl"),
            Ok(BigQueryTable::new("example.com:proj-1", "ds", "tbl"))
        );
    }

    #[test]
    fn parse_url_rejects_malformed_values() {
        for bad in [
            "",
            "sirhco-apps.ds.tbl",
            "bq://sirhco-apps.ds.tbl",
            "bigquery://sirhco-apps.ds",
            "bigquery://.ds.tbl",
            "bigquery://Sirhco-apps.ds.tbl",
            "bigquery://abc.ds.tbl",
            "bigquery://sirhco-apps.d-s.tbl",
            "bigquery://sirhco-apps.ds.t`bl",
            "bigquery://sirhco-apps.ds.",
            "bigquery://EXAMPLE.com:proj-1.ds.tbl",
        ] {
            let err = BigQueryTable::parse_url(bad).unwrap_err();
            assert!(
                err.contains("bigquery://<project>.<dataset>.<table>"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn sql_path_backquotes_valid_tables_and_rejects_others() {
        assert_eq!(
            BigQueryTable::new("example.com:proj-1", "ds", "tbl").sql_path(),
            Ok("`example.com:proj-1.ds.tbl`".to_owned())
        );
        let err = BigQueryTable::new("p", "ds", "t`; DROP")
            .sql_path()
            .unwrap_err();
        assert_eq!(
            err,
            SourceError::BigQuery {
                status: None,
                reason: None,
                detail: "invalid table p.ds.t`; DROP".into()
            }
        );
    }

    #[test]
    fn span_names_follow_the_event_family() {
        let cases = [
            ("USER_MESSAGE_RECEIVED", "invocation"),
            ("INVOCATION_STARTING", "invocation"),
            ("INVOCATION_COMPLETED", "invocation"),
            ("AGENT_STARTING", "invoke_agent weather_bot"),
            ("LLM_REQUEST", "call_llm gemini-3.8-flash"),
            ("TOOL_STARTING", "execute_tool get_weather"),
            ("STATE_DELTA", "state_delta"),
        ];
        for (event, want) in cases {
            assert_eq!(
                span_name(
                    event,
                    Some("weather_bot"),
                    Some("gemini-3.8-flash"),
                    Some("get_weather")
                ),
                want,
                "{event}"
            );
        }
        assert_eq!(
            span_name("AGENT_COMPLETED", None, None, None),
            "invoke_agent"
        );
        assert_eq!(span_name("LLM_RESPONSE", None, Some(""), None), "call_llm");
        assert_eq!(span_name("TOOL_ERROR", None, None, None), "execute_tool");
        assert!(is_named_family("USER_MESSAGE_RECEIVED"));
        assert!(is_named_family("TOOL_COMPLETED"));
        assert!(!is_named_family("STATE_DELTA"));
    }

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
