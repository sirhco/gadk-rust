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

//! Port of ADK-Go `tool/toolauth/auth_credential_test.go`.

use std::collections::BTreeMap;

use gadk_tools::toolauth::{
    Credential, CredentialType, Extra, HttpAuth, HttpCredentials, OAuth2Auth, ServiceAccount,
    ServiceAccountCredential,
};

fn s(v: &str) -> String {
    v.to_owned()
}

/// Go `decodeJSON`: decodes for a comparison that ignores key order and spacing.
fn decode_json(data: &str) -> serde_json::Value {
    serde_json::from_str(data).unwrap_or_else(|e| panic!("decoding JSON: {e}"))
}

/// The raw JSON text of each extra, for comparing with Go's `map[string]any` (a Go string `"t"`
/// is the text `"\"t\""`, a `json.Number` is its digits).
fn texts(extra: &Extra) -> BTreeMap<&str, &str> {
    extra.iter().map(|(k, v)| (k.as_str(), v.get())).collect()
}

fn sa_key(key_type: &str) -> ServiceAccountCredential {
    ServiceAccountCredential {
        r#type: s(key_type),
        project_id: s("proj"),
        private_key_id: s("kid"),
        private_key: s("-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n"),
        client_email: s("sa@proj.iam.gserviceaccount.com"),
        client_id: s("123"),
        auth_uri: s("https://accounts.google.com/o/oauth2/auth"),
        token_uri: s("https://oauth2.googleapis.com/token"),
        auth_provider_x509_cert_url: s("https://www.googleapis.com/oauth2/v1/certs"),
        client_x509_cert_url: s("https://www.googleapis.com/robot/v1/metadata/x509/sa"),
        universe_domain: s("googleapis.com"),
        ..Default::default()
    }
}

struct PythonCase {
    name: &'static str,
    cred: Credential,
    camel: &'static str,
    snake: &'static str,
}

