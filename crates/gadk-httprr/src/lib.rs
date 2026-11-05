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

//! gadk-httprr: HTTP record/replay for deterministic tests.
//!
//! Port of ADK-Go `internal/httprr`. A [`RecordReplay`] is an [`HttpClient`] that
//! either records real exchanges to a trace file or replays them from one. The
//! trace format is Go's (`httprr trace v1`), so ADK-Go fixtures replay unmodified.
//!
//! Replay matches on method, URL (without any `key` query parameter) and
//! [`canonical_body`], so requests that differ only in JSON key order, whitespace
//! or integral-float spelling still match. Headers are not part of the match.

mod canon;
mod wire;

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use gadk_model::{BodyStream, HttpClient, ModelError, collect_body};
use parking_lot::Mutex;

pub use canon::canonical_body;

/// Environment variable holding a regex: traces whose path matches are re-recorded.
pub const RECORD_ENV: &str = "GADK_HTTPRR_RECORD";

/// Errors from opening, recording or replaying a trace.
#[derive(Debug, thiserror::Error)]
pub enum HttprrError {
    /// The record pattern is not a valid regex.
    #[error("invalid GADK_HTTPRR_RECORD regex {0:?}: {1}")]
    InvalidRecordRegex(String, String),
    /// The file does not start with the trace header.
    #[error("read {0}: not an httprr trace")]
    NotATrace(PathBuf),
    /// The trace (or an entry in it) is malformed.
    #[error("read {0}: corrupt httprr trace")]
    Corrupt(PathBuf),
    /// An I/O error reading or writing the trace.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// No logged response matches the request.
    #[error("cached HTTP response not found for {method} {url}:\n{diff}")]
    NoMatch {
        /// Request method.
        method: String,
        /// Request URL (scrubbed).
        url: String,
        /// Diff against the closest logged request body, if any.
        diff: String,
    },
    /// An HTTP message could not be encoded or decoded, or a scrubber failed.
    #[error("httprr wire error: {0}")]
    Wire(String),
}

/// Scrubs the logged copy of a request (also applied before replay lookup).
pub type ReqScrub =
    Box<dyn Fn(&mut http::Request<bytes::Bytes>) -> Result<(), HttprrError> + Send + Sync>;
/// Scrubs the wire bytes of a logged response.
pub type RespScrub = Box<dyn Fn(&mut Vec<u8>) -> Result<(), HttprrError> + Send + Sync>;

struct RecordState {
    file: Option<File>,
    write_err: Option<String>,
}

impl RecordState {
    fn check(&self, path: &Path) -> Result<(), HttprrError> {
        if let Some(err) = &self.write_err {
            return Err(HttprrError::Wire(err.clone()));
        }
        if self.file.is_none() {
            return Err(HttprrError::Wire(format!(
                "{}: trace already closed",
                path.display()
            )));
        }
        Ok(())
    }
}

enum Mode {
    Replay(HashMap<canon::Key, Vec<u8>>),
    Record(Mutex<RecordState>),
}

/// A record/replay [`HttpClient`] backed by a Go-compatible trace file.
pub struct RecordReplay {
    path: PathBuf,
    real: Arc<dyn HttpClient>,
    mode: Mode,
    req_scrub: Vec<ReqScrub>,
    resp_scrub: Vec<RespScrub>,
}

impl std::fmt::Debug for RecordReplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordReplay")
            .field("path", &self.path)
            .field("recording", &self.is_recording())
            .finish_non_exhaustive()
    }
}

fn pattern_matches(path: &Path, pattern: Option<&str>) -> Result<bool, HttprrError> {
    match pattern {
        Some(p) if !p.is_empty() => {
            let re = regex::Regex::new(p)
                .map_err(|e| HttprrError::InvalidRecordRegex(p.to_string(), e.to_string()))?;
            Ok(re.is_match(&path.to_string_lossy()))
        }
        _ => Ok(false),
    }
}

fn env_pattern() -> Option<String> {
    std::env::var(RECORD_ENV).ok()
}

fn transport(e: HttprrError) -> ModelError {
    ModelError::Transport(Box::new(e))
}

fn one_chunk(body: Bytes) -> BodyStream {
    futures::stream::iter([Ok(body)]).boxed()
}

/// The logged copy of `req` with the default scrubbers applied: API key headers
/// removed and any `key` query parameter stripped.
fn default_scrubbed(req: &http::Request<Bytes>) -> Result<http::Request<Bytes>, HttprrError> {
    let uri: http::Uri = canon::canonical_url(&req.uri().to_string())
        .parse()
        .map_err(|e: http::uri::InvalidUri| HttprrError::Wire(e.to_string()))?;
    let mut out = http::Request::new(req.body().clone());
    *out.method_mut() = req.method().clone();
    *out.uri_mut() = uri;
    *out.version_mut() = req.version();
    *out.headers_mut() = req.headers().clone();
    out.headers_mut().remove("x-goog-api-key");
    out.headers_mut().remove(http::header::AUTHORIZATION);
    Ok(out)
}

impl RecordReplay {
    /// Opens `path`, recording if [`RECORD_ENV`] is a regex matching the path and
    /// replaying otherwise.
    pub fn open(path: impl AsRef<Path>, real: Arc<dyn HttpClient>) -> Result<Self, HttprrError> {
        Self::open_with(path, real, env_pattern().as_deref())
    }

