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

//! Live (bidi streaming) connection abstraction: [`LiveConnection`], its error type and the
//! rule for when a failed connection can be resumed.

use std::error::Error;
use std::fmt;

use async_trait::async_trait;
use gadk_genai::{Blob, Content};

use crate::LlmResponse;

/// Realtime input for [`LiveConnection::send_realtime`]: a blob, activity start/end markers
/// and the end of the audio stream. Moved from gadk-gemini, which re-exports it.
#[derive(Debug, Clone, PartialEq)]
pub enum RealtimeInput {
    /// Audio or image data. An empty MIME type is detected by the connection.
    Blob(Blob),
    /// Start of user activity (manual activity detection).
    ActivityStart,
    /// End of user activity (manual activity detection).
    ActivityEnd,
    /// The audio stream has ended (automatic activity detection).
    AudioStreamEnd,
}

/// What kind of failure a [`LiveConnError`] is, as [`LiveConnError::is_resumable`] checks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveConnErrorKind {
    /// The socket of an established connection failed, or the stream ended without a close
    /// frame.
    ConnectionLost,
    /// A close frame was received; a frame without a payload is 1005.
    Closed {
        /// The close code.
        code: u16,
    },
    /// Anything else.
    Other,
}

/// A failure on a live connection. `message` is the full error text.
#[derive(Debug)]
pub struct LiveConnError {
    /// The classification.
    pub kind: LiveConnErrorKind,
    /// The full error text.
    pub message: String,
    /// The underlying error, if any.
    pub source: Option<Box<dyn Error + Send + Sync>>,
}

/// The text fragments that mark an error as a lost connection.
const RESUMABLE_TEXT: [&str; 4] = ["broken pipe", "connection reset", "EOF", "GoAway"];

/// The WebSocket close code for a policy violation.
const CLOSE_POLICY_VIOLATION: u16 = 1008;

impl LiveConnError {
    /// An error with no source.
    pub fn new(kind: LiveConnErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// Attaches the underlying error.
    pub fn with_source(mut self, source: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// The stream ended without a close frame.
    pub fn eof() -> Self {
        Self::new(LiveConnErrorKind::ConnectionLost, "EOF")
    }

    /// Whether the session can be resumed after this error: a lost connection, a close frame
    /// with code 1008, or text naming a dropped connection. The text rule applies to every
    /// kind, including a close frame with any other code.
    pub fn is_resumable(&self) -> bool {
        match self.kind {
            LiveConnErrorKind::ConnectionLost => true,
            LiveConnErrorKind::Closed { code } if code == CLOSE_POLICY_VIOLATION => true,
            _ => RESUMABLE_TEXT.iter().any(|t| self.message.contains(t)),
        }
    }
}

impl fmt::Display for LiveConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for LiveConnError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|e| e as &(dyn Error + 'static))
    }
}

/// A live (bidi streaming) connection to a model, model-agnostic. `recv` must be cancel-safe
/// and may run concurrently with a send.
#[async_trait]
pub trait LiveConnection: Send + Sync {
    /// Sends the earlier conversation turns to the model.
    async fn send_history(&self, history: &[Content]) -> Result<(), LiveConnError>;
    /// Sends one content turn.
    async fn send_content(&self, content: Content) -> Result<(), LiveConnError>;
    /// Sends realtime input (audio or image data, or an activity marker).
    async fn send_realtime(&self, input: RealtimeInput) -> Result<(), LiveConnError>;
    /// Receives the next response. `Ok(None)`: the stream ended without a close frame (callers
    /// treat it as [`LiveConnError::eof`]). A received close frame is `Err` with kind
    /// `Closed { code }`.
    async fn recv(&self) -> Result<Option<LlmResponse>, LiveConnError>;
    /// Closes the connection.
    async fn close(&self) -> Result<(), LiveConnError>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::any::Any;
    use std::error::Error;

    use futures::stream::BoxStream;
    use gadk_genai::LiveConnectConfig;

    use super::{LiveConnError, LiveConnErrorKind};
    use crate::{Llm, LlmRequest, LlmResponse, ModelError};

    struct NoLive;

    impl Llm for NoLive {
        fn name(&self) -> &str {
            "no-live"
        }
        fn generate_content(
            &self,
            _req: LlmRequest,
            _stream: bool,
        ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
            Box::pin(futures::stream::empty())
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[tokio::test]
    async fn live_default_connect_live_is_unsupported() {
        let m: &dyn Llm = &NoLive;
        let Err(err) = m.connect_live(LiveConnectConfig::default()).await else {
            panic!("the default connect_live must fail");
        };
        assert!(matches!(err, ModelError::Other(_)), "{err:?}");
        assert_eq!(err.to_string(), "model does not support live connection");
    }

    fn other(msg: &str) -> LiveConnError {
        LiveConnError::new(LiveConnErrorKind::Other, msg)
    }

    #[test]
    fn live_is_resumable_follows_go_is_resumable() {
        let cases: Vec<(&str, LiveConnError, bool)> = vec![
            ("eof", LiveConnError::eof(), true),
            (
                "connection lost",
                LiveConnError::new(
                    LiveConnErrorKind::ConnectionLost,
                    "failed to send message: IO error: x",
                ),
                true,
            ),
            (
                "close 1008",
                LiveConnError::new(
                    LiveConnErrorKind::Closed { code: 1008 },
                    "websocket: close 1008 (policy violation)",
                ),
                true,
            ),
            (
                "close 1002",
                LiveConnError::new(
                    LiveConnErrorKind::Closed { code: 1002 },
                    "websocket: close 1002 (protocol error): fatal",
                ),
                false,
            ),
            // Go falls through to the text rule after a non-1008 close error.
            (
                "close 1002 with EOF in reason",
                LiveConnError::new(
                    LiveConnErrorKind::Closed { code: 1002 },
                    "websocket: close 1002 (protocol error): unexpected EOF",
                ),
                true,
            ),
            (
                "close 1002 with 1008 in reason",
                LiveConnError::new(
                    LiveConnErrorKind::Closed { code: 1002 },
                    "websocket: close 1002 (protocol error): error 1008",
                ),
                false,
            ),
            ("broken pipe text", other("write: broken pipe"), true),
            (
                "connection reset text",
                other("read: connection reset by peer"),
                true,
            ),
            (
                "EOF text",
                other(
                    "failed to receive message: websocket: close 1006 (abnormal closure): unexpected EOF",
                ),
                true,
            ),
            ("GoAway text", other("GoAway received"), true),
            (
                "1008 in a port is not a code",
                other(
                    "failed to receive message: read tcp 127.0.0.1:41008->127.0.0.1:42207: i/o timeout",
                ),
                false,
            ),
            (
                "application error",
                other("model refused the request"),
                false,
            ),
        ];
        for (name, err, want) in cases {
            assert_eq!(err.is_resumable(), want, "{name}: {err}");
        }
    }

    #[test]
    fn live_display_is_message_and_source_is_kept() {
        let src = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe");
        let err = LiveConnError::new(
            LiveConnErrorKind::ConnectionLost,
            "failed to send message: pipe",
        )
        .with_source(src);
        assert_eq!(err.to_string(), "failed to send message: pipe");
        assert_eq!(err.kind, LiveConnErrorKind::ConnectionLost);
        let source = err.source().expect("source kept");
        assert!(source.downcast_ref::<std::io::Error>().is_some());
        let eof = LiveConnError::eof();
        assert_eq!(
            (eof.kind, eof.message.as_str()),
            (LiveConnErrorKind::ConnectionLost, "EOF")
        );
        assert!(eof.source.is_none());
    }
}
