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

//! The Live (bidi streaming) connection over a WebSocket, for the Gemini API and Vertex AI.
//!
//! Ports genai v1.75.0 `live.go` (`Live.Connect`, `Session` send/receive) and ADK-Go
//! `internal/llminternal/googlellm/live_connection.go` (`LiveConnection`).

mod messages;
mod url;

use std::fmt;

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use gadk_genai::{
    ActivityEnd, ActivityStart, Content, FunctionResponse, LiveClientContentInput,
    LiveConnectConfig, LiveRealtimeInput, LiveServerMessage, LiveToolResponseInput, Part,
};
pub use gadk_model::RealtimeInput;
use gadk_model::{Backend, LlmResponse};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use self::messages::RecvState;
use self::url::{live_url, model_full_name};
use crate::auth::auth_headers;
use crate::config::ResolvedConfig;
use crate::error::GeminiError;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The PNG file signature (Go `SendRealtime` MIME detection).
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// A live connection to a model (port of ADK-Go `googlellm.LiveConnection` over a genai
/// `Session`).
///
/// Every method takes `&self`, so a connection can be shared through an `Arc` and used
/// from several tasks at once, as ADK-Go's live flow does (one goroutine receiving while
/// another sends). The write half and the read half sit behind separate async mutexes:
/// frames from concurrent sends are never interleaved, and a pending
/// [`recv`](Self::recv) never blocks a send. Concurrent `recv` calls are served one at a
/// time.
pub struct LiveConnection {
    sink: tokio::sync::Mutex<SplitSink<Ws, Message>>,
    rx: tokio::sync::Mutex<RecvHalf>,
    backend: Backend,
    model_name: String,
}

/// The read half of a [`LiveConnection`] and the state the receive path keeps.
struct RecvHalf {
    stream: SplitStream<Ws>,
    /// genai `Session.bufferedMessage`: the setupComplete message, returned by the first
    /// receive.
    buffered_message: Option<LiveServerMessage>,
    state: RecvState,
}

impl RecvHalf {
    /// genai `Session.Receive`: the buffered message, else the next data frame decoded.
    /// `None` when the connection is closed.
    async fn receive(
        &mut self,
        backend: Backend,
    ) -> Result<Option<LiveServerMessage>, GeminiError> {
        if let Some(msg) = self.buffered_message.take() {
            return Ok(Some(msg));
        }
        loop {
            let data = match self.stream.next().await {
                None => return Ok(None),
                Some(Err(e)) => return Err(live_err("failed to receive message", e)),
                Some(Ok(Message::Text(t))) => t.as_bytes().to_vec(),
                Some(Ok(Message::Binary(b))) => b.to_vec(),
                Some(Ok(Message::Close(_))) => return Ok(None),
                // Pings are answered by tungstenite; pongs and raw frames carry no message.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
            };
            return messages::decode_server_message(backend, &data).map(Some);
        }
    }
}

impl fmt::Debug for LiveConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveConnection")
            .field("backend", &self.backend)
            .field("model_name", &self.model_name)
            .finish_non_exhaustive()
    }
}

fn live_err(context: &str, e: impl fmt::Display) -> GeminiError {
    GeminiError::Live(format!("{context}: {e}"))
}

