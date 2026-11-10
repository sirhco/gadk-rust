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

//! [`AgentEngineData`] (Go `util/vertexai`), Go `aiplatform.HostURL`, and the endpoint,
//! credential and HTTP-client resolution both services share.

#![allow(dead_code)]

use std::sync::Arc;

use gadk_model::gcp_auth::adc_credentials;
use gadk_model::{HttpClient, ReqwestClient};
use gadk_platform::env::EnvProvider;
use google_cloud_auth::credentials::Credentials;

use crate::error::BoxError;

/// An Agent Engine (reasoning engine) instance (Go `vertexaiutil.AgentEngineData`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentEngineData {
    /// The Google Cloud project ID.
    pub project_id: String,
    /// The engine's location (region), e.g. `us-central1`.
    pub location: String,
    /// The reasoning engine's numeric ID.
    pub reasoning_engine: String,
}

impl AgentEngineData {
    /// The engine's resource name (Go `AgentEngineResource`):
    /// `projects/{project_id}/locations/{location}/reasoningEngines/{reasoning_engine}`.
    pub fn engine_resource(&self) -> String {
        format!(
            "projects/{}/locations/{}/reasoningEngines/{}",
            self.project_id, self.location, self.reasoning_engine
        )
    }

    /// A session's resource name (Go `SessionResource`): `{engine_resource}/sessions/{session_id}`.
    pub fn session_resource(&self, session_id: &str) -> String {
        format!("{}/sessions/{session_id}", self.engine_resource())
    }
}

/// The Vertex AI host of a location (Go `aiplatform.HostURL`): `aiplatform.googleapis.com` for
/// `""` or `"global"`, else `{location}-aiplatform.googleapis.com`.
pub(crate) fn host_url(location: &str) -> String {
    if location.is_empty() || location == "global" {
        "aiplatform.googleapis.com".to_owned()
    } else {
        format!("{location}-aiplatform.googleapis.com")
    }
}

/// The OAuth scopes requested for Application Default Credentials (Go's aiplatform clients'
/// `DefaultAuthScopes`).
pub(crate) const SCOPES: [&str; 1] = ["https://www.googleapis.com/auth/cloud-platform"];

/// The session client's default endpoint (Go `defaultSessionRESTClientOptions`), whatever the
/// location.
pub(crate) const SESSION_ENDPOINT: &str = "https://aiplatform.googleapis.com";

/// What a service's client needs once its config is resolved.
pub(crate) struct ResolvedClient {
    /// Scheme + authority (+ optional path prefix), no trailing `/`.
    pub(crate) base_url: String,
    /// The credentials sent with every request; `None` = send no credential headers.
    pub(crate) credentials: Option<Credentials>,
    /// The HTTP client requests go through.
    pub(crate) http: Arc<dyn HttpClient>,
}

