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

//! Port of Go `server/authn/header.go`.

use std::sync::Arc;

use super::{Authenticator, AuthnError, Caller};

struct Header {
    name: String,
}

/// Port of Go `authn.NewHeader`: the caller's user ID is the single, non-empty value of header
/// `name`.
pub fn header(name: &str) -> Arc<dyn Authenticator> {
    Arc::new(Header {
        name: name.to_owned(),
    })
}

#[async_trait::async_trait]
impl Authenticator for Header {
    async fn authenticate(&self, req: &http::request::Parts) -> Result<Option<Caller>, AuthnError> {
        let vals: Vec<_> = req.headers.get_all(self.name.as_str()).iter().collect();
        match vals.as_slice() {
            [] => Err(AuthnError::Unauthenticated(Some("header not found".into()))),
            [v] => {
                let user_id = String::from_utf8_lossy(v.as_bytes()).into_owned();
                if user_id.is_empty() {
                    return Err(AuthnError::Unauthenticated(Some(
                        "header found with empty value".into(),
                    )));
                }
                Ok(Some(Caller {
                    user_id,
                    claims: None,
                }))
            }
            _ => Err(AuthnError::Unauthenticated(Some(
                "header found with multiple values".into(),
            ))),
        }
    }
}
