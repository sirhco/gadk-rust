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

//! Port of Go `controllers/runtime.go` `RunLiveHandler` (`/run_live` over WebSocket) with
//! `acquireLiveSlot`, `applyLiveConnLimits`, `liveDeadline` and `truncateCloseReason`, and of
//! the parts of gorilla/websocket's `Upgrader` and `Conn` whose behavior a client sees: the
//! handshake checks and their error responses, the read limit's 1009 close, the close reply, and
//! read/write deadlines (here `tokio::time` timeouts around each read and write).

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::FromRequestParts;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use base64::Engine as _;
use bytes::Bytes;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use gadk_agent::{EventStream, LiveRequest as AgentLiveRequest, LiveRunConfig, LiveSession};
use gadk_engine::{RunOptions, Runner};
use gadk_genai::{AudioTranscriptionConfig, Blob, Modality};
use gadk_model::RealtimeInput;
use http::header::{CONNECTION, HOST, ORIGIN, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION, UPGRADE};
use http::{HeaderMap, HeaderValue, Method, StatusCode, request::Parts};
use tokio::sync::{Mutex, OwnedSemaphorePermit};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use super::runtime::RuntimeApiController;
use crate::authn::Caller;
use crate::authz;
use crate::rest::http_error::http_error;
use crate::rest::models::{Event as ApiEvent, LiveRequest as ApiLiveRequest, RunAgentRequest};
use crate::rest::routing::{Request, Response};

/// Go `maxCloseReason`: the longest close reason a control frame carries (a 125-byte payload
/// less the two-byte code).
pub(crate) const MAX_CLOSE_REASON: usize = 123;

/// gorilla `writeWait`: the deadline of the read-limit close frame and of the close reply.
const WRITE_WAIT: Duration = Duration::from_secs(1);
/// How long Go's `sendClose` waits for the client's close reply.
const CLOSE_DRAIN: Duration = Duration::from_secs(1);
/// The MIME type Go gives a binary client frame.
const BINARY_AUDIO_MIME_TYPE: &str = "audio/pcm;rate=16000";
/// gorilla `CloseNormalClosure`.
const CLOSE_NORMAL_CLOSURE: u16 = 1000;
/// gorilla `CloseGoingAway`.
const CLOSE_GOING_AWAY: u16 = 1001;
/// gorilla `CloseNoStatusReceived`: a close frame without a code.
const CLOSE_NO_STATUS_RECEIVED: u16 = 1005;
/// gorilla `CloseMessageTooBig`.
const CLOSE_MESSAGE_TOO_BIG: u16 = 1009;
/// gorilla `CloseInternalServerErr`.
const CLOSE_INTERNAL_SERVER_ERR: u16 = 1011;
/// tungstenite's `Error::Capacity` display prefix: the message or frame exceeded the read limit.
const READ_LIMIT_ERROR_PREFIX: &str = "Space limit exceeded";

impl RuntimeApiController {
    /// Go `RunLiveHandler` (through `NewErrorHandler`): upgrades to a WebSocket and streams a
    /// live agent session.
    pub async fn run_live_handler(&self, req: Request) -> Response {
        let (mut parts, _body) = req.into_parts();

        let query = parse_query(parts.uri.query());
        let app_name = query_get(&query, "appName", "app_name");
        let user_id = query_get(&query, "userId", "user_id");

        if let Err(e) = self
            .authorizer
            .can_act_as_user(parts.extensions.get::<Caller>(), &user_id)
        {
            return authz::write_http_status_for_auth_error(Some(&e));
        }

        let session_id = query_get(&query, "sessionId", "session_id");
        if app_name.is_empty() || user_id.is_empty() || session_id.is_empty() {
            return http_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "appName, userId, and sessionId are required",
            );
        }

