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

/// Go `decodeJSON` (auth_credential_test.go:195): the JSON as a tree, so two encodings compare
/// regardless of key order. Every number in these vectors fits serde_json's `Number`.
fn decode_json(data: &str) -> serde_json::Value {
    serde_json::from_str(data).unwrap_or_else(|e| panic!("decoding JSON: {e}"))
}

#[test]
fn test_config_marshal_matches_python() {
    for tc in python_config_cases() {
        let got = serde_json::to_string(&tc.config)
            .unwrap_or_else(|e| panic!("{}: serde_json::to_string() error = {e}", tc.name));
        assert_eq!(
            decode_json(tc.json),
            decode_json(&got),
            "{}: encoding differs from adk-python (left: want, right: got)",
            tc.name
        );
    }
}

#[test]
fn test_tool_arguments_matches_python() {
    let args = ToolArguments {
        function_call_id: "call-1".into(),
        auth_config: api_key_config(),
        ..Default::default()
    };
    // What adk-python 2.12.0 writes for the same arguments.
    let want = format!(r#"{{"functionCallId":"call-1","authConfig":{API_KEY_CONFIG_JSON}}}"#);

    let got = serde_json::to_string(&args).unwrap();
    assert_eq!(
        decode_json(&want),
        decode_json(&got),
        "encoding differs from adk-python (left: want, right: got)"
    );

    let snake_case = concat!(
        r#"{"function_call_id":"call-1","auth_config":{"auth_scheme":{"type":"apiKey","in":"header","name":"X-Api-Key"},"#,
        r#""raw_auth_credential":{"auth_type":"apiKey","api_key":"k"},"credential_key":"my_api_key"}}"#,
    );
    for (name, data) in [("camelCase", want.as_str()), ("snake_case", snake_case)] {
        let decoded: ToolArguments = serde_json::from_str(data)
            .unwrap_or_else(|e| panic!("serde_json::from_str({name}) error = {e}"));
        assert_eq!(
            args, decoded,
            "decode({name}) mismatch (left: want, right: got)"
        );
    }
}

#[test]
fn test_config_key_matches_python() {
    // Each want is the credential_key adk-python 2.12.0 gives the same config after
    // AuthConfig.model_validate. The authorization URL carries & and the non-ASCII case carries
    // é and ’, which Python's canonical JSON writes unescaped.
    struct Case {
        name: &'static str,
        data: &'static str,
        want: &'static str,
    }
    let tests = [
        Case {
            name: "API key scheme, no credential",
            data: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X-Api-Key"}}"#,
            want: "adk_apiKey_12037b6b68d3604d_",
        },
        Case {
            name: "API key",
            data: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X-Api-Key"},"rawAuthCredential":{"authType":"apiKey","apiKey":"k"}}"#,
            want: "adk_apiKey_12037b6b68d3604d_apiKey_39fdaa2147b3f2d1",
        },
        Case {
            name: "HTTP bearer",
            data: r#"{"authScheme":{"type":"http","scheme":"bearer"},"rawAuthCredential":{"authType":"http","http":{"scheme":"bearer","credentials":{"token":"t"}}}}"#,
            want: "adk_http_178204f758fb78d5_http_26adec9b11eb83bc",
        },
        Case {
            // The flow fields in the credential are left out of the key.
            name: "OAuth2 midway through a flow",
            data: concat!(
                r#"{"authScheme":{"type":"oauth2","flows":{"authorizationCode":{"authorizationUrl":"https://example.com/authorize?audience=api&prompt=consent","tokenUrl":"https://example.com/token","scopes":{"read":"Read"}}}},"#,
                r#""rawAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","clientSecret":"sec","redirectUri":"https://app/cb","authUri":"https://x","state":"s","accessToken":"at","expiresAt":5}}}"#,
            ),
            want: "adk_oauth2_8e6640e02b286ea7_oauth2_0833ddacfe0bd302",
        },
        Case {
            // Ported from test_credential_key_is_stable_across_redirect_uri: the same key as the
            // case above, with another redirect URI and the flow fields that case leaves unset.
            name: "OAuth2 with another redirect URI",
            data: concat!(
                r#"{"authScheme":{"type":"oauth2","flows":{"authorizationCode":{"authorizationUrl":"https://example.com/authorize?audience=api&prompt=consent","tokenUrl":"https://example.com/token","scopes":{"read":"Read"}}}},"#,
                r#""rawAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","clientSecret":"sec","redirectUri":"https://other/cb","#,
                r#""authResponseUri":"https://other/cb?code=c","authCode":"c","refreshToken":"rt","expiresIn":3600}}}"#,
            ),
            want: "adk_oauth2_8e6640e02b286ea7_oauth2_0833ddacfe0bd302",
        },
        Case {
            // These OAuth2 fields are not flow fields, so they count.
            name: "OAuth2 with an ID token, nonce and code verifier",
            data: concat!(
                r#"{"authScheme":{"type":"oauth2","flows":{"authorizationCode":{"authorizationUrl":"https://example.com/authorize?audience=api&prompt=consent","tokenUrl":"https://example.com/token","scopes":{"read":"Read"}}}},"#,
                r#""rawAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","clientSecret":"sec","idToken":"it","nonce":"n","codeVerifier":"cv"}}}"#,
            ),
            want: "adk_oauth2_8e6640e02b286ea7_oauth2_466def9e19464a97",
        },
        Case {
            // The canonical JSON writes the number as it arrived; as a float64 it would lose
            // digits.
            name: "OAuth2 with a large number in a nested unknown key",
            data: concat!(
                r#"{"authScheme":{"type":"oauth2","flows":{"authorizationCode":{"authorizationUrl":"https://example.com/authorize?audience=api&prompt=consent","tokenUrl":"https://example.com/token","scopes":{"read":"Read"}}}},"#,
                r#""rawAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","x-count":12345678901234567890}}}"#,
            ),
            want: "adk_oauth2_8e6640e02b286ea7_oauth2_1dc65c3746ce454a",
        },
        Case {
            // A nested unknown key counts towards the key, and a top-level one does not.
            name: "OAuth2 with unknown keys",
            data: concat!(
                r#"{"authScheme":{"type":"oauth2","flows":{"authorizationCode":{"authorizationUrl":"https://example.com/authorize?audience=api&prompt=consent","tokenUrl":"https://example.com/token"}}},"#,
                r#""rawAuthCredential":{"authType":"oauth2","oauth2":{"clientId":"cid","scopes":["a"]},"x-top":1}}"#,
            ),
            want: "adk_oauth2_6a7110d552770734_oauth2_589cf589f818b95c",
        },
        Case {
            name: "OpenID Connect endpoints",
            data: concat!(
                r#"{"authScheme":{"type":"openIdConnect","authorization_endpoint":"https://example.com/authorize?audience=api&prompt=consent","token_endpoint":"https://example.com/token","scopes":["openid"]},"#,
                r#""rawAuthCredential":{"authType":"openIdConnect","oauth2":{"clientId":"cid","clientSecret":"sec"}}}"#,
            ),
            want: "adk_openIdConnect_4450fdfd7924efbf_openIdConnect_d62d4f4b945e72e7",
        },
        Case {
            name: "OpenID Connect discovery URL",
            data: r#"{"authScheme":{"type":"openIdConnect","openIdConnectUrl":"https://example.com/.well-known/openid-configuration"}}"#,
            want: "adk_openIdConnect_480372faebed744f_",
        },
        Case {
            name: "service account",
            data: r#"{"authScheme":{"type":"http","scheme":"bearer"},"rawAuthCredential":{"authType":"serviceAccount","serviceAccount":{"useDefaultCredential":true,"scopes":["s"]}}}"#,
            want: "adk_http_178204f758fb78d5_serviceAccount_739833674b9d134f",
        },
        Case {
            // An empty list or map counts towards the key, unlike a missing one.
            name: "service account with an empty scope list",
            data: r#"{"authScheme":{"type":"http","scheme":"bearer"},"rawAuthCredential":{"authType":"serviceAccount","serviceAccount":{"useDefaultCredential":true,"scopes":[]}}}"#,
            want: "adk_http_178204f758fb78d5_serviceAccount_50785dccd95b1002",
        },
        Case {
            name: "HTTP bearer with an empty header map",
            data: r#"{"authScheme":{"type":"http","scheme":"bearer"},"rawAuthCredential":{"authType":"http","http":{"scheme":"bearer","credentials":{"token":"t"},"additionalHeaders":{}}}}"#,
            want: "adk_http_178204f758fb78d5_http_b4147d4fceb4365c",
        },
        Case {
            name: "OpenID Connect endpoints with an empty scope list",
            data: r#"{"authScheme":{"type":"openIdConnect","authorization_endpoint":"https://example.com/authorize","token_endpoint":"https://example.com/token","scopes":[]}}"#,
            want: "adk_openIdConnect_591dd13c93a8e50b_",
        },
        Case {
            name: "non-ASCII",
            data: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X-Clé","description":"Clé d’API"}}"#,
            want: "adk_apiKey_95c9f80bb80acb35_",
        },
        Case {
            // Ported from test_credential_key_with_custom_auth_scheme.
            name: "custom scheme",
            data: r#"{"authScheme":{"type":"mock_custom_type"}}"#,
            want: "adk_mock_custom_type_0be4d0a15c0e9488_",
        },
        Case {
            // Ported from test_get_credential_key_with_extras: unknown keys on the scheme leave
            // the key unchanged.
            name: "custom scheme with an unknown key",
            data: r#"{"authScheme":{"type":"mock_custom_type","x-provider":"acme"}}"#,
            want: "adk_mock_custom_type_0be4d0a15c0e9488_",
        },
        Case {
            name: "key among the credential's unknown keys",
            data: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X","credential_key":"from_scheme"},"rawAuthCredential":{"authType":"apiKey","credentialKey":"from_cred"}}"#,
            want: "from_cred",
        },
        Case {
            name: "empty key among the credential's unknown keys",
            data: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X-Api-Key"},"rawAuthCredential":{"authType":"apiKey","apiKey":"k","credential_key":""}}"#,
            want: "adk_apiKey_12037b6b68d3604d_apiKey_39fdaa2147b3f2d1",
        },
        Case {
            name: "key among the scheme's unknown keys",
            data: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X","credential_key":"from_scheme"}}"#,
            want: "from_scheme",
        },
        Case {
            // Ported from test_custom_credential_key.
            name: "explicit key",
            data: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X","credential_key":"from_scheme"},"credentialKey":"explicit"}"#,
            want: "explicit",
        },
    ];
    for tc in tests {
        let config: Config = serde_json::from_str(tc.data)
            .unwrap_or_else(|e| panic!("{}: serde_json::from_str() error = {e}", tc.name));
        let got = config.key();
        assert_eq!(
            got, tc.want,
            "{}: key() = {got:?}, want {:?}",
            tc.name, tc.want
        );
    }
}

#[test]
fn test_config_key_matches_python_extended_o_auth2() {
    let config = Config {
        auth_scheme: Scheme {
            r#type: SchemeType::OAUTH2,
            flows: Some(OAuthFlows {
                authorization_code: Some(OAuthFlow {
                    authorization_url: "https://example.com/authorize?audience=api&prompt=consent"
                        .into(),
                    token_url: TEST_TOKEN_URL.into(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            issuer_url: "https://issuer".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let got = config.key();
    // The key adk-python 2.12.0 gives an AuthConfig built from the same ExtendedOAuth2 scheme.
    let want = "adk_oauth2_0154a991a741d11d_";
    assert_eq!(got, want, "key() = {got:?}, want {want:?}");
}

#[test]
fn test_config_key_counts_scheme_fields_of_custom_scheme() {
    // Name is a field of Scheme, so it counts for a custom type too.
    let first = Config {
        auth_scheme: Scheme {
            r#type: SchemeType::from("gcp_provider"),
            name: "projects/p/locations/l/authProviders/a".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let second = Config {
        auth_scheme: Scheme {
            r#type: SchemeType::from("gcp_provider"),
            name: "projects/p/locations/l/authProviders/b".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let first_key = first.key();
    let second_key = second.key();
    assert_ne!(
        first_key, second_key,
        "key() = {first_key:?} for both schemes, want different keys"
    );
}

// parity: skipped TestConfigKeyFailsOnUnencodableExtra: extra holds only already-valid JSON text, so no unencodable value (Go's func(){} in an Extra map) can exist and key() is infallible (spec R12)

#[test]
fn test_config_marshal_writes_key() {
    // Neither config sets credential_key. Each want is what adk-python 2.12.0 writes for the
    // same config: the key it derived, or the one it found among the credential's unknown
    // keys, which also stays where it was.
    struct Case {
        name: &'static str,
        config: Config,
        want: &'static str,
    }
    let tests = [
        Case {
            name: "derived key",
            config: Config {
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
                ..Default::default()
            },
            want: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X-Api-Key"},"rawAuthCredential":{"authType":"apiKey","apiKey":"k"},"credentialKey":"adk_apiKey_12037b6b68d3604d_apiKey_39fdaa2147b3f2d1"}"#,
        },
        Case {
            name: "key among the credential's unknown keys",
            config: Config {
                auth_scheme: Scheme {
                    r#type: SchemeType::API_KEY,
                    r#in: "header".into(),
                    name: "X".into(),
                    ..Default::default()
                },
                raw_auth_credential: Some(Credential {
                    auth_type: CredentialType::API_KEY,
                    extra: extra(&[("credentialKey", r#""from_cred""#)]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            want: r#"{"authScheme":{"type":"apiKey","in":"header","name":"X"},"rawAuthCredential":{"authType":"apiKey","credentialKey":"from_cred"},"credentialKey":"from_cred"}"#,
        },
    ];
    for tc in tests {
        let got = serde_json::to_string(&tc.config)
            .unwrap_or_else(|e| panic!("{}: serde_json::to_string() error = {e}", tc.name));
        assert_eq!(
            decode_json(tc.want),
            decode_json(&got),
            "{}: encoding differs from adk-python (left: want, right: got)",
            tc.name
        );
    }
}
