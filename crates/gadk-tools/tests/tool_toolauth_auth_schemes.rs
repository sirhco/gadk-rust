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

//! Port of ADK-Go `tool/toolauth/auth_schemes_test.go`.

use std::collections::BTreeMap;

use gadk_tools::toolauth::{Extra, OAuthFlow, OAuthFlows, OAuthGrantType, Scheme, SchemeType};
use serde_json::value::RawValue;

const TEST_TOKEN_URL: &str = "https://example.com/token";
const TEST_AUTH_URL: &str = "https://example.com/authorize";
const TEST_REFRESH_URL: &str = "https://example.com/refresh";

/// Go `decodeJSON`: decodes for a comparison that ignores key order and spacing.
fn decode_json(data: &str) -> serde_json::Value {
    serde_json::from_str(data).unwrap_or_else(|e| panic!("decoding JSON: {e}"))
}

/// Builds an `Extra` from `(key, raw JSON text)` pairs; Go's `json.Number("1")` is `"1"`, its
/// string `"x"` is `"\"x\""`.
fn extra(pairs: &[(&str, &str)]) -> Extra {
    pairs
        .iter()
        .map(|(k, v)| {
            (
                (*k).to_owned(),
                RawValue::from_string((*v).to_owned()).unwrap(),
            )
        })
        .collect()
}

fn s(v: &str) -> String {
    v.to_owned()
}

struct FlowCase {
    name: &'static str,
    flows: OAuthFlows,
    json: &'static str,
}

