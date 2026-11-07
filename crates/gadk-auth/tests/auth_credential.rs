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

//! Port of Go `auth/credential_test.go` (package `auth_test`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_auth::{
    ApiKey, Basic, Bearer, Credential, Error, OAuth2, StaticTokenSource, Token, TokenSource,
    with_headers,
};
use http::{HeaderMap, HeaderName, HeaderValue};
use pretty_assertions::assert_eq;

// Compile-time assertions that the built-in credentials implement `Credential`.
const _: fn() = || {
    fn is_credential<T: Credential>() {}
    is_credential::<ApiKey>();
    is_credential::<Bearer>();
    is_credential::<Basic>();
    is_credential::<OAuth2>();
};

/// Go `http.Header{...}` literal.
fn header(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(
            HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    h
}

fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn static_source(access_token: &str, token_type: &str) -> Arc<dyn TokenSource> {
    Arc::new(StaticTokenSource(Token {
        access_token: access_token.into(),
        token_type: token_type.into(),
    }))
}

// parity: adapted: the "oauth2 missing token source" and "with headers nil inner" cases cannot be built, because OAuth2::token_source and the with_headers inner credential are non-optional Arcs
#[tokio::test]
async fn test_credential_apply() {
    struct Case {
        name: &'static str,
        cred: Arc<dyn Credential>,
        /// `None` is Go's `wantErr: true`.
        want: Option<HeaderMap>,
    }
    let tests = vec![
        Case {
            name: "api key",
            cred: Arc::new(ApiKey {
                name: "X-Api-Key".into(),
                value: "secret".into(),
            }),
            want: Some(header(&[("X-Api-Key", "secret")])),
        },
        Case {
            name: "bearer",
            cred: Arc::new(Bearer {
                token: "abc".into(),
            }),
            want: Some(header(&[("Authorization", "Bearer abc")])),
        },
        Case {
            name: "basic",
            cred: Arc::new(Basic {
                username: "u".into(),
                password: "p".into(),
            }),
            // base64("u:p") == "dTpw"
            want: Some(header(&[("Authorization", "Basic dTpw")])),
        },
        Case {
            name: "bearer with additional headers",
            cred: with_headers(
                Arc::new(Bearer {
                    token: "abc".into(),
                }),
                headers(&[("X-Extra", "1")]),
            ),
            want: Some(header(&[("Authorization", "Bearer abc"), ("X-Extra", "1")])),
        },
        Case {
            name: "with headers overrides inner on conflict",
            cred: with_headers(
                Arc::new(Bearer {
                    token: "abc".into(),
                }),
                headers(&[("Authorization", "Bearer override")]),
            ),
            want: Some(header(&[("Authorization", "Bearer override")])),
        },
        Case {
            name: "oauth2 static access token",
            cred: Arc::new(OAuth2 {
                token_source: static_source("tok", "Bearer"),
            }),
            want: Some(header(&[("Authorization", "Bearer tok")])),
        },
        Case {
            name: "oauth2 token source",
            cred: Arc::new(OAuth2 {
                token_source: static_source("fresh", "Bearer"),
            }),
            want: Some(header(&[("Authorization", "Bearer fresh")])),
        },
        Case {
            name: "api key missing name",
            cred: Arc::new(ApiKey {
                name: String::new(),
                value: "y".into(),
            }),
            want: None,
        },
        Case {
            name: "bearer missing token",
            cred: Arc::new(Bearer::default()),
            want: None,
        },
        Case {
            name: "basic missing username and password",
            cred: Arc::new(Basic::default()),
            want: None,
        },
    ];
    for tt in tests {
        let mut h = HeaderMap::new();
        let res = tt.cred.apply(&mut h).await;
        match tt.want {
            None => {
                assert!(res.is_err(), "{}: Apply() = nil error, want error", tt.name);
            }
            Some(want) => {
                if let Err(err) = res {
                    panic!("{}: Apply() error = {err}, want nil", tt.name);
                }
                assert_eq!(want, h, "{}: header mismatch (-want +got)", tt.name);
            }
        }
    }
}

struct ErrTokenSource;

#[async_trait::async_trait]
impl TokenSource for ErrTokenSource {
    async fn token(&self) -> Result<Token, Error> {
        Err(Error::msg("token source failure"))
    }
}

#[tokio::test]
async fn test_o_auth2_token_source_error() {
    let cred = OAuth2 {
        token_source: Arc::new(ErrTokenSource),
    };
    let res = cred.apply(&mut HeaderMap::new()).await;
    assert!(
        res.is_err(),
        "Apply() = nil error, want error from failing token source"
    );
}

// auth/gcp reads the token out of a wrapped credential through inner(), so the promise in
// with_headers' doc is pinned here, in the crate that makes it.
#[test]
fn test_with_headers_unwraps_to_inner() {
    let inner: Arc<dyn Credential> = Arc::new(Bearer {
        token: "abc".into(),
    });
    let wrapped = with_headers(inner.clone(), headers(&[("X-Extra", "1")]));
    let Some(got) = wrapped.inner() else {
        panic!("with_headers() result has no inner() credential");
    };
    assert!(
        std::ptr::addr_eq(got, Arc::as_ptr(&inner)),
        "inner() = {got:?}, want {inner:?}"
    );
    assert_eq!(
        got.downcast_ref::<Bearer>(),
        Some(&Bearer {
            token: "abc".into()
        })
    );
}
