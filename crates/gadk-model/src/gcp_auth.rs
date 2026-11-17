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

//! Google Cloud credentials (feature `gcp-auth`), shared by the Google Cloud clients
//! (gadk-anthropic's Vertex AI backend and gadk-artifact-gcs): Application Default Credentials
//! and the request headers credentials produce.

use google_cloud_auth::credentials::{
    Builder, CacheableResource, Credentials, external_account, impersonated, service_account,
    user_account,
};
use http::HeaderMap;
use serde_json::Value;

use crate::http::error_chain;

/// The [`AdcError`] detail when [`adc_credentials`] is called outside a Tokio runtime.
pub const NO_RUNTIME: &str =
    "no Tokio runtime: Application Default Credentials must be built from within one";

/// Why [`adc_credentials`] found no credentials: the underlying detail (the file read, the
/// JSON parse, an unknown credential `"type"`, or google-cloud-auth's own error text). Callers
/// wrap it in their own error text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct AdcError {
    /// The failure detail.
    pub detail: String,
}

impl AdcError {
    fn new(detail: impl std::fmt::Display) -> Self {
        Self {
            detail: detail.to_string(),
        }
    }
}

/// Application Default Credentials with `scopes`. `credentials_file` is the caller's
/// `GOOGLE_APPLICATION_CREDENTIALS` value, read through its injected environment: a non-empty
/// path is read and dispatched on its `"type"` to google-cloud-auth's per-type builders
/// (`authorized_user`, `service_account`, `impersonated_service_account`, `external_account`);
/// `None` or an empty path falls back to google-cloud-auth's default builder, which reads the
/// real process environment, gcloud's well-known file and the metadata server.
///
/// # Errors
///
/// [`AdcError`] when the credentials file cannot be read or parsed, names an unknown type, or
/// the credentials cannot be built — including, with [`NO_RUNTIME`], when no Tokio runtime is
/// running on the calling thread (the built credentials start their token refresh on it).
pub fn adc_credentials(
    credentials_file: Option<&str>,
    scopes: &[&str],
) -> Result<Credentials, AdcError> {
    let scopes = scopes.iter().copied();
    let Some(path) = credentials_file.filter(|p| !p.is_empty()) else {
        require_runtime()?;
        return Builder::default()
            .with_scopes(scopes)
            .build()
            .map_err(AdcError::new);
    };
    let contents = std::fs::read(path).map_err(AdcError::new)?;
    let json: Value = serde_json::from_slice(&contents).map_err(AdcError::new)?;
    let kind = json.get("type").and_then(Value::as_str).unwrap_or_default();
    if matches!(
        kind,
        "authorized_user" | "service_account" | "impersonated_service_account" | "external_account"
    ) {
        require_runtime()?;
    }
    let built = match kind {
        "authorized_user" => user_account::Builder::new(json).with_scopes(scopes).build(),
        "service_account" => service_account::Builder::new(json)
            .with_access_specifier(service_account::AccessSpecifier::from_scopes(scopes))
            .build(),
        "impersonated_service_account" => {
            impersonated::Builder::new(json).with_scopes(scopes).build()
        }
        "external_account" => external_account::Builder::new(json)
            .with_scopes(scopes)
            .build(),
        other => return Err(AdcError::new(format!("unknown credential type: {other}"))),
    };
    built.map_err(AdcError::new)
}

/// google-cloud-auth's credential builders start a token cache with an unguarded
/// `tokio::spawn`, which panics outside a runtime: refuse with [`NO_RUNTIME`] instead.
fn require_runtime() -> Result<(), AdcError> {
    tokio::runtime::Handle::try_current()
        .map(drop)
        .map_err(|_| AdcError::new(NO_RUNTIME))
}

/// Why [`credential_headers`] produced no headers: google-cloud-auth's error and its sources
/// joined with `": "`. Callers wrap it in their own error text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct CredentialHeadersError {
    /// The failure detail.
    pub detail: String,
}

