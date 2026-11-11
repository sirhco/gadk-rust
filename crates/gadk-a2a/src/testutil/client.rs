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

//! A scripted A2A client and a provider that hands it out.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
///
/// `send_message` and `cancel_task` answer with their scripted responses in order, one per call;
/// a call with nothing left to answer fails with an internal error naming the method. Every
/// `send_streaming_message` streams all the scripted stream events, in order.
pub struct FakeA2AClient {
    send: parking_lot::Mutex<Vec<Result<a2a::SendMessageResponse, a2a::A2AError>>>,
    stream: parking_lot::Mutex<Vec<Result<a2a::StreamResponse, a2a::A2AError>>>,
    cancel: parking_lot::Mutex<Vec<Result<a2a::Task, a2a::A2AError>>>,
    calls: parking_lot::Mutex<Vec<FakeCall>>,
}

impl FakeA2AClient {
    /// A client with nothing scripted.
    pub fn new() -> Self {
        Self {
            send: parking_lot::Mutex::new(Vec::new()),
            stream: parking_lot::Mutex::new(Vec::new()),
            cancel: parking_lot::Mutex::new(Vec::new()),
            calls: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// Scripts the next `send_message` response (responses answer calls in order).
    pub fn with_send(self, resp: Result<a2a::SendMessageResponse, a2a::A2AError>) -> Self {
        self.send.lock().push(resp);
        self
    }

    /// Scripts the events of every `send_streaming_message` stream (appended to any scripted
    /// before).
    pub fn with_stream(self, events: Vec<Result<a2a::StreamResponse, a2a::A2AError>>) -> Self {
        self.stream.lock().extend(events);
        self
    }

    /// Scripts the next `cancel_task` response (responses answer calls in order).
    pub fn with_cancel(self, resp: Result<a2a::Task, a2a::A2AError>) -> Self {
        self.cancel.lock().push(resp);
        self
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<FakeCall> {
        self.calls.lock().clone()
    }

    fn record(&self, call: FakeCall) {
        self.calls.lock().push(call);
    }
}

/// Takes the first scripted response, or an internal error naming `method` when none is left.
fn next<T>(
    script: &parking_lot::Mutex<Vec<Result<T, a2a::A2AError>>>,
    method: &str,
) -> Result<T, a2a::A2AError> {
    let mut script = script.lock();
    if script.is_empty() {
        return Err(a2a::A2AError::internal(format!(
            "FakeA2AClient: no scripted {method} response"
        )));
    }
    script.remove(0)
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
        self.record(FakeCall::Send(req.clone()));
        next(&self.send, "send_message")
    }

    async fn send_streaming_message(
        &self,
        req: &a2a::SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<a2a::StreamResponse, a2a::A2AError>>, a2a::A2AError> {
        self.record(FakeCall::Stream(req.clone()));
        let events = self.stream.lock().clone();
        Ok(Box::pin(futures::stream::iter(events)))
    }

    async fn cancel_task(&self, req: &a2a::CancelTaskRequest) -> Result<a2a::Task, a2a::A2AError> {
        self.record(FakeCall::Cancel(req.clone()));
        next(&self.cancel, "cancel_task")
    }

    async fn destroy(&self) -> Result<(), a2a::A2AError> {
        self.record(FakeCall::Destroy);
        Ok(())
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
        Self {
            client,
            created: AtomicUsize::new(0),
        }
    }

    /// How many clients were created so far.
    pub fn created(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl A2AClientProvider for FakeClientProvider {
    async fn create_client(
        &self,
        _cx: ClientContext<'_>,
        _card: &a2a::AgentCard,
    ) -> Result<Arc<dyn A2AClient>, Error> {
        self.created.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::clone(&self.client) as Arc<dyn A2AClient>)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use futures::StreamExt;

    use super::*;

    fn request(text: &str) -> a2a::SendMessageRequest {
        a2a::SendMessageRequest {
            message: a2a::Message::new(a2a::Role::User, vec![a2a::Part::text(text)]),
            configuration: None,
            metadata: None,
            tenant: None,
        }
    }

    fn agent_message(text: &str) -> a2a::Message {
        a2a::Message::new(a2a::Role::Agent, vec![a2a::Part::text(text)])
    }

    fn task(id: &str, state: a2a::TaskState) -> a2a::Task {
        a2a::Task {
            id: id.into(),
            context_id: "ctx".into(),
            status: a2a::TaskStatus {
                state,
                message: None,
                timestamp: None,
            },
            artifacts: None,
            history: None,
            metadata: None,
        }
    }

    #[tokio::test]
    async fn answers_send_and_cancel_in_order_then_fails() {
        let client = FakeA2AClient::new()
            .with_send(Ok(a2a::SendMessageResponse::Message(agent_message("one"))))
            .with_send(Err(a2a::A2AError::internal("boom")))
            .with_cancel(Ok(task("t1", a2a::TaskState::Canceled)));

        let first = client.send_message(&request("a")).await.unwrap();
        assert!(matches!(first, a2a::SendMessageResponse::Message(m) if m.text() == Some("one")));
        assert_eq!(
            client
                .send_message(&request("b"))
                .await
                .unwrap_err()
                .message,
            "boom"
        );
        assert_eq!(
            client
                .send_message(&request("c"))
                .await
                .unwrap_err()
                .message,
            "FakeA2AClient: no scripted send_message response"
        );

        let cancel = a2a::CancelTaskRequest {
            id: "t1".into(),
            metadata: None,
            tenant: None,
        };
        assert_eq!(
            client.cancel_task(&cancel).await.unwrap().status.state,
            a2a::TaskState::Canceled
        );
        assert_eq!(
            client.cancel_task(&cancel).await.unwrap_err().message,
            "FakeA2AClient: no scripted cancel_task response"
        );
    }

    #[tokio::test]
    async fn streams_the_scripted_events_on_every_call() {
        let client = FakeA2AClient::new()
            .with_stream(vec![Ok(a2a::StreamResponse::Task(task(
                "t1",
                a2a::TaskState::Working,
            )))])
            .with_stream(vec![Err(a2a::A2AError::internal("lost"))]);
        for _ in 0..2 {
            let events: Vec<_> = client
                .send_streaming_message(&request("a"))
                .await
                .unwrap()
                .collect()
                .await;
            assert_eq!(events.len(), 2);
            assert!(matches!(&events[0], Ok(a2a::StreamResponse::Task(t)) if t.id == "t1"));
            assert_eq!(events[1].as_ref().unwrap_err().message, "lost");
        }
    }

    #[tokio::test]
    async fn records_every_call_in_order() {
        let client = FakeA2AClient::default();
        client.send_message(&request("send")).await.ok();
        client.send_streaming_message(&request("stream")).await.ok();
        let cancel = a2a::CancelTaskRequest {
            id: "t9".into(),
            metadata: None,
            tenant: None,
        };
        client.cancel_task(&cancel).await.ok();
        client.destroy().await.unwrap();

        let calls = client.calls();
        assert_eq!(calls.len(), 4);
        assert!(matches!(&calls[0], FakeCall::Send(r) if r.message.text() == Some("send")));
        assert!(matches!(&calls[1], FakeCall::Stream(r) if r.message.text() == Some("stream")));
        assert!(matches!(&calls[2], FakeCall::Cancel(r) if r.id == "t9"));
        assert!(matches!(calls[3], FakeCall::Destroy));
    }

    #[tokio::test]
    async fn provider_hands_out_the_shared_client_and_counts() {
        let fake = Arc::new(FakeA2AClient::new());
        let provider = FakeClientProvider::new(Arc::clone(&fake));
        assert_eq!(provider.created(), 0);
        let card: a2a::AgentCard = serde_json::from_value(serde_json::json!({
            "name": "remote", "version": "1.0", "supportedInterfaces": [], "capabilities": {},
        }))
        .unwrap();
        for _ in 0..2 {
            let client = provider
                .create_client(ClientContext::Detached, &card)
                .await
                .unwrap();
            client.destroy().await.unwrap();
        }
        assert_eq!(provider.created(), 2);
        assert_eq!(fake.calls().len(), 2);
    }
}
