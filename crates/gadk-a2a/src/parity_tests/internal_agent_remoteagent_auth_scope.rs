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

//! Ports of ADK-Go `internal/agent/remoteagent/auth_scope_test.go`.
//!
//! Go `AttachAuthScope(ctx, cfg, id)` attaches the scope and the card-fetch client only when
//! `cfg.OwnsAuthScope`; here the caller makes that decision by handing [`with_auth_scope`] a
//! `Some(CallIdentity)` or `None`, and Go's context values are the `AUTH_SCOPE` task-local.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use crate::remoteagent::auth::{
    CallIdentity, CredentialCell, CredentialScope, credential_scope, current_card_fetch_client,
    current_credential_scope, with_auth_scope,
};

fn identity(scope: CredentialScope, client: Option<reqwest::Client>) -> CallIdentity {
    CallIdentity {
        scope,
        identity: None,
        auth: gadk_auth::static_token("tok"),
        card_fetch_client: client,
        cell: Arc::new(CredentialCell::new()),
    }
}

/// Go `TestAttachAuthScope`.
// parity: adapted: OwnsAuthScope is the caller's choice of Some/None; reqwest::Client has no identity, so "the client was attached" is checked as presence
#[tokio::test]
async fn test_attach_auth_scope() {
    let client = reqwest::Client::new();
    let id = identity(credential_scope("shop", "alice", "s1", "crm"), Some(client));

    // owned
    let (scope, has_client) = with_auth_scope(Some(id.clone()), async {
        (
            current_credential_scope(),
            current_card_fetch_client().is_some(),
        )
    })
    .await;
    assert_eq!(scope, Some(CredentialScope("shop/alice/s1/crm".into())));
    assert!(
        has_client,
        "the card-fetch client was not attached, so a card fetched for this cancel would go out unauthenticated"
    );

    // not owned leaves the caller's scope alone
    let theirs = identity(CredentialScope("caller-tenant".into()), None);
    let (scope, has_client) = with_auth_scope(
        Some(theirs),
        with_auth_scope(None, async {
            (
                current_credential_scope(),
                current_card_fetch_client().is_some(),
            )
        }),
    )
    .await;
    assert_eq!(
        scope,
        Some(CredentialScope("caller-tenant".into())),
        "a caller who wired their own auth owns the key"
    );
    assert!(
        !has_client,
        "a card-fetch client was attached to a scope this SDK does not own"
    );
}

/// Go `TestCardFetchClientFromEmptyContext`.
#[tokio::test]
async fn test_card_fetch_client_from_empty_context() {
    assert!(current_card_fetch_client().is_none());
    let none = identity(credential_scope("a", "b", "c", "d"), None);
    let got = with_auth_scope(Some(none), async { current_card_fetch_client() }).await;
    assert!(got.is_none(), "a scope without a client attached one");
}
