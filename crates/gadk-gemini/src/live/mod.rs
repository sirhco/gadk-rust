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
//! Dials the Live endpoint, sends the setup message, and implements the send and receive
//! sides of a [`LiveConnection`].

mod messages;
pub(crate) mod proxy;
mod url;

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use gadk_genai::{
    ActivityEnd, ActivityStart, Content, FunctionResponse, LiveClientContentInput,
    LiveClientContextUpdate, LiveConnectConfig, LiveRealtimeInput, LiveServerMessage,
    LiveToolResponseInput, Part,
};
pub use gadk_model::RealtimeInput;
use gadk_model::{Backend, LiveConnError, LiveConnErrorKind, LlmResponse};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use self::messages::RecvState;
use self::url::{live_url, model_full_name};
use crate::auth::auth_headers;
use crate::config::ResolvedConfig;
use crate::error::GeminiError;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Bound on the dial, proxy CONNECT, TLS and WebSocket upgrade together.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(45);

/// The PNG file signature, used to detect the MIME type of realtime image data.
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// A live connection to a model over a WebSocket.
///
/// Every method takes `&self`, so a connection can be shared through an `Arc` and used
/// from several tasks at once (one receiving while another sends). The write half and the
/// read half sit behind separate async mutexes:
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
    /// The setupComplete message, returned by the first
    /// receive.
    buffered_message: Option<LiveServerMessage>,
    state: RecvState,
}

impl RecvHalf {
    /// The buffered message, else the next data frame decoded.
    /// `None` when the stream ended without a close frame; a received close frame is an error
    /// with its code.
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
                Some(Err(e)) => {
                    return Err(GeminiError::Live(classify_ws_error(
                        "failed to receive message",
                        e,
                    )));
                }
                Some(Ok(Message::Text(t))) => t.as_bytes().to_vec(),
                Some(Ok(Message::Binary(b))) => b.to_vec(),
                // A received close frame is an error carrying its code.
                Some(Ok(Message::Close(frame))) => {
                    return Err(GeminiError::Live(close_frame_error(
                        "failed to receive message",
                        frame.as_ref(),
                    )));
                }
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

/// The text of a WebSocket close error: `websocket: close <code>`, the code's
/// name in parentheses for the named codes, then `: <reason>` when the reason is non-empty.
pub fn gorilla_close_text(code: u16, reason: &str) -> String {
    let name = match code {
        1000 => " (normal)",
        1001 => " (going away)",
        1002 => " (protocol error)",
        1003 => " (unsupported data)",
        1005 => " (no status)",
        1006 => " (abnormal closure)",
        1007 => " (invalid payload data)",
        1008 => " (policy violation)",
        1009 => " (message too big)",
        1010 => " (mandatory extension missing)",
        1011 => " (internal server error)",
        1015 => " (TLS handshake error)",
        _ => "",
    };
    let mut s = format!("websocket: close {code}{name}");
    if !reason.is_empty() {
        s.push_str(": ");
        s.push_str(reason);
    }
    s
}

/// `<context>: <e>`, classified: a closed connection, a socket I/O failure or a
/// reset without a closing handshake (close 1006) is `ConnectionLost`; anything else is
/// `Other`. The tungstenite error is kept as the source.
pub fn classify_ws_error(context: &str, e: tokio_tungstenite::tungstenite::Error) -> LiveConnError {
    use tokio_tungstenite::tungstenite::Error as WsError;
    let kind = match &e {
        WsError::ConnectionClosed
        | WsError::AlreadyClosed
        | WsError::Io(_)
        | WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
            LiveConnErrorKind::ConnectionLost
        }
        _ => LiveConnErrorKind::Other,
    };
    LiveConnError::new(kind, format!("{context}: {e}")).with_source(e)
}

