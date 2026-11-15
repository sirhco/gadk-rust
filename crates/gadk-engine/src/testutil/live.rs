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

//! Scripted in-memory live model (stands in for Go plugin/agentanalytics `liveTestLLM` over
//! `newFakeLiveServer`, and for the fake server of internal/llminternal/base_flow_live_test.go
//! where no socket is needed), plus the responses Go's fake Live server messages decode to.
//!
//! A [`ScriptedLiveLlm`] connection yields its scripted responses in order, then parks until the
//! flow closes it, as Go's fake server writes its messages and then reads until the client
//! closes the connection.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use gadk_genai::{Content, FunctionCall, LiveConnectConfig, Part};
use gadk_model::{
    LiveConnError, LiveConnErrorKind, LiveConnection, Llm, LlmRequest, LlmResponse, ModelError,
    RealtimeInput,
};
use parking_lot::Mutex;
use tokio::sync::Notify;

/// Shared state of a [`ScriptedLiveLlm`] and every connection it opened.
struct ScriptedState {
    responses: Mutex<VecDeque<LlmResponse>>,
    sent: Mutex<Vec<Content>>,
    closed: AtomicBool,
    close_notify: Notify,
    fail_history: bool,
    connects: AtomicUsize,
}

/// An [`Llm`] whose `connect_live` returns an in-memory connection that yields `responses` in
/// order and records what the flow sends. Its `generate_content` yields nothing: like Go's
/// `liveTestLLM`, it is for live runs only. All connections share one script.
pub struct ScriptedLiveLlm {
    name: String,
    state: Arc<ScriptedState>,
}

impl ScriptedLiveLlm {
    fn build(name: &str, responses: Vec<LlmResponse>, fail_history: bool) -> Arc<ScriptedLiveLlm> {
        Arc::new(ScriptedLiveLlm {
            name: name.to_owned(),
            state: Arc::new(ScriptedState {
                responses: Mutex::new(responses.into()),
                sent: Mutex::new(Vec::new()),
                closed: AtomicBool::new(false),
                close_notify: Notify::new(),
                fail_history,
                connects: AtomicUsize::new(0),
            }),
        })
    }

    /// A model named `name` whose connections yield `responses` in order, then park until closed.
    pub fn new(name: &str, responses: Vec<LlmResponse>) -> Arc<ScriptedLiveLlm> {
        Self::build(name, responses, false)
    }

    /// `send_history` fails client-side, as Go's NaN history does (`json: unsupported value: NaN`).
    pub fn failing_history(name: &str) -> Arc<ScriptedLiveLlm> {
        Self::build(name, Vec::new(), true)
    }

    /// Every content the flow sent on any connection (function responses included), in order.
    pub fn sent_contents(&self) -> Vec<Content> {
        self.state.sent.lock().clone()
    }

    /// Whether the flow closed a connection.
    pub fn closed(&self) -> bool {
        self.state.closed.load(Ordering::SeqCst)
    }

    /// How many connections the flow opened.
    pub fn connects(&self) -> usize {
        self.state.connects.load(Ordering::SeqCst)
    }
}

impl Llm for ScriptedLiveLlm {
    fn name(&self) -> &str {
        &self.name
    }

    fn generate_content(
        &self,
        _req: LlmRequest,
        _stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        futures::stream::empty().boxed()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn connect_live<'a>(
        &'a self,
        _cfg: LiveConnectConfig,
    ) -> BoxFuture<'a, Result<Box<dyn LiveConnection>, ModelError>> {
        self.state.connects.fetch_add(1, Ordering::SeqCst);
        let conn: Box<dyn LiveConnection> = Box::new(ScriptedConn(self.state.clone()));
        Box::pin(async move { Ok(conn) })
    }
}

struct ScriptedConn(Arc<ScriptedState>);

#[async_trait]
impl LiveConnection for ScriptedConn {
    async fn send_history(&self, _history: &[Content]) -> Result<(), LiveConnError> {
        if self.0.fail_history {
            return Err(LiveConnError::new(
                LiveConnErrorKind::Other,
                "failed to send history: json: unsupported value: NaN",
            ));
        }
        Ok(())
    }

    async fn send_content(&self, content: Content) -> Result<(), LiveConnError> {
        self.0.sent.lock().push(content);
        Ok(())
    }

    async fn send_realtime(&self, _input: RealtimeInput) -> Result<(), LiveConnError> {
        Ok(())
    }

    /// The scripted responses in order, then parks until `close`, then a non-resumable error.
    async fn recv(&self) -> Result<Option<LlmResponse>, LiveConnError> {
        loop {
            let notified = self.0.close_notify.notified();
            if let Some(r) = self.0.responses.lock().pop_front() {
                return Ok(Some(r));
            }
            if self.0.closed.load(Ordering::SeqCst) {
                return Err(LiveConnError::new(
                    LiveConnErrorKind::Other,
                    "scripted connection closed",
                ));
            }
            notified.await;
        }
    }

