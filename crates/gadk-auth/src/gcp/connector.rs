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

//! The IAM Connector `credentials:retrieve` protocol (port of ADK-Go `auth/gcp/connector.go`,
//! plus its request type from `client.go`). The service wraps its answer in a
//! `google.longrunning.Operation` that it never leaves running: the terminal result is read
//! inline from `response`/`metadata`, and the `@type` field of those `Any`s is ignored.

use gadk_agent::go_quote;
use serde::{Deserialize, Serialize};

use super::Error;
use super::agent_identity::ConsentDetail;
use super::client::{
    Client, CredentialPayload, Empty, GoObject, Outcome, Request, go_object, go_zero,
    parse_expire_time,
};
use super::redact::redacted_for_error;

/// Go `connectorRequest` (client.go:601). The connector takes a boolean where Agent Identity
/// takes the rejected token (adk-python's generated client sends `force_refresh`). Sending one
/// service the other's field would leave the refresh unforced, or be rejected as unknown.
#[derive(Serialize)]
pub(crate) struct ConnectorRequest {
    #[serde(rename = "userId", skip_serializing_if = "String::is_empty")]
    user_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    scopes: Vec<String>,
    #[serde(rename = "continueUri", skip_serializing_if = "String::is_empty")]
    continue_uri: String,
    #[serde(rename = "forceRefresh", skip_serializing_if = "is_false")]
    force_refresh: bool,
}

/// Go `omitempty` on a bool.
fn is_false(b: &bool) -> bool {
    !*b
}

/// Go `connectorOperation` (connector.go:28).
#[derive(Default, Deserialize)]
pub(crate) struct ConnectorOperation {
    #[serde(default, deserialize_with = "go_zero")]
    done: bool,
    #[serde(default, deserialize_with = "go_object")]
    response: Option<CredentialPayload>,
    #[serde(default, deserialize_with = "go_object")]
    metadata: Option<OperationMetadata>,
    #[serde(default, deserialize_with = "go_object")]
    error: Option<OperationError>,
}

/// The operation's `metadata` (connector.go:31).
#[derive(Default, Deserialize)]
pub(crate) struct OperationMetadata {
    #[serde(default, rename = "consentPending", deserialize_with = "go_object")]
    consent_pending: Option<Empty>,
    #[serde(default, rename = "uriConsentRequired", deserialize_with = "go_object")]
    uri_consent_required: Option<ConsentDetail>,
    #[serde(default, rename = "consentRejected", deserialize_with = "go_object")]
    consent_rejected: Option<Empty>,
}

/// The operation's `error` (connector.go:36). Go's `int` is 64-bit.
#[derive(Default, Deserialize)]
pub(crate) struct OperationError {
    #[serde(default, deserialize_with = "go_zero")]
    code: i64,
    #[serde(default, deserialize_with = "go_zero")]
    message: String,
}

impl ConnectorOperation {
    /// Go `connectorOperation.result` (connector.go:43): `error` first, then a done operation's
    /// credential, then the metadata arms in Go's order. Anything else is pending: an absent
    /// or unknown status means "just retry".
    pub(crate) fn result(self, req: &Request) -> Result<Outcome, Error> {
        if let Some(err) = self.error {
            if !err.message.is_empty() {
                // Service text: the same scrub, cap and %q quoting do_post gives a body.
                let shown = redacted_for_error(
                    &err.message,
                    &[req.user_id.as_str(), req.continue_uri.as_str()],
                );
                return Err(Error::new(format!(
                    "gcp: connector operation failed (code {}): {}",
                    err.code,
                    go_quote(&shown)
                )));
            }
            return Err(Error::new(format!(
                "gcp: connector operation failed (code {})",
                err.code
            )));
        }
        if self.done {
            // A terminal operation must carry a credential: an empty one is an error, not a
            // reason to poll to the timeout.
            let Some(resp) = self.response else {
                return Err(Error::new(
                    "gcp: connector operation done but returned no credential",
                ));
            };
            return Ok(Outcome::Cred {
                expires_at: parse_expire_time(&resp.expire_time),
                header: resp.header,
                token: resp.token,
            });
        }
        if let Some(md) = self.metadata {
            if let Some(c) = md.uri_consent_required {
                return Ok(Outcome::Consent {
                    auth_uri: c.authorization_uri,
                    nonce: c.consent_nonce,
                });
            }
            if md.consent_rejected.is_some() {
                return Ok(Outcome::Rejected);
            }
            if md.consent_pending.is_some() {
                return Ok(Outcome::Pending);
            }
        }
        Ok(Outcome::Pending)
    }
}