/// A received close frame: kind `Closed { code }`, code 1005 for a
/// frame without a payload, message `<context>: <close text>` (see [`gorilla_close_text`]).
pub fn close_frame_error(context: &str, frame: Option<&CloseFrame>) -> LiveConnError {
    let (code, reason) = match frame {
        Some(f) => (u16::from(f.code), f.reason.as_str()),
        None => (1005, ""),
    };
    LiveConnError::new(
        LiveConnErrorKind::Closed { code },
        format!("{context}: {}", gorilla_close_text(code, reason)),
    )
}

/// Prefixes the text with `<context>: `, keeps the kind, and keeps `e` as the source.
pub fn wrap_conn_error(context: &str, e: LiveConnError) -> LiveConnError {
    LiveConnError::new(e.kind, format!("{context}: {}", e.message)).with_source(e)
}

/// [`wrap_conn_error`] on a [`GeminiError`]; a non-live error becomes `Other`.
fn wrap_live(context: &str, e: GeminiError) -> GeminiError {
    GeminiError::Live(wrap_conn_error(context, into_conn_error(e)))
}

/// An unclassified live error (decode failures, invalid input, setup failures).
pub(crate) fn live_other(message: impl Into<String>) -> GeminiError {
    GeminiError::Live(LiveConnError::new(LiveConnErrorKind::Other, message))
}

/// The trait-level error: the classified error itself, or `Other` with the text.
fn into_conn_error(e: GeminiError) -> LiveConnError {
    match e {
        GeminiError::Live(e) => e,
        other => LiveConnError::new(LiveConnErrorKind::Other, other.to_string()).with_source(other),
    }
}

/// Dials the Live endpoint (through `HTTPS_PROXY`/`HTTP_PROXY`
/// unless `NO_PROXY` or a loopback host says otherwise), sends the setup message and waits for
/// `setupComplete`, which is buffered for the first receive.
///
/// The setup message is built before dialing, so an invalid configuration
/// fails without opening a connection.
pub(crate) async fn connect(
    resolved: &ResolvedConfig,
    model_name: &str,
    cfg: &LiveConnectConfig,
) -> Result<LiveConnection, GeminiError> {
    connect_with_env(resolved, model_name, cfg, &|k: &str| std::env::var(k).ok()).await
}

/// [`connect`] with an injectable environment lookup for proxy resolution (test seam).
pub(crate) async fn connect_with_env(
    resolved: &ResolvedConfig,
    model_name: &str,
    cfg: &LiveConnectConfig,
    env: proxy::EnvLookup<'_>,
) -> Result<LiveConnection, GeminiError> {
    connect_with_timeout(resolved, model_name, cfg, env, HANDSHAKE_TIMEOUT).await
}

