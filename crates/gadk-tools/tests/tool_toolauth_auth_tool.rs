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

//! Port of Go tool/toolauth/auth_tool_test.go.

use gadk_tools::toolauth::{
    Config, Credential, CredentialType, Extra, OAuth2Auth, OAuthFlow, OAuthFlows,
    REQUEST_CREDENTIAL_FUNCTION_CALL_NAME, Scheme, SchemeType, ToolArguments,
};
use serde_json::value::RawValue;

// auth_schemes_test.go:29-30.
const TEST_TOKEN_URL: &str = "https://example.com/token";
const TEST_AUTH_URL: &str = "https://example.com/authorize";

/// One `Extra` value: `json` is the raw JSON text (Go builds these maps from Go values).
fn raw(json: &str) -> Box<RawValue> {
    RawValue::from_string(json.to_owned()).unwrap()
}

/// An `Extra` map from `(key, raw JSON text)` pairs.
fn extra(pairs: &[(&str, &str)]) -> Extra {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), raw(v)))
        .collect()
}

fn api_key_config() -> Config {
    Config {
        auth_scheme: Scheme {
            r#type: SchemeType::API_KEY,
            r#in: "header".into(),
            name: "X-Api-Key".into(),
            ..Default::default()
        },
        raw_auth_credential: Some(Credential {
            auth_type: CredentialType::API_KEY,
            api_key: "k".into(),
            ..Default::default()
        }),
        credential_key: "my_api_key".into(),
        ..Default::default()
    }
}

const API_KEY_CONFIG_JSON: &str = r#"{"authScheme":{"type":"apiKey","in":"header","name":"X-Api-Key"},"rawAuthCredential":{"authType":"apiKey","apiKey":"k"},"credentialKey":"my_api_key"}"#;

struct PythonConfigCase {
    name: &'static str,
    config: Config,
    json: &'static str,
}

/// Go `pythonConfigCases`: auth configs paired with the JSON adk-python 2.12.0 writes for the
/// same config, from model_dump(by_alias=True, exclude_none=True). Each has an explicit
/// credential key, which Python then writes as given.
fn python_config_cases() -> Vec<PythonConfigCase> {
    vec![
        PythonConfigCase {
            name: "API key",
            config: api_key_config(),
            json: API_KEY_CONFIG_JSON,
        },
        PythonConfigCase {
            name: "OAuth2 waiting for consent",
            config: Config {
                auth_scheme: Scheme {
                    r#type: SchemeType::OAUTH2,
                    flows: Some(OAuthFlows {
                        authorization_code: Some(OAuthFlow {
                            authorization_url: TEST_AUTH_URL.into(),
                            token_url: TEST_TOKEN_URL.into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                raw_auth_credential: Some(Credential {
                    auth_type: CredentialType::OAUTH2,
                    oauth2: Some(OAuth2Auth {
                        client_id: "cid".into(),
                        client_secret: "sec".into(),
                        token_endpoint_auth_method: "client_secret_basic".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                exchanged_auth_credential: Some(Credential {
                    auth_type: CredentialType::OAUTH2,
                    oauth2: Some(OAuth2Auth {
                        client_id: "cid".into(),
                        auth_uri: format!("{TEST_AUTH_URL}?state=s1"),
                        state: "s1".into(),
                        token_endpoint_auth_method: "client_secret_basic".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                credential_key: "my_oauth_key".into(),
                ..Default::default()
            },
            json: r#"{"authScheme":{"type":"oauth2","flows":{"authorizationCode":{"scopes":{},"authorizationUrl":"https://example.com/authorize","tokenUrl":"https://example.com/token"}}},"rawAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","clientSecret":"sec","tokenEndpointAuthMethod":"client_secret_basic"}},"exchangedAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","authUri":"https://example.com/authorize?state=s1","state":"s1","tokenEndpointAuthMethod":"client_secret_basic"}},"credentialKey":"my_oauth_key"}"#,
        },
        PythonConfigCase {
            name: "unknown key",
            config: Config {
                auth_scheme: Scheme {
                    r#type: SchemeType::API_KEY,
                    r#in: "header".into(),
                    name: "X-Api-Key".into(),
                    ..Default::default()
                },
                credential_key: "k1".into(),
                extra: extra(&[("x-note", r#""n""#)]),
                ..Default::default()
            },
            json: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X-Api-Key"},"credentialKey":"k1","x-note":"n"}"#,
        },
    ]
}

#[test]
fn test_config_unmarshal_python_json() {
    for tc in python_config_cases() {
        let got: Config = serde_json::from_str(tc.json)
            .unwrap_or_else(|e| panic!("{}: serde_json::from_str() error = {e}", tc.name));
        assert_eq!(
            tc.config, got,
            "{}: decode mismatch (left: want, right: got)",
            tc.name
        );
    }
}

#[test]
fn test_config_unmarshal_snake_case() {
    // adk-python 2.12.0 reads this as the same config as API_KEY_CONFIG_JSON.
    let data = r#"{"auth_scheme":{"type":"apiKey","in":"header","name":"X-Api-Key"},"raw_auth_credential":{"auth_type":"apiKey","api_key":"k"},"credential_key":"my_api_key"}"#;
    let got: Config = serde_json::from_str(data).unwrap();
    assert_eq!(api_key_config(), got);
}

#[test]
fn test_config_and_tool_arguments_unmarshal_rejects() {
    // adk-python 2.12.0 rejects every one of these.
    fn into_config(data: &str) -> Result<(), String> {
        serde_json::from_str::<Config>(data)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn into_tool_arguments(data: &str) -> Result<(), String> {
        serde_json::from_str::<ToolArguments>(data)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    struct Case {
        name: &'static str,
        data: &'static str,
        into: fn(&str) -> Result<(), String>,
        want_err: &'static str,
    }
    let tests = [
        Case {
            name: "config without authScheme",
            data: r#"{"credentialKey":"k"}"#,
            into: into_config,
            want_err: "missing required field authScheme",
        },
        Case {
            name: "config with null authScheme",
            data: r#"{"authScheme":null}"#,
            into: into_config,
            want_err: "missing required field authScheme",
        },
        Case {
            name: "arguments without functionCallId",
            data: r#"{"authConfig":{"authScheme":{"type":"apiKey","in":"header","name":"X"}}}"#,
            into: into_tool_arguments,
            want_err: "missing required field functionCallId",
        },
        Case {
            name: "arguments without authConfig",
            data: r#"{"functionCallId":"c"}"#,
            into: into_tool_arguments,
            want_err: "missing required field authConfig",
        },
    ];
    for tc in tests {
        let Err(err) = (tc.into)(tc.data) else {
            panic!("{}: decode succeeded, want an error", tc.name);
        };
        assert!(
            err.contains(tc.want_err),
            "{}: decode error = {err:?}, want it to contain {:?}",
            tc.name,
            tc.want_err
        );
    }
}

#[test]
fn test_request_credential_function_call_name() {
    // ADK Web and adk-python recognize the request by this exact name.
    assert_eq!(
        REQUEST_CREDENTIAL_FUNCTION_CALL_NAME, "adk_request_credential",
        "REQUEST_CREDENTIAL_FUNCTION_CALL_NAME = {REQUEST_CREDENTIAL_FUNCTION_CALL_NAME:?}, want {:?}",
        "adk_request_credential"
    );
}
