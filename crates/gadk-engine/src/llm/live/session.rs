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

//! Port of Go internal/llminternal/base_flow.go `liveSessionImpl` (lines 199-320).

use std::any::Any;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context as TaskContext, Poll};

use async_trait::async_trait;
use futures::Stream;
use gadk_agent::{AgentError, EventStream, LiveRequest, LiveSession};
use gadk_session::Event;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::llm::live::audio_cache::AudioCacheManager;

type InputMsg = (LiveRequest, oneshot::Sender<()>);
type OutputMsg = (Result<Event, AgentError>, oneshot::Sender<()>);

/// Task accounting for tests (Go's tests count goroutine stacks instead).
#[derive(Debug, Default)]
pub(crate) struct LiveProbe {
    /// Tasks spawned by the live flow that are still alive.
    pub(crate) live_tasks: AtomicUsize,
    /// Reconnect waits in progress.
    pub(crate) in_backoff: AtomicUsize,
}

impl LiveProbe {
    /// Counts one live task until the guard is dropped.
    pub(crate) fn task_guard(self: &Arc<Self>) -> LiveTaskGuard {
        self.live_tasks.fetch_add(1, Ordering::SeqCst);
        LiveTaskGuard(self.clone())
    }
}

/// Decrements [`LiveProbe::live_tasks`] on drop (including when the task is aborted).
pub(crate) struct LiveTaskGuard(Arc<LiveProbe>);