impl Client {
    /// Go `retrieveConnector` (connector.go:76): posts to
    /// `{connector_url}/v1alpha/{resource}/credentials:retrieve`.
    pub(crate) async fn retrieve_connector(&self, req: &Request) -> Result<Outcome, Error> {
        let url = format!(
            "{}/v1alpha/{}/credentials:retrieve",
            self.connector_url, req.resource
        );
        let body = ConnectorRequest {
            user_id: req.user_id.clone(),
            scopes: req.scopes.clone(),
            continue_uri: req.continue_uri.clone(),
            force_refresh: !req.prior_token.is_empty(),
        };
        // The connector is never sent the token, so only the user and the continue URI can
        // come back in its text.
        let secrets = [req.user_id.as_str(), req.continue_uri.as_str()];
        let op: GoObject<ConnectorOperation> = self.do_post(&url, &body, &secrets).await?;
        op.0.result(req)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn decode(s: &str) -> Result<ConnectorOperation, serde_json::Error> {
        serde_json::from_str::<GoObject<ConnectorOperation>>(s).map(|g| g.0)
    }

    fn outcome_for(s: &str, req: &Request) -> Result<Outcome, Error> {
        decode(s)
            .unwrap_or_else(|e| panic!("decode {s}: {e}"))
            .result(req)
    }

    fn outcome(s: &str) -> Result<Outcome, Error> {
        outcome_for(s, &Request::default())
    }

    /// Each row's expectation is what `encoding/json.Unmarshal` into Go's `connectorOperation`
    /// produced.
    #[test]
    fn decodes_null_and_non_objects_as_encoding_json_does() {
        for s in [
            "null",
            r#"{"done":null}"#,
            "{}",
            r#"{"metadata":{}}"#,
            r#"{"metadata":null}"#,
        ] {
            assert_eq!(outcome(s).unwrap(), Outcome::Pending, "{s}");
        }
        assert_eq!(
            outcome(r#"{"error":{"code":null,"message":null}}"#)
                .unwrap_err()
                .to_string(),
            "gcp: connector operation failed (code 0)"
        );
        for s in [
            r#"{"metadata":[]}"#,
            r#"{"error":[]}"#,
            r#"{"done":"yes"}"#,
            "[]",
        ] {
            assert!(decode(s).is_err(), "{s}: want a decode error");
        }
        // A repeated key keeps its last value, so this operation is still pending.
        assert_eq!(
            outcome(r#"{"done":true,"response":{"token":"t","header":"h"},"done":false}"#).unwrap(),
            Outcome::Pending
        );
    }

    #[test]
    fn result_precedence_matches_go() {
        // error beats done
        assert_eq!(
            outcome(r#"{"done":true,"response":{"token":"t","header":"h"},"error":{"code":7}}"#)
                .unwrap_err()
                .to_string(),
            "gcp: connector operation failed (code 7)"
        );
        assert_eq!(
            outcome(r#"{"done":true}"#).unwrap_err().to_string(),
            "gcp: connector operation done but returned no credential"
        );
        // done beats metadata, and the expiry comes from the response
        assert_eq!(
            outcome(r#"{"done":true,"response":{"token":"t","header":"h","expireTime":"2999-01-01T00:00:00Z"},"metadata":{"consentRejected":{}}}"#)
                .unwrap(),
            Outcome::Cred {
                header: "h".into(),
                token: "t".into(),
                expires_at: parse_expire_time("2999-01-01T00:00:00Z"),
            }
        );
        // metadata arms: uri consent, then rejected, then pending
        assert_eq!(
            outcome(r#"{"metadata":{"consentPending":{},"consentRejected":{},"uriConsentRequired":{"authorizationUri":"u","consentNonce":"n"}}}"#)
                .unwrap(),
            Outcome::Consent {
                auth_uri: "u".into(),
                nonce: "n".into(),
            }
        );
        assert_eq!(
            outcome(r#"{"metadata":{"consentPending":{},"consentRejected":{}}}"#).unwrap(),
            Outcome::Rejected
        );
        assert_eq!(
            outcome(r#"{"metadata":{"consentPending":{}}}"#).unwrap(),
            Outcome::Pending
        );
    }

    /// The expected text is Go's `connectorOperation.result` output for the same operation
    /// and request.
    #[test]
    fn operation_error_message_is_scrubbed_and_quoted() {
        let req = Request {
            user_id: "alice@example.test".into(),
            continue_uri: "https://app.test/cb".into(),
            ..Default::default()
        };
        let err = outcome_for(
            r#"{"error":{"code":7,"message":"user alice@example.test is not permitted\nINFO forged"}}"#,
            &req,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"gcp: connector operation failed (code 7): "user [redacted] is not permitted\ninfo forged""#
        );
    }
}