        // Before the upgrade, so a refusal is an ordinary HTTP response the client can read
        // rather than a close frame on a connection it just won.
        let Ok(slot) = self.acquire_live_slot() else {
            return http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!("live session limit of {} reached", self.live_slot_cap),
            );
        };

        if let Some((status, reason)) = self.check_handshake(&parts) {
            return upgrade_error(status, reason);
        }
        let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
            // gorilla's Upgrader fails the same way when the connection cannot be hijacked.
            return upgrade_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "websocket: response does not implement http.Hijacker",
            );
        };
        // The upgrade bypasses MaxBytesMiddleware; negative turns the limit off.
        let read_limit = usize::try_from(self.max_live_message_bytes)
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(usize::MAX);
        let upgrade = upgrade
            .max_message_size(read_limit)
            .max_frame_size(read_limit);

        let live = LiveConn {
            runner: self.live_runner(&app_name),
            app_name,
            user_id,
            session_id,
            keepalive: self.live_keepalive_timeout,
            ping_interval: self.live_ping_interval,
            slot,
        };
        upgrade.on_upgrade(move |ws| live.serve(ws))
    }

    /// Go `acquireLiveSlot`: one of the controller's concurrent live-session slots (`None`
    /// when the controller has no cap), or `Err` when they are all held. Dropping the permit
    /// gives the slot back.
    fn acquire_live_slot(&self) -> Result<Option<OwnedSemaphorePermit>, ()> {
        match &self.live_slots {
            None => Ok(None),
            Some(slots) => slots.clone().try_acquire_owned().map(Some).map_err(|_| ()),
        }
    }

    /// gorilla `Upgrader.Upgrade`'s handshake checks, in its order: the status and reason of
    /// the first that fails.
    fn check_handshake(&self, parts: &Parts) -> Option<(StatusCode, &'static str)> {
        let h = &parts.headers;
        if !token_list_contains_value(h, CONNECTION.as_str(), "upgrade") {
            return Some((
                StatusCode::BAD_REQUEST,
                "websocket: the client is not using the websocket protocol: 'upgrade' token not found in 'Connection' header",
            ));
        }
        if !token_list_contains_value(h, UPGRADE.as_str(), "websocket") {
            return Some((
                StatusCode::BAD_REQUEST,
                "websocket: the client is not using the websocket protocol: 'websocket' token not found in 'Upgrade' header",
            ));
        }
        if parts.method != Method::GET {
            return Some((
                StatusCode::METHOD_NOT_ALLOWED,
                "websocket: the client is not using the websocket protocol: request method is not GET",
            ));
        }
        if !token_list_contains_value(h, SEC_WEBSOCKET_VERSION.as_str(), "13") {
            return Some((
                StatusCode::BAD_REQUEST,
                "websocket: unsupported version: 13 not found in 'Sec-Websocket-Version' header",
            ));
        }
        let origin_ok = match &self.check_origin {
            Some(check) => check(parts),
            None => check_same_origin(parts),
        };
        if !origin_ok {
            return Some((
                StatusCode::FORBIDDEN,
                "websocket: request origin not allowed by Upgrader.CheckOrigin",
            ));
        }
        let key = h
            .get(SEC_WEBSOCKET_KEY)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if !is_valid_challenge_key(key) {
            return Some((
                StatusCode::BAD_REQUEST,
                "websocket: not a websocket handshake: 'Sec-WebSocket-Key' header must be Base64 encoded value of 16-byte in length",
            ));
        }
        None
    }

    /// Go `getRunner` for a live run, plus `RunLiveHandler`'s close reason: `Err` holds the
    /// error to log and the reason (`agent … not found for original error: …` when the agent
    /// cannot be loaded, else the error itself).
    fn live_runner(&self, app_name: &str) -> Result<Runner, (String, String)> {
        let req = RunAgentRequest {
            app_name: app_name.to_owned(),
            ..Default::default()
        };
        self.get_runner(&req)
            .map(|(runner, _)| runner)
            .map_err(|e| {
                let err = e.to_string();
                let loads = self
                    .agent_loader
                    .as_ref()
                    .is_some_and(|l| l.load_agent(app_name).is_ok());
                let reason = if loads {
                    err.clone()
                } else {
                    format!("agent {app_name} not found for original error: {err}")
                };
                (err, reason)
            })
    }
}

