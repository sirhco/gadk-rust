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

//! A scripted [`HttpClient`] for the continuation tests.

use bytes::Bytes;
use gadk_model::{BodyStream, HttpClient, ModelError};
use parking_lot::Mutex;
use std::collections::VecDeque;

/// Returns queued (status, content-type, body) responses in order; records request bodies.
/// A queued entry with status 0 makes the request fail with a transport error.
pub struct Scripted {
    pub responses: Mutex<VecDeque<(u16, &'static str, String)>>,
    pub bodies: Mutex<Vec<serde_json::Value>>,
}

impl Scripted {
    pub fn new(r: Vec<(u16, &'static str, String)>) -> Self {
        Self {
            responses: Mutex::new(r.into()),
            bodies: Mutex::new(vec![]),
        }
    }
}

#[async_trait::async_trait]
impl HttpClient for Scripted {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        self.bodies
            .lock()
            .push(serde_json::from_slice(req.body()).unwrap());
        let (status, ct, body) = self
            .responses
            .lock()
            .pop_front()
            .expect("unexpected extra request");
        if status == 0 {
            return Err(ModelError::Transport("scripted transport error".into()));
        }
        let stream: BodyStream = Box::pin(futures::stream::iter(vec![Ok(Bytes::from(body))]));
        Ok(http::Response::builder()
            .status(status)
            .header("content-type", ct)
            .body(stream)
            .unwrap())
    }
}
