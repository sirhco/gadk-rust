// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! Port of ADK-Go `plugin/agentanalytics/redaction.go`: sensitive-key detection, URI / path /
//! query sanitizing, credential-marker canonicalization, JSON-text redaction (fail-closed),
//! error-message and raw-text scrubbing, the content-formatter guard, the payload-column
//! denylist and the custom-metadata filter. Go's `session.KeyPrefixUser/App/Temp` are
//! `gadk_session::KEY_PREFIX_USER/APP/TEMP`.
//!
//! Only the cross-module entry points are declared here; Task 4 adds the private helpers of
//! `redaction.go` (`sensitiveKeys`, the regexes, `nestsSensitiveKey`, …) as it ports them.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::collections::{BTreeMap, BTreeSet};

use gadk_platform::go_quote;
use serde_json::{Map, Value};

use crate::config::{ContentFormatter, Logger};
use crate::error::Error;
use crate::formatter::NodeBudget;

/// Go `redactedValue`.
pub(crate) const REDACTED_VALUE: &str = "[REDACTED]";
/// Go `redactedURIPlaceholder`.
pub(crate) const REDACTED_URI_PLACEHOLDER: &str = "[REDACTED_SENSITIVE_URI]";
/// Go `redactedSensitiveTextPlaceholder`.
pub(crate) const REDACTED_SENSITIVE_TEXT_PLACEHOLDER: &str = "[REDACTED_SENSITIVE_TEXT]";
/// Go `formattingFailedPlaceholder`.
pub(crate) const FORMATTING_FAILED_PLACEHOLDER: &str = "[FORMATTER_FAILED]";
/// Go `unparseableJSONPlaceholder`.
pub(crate) const UNPARSEABLE_JSON_PLACEHOLDER: &str = "[UNPARSEABLE_JSON_BLOB]";

/// Go `maxInspectableJSONTextBytes` (adk-python `_MAX_JSON_INSPECT_CHARS`).
pub(crate) const MAX_INSPECTABLE_JSON_TEXT_BYTES: usize = 4_000_000;
/// Go `maxCanonicalUnescapePasses`.
pub(crate) const MAX_CANONICAL_UNESCAPE_PASSES: usize = 4;
/// Go `maxQuotedFragmentsPerInspect`.
pub(crate) const MAX_QUOTED_FRAGMENTS_PER_INSPECT: usize = 64;

/// Go `columnContent`: a payload column [`Config::payload_column_denylist`] may suppress.
///
/// [`Config::payload_column_denylist`]: crate::config::Config::payload_column_denylist
pub(crate) const COLUMN_CONTENT: &str = "content";
/// Go `columnContentParts`.
pub(crate) const COLUMN_CONTENT_PARTS: &str = "content_parts";
/// Go `columnAttributes`.
pub(crate) const COLUMN_ATTRIBUTES: &str = "attributes";
/// Go `columnLatencyMS`.
pub(crate) const COLUMN_LATENCY_MS: &str = "latency_ms";

/// Go `parsePayloadColumnDenylist(cols)`: validates `cols` and returns them as a set.
pub(crate) fn parse_payload_column_denylist(cols: &[String]) -> Result<BTreeSet<String>, Error> {
    let mut denied = BTreeSet::new();
    for c in cols {
        match c.as_str() {
            COLUMN_CONTENT | COLUMN_CONTENT_PARTS | COLUMN_ATTRIBUTES | COLUMN_LATENCY_MS => {
                denied.insert(c.clone());
            }
            _ => {
                return Err(Error::msg(format!(
                    "unsupported PayloadColumnDenylist entry {}: allowed values are {}, {}, {} and {}",
                    go_quote(c),
                    go_quote(COLUMN_CONTENT),
                    go_quote(COLUMN_CONTENT_PARTS),
                    go_quote(COLUMN_ATTRIBUTES),
                    go_quote(COLUMN_LATENCY_MS)
                )));
            }
        }
    }
    Ok(denied)
}

