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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Port of Go `auth/providers_test.go`.

use std::sync::Arc;

use futures::FutureExt as _;
use gadk_auth::{
    AuthContext, Bearer, ConsentRequiredError, Credential, CredentialFuture, CredentialProvider,
    ProviderFn, ServiceAccountConfig, StaticTokenSource, Token, adc, api_key, find_consent,
    service_account, static_token, token_source,
};
use http::HeaderMap;
use http::header::AUTHORIZATION;

/// Go `t.Context()`: no invocation identity.
fn cx() -> AuthContext {
    AuthContext::anonymous()
}

/// Go `resolvedAuth`: resolves the provider and returns the Authorization header its credential
/// produces.
async fn resolved_auth(p: &Arc<dyn CredentialProvider>) -> String {
    let cred = p
        .credential(&cx())
        .await
        .unwrap_or_else(|e| panic!("Credential() error = {e}"));
    let mut h = HeaderMap::new();
    if let Err(err) = cred.apply(&mut h).await {
        panic!("Apply() error = {err}");
    }
    h.get(AUTHORIZATION)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default()
}

#[tokio::test]
async fn test_static_token() {
    let got = resolved_auth(&static_token("abc")).await;
    assert_eq!(
        got, "Bearer abc",
        "Authorization = {got:?}, want \"Bearer abc\""
    );
}

#[tokio::test]
async fn test_api_key() {
    let cred = api_key("X-Api-Key", "secret")
        .credential(&cx())
        .await
        .unwrap_or_else(|e| panic!("Credential() error = {e}"));
    let mut h = HeaderMap::new();
    if let Err(err) = cred.apply(&mut h).await {
        panic!("Apply() error = {err}");
    }
    let got = h.get("X-Api-Key").and_then(|v| v.to_str().ok());
    assert_eq!(got, Some("secret"), "X-Api-Key = {got:?}, want \"secret\"");
}

#[tokio::test]
async fn test_token_source_provider() {
    let ts = Arc::new(StaticTokenSource(Token {
        access_token: "tok".into(),
        token_type: String::new(),
    }));
    let got = resolved_auth(&token_source(ts)).await;
    assert_eq!(
        got, "Bearer tok",
        "Authorization = {got:?}, want \"Bearer tok\""
    );
}

// parity: skipped TestTokenSourceProviderNil: `token_source` takes a non-optional `Arc<dyn TokenSource>`, so Go's nil token source cannot be passed.

#[tokio::test]
async fn test_service_account_invalid_key() {
    // Scopes are set so the invalid-key path (not the scopes gate) is exercised.
    let p = service_account(ServiceAccountConfig {
        json_key: b"not-json".to_vec(),
        scopes: vec!["https://www.googleapis.com/auth/cloud-platform".into()],
        ..Default::default()
    });
    match p.credential(&cx()).await {
        Ok(cred) => {
            panic!("Credential() = {cred:?}, want error for invalid service-account key")
        }
        Err(err) => assert!(
            err.to_string().starts_with("auth: credentials from json: "),
            "Credential() error = {err}, want the credentials-from-json wrapping"
        ),
    }
}

#[tokio::test]
async fn test_service_account_access_token_requires_scopes() {
    // Explicit key + no scopes must error (parity with adk-python), before any I/O.
    let p = service_account(ServiceAccountConfig {
        json_key: b"{}".to_vec(),
        ..Default::default()
    });
    match p.credential(&cx()).await {
        Ok(cred) => panic!("Credential() = {cred:?}, want error for missing scopes"),
        Err(err) => assert_eq!(
            err.to_string(),
            "auth: scopes are required for a service-account access token"
        ),
    }
}

#[test]
fn test_adc_returns_provider() {
    // Only assert construction; resolving ADC depends on the ambient environment and would
    // perform I/O, which the offline test suite avoids. An `Arc` is never nil, so what is left
    // to check is that construction does no I/O and needs no runtime (this is a plain `#[test]`).
    let p = adc(&["https://www.googleapis.com/auth/cloud-platform"]);
    assert!(
        p.as_refreshing().is_none(),
        "ADC() is a self-refreshing provider"
    );
}

#[tokio::test]
async fn test_provider_func() {
    let want = Bearer {
        token: "abc".into(),
    };
    let p: Arc<dyn CredentialProvider> = ProviderFn::new({
        let want = want.clone();
        move |_: &AuthContext| -> CredentialFuture {
            let cred: Arc<dyn Credential> = Arc::new(want.clone());
            std::future::ready(Ok(cred)).boxed()
        }
    })
    .into_provider();
    let got = p
        .credential(&cx())
        .await
        .unwrap_or_else(|e| panic!("Credential() error = {e}"));
    assert_eq!(
        got.downcast_ref::<Bearer>(),
        Some(&want),
        "Credential() = {got:?}, want {want:?}"
    );
}

#[test]
fn test_consent_required_error() {
    let err: Box<dyn std::error::Error + Send + Sync> = Box::new(ConsentRequiredError {
        auth_uri: "https://consent.example".into(),
        nonce: "n".into(),
        key: "k".into(),
    });

    let consent = find_consent(&*err).unwrap_or_else(|| panic!("errors.As failed for {err}"));
    assert_eq!(
        consent.auth_uri, "https://consent.example",
        "AuthURI = {:?}, want \"https://consent.example\"",
        consent.auth_uri
    );
    // The message must not carry the URI: it becomes a tool's error, which reaches the model and
    // the session store, and the URI carries the state and nonce that bind the credential.
    // Asserted exactly, not just by absence of the URI: the message has no branches, so a
    // negative check passes for an empty string too.
    assert_eq!(consent.to_string(), "auth: interactive consent required");
    assert!(
        !err.to_string().contains("consent.example"),
        "Error() = {err:?}, want the auth URI kept on the field only"
    );
}
