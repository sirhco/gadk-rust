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

//! Test fixtures: the injected environment, fake GCP detectors, fake credentials, failing
//! processors, a span exporter that keeps its resource, and a local OTLP/HTTP sink.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use gadk_platform::env::{EnvProvider, MapEnv};
use google_cloud_auth::credentials::{
    CacheableResource, Credentials, CredentialsProvider, EntityTag,
};
use google_cloud_auth::errors::CredentialsError;
use http::{HeaderMap, HeaderName, HeaderValue};
use opentelemetry::Context;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::{LogProcessor, SdkLogRecord};
use opentelemetry_sdk::trace::{InMemorySpanExporter, Span, SpanData, SpanExporter, SpanProcessor};

use crate::gcp_detector::{PartialResource, ResourceDetector};

/// An injected environment holding exactly `vars`.
pub(crate) fn env(vars: &[(&str, &str)]) -> Arc<dyn EnvProvider> {
    Arc::new(MapEnv(
        vars.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<HashMap<_, _>>(),
    ))
}

/// A GCP detector off GCP: adds nothing.
pub(crate) struct NoDetector;

#[async_trait]
impl ResourceDetector for NoDetector {
    async fn detect(&self) -> Result<Option<Resource>, PartialResource> {
        Ok(None)
    }
}

/// A GCP detector returning a fixed outcome.
pub(crate) struct FixedDetector(pub(crate) Result<Option<Resource>, PartialResource>);

#[async_trait]
impl ResourceDetector for FixedDetector {
    async fn detect(&self) -> Result<Option<Resource>, PartialResource> {
        self.0.clone()
    }
}

/// Credentials producing fixed headers (or failing when `headers` is `None`).
#[derive(Debug)]
struct FixedCredentials(Option<HeaderMap>);

impl CredentialsProvider for FixedCredentials {
    async fn headers(
        &self,
        _extensions: http::Extensions,
    ) -> Result<CacheableResource<HeaderMap>, CredentialsError> {
        match &self.0 {
            Some(h) => Ok(CacheableResource::New {
                entity_tag: EntityTag::new(),
                data: h.clone(),
            }),
            None => Err(CredentialsError::from_msg(false, "no token")),
        }
    }

    async fn universe_domain(&self) -> Option<String> {
        None
    }
}

/// Credentials whose headers are `headers`.
pub(crate) fn fake_credentials(headers: &[(&'static str, &'static str)]) -> Credentials {
    let map = headers
        .iter()
        .map(|(k, v)| (HeaderName::from_static(k), HeaderValue::from_static(v)))
        .collect();
    Credentials::from(FixedCredentials(Some(map)))
}

/// Credentials whose header lookup fails.
pub(crate) fn failing_credentials() -> Credentials {
    Credentials::from(FixedCredentials(None))
}

/// A span processor whose shutdown fails with `Operation failed: <0>`.
#[derive(Debug)]
pub(crate) struct FailingSpanProcessor(pub(crate) &'static str);

impl SpanProcessor for FailingSpanProcessor {
    fn on_start(&self, _span: &mut Span, _cx: &Context) {}

    fn on_end(&self, _span: SpanData) {}

    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        Err(OTelSdkError::InternalFailure(self.0.to_owned()))
    }
}

/// A log processor whose shutdown fails with `Operation failed: <0>`.
#[derive(Debug)]
pub(crate) struct FailingLogProcessor(pub(crate) &'static str);

impl LogProcessor for FailingLogProcessor {
    fn emit(&self, _data: &mut SdkLogRecord, _scope: &opentelemetry::InstrumentationScope) {}

    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        Err(OTelSdkError::InternalFailure(self.0.to_owned()))
    }
}

/// An in-memory span exporter that also keeps the resource its provider set (Go's
/// `tracetest.SpanStub.Resource`).
#[derive(Debug, Clone, Default)]
pub(crate) struct ResourceSpanExporter {
    pub(crate) spans: InMemorySpanExporter,
    resource: Arc<Mutex<Option<Resource>>>,
}

impl ResourceSpanExporter {
    /// The resource the provider set.
    pub(crate) fn resource(&self) -> Option<Resource> {
        self.resource.lock().unwrap().clone()
    }
}

impl SpanExporter for ResourceSpanExporter {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        self.spans.export(batch).await
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.spans.shutdown_with_timeout(timeout)
    }

    fn set_resource(&mut self, resource: &Resource) {
        *self.resource.lock().unwrap() = Some(resource.clone());
    }
}

/// One request the [`Sink`] received.
#[derive(Debug, Clone)]
pub(crate) struct Captured {
    pub(crate) path: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
}

/// A local OTLP/HTTP collector: answers every request `200` and records it.
pub(crate) struct Sink {
    pub(crate) url: String,
    requests: Arc<Mutex<Vec<Captured>>>,
}

impl Sink {
    /// Serves on `127.0.0.1` on the current runtime.
    pub(crate) async fn start() -> Self {
        let requests: Arc<Mutex<Vec<Captured>>> = Arc::default();
        let seen = Arc::clone(&requests);
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let seen = Arc::clone(&seen);
            async move {
                let (parts, body) = req.into_parts();
                let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                seen.lock().unwrap().push(Captured {
                    path: parts.uri.path().to_owned(),
                    headers: parts.headers,
                    body: body.to_vec(),
                });
                http::StatusCode::OK
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, requests }
    }

    /// The requests received so far.
    pub(crate) fn requests(&self) -> Vec<Captured> {
        self.requests.lock().unwrap().clone()
    }
}