/// The response of a failed upgrade: gorilla's `returnError` (`Sec-Websocket-Version: 13` and
/// `http.Error` with the status text), then `NewErrorHandler`'s `http.Error` of the returned
/// error, which appends its line to the body already written.
fn upgrade_error(status: StatusCode, reason: &str) -> Response {
    let text = status.canonical_reason().unwrap_or_default();
    let mut resp = http_error(
        status,
        &format!("{text}\nfailed to upgrade to websocket: {reason}"),
    );
    resp.headers_mut()
        .insert(SEC_WEBSOCKET_VERSION, HeaderValue::from_static("13"));
    resp
}

/// Go `url.Values` of the request's query.
fn parse_query(query: Option<&str>) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// Go `q.Get(name)`, falling back to `q.Get(alt)` when that is empty.
fn query_get(query: &[(String, String)], name: &str, alt: &str) -> String {
    let get = |key: &str| {
        query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let v = get(name);
    if v.is_empty() { get(alt) } else { v }
}

/// gorilla `tokenListContainsValue`: whether a comma-separated token list in any `name` header
/// holds `value` (ASCII case-insensitive). A header value stops counting at the first item that
/// is not a single token.
fn token_list_contains_value(headers: &HeaderMap, name: &str, value: &str) -> bool {
    headers.get_all(name).iter().any(|v| {
        let Ok(s) = std::str::from_utf8(v.as_bytes()) else {
            return false;
        };
        for item in s.split(',') {
            let t = item.trim_matches(|c| c == ' ' || c == '\t');
            if t.is_empty() || !t.bytes().all(is_token_octet) {
                return false;
            }
            if t.eq_ignore_ascii_case(value) {
                return true;
            }
        }
        false
    })
}

/// RFC 7230 `tchar`.
fn is_token_octet(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// gorilla `checkSameOrigin`: no `Origin` header, or its host equals the request's `Host`
/// (ASCII case-insensitive).
fn check_same_origin(parts: &Parts) -> bool {
    let Some(origin) = parts.headers.get(ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let host = parts
        .headers
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned)
        .or_else(|| parts.uri.authority().map(|a| a.as_str().to_owned()))
        .unwrap_or_default();
    url_host(origin).eq_ignore_ascii_case(&host)
}

/// Go `url.Parse(s).Host`: the authority after `scheme://`, without user info; empty when `s`
/// has no authority.
fn url_host(s: &str) -> &str {
    let Some(colon) = s.find(':') else {
        return "";
    };
    let Some(rest) = s[colon + 1..].strip_prefix("//") else {
        return "";
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority.rsplit_once('@').map_or(authority, |(_, h)| h)
}

/// gorilla `isValidChallengeKey`: standard base64 of 16 bytes.
fn is_valid_challenge_key(key: &str) -> bool {
    !key.is_empty()
        && base64::engine::general_purpose::STANDARD
            .decode(key)
            .is_ok_and(|d| d.len() == 16)
}

/// Go `truncateCloseReason`: `reason` cut to [`MAX_CLOSE_REASON`] bytes on a UTF-8 boundary.
pub(crate) fn truncate_close_reason(reason: &str) -> &str {
    if reason.len() <= MAX_CLOSE_REASON {
        return reason;
    }
    let mut end = MAX_CLOSE_REASON;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    &reason[..end]
}

/// gorilla `FormatCloseMessage` (with the reason truncated as Go's handler does).
fn close_message(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: truncate_close_reason(reason).into(),
    }))
}

/// gorilla `CloseError.Error()`: how a client's close frame reads in Go's log line.
fn close_error_text(code: u16, text: &str) -> String {
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
    if text.is_empty() {
        format!("websocket: close {code}{name}")
    } else {
        format!("websocket: close {code}{name}: {text}")
    }
}

