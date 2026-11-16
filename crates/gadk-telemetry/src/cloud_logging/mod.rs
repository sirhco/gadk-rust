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

//! Cloud Logging export of OpenTelemetry log records: the record-to-entry mapping
//! ([`entry`]), oversize handling ([`truncate`]), the `entries:write` exporter ([`api`]) and
//! the structured-JSON stdout exporter ([`stdout`]).

use std::collections::BTreeMap;

use opentelemetry_sdk::Resource;
use serde_json::{Value, json};

pub(crate) mod api;
pub(crate) mod entry;
pub(crate) mod stdout;
#[cfg(test)]
pub(crate) mod test_records;
pub(crate) mod truncate;

/// The log ID used when none is configured.
pub(crate) const DEFAULT_LOG_ID: &str = "gadk";
/// Cloud Logging's write endpoint.
pub(crate) const ENTRIES_WRITE_ENDPOINT: &str = "https://logging.googleapis.com/v2/entries:write";
/// The largest serialized entry Cloud Logging accepts (bytes).
pub(crate) const ENTRY_LIMIT: usize = 256_000;
/// The largest `entries:write` request body this exporter sends (bytes; Cloud Logging's limit
/// is 10 MB).
pub(crate) const REQUEST_LIMIT: usize = 9_000_000;
/// Env var that selects the stdout exporter (`1` or `true`, case-insensitive).
pub(crate) const LOGS_STDOUT_ENV: &str = "GADK_TELEMETRY_LOGS_STDOUT";

/// Percent-encodes every byte outside RFC 3986 unreserved characters.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A Cloud Logging monitored resource (`{"type": kind, "labels": {...}}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MonitoredResource {
    pub(crate) kind: &'static str,
    pub(crate) labels: BTreeMap<&'static str, String>,
}

impl MonitoredResource {
    pub(crate) fn to_json(&self) -> Value {
        json!({ "type": self.kind, "labels": self.labels })
    }
}

/// Where entries go: the project, the log ID and the monitored resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CloudLogTarget {
    pub(crate) project: String,
    pub(crate) log_id: String,
    pub(crate) resource: MonitoredResource,
}

impl CloudLogTarget {
    /// An empty `log_id` means [`DEFAULT_LOG_ID`].
    pub(crate) fn new(project: &str, log_id: &str, resource: Option<&Resource>) -> Self {
        let log_id = if log_id.is_empty() {
            DEFAULT_LOG_ID
        } else {
            log_id
        };
        Self {
            project: project.to_owned(),
            log_id: log_id.to_owned(),
            resource: entry::monitored_resource(resource, project),
        }
    }

    /// `projects/{project}/logs/{percent-encoded log id}`.
    pub(crate) fn log_name(&self) -> String {
        format!(
            "projects/{}/logs/{}",
            self.project,
            percent_encode(&self.log_id)
        )
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn log_name_defaults_and_encodes() {
        assert_eq!(
            CloudLogTarget::new("p", "", None).log_name(),
            "projects/p/logs/gadk"
        );
        assert_eq!(
            CloudLogTarget::new("p", "a/b c", None).log_name(),
            "projects/p/logs/a%2Fb%20c"
        );
        assert_eq!(percent_encode("A-z_0.9~"), "A-z_0.9~");
        assert_eq!(percent_encode("é"), "%C3%A9");
    }
}
