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

//! [`SourceError`], the error of every query, and the [`Service`] it names.

use std::fmt;

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};

use crate::row::{BQ_BYTES_HINT, BQ_ROLES_HINT};

/// The upstream API a [`SourceError`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// Cloud Trace.
    Trace,
    /// Cloud Logging.
    Logging,
}

impl fmt::Display for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Service::Trace => "Cloud Trace",
            Service::Logging => "Cloud Logging",
        })
    }
}

/// Serializes as the [`Display`](fmt::Display) text: `"Cloud Trace"` or `"Cloud Logging"`.
impl Serialize for Service {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// A failed trace or log read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SourceError {
    /// Non-2xx reply. `excerpt` is at most 1,024 bytes of the body, cut on a char boundary.
    #[error("{service}: HTTP {status}: {excerpt}")]
    Upstream {
        /// The API that replied.
        service: Service,
        /// The HTTP status code.
        status: u16,
        /// At most 1,024 bytes of the reply body.
        excerpt: String,
        /// The `reason` of the reply's `google.rpc.ErrorInfo` detail, if any (for example
        /// `SERVICE_DISABLED`).
        reason: Option<String>,
    },
    /// The request failed before a reply arrived.
    #[error("{service}: {detail}")]
    Transport {
        /// The API that was called.
        service: Service,
        /// The failure text.
        detail: String,
    },
    /// The reply could not be decoded.
    #[error("{service}: unexpected response: {detail}")]
    Decode {
        /// The API that replied.
        service: Service,
        /// What was wrong with the reply.
        detail: String,
    },
    #[error("credentials: {0}")]
    /// Credentials could not be obtained.
    Credentials(String),
    /// The trace id is unknown to Cloud Trace (HTTP 404 on get).
    #[error("trace not found")]
    NotFound,
    /// A BigQuery call failed (the BigQuery trace source).
    #[error("BigQuery: {}{detail}", .status.map(|s| format!("HTTP {s}: ")).unwrap_or_default())]
    #[non_exhaustive]
    BigQuery {
        /// The HTTP status, when BigQuery replied.
        status: Option<u16>,
        /// BigQuery's error reason (for example `bytesBilledLimitExceeded`), when given.
        reason: Option<String>,
        /// The error text.
        detail: String,
    },
}

impl SourceError {
    /// A [`SourceError::BigQuery`]: the variant is `#[non_exhaustive]`, so code outside this
    /// crate builds it here.
    pub fn bigquery(
        status: Option<u16>,
        reason: Option<String>,
        detail: impl Into<String>,
    ) -> Self {
        SourceError::BigQuery {
            status,
            reason,
            detail: detail.into(),
        }
    }

    /// The hint shown under the error banner, if any. The hints are the console's texts: the
    /// byte-cap hint, for example, names the console's `--bigquery-max-bytes` flag.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            SourceError::Upstream {
                service,
                reason: Some(reason),
                ..
            } if reason == "SERVICE_DISABLED" => Some(match service {
                Service::Trace => "enable cloudtrace.googleapis.com",
                Service::Logging => "enable logging.googleapis.com",
            }),
            SourceError::Upstream {
                service: Service::Trace,
                status: 403,
                ..
            } => Some("needs roles/cloudtrace.user"),
            SourceError::Upstream {
                service: Service::Logging,
                status: 403,
                ..
            } => Some("needs roles/logging.viewer"),
            SourceError::Credentials(_) => Some("run `gcloud auth application-default login`"),
            SourceError::BigQuery {
                status: Some(403),
                reason,
                ..
            } if matches!(reason.as_deref(), None | Some("accessDenied")) => Some(BQ_ROLES_HINT),
            SourceError::BigQuery { reason, detail, .. }
                if reason.as_deref() == Some("bytesBilledLimitExceeded")
                    || detail
                        .to_ascii_lowercase()
                        .contains("exceeded limit for bytes billed") =>
            {
                Some(BQ_BYTES_HINT)
            }
            _ => None,
        }
    }
}

impl SourceError {
    /// The snake_case name of the variant: the `kind` of the JSON form.
    fn kind(&self) -> &'static str {
        match self {
            SourceError::Upstream { .. } => "upstream",
            SourceError::Transport { .. } => "transport",
            SourceError::Decode { .. } => "decode",
            SourceError::Credentials(_) => "credentials",
            SourceError::NotFound => "not_found",
            SourceError::BigQuery { .. } => "bigquery",
        }
    }
}