/// `Some(f)` before `deadline` (or without one), `None` once it passes.
async fn with_deadline<F: Future>(deadline: Option<Instant>, f: F) -> Option<F::Output> {
    match deadline {
        Some(d) => tokio::time::timeout_at(d, f).await.ok(),
        None => Some(f.await),
    }
}

/// Why a write on a live connection failed (the texts stand in for Go's net errors).
#[derive(Debug)]
enum WriteError {
    /// gorilla `ErrCloseSent`: a close frame already went out.
    CloseSent,
    /// The connection was closed (Go's `use of closed network connection`).
    Closed,
    /// The write deadline passed.
    Timeout,
    /// The encoder or transport failed.
    Other(String),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::CloseSent => f.write_str("websocket: close sent"),
            WriteError::Closed => f.write_str("use of closed network connection"),
            WriteError::Timeout => f.write_str("i/o timeout"),
            WriteError::Other(e) => f.write_str(e),
        }
    }
}

/// The write half of a live WebSocket, shared by the event loop, the pinger and the reader
/// (gorilla allows one writer at a time; a write waits for the previous one within its own
/// deadline, as `WriteControl` does).
struct Conn {
    sink: Mutex<SplitSink<WebSocket, Message>>,
    /// Go `ws.Close()`: every pending and later read or write on the connection fails.
    closed: CancellationToken,
    /// gorilla `ErrCloseSent`: a close frame (ours or the reply to the client's) went out.
    close_sent: AtomicBool,
    /// `None`: the keepalive is off and no read or write has a deadline.
    keepalive: Option<Duration>,
}

impl Conn {
    /// Go `liveDeadline`: the keepalive timeout from now, or no deadline.
    fn deadline(&self) -> Option<Instant> {
        self.keepalive.map(|t| Instant::now() + t)
    }

    /// Go `ws.Close()`.
    fn close(&self) {
        self.closed.cancel();
    }

    /// One message out, within `deadline`.
    async fn write(&self, msg: Message, deadline: Option<Instant>) -> Result<(), WriteError> {
        if self.close_sent.load(Ordering::SeqCst) {
            return Err(WriteError::CloseSent);
        }
        let is_close = matches!(msg, Message::Close(_));
        let send = async {
            let mut sink = self.sink.lock().await;
            sink.send(msg).await
        };
        let res = tokio::select! {
            biased;
            () = self.closed.cancelled() => Err(WriteError::Closed),
            r = with_deadline(deadline, send) => match r {
                None => Err(WriteError::Timeout),
                Some(Err(e)) => Err(WriteError::Other(e.to_string())),
                Some(Ok(())) => Ok(()),
            },
        };
        if is_close && res.is_ok() {
            self.close_sent.store(true, Ordering::SeqCst);
        }
        res
    }

    /// gorilla's default close handler: the reply tungstenite queued for the client's close
    /// frame goes out now, within [`WRITE_WAIT`].
    async fn send_close_reply(&self) {
        let flush = async {
            let mut sink = self.sink.lock().await;
            sink.flush().await
        };
        tokio::select! {
            biased;
            () = self.closed.cancelled() => {}
            _ = tokio::time::timeout(WRITE_WAIT, flush) => {}
        }
        self.close_sent.store(true, Ordering::SeqCst);
    }
}

/// One accepted `/run_live` request, carried into the upgraded connection.
struct LiveConn {
    app_name: String,
    user_id: String,
    session_id: String,
    runner: Result<Runner, (String, String)>,
    keepalive: Option<Duration>,
    ping_interval: Option<Duration>,
    /// Released after the connection's last task ends.
    slot: Option<OwnedSemaphorePermit>,
}

impl LiveConn {
    /// The upgraded half of Go `RunLiveHandler`. Returns once the session is over; the slot
    /// is released and the socket dropped (closed) last.
    async fn serve(self, ws: WebSocket) {
        let LiveConn {
            app_name,
            user_id,
            session_id,
            runner,
            keepalive,
            ping_interval,
            slot,
        } = self;
        let (sink, stream) = ws.split();
        let conn = Arc::new(Conn {
            sink: Mutex::new(sink),
            closed: CancellationToken::new(),
            close_sent: AtomicBool::new(false),
            keepalive,
        });

        let (stop_pings, peer_gone) = apply_live_conn_limits(&conn, ping_interval);
        run_live(
            &conn,
            stream,
            runner,
            &app_name,
            &user_id,
            &session_id,
            &peer_gone,
        )
        .await;
        stop_pings.cancel();
        conn.close();
        drop(slot);
    }
}

