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

//! Incremental server-sent-events parser, shared by the model backends (moved from
//! gadk-gemini, where it parsed `streamGenerateContent?alt=sse`).

use std::collections::VecDeque;

use futures::StreamExt;
use futures::stream::BoxStream;

use crate::error::{ModelError, body_prefix};
use crate::http::BodyStream;

/// One dispatched server-sent event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The value of the event's last `event:` field, if it had one.
    pub event: Option<String>,
    /// The event's `data:` lines joined with `\n`.
    pub data: String,
}

/// The line limit of the Anthropic SDK's SSE decoder: 32 MiB (`64 KiB << 9`).
pub const ANTHROPIC_SDK_MAX_LINE: usize = (64 * 1024) << 9;

/// The error text for a line over the limit.
const TOKEN_TOO_LONG: &str = "SSE line too long";

struct Parser {
    body: BodyStream,
    buf: Vec<u8>,
    /// `buf[..scanned]` holds no `\n`: the next search starts there.
    scanned: usize,
    /// The longest line allowed, if any.
    max_line: Option<usize>,
    event: Option<String>,
    data: Vec<String>,
    ready: VecDeque<Result<SseEvent, ModelError>>,
    eof: bool,
}

impl Parser {
    /// Handles one line (without its terminator).
    fn line(&mut self, mut line: &[u8]) {
        if let Some(stripped) = line.strip_suffix(b"\r") {
            line = stripped;
        }
        if line.is_empty() {
            self.dispatch();
            return;
        }
        if line.starts_with(b":") {
            return; // comment
        }
        let (field, value) = match line.iter().position(|b| *b == b':') {
            Some(i) => {
                let value = &line[i + 1..];
                (&line[..i], value.strip_prefix(b" ").unwrap_or(value))
            }
            None => (line, &b""[..]),
        };
        let (is_event, name) = match field {
            b"data" => (false, "data"),
            b"event" => (true, "event"),
            _ => return, // id, retry and unknown fields are ignored
        };
        match std::str::from_utf8(value) {
            Ok(s) if is_event => self.event = Some(s.to_string()),
            Ok(s) => self.data.push(s.to_string()),
            Err(e) => self.ready.push_back(Err(ModelError::Decode {
                message: format!("invalid UTF-8 in SSE {name}: {e}"),
                body_prefix: body_prefix(value),
            })),
        }
    }

    /// Emits the pending event, if it has data. The event name is reset either way.
    fn dispatch(&mut self) {
        let event = self.event.take();
        if !self.data.is_empty() {
            let data = std::mem::take(&mut self.data).join("\n");
            self.ready.push_back(Ok(SseEvent { event, data }));
        }
    }

    /// Consumes every complete line in the buffer. Returns `false` (token too long) when
    /// a line with its `\n` does not fit in `max_line` bytes, or the unterminated tail already
    /// fills them.
    fn drain_lines(&mut self) -> bool {
        let max_line = self.max_line;
        let too_long = |len: usize| max_line.is_some_and(|max| len > max);
        let mut start = 0;
        let mut from = self.scanned;
        while let Some(off) = self.buf[from..].iter().position(|b| *b == b'\n') {
            let end = from + off;
            if too_long(end + 1 - start) {
                return false;
            }
            let line = self.buf[start..end].to_vec();
            self.line(&line);
            start = end + 1;
            from = start;
        }
        if too_long(self.buf.len() - start + 1) {
            return false;
        }
        self.buf.drain(..start);
        self.scanned = self.buf.len();
        true
    }
}

/// Parses `body` as an SSE stream and yields each event: its `event:` name and its joined
/// `data:` payload.
///
/// Lines end in `\n` (a trailing `\r` is stripped); field values have one leading space
/// removed; multiple `data:` lines of an event are joined with `\n`; the last `event:` line
/// names the event; a blank line ends an event, which is emitted only if it has data;
/// comments and other fields are ignored. At end of input any pending data is emitted. A body
/// error is yielded and ends the stream; so is invalid UTF-8 in a `data:` or `event:` value
/// ([`ModelError::Decode`] naming the field). Lines have no length limit.
pub fn sse_events(body: BodyStream) -> BoxStream<'static, Result<SseEvent, ModelError>> {
    parse(body, None)
}

/// [`sse_events`] with a line limit: a line whose
/// bytes plus its `\n` exceed `max_line`, or an unterminated tail of `max_line` bytes, ends the
/// stream with a [`ModelError::Other`] whose text is `SSE line too long`.
pub fn sse_events_with_max_line(
    body: BodyStream,
    max_line: usize,
) -> BoxStream<'static, Result<SseEvent, ModelError>> {
    parse(body, Some(max_line))
}

