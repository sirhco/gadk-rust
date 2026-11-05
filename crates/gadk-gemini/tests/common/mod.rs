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
#![allow(dead_code)]

//! Shared helpers for the gadk-gemini integration tests.

use bytes::Bytes;
use futures::StreamExt;
use gadk_gemini::{ClientConfig, MapEnv};
use gadk_httprr::RecordReplay;
use gadk_model::{BodyStream, HttpClient, ModelError, ReqwestClient};
use parking_lot::Mutex;
use std::sync::Arc;

pub fn testdata(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata")
        .join(name)
}

/// Port of testutil.NewGeminiTestClientConfig: httprr transport + fake key when replaying.
pub fn gemini_test_config(rr_file: &str, http: Arc<dyn HttpClient>) -> ClientConfig {
    let recording = RecordReplay::recording(testdata(rr_file)).unwrap_or(false);
    ClientConfig {
        api_key: Some(if recording {
            std::env::var("GOOGLE_API_KEY").unwrap_or_default()
        } else {
            "fakekey".into()
        }),
        http_client: Some(http),
        env: Arc::new(MapEnv(Default::default())),
        ..Default::default()
    }
}

pub fn rr(rr_file: &str) -> Arc<RecordReplay> {
    Arc::new(RecordReplay::open(testdata(rr_file), Arc::new(ReqwestClient::default())).unwrap())
}

/// Port of Go headerInterceptor: records requests, delegates to `base`.
pub struct Recorder {
    pub base: Arc<dyn HttpClient>,
    pub seen: Mutex<Vec<http::Request<Bytes>>>,
}

impl Recorder {
    pub fn new(base: Arc<dyn HttpClient>) -> Arc<Self> {
        Arc::new(Self {
            base,
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl HttpClient for Recorder {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        let mut copy = http::Request::builder()
            .method(req.method().clone())
            .uri(req.uri().clone())
            .body(req.body().clone())
            .unwrap();
        *copy.headers_mut() = req.headers().clone();
        self.seen.lock().push(copy);
        self.base.send(req).await
    }
}

/// Port of Go roundTripFunc test transports: answers every request with a fixed body
/// and records the request bodies.
pub struct FakeHttp {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
    pub seen: Mutex<Vec<http::Request<Bytes>>>,
}

impl FakeHttp {
    pub fn json(body: &str) -> Arc<Self> {
        Arc::new(Self {
            status: 200,
            content_type: "application/json",
            body: body.to_string(),
            seen: Mutex::new(Vec::new()),
        })
    }

    pub fn sse(body: &str) -> Arc<Self> {
        Arc::new(Self {
            status: 200,
            content_type: "text/event-stream",
            body: body.to_string(),
            seen: Mutex::new(Vec::new()),
        })
    }

    /// The JSON body of the last request.
    pub fn last_body(&self) -> serde_json::Value {
        let seen = self.seen.lock();
        let req = seen.last().expect("no request was sent");
        serde_json::from_slice(req.body()).unwrap()
    }

    pub fn last_uri(&self) -> String {
        self.seen
            .lock()
            .last()
            .expect("no request")
            .uri()
            .to_string()
    }
}

#[async_trait::async_trait]
impl HttpClient for FakeHttp {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        self.seen.lock().push(req);
        let body = Bytes::from(self.body.clone());
        let stream: BodyStream = futures::stream::iter([Ok(body)]).boxed();
        let resp = http::Response::builder()
            .status(self.status)
            .header("content-type", self.content_type)
            .body(stream)
            .unwrap();
        Ok(resp)
    }
}

/// A Vertex AI test config (as the Go tests' `Backend: VertexAI, Project, Location: "eu"`).
pub fn vertex_config(http: Arc<dyn HttpClient>) -> ClientConfig {
    ClientConfig {
        backend: Some(gadk_model::Backend::VertexAi),
        project: Some("test-project".into()),
        location: Some("eu".into()),
        http_client: Some(http),
        env: Arc::new(MapEnv(Default::default())),
        ..Default::default()
    }
}
