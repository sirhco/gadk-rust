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
//! [`RestRequester`], the registry's GET transport. The client holds the REST requester
//! directly; there is no trait seam for a non-REST transport.

use std::sync::Arc;

use bytes::Bytes;
use gadk_auth::{AuthContext, CredentialProvider};
use gadk_platform::go_query_escape;
use http::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;

use crate::error::{ApiError, Error};

/// Issues authenticated GETs to the public Agent Registry REST endpoint.
#[derive(Clone)]
pub(crate) struct RestRequester {
    /// The caller's client, or a default one on the ADC path.
    pub(crate) http: reqwest::Client,
    /// The ADC credential provider (cloud-platform scope) on the ADC path; `None` for a
    /// caller-supplied client. Its credential's headers are applied to every request.
    pub(crate) auth: Option<Arc<dyn CredentialProvider>>,
    /// Base URL, e.g. `https://agentregistry.googleapis.com/v1`.
    pub(crate) base_url: String,
    /// Base path: `projects/<project>/locations/<location>`.
    pub(crate) base_path: String,
    /// The `x-goog-user-project` value; empty sends none.
    pub(crate) user_project: String,
}

impl RestRequester {
    /// GETs `resource_path` (absolute when it starts with `projects/`, else under `base_path`)
    /// with `params` as the query (keys sorted, query-escaped; no `?` when empty). Applies the
    /// credential's headers (ADC path), then sets `Content-Type: application/json` and, when
    /// `user_project` is non-empty, `x-goog-user-project` over them (one value each). A non-2xx
    /// status is [`Error::Api`]; the body of a 2xx answer is returned undecoded.
    pub(crate) async fn get(
        &self,
        resource_path: &str,
        params: &[(String, String)],
    ) -> Result<Bytes, Error> {
        let full_url = if resource_path.starts_with("projects/") {
            format!("{}/{}", self.base_url, resource_path)
        } else {
            format!("{}/{}/{}", self.base_url, self.base_path, resource_path)
        };
        // `Url::parse` (WHATWG) normalises `.` / `..` path segments and accepts an invalid escape
        // such as `%zz`, so `projects/p/../x` is not sent as written.
        let mut url =
            reqwest::Url::parse(&full_url).map_err(|e| Error::BuildRequest(e.to_string()))?;
        if !params.is_empty() {
            let query = params
                .iter()
                .map(|(k, v)| format!("{}={}", go_query_escape(k), go_query_escape(v)))
                .collect::<Vec<_>>()
                .join("&");
            url.set_query(Some(&query));
        }

        let http_err = |message: String| Error::Http {
            path: resource_path.to_owned(),
            message,
        };
        let mut headers = HeaderMap::new();
        if let Some(auth) = &self.auth {
            gadk_auth::apply_credential(auth.as_ref(), &AuthContext::anonymous(), &mut headers)
                .await
                .map_err(|e| http_err(e.to_string()))?;
        }
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if !self.user_project.is_empty() {
            // A value with control characters is rejected when the request is sent.
            let value = HeaderValue::from_bytes(self.user_project.as_bytes()).map_err(|_| {
                http_err(
                    "net/http: invalid header field value for \"X-Goog-User-Project\"".to_owned(),
                )
            })?;
            headers.insert(HeaderName::from_static("x-goog-user-project"), value);
        }

        let resp = self
            .http
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| http_err(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| Error::ReadBody(e.to_string()))?;

        if !(200..300).contains(&status) {
            return Err(Error::Api(ApiError {
                status_code: status,
                body: String::from_utf8_lossy(&body).into_owned(),
            }));
        }
        Ok(body)
    }

    /// [`RestRequester::get`] then a JSON decode into `T`
    /// ([`Error::Decode`] on failure).
    pub(crate) async fn get_json<T: DeserializeOwned>(
        &self,
        resource_path: &str,
        params: &[(String, String)],
    ) -> Result<T, Error> {
        let body = self.get(resource_path, params).await?;
        serde_json::from_slice(&body).map_err(|e| Error::Decode(e.to_string()))
    }
}