    /// Opens `path`, recording iff `record_pattern` is a non-empty regex matching
    /// the path. Recording creates (truncates) the file; its directory must exist.
    pub fn open_with(
        path: impl AsRef<Path>,
        real: Arc<dyn HttpClient>,
        record_pattern: Option<&str>,
    ) -> Result<Self, HttprrError> {
        let path = path.as_ref().to_path_buf();
        let mode = if pattern_matches(&path, record_pattern)? {
            let mut file = File::create(&path)?;
            file.write_all(format!("{}\n", wire::TRACE_HEADER).as_bytes())?;
            Mode::Record(Mutex::new(RecordState {
                file: Some(file),
                write_err: None,
            }))
        } else {
            Mode::Replay(Self::load(&path)?)
        };
        Ok(Self {
            path,
            real,
            mode,
            req_scrub: Vec::new(),
            resp_scrub: Vec::new(),
        })
    }

    fn load(path: &Path) -> Result<HashMap<canon::Key, Vec<u8>>, HttprrError> {
        let data = std::fs::read(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "{}: no httprr trace; record it with {RECORD_ENV}=<regex matching the path>",
                        path.display()
                    ),
                )
            } else {
                e
            }
        })?;
        let mut replay = HashMap::new();
        for (req, resp) in wire::parse_trace(path, &data)? {
            let parsed =
                wire::parse_request(&req).map_err(|_| HttprrError::Corrupt(path.to_path_buf()))?;
            // Later entries overwrite earlier ones: the last duplicate wins.
            replay.insert(canon::key(&parsed.method, &parsed.url, &parsed.body), resp);
        }
        Ok(replay)
    }

    /// Reports whether [`RECORD_ENV`] selects `path` for recording.
    pub fn recording(path: impl AsRef<Path>) -> Result<bool, HttprrError> {
        pattern_matches(path.as_ref(), env_pattern().as_deref())
    }

    /// Reports whether this instance is recording.
    pub fn is_recording(&self) -> bool {
        matches!(self.mode, Mode::Record(_))
    }

    /// Adds a request scrubber. Scrubbers run in registration order, after the
    /// default ones, on a copy of the request; the real request is unchanged.
    pub fn scrub_req(&mut self, f: ReqScrub) {
        self.req_scrub.push(f);
    }

    /// Adds a response scrubber, run on the logged response wire bytes.
    pub fn scrub_resp(&mut self, f: RespScrub) {
        self.resp_scrub.push(f);
    }

    /// Closes the trace file, reporting any earlier write error. Requests sent
    /// after closing a recording fail. A no-op in replay mode.
    pub fn close(&self) -> Result<(), HttprrError> {
        let Mode::Record(state) = &self.mode else {
            return Ok(());
        };
        let mut state = state.lock();
        if let Some(err) = &state.write_err {
            return Err(HttprrError::Wire(err.clone()));
        }
        if let Some(mut file) = state.file.take() {
            file.flush()?;
        }
        Ok(())
    }

    fn log_request(&self, req: &http::Request<Bytes>) -> Result<http::Request<Bytes>, HttprrError> {
        let mut log = default_scrubbed(req)?;
        for scrub in &self.req_scrub {
            scrub(&mut log)?;
        }
        Ok(log)
    }

    fn replay(
        &self,
        replay: &HashMap<canon::Key, Vec<u8>>,
        log: &http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, HttprrError> {
        let key = canon::key(log.method().as_str(), &log.uri().to_string(), log.body());
        let Some(resp) = replay.get(&key) else {
            let diff = replay
                .keys()
                .filter(|(m, u, _)| *m == key.0 && *u == key.1)
                .min_by_key(|(_, _, body)| canon::distance(body, &key.2))
                .map(|(_, _, body)| canon::diff(body, &key.2))
                .unwrap_or_else(|| "no entry for method+URL".to_string());
            return Err(HttprrError::NoMatch {
                method: key.0,
                url: key.1,
                diff,
            });
        };
        let resp =
            wire::parse_response(resp).map_err(|_| HttprrError::Corrupt(self.path.clone()))?;
        Ok(resp.map(one_chunk))
    }

    /// Appends one entry. On a write failure the trace is deleted and every later
    /// request (and [`close`](Self::close)) reports the failure, as in Go.
    fn write_log(
        &self,
        state: &Mutex<RecordState>,
        req: &[u8],
        resp: &[u8],
    ) -> Result<(), HttprrError> {
        let mut state = state.lock();
        state.check(&self.path)?;
        let mut entry = format!("{} {}\n", req.len(), resp.len()).into_bytes();
        entry.extend_from_slice(req);
        entry.extend_from_slice(resp);
        let result = match state.file.as_mut() {
            Some(file) => file.write_all(&entry),
            None => Ok(()),
        };
        if let Err(e) = result {
            state.write_err = Some(e.to_string());
            state.file = None;
            let _ = std::fs::remove_file(&self.path);
            return Err(e.into());
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl HttpClient for RecordReplay {
    async fn send(
        &self,
        req: http::Request<Bytes>,
    ) -> Result<http::Response<BodyStream>, ModelError> {
        let log = self.log_request(&req).map_err(transport)?;
        let state = match &self.mode {
            Mode::Replay(replay) => return self.replay(replay, &log).map_err(transport),
            Mode::Record(state) => state,
        };
        let req_wire = wire::write_request(&log).map_err(transport)?;
        state.lock().check(&self.path).map_err(transport)?;

        // The real server gets the unscrubbed request.
        let resp = self.real.send(req).await?;
        let (parts, body) = resp.into_parts();
        let body = collect_body(body).await?;

        let mut resp_wire = wire::write_response(parts.status, &parts.headers, &body);
        for scrub in &self.resp_scrub {
            scrub(&mut resp_wire).map_err(transport)?;
        }
        self.write_log(state, &req_wire, &resp_wire)
            .map_err(transport)?;
        Ok(http::Response::from_parts(parts, one_chunk(body)))
    }
}