impl Drop for LiveTaskGuard {
    fn drop(&mut self) {
        self.0.live_tasks.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Go activeTask.
struct ActiveTask {
    call_id: String,
    cancel: CancellationToken,
}

/// Port of Go liveSessionImpl: the live flow's session.
pub(crate) struct LiveSessionImpl {
    input_tx: mpsc::Sender<InputMsg>,
    input_rx: tokio::sync::Mutex<mpsc::Receiver<InputMsg>>,
    output_tx: mpsc::Sender<OutputMsg>,
    output_rx: parking_lot::Mutex<Option<mpsc::Receiver<OutputMsg>>>,
    done: CancellationToken,
    audio: AudioCacheManager,
    active_tools: parking_lot::Mutex<HashMap<String, Vec<ActiveTask>>>,
    probe: Arc<LiveProbe>,
}

fn eof() -> AgentError {
    // Go io.EOF.
    AgentError::other("EOF")
}

impl LiveSessionImpl {
    /// Go newLiveSessionImpl.
    pub(crate) fn new() -> Arc<LiveSessionImpl> {
        let (input_tx, input_rx) = mpsc::channel(1);
        let (output_tx, output_rx) = mpsc::channel(1);
        Arc::new(LiveSessionImpl {
            input_tx,
            input_rx: tokio::sync::Mutex::new(input_rx),
            output_tx,
            output_rx: parking_lot::Mutex::new(Some(output_rx)),
            done: CancellationToken::new(),
            audio: AudioCacheManager::new(),
            active_tools: parking_lot::Mutex::new(HashMap::new()),
            probe: Arc::new(LiveProbe::default()),
        })
    }

    /// Go `done`: cancelled by `close` (and by dropping the event stream).
    pub(crate) fn done(&self) -> &CancellationToken {
        &self.done
    }

    /// Go `audioMgr`.
    pub(crate) fn audio(&self) -> &AudioCacheManager {
        &self.audio
    }

    /// Test probe shared by every task of this session.
    pub(crate) fn probe(&self) -> &Arc<LiveProbe> {
        &self.probe
    }

    /// Rendezvous with the event stream. `done` is checked first while queueing, so nothing is
    /// handed over after `close`; once queued, an acknowledgement wins over `done`, because in Go
    /// a completed channel handoff reports delivery.
    async fn push(&self, item: Result<Event, AgentError>) -> bool {
        let (ack_tx, ack_rx) = oneshot::channel();
        tokio::select! {
            biased;
            _ = self.done.cancelled() => return false,
            sent = self.output_tx.send((item, ack_tx)) => if sent.is_err() { return false; },
        }
        tokio::select! {
            biased;
            acked = ack_rx => acked.is_ok(),
            _ = self.done.cancelled() => false,
        }
    }

    /// Go pushEvent: hands `ev` to the event stream; `false` once the session is done.
    pub(crate) async fn push_event(&self, ev: Event) -> bool {
        self.push(Ok(ev)).await
    }

    /// Go pushError: hands `err` to the event stream; `false` once the session is done.
    pub(crate) async fn push_error(&self, err: AgentError) -> bool {
        self.push(Err(err)).await
    }

    /// The receiving side of Go's `inputCh` for one connection's sender task: the next request
    /// (acknowledged, so the caller's `send` returns), or `None` once `conn_token` or `done` is
    /// cancelled. Requests not yet taken stay queued for the next connection's sender.
    pub(crate) async fn next_input(&self, conn_token: &CancellationToken) -> Option<LiveRequest> {
        let mut rx = tokio::select! {
            biased;
            _ = conn_token.cancelled() => return None,
            _ = self.done.cancelled() => return None,
            guard = self.input_rx.lock() => guard,
        };
        tokio::select! {
            biased;
            _ = conn_token.cancelled() => None,
            _ = self.done.cancelled() => None,
            msg = rx.recv() => {
                let (req, ack) = msg?;
                let _ = ack.send(());
                Some(req)
            }
        }
    }

    /// Go recvIter: the pushed items until `done`. Called once per session (later calls get an
    /// empty stream). Dropping the stream cancels `done` and aborts `task`.
    pub(crate) fn event_stream(
        self: &Arc<Self>,
        task: Option<tokio::task::JoinHandle<()>>,
    ) -> EventStream {
        let rx = self.output_rx.lock().take();
        let done = self.done.clone();
        let inner: EventStream = Box::pin(async_stream::stream! {
            let Some(mut rx) = rx else { return; };
            loop {
                tokio::select! {
                    biased;
                    _ = done.cancelled() => return,
                    msg = rx.recv() => match msg {
                        None => return,
                        Some((item, ack)) => {
                            let _ = ack.send(());
                            yield item;
                        }
                    },
                }
            }
        });
        Box::pin(GuardedStream {
            inner,
            done: self.done.clone(),
            task,
        })
    }

    /// Go RegisterStreamingTool.
    pub(crate) fn register_streaming_tool(
        &self,
        tool_name: &str,
        call_id: &str,
        cancel: CancellationToken,
    ) {
        self.active_tools
            .lock()
            .entry(tool_name.to_owned())
            .or_default()
            .push(ActiveTask {
                call_id: call_id.to_owned(),
                cancel,
            });
    }

    /// Go UnregisterStreamingTool: removes the first entry with `call_id` (without cancelling it)
    /// and deletes an emptied name.
    pub(crate) fn unregister_streaming_tool(&self, tool_name: &str, call_id: &str) {
        let mut tools = self.active_tools.lock();
        let Some(tasks) = tools.get_mut(tool_name) else {
            return;
        };
        if let Some(i) = tasks.iter().position(|t| t.call_id == call_id) {
            tasks.remove(i);
        }
        if tasks.is_empty() {
            tools.remove(tool_name);
        }
    }

    /// Go CancelAllStreamingTools: cancels every running instance of `tool_name`; `false` when
    /// there was none.
    pub(crate) fn cancel_all_streaming_tools(&self, tool_name: &str) -> bool {
        let mut tools = self.active_tools.lock();
        match tools.remove(tool_name) {
            Some(tasks) if !tasks.is_empty() => {
                for t in &tasks {
                    t.cancel.cancel();
                }
                true
            }
            _ => false,
        }
    }

    /// Teardown (R9): cancels every registered streaming tool, so none outlives the flow.
    pub(crate) fn cancel_every_streaming_tool(&self) {
        let drained: Vec<ActiveTask> = self
            .active_tools
            .lock()
            .drain()
            .flat_map(|(_, tasks)| tasks)
            .collect();
        for t in drained {
            t.cancel.cancel();
        }
    }
}

#[async_trait]
impl LiveSession for LiveSessionImpl {
    /// Go Send: returns once the flow's sender task has taken `req`; `EOF` after `close`. As in
    /// `push`, `done` wins while queueing and a taken request reports success.
    async fn send(&self, req: LiveRequest) -> Result<(), AgentError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        tokio::select! {
            biased;
            _ = self.done.cancelled() => return Err(eof()),
            sent = self.input_tx.send((req, ack_tx)) => if sent.is_err() { return Err(eof()); },
        }
        tokio::select! {
            biased;
            acked = ack_rx => acked.map_err(|_| eof()),
            _ = self.done.cancelled() => Err(eof()),
        }
    }

    /// Go Close (closeOnce + close(done)), plus cancelling the streaming tools.
    async fn close(&self) -> Result<(), AgentError> {
        self.done.cancel();
        self.cancel_every_streaming_tool();
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The event stream plus the teardown its drop performs.
struct GuardedStream {
    inner: EventStream,
    done: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Stream for GuardedStream {
    type Item = Result<Event, AgentError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl Drop for GuardedStream {
    fn drop(&mut self) {
        self.done.cancel();
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::task::Poll;

    use futures::{StreamExt, poll};
    use gadk_genai::Content;
    use gadk_platform::Platform;

    use super::*;

    fn text_req(t: &str) -> LiveRequest {
        LiveRequest {
            content: Some(Content::from_text(t, "user")),
            ..Default::default()
        }
    }

    fn event(author: &str) -> Event {
        let mut ev = Event::new(&Platform::default(), "inv");
        ev.author = author.into();
        ev
    }

    // The rendezvous tests poll the futures by hand, so they never depend on timing.

    #[tokio::test]
    async fn send_is_a_rendezvous_with_next_input() {
        let sess = LiveSessionImpl::new();
        let mut send = std::pin::pin!(sess.send(text_req("hi")));
        assert!(
            poll!(send.as_mut()).is_pending(),
            "send returned before anyone took the request"
        );
        assert!(
            poll!(send.as_mut()).is_pending(),
            "send returned before anyone took the request"
        );
        let req = sess
            .next_input(&CancellationToken::new())
            .await
            .expect("request");
        assert_eq!(req.content.unwrap().parts[0].text, "hi");
        assert!(
            matches!(poll!(send.as_mut()), Poll::Ready(Ok(()))),
            "send returns once taken"
        );
    }

    #[tokio::test]
    async fn a_second_send_waits_for_its_own_rendezvous() {
        let sess = LiveSessionImpl::new();
        let mut first = std::pin::pin!(sess.send(text_req("1")));
        let mut second = std::pin::pin!(sess.send(text_req("2")));
        assert!(poll!(first.as_mut()).is_pending());
        assert!(poll!(second.as_mut()).is_pending());
        let conn = CancellationToken::new();
        assert_eq!(
            sess.next_input(&conn).await.unwrap().content.unwrap().parts[0].text,
            "1"
        );
        assert!(matches!(poll!(first.as_mut()), Poll::Ready(Ok(()))));
        assert!(
            poll!(second.as_mut()).is_pending(),
            "the second send is not taken yet"
        );
        assert_eq!(
            sess.next_input(&conn).await.unwrap().content.unwrap().parts[0].text,
            "2"
        );
        assert!(matches!(poll!(second.as_mut()), Poll::Ready(Ok(()))));
    }

    #[tokio::test]
    async fn a_waiting_send_is_released_by_close_with_eof() {
        let sess = LiveSessionImpl::new();
        let mut send = std::pin::pin!(sess.send(text_req("never taken")));
        assert!(poll!(send.as_mut()).is_pending());
        sess.close().await.unwrap();
        match poll!(send.as_mut()) {
            Poll::Ready(Err(e)) => assert_eq!(e.to_string(), "EOF"),
            other => panic!("want EOF, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_untaken_request_stays_queued_for_the_next_connection() {
        let sess = LiveSessionImpl::new();
        let mut send = std::pin::pin!(sess.send(text_req("carry")));
        assert!(poll!(send.as_mut()).is_pending());
        let old_conn = CancellationToken::new();
        old_conn.cancel();
        assert!(sess.next_input(&old_conn).await.is_none());
        assert!(
            poll!(send.as_mut()).is_pending(),
            "a cancelled connection does not take the request"
        );
        let req = sess.next_input(&CancellationToken::new()).await.unwrap();
        assert_eq!(req.content.unwrap().parts[0].text, "carry");
        assert!(matches!(poll!(send.as_mut()), Poll::Ready(Ok(()))));
    }

    #[tokio::test]
    async fn push_is_a_rendezvous_with_the_stream() {
        let sess = LiveSessionImpl::new();
        let mut stream = sess.event_stream(None);
        let mut push = std::pin::pin!(sess.push_event(event("a")));
        assert!(
            poll!(push.as_mut()).is_pending(),
            "push returned before the stream took the event"
        );
        assert!(
            poll!(push.as_mut()).is_pending(),
            "push returned before the stream took the event"
        );
        assert_eq!(stream.next().await.unwrap().unwrap().author, "a");
        assert!(matches!(poll!(push.as_mut()), Poll::Ready(true)));
    }

    #[tokio::test]
    async fn a_waiting_push_is_released_by_close() {
        let sess = LiveSessionImpl::new();
        let mut stream = sess.event_stream(None);
        let mut push = std::pin::pin!(sess.push_event(event("never taken")));
        assert!(poll!(push.as_mut()).is_pending());
        sess.close().await.unwrap();
        assert!(matches!(poll!(push.as_mut()), Poll::Ready(false)));
        assert!(
            stream.next().await.is_none(),
            "the stream never yields after close"
        );
    }

    #[tokio::test]
    async fn send_after_close_is_eof() {
        let sess = LiveSessionImpl::new();
        sess.close().await.unwrap();
        sess.close().await.unwrap(); // idempotent
        let err = sess.send(text_req("late")).await.unwrap_err();
        assert_eq!(err.to_string(), "EOF");
    }

    #[tokio::test]
    async fn next_input_returns_none_when_the_connection_ends() {
        let sess = LiveSessionImpl::new();
        let conn = CancellationToken::new();
        conn.cancel();
        assert!(sess.next_input(&conn).await.is_none());
        let open = CancellationToken::new();
        sess.close().await.unwrap();
        assert!(sess.next_input(&open).await.is_none());
    }

    #[tokio::test]
    async fn pushed_event_reaches_the_stream_which_ends_at_close() {
        let sess = LiveSessionImpl::new();
        let mut stream = sess.event_stream(None);
        let s2 = sess.clone();
        let pusher = tokio::spawn(async move { s2.push_event(event("a")).await });
        let got = stream.next().await.unwrap().unwrap();
        assert_eq!(got.author, "a");
        assert!(pusher.await.unwrap(), "push_event reports delivery");
        sess.close().await.unwrap();
        assert!(stream.next().await.is_none(), "the stream ends at close");
        assert!(
            !sess.push_error(AgentError::other("late")).await,
            "nothing is delivered after close"
        );
    }

    #[tokio::test]
    async fn pushed_error_is_delivered_before_close() {
        let sess = LiveSessionImpl::new();
        let mut stream = sess.event_stream(None);
        let s2 = sess.clone();
        tokio::spawn(async move {
            s2.push_error(AgentError::other("boom")).await;
            s2.close().await.unwrap();
        });
        assert_eq!(
            stream.next().await.unwrap().unwrap_err().to_string(),
            "boom"
        );
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn dropping_the_stream_cancels_done_and_aborts_the_task() {
        let sess = LiveSessionImpl::new();
        // The task holds `alive` until its future is dropped, which only an abort does.
        let (alive, dropped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _alive = alive;
            std::future::pending::<()>().await;
        });
        let stream = sess.event_stream(Some(task));
        drop(stream);
        assert!(sess.done().is_cancelled());
        // The timeout only turns a missing abort into a failure instead of a hang.
        let got = tokio::time::timeout(std::time::Duration::from_secs(30), dropped).await;
        assert!(
            matches!(got, Ok(Err(_))),
            "the session task was not aborted"
        );
    }

    #[test]
    fn streaming_tool_registry_matches_go() {
        let sess = LiveSessionImpl::new();
        let (c1, c2) = (CancellationToken::new(), CancellationToken::new());
        sess.register_streaming_tool("count", "call_1", c1.clone());
        sess.register_streaming_tool("count", "call_2", c2.clone());
        sess.unregister_streaming_tool("count", "call_1");
        sess.unregister_streaming_tool("count", "missing"); // no-op
        sess.unregister_streaming_tool("other", "call_1"); // no-op
        assert!(!c1.is_cancelled(), "unregistering does not cancel");
        assert!(sess.cancel_all_streaming_tools("count"));
        assert!(c2.is_cancelled());
        assert!(
            !sess.cancel_all_streaming_tools("count"),
            "the name was deleted"
        );
        assert!(!sess.cancel_all_streaming_tools("never_registered"));
        // Unregistering the last instance deletes the name.
        let c3 = CancellationToken::new();
        sess.register_streaming_tool("solo", "call_3", c3.clone());
        sess.unregister_streaming_tool("solo", "call_3");
        assert!(!sess.cancel_all_streaming_tools("solo"));
        assert!(!c3.is_cancelled());
    }

    #[tokio::test]
    async fn close_cancels_every_streaming_tool() {
        let sess = LiveSessionImpl::new();
        let (a, b) = (CancellationToken::new(), CancellationToken::new());
        sess.register_streaming_tool("x", "1", a.clone());
        sess.register_streaming_tool("y", "2", b.clone());
        sess.close().await.unwrap();
        assert!(a.is_cancelled() && b.is_cancelled());
    }

    #[test]
    fn probe_counts_task_guards() {
        let sess = LiveSessionImpl::new();
        let g1 = sess.probe().task_guard();
        let g2 = sess.probe().task_guard();
        assert_eq!(sess.probe().live_tasks.load(Ordering::SeqCst), 2);
        drop(g1);
        drop(g2);
        assert_eq!(sess.probe().live_tasks.load(Ordering::SeqCst), 0);
    }
}
