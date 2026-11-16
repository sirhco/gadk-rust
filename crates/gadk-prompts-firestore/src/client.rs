// Copyright 2026 Chris Olson
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

//! Firestore REST calls on gadk's REST transport.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gadk_model::gcp_auth::credential_headers;
use gadk_model::rest::{
    CallTimeout, RequestHeaders, RestError, RestReply, RestTransport, RetryPolicy,
};
use gadk_model::{HttpClient, ReqwestClient};
use gadk_prompts::StoreError;
use google_cloud_auth::credentials::Credentials;
use http::header::{CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderValue, Method, Uri};
use serde_json::Value;

/// Store-level retries only: the transport never retries.
const POLICY: RetryPolicy = RetryPolicy {
    max_retries: 0,
    ..RetryPolicy::OPENAI
};

/// A non-2xx reply, classified.
#[derive(Debug)]
pub(crate) enum CallError {
    NotFound,
    Aborted,
    Store(StoreError),
}

struct Headers {
    credentials: Option<Credentials>,
    json: bool,
}

#[async_trait::async_trait]
impl RequestHeaders for Headers {
    async fn headers(&self) -> Result<HeaderMap, RestError> {
        let mut h = HeaderMap::new();
        let ua = HeaderValue::from_str(&gadk_platform::tracking_header())
            .map_err(|e| RestError::Headers(Box::new(e)))?;
        h.insert(USER_AGENT, ua);
        if self.json {
            h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        if let Some(c) = &self.credentials {
            h.extend(
                credential_headers(c)
                    .await
                    .map_err(|e| RestError::Headers(Box::new(e)))?,
            );
        }
        Ok(h)
    }
}

pub(crate) struct Client {
    transport: RestTransport,
    credentials: Option<Credentials>,
    /// `{endpoint}/v1/`
    pub(crate) base: String,
}

impl Client {
    pub(crate) fn new(
        endpoint: &str,
        credentials: Option<Credentials>,
        http: Option<Arc<dyn HttpClient>>,
    ) -> Self {
        let own = http.is_none();
        Self {
            transport: RestTransport {
                http: http.unwrap_or_else(|| Arc::new(ReqwestClient::default())),
                policy: POLICY,
                response_header_timeout: own.then_some(Duration::from_secs(60)),
            },
            credentials,
            base: format!("{}/v1/", endpoint.trim_end_matches('/')),
        }
    }

    /// `method` on `{base}{path}` (`path` already URL-escaped), JSON body if any. A non-2xx
    /// reply is classified for the transactional paths: 404 / `NOT_FOUND` → `NotFound`; 409 /
    /// `ABORTED` / `FAILED_PRECONDITION` → `Aborted`; anything else → `Http`.
    pub(crate) async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, CallError> {
        match self
            .send(method, path, body)
            .await
            .map_err(CallError::Store)?
        {
            Ok(v) => Ok(v),
            Err((status, text)) => {
                let code = serde_json::from_slice::<Value>(text.as_bytes())
                    .ok()
                    .and_then(|v| {
                        v.pointer("/error/status")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                Err(match (status, code.as_str()) {
                    (404, _) | (_, "NOT_FOUND") => CallError::NotFound,
                    (409, _) | (_, "ABORTED" | "FAILED_PRECONDITION") => CallError::Aborted,
                    _ => CallError::Store(StoreError::Http { status, body: text }),
                })
            }
        }
    }

    /// Like [`call`](Self::call), but every non-2xx reply is `Http { status, body }` with the
    /// server's body kept (a missing database, a required index, …), for reads that are not
    /// part of a transaction.
    pub(crate) async fn call_unclassified(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, StoreError> {
        self.send(method, path, body)
            .await?
            .map_err(|(status, body)| StoreError::Http { status, body })
    }

    /// The decoded 2xx body, or `(status, body)` for a non-2xx reply.
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Result<Value, (u16, String)>, StoreError> {
        let url = format!("{}{path}", self.base);
        let uri: Uri = url
            .parse()
            .map_err(|e| StoreError::Invalid(format!("url {url}: {e}")))?;
        let bytes = body.map(|b| Bytes::from(b.to_string())).unwrap_or_default();
        let headers = Headers {
            credentials: self.credentials.clone(),
            json: !bytes.is_empty(),
        };
        let reply = self
            .transport
            .send_method(method, &uri, bytes, false, &headers, CallTimeout::None)
            .await
            .map_err(|e| StoreError::Io(e.to_string()))?;
        let RestReply::Read(parts, body) = reply else {
            return Err(StoreError::Decode("unexpected streaming reply".into()));
        };
        if parts.status.is_success() {
            return serde_json::from_slice(&body)
                .map(Ok)
                .map_err(|e| StoreError::Decode(e.to_string()));
        }
        Ok(Err((
            parts.status.as_u16(),
            String::from_utf8_lossy(&body).into_owned(),
        )))
    }
}