fn parse(
    body: BodyStream,
    max_line: Option<usize>,
) -> BoxStream<'static, Result<SseEvent, ModelError>> {
    let parser = Parser {
        body,
        buf: Vec::new(),
        scanned: 0,
        max_line,
        event: None,
        data: Vec::new(),
        ready: VecDeque::new(),
        eof: false,
    };
    futures::stream::unfold(parser, |mut p| async move {
        loop {
            if let Some(item) = p.ready.pop_front() {
                return Some((item, p));
            }
            if p.eof {
                return None;
            }
            match p.body.next().await {
                Some(Ok(chunk)) => {
                    p.buf.extend_from_slice(&chunk);
                    if !p.drain_lines() {
                        // Events completed before the long line are still delivered first.
                        p.eof = true;
                        p.ready
                            .push_back(Err(ModelError::Other(TOKEN_TOO_LONG.into())));
                    }
                }
                Some(Err(e)) => {
                    p.eof = true;
                    return Some((Err(e), p));
                }
                None => {
                    p.eof = true;
                    if !p.buf.is_empty() {
                        let rest = std::mem::take(&mut p.buf);
                        p.line(&rest);
                    }
                    p.dispatch();
                }
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use bytes::Bytes;

    fn body(chunks: &[&'static str]) -> BodyStream {
        futures::stream::iter(
            chunks
                .iter()
                .map(|c| Ok(Bytes::from_static(c.as_bytes())))
                .collect::<Vec<_>>(),
        )
        .boxed()
    }

    async fn events(chunks: &[&'static str]) -> Vec<SseEvent> {
        sse_events(body(chunks)).map(|r| r.unwrap()).collect().await
    }

    fn data(d: &str) -> SseEvent {
        SseEvent {
            event: None,
            data: d.to_string(),
        }
    }

    fn named(event: &str, d: &str) -> SseEvent {
        SseEvent {
            event: Some(event.to_string()),
            data: d.to_string(),
        }
    }

    #[tokio::test]
    async fn single_event() {
        assert_eq!(
            events(&["data: {\"a\":1}\n\n"]).await,
            vec![data("{\"a\":1}")]
        );
    }

    #[tokio::test]
    async fn split_across_chunks_and_crlf() {
        assert_eq!(
            events(&["da", "ta: one\r", "\n\r\ndata:two\n", "\n"]).await,
            vec![data("one"), data("two")]
        );
    }

    #[tokio::test]
    async fn multi_line_data_comments_and_fields() {
        assert_eq!(
            events(&[": hi\nevent: x\ndata: a\ndata:  b\nid: 1\n\n\n"]).await,
            vec![named("x", "a\n b")]
        );
    }

    #[tokio::test]
    async fn pending_data_at_eof() {
        assert_eq!(
            events(&["data: x\n\ndata: tail"]).await,
            vec![data("x"), data("tail")]
        );
    }

    #[tokio::test]
    async fn body_error_ends_stream() {
        let b: BodyStream = futures::stream::iter(vec![
            Ok(Bytes::from_static(b"data: x\n\n")),
            Err(ModelError::EmptyResponse),
            Ok(Bytes::from_static(b"data: y\n\n")),
        ])
        .boxed();
        let out: Vec<_> = sse_events(b).collect().await;
        assert_eq!(out.len(), 2);
        assert!(matches!(out[1], Err(ModelError::EmptyResponse)));
    }

    /// The event name belongs to one event: it is reset at the blank line that ends it, even
    /// when that event had no data, and the last `event:` line of an event wins.
    #[tokio::test]
    async fn event_names_reset_per_event() {
        assert_eq!(
            events(&[
                "event: a\nevent: message_start\ndata: 1\n\nevent: ping\n\ndata: 2\n\nevent: error\ndata: {}\n\n"
            ])
            .await,
            vec![named("message_start", "1"), data("2"), named("error", "{}")]
        );
    }

    #[tokio::test]
    async fn invalid_utf8_names_the_field() {
        for (input, field) in [
            (&b"event: \xff\ndata: x\n\n"[..], "event"),
            (&b"data: \xff\n\n"[..], "data"),
        ] {
            let b: BodyStream =
                futures::stream::iter(vec![Ok(Bytes::copy_from_slice(input))]).boxed();
            let out: Vec<_> = sse_events(b).collect().await;
            let err = out[0].as_ref().unwrap_err().to_string();
            assert!(
                err.contains(&format!("invalid UTF-8 in SSE {field}")),
                "{field}: {err}"
            );
        }
    }

    fn limited(
        chunks: Vec<Vec<u8>>,
        max: usize,
    ) -> BoxStream<'static, Result<SseEvent, ModelError>> {
        let b: BodyStream =
            futures::stream::iter(chunks.into_iter().map(|c| Ok(Bytes::from(c)))).boxed();
        sse_events_with_max_line(b, max)
    }

    /// Go `bufio.Scanner` with `Buffer(nil, max)`: a line and its `\n` must fit in `max`
    /// bytes, and an unterminated tail of `max` bytes is already too long.
    #[tokio::test]
    async fn max_line_follows_go_bufio_scanner() {
        // "data: " + 3 bytes + "\n" = 10 bytes: fits a 10-byte buffer.
        let ok: Vec<_> = limited(vec![b"data: abc\n\n".to_vec()], 10).collect().await;
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].as_ref().unwrap(), &data("abc"));
        // One byte more, in one chunk or split, is too long.
        for chunks in [
            vec![b"data: abcd\n\n".to_vec()],
            vec![b"data".to_vec(), b": ab".to_vec(), b"cd\n\n".to_vec()],
        ] {
            let out: Vec<_> = limited(chunks, 10).collect().await;
            assert_eq!(out.len(), 1, "{out:?}");
            assert_eq!(
                out[0].as_ref().unwrap_err().to_string(),
                "SSE line too long"
            );
        }
        // An unterminated tail of exactly `max` bytes is too long, even at EOF.
        let out: Vec<_> = limited(vec![b"data: abcd".to_vec()], 10).collect().await;
        assert_eq!(
            out[0].as_ref().unwrap_err().to_string(),
            "SSE line too long"
        );
        let out: Vec<_> = limited(vec![b"data: abc".to_vec()], 10).collect().await;
        assert_eq!(out[0].as_ref().unwrap(), &data("abc"));
    }

    /// Many small chunks of one long line are scanned once each, not from the start of the
    /// buffer every time.
    #[tokio::test]
    async fn long_line_in_many_chunks() {
        let mut chunks = vec![b"data: ".to_vec()];
        chunks.extend(std::iter::repeat_n(b"x".to_vec(), 10_000));
        chunks.push(b"\n\n".to_vec());
        let out: Vec<_> = limited(chunks, ANTHROPIC_SDK_MAX_LINE).collect().await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].as_ref().unwrap().data.len(), 10_000);
    }
}
