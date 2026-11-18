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

//! Request credentials: [`AuthHeaders`], [`Adc`] and [`NoAuth`].

use gadk_model::gcp_auth::{adc_credentials, credential_headers};
use google_cloud_auth::credentials::Credentials;
use http::HeaderMap;
use tokio::sync::OnceCell;

/// The OAuth scope requested for Application Default Credentials.
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// The headers that authenticate one Cloud Trace request.
#[async_trait::async_trait]
pub trait AuthHeaders: Send + Sync {
    /// The headers for one request, typically `Authorization: Bearer …`; the error says why none
    /// could be produced and becomes [`TraceError::Credentials`](crate::TraceError::Credentials).
    async fn headers(&self) -> Result<HeaderMap, String>;
}

/// Application Default Credentials with the `cloud-platform` scope.
pub struct Adc {
    credentials: OnceCell<Credentials>,
}

impl Adc {
    /// Credentials built on first use, so a program starts without them and reports a missing
    /// credential on its first request.
    pub fn lazy() -> Self {
        Self {
            credentials: OnceCell::new(),
        }
    }

    /// Credentials built now, so a missing credential is reported up front. Must be called
    /// inside a Tokio runtime.
    ///
    /// # Errors
    ///
    /// Why no credentials could be built.
    pub fn resolve() -> Result<Self, String> {
        let credentials = adc_credentials(None, &[SCOPE]).map_err(|e| e.to_string())?;
        Ok(Self {
            credentials: OnceCell::new_with(Some(credentials)),
        })
    }
}

#[async_trait::async_trait]
impl AuthHeaders for Adc {
    async fn headers(&self) -> Result<HeaderMap, String> {
        let credentials = self
            .credentials
            .get_or_try_init(|| async {
                adc_credentials(None, &[SCOPE]).map_err(|e| e.to_string())
            })
            .await?;
        credential_headers(credentials)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Sends no credentials (tests and emulators).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAuth;

#[async_trait::async_trait]
impl AuthHeaders for NoAuth {
    async fn headers(&self) -> Result<HeaderMap, String> {
        Ok(HeaderMap::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_auth_sends_no_headers() {
        assert!(NoAuth.headers().await.unwrap().is_empty());
    }

    #[test]
    fn lazy_adc_builds_no_credentials() {
        // Outside a Tokio runtime: building credentials here would fail, so `lazy` must not.
        let adc = Adc::lazy();
        assert!(!adc.credentials.initialized());
    }

    #[test]
    fn resolve_builds_credentials_now() {
        // Outside a Tokio runtime ADC cannot be built, so `resolve` reports it at once.
        let err = Adc::resolve().err().unwrap();
        assert!(err.contains("no Tokio runtime"), "{err}");
    }
}
