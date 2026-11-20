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

//! Request credentials: Application Default Credentials or none.

use gadk_model::gcp_auth::{adc_credentials, credential_headers};
use google_cloud_auth::credentials::Credentials;
use http::HeaderMap;
use tokio::sync::OnceCell;

/// The OAuth scope requested for Application Default Credentials.
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Produces the headers that authenticate one BigQuery request.
#[async_trait::async_trait]
pub trait AuthHeaders: Send + Sync {
    /// The headers for one request, typically `Authorization: Bearer …`; the error says why none
    /// could be produced and becomes [`BigQueryError::Credentials`](crate::BigQueryError::Credentials).
    async fn headers(&self) -> Result<HeaderMap, String>;
}

/// Application Default Credentials with the `cloud-platform` scope.
#[non_exhaustive]
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

    /// Credentials built now, so most credential problems are reported up front. Without an ADC
    /// file the credentials may come from the metadata server, and some failures (no metadata
    /// server, say) only show on the first request. Must be called inside a Tokio runtime.
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

impl std::fmt::Debug for Adc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Adc")
            .field("built", &self.credentials.initialized())
            .finish_non_exhaustive()
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

/// No credentials: every request is sent without authentication headers (tests, emulators).
///
/// A unit struct callers write as `NoAuth`, like `gadk-cloudtrace`'s; it is the one public type
/// left without `#[non_exhaustive]`, since marking a unit struct would stop callers naming it.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAuth;

#[async_trait::async_trait]
impl AuthHeaders for NoAuth {
    async fn headers(&self) -> Result<HeaderMap, String> {
        Ok(HeaderMap::new())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_auth_sends_no_headers() {
        assert!(NoAuth.headers().await.unwrap().is_empty());
    }

    #[test]
    fn lazy_adc_debug_shows_no_credentials() {
        let text = format!("{:?}", Adc::lazy());
        assert_eq!(text, "Adc { built: false, .. }");
    }
}