/// Go `applyLiveConnLimits`' keepalive (the read limit is set on the upgrade): pings every
/// `interval` from a task of their own, each within the keepalive deadline. Returns the token
/// that stops the pinger and the one cancelled once a ping cannot be written in time (Go
/// `peerGone`; never cancelled when the keepalive is off).
fn apply_live_conn_limits(
    conn: &Arc<Conn>,
    interval: Option<Duration>,
) -> (CancellationToken, CancellationToken) {
    let stop = CancellationToken::new();
    let peer_gone = CancellationToken::new();
    let Some(interval) = interval else {
        return (stop, peer_gone);
    };
    let (conn, stop_rx, gone) = (conn.clone(), stop.clone(), peer_gone.clone());
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval_at(Instant::now() + interval, interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                () = stop_rx.cancelled() => return,
                _ = ticker.tick() => {
                    // The same deadline as an event write, so a slow event write that finishes
                    // within its own deadline does not fail a queued ping.
                    if conn.write(Message::Ping(Bytes::new()), conn.deadline()).await.is_err() {
                        conn.close();
                        gone.cancel();
                        return;
                    }
                }
            }
        }
    });
    (stop, peer_gone)
}

/// Go `sendClose`: the close frame, then the client's messages until its close reply, an error
/// or [`CLOSE_DRAIN`] (pongs do not extend it).
async fn send_close(conn: &Conn, stream: &mut SplitStream<WebSocket>, code: u16, reason: &str) {
    let _ = conn.write(close_message(code, reason), None).await;
    let deadline = Instant::now() + CLOSE_DRAIN;
    loop {
        let next = tokio::select! {
            biased;
            () = conn.closed.cancelled() => return,
            r = tokio::time::timeout_at(deadline, stream.next()) => r,
        };
        match next {
            Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => return,
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// Go `RunLiveHandler` from the runner on: the session, the reader task and the event loop.
async fn run_live(
    conn: &Arc<Conn>,
    mut stream: SplitStream<WebSocket>,
    runner: Result<Runner, (String, String)>,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    peer_gone: &CancellationToken,
) {
    let runner = match runner {
        Ok(r) => r,
        Err((err, reason)) => {
            tracing::warn!(target: "adk", "Failed to get runner for app {app_name}: {err}");
            send_close(conn, &mut stream, CLOSE_INTERNAL_SERVER_ERR, &reason).await;
            return;
        }
    };

    let cfg = LiveRunConfig {
        max_llm_calls: 100,
        response_modalities: vec![Modality::Audio],
        input_audio_transcription: Some(AudioTranscriptionConfig::default()),
        output_audio_transcription: Some(AudioTranscriptionConfig::default()),
        ..Default::default()
    };
    let (session, mut events) = match runner
        .run_live(user_id, session_id, cfg, RunOptions::default())
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "adk", "RunLive failed for app {app_name}: {e}");
            send_close(conn, &mut stream, CLOSE_INTERNAL_SERVER_ERR, &e.to_string()).await;
            return;
        }
    };

    // Once the keepalive gives up, neither a reader blocked in send nor the loop below waiting
    // on the agent would see the socket close: closing the session returns both.
    let done = CancellationToken::new();
    let watcher = {
        let (done, gone, session) = (done.clone(), peer_gone.clone(), session.clone());
        tokio::spawn(async move {
            tokio::select! {
                biased;
                () = done.cancelled() => {}
                () = gone.cancelled() => { let _ = session.close().await; }
            }
        })
    };

    let reader = tokio::spawn(read_client(
        conn.clone(),
        stream,
        session.clone(),
        app_name.to_owned(),
    ));

    write_events(conn, &mut events, app_name).await;

    // Go's deferred calls, last registered first: the AfterFunc's stop, the session's Close,
    // the pinger's stop (the caller), ws.Close.
    done.cancel();
    let _ = watcher.await;
    drop(events);
    let _ = session.close().await;
    conn.close();
    let _ = reader.await;
}