/// Go `pythonFlowCases`: OAuth flows with the JSON adk-python 2.12.0 writes for them.
fn python_flow_cases() -> Vec<FlowCase> {
    vec![
        FlowCase {
            name: "authorization code",
            flows: OAuthFlows {
                authorization_code: Some(OAuthFlow {
                    authorization_url: s(TEST_AUTH_URL),
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                ..Default::default()
            },
            json: r#"{"authorizationCode":{"scopes":{},"authorizationUrl":"https://example.com/authorize","tokenUrl":"https://example.com/token"}}"#,
        },
        FlowCase {
            name: "client credentials with a scope",
            flows: OAuthFlows {
                client_credentials: Some(OAuthFlow {
                    token_url: s(TEST_TOKEN_URL),
                    scopes: BTreeMap::from([(s("read"), s("Read access"))]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            json: r#"{"clientCredentials":{"scopes":{"read":"Read access"},"tokenUrl":"https://example.com/token"}}"#,
        },
        FlowCase {
            name: "implicit with a refresh URL",
            flows: OAuthFlows {
                implicit: Some(OAuthFlow {
                    authorization_url: s(TEST_AUTH_URL),
                    refresh_url: s(TEST_REFRESH_URL),
                    ..Default::default()
                }),
                ..Default::default()
            },
            json: r#"{"implicit":{"refreshUrl":"https://example.com/refresh","scopes":{},"authorizationUrl":"https://example.com/authorize"}}"#,
        },
        FlowCase {
            name: "password",
            flows: OAuthFlows {
                password: Some(OAuthFlow {
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                ..Default::default()
            },
            json: r#"{"password":{"scopes":{},"tokenUrl":"https://example.com/token"}}"#,
        },
        FlowCase {
            name: "all four flows",
            flows: OAuthFlows {
                implicit: Some(OAuthFlow {
                    authorization_url: s(TEST_AUTH_URL),
                    ..Default::default()
                }),
                password: Some(OAuthFlow {
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                client_credentials: Some(OAuthFlow {
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                authorization_code: Some(OAuthFlow {
                    authorization_url: s(TEST_AUTH_URL),
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                ..Default::default()
            },
            json: r#"{"implicit":{"scopes":{},"authorizationUrl":"https://example.com/authorize"},"password":{"scopes":{},"tokenUrl":"https://example.com/token"},"clientCredentials":{"scopes":{},"tokenUrl":"https://example.com/token"},"authorizationCode":{"scopes":{},"authorizationUrl":"https://example.com/authorize","tokenUrl":"https://example.com/token"}}"#,
        },
        // fastapi's models take no snake_case names, so authorization_url is an unknown key
        // next to authorizationUrl, kept and written back.
        FlowCase {
            name: "unknown keys",
            flows: OAuthFlows {
                authorization_code: Some(OAuthFlow {
                    authorization_url: s(TEST_AUTH_URL),
                    token_url: s(TEST_TOKEN_URL),
                    extra: extra(&[("authorization_url", r#""x""#), ("x-vendor", "1")]),
                    ..Default::default()
                }),
                extra: extra(&[("x-flows", "true")]),
                ..Default::default()
            },
            json: r#"{"authorizationCode":{"scopes":{},"authorizationUrl":"https://example.com/authorize","tokenUrl":"https://example.com/token","authorization_url":"x","x-vendor":1},"x-flows":true}"#,
        },
        FlowCase {
            name: "no flows",
            flows: OAuthFlows::default(),
            json: "{}",
        },
    ]
}

#[test]
fn test_o_auth_flows_marshal_matches_python() {
    for tc in python_flow_cases() {
        let got = serde_json::to_string(&tc.flows)
            .unwrap_or_else(|e| panic!("{}: serde_json::to_string() error = {e}", tc.name));
        assert_eq!(
            decode_json(&got),
            decode_json(tc.json),
            "{}: serde_json::to_string() differs from adk-python",
            tc.name
        );
    }
}

#[test]
fn test_o_auth_flows_unmarshal_python_json() {
    for tc in python_flow_cases() {
        let got: OAuthFlows = serde_json::from_str(tc.json)
            .unwrap_or_else(|e| panic!("{}: serde_json::from_str() error = {e}", tc.name));
        // Python writes empty scopes as {}, which decodes to the empty map the case has.
        assert!(
            got == tc.flows,
            "{}: serde_json::from_str() mismatch: got {got:?}, want {:?}",
            tc.name,
            tc.flows
        );
    }
}

#[test]
fn test_o_auth_flows_unmarshal_rejects() {
    // adk-python 2.12.0 rejects every one of these.
    let tests = [
        (
            "implicit without authorizationUrl",
            r#"{"implicit":{}}"#,
            "implicit flow needs authorizationUrl",
        ),
        (
            "password without tokenUrl",
            r#"{"password":{}}"#,
            "password flow needs tokenUrl",
        ),
        (
            "client credentials without tokenUrl",
            r#"{"clientCredentials":{}}"#,
            "clientCredentials flow needs tokenUrl",
        ),
        (
            "authorization code without tokenUrl",
            r#"{"authorizationCode":{"authorizationUrl":"https://example.com/authorize"}}"#,
            "authorizationCode flow needs tokenUrl",
        ),
        (
            "authorization code without authorizationUrl",
            r#"{"authorizationCode":{"tokenUrl":"https://example.com/token"}}"#,
            "authorizationCode flow needs authorizationUrl",
        ),
        (
            "snake_case name only",
            r#"{"password":{"token_url":"https://example.com/token"}}"#,
            "password flow needs tokenUrl",
        ),
    ];
    for (name, data, want_err) in tests {
        let Err(err) = serde_json::from_str::<OAuthFlows>(data) else {
            panic!("{name}: serde_json::from_str() succeeded, want an error");
        };
        assert!(
            err.to_string().contains(want_err),
            "{name}: serde_json::from_str() error = {:?}, want it to contain {want_err:?}",
            err.to_string()
        );
    }
}

#[test]
fn test_o_auth_flows_grant_type() {
    let tests = [
        (
            "client credentials",
            OAuthFlows {
                client_credentials: Some(OAuthFlow {
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(OAuthGrantType::ClientCredentials),
        ),
        (
            "authorization code",
            OAuthFlows {
                authorization_code: Some(OAuthFlow {
                    authorization_url: s(TEST_AUTH_URL),
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(OAuthGrantType::AuthorizationCode),
        ),
        (
            "implicit",
            OAuthFlows {
                implicit: Some(OAuthFlow {
                    authorization_url: s(TEST_AUTH_URL),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(OAuthGrantType::Implicit),
        ),
        (
            "password",
            OAuthFlows {
                password: Some(OAuthFlow {
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(OAuthGrantType::Password),
        ),
        // An OAuth2 scheme declaring no flow has no grant type to exchange with.
        ("no flow", OAuthFlows::default(), None),
        // adk-python's from_flow checks client credentials first.
        (
            "client credentials before authorization code",
            OAuthFlows {
                client_credentials: Some(OAuthFlow {
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                authorization_code: Some(OAuthFlow {
                    authorization_url: s(TEST_AUTH_URL),
                    token_url: s(TEST_TOKEN_URL),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(OAuthGrantType::ClientCredentials),
        ),
    ];
    for (name, flows, want) in tests {
        assert_eq!(flows.grant_type(), want, "{name}: grant_type()");
    }
}

#[test]
fn test_o_auth_grant_type_values_are_wire_names() {
    // These strings go on the wire as the OAuth2 grant_type parameter, so they must stay
    // exactly as RFC 6749 names them.
    let tests = [
        (OAuthGrantType::ClientCredentials, "client_credentials"),
        (OAuthGrantType::AuthorizationCode, "authorization_code"),
        (OAuthGrantType::Implicit, "implicit"),
        (OAuthGrantType::Password, "password"),
    ];
    for (got, want) in tests {
        assert_eq!(got.as_str(), want, "grant type {got:?}");
    }
}

const TEST_OIDC_DISCOVERY_URL: &str = "https://example.com/.well-known/openid-configuration";
const TEST_USERINFO_URL: &str = "https://example.com/userinfo";
const TEST_REVOKE_URL: &str = "https://example.com/revoke";

struct SchemeCase {
    name: &'static str,
    scheme: Scheme,
    json: &'static str,
}

/// Go `pythonSchemeCases`: auth schemes with the JSON adk-python 2.12.0 writes for the same
/// scheme after reading it into an AuthConfig.
fn python_scheme_cases() -> Vec<SchemeCase> {
    vec![
        SchemeCase {
            name: "API key",
            scheme: Scheme {
                r#type: SchemeType::API_KEY,
                r#in: s("header"),
                name: s("X-Api-Key"),
                ..Default::default()
            },
            json: r#"{"type":"apiKey","in":"header","name":"X-Api-Key"}"#,
        },
        SchemeCase {
            name: "HTTP basic",
            scheme: Scheme {
                r#type: SchemeType::HTTP,
                description: s("Basic auth"),
                scheme: s("basic"),
                ..Default::default()
            },
            json: r#"{"type":"http","description":"Basic auth","scheme":"basic"}"#,
        },
        SchemeCase {
            name: "HTTP bearer with a format",
            scheme: Scheme {
                r#type: SchemeType::HTTP,
                scheme: s("bearer"),
                bearer_format: s("JWT"),
                ..Default::default()
            },
            json: r#"{"type":"http","scheme":"bearer","bearerFormat":"JWT"}"#,
        },
        SchemeCase {
            name: "OAuth2",
            scheme: Scheme {
                r#type: SchemeType::OAUTH2,
                flows: Some(OAuthFlows {
                    authorization_code: Some(OAuthFlow {
                        authorization_url: s(TEST_AUTH_URL),
                        token_url: s(TEST_TOKEN_URL),
                        scopes: BTreeMap::from([(s("read"), s("Read access"))]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            json: r#"{"type":"oauth2","flows":{"authorizationCode":{"scopes":{"read":"Read access"},"authorizationUrl":"https://example.com/authorize","tokenUrl":"https://example.com/token"}}}"#,
        },
        SchemeCase {
            name: "OAuth2 with an issuer",
            scheme: Scheme {
                r#type: SchemeType::OAUTH2,
                flows: Some(OAuthFlows::default()),
                issuer_url: s("https://issuer.example.com"),
                ..Default::default()
            },
            json: r#"{"type":"oauth2","flows":{},"issuer_url":"https://issuer.example.com"}"#,
        },
        SchemeCase {
            name: "OpenID Connect discovery URL",
            scheme: Scheme {
                r#type: SchemeType::OPEN_ID_CONNECT,
                open_id_connect_url: s(TEST_OIDC_DISCOVERY_URL),
                ..Default::default()
            },
            json: r#"{"type":"openIdConnect","openIdConnectUrl":"https://example.com/.well-known/openid-configuration"}"#,
        },
        SchemeCase {
            name: "OpenID Connect endpoints",
            scheme: Scheme {
                r#type: SchemeType::OPEN_ID_CONNECT,
                authorization_endpoint: s(TEST_AUTH_URL),
                token_endpoint: s(TEST_TOKEN_URL),
                userinfo_endpoint: s(TEST_USERINFO_URL),
                revocation_endpoint: s(TEST_REVOKE_URL),
                token_endpoint_auth_methods_supported: Some(vec![s("client_secret_basic")]),
                grant_types_supported: Some(vec![s("authorization_code")]),
                scopes: Some(vec![s("openid"), s("email")]),
                ..Default::default()
            },
            json: r#"{"type":"openIdConnect","authorization_endpoint":"https://example.com/authorize","token_endpoint":"https://example.com/token","userinfo_endpoint":"https://example.com/userinfo","revocation_endpoint":"https://example.com/revoke","token_endpoint_auth_methods_supported":["client_secret_basic"],"grant_types_supported":["authorization_code"],"scopes":["openid","email"]}"#,
        },
        SchemeCase {
            name: "OpenID Connect endpoints with empty lists",
            scheme: Scheme {
                r#type: SchemeType::OPEN_ID_CONNECT,
                authorization_endpoint: s(TEST_AUTH_URL),
                token_endpoint: s(TEST_TOKEN_URL),
                token_endpoint_auth_methods_supported: Some(vec![]),
                grant_types_supported: Some(vec![]),
                scopes: Some(vec![]),
                ..Default::default()
            },
            json: r#"{"type":"openIdConnect","authorization_endpoint":"https://example.com/authorize","token_endpoint":"https://example.com/token","token_endpoint_auth_methods_supported":[],"grant_types_supported":[],"scopes":[]}"#,
        },
        SchemeCase {
            name: "custom scheme",
            scheme: Scheme {
                r#type: SchemeType::from("mock_custom_type"),
                extra: extra(&[("x-provider", r#""acme""#)]),
                ..Default::default()
            },
            json: r#"{"type":"mock_custom_type","x-provider":"acme"}"#,
        },
        // adk-python reads this as a custom scheme, since "body" is not an OpenAPI key
        // location, and writes it back unchanged.
        SchemeCase {
            name: "API key in an unknown location",
            scheme: Scheme {
                r#type: SchemeType::API_KEY,
                r#in: s("body"),
                name: s("X"),
                ..Default::default()
            },
            json: r#"{"type":"apiKey","in":"body","name":"X"}"#,
        },
        // The scheme models take no other spelling of type, so type_ is an unknown key.
        SchemeCase {
            name: "custom scheme with type_",
            scheme: Scheme {
                r#type: SchemeType::from("custom"),
                extra: extra(&[("type_", r#""other""#)]),
                ..Default::default()
            },
            json: r#"{"type":"custom","type_":"other"}"#,
        },
    ]
}

#[test]
fn test_scheme_marshal_matches_python() {
    for tc in python_scheme_cases() {
        let got = serde_json::to_string(&tc.scheme)
            .unwrap_or_else(|e| panic!("{}: serde_json::to_string() error = {e}", tc.name));
        assert_eq!(
            decode_json(&got),
            decode_json(tc.json),
            "{}: serde_json::to_string() differs from adk-python",
            tc.name
        );
    }
}

#[test]
fn test_scheme_unmarshal_python_json() {
    for tc in python_scheme_cases() {
        let got: Scheme = serde_json::from_str(tc.json)
            .unwrap_or_else(|e| panic!("{}: serde_json::from_str() error = {e}", tc.name));
        assert!(
            got == tc.scheme,
            "{}: serde_json::from_str() mismatch: got {got:?}, want {:?}",
            tc.name,
            tc.scheme
        );
    }
}

#[test]
fn test_scheme_infers_missing_type() {
    // Each input has no type. want is what adk-python 2.12.0 writes after reading it, with
    // the type its models inferred.
    let tests = [
        (
            "API key",
            r#"{"in":"header","name":"X-Api-Key"}"#,
            r#"{"type":"apiKey","in":"header","name":"X-Api-Key"}"#,
        ),
        (
            "HTTP",
            r#"{"scheme":"basic"}"#,
            r#"{"type":"http","scheme":"basic"}"#,
        ),
        (
            "OAuth2",
            r#"{"flows":{"password":{"tokenUrl":"https://example.com/token"}}}"#,
            r#"{"type":"oauth2","flows":{"password":{"scopes":{},"tokenUrl":"https://example.com/token"}}}"#,
        ),
        (
            "OpenID Connect discovery URL",
            r#"{"openIdConnectUrl":"https://example.com/.well-known/openid-configuration"}"#,
            r#"{"type":"openIdConnect","openIdConnectUrl":"https://example.com/.well-known/openid-configuration"}"#,
        ),
        (
            "OpenID Connect endpoints",
            r#"{"authorization_endpoint":"https://example.com/authorize","token_endpoint":"https://example.com/token"}"#,
            r#"{"type":"openIdConnect","authorization_endpoint":"https://example.com/authorize","token_endpoint":"https://example.com/token"}"#,
        ),
    ];
    for (name, input, want) in tests {
        let scheme: Scheme = serde_json::from_str(input)
            .unwrap_or_else(|e| panic!("{name}: serde_json::from_str() error = {e}"));
        let got = serde_json::to_string(&scheme)
            .unwrap_or_else(|e| panic!("{name}: serde_json::to_string() error = {e}"));
        assert_eq!(
            decode_json(&got),
            decode_json(want),
            "{name}: round trip differs from adk-python"
        );
    }
}

#[test]
fn test_scheme_rejects_scheme_with_nothing_to_infer_type_from() {
    // adk-python 2.12.0 reads both as {"type":"http","scheme":"bearer"}, only because
    // fastapi's HTTPBearer has a default for every field.
    for data in ["{}", r#"{"description":"d"}"#] {
        let err = serde_json::from_str::<Scheme>(data).err();
        assert!(
            err.as_ref()
                .is_some_and(|e| e.to_string().contains("missing type")),
            "serde_json::from_str({data}) error = {err:?}, want a missing type error"
        );
    }
}