    async fn close(&self) -> Result<(), LiveConnError> {
        self.0.closed.store(true, Ordering::SeqCst);
        self.0.close_notify.notify_waiters();
        Ok(())
    }
}

/// The response the Gemini live client delivers for the server's `{"setupComplete":{}}` (it
/// buffers that message for the first receive): an empty response. Go's fake Live server sends
/// it before its scripted messages, so a script that stands in for one starts with this.
pub fn setup_complete() -> LlmResponse {
    LlmResponse::default()
}

/// The response for `{"toolCall":{"functionCalls":[{"id":<id>,"name":<name>,"args":{}}, …]}}`:
/// a `"model"` content with one function-call part per `(id, name)`, in order, each with empty
/// args.
pub fn tool_call(calls: &[(&str, &str)]) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".into(),
            parts: calls
                .iter()
                .map(|(id, name)| Part {
                    function_call: Some(FunctionCall {
                        id: (*id).to_owned(),
                        name: (*name).to_owned(),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .collect(),
        }),
        ..Default::default()
    }
}

/// The response for
/// `{"serverContent":{"modelTurn":{"parts":[{"text":<text>}],"role":"model"},"turnComplete":<turn_complete>}}`.
pub fn model_turn(text: &str, turn_complete: bool) -> LlmResponse {
    LlmResponse {
        content: Some(Content::from_text(text, "model")),
        turn_complete,
        ..Default::default()
    }
}

/// Go `newFakeLiveServer(t, messages...)` + `&liveTestLLM{client}` (name `"live-test-model"`),
/// for scripts built from [`tool_call`] and [`model_turn`]: a [`ScriptedLiveLlm`] named
/// `"live-test-model"` that yields [`setup_complete`] and then `responses`.
pub fn go_live_test_llm(responses: Vec<LlmResponse>) -> Arc<ScriptedLiveLlm> {
    let mut script = Vec::with_capacity(responses.len() + 1);
    script.push(setup_complete());
    script.extend(responses);
    ScriptedLiveLlm::new("live-test-model", script)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::live::test_server::{FakeConn, FakeLiveServer, serve};

    /// What the real Gemini live client decodes from Go's fake Live server messages (the exact
    /// strings the agentanalytics live tests send) equals what the helpers script.
    #[tokio::test]
    async fn helpers_match_the_gemini_decoding_of_go_fake_server_messages() {
        const TOOL_CALL: &str =
            r#"{"toolCall":{"functionCalls":[{"id":"call-1","name":"run_helper","args":{}}]}}"#;
        const DONE: &str = r#"{"serverContent":{"modelTurn":{"parts":[{"text":"done"}],"role":"model"},"turnComplete":true}}"#;
        let server = FakeLiveServer::start(serve(|_, mut conn: FakeConn| async move {
            for msg in [TOOL_CALL, DONE] {
                assert!(conn.write_text(msg).await, "writing {msg}");
            }
            conn.block_until_client_closes().await;
        }))
        .await;
        let model = server.model().await;
        let conn = model
            .connect_live(LiveConnectConfig::default())
            .await
            .unwrap();
        let mut got = Vec::new();
        for _ in 0..3 {
            let resp = conn.recv().await.unwrap().unwrap();
            got.push(resp);
        }
        conn.close().await.unwrap();

        assert_eq!(
            got,
            vec![
                setup_complete(),
                tool_call(&[("call-1", "run_helper")]),
                model_turn("done", true),
            ]
        );
    }

    /// The scripted connection yields the script in order, records sends, and ends after close.
    #[tokio::test]
    async fn scripted_connection_yields_script_then_parks_until_closed() {
        let llm = go_live_test_llm(vec![tool_call(&[("call-1", "t")])]);
        assert_eq!(llm.name(), "live-test-model");
        let conn = llm
            .connect_live(LiveConnectConfig::default())
            .await
            .unwrap();
        assert_eq!(llm.connects(), 1);
        assert_eq!(conn.recv().await.unwrap(), Some(setup_complete()));
        assert_eq!(
            conn.recv().await.unwrap(),
            Some(tool_call(&[("call-1", "t")]))
        );
        conn.send_content(Content::from_text("hi", "user"))
            .await
            .unwrap();
        assert_eq!(llm.sent_contents(), vec![Content::from_text("hi", "user")]);
        let parked = tokio::time::timeout(std::time::Duration::from_millis(50), conn.recv()).await;
        assert!(parked.is_err(), "recv returned before close");
        conn.close().await.unwrap();
        assert!(llm.closed());
        assert!(conn.recv().await.is_err());
    }
}