/// Resolves a service config (Go's `option.ClientOption` handling in `NewSessionClient` /
/// `NewMemoryBankClient`): the base URL is `endpoint` with trailing `/`s trimmed (an empty
/// endpoint counts as none), else `default_endpoint`; the credentials are none with
/// `without_authentication` (Go `option.WithoutAuthentication`), else `credentials`, else
/// Application Default Credentials with [`SCOPES`], reading `GOOGLE_APPLICATION_CREDENTIALS`
/// through `env`; the HTTP client is `http_client`, else a reqwest client.
///
/// # Errors
///
/// The [`AdcError`](gadk_model::gcp_auth::AdcError) when Application Default Credentials cannot
/// be built, boxed but not wrapped: callers add Go's prefixes.
pub(crate) fn resolve(
    endpoint: Option<&str>,
    default_endpoint: &str,
    credentials: Option<Credentials>,
    without_authentication: bool,
    http_client: Option<Arc<dyn HttpClient>>,
    env: &dyn EnvProvider,
) -> Result<ResolvedClient, BoxError> {
    let base_url = endpoint
        .map(|e| e.trim_end_matches('/'))
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| default_endpoint.trim_end_matches('/'))
        .to_owned();
    let credentials = match (without_authentication, credentials) {
        (true, _) => None,
        (false, Some(credentials)) => Some(credentials),
        (false, None) => {
            let file = env.var("GOOGLE_APPLICATION_CREDENTIALS");
            Some(adc_credentials(file.as_deref(), &SCOPES)?)
        }
    };
    let http = http_client.unwrap_or_else(|| Arc::new(ReqwestClient::default()));
    Ok(ResolvedClient {
        base_url,
        credentials,
        http,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;

    use gadk_model::gcp_auth::{AdcError, NO_RUNTIME};
    use gadk_platform::env::MapEnv;
    use google_cloud_auth::credentials::anonymous;
    use pretty_assertions::assert_eq;

    use super::*;

    fn empty_env() -> MapEnv {
        MapEnv(HashMap::new())
    }

    fn engine() -> AgentEngineData {
        AgentEngineData {
            project_id: "adk-go-e2e".into(),
            location: "us-central1".into(),
            reasoning_engine: "1491331942182813696".into(),
        }
    }

    /// Go `AgentEngineResource` / `SessionResource`.
    #[test]
    fn resource_names_follow_go_templates() {
        assert_eq!(
            engine().engine_resource(),
            "projects/adk-go-e2e/locations/us-central1/reasoningEngines/1491331942182813696"
        );
        assert_eq!(
            engine().session_resource("s1"),
            "projects/adk-go-e2e/locations/us-central1/reasoningEngines/1491331942182813696/sessions/s1"
        );
        // Go's Sprintf does no escaping or validation.
        assert_eq!(
            AgentEngineData::default().session_resource(""),
            "projects//locations//reasoningEngines//sessions/"
        );
    }

    /// Go `aiplatform.HostURL`.
    #[test]
    fn host_url_matches_go() {
        assert_eq!(host_url(""), "aiplatform.googleapis.com");
        assert_eq!(host_url("global"), "aiplatform.googleapis.com");
        assert_eq!(
            host_url("us-central1"),
            "us-central1-aiplatform.googleapis.com"
        );
        assert_eq!(
            host_url("europe-west4"),
            "europe-west4-aiplatform.googleapis.com"
        );
    }

    #[test]
    fn endpoint_defaults_and_trims() {
        let base = |endpoint: Option<&str>| {
            resolve(endpoint, SESSION_ENDPOINT, None, true, None, &empty_env())
                .unwrap()
                .base_url
        };
        assert_eq!(base(None), "https://aiplatform.googleapis.com");
        assert_eq!(base(Some("")), "https://aiplatform.googleapis.com");
        assert_eq!(base(Some("http://127.0.0.1:8080")), "http://127.0.0.1:8080");
        assert_eq!(
            base(Some("http://127.0.0.1:8080/")),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            base(Some("http://127.0.0.1:8080/prefix//")),
            "http://127.0.0.1:8080/prefix"
        );
        let memory_default = format!("https://{}", host_url("us-central1"));
        assert_eq!(
            resolve(None, &memory_default, None, true, None, &empty_env())
                .unwrap()
                .base_url,
            "https://us-central1-aiplatform.googleapis.com"
        );
    }

    /// `without_authentication` and supplied credentials need no runtime (R2).
    #[test]
    fn credentials_without_authentication_or_supplied_need_no_runtime() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let none = resolve(None, SESSION_ENDPOINT, None, true, None, &empty_env()).unwrap();
        assert!(none.credentials.is_none());
        let creds = anonymous::Builder::new().build();
        let ignored = resolve(
            None,
            SESSION_ENDPOINT,
            Some(creds.clone()),
            true,
            None,
            &empty_env(),
        )
        .unwrap();
        assert!(ignored.credentials.is_none());
        let supplied = resolve(
            None,
            SESSION_ENDPOINT,
            Some(creds),
            false,
            None,
            &empty_env(),
        )
        .unwrap();
        assert!(supplied.credentials.is_some());
    }

    /// The ADC path reads `GOOGLE_APPLICATION_CREDENTIALS` from the injected environment and
    /// returns the bare `AdcError`.
    #[test]
    fn adc_errors_are_returned_unwrapped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("creds.json");
        std::fs::write(&file, r#"{"type":"bogus"}"#).unwrap();
        let env = MapEnv(HashMap::from([(
            "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
            file.to_string_lossy().into_owned(),
        )]));
        let err = resolve(None, SESSION_ENDPOINT, None, false, None, &env)
            .err()
            .unwrap();
        assert_eq!(err.to_string(), "unknown credential type: bogus");
        assert!(err.downcast_ref::<AdcError>().is_some());

        // A known type outside a runtime is refused instead of panicking.
        std::fs::write(&file, r#"{"type":"authorized_user"}"#).unwrap();
        let err = resolve(None, SESSION_ENDPOINT, None, false, None, &env)
            .err()
            .unwrap();
        assert_eq!(err.to_string(), NO_RUNTIME);
    }

    #[test]
    fn http_client_is_kept_when_supplied() {
        let client: Arc<dyn HttpClient> = Arc::new(ReqwestClient::default());
        let resolved = resolve(
            None,
            SESSION_ENDPOINT,
            None,
            true,
            Some(Arc::clone(&client)),
            &empty_env(),
        )
        .unwrap();
        assert!(Arc::ptr_eq(&resolved.http, &client));
    }
}