/// Port of genai `Live.Connect`: dials the Live endpoint, sends the setup message and
/// waits for `setupComplete`, which is buffered for the first receive.
///
/// gadk deviation: the setup message is built before dialing, so an invalid configuration
/// fails without opening a connection.
pub(crate) async fn connect(
    resolved: &ResolvedConfig,
    model_name: &str,
    cfg: &LiveConnectConfig,
) -> Result<LiveConnection, GeminiError> {
    let url = live_url(resolved)?;
    let model_full_name = model_full_name(resolved, model_name)?;
    let setup = messages::setup_message(resolved.backend, &model_full_name, cfg)?;

    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| live_err("invalid Live request", e))?;
    let headers = request.headers_mut();
    // genai mergeHeaders: the client headers, every value added.
    for (name, value) in &resolved.http_options.headers {
        headers.append(name.clone(), value.clone());
    }
    // `extend` replaces each auth header name once and keeps all of its values.
    headers.extend(auth_headers(&resolved.auth).await?);

    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| live_err(&format!("Connect to {url} failed"), e))?;
    let (sink, stream) = ws.split();
    let mut conn = LiveConnection {
        sink: tokio::sync::Mutex::new(sink),
        rx: tokio::sync::Mutex::new(RecvHalf {
            stream,
            buffered_message: None,
            state: RecvState::default(),
        }),
        backend: resolved.backend,
        model_name: model_name.to_string(),
    };

    if let Err(e) = conn.write(setup).await {
        let _ = conn.close().await;
        return Err(live_err("failed to write LiveClientSetup", e));
    }
    let backend = conn.backend;
    let setup_message = match conn.rx.get_mut().receive(backend).await {
        Ok(Some(msg)) => msg,
        Ok(None) => {
            return Err(GeminiError::Live(
                "failed to receive setup complete: connection closed".into(),
            ));
        }
        Err(e) => {
            let _ = conn.close().await;
            return Err(live_err("failed to receive setup complete", e));
        }
    };
    if setup_message.setup_complete.is_none() {
        let _ = conn.close().await;
        let got = serde_json::to_string(&setup_message).unwrap_or_default();
        return Err(GeminiError::Live(format!(
            "expected SetupComplete message, got: {got}"
        )));
    }
    conn.rx.get_mut().buffered_message = Some(setup_message);
    Ok(conn)
}

impl LiveConnection {
    fn is_gemini_31(&self) -> bool {
        self.model_name.contains("gemini-3.1")
    }

    async fn write(&self, text: String) -> Result<(), GeminiError> {
        self.sink
            .lock()
            .await
            .send(Message::text(text))
            .await
            .map_err(|e| live_err("failed to send message", e))
    }

    async fn send_client_content(&self, input: LiveClientContentInput) -> Result<(), GeminiError> {
        self.write(messages::client_content_message(self.backend, &input)?)
            .await
    }

    async fn send_realtime_input(&self, input: LiveRealtimeInput) -> Result<(), GeminiError> {
        self.write(messages::realtime_input_message(&input)?).await
    }

    /// Port of Go `SendHistory`: sends `history` without audio inline-data parts as one
    /// client-content message, completing the turn when the last content is the user's.
    /// Nothing is sent when no part remains, or for gemini-3.1 models.
    pub async fn send_history(&self, history: &[Content]) -> Result<(), GeminiError> {
        // TODO(adk-go): genai seems to be missing initial_history_in_client_content.
        if self.is_gemini_31() {
            tracing::debug!("skipping sending history for gemini 3.1");
            return Ok(());
        }
        let filtered: Vec<Content> = history
            .iter()
            .filter_map(|c| {
                let parts: Vec<Part> = c
                    .parts
                    .iter()
                    .filter(|p| {
                        !p.inline_data
                            .as_ref()
                            .is_some_and(|b| b.mime_type.starts_with("audio/"))
                    })
                    .cloned()
                    .collect();
                (!parts.is_empty()).then(|| Content {
                    parts,
                    role: c.role.clone(),
                })
            })
            .collect();
        tracing::debug!(len = filtered.len(), "sending history");
        let Some(last) = filtered.last() else {
            return Ok(());
        };
        let turn_complete = last.role == "user";
        self.send_client_content(LiveClientContentInput {
            turns: filtered,
            turn_complete: Some(turn_complete),
        })
        .await
        .map_err(|e| live_err("failed to send history", e))
    }

