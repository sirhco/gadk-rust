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

//! Authorization for the ADK REST API (for the ADK REST API): whether the authenticated
//! [`Caller`] may act as the user a request names.

use http::StatusCode;

use crate::authn::Caller;
use crate::rest::http_error::http_error;

mod noop;
mod strict;

pub use noop::noop;
pub use strict::strict;

/// An "unauthorized" error, optionally with a reason (rendered `authz: unauthorized: <reason>`).
/// The text is sent to the client, so it must not carry PII.
#[derive(Debug, thiserror::Error)]
#[error("authz: unauthorized{}", .0.as_ref().map(|c| format!(": {c}")).unwrap_or_default())]
pub struct AuthzError(pub Option<String>);

/// Decides whether the authenticated caller may act as the user a request names.
pub trait Authorizer: Send + Sync {
    /// `Ok` iff `caller` (the request's authenticated [`Caller`] extension, `None` when the
    /// request carries none) may act as `user_id`.
    fn can_act_as_user(&self, caller: Option<&Caller>, user_id: &str) -> Result<(), AuthzError>;
}

/// A 403 response with the body `forbidden` or `forbidden: <err>`, framed like
/// [`http_error`] (plain text plus a trailing newline).
pub fn write_http_status_for_auth_error(err: Option<&AuthzError>) -> axum::response::Response {
    let msg = match err {
        Some(e) => format!("forbidden: {e}"),
        None => "forbidden".to_owned(),
    };
    http_error(StatusCode::FORBIDDEN, &msg)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn errors_carry_go_text() {
        assert_eq!(AuthzError(None).to_string(), "authz: unauthorized");
        assert_eq!(
            strict().can_act_as_user(None, "u").unwrap_err().to_string(),
            "authz: unauthorized: no caller found in context"
        );
        let caller = Caller {
            user_id: "a".into(),
            claims: None,
        };
        assert_eq!(
            strict()
                .can_act_as_user(Some(&caller), "b")
                .unwrap_err()
                .to_string(),
            "authz: unauthorized: caller doesn't match the provided userID"
        );
        assert!(strict().can_act_as_user(Some(&caller), "a").is_ok());
        assert!(noop().can_act_as_user(None, "anyone").is_ok());
    }
}
