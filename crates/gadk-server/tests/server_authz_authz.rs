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
//! Ports of ADK-Go `server/authz/authz_test.go`. Go's `ctx` argument is the request's
//! [`Caller`] (`None` for a context without one, and for Go's nil context). Each Go subtest is a
//! named case; a failure names it.

use gadk_server::authn::Caller;
use gadk_server::authz::{AuthzError, noop, strict, write_http_status_for_auth_error};
use http::StatusCode;
use pretty_assertions::assert_eq;

/// Go `ctxWithUser`: the caller `authn.Middleware` would put on the request.
fn caller(user_id: &str) -> Option<Caller> {
    Some(Caller {
        user_id: user_id.to_owned(),
        claims: None,
    })
}

#[test]
fn test_strict_can_act_as_user() {
    let cases = [
        ("matching user is allowed", caller("alice"), "alice", true),
        ("different user is denied", caller("alice"), "bob", false),
        (
            "authenticated user cannot act as a different empty-string user",
            caller("alice"),
            "",
            false,
        ),
        (
            "empty identity cannot act as a named user",
            caller(""),
            "alice",
            false,
        ),
        // Strict compares user IDs verbatim, so an empty identity (what authn::noop yields)
        // matches the empty-string user.
        (
            "empty identity matches the empty-string user",
            caller(""),
            "",
            true,
        ),
    ];

    let s = strict();
    for (name, c, path_user_id, want_allow) in cases {
        let got = s.can_act_as_user(c.as_ref(), path_user_id);
        if want_allow {
            assert!(got.is_ok(), "{name}: CanActAsUser() = {got:?}, want Ok");
            continue;
        }
        let err = got.expect_err(name);
        assert!(
            err.to_string().starts_with("authz: unauthorized"),
            "{name}: CanActAsUser() error = {err}, want it to wrap authz.ErrUnauthorized"
        );
    }
}

#[test]
fn test_strict_denies_without_caller_identity() {
    let s = strict();
    for name in ["background context has no identity", "nil context"] {
        let err = s.can_act_as_user(None, "alice").expect_err(name);
        assert_eq!(
            err.to_string(),
            "authz: unauthorized: no caller found in context",
            "{name}"
        );
    }
}

#[test]
fn test_noop_allows_everything() {
    let n = noop();
    let cases = [
        ("matching caller's identity", caller("alice"), "alice"),
        (
            "mismatched caller's identity is still allowed",
            caller("alice"),
            "bob",
        ),
        ("no caller's identity is still allowed", None, "alice"),
        ("empty user is allowed", None, ""),
    ];
    for (name, c, path_user_id) in cases {
        let got = n.can_act_as_user(c.as_ref(), path_user_id);
        assert!(
            got.is_ok(),
            "{name}: CanActAsUser() = {got:?}, want Ok (Noop permits everything)"
        );
    }
}

#[tokio::test]
async fn test_write_http_status_for_auth_error() {
    let mismatch: Option<AuthzError> = strict()
        .can_act_as_user(caller("alice").as_ref(), "bob")
        .err();
    let cases = [
        (
            "with error appends its message",
            mismatch,
            "forbidden: authz: unauthorized: caller doesn't match the provided userID\n",
        ),
        ("nil error writes a bare message", None, "forbidden\n"),
    ];

    for (name, err, want_body) in cases {
        let resp = write_http_status_for_auth_error(err.as_ref());
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "{name}: authorization failure is Forbidden, not Unauthorized"
        );
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "text/plain; charset=utf-8",
            "{name}"
        );
        assert_eq!(
            resp.headers().get("x-content-type-options").unwrap(),
            "nosniff",
            "{name}"
        );
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            want_body,
            "{name}"
        );
    }
}