/// The event loop of Go `RunLiveHandler`: each event as a JSON text frame, an iterator error as
/// a 1011 close frame; the first failure ends it.
async fn write_events(conn: &Conn, events: &mut EventStream, app_name: &str) {
    while let Some(item) = events.next().await {
        // A peer that accepts nothing for the keepalive timeout has stopped responding too,
        // even if it keeps sending messages or pongs that extend the read deadline.
        let deadline = conn.deadline();
        let event = match item {
            Ok(event) => event,
            Err(e) => {
                tracing::warn!(target: "adk", "RunLive failed: {e}");
                let reason = e.to_string();
                let _ = conn
                    .write(close_message(CLOSE_INTERNAL_SERVER_ERR, &reason), deadline)
                    .await;
                break;
            }
        };
        let res = match encode_event(&event) {
            Ok(text) => conn.write(Message::Text(text.into()), deadline).await,
            Err(e) => Err(WriteError::Other(e)),
        };
        if let Err(e) = res {
            if !matches!(e, WriteError::CloseSent) {
                tracing::warn!(target: "adk", "WebSocket write error for app {app_name}: {e}");
            }
            break;
        }
    }
}

/// gorilla `WriteJSON` of Go `models.FromSessionEvent`: `json.Encoder` output, newline included.
fn encode_event(event: &gadk_session::Event) -> Result<String, String> {
    let mut json =
        gadk_model::to_go_json(&ApiEvent::from_session_event(event)).map_err(|e| e.to_string())?;
    json.push(b'\n');
    String::from_utf8(json).map_err(|e| e.to_string())
}

/// The reader goroutine of Go `RunLiveHandler`: client frames to the live session until the
/// client leaves, asks to close, or a send fails; then closes the session.
async fn read_client(
    conn: Arc<Conn>,
    mut stream: SplitStream<WebSocket>,
    session: Arc<dyn LiveSession>,
    app_name: String,
) {
    'frames: loop {
        // Armed only while waiting on the client, so neither session setup nor a slow hand-off
        // to the agent counts against a peer that answers every ping.
        let mut deadline = conn.deadline();
        let msg = loop {
            let next = tokio::select! {
                biased;
                () = conn.closed.cancelled() => {
                    tracing::warn!(target: "adk", "WebSocket read error for app {app_name}: use of closed network connection");
                    break 'frames;
                }
                r = with_deadline(deadline, stream.next()) => r,
            };
            match next {
                None => {
                    tracing::warn!(target: "adk", "WebSocket read error for app {app_name}: i/o timeout");
                    conn.close();
                    break 'frames;
                }
                Some(None) => {
                    tracing::warn!(target: "adk", "WebSocket read error for app {app_name}: unexpected EOF");
                    conn.close();
                    break 'frames;
                }
                Some(Some(Err(e))) => {
                    let e = e.to_string();
                    if e.starts_with(READ_LIMIT_ERROR_PREFIX) {
                        let _ = conn
                            .write(
                                close_message(CLOSE_MESSAGE_TOO_BIG, ""),
                                Some(Instant::now() + WRITE_WAIT),
                            )
                            .await;
                        tracing::warn!(target: "adk", "WebSocket read error for app {app_name}: websocket: read limit exceeded");
                    } else {
                        tracing::warn!(target: "adk", "WebSocket read error for app {app_name}: {e}");
                    }
                    conn.close();
                    break 'frames;
                }
                Some(Some(Ok(Message::Close(frame)))) => {
                    conn.send_close_reply().await;
                    let (code, text) = frame
                        .map(|f| (f.code, f.reason.to_string()))
                        .unwrap_or((CLOSE_NO_STATUS_RECEIVED, String::new()));
                    if code != CLOSE_NORMAL_CLOSURE && code != CLOSE_GOING_AWAY {
                        tracing::warn!(target: "adk", "WebSocket read error for app {app_name}: {}", close_error_text(code, &text));
                    }
                    conn.close();
                    break 'frames;
                }
                // The keepalive's pong handler pushes the read deadline out.
                Some(Some(Ok(Message::Pong(_)))) => deadline = conn.deadline(),
                // tungstenite queues the pong itself (gorilla's default ping handler).
                Some(Some(Ok(Message::Ping(_)))) => {}
                Some(Some(Ok(m))) => break m,
            }
        };

        let (req, binary) = match msg {
            Message::Binary(data) => (
                AgentLiveRequest {
                    realtime_input: Some(RealtimeInput::Blob(Blob {
                        mime_type: BINARY_AUDIO_MIME_TYPE.to_owned(),
                        data,
                        ..Default::default()
                    })),
                    content: None,
                },
                true,
            ),
            Message::Text(text) => {
                let api: ApiLiveRequest = match serde_json::from_str(text.as_str()) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(target: "adk", "Failed to unmarshal client message for app {app_name}: {e}");
                        continue;
                    }
                };
                if api.close {
                    break;
                }
                (live_request(api), false)
            }
            _ => continue,
        };
        let sent = tokio::select! {
            biased;
            () = conn.closed.cancelled() => break,
            r = session.send(req) => r,
        };
        if let Err(e) = sent {
            if binary {
                tracing::warn!(target: "adk", "Failed to send binary data to Gemini for app {app_name}: {e}");
            } else {
                tracing::warn!(target: "adk", "Failed to send message to Gemini for app {app_name}: {e}");
            }
            break;
        }
    }
    let _ = session.close().await;
}

