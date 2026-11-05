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

//! Incremental server-sent-events parser for `streamGenerateContent?alt=sse`.

use std::collections::VecDeque;

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_model::{BodyStream, ModelError, body_prefix};

struct Parser {
    body: BodyStream,
    buf: Vec<u8>,
    data: Vec<String>,
    ready: VecDeque<Result<String, ModelError>>,
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
        if field != b"data" {
            return; // event, id, retry and unknown fields are ignored
        }
        match std::str::from_utf8(value) {
            Ok(s) => self.data.push(s.to_string()),
            Err(e) => self.ready.push_back(Err(ModelError::Decode {
                message: format!("invalid UTF-8 in SSE data: {e}"),
                body_prefix: body_prefix(value),
            })),
        }
    }

    /// Emits the pending event, if it has data.
    fn dispatch(&mut self) {
        if !self.data.is_empty() {
            let data = std::mem::take(&mut self.data).join("\n");
            self.ready.push_back(Ok(data));
        }
    }

    /// Consumes every complete line in the buffer.
    fn drain_lines(&mut self) {
        let mut start = 0;
        while let Some(off) = self.buf[start..].iter().position(|b| *b == b'\n') {
            let end = start + off;
            let line = self.buf[start..end].to_vec();
            self.line(&line);
            start = end + 1;
        }
        self.buf.drain(..start);
    }
}

/// Parses `body` as an SSE stream and yields each event's joined `data:` payload.
///
/// Lines end in `\n` (a trailing `\r` is stripped); `data:` values have one leading space
/// removed and multiple `data:` lines of an event are joined with `\n`; a blank line ends
/// an event; comments and other fields are ignored. At end of input any pending data is
/// emitted. A body error is yielded and ends the stream.
pub(crate) fn sse_events(body: BodyStream) -> BoxStream<'static, Result<String, ModelError>> {
    let parser = Parser {
        body,
        buf: Vec::new(),
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
                    p.drain_lines();
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

    async fn events(chunks: &[&'static str]) -> Vec<String> {
        sse_events(body(chunks)).map(|r| r.unwrap()).collect().await
    }

    #[tokio::test]
    async fn single_event() {
        assert_eq!(events(&["data: {\"a\":1}\n\n"]).await, vec!["{\"a\":1}"]);
    }

    #[tokio::test]
    async fn split_across_chunks_and_crlf() {
        assert_eq!(
            events(&["da", "ta: one\r", "\n\r\ndata:two\n", "\n"]).await,
            vec!["one", "two"]
        );
    }

    #[tokio::test]
    async fn multi_line_data_comments_and_fields() {
        assert_eq!(
            events(&[": hi\nevent: x\ndata: a\ndata:  b\nid: 1\n\n\n"]).await,
            vec!["a\n b"]
        );
    }

    #[tokio::test]
    async fn pending_data_at_eof() {
        assert_eq!(events(&["data: x\n\ndata: tail"]).await, vec!["x", "tail"]);
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
}