/// The headers `credentials` authenticate a request with, typically `Authorization: Bearer …`,
/// fetched for every request so a refreshed token is used.
pub async fn credential_headers(
    credentials: &Credentials,
) -> Result<HeaderMap, CredentialHeadersError> {
    match credentials.headers(http::Extensions::new()).await {
        Ok(CacheableResource::New { data, .. }) => Ok(data),
        // Only returned when the caller passes an entity tag, which gadk never does.
        Ok(CacheableResource::NotModified) => Err(CredentialHeadersError {
            detail: "credentials returned no headers".to_owned(),
        }),
        Err(e) => Err(CredentialHeadersError {
            detail: error_chain(&e),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use google_cloud_auth::credentials::{CredentialsProvider, EntityTag};
    use google_cloud_auth::errors::CredentialsError;
    use http::HeaderValue;

    use pretty_assertions::assert_eq;

    use super::*;

    const SCOPES: &[&str] = &["https://www.googleapis.com/auth/cloud-platform"];

    fn file(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    fn path(f: &tempfile::NamedTempFile) -> String {
        f.path().to_string_lossy().into_owned()
    }

    // Building credentials outside a runtime would panic in google-cloud-auth's token cache.
    #[test]
    fn outside_a_runtime_is_an_error() {
        let f = file(
            r#"{"type":"authorized_user","client_id":"c","client_secret":"s","refresh_token":"r"}"#,
        );
        let err = adc_credentials(Some(&path(&f)), SCOPES).unwrap_err();
        assert_eq!(err.detail, NO_RUNTIME);
    }

    // The built credentials start their token cache on the tokio runtime.
    #[tokio::test]
    async fn authorized_user_file_builds_credentials() {
        let f = file(
            r#"{"type":"authorized_user","client_id":"c","client_secret":"s","refresh_token":"r"}"#,
        );
        assert!(adc_credentials(Some(&path(&f)), SCOPES).is_ok());
    }

    #[test]
    fn missing_file_is_the_read_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let err = adc_credentials(Some(&missing.to_string_lossy()), SCOPES).unwrap_err();
        let want = std::fs::read(&missing).unwrap_err().to_string();
        assert_eq!(err, AdcError { detail: want });
    }

    #[test]
    fn non_json_file_is_the_parse_error() {
        let f = file("not json");
        let err = adc_credentials(Some(&path(&f)), SCOPES).unwrap_err();
        let want = serde_json::from_str::<Value>("not json")
            .unwrap_err()
            .to_string();
        assert_eq!(err.to_string(), want);
    }

    #[test]
    fn unknown_type_is_named() {
        let f = file(r#"{"type":"mystery"}"#);
        let err = adc_credentials(Some(&path(&f)), SCOPES).unwrap_err();
        assert_eq!(err.to_string(), "unknown credential type: mystery");
        let f = file("{}");
        let err = adc_credentials(Some(&path(&f)), SCOPES).unwrap_err();
        assert_eq!(err.to_string(), "unknown credential type: ");
    }

    #[test]
    fn a_builder_error_is_google_cloud_auths_text() {
        let f = file(r#"{"type":"authorized_user"}"#);
        let err = adc_credentials(Some(&path(&f)), SCOPES).unwrap_err();
        assert!(!err.detail.is_empty());
        assert!(!err.detail.starts_with("unknown credential type"), "{err}");
    }

    /// A provider returning fixed headers, `NotModified`, or an error.
    #[derive(Debug)]
    enum TestCreds {
        Headers(HeaderMap),
        NotModified,
        Fail,
    }

    impl CredentialsProvider for TestCreds {
        async fn headers(
            &self,
            _extensions: http::Extensions,
        ) -> Result<CacheableResource<HeaderMap>, CredentialsError> {
            match self {
                Self::Headers(h) => Ok(CacheableResource::New {
                    entity_tag: EntityTag::new(),
                    data: h.clone(),
                }),
                Self::NotModified => Ok(CacheableResource::NotModified),
                Self::Fail => Err(CredentialsError::from_msg(false, "boom")),
            }
        }

        async fn universe_domain(&self) -> Option<String> {
            None
        }
    }

    #[tokio::test]
    async fn credential_headers_are_the_providers() {
        let want = HeaderMap::from_iter([(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer t"),
        )]);
        let creds = Credentials::from(TestCreds::Headers(want.clone()));
        assert_eq!(credential_headers(&creds).await.unwrap(), want);

        let creds = Credentials::from(TestCreds::NotModified);
        assert_eq!(
            credential_headers(&creds).await.unwrap_err().to_string(),
            "credentials returned no headers"
        );

        let creds = Credentials::from(TestCreds::Fail);
        let err = credential_headers(&creds).await.unwrap_err();
        let want = error_chain(&CredentialsError::from_msg(false, "boom"));
        assert_eq!(err.detail, want);
    }
}