/// Go `isSensitiveKey(k)`: whether the values under map key `k` are redacted (case- and
/// separator-insensitive, `user:` / `app:` prefixes stripped, every `temp:` key).
pub(crate) fn is_sensitive_key(k: &str) -> bool {
    todo!("Task 4")
}

/// Go `canonicalizeCredentialMarker(s)`.
pub(crate) fn canonicalize_credential_marker(s: &str) -> String {
    todo!("Task 4")
}

/// Go `sanitizeURI(raw)`: `raw` with credentials removed from its userinfo, path and query;
/// `true` when it changed.
pub(crate) fn sanitize_uri(raw: &str) -> (String, bool) {
    todo!("Task 4")
}

/// Go `sanitizeRawText(raw)`: free text with embedded JSON keys, URIs and credential patterns
/// redacted; `true` when content was lost.
pub(crate) fn sanitize_raw_text(raw: &str) -> (String, bool) {
    todo!("Task 4")
}

/// Go `redactJSONTextWithBudget(s, depth, budget)`: redacts sensitive keys inside JSON-encoded
/// text (fail-closed: unparseable or over-budget JSON-like blobs become placeholders). Returns
/// `(sanitized, changed, content_lost)`.
pub(crate) fn redact_json_text_with_budget(
    s: &str,
    depth: usize,
    budget: &mut NodeBudget,
) -> (String, bool, bool) {
    todo!("Task 4")
}

/// Go `redactJSONValue(v, depth, budget)` over a decoded JSON value. Returns
/// `(out, changed, content_lost)`.
pub(crate) fn redact_json_value(
    v: &Value,
    depth: usize,
    budget: &mut NodeBudget,
) -> (Value, bool, bool) {
    todo!("Task 4")
}

/// Go `redactErrorMessage(msg)`: an error message with credentials scrubbed.
pub(crate) fn redact_error_message(msg: &str) -> String {
    todo!("Task 4")
}

/// Go `scrubErrorText(msg)`.
pub(crate) fn scrub_error_text(msg: &str) -> String {
    todo!("Task 4")
}

/// Go `applyContentFormatter(formatter, logger, payload, eventType, debugErrors)`: runs the
/// formatter fail-closed. Returns `(result, err_note, failed)`; on an error or a panic the
/// result is [`FORMATTING_FAILED_PLACEHOLDER`] (as a JSON string) and `failed` is true.
pub(crate) fn apply_content_formatter(
    formatter: Option<&ContentFormatter>,
    logger: Option<&dyn Logger>,
    payload: &Value,
    event_type: &str,
    debug_errors: bool,
) -> (Value, String, bool) {
    todo!("Task 4")
}

/// Go `customMetadataFilter`: selects custom-metadata keys by exact name or by prefix (entries
/// ending in `*`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CustomMetadataFilter {
    pub(crate) exact: BTreeSet<String>,
    pub(crate) prefixes: Vec<String>,
}

impl CustomMetadataFilter {
    /// Go `newCustomMetadataFilter(allowlist)`.
    pub(crate) fn new(allowlist: &[String]) -> CustomMetadataFilter {
        todo!("Task 4")
    }

    /// Go `allows(key)`.
    pub(crate) fn allows(&self, key: &str) -> bool {
        todo!("Task 4")
    }

    /// Go `apply(md)`: the allowed entries, or `None` when there are none.
    pub(crate) fn apply(&self, md: &Map<String, Value>) -> Option<Map<String, Value>> {
        todo!("Task 4")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn parse_payload_column_denylist_accepts_the_four_columns() {
        let cols: Vec<String> = [
            "content",
            "content_parts",
            "attributes",
            "latency_ms",
            "content",
        ]
        .map(String::from)
        .to_vec();
        let set = parse_payload_column_denylist(&cols).unwrap();
        assert_eq!(set.len(), 4);
        assert!(parse_payload_column_denylist(&[]).unwrap().is_empty());
    }

    #[test]
    fn parse_payload_column_denylist_rejects_other_columns_with_gos_text() {
        let err = parse_payload_column_denylist(&["content".into(), "status".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"unsupported PayloadColumnDenylist entry "status": allowed values are "content", "content_parts", "attributes" and "latency_ms""#
        );
    }
}
