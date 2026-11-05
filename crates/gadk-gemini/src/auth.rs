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

//! Authentication headers for REST requests.

use gadk_model::ModelError;
use google_cloud_auth::credentials::CacheableResource;
use http::{HeaderMap, HeaderValue};

use crate::config::Auth;

/// The headers that authenticate a request: `x-goog-api-key` for an API key (as genai
/// `buildRequest`), or the credentials' headers (typically `authorization`).
pub(crate) async fn auth_headers(auth: &Option<Auth>) -> Result<HeaderMap, ModelError> {
    match auth {
        None => Ok(HeaderMap::new()),
        Some(Auth::ApiKey(key)) => {
            let mut headers = HeaderMap::new();
            let mut value =
                HeaderValue::from_str(key).map_err(|e| ModelError::Other(Box::new(e)))?;
            value.set_sensitive(true);
            headers.insert("x-goog-api-key", value);
            Ok(headers)
        }
        Some(Auth::Credentials(credentials)) => {
            match credentials
                .headers(http::Extensions::new())
                .await
                .map_err(|e| ModelError::Other(Box::new(e)))?
            {
                CacheableResource::New { data, .. } => Ok(data),
                // Only returned when the caller passes an entity tag, which we never do.
                CacheableResource::NotModified => {
                    Err(ModelError::Other("unexpected NotModified".into()))
                }
            }
        }
    }
}
