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

//! Port of ADK-Go `internal/sessionutils/requested_auth_test.go`.

use std::collections::BTreeMap;

use gadk_session::state_util::check_requested_auth_configs;
use gadk_session::toolauth::{
    Config, Credential, CredentialType, OAuth2Auth, OAuthFlow, OAuthFlows, Scheme, SchemeType,
};
use serde_json::value::RawValue;

// TestCheckRequestedAuthConfigsReadsNumbersAsFloat64 pins the part of the check that goes beyond
// a plain JSON round trip: the Vertex AI service stores numbers as float64, so a config that
// conversion breaks is refused by every service that calls the check.
#[test]
fn test_check_requested_auth_configs_reads_numbers_as_float64() {
    let scheme = Scheme {
        r#type: SchemeType::OAUTH2,
        flows: Some(OAuthFlows {
            client_credentials: Some(OAuthFlow {
                token_url: "https://example.com/token".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let oauth2 = |expires_at: i64| Config {
        auth_scheme: scheme.clone(),
        raw_auth_credential: Some(Credential {
            auth_type: CredentialType::OAUTH2,
            oauth2: Some(OAuth2Auth {
                client_id: "cid".into(),
                expires_at,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let tests = [
        // 2^53 + 1 loses its last digit as a float64 but still fits.
        (
            "expiresAt that float64 rounds within range",
            oauth2(9_007_199_254_740_993),
            false,
        ),
        // As a float64 this is 2^63, which does not fit expiresAt.
        (
            "expiresAt that float64 rounds out of range",
            oauth2(i64::MAX),
            true,
        ),
        // Extra keeps a number as written, but no float64 holds it.
        (
            "number in extra beyond float64",
            Config {
                auth_scheme: Scheme {
                    r#type: SchemeType::API_KEY,
                    r#in: "header".into(),
                    name: "X-Key".into(),
                    ..Default::default()
                },
                extra: BTreeMap::from([(
                    "big".into(),
                    RawValue::from_string("1e400".into()).unwrap(),
                )]),
                ..Default::default()
            },
            true,
        ),
    ];
    for (name, config, want_err) in tests {
        let err = check_requested_auth_configs(&BTreeMap::from([("call-1".into(), config)]));
        assert_eq!(
            err.is_err(),
            want_err,
            "{name}: CheckRequestedAuthConfigs() error = {err:?}, wantErr {want_err}"
        );
    }
}