/// Serializes as `{"kind", "message", "hint"}`: the error kind (`upstream`,
/// `transport`, `decode`, `credentials`, `not_found` or `bigquery`), the
/// [`Display`](fmt::Display) text and [`SourceError::hint`] (`null` when there is none).
impl Serialize for SourceError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("kind", self.kind())?;
        map.serialize_entry("message", &self.to_string())?;
        map.serialize_entry("hint", &self.hint())?;
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(service: Service, status: u16, reason: Option<&str>) -> SourceError {
        SourceError::Upstream {
            service,
            status,
            excerpt: "{}".into(),
            reason: reason.map(str::to_owned),
        }
    }

    #[test]
    fn hint_for_every_row() {
        let disabled = Some("SERVICE_DISABLED");
        assert_eq!(
            upstream(Service::Trace, 403, disabled).hint(),
            Some("enable cloudtrace.googleapis.com")
        );
        assert_eq!(
            upstream(Service::Logging, 403, disabled).hint(),
            Some("enable logging.googleapis.com")
        );
        // SERVICE_DISABLED wins whatever the status.
        assert_eq!(
            upstream(Service::Logging, 400, disabled).hint(),
            Some("enable logging.googleapis.com")
        );
        assert_eq!(
            upstream(Service::Trace, 403, None).hint(),
            Some("needs roles/cloudtrace.user")
        );
        assert_eq!(
            upstream(Service::Logging, 403, None).hint(),
            Some("needs roles/logging.viewer")
        );
        assert_eq!(
            upstream(Service::Trace, 403, Some("IAM_PERMISSION_DENIED")).hint(),
            Some("needs roles/cloudtrace.user")
        );
        assert_eq!(
            SourceError::Credentials("no ADC".into()).hint(),
            Some("run `gcloud auth application-default login`")
        );
    }

    #[test]
    fn hint_is_none_otherwise() {
        assert_eq!(upstream(Service::Trace, 500, None).hint(), None);
        assert_eq!(upstream(Service::Logging, 404, Some("OTHER")).hint(), None);
        let transport = SourceError::Transport {
            service: Service::Trace,
            detail: "timed out".into(),
        };
        assert_eq!(transport.hint(), None);
        let decode = SourceError::Decode {
            service: Service::Logging,
            detail: "bad json".into(),
        };
        assert_eq!(decode.hint(), None);
        assert_eq!(SourceError::NotFound.hint(), None);
    }

    #[test]
    fn service_display() {
        assert_eq!(Service::Trace.to_string(), "Cloud Trace");
        assert_eq!(Service::Logging.to_string(), "Cloud Logging");
    }

    #[test]
    fn source_error_display() {
        assert_eq!(
            upstream(Service::Trace, 403, None).to_string(),
            "Cloud Trace: HTTP 403: {}"
        );
        let transport = SourceError::Transport {
            service: Service::Logging,
            detail: "timed out".into(),
        };
        assert_eq!(transport.to_string(), "Cloud Logging: timed out");
        let decode = SourceError::Decode {
            service: Service::Trace,
            detail: "missing traces".into(),
        };
        assert_eq!(
            decode.to_string(),
            "Cloud Trace: unexpected response: missing traces"
        );
        assert_eq!(
            SourceError::Credentials("no ADC".into()).to_string(),
            "credentials: no ADC"
        );
        assert_eq!(SourceError::NotFound.to_string(), "trace not found");
    }

    #[test]
    fn bigquery_errors_display_and_hint() {
        let denied = SourceError::BigQuery {
            status: Some(403),
            reason: Some("accessDenied".into()),
            detail: "Access Denied".into(),
        };
        assert_eq!(denied.to_string(), "BigQuery: HTTP 403: Access Denied");
        assert_eq!(denied.hint(), Some(BQ_ROLES_HINT));

        let cap_reason = SourceError::BigQuery {
            status: Some(400),
            reason: Some("bytesBilledLimitExceeded".into()),
            detail: "Query exceeded limit for bytes billed: 10. 20 or higher required.".into(),
        };
        assert_eq!(cap_reason.hint(), Some(BQ_BYTES_HINT));

        let cap_job = SourceError::BigQuery {
            status: None,
            reason: None,
            detail: "Query exceeded limit for bytes billed: 10. 20 or higher required.".into(),
        };
        assert_eq!(
            cap_job.to_string(),
            "BigQuery: Query exceeded limit for bytes billed: 10. 20 or higher required."
        );
        assert_eq!(cap_job.hint(), Some(BQ_BYTES_HINT));

        let other = SourceError::BigQuery {
            status: Some(400),
            reason: None,
            detail: "bad".into(),
        };
        assert_eq!(other.hint(), None);

        let no_reason = SourceError::BigQuery {
            status: Some(403),
            reason: None,
            detail: "Access Denied".into(),
        };
        assert_eq!(no_reason.hint(), Some(BQ_ROLES_HINT));

        for reason in ["quotaExceeded", "rateLimitExceeded", "billingNotEnabled"] {
            let not_roles = SourceError::BigQuery {
                status: Some(403),
                reason: Some(reason.into()),
                detail: "Quota exceeded".into(),
            };
            assert_eq!(not_roles.hint(), None, "{reason}");
        }
    }
}
