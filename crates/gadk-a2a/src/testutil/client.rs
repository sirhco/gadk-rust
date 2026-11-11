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

//! A scripted A2A client and a provider that hands it out. Bodies: plan 5b Task 6.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use futures::stream::BoxStream;

use crate::Error;
use crate::remoteagent::{A2AClient, A2AClientProvider, ClientContext};

/// One call a [`FakeA2AClient`] received.
#[derive(Debug, Clone)]
pub enum FakeCall {
    /// `send_message`.
    Send(a2a::SendMessageRequest),
    /// `send_streaming_message`.
    Stream(a2a::SendMessageRequest),
    /// `cancel_task`.
    Cancel(a2a::CancelTaskRequest),
    /// `destroy`.
    Destroy,
}

/// Answers each call with its scripted response and records the requests.
pub struct FakeA2AClient {
    send: parking_lot::Mutex<Vec<Result<a2a::SendMessageResponse, a2a::A2AError>>>,
    stream: parking_lot::Mutex<Vec<Result<a2a::StreamResponse, a2a::A2AError>>>,
    cancel: parking_lot::Mutex<Vec<Result<a2a::Task, a2a::A2AError>>>,
    calls: parking_lot::Mutex<Vec<FakeCall>>,
}

impl FakeA2AClient {
    /// A client with nothing scripted.
    pub fn new() -> Self {
        todo!("plan 5b Task 6")
    }

    /// Scripts a `send_message` response.
    pub fn with_send(self, resp: Result<a2a::SendMessageResponse, a2a::A2AError>) -> Self {
        todo!("plan 5b Task 6")
    }

    /// Scripts the events of a `send_streaming_message` stream.
    pub fn with_stream(self, events: Vec<Result<a2a::StreamResponse, a2a::A2AError>>) -> Self {
        todo!("plan 5b Task 6")
    }

    /// Scripts a `cancel_task` response.
    pub fn with_cancel(self, resp: Result<a2a::Task, a2a::A2AError>) -> Self {
        todo!("plan 5b Task 6")
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<FakeCall> {
        todo!("plan 5b Task 6")
    }
}

impl Default for FakeA2AClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl A2AClient for FakeA2AClient {
    async fn send_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<a2a::SendMessageResponse, a2a::A2AError> {
        todo!("plan 5b Task 6")
    }

    async fn send_streaming_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>, a2a::A2AError> {
        todo!("plan 5b Task 6")
    }

    async fn cancel_task(&self, req: &a2a::CancelTaskRequest) -> Result<a2a::Task, a2a::A2AError> {
        todo!("plan 5b Task 6")
    }

    async fn destroy(&self) -> Result<(), a2a::A2AError> {
        todo!("plan 5b Task 6")
    }
}

/// Hands out one shared [`FakeA2AClient`] and counts the clients created.
pub struct FakeClientProvider {
    client: Arc<FakeA2AClient>,
    created: AtomicUsize,
}

impl FakeClientProvider {
    /// A provider handing out `client`.
    pub fn new(client: Arc<FakeA2AClient>) -> Self {
        todo!("plan 5b Task 6")
    }

    /// How many clients were created so far.
    pub fn created(&self) -> usize {
        todo!("plan 5b Task 6")
    }
}

#[async_trait::async_trait]
impl A2AClientProvider for FakeClientProvider {
    async fn create_client(
        &self,
        cx: ClientContext<'_>,
        card: &a2a::AgentCard,
    ) -> Result<Arc<dyn A2AClient>, Error> {
        todo!("plan 5b Task 6")
    }
}
