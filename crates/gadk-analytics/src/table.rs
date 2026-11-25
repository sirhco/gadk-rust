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

//! [`BigQueryTable`]: the agent-analytics table a query reads.

use serde::Serialize;

use crate::SourceError;

/// The URL form `--traces` accepts.
const URL_FORM: &str = "bigquery://<project>.<dataset>.<table>";

/// A BigQuery table: `project.dataset.table`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    ///
    /// # Errors
    ///
    /// [`SourceError::BigQuery`] naming the table when a part has characters a BigQuery id
    /// cannot have.
    pub fn sql_path(&self) -> Result<String, SourceError> {
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

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

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
}
