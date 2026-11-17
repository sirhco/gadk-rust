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

//! The identity headers Google Cloud Identity-Aware Proxy sets.

use std::sync::Arc;

use super::{Authenticator, AuthnError, Caller};

/// Header carrying the IAP-authenticated user's email.
pub const USER_EMAIL_HEADER: &str = "X-Goog-Authenticated-User-Email";
/// Header carrying the IAP-authenticated user's ID.
pub const USER_ID_HEADER: &str = "X-Goog-Authenticated-User-Id";

/// The prefix IAP puts on both header values.
const PREFIX: &str = "accounts.google.com:";

struct IdentityAwareProxy;

/// The caller is the IAP-authenticated user (user ID
/// from [`USER_ID_HEADER`], claim `email` from [`USER_EMAIL_HEADER`], both without IAP's
/// `accounts.google.com:` prefix).
pub fn identity_aware_proxy() -> Arc<dyn Authenticator> {
    Arc::new(IdentityAwareProxy)
}

/// The first value of `name`, empty when absent.
fn first(req: &http::request::Parts, name: &str) -> String {
    req.headers
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default()
}

#[async_trait::async_trait]
impl Authenticator for IdentityAwareProxy {
    async fn authenticate(&self, req: &http::request::Parts) -> Result<Option<Caller>, AuthnError> {
        let email = first(req, USER_EMAIL_HEADER);
        let user_id = first(req, USER_ID_HEADER);
        if email.is_empty() || user_id.is_empty() {
            return Err(AuthnError::Unauthenticated(None));
        }
        let Some(email) = email.strip_prefix(PREFIX) else {
            return Err(AuthnError::Unauthenticated(Some(
                "email header is missing the expected prefix".into(),
            )));
        };
        let Some(user_id) = user_id.strip_prefix(PREFIX) else {
            return Err(AuthnError::Unauthenticated(Some(
                "user id header is missing the expected prefix".into(),
            )));
        };
        Ok(Some(Caller {
            user_id: user_id.to_owned(),
            claims: Some(serde_json::Map::from_iter([(
                "email".to_owned(),
                serde_json::Value::String(email.to_owned()),
            )])),
        }))
    }
}
