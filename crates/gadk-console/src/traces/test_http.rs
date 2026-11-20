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

//! A scripted fake HTTP client for the trace-source tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::StreamExt as _;
use gadk_model::{BodyStream, HttpClient, ModelError};
use http::{HeaderMap, Method, StatusCode};
use serde_json::Value;

/// One request the fake client saw.
pub(crate) struct Seen {
    pub(crate) method: Method,
    pub(crate) uri: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

/// Records requests; answers each with the next scripted reply
/// (`Ok((status, body))`, or `Err(detail)` for a transport failure).
#[derive(Default)]
pub(crate) struct FakeHttp {
    replies: Mutex<VecDeque<Result<(u16, String), String>>>,
    pub(crate) seen: Mutex<Vec<Seen>>,
}

impl FakeHttp {
    pub(crate) fn new(replies: Vec<Result<(u16, String), String>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            seen: Mutex::default(),
        })
    }

    pub(crate) fn uris(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.uri.clone())
            .collect()
    }

    pub(crate) fn body(&self, i: usize) -> Value {
        serde_json::from_slice(&self.seen.lock().unwrap()[i].body).unwrap()
    }
}

#[async_trait::async_trait]
impl HttpClient for FakeHttp {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        let (parts, body) = req.into_parts();
        self.seen.lock().unwrap().push(Seen {
            method: parts.method,
            uri: parts.uri.to_string(),
            headers: parts.headers,
            body,
        });
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unscripted request");
        match reply {
            Ok((status, text)) => {
                let stream: BodyStream =
                    futures::stream::once(async move { Ok(Bytes::from(text)) }).boxed();
                let mut resp = http::Response::new(stream);
                *resp.status_mut() = StatusCode::from_u16(status).unwrap();
                Ok(resp)
            }
            Err(detail) => Err(ModelError::Transport(detail.into())),
        }
    }
}