    /// Port of Go `SendContent`: function responses (when the first part is one) go as a
    /// tool response; single-text content to a gemini-3.1 model on the Gemini API goes as
    /// realtime text; anything else as a complete client-content turn.
    pub async fn send_content(&self, content: Content) -> Result<(), GeminiError> {
        if content.parts.is_empty() {
            return Err(GeminiError::Live("empty content".into()));
        }
        if let Some(responses) = messages::function_responses(&content) {
            return self
                .send_tool_response(responses)
                .await
                .map_err(|e| live_err("failed to send tool response", e));
        }
        if let [only] = content.parts.as_slice()
            && !only.text.is_empty()
            && self.is_gemini_31()
            && self.backend == Backend::GeminiApi
        {
            let text = only.text.clone();
            return self
                .send_realtime_input(LiveRealtimeInput {
                    text,
                    ..Default::default()
                })
                .await
                .map_err(|e| live_err("failed to send realtime text", e));
        }
        self.send_client_content(LiveClientContentInput {
            turns: vec![content],
            turn_complete: Some(true),
        })
        .await
        .map_err(|e| live_err("failed to send content", e))
    }

    /// Port of Go `SendRealtime`. Blobs go as `mediaChunks`, or for a gemini-3.1 model on
    /// the Gemini API as `video` (image MIME types) or `audio`.
    pub async fn send_realtime(&self, input: RealtimeInput) -> Result<(), GeminiError> {
        let input = match input {
            RealtimeInput::Blob(mut blob) => {
                if blob.mime_type.is_empty() {
                    blob.mime_type = if blob.data.starts_with(&PNG_SIGNATURE) {
                        "image/png".into()
                    } else {
                        "audio/pcm".into()
                    };
                }
                if self.is_gemini_31() && self.backend == Backend::GeminiApi {
                    if blob.mime_type.starts_with("image/") {
                        LiveRealtimeInput {
                            video: Some(blob),
                            ..Default::default()
                        }
                    } else {
                        LiveRealtimeInput {
                            audio: Some(blob),
                            ..Default::default()
                        }
                    }
                } else {
                    LiveRealtimeInput {
                        media: Some(blob),
                        ..Default::default()
                    }
                }
            }
            RealtimeInput::ActivityStart => {
                tracing::debug!("sending activity start");
                LiveRealtimeInput {
                    activity_start: Some(ActivityStart {}),
                    ..Default::default()
                }
            }
            RealtimeInput::ActivityEnd => {
                tracing::debug!("sending activity end");
                LiveRealtimeInput {
                    activity_end: Some(ActivityEnd {}),
                    ..Default::default()
                }
            }
            RealtimeInput::AudioStreamEnd => LiveRealtimeInput {
                audio_stream_end: true,
                ..Default::default()
            },
        };
        self.send_realtime_input(input).await
    }

    /// genai `SendToolResponse`: replies to a tool call.
    pub async fn send_tool_response(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), GeminiError> {
        let input = LiveToolResponseInput {
            function_responses: responses,
        };
        self.write(messages::tool_response_message(&input)?).await
    }

    /// Port of Go `Recv`: the next response. `None` when the connection is closed.
    ///
    /// Takes `&self` and locks only the read half, so it may be awaited while other tasks
    /// send on the same connection. Cancel-safe: dropping a pending `recv` loses no
    /// message.
    pub async fn recv(&self) -> Result<Option<LlmResponse>, GeminiError> {
        let mut rx = self.rx.lock().await;
        if let Some(resp) = rx.state.pop_buffered() {
            return Ok(Some(resp));
        }
        let Some(msg) = rx.receive(self.backend).await? else {
            return Ok(None);
        };
        Ok(Some(rx.state.map(msg)))
    }

    /// Closes the connection by sending a close frame.
    pub async fn close(&self) -> Result<(), GeminiError> {
        self.sink
            .lock()
            .await
            .send(Message::Close(None))
            .await
            .map_err(|e| live_err("failed to close connection", e))
    }
}
