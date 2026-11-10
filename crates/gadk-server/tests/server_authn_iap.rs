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
//! Ports of ADK-Go `server/authn/iap_test.go`. Each Go subtest is a named case; a failure names
//! it.

use gadk_server::authn::{AuthnError, USER_EMAIL_HEADER, USER_ID_HEADER, identity_aware_proxy};
use pretty_assertions::assert_eq;
use serde_json::json;

/// A case: name, `X-Goog-Authenticated-User-Id` value, `X-Goog-Authenticated-User-Email` value
/// (empty = header not set), and the wanted user ID and email (`None` = Unauthenticated).
type Case = (&'static str, String, String, Option<(&'static str, String)>);

#[tokio::test]
async fn test_identity_aware_proxy_strips_prefix() {
    const PREFIX: &str = "accounts.google.com:";
    let p = |s: &str| format!("{PREFIX}{s}");

    let cases: Vec<Case> = vec![
        (
            "both prefixed values are stripped",
            p("108100000000000000000"),
            p("alice@example.com"),
            Some(("108100000000000000000", "alice@example.com".into())),
        ),
        (
            "only the first prefix occurrence is stripped",
            p("108"),
            p(&p("weird@example.com")),
            Some(("108", p("weird@example.com"))),
        ),
        (
            "id without the prefix is rejected",
            "108100000000000000000".into(),
            p("alice@example.com"),
            None,
        ),
        (
            "email without the prefix is rejected",
            p("108100000000000000000"),
            "alice@example.com".into(),
            None,
        ),
        (
            "missing id header is unauthenticated",
            String::new(),
            p("alice@example.com"),
            None,
        ),
        (
            "missing email header is unauthenticated",
            p("108100000000000000000"),
            String::new(),
            None,
        ),
        (
            "both headers missing is unauthenticated",
            String::new(),
            String::new(),
            None,
        ),
    ];

    let iap = identity_aware_proxy();
    for (name, id, email, want) in cases {
        let mut b = http::Request::get("/");
        if !id.is_empty() {
            b = b.header(USER_ID_HEADER, id);
        }
        if !email.is_empty() {
            b = b.header(USER_EMAIL_HEADER, email);
        }
        let (parts, ()) = b.body(()).unwrap().into_parts();

        let got = iap.authenticate(&parts).await;
        let Some((want_user, want_email)) = want else {
            assert!(
                matches!(got, Err(AuthnError::Unauthenticated(_))),
                "{name}: Authenticate() = {got:?}, want Unauthenticated"
            );
            continue;
        };
        let caller = got
            .unwrap_or_else(|e| panic!("{name}: Authenticate() error = {e}"))
            .unwrap_or_else(|| panic!("{name}: Authenticate() returned no caller"));
        assert_eq!(caller.user_id, want_user, "{name}: Caller.UserID");
        assert_eq!(
            caller.claims.unwrap_or_default().get("email"),
            Some(&json!(want_email)),
            "{name}: Claims[email]"
        );
    }
}