/// Go's mapping of a client `models.LiveRequest` onto `agent.LiveRequest`: the content, and
/// the first of activity start, activity end and blob as the realtime input.
fn live_request(api: ApiLiveRequest) -> AgentLiveRequest {
    let realtime_input = if api.activity_start.is_some() {
        Some(RealtimeInput::ActivityStart)
    } else if api.activity_end.is_some() {
        Some(RealtimeInput::ActivityEnd)
    } else {
        api.blob.map(|b| {
            RealtimeInput::Blob(Blob {
                mime_type: b.mime_type,
                data: b.data,
                ..Default::default()
            })
        })
    };
    AgentLiveRequest {
        realtime_input,
        content: api.content,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn handshake_token_lists_follow_gorilla() {
        let mut h = HeaderMap::new();
        h.append(CONNECTION, HeaderValue::from_static("keep-alive, Upgrade"));
        assert!(token_list_contains_value(&h, "connection", "upgrade"));
        let mut h = HeaderMap::new();
        h.append(CONNECTION, HeaderValue::from_static("keep alive, upgrade"));
        assert!(!token_list_contains_value(&h, "connection", "upgrade"));
        h.append(CONNECTION, HeaderValue::from_static("upgrade"));
        assert!(token_list_contains_value(&h, "connection", "upgrade"));
    }

    #[test]
    fn url_host_matches_go_url_parse() {
        assert_eq!(url_host("http://Example.com:8080/x"), "Example.com:8080");
        assert_eq!(url_host("https://user@host"), "host");
        assert_eq!(url_host("null"), "");
    }

    #[test]
    fn challenge_key_must_decode_to_16_bytes() {
        assert!(is_valid_challenge_key("dGhlIHNhbXBsZSBub25jZQ=="));
        assert!(!is_valid_challenge_key("abc"));
        assert!(!is_valid_challenge_key(""));
    }

    #[test]
    fn close_error_text_matches_gorilla() {
        assert_eq!(
            close_error_text(1002, "bad"),
            "websocket: close 1002 (protocol error): bad"
        );
        assert_eq!(close_error_text(4000, ""), "websocket: close 4000");
    }
}