/// Go `pythonCases`: credentials with the JSON adk-python 2.12.0 writes for them, from
/// `model_dump(exclude_none=True)`: camel by alias, snake by field name.
fn python_cases() -> Vec<PythonCase> {
    vec![
        PythonCase {
            name: "API key",
            cred: Credential {
                auth_type: CredentialType::API_KEY,
                api_key: s("test-api-key"),
                ..Default::default()
            },
            camel: r#"{"authType":"apiKey","apiKey":"test-api-key"}"#,
            snake: r#"{"auth_type":"apiKey","api_key":"test-api-key"}"#,
        },
        PythonCase {
            name: "resource ref",
            cred: Credential {
                auth_type: CredentialType::API_KEY,
                resource_ref: s("projects/1/locations/us/resources/r"),
                ..Default::default()
            },
            camel: r#"{"authType":"apiKey","resourceRef":"projects/1/locations/us/resources/r"}"#,
            snake: r#"{"auth_type":"apiKey","resource_ref":"projects/1/locations/us/resources/r"}"#,
        },
        PythonCase {
            name: "HTTP bearer with no credentials",
            cred: Credential {
                auth_type: CredentialType::HTTP,
                http: Some(HttpAuth {
                    scheme: s("bearer"),
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"http","http":{"scheme":"bearer","credentials":{}}}"#,
            snake: r#"{"auth_type":"http","http":{"scheme":"bearer","credentials":{}}}"#,
        },
        PythonCase {
            name: "HTTP basic with a header",
            cred: Credential {
                auth_type: CredentialType::HTTP,
                http: Some(HttpAuth {
                    scheme: s("basic"),
                    credentials: HttpCredentials {
                        username: s("user"),
                        password: s("pass"),
                        ..Default::default()
                    },
                    additional_headers: Some(BTreeMap::from([(
                        s("X-Goog-User-Project"),
                        s("proj"),
                    )])),
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"http","http":{"scheme":"basic","credentials":{"username":"user","password":"pass"},"additionalHeaders":{"X-Goog-User-Project":"proj"}}}"#,
            snake: r#"{"auth_type":"http","http":{"scheme":"basic","credentials":{"username":"user","password":"pass"},"additional_headers":{"X-Goog-User-Project":"proj"}}}"#,
        },
        // An empty map is written, where a nil one is not.
        PythonCase {
            name: "HTTP basic with an empty header map",
            cred: Credential {
                auth_type: CredentialType::HTTP,
                http: Some(HttpAuth {
                    scheme: s("basic"),
                    additional_headers: Some(BTreeMap::new()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"http","http":{"scheme":"basic","credentials":{},"additionalHeaders":{}}}"#,
            snake: r#"{"auth_type":"http","http":{"scheme":"basic","credentials":{},"additional_headers":{}}}"#,
        },
        PythonCase {
            name: "OAuth2 client",
            cred: Credential {
                auth_type: CredentialType::OAUTH2,
                oauth2: Some(OAuth2Auth {
                    client_id: s("client"),
                    token_endpoint_auth_method: s("client_secret_basic"),
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"oauth2","oauth2":{"clientId":"client","tokenEndpointAuthMethod":"client_secret_basic"}}"#,
            snake: r#"{"auth_type":"oauth2","oauth2":{"client_id":"client","token_endpoint_auth_method":"client_secret_basic"}}"#,
        },
        PythonCase {
            name: "OAuth2 with every field",
            cred: Credential {
                auth_type: CredentialType::OAUTH2,
                oauth2: Some(OAuth2Auth {
                    client_id: s("client"),
                    client_secret: s("secret"),
                    auth_uri: s("https://example.com/auth"),
                    nonce: s("n"),
                    state: s("s"),
                    redirect_uri: s("https://example.com/cb"),
                    auth_response_uri: s("https://example.com/cb?code=c"),
                    auth_code: s("c"),
                    access_token: s("at"),
                    refresh_token: s("rt"),
                    id_token: s("it"),
                    expires_at: 1767225600,
                    expires_in: 3600,
                    audience: s("aud"),
                    prompt: s("consent"),
                    code_verifier: s("cv"),
                    code_challenge_method: s("S256"),
                    token_endpoint_auth_method: s("client_secret_post"),
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"oauth2","oauth2":{"clientId":"client","clientSecret":"secret","authUri":"https://example.com/auth","nonce":"n","state":"s","redirectUri":"https://example.com/cb","authResponseUri":"https://example.com/cb?code=c","authCode":"c","accessToken":"at","refreshToken":"rt","idToken":"it","expiresAt":1767225600,"expiresIn":3600,"audience":"aud","prompt":"consent","codeVerifier":"cv","codeChallengeMethod":"S256","tokenEndpointAuthMethod":"client_secret_post"}}"#,
            snake: r#"{"auth_type":"oauth2","oauth2":{"client_id":"client","client_secret":"secret","auth_uri":"https://example.com/auth","nonce":"n","state":"s","redirect_uri":"https://example.com/cb","auth_response_uri":"https://example.com/cb?code=c","auth_code":"c","access_token":"at","refresh_token":"rt","id_token":"it","expires_at":1767225600,"expires_in":3600,"audience":"aud","prompt":"consent","code_verifier":"cv","code_challenge_method":"S256","token_endpoint_auth_method":"client_secret_post"}}"#,
        },
        PythonCase {
            name: "OpenID Connect client",
            cred: Credential {
                auth_type: CredentialType::OPEN_ID_CONNECT,
                oauth2: Some(OAuth2Auth {
                    client_id: s("client"),
                    client_secret: s("secret"),
                    redirect_uri: s("https://example.com/cb"),
                    token_endpoint_auth_method: s("client_secret_basic"),
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"openIdConnect","oauth2":{"clientId":"client","clientSecret":"secret","redirectUri":"https://example.com/cb","tokenEndpointAuthMethod":"client_secret_basic"}}"#,
            snake: r#"{"auth_type":"openIdConnect","oauth2":{"client_id":"client","client_secret":"secret","redirect_uri":"https://example.com/cb","token_endpoint_auth_method":"client_secret_basic"}}"#,
        },
        PythonCase {
            name: "service account through ADC",
            cred: Credential {
                auth_type: CredentialType::SERVICE_ACCOUNT,
                service_account: Some(ServiceAccount {
                    scopes: Some(vec![s("https://www.googleapis.com/auth/cloud-platform")]),
                    use_default_credential: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"serviceAccount","serviceAccount":{"scopes":["https://www.googleapis.com/auth/cloud-platform"],"useDefaultCredential":true,"useIdToken":false}}"#,
            snake: r#"{"auth_type":"serviceAccount","service_account":{"scopes":["https://www.googleapis.com/auth/cloud-platform"],"use_default_credential":true,"use_id_token":false}}"#,
        },
        PythonCase {
            name: "service account through ADC with an empty scope list",
            cred: Credential {
                auth_type: CredentialType::SERVICE_ACCOUNT,
                service_account: Some(ServiceAccount {
                    scopes: Some(vec![]),
                    use_default_credential: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"serviceAccount","serviceAccount":{"scopes":[],"useDefaultCredential":true,"useIdToken":false}}"#,
            snake: r#"{"auth_type":"serviceAccount","service_account":{"scopes":[],"use_default_credential":true,"use_id_token":false}}"#,
        },
        PythonCase {
            name: "service account key for an ID token",
            cred: Credential {
                auth_type: CredentialType::SERVICE_ACCOUNT,
                service_account: Some(ServiceAccount {
                    service_account_credential: Some(sa_key("service_account")),
                    use_id_token: true,
                    audience: s("https://svc.run.app"),
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"serviceAccount","serviceAccount":{"serviceAccountCredential":{"type":"service_account","projectId":"proj","privateKeyId":"kid","privateKey":"-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n","clientEmail":"sa@proj.iam.gserviceaccount.com","clientId":"123","authUri":"https://accounts.google.com/o/oauth2/auth","tokenUri":"https://oauth2.googleapis.com/token","authProviderX509CertUrl":"https://www.googleapis.com/oauth2/v1/certs","clientX509CertUrl":"https://www.googleapis.com/robot/v1/metadata/x509/sa","universeDomain":"googleapis.com"},"useDefaultCredential":false,"useIdToken":true,"audience":"https://svc.run.app"}}"#,
            snake: r#"{"auth_type":"serviceAccount","service_account":{"service_account_credential":{"type_":"service_account","project_id":"proj","private_key_id":"kid","private_key":"-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n","client_email":"sa@proj.iam.gserviceaccount.com","client_id":"123","auth_uri":"https://accounts.google.com/o/oauth2/auth","token_uri":"https://oauth2.googleapis.com/token","auth_provider_x509_cert_url":"https://www.googleapis.com/oauth2/v1/certs","client_x509_cert_url":"https://www.googleapis.com/robot/v1/metadata/x509/sa","universe_domain":"googleapis.com"},"use_default_credential":false,"use_id_token":true,"audience":"https://svc.run.app"}}"#,
        },
        PythonCase {
            name: "service account key with no type",
            cred: Credential {
                auth_type: CredentialType::SERVICE_ACCOUNT,
                service_account: Some(ServiceAccount {
                    service_account_credential: Some(sa_key("")),
                    ..Default::default()
                }),
                ..Default::default()
            },
            camel: r#"{"authType":"serviceAccount","serviceAccount":{"serviceAccountCredential":{"type":"","projectId":"proj","privateKeyId":"kid","privateKey":"-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n","clientEmail":"sa@proj.iam.gserviceaccount.com","clientId":"123","authUri":"https://accounts.google.com/o/oauth2/auth","tokenUri":"https://oauth2.googleapis.com/token","authProviderX509CertUrl":"https://www.googleapis.com/oauth2/v1/certs","clientX509CertUrl":"https://www.googleapis.com/robot/v1/metadata/x509/sa","universeDomain":"googleapis.com"},"useDefaultCredential":false,"useIdToken":false}}"#,
            snake: r#"{"auth_type":"serviceAccount","service_account":{"service_account_credential":{"type_":"","project_id":"proj","private_key_id":"kid","private_key":"-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n","client_email":"sa@proj.iam.gserviceaccount.com","client_id":"123","auth_uri":"https://accounts.google.com/o/oauth2/auth","token_uri":"https://oauth2.googleapis.com/token","auth_provider_x509_cert_url":"https://www.googleapis.com/oauth2/v1/certs","client_x509_cert_url":"https://www.googleapis.com/robot/v1/metadata/x509/sa","universe_domain":"googleapis.com"},"use_default_credential":false,"use_id_token":false}}"#,
        },
    ]
}

#[test]
fn test_credential_marshal_matches_python() {
    for tc in python_cases() {
        let got = serde_json::to_string(&tc.cred)
            .unwrap_or_else(|e| panic!("{}: serde_json::to_string() error = {e}", tc.name));
        assert_eq!(
            decode_json(&got),
            decode_json(tc.camel),
            "{}: serde_json::to_string() differs from adk-python",
            tc.name
        );
    }
}

#[test]
fn test_credential_unmarshal_python_json() {
    for tc in python_cases() {
        for (form, data) in [("camel", tc.camel), ("snake", tc.snake)] {
            let got: Credential = serde_json::from_str(data).unwrap_or_else(|e| {
                panic!("{}/{form}: serde_json::from_str() error = {e}", tc.name)
            });
            assert!(
                got == tc.cred,
                "{}/{form}: serde_json::from_str() mismatch: got {got:?}, want {:?}",
                tc.name,
                tc.cred
            );
        }
    }
}

#[test]
fn test_o_auth2_auth_marshal_writes_default_token_endpoint_auth_method() {
    let got = serde_json::to_string(&OAuth2Auth {
        client_id: s("client"),
        ..Default::default()
    })
    .unwrap();
    // What adk-python 2.12.0 writes for OAuth2Auth(client_id="client").
    let want = r#"{"clientId":"client","tokenEndpointAuthMethod":"client_secret_basic"}"#;
    assert_eq!(
        decode_json(&got),
        decode_json(want),
        "serde_json::to_string() differs from adk-python"
    );
}

#[test]
fn test_credential_unmarshal_prefers_camel_case() {
    // adk-python 2.12.0 reads the alias when an object has both names. Every field here comes
    // under both.
    let data = concat!(
        r#"{"authType":"oauth2","auth_type":"apiKey","oauth2":{"#,
        r#""clientId":"camel","client_id":"snake","clientSecret":"camel","client_secret":"snake","#,
        r#""authUri":"camel","auth_uri":"snake","redirectUri":"camel","redirect_uri":"snake","#,
        r#""authResponseUri":"camel","auth_response_uri":"snake","authCode":"camel","auth_code":"snake","#,
        r#""accessToken":"camel","access_token":"snake","refreshToken":"camel","refresh_token":"snake","#,
        r#""idToken":"camel","id_token":"snake","codeVerifier":"camel","code_verifier":"snake","#,
        r#""codeChallengeMethod":"camel","code_challenge_method":"snake","#,
        r#""expiresAt":1,"expires_at":2,"expiresIn":1,"expires_in":2,"#,
        r#""tokenEndpointAuthMethod":"client_secret_post","token_endpoint_auth_method":"client_secret_jwt"}}"#,
    );
    let got: Credential = serde_json::from_str(data).unwrap();
    // Extra stays empty: the snake_case keys name fields, so they are dropped.
    let want = Credential {
        auth_type: CredentialType::OAUTH2,
        oauth2: Some(OAuth2Auth {
            client_id: s("camel"),
            client_secret: s("camel"),
            auth_uri: s("camel"),
            redirect_uri: s("camel"),
            auth_response_uri: s("camel"),
            auth_code: s("camel"),
            access_token: s("camel"),
            refresh_token: s("camel"),
            id_token: s("camel"),
            code_verifier: s("camel"),
            code_challenge_method: s("camel"),
            expires_at: 1,
            expires_in: 1,
            token_endpoint_auth_method: s("client_secret_post"),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(
        got == want,
        "serde_json::from_str() mismatch: got {got:?}, want {want:?}"
    );
}

#[test]
fn test_service_account_credential_reads_key_file() {
    let key_file = r#"{
  "type": "service_account",
  "project_id": "proj",
  "private_key_id": "kid",
  "private_key": "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n",
  "client_email": "sa@proj.iam.gserviceaccount.com",
  "client_id": "123",
  "auth_uri": "https://accounts.google.com/o/oauth2/auth",
  "token_uri": "https://oauth2.googleapis.com/token",
  "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
  "client_x509_cert_url": "https://www.googleapis.com/robot/v1/metadata/x509/sa",
  "universe_domain": "googleapis.com"
}"#;
    let got: ServiceAccountCredential = serde_json::from_str(key_file).unwrap();
    let want = sa_key("service_account");
    assert!(
        got == want,
        "serde_json::from_str() mismatch: got {got:?}, want {want:?}"
    );
}

#[test]
fn test_credential_keeps_unknown_keys() {
    // The input, and the output adk-python 2.12.0 writes for it.
    let input = r#"{"authType":"oauth2","oauth2":{"clientId":"c","scopes":["a"],"rapt_token":"r"},"tenantId":"t","count":12345678901234567890}"#;
    let want = r#"{"authType":"oauth2","oauth2":{"clientId":"c","tokenEndpointAuthMethod":"client_secret_basic","scopes":["a"],"rapt_token":"r"},"tenantId":"t","count":12345678901234567890}"#;
    let cred: Credential = serde_json::from_str(input).unwrap();
    assert_eq!(
        texts(&cred.extra),
        BTreeMap::from([("tenantId", r#""t""#), ("count", "12345678901234567890")]),
        "Extra mismatch"
    );
    let oauth2 = cred
        .oauth2
        .as_ref()
        .expect("oauth2 = None, want the decoded credential");
    assert_eq!(
        texts(&oauth2.extra),
        BTreeMap::from([("scopes", r#"["a"]"#), ("rapt_token", r#""r""#)]),
        "OAuth2 extra mismatch"
    );
    let got = serde_json::to_string(&cred).unwrap();
    assert_eq!(
        decode_json(&got),
        decode_json(want),
        "serde_json::to_string() differs from adk-python"
    );
}

#[test]
fn test_credential_marshal_skips_extra_keys_that_name_fields() {
    let raw = |t: &str| serde_json::value::RawValue::from_string(t.to_owned()).unwrap();
    let cred = Credential {
        auth_type: CredentialType::API_KEY,
        api_key: s("k"),
        extra: Extra::from([
            (s("apiKey"), raw(r#""x""#)),
            (s("api_key"), raw(r#""y""#)),
            (s("tenantId"), raw(r#""t""#)),
        ]),
        ..Default::default()
    };
    let got = serde_json::to_string(&cred).unwrap();
    // Compared as bytes, because decoding would hide a key written twice.
    assert_eq!(got, r#"{"authType":"apiKey","apiKey":"k","tenantId":"t"}"#);
}

#[test]
fn test_credential_unmarshal_rejects() {
    // adk-python 2.12.0 rejects every one of these. The fourth column, when not empty, is a
    // secret that must not appear in the error.
    let tests = [
        (
            "unknown authType",
            r#"{"authType":"api_key"}"#,
            "unknown authType",
            "",
        ),
        ("no authType", "{}", "missing required field authType", ""),
        (
            "null authType",
            r#"{"authType":null}"#,
            "missing required field authType",
            "",
        ),
        (
            "HTTP without credentials",
            r#"{"authType":"http","http":{"scheme":"basic"}}"#,
            "missing required field credentials",
            "",
        ),
        (
            "HTTP with null credentials",
            r#"{"authType":"http","http":{"scheme":"basic","credentials":null}}"#,
            "missing required field credentials",
            "",
        ),
        (
            "HTTP without scheme",
            r#"{"authType":"http","http":{"credentials":{}}}"#,
            "missing required field scheme",
            "",
        ),
        (
            "unsupported tokenEndpointAuthMethod",
            r#"{"authType":"oauth2","oauth2":{"tokenEndpointAuthMethod":"tls_client_auth"}}"#,
            "unsupported tokenEndpointAuthMethod",
            "",
        ),
        // adk-python uses none only in the token request it builds for a client without a
        // secret, never in the model.
        (
            "none as tokenEndpointAuthMethod",
            r#"{"authType":"oauth2","oauth2":{"clientId":"c","tokenEndpointAuthMethod":"none"}}"#,
            "unsupported tokenEndpointAuthMethod",
            "",
        ),
        (
            "service account with neither a key nor ADC",
            r#"{"authType":"serviceAccount","serviceAccount":{}}"#,
            "serviceAccountCredential is required",
            "",
        ),
        (
            "ID token without an audience",
            r#"{"authType":"serviceAccount","serviceAccount":{"useDefaultCredential":true,"useIdToken":true}}"#,
            "audience is required",
            "",
        ),
        (
            "key with missing fields",
            r#"{"authType":"serviceAccount","serviceAccount":{"serviceAccountCredential":{"projectId":"p"}}}"#,
            "missing required field privateKeyId",
            "",
        ),
        (
            "API key as a list",
            r#"{"authType":"apiKey","apiKey":["sk-live-secret-api-key-12345"]}"#,
            "field apiKey",
            "sk-live-secret-api-key-12345",
        ),
        (
            "API key as a number",
            r#"{"authType":"apiKey","apiKey":12345678901}"#,
            "field apiKey",
            "12345678901",
        ),
        // encoding/json quotes a number in full only when it overflows an integer field.
        (
            "number too large for expiresAt",
            r#"{"authType":"oauth2","oauth2":{"expiresAt":98765432109876543210}}"#,
            "field expiresAt",
            "98765432109876543210",
        ),
        (
            "credential that is not an object",
            r#"{"authType":"oauth2","oauth2":"secret-string"}"#,
            "expected a JSON object",
            "secret-string",
        ),
    ];
    for (name, data, want_err, secret) in tests {
        let Err(err) = serde_json::from_str::<Credential>(data) else {
            panic!("{name}: serde_json::from_str() succeeded, want an error");
        };
        let msg = err.to_string();
        assert!(
            secret.is_empty() || !msg.contains(secret),
            "{name}: serde_json::from_str() error quotes the rejected value"
        );
        assert!(
            msg.contains(want_err),
            "{name}: serde_json::from_str() error = {msg:?}, want it to contain {want_err:?}"
        );
    }
}
