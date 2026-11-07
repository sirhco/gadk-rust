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

//! Port of ADK-Go `tool/toolauth/redact_test.go`.

use std::collections::BTreeMap;
use std::fmt::Debug;

use gadk_tools::toolauth::{
    Config, Credential, CredentialType, Extra, HttpAuth, HttpCredentials, OAuth2Auth, OAuthFlows,
    Scheme, SchemeType, ServiceAccount, ServiceAccountCredential, ToolArguments,
};
use serde_json::value::RawValue;

fn s(v: &str) -> String {
    v.to_owned()
}

/// One extra holding a JSON string.
fn extra(key: &str, value: &str) -> Extra {
    Extra::from([(
        key.to_owned(),
        RawValue::from_string(serde_json::to_string(value).unwrap()).unwrap(),
    )])
}

/// Go `hex.EncodeToString`.
fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

struct Case {
    name: &'static str,
    value: Box<dyn Debug>,
    secrets: &'static [&'static str],
    /// Must still show in the `{:?}` output.
    visible: &'static [&'static str],
}

// parity: adapted: Rust prints through `{:?}` and `{:#?}` only; Go's %v %+v %#v %s %q %x verbs and slog collapse to those two, and the hex-form check runs on both outputs.
#[test]
fn test_models_keep_secrets_out_of_printing() {
    // The first six cases port the repr and str tests in adk-python's test_auth_credential.py.
    let tests = [
        Case {
            name: "API key",
            value: Box::new(Credential {
                auth_type: CredentialType::API_KEY,
                api_key: s("sk-live-secret-api-key-12345"),
                ..Default::default()
            }),
            secrets: &["sk-live-secret-api-key-12345"],
            visible: &["apiKey"],
        },
        Case {
            name: "HTTP credentials",
            value: Box::new(Credential {
                auth_type: CredentialType::HTTP,
                http: Some(HttpAuth {
                    scheme: s("basic"),
                    credentials: HttpCredentials {
                        username: s("my_user"),
                        password: s("secret_password_999"),
                        token: s("secret_token_abc"),
                        ..Default::default()
                    },
                    additional_headers: Some(BTreeMap::from([(
                        s("Authorization"),
                        s("Bearer secret_bearer_token"),
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            secrets: &[
                "secret_password_999",
                "secret_token_abc",
                "secret_bearer_token",
            ],
            visible: &["my_user", "Authorization"],
        },
        Case {
            name: "OAuth2 credentials",
            value: Box::new(Credential {
                auth_type: CredentialType::OAUTH2,
                oauth2: Some(OAuth2Auth {
                    client_id: s("my_client_id"),
                    client_secret: s("top_secret_client_secret"),
                    access_token: s("secret_access_token"),
                    refresh_token: s("secret_refresh_token"),
                    id_token: s("secret_id_token"),
                    auth_code: s("secret_auth_code"),
                    auth_response_uri: s("https://example.com/callback?code=secret_response_code"),
                    code_verifier: s("secret_code_verifier"),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            secrets: &[
                "top_secret_client_secret",
                "secret_access_token",
                "secret_refresh_token",
                "secret_id_token",
                "secret_auth_code",
                "secret_response_code",
                "secret_code_verifier",
            ],
            visible: &["my_client_id"],
        },
        Case {
            name: "service account key",
            value: Box::new(ServiceAccountCredential {
                r#type: s("service_account"),
                project_id: s("test_project"),
                private_key_id: s("secret_private_key_id"),
                private_key: s(
                    "-----BEGIN PRIVATE KEY-----\nsecret_key_data\n-----END PRIVATE KEY-----",
                ),
                client_email: s("test@iam.gserviceaccount.com"),
                client_id: s("12345"),
                auth_uri: s("https://example.com/o/oauth2/auth"),
                token_uri: s("https://example.com/token"),
                auth_provider_x509_cert_url: s("https://example.com/oauth2/v1/certs"),
                client_x509_cert_url: s("https://example.com/robot/v1/metadata/x509/test"),
                universe_domain: s("example.com"),
                ..Default::default()
            }),
            secrets: &["secret_key_data", "secret_private_key_id"],
            visible: &["test@iam.gserviceaccount.com"],
        },
        Case {
            name: "unknown key",
            value: Box::new(Credential {
                auth_type: CredentialType::API_KEY,
                extra: extra("undeclared_secret", "secret_extra_value"),
                ..Default::default()
            }),
            secrets: &["secret_extra_value"],
            visible: &["undeclared_secret"],
        },
        Case {
            name: "nested unknown key",
            value: Box::new(Credential {
                auth_type: CredentialType::OAUTH2,
                oauth2: Some(OAuth2Auth {
                    client_id: s("my_client_id"),
                    extra: extra("unexpected_token", "secret_unexpected_token"),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            secrets: &["secret_unexpected_token"],
            visible: &["my_client_id", "unexpected_token"],
        },
        Case {
            name: "credential request",
            value: Box::new(ToolArguments {
                function_call_id: s("call-1"),
                auth_config: Config {
                    auth_scheme: Scheme {
                        r#type: SchemeType::OAUTH2,
                        flows: Some(OAuthFlows::default()),
                        ..Default::default()
                    },
                    raw_auth_credential: Some(Credential {
                        auth_type: CredentialType::OAUTH2,
                        oauth2: Some(OAuth2Auth {
                            client_id: s("my_client_id"),
                            client_secret: s("top_secret_client_secret"),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    exchanged_auth_credential: Some(Credential {
                        auth_type: CredentialType::SERVICE_ACCOUNT,
                        service_account: Some(ServiceAccount {
                            service_account_credential: Some(ServiceAccountCredential {
                                private_key: s("secret_key_data"),
                                ..Default::default()
                            }),
                            extra: extra("x-sa", "secret_sa_extra"),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    extra: extra("x-config", "secret_config_extra"),
                    ..Default::default()
                },
                extra: extra("x-args", "secret_args_extra"),
            }),
            secrets: &[
                "top_secret_client_secret",
                "secret_key_data",
                "secret_sa_extra",
                "secret_config_extra",
                "secret_args_extra",
            ],
            visible: &["call-1", "my_client_id", "x-config", "x-args"],
        },
        // The cases below print each model type on its own.
        Case {
            name: "config",
            value: Box::new(Config {
                auth_scheme: Scheme {
                    r#type: SchemeType::API_KEY,
                    r#in: s("header"),
                    name: s("X-Api-Key"),
                    ..Default::default()
                },
                raw_auth_credential: Some(Credential {
                    auth_type: CredentialType::API_KEY,
                    api_key: s("sk-live-secret-api-key-12345"),
                    ..Default::default()
                }),
                extra: extra("x-config", "secret_config_extra"),
                ..Default::default()
            }),
            secrets: &["sk-live-secret-api-key-12345", "secret_config_extra"],
            visible: &["X-Api-Key", "x-config"],
        },
        Case {
            name: "OAuth2 credential",
            value: Box::new(OAuth2Auth {
                client_id: s("my_client_id"),
                client_secret: s("top_secret_client_secret"),
                extra: extra("x-oauth2", "secret_oauth2_extra"),
                ..Default::default()
            }),
            secrets: &["top_secret_client_secret", "secret_oauth2_extra"],
            visible: &["my_client_id", "x-oauth2"],
        },
        Case {
            name: "HTTP auth",
            value: Box::new(HttpAuth {
                scheme: s("basic"),
                credentials: HttpCredentials {
                    username: s("my_user"),
                    password: s("secret_password_999"),
                    ..Default::default()
                },
                additional_headers: Some(BTreeMap::from([(
                    s("X-Api-Key"),
                    s("secret_header_value"),
                )])),
                extra: extra("x-http", "secret_http_extra"),
            }),
            secrets: &[
                "secret_password_999",
                "secret_header_value",
                "secret_http_extra",
            ],
            visible: &["my_user", "X-Api-Key", "x-http"],
        },
        Case {
            name: "HTTP credentials",
            value: Box::new(HttpCredentials {
                username: s("my_user"),
                token: s("secret_token_abc"),
                extra: extra("x-cred", "secret_cred_extra"),
                ..Default::default()
            }),
            secrets: &["secret_token_abc", "secret_cred_extra"],
            visible: &["my_user", "x-cred"],
        },
        Case {
            name: "service account",
            value: Box::new(ServiceAccount {
                service_account_credential: Some(ServiceAccountCredential {
                    client_email: s("test@iam.gserviceaccount.com"),
                    private_key: s("secret_key_data"),
                    extra: extra("x-key", "secret_key_extra"),
                    ..Default::default()
                }),
                extra: extra("x-sa", "secret_sa_extra"),
                ..Default::default()
            }),
            secrets: &["secret_key_data", "secret_key_extra", "secret_sa_extra"],
            visible: &["test@iam.gserviceaccount.com", "x-key", "x-sa"],
        },
    ];
    for tc in tests {
        let outputs = [
            ("{:?}", format!("{:?}", tc.value)),
            ("{:#?}", format!("{:#?}", tc.value)),
        ];
        for (verb, out) in &outputs {
            for secret in tc.secrets {
                assert!(
                    !out.contains(secret) && !out.contains(&hex(secret)),
                    "{}: {verb} output contains the secret {secret:?}",
                    tc.name
                );
            }
        }
        for v in tc.visible {
            assert!(
                outputs[0].1.contains(v),
                "{}: {{:?}} output = {}, want it to show {v:?}",
                tc.name,
                outputs[0].1
            );
        }
    }
}

// parity: adapted: the expected string is the Rust Debug output of the same Config (type names Config, Scheme, Credential, with api_key: "<redacted>").
#[test]
fn test_go_syntax_printing_names_model_types() {
    let config = Config {
        auth_scheme: Scheme {
            r#type: SchemeType::API_KEY,
            r#in: s("header"),
            name: s("X"),
            ..Default::default()
        },
        raw_auth_credential: Some(Credential {
            auth_type: CredentialType::API_KEY,
            api_key: s("k"),
            ..Default::default()
        }),
        ..Default::default()
    };
    let got = format!("{config:?}");
    let want = concat!(
        r#"Config { auth_scheme: Scheme { type: SchemeType("apiKey"), description: "", in: "header", "#,
        r#"name: "X", scheme: "", bearer_format: "", flows: None, issuer_url: "", "#,
        r#"open_id_connect_url: "", authorization_endpoint: "", token_endpoint: "", "#,
        r#"userinfo_endpoint: "", revocation_endpoint: "", "#,
        r#"token_endpoint_auth_methods_supported: None, grant_types_supported: None, scopes: None, "#,
        r#"extra: {} }, "#,
        r#"raw_auth_credential: Some(Credential { auth_type: CredentialType("apiKey"), "#,
        r#"resource_ref: "", api_key: "<redacted>", http: None, service_account: None, oauth2: None, "#,
        r#"extra: {} }), "#,
        r#"exchanged_auth_credential: None, credential_key: "", extra: {} }"#,
    );
    assert_eq!(got, want, "{{:?}} output");
}

#[test]
fn test_printing_leaves_secrets_in_place() {
    // Only the printed text is redacted; the values stay readable, as in adk-python.
    let cred = Credential {
        auth_type: CredentialType::HTTP,
        http: Some(HttpAuth {
            scheme: s("basic"),
            credentials: HttpCredentials {
                password: s("p"),
                ..Default::default()
            },
            additional_headers: Some(BTreeMap::from([(s("X-Secret"), s("h"))])),
            ..Default::default()
        }),
        extra: extra("undeclared_secret", "v"),
        ..Default::default()
    };
    let _ = format!("{cred:?}");

    let http = cred.http.as_ref().unwrap();
    assert_eq!(http.credentials.password, "p", "password after printing");
    assert_eq!(
        http.additional_headers.as_ref().unwrap()["X-Secret"],
        "h",
        r#"additional_headers["X-Secret"] after printing"#
    );
    assert_eq!(
        cred.extra["undeclared_secret"].get(),
        r#""v""#,
        r#"extra["undeclared_secret"] after printing"#
    );
}