/// [`connect_with_env`] with an injectable dial and handshake timeout (test seam).
pub(crate) async fn connect_with_timeout(
    resolved: &ResolvedConfig,
    model_name: &str,
    cfg: &LiveConnectConfig,
    env: proxy::EnvLookup<'_>,
    handshake_timeout: Duration,
) -> Result<LiveConnection, GeminiError> {
    let url = live_url(resolved)?;
    let model_full_name = model_full_name(resolved, model_name)?;
    let setup = messages::setup_message(resolved.backend, &model_full_name, cfg)?;

    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| live_other(format!("invalid Live request: {e}")))?;
    let headers = request.headers_mut();
    // The client headers, every value added.
    for (name, value) in &resolved.http_options.headers {
        headers.append(name.clone(), value.clone());
    }
    // `extend` replaces each auth header name once and keeps all of its values.
    headers.extend(auth_headers(&resolved.auth).await?);

    // A dial failure is never a lost established connection: it is `Other`.
    // The dial uses the proxy from the environment, then a TCP (or CONNECT
    // tunnel) dial; TLS for wss is layered on the same stream.
    let proxy = proxy::proxy_for_with(&url, env);
    if let Some(p) = &proxy {
        // Host only: the proxy URL may carry credentials.
        tracing::debug!(
            proxy_host = p.host_str().unwrap_or_default(),
            "dialing Live through a proxy"
        );
    }
    // The handshake timeout bounds the dial, the proxy CONNECT, TLS and the
    // WebSocket upgrade together, so a black-holed endpoint cannot park a (re)connect forever.
    let handshake = async {
        let tcp = proxy::dial(&url, proxy.as_ref()).await.map_err(|e| {
            GeminiError::Live(LiveConnError::new(
                LiveConnErrorKind::Other,
                format!("Connect to {url} failed: {e}"),
            ))
        })?;
        tokio_tungstenite::client_async_tls_with_config(request, tcp, None, None)
            .await
            .map_err(|e| {
                GeminiError::Live(
                    LiveConnError::new(
                        LiveConnErrorKind::Other,
                        format!("Connect to {url} failed: {e}"),
                    )
                    .with_source(e),
                )
            })
    };
    let (ws, _) = tokio::time::timeout(handshake_timeout, handshake)
        .await
        .map_err(|_| {
            GeminiError::Live(LiveConnError::new(
                LiveConnErrorKind::Other,
                format!("Connect to {url} failed: i/o timeout"),
            ))
        })??;
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
        return Err(live_other(format!(
            "failed to write LiveClientSetup: {}",
            into_conn_error(e)
        )));
    }
    let backend = conn.backend;
    let setup_message = match conn.rx.get_mut().receive(backend).await {
        Ok(Some(msg)) => msg,
        Ok(None) => {
            return Err(live_other(
                "failed to receive setup complete: connection closed",
            ));
        }
        Err(e) => {
            let _ = conn.close().await;
            return Err(live_other(format!(
                "failed to receive setup complete: {}",
                into_conn_error(e)
            )));
        }
    };
    if setup_message.setup_complete.is_none() {
        let _ = conn.close().await;
        let got = serde_json::to_string(&setup_message).unwrap_or_default();
        return Err(live_other(format!(
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

    /// A Gemini 3.x Live model (not Live Translate), on
    /// either backend.
    fn is_gemini_3x_live(&self) -> bool {
        gadk_model::is_gemini_3x_live(&self.model_name)
    }

    async fn write(&self, text: String) -> Result<(), GeminiError> {
        self.sink
            .lock()
            .await
            .send(Message::text(text))
            .await
            .map_err(|e| GeminiError::Live(classify_ws_error("failed to send message", e)))
    }

    async fn send_client_content(&self, input: LiveClientContentInput) -> Result<(), GeminiError> {
        self.write(messages::client_content_message(self.backend, &input)?)
            .await
    }

    async fn send_realtime_input(&self, input: LiveRealtimeInput) -> Result<(), GeminiError> {
        self.write(messages::realtime_input_message(&input)?).await
    }

    /// Sends `history` without audio inline-data parts and without
    /// internal live-artifact references ([`gadk_model::is_internal_live_artifact_part`])
    /// as one client-content message, completing the turn when the last content is
    /// the user's. Nothing is sent when no part remains, or for gemini-3.1 models.
    pub async fn send_history(&self, history: &[Content]) -> Result<(), GeminiError> {
        // TODO: `initial_history_in_client_content` is not sent.
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
                            && !gadk_model::is_internal_live_artifact_part(p)
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
        .map_err(|e| wrap_live("failed to send history", e))
    }

    /// Sends function responses (when the first part is one) as a
    /// tool response; single-text content to a Gemini 3.x Live model as realtime text;
    /// anything else as a complete client-content turn.
    pub async fn send_content(&self, content: Content) -> Result<(), GeminiError> {
        if content.parts.is_empty() {
            return Err(live_other("empty content"));
        }
        if let Some(responses) = messages::function_responses(&content) {
            return self
                .send_tool_response(responses)
                .await
                .map_err(|e| wrap_live("failed to send tool response", e));
        }
        if let [only] = content.parts.as_slice()
            && !only.text.is_empty()
            && self.is_gemini_3x_live()
        {
            let text = only.text.clone();
            return self
                .send_realtime_input(LiveRealtimeInput {
                    text,
                    ..Default::default()
                })
                .await
                .map_err(|e| wrap_live("failed to send realtime text", e));
        }
        self.send_client_content(LiveClientContentInput {
            turns: vec![content],
            turn_complete: Some(true),
        })
        .await
        .map_err(|e| wrap_live("failed to send content", e))
    }

    /// Sends realtime input. Blobs go as `mediaChunks`, or for a Gemini 3.x Live or
    /// Gemini 3.5 Live Translate model as `video` (image MIME types) or `audio` (everything
    /// else, where the server rejects a type other than `audio/*`; the error goes to the
    /// caller).
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
                if self.is_gemini_3x_live()
                    || gadk_model::is_gemini_35_live_translate(&self.model_name)
                {
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

    /// Replies to a tool call.
    pub async fn send_tool_response(
        &self,
        responses: Vec<FunctionResponse>,
    ) -> Result<(), GeminiError> {
        let input = LiveToolResponseInput {
            function_responses: responses,
        };
        self.write(messages::tool_response_message(self.backend, &input)?)
            .await
    }

    /// Updates (preview) the system instruction and/or
    /// the tools of the current session. Only the fields that are set are updated; the
    /// update is processed in order with the rest of the inputs.
    pub async fn send_context_update(
        &self,
        input: LiveClientContextUpdate,
    ) -> Result<(), GeminiError> {
        self.write(messages::context_update_message(self.backend, &input)?)
            .await
    }

    /// The next response. `None` when the stream ended without a close
    /// frame; a received close frame is an error with its code.
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
            .map_err(|e| GeminiError::Live(classify_ws_error("failed to close connection", e)))
    }
}

/// The model-agnostic trait (gadk-model) over the Gemini Live connection.
#[async_trait]
impl gadk_model::LiveConnection for LiveConnection {
    async fn send_history(&self, history: &[Content]) -> Result<(), LiveConnError> {
        LiveConnection::send_history(self, history)
            .await
            .map_err(into_conn_error)
    }

    async fn send_content(&self, content: Content) -> Result<(), LiveConnError> {
        LiveConnection::send_content(self, content)
            .await
            .map_err(into_conn_error)
    }

    async fn send_realtime(&self, input: RealtimeInput) -> Result<(), LiveConnError> {
        LiveConnection::send_realtime(self, input)
            .await
            .map_err(into_conn_error)
    }

    async fn recv(&self) -> Result<Option<LlmResponse>, LiveConnError> {
        LiveConnection::recv(self).await.map_err(into_conn_error)
    }

    async fn close(&self) -> Result<(), LiveConnError> {
        LiveConnection::close(self).await.map_err(into_conn_error)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::error::Error;
    use std::io;

    use gadk_model::LiveConnErrorKind;
    use tokio_tungstenite::tungstenite::Error as WsError;
    use tokio_tungstenite::tungstenite::error::ProtocolError;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

    use super::{classify_ws_error, close_frame_error, gorilla_close_text, wrap_conn_error};

    fn frame(code: u16, reason: &str) -> CloseFrame {
        CloseFrame {
            code: CloseCode::from(code),
            reason: reason.into(),
        }
    }

    #[test]
    fn gorilla_close_text_matches_close_error_error() {
        let cases = [
            (1000, "", "websocket: close 1000 (normal)"),
            (1001, "", "websocket: close 1001 (going away)"),
            (
                1002,
                "fatal",
                "websocket: close 1002 (protocol error): fatal",
            ),
            (1003, "", "websocket: close 1003 (unsupported data)"),
            (1005, "", "websocket: close 1005 (no status)"),
            (1006, "", "websocket: close 1006 (abnormal closure)"),
            (1007, "", "websocket: close 1007 (invalid payload data)"),
            (
                1008,
                "policy violation",
                "websocket: close 1008 (policy violation): policy violation",
            ),
            (1009, "", "websocket: close 1009 (message too big)"),
            (
                1010,
                "",
                "websocket: close 1010 (mandatory extension missing)",
            ),
            (1011, "", "websocket: close 1011 (internal server error)"),
            (1015, "", "websocket: close 1015 (TLS handshake error)"),
            (1012, "", "websocket: close 1012"),
            (4000, "app", "websocket: close 4000: app"),
        ];
        for (code, reason, want) in cases {
            assert_eq!(gorilla_close_text(code, reason), want, "code {code}");
        }
    }

    #[test]
    fn classify_ws_error_kinds_and_resumability() {
        let cases: Vec<(&str, WsError, LiveConnErrorKind, bool)> = vec![
            (
                "connection closed",
                WsError::ConnectionClosed,
                LiveConnErrorKind::ConnectionLost,
                true,
            ),
            (
                "already closed",
                WsError::AlreadyClosed,
                LiveConnErrorKind::ConnectionLost,
                true,
            ),
            (
                "io broken pipe",
                WsError::Io(io::ErrorKind::BrokenPipe.into()),
                LiveConnErrorKind::ConnectionLost,
                true,
            ),
            (
                "io reset",
                WsError::Io(io::ErrorKind::ConnectionReset.into()),
                LiveConnErrorKind::ConnectionLost,
                true,
            ),
            (
                "reset without closing handshake",
                WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake),
                LiveConnErrorKind::ConnectionLost,
                true,
            ),
            (
                "other protocol error",
                WsError::Protocol(ProtocolError::NonZeroReservedBits),
                LiveConnErrorKind::Other,
                false,
            ),
            (
                "attack attempt",
                WsError::AttackAttempt,
                LiveConnErrorKind::Other,
                false,
            ),
        ];
        for (name, e, kind, resumable) in cases {
            let err = classify_ws_error("failed to receive message", e);
            assert_eq!(err.kind, kind, "{name}");
            assert_eq!(err.is_resumable(), resumable, "{name}: {err}");
            assert!(
                err.message.starts_with("failed to receive message: "),
                "{name}: {err}"
            );
            let source = err.source().expect("tungstenite error kept as source");
            assert!(source.downcast_ref::<WsError>().is_some(), "{name}");
        }
    }

    #[test]
    fn close_frames_are_errors_with_their_code() {
        let e = close_frame_error("failed to receive message", Some(&frame(1002, "fatal")));
        assert_eq!(e.kind, LiveConnErrorKind::Closed { code: 1002 });
        assert_eq!(
            e.to_string(),
            "failed to receive message: websocket: close 1002 (protocol error): fatal"
        );
        assert!(e.to_string().contains("1002"));
        assert!(!e.is_resumable());

        let e = close_frame_error(
            "failed to receive message",
            Some(&frame(1008, "policy violation")),
        );
        assert_eq!(e.kind, LiveConnErrorKind::Closed { code: 1008 });
        assert!(e.is_resumable());

        let e = close_frame_error(
            "failed to receive message",
            Some(&frame(1002, "error 1008")),
        );
        assert!(!e.is_resumable(), "1008 only in the reason must stay fatal");

        let e = close_frame_error("failed to receive message", None);
        assert_eq!(e.kind, LiveConnErrorKind::Closed { code: 1005 });
        assert_eq!(
            e.to_string(),
            "failed to receive message: websocket: close 1005 (no status)"
        );
        assert!(!e.is_resumable());
    }

    #[test]
    fn wrapping_keeps_kind_and_source() {
        let inner = classify_ws_error(
            "failed to send message",
            WsError::Io(io::Error::from_raw_os_error(10053)),
        );
        let wrapped = wrap_conn_error("failed to send content", inner);
        assert_eq!(wrapped.kind, LiveConnErrorKind::ConnectionLost);
        assert!(
            wrapped
                .message
                .starts_with("failed to send content: failed to send message: IO error: ")
        );
        assert!(wrapped.is_resumable());
        assert!(wrapped.source().is_some());
    }
}
