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

//! The Agent Identity `credentials:retrieve` protocol (port of ADK-Go
//! `auth/gcp/agentidentity.go`, plus its request type from `client.go`). The response is
//! synchronous: a closed "result" oneof with no long-running-operation wrapper.

use serde::{Deserialize, Serialize};

use super::Error;
use super::client::{
    Client, CredentialPayload, Empty, GoObject, Outcome, Request, go_object, go_zero,
    parse_expire_time,
};

/// Go `agentIdentityRequest` (client.go:594). The auth provider is bound to the URL path, not
/// the body. Agent Identity takes the rejected token itself as `forceRefreshToken`.
#[derive(Serialize)]
pub(crate) struct AgentIdentityRequest {
    #[serde(rename = "userId", skip_serializing_if = "String::is_empty")]
    user_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    scopes: Vec<String>,
    #[serde(rename = "continueUri", skip_serializing_if = "String::is_empty")]
    continue_uri: String,
    #[serde(rename = "forceRefreshToken", skip_serializing_if = "String::is_empty")]
    force_refresh_token: String,
}

/// Go `agentIdentityResponse` (agentidentity.go:23): the RetrieveCredentialsResponse "result"
/// oneof.
#[derive(Default, Deserialize)]
pub(crate) struct AgentIdentityResponse {
    #[serde(default, deserialize_with = "go_object")]
    success: Option<CredentialPayload>,
    #[serde(default, deserialize_with = "go_object")]
    pending: Option<Empty>,
    #[serde(default, rename = "uriConsentRequired", deserialize_with = "go_object")]
    uri_consent_required: Option<ConsentDetail>,
    #[serde(default, rename = "consentRejected", deserialize_with = "go_object")]
    consent_rejected: Option<Empty>,
}

/// Go `consentDetail` (agentidentity.go:31): the uri-consent payload both services share.
#[derive(Default, Deserialize)]
pub(crate) struct ConsentDetail {
    #[serde(default, rename = "authorizationUri", deserialize_with = "go_zero")]
    pub(crate) authorization_uri: String,
    #[serde(default, rename = "consentNonce", deserialize_with = "go_zero")]
    pub(crate) consent_nonce: String,
}

impl AgentIdentityResponse {
    /// Go `agentIdentityResponse.result` (agentidentity.go:45): the oneof as an outcome, in Go's
    /// arm order, or an error when no arm is recognized.
    ///
    /// The request is unused, and taken anyway to match `ConnectorOperation::result`, which
    /// scrubs caller values out of the service's message. No arm here puts service text in an
    /// error; `success.header` does reach one, through `map_credential`, which scrubs it with
    /// the request in scope.
    pub(crate) fn result(self, _req: &Request) -> Result<Outcome, Error> {
        if let Some(s) = self.success {
            return Ok(Outcome::Cred {
                expires_at: parse_expire_time(&s.expire_time),
                header: s.header,
                token: s.token,
            });
        }
        if let Some(c) = self.uri_consent_required {
            return Ok(Outcome::Consent {
                auth_uri: c.authorization_uri,
                nonce: c.consent_nonce,
            });
        }
        if self.consent_rejected.is_some() {
            return Ok(Outcome::Rejected);
        }
        if self.pending.is_some() {
            return Ok(Outcome::Pending);
        }
        Err(Error::new("gcp: agent identity returned an empty result"))
    }
}

impl Client {
    /// Go `retrieveAgentIdentity` (agentidentity.go:63): posts to
    /// `{agent_identity_url}/v1/{resource}/credentials:retrieve`.
    pub(crate) async fn retrieve_agent_identity(&self, req: &Request) -> Result<Outcome, Error> {
        let url = format!(
            "{}/v1/{}/credentials:retrieve",
            self.agent_identity_url, req.resource
        );
        let body = AgentIdentityRequest {
            user_id: req.user_id.clone(),
            scopes: req.scopes.clone(),
            continue_uri: req.continue_uri.clone(),
            force_refresh_token: req.prior_token.clone(),
        };
        let secrets = [
            req.user_id.as_str(),
            req.continue_uri.as_str(),
            req.prior_token.as_str(),
        ];
        let out: GoObject<AgentIdentityResponse> = self.do_post(&url, &body, &secrets).await?;
        out.0.result(req)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn decode(s: &str) -> Result<AgentIdentityResponse, serde_json::Error> {
        serde_json::from_str::<GoObject<AgentIdentityResponse>>(s).map(|g| g.0)
    }

    fn outcome(s: &str) -> Result<Outcome, Error> {
        decode(s)
            .unwrap_or_else(|e| panic!("decode {s}: {e}"))
            .result(&Request::default())
    }

    /// Each row's expectation is what `encoding/json.Unmarshal` into Go's
    /// `agentIdentityResponse` produced.
    #[test]
    fn decodes_null_and_non_objects_as_encoding_json_does() {
        for s in [
            "null",
            r#"{"success":null}"#,
            "{}",
            r#"{"pending":{},"pending":null}"#,
        ] {
            assert_eq!(
                outcome(s).unwrap_err().to_string(),
                "gcp: agent identity returned an empty result",
                "{s}"
            );
        }
        assert_eq!(
            outcome(r#"{"success":{"token":null,"header":"h","expireTime":null}}"#).unwrap(),
            Outcome::Cred {
                header: "h".into(),
                token: String::new(),
                expires_at: None,
            }
        );
        assert_eq!(outcome(r#"{"pending":{"x":1}}"#).unwrap(), Outcome::Pending);
        for s in [
            r#"{"pending":[]}"#,
            r#"{"success":[]}"#,
            r#"{"pending":true}"#,
            "[]",
        ] {
            assert!(decode(s).is_err(), "{s}: want a decode error");
        }
    }

    #[test]
    fn result_takes_the_arms_in_go_order() {
        let cred = r#""success":{"token":"t","header":"h","expireTime":"2999-01-01T00:00:00Z"}"#;
        let consent = r#""uriConsentRequired":{"authorizationUri":"https://c","consentNonce":"n"}"#;
        let rejected = r#""consentRejected":{}"#;
        let pending = r#""pending":{}"#;
        assert_eq!(
            outcome(&format!("{{{pending},{rejected},{consent},{cred}}}")).unwrap(),
            Outcome::Cred {
                header: "h".into(),
                token: "t".into(),
                expires_at: parse_expire_time("2999-01-01T00:00:00Z"),
            }
        );
        assert_eq!(
            outcome(&format!("{{{pending},{rejected},{consent}}}")).unwrap(),
            Outcome::Consent {
                auth_uri: "https://c".into(),
                nonce: "n".into(),
            }
        );
        assert_eq!(
            outcome(&format!("{{{pending},{rejected}}}")).unwrap(),
            Outcome::Rejected
        );
        assert_eq!(
            outcome(&format!("{{{pending}}}")).unwrap(),
            Outcome::Pending
        );
    }
}
