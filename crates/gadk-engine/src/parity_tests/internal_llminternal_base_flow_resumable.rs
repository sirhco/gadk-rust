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

//! Parity port of Go internal/llminternal/base_flow_resumable_test.go.
//!
//! Go classifies with `isResumable(err error)`; gadk classifies when the error is produced: the
//! Gemini transport turns tungstenite errors and close frames into a `LiveConnError` (kind +
//! full text), and `LiveConnError::is_resumable` applies Go's rules. Each Go error shape is
//! rebuilt through the production constructors in `gadk_gemini::internal`.

use std::io;

use gadk_gemini::internal::tungstenite::Error as WsError;
use gadk_gemini::internal::tungstenite::error::ProtocolError;
use gadk_gemini::internal::tungstenite::protocol::CloseFrame;
use gadk_gemini::internal::tungstenite::protocol::frame::coding::CloseCode;
use gadk_gemini::internal::{classify_ws_error, close_frame_error, wrap_conn_error};
use gadk_model::{LiveConnError, LiveConnErrorKind};

/// Go socketErr: a failed read or write on a live socket, as tungstenite reports it.
fn socket_err(context: &str, e: io::Error) -> LiveConnError {
    classify_ws_error(context, WsError::Io(e))
}

/// Windows WSA error numbers (Go wsaeConnAborted / wsaeConnReset), spelled numerically.
const WSAE_CONN_ABORTED: i32 = 10053;
const WSAE_CONN_RESET: i32 = 10054;

fn close(code: u16, reason: &str) -> LiveConnError {
    close_frame_error(
        "failed to receive message",
        Some(&CloseFrame {
            code: CloseCode::from(code),
            reason: reason.into(),
        }),
    )
}

fn text(msg: &str) -> LiveConnError {
    LiveConnError::new(LiveConnErrorKind::Other, msg)
}

// parity: adapted: Go's nil case has no Rust value (errors are not optional here); the *net.OpError cases are tungstenite Io errors classified ConnectionLost; the crypto/tls-wrapped wss write error is the io::Error tokio-rustls returns for a failed write
#[test]
fn test_is_resumable() {
    let cases: Vec<(&str, LiveConnError, bool)> = vec![
        ("io.EOF", LiveConnError::eof(), true),
        // The reader path.
        (
            "reader: abnormal closure 1006",
            text(
                "failed to receive message: websocket: close 1006 (abnormal closure): unexpected EOF",
            ),
            true,
        ),
        (
            "reader: abnormal closure as tungstenite reports it",
            classify_ws_error(
                "failed to receive message",
                WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake),
            ),
            true,
        ),
        ("reader: policy violation 1008", close(1008, ""), true),
        ("reader: GoAway", text("GoAway received"), true),
        // The sender path.
        (
            "sender: linux EPIPE",
            socket_err("failed to send message", io::ErrorKind::BrokenPipe.into()),
            true,
        ),
        (
            "sender: linux ECONNRESET",
            socket_err(
                "failed to send message",
                io::ErrorKind::ConnectionReset.into(),
            ),
            true,
        ),
        (
            "sender: windows WSAECONNABORTED",
            socket_err(
                "failed to send message",
                io::Error::from_raw_os_error(WSAE_CONN_ABORTED),
            ),
            true,
        ),
        (
            "sender: windows WSAECONNRESET",
            socket_err(
                "failed to send message",
                io::Error::from_raw_os_error(WSAE_CONN_RESET),
            ),
            true,
        ),
        (
            "reader: windows WSAECONNRESET on recv",
            socket_err(
                "failed to receive message",
                io::Error::from_raw_os_error(WSAE_CONN_RESET),
            ),
            true,
        ),
        (
            "sender: SendContent wrapper around a socket error",
            wrap_conn_error(
                "failed to send content",
                socket_err(
                    "failed to send message",
                    io::Error::from_raw_os_error(WSAE_CONN_ABORTED),
                ),
            ),
            true,
        ),
        (
            "sender: wss write error wrapped by the TLS layer",
            socket_err(
                "failed to send message",
                io::Error::other("tls write failed: connection aborted"),
            ),
            true,
        ),
        // Errors about the exchange rather than the transport terminate the flow.
        (
            "protocol error 1002 is fatal",
            text("websocket: close 1002 (protocol error): fatal"),
            false,
        ),
        (
            "protocol error 1002 close frame is fatal",
            close(1002, "fatal"),
            false,
        ),
        (
            "timeout with 1008 in a port is fatal",
            text(
                "failed to receive message: read tcp 127.0.0.1:41008->127.0.0.1:42207: i/o timeout",
            ),
            false,
        ),
        (
            "close frame with 1008 only in its reason is fatal",
            close(1002, "error 1008"),
            false,
        ),
        (
            "application error is fatal",
            text("model refused the request"),
            false,
        ),
    ];
    for (name, err, want) in cases {
        assert_eq!(err.is_resumable(), want, "isResumable({err}) [{name}]");
    }
    // The close-1002 error carries its code in its text, as gorilla's does.
    assert!(close(1002, "fatal").to_string().contains("1002"));
}

// parity: adapted: the reader's dropped-connection error is tungstenite's ResetWithoutClosingHandshake (plus Go's gorilla 1006 text) and the sender's is a tungstenite Io error, rather than gorilla and *net.OpError values
#[test]
fn test_is_resumable_agrees_across_reader_and_sender() {
    let reader_text =
        "failed to receive message: websocket: close 1006 (abnormal closure): unexpected EOF";
    let platforms: Vec<(&str, Vec<LiveConnError>, LiveConnError)> = vec![
        (
            "linux",
            vec![
                text(reader_text),
                classify_ws_error(
                    "failed to receive message",
                    WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake),
                ),
            ],
            socket_err("failed to send message", io::ErrorKind::BrokenPipe.into()),
        ),
        (
            "windows",
            vec![
                text(reader_text),
                classify_ws_error(
                    "failed to receive message",
                    WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake),
                ),
            ],
            socket_err(
                "failed to send message",
                io::Error::from_raw_os_error(WSAE_CONN_ABORTED),
            ),
        ),
    ];
    for (name, readers, sender_err) in platforms {
        let sender = sender_err.is_resumable();
        for reader_err in readers {
            let reader = reader_err.is_resumable();
            assert_eq!(
                reader, sender,
                "{name}: same connection loss classified differently: reader={reader} sender={sender}"
            );
            assert!(
                reader,
                "{name}: a dropped connection should be resumable, got reader={reader} sender={sender}"
            );
        }
    }
}
