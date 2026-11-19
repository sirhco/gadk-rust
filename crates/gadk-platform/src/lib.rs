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

//! gadk-platform: overridable time, UUID and task-execution seams, the ADK tracking header
//! value, the injectable environment ([`env`](mod@env)), and shared helpers with exact output
//! formats: double-quoted strings ([`go_quote`]), shortest round-trip floats
//! ([`go_format_float_g`]), simple one-rune casing ([`go_to_lower`], [`go_to_upper`]), query
//! escaping ([`go_query_escape`]), `%v`-style rendering of a JSON value ([`go_sprint_value`]),
//! RFC 3339 parsing ([`go_parse_rfc3339`]), lexical path functions ([`go_path_clean`],
//! [`go_path_join`], [`go_path_dir`], [`go_path_base`]), Unix [`go_filepath_rel`] and
//! [`go_filepath_is_local`], safe resolution of an untrusted relative path ([`safe_subpath`]),
//! URL parsing ([`go_url`]), error wrapping with a prefix ([`GoWrapped`]), a built-in MIME
//! table and content sniffing ([`go_mime`]), durations ([`GoDuration`], [`go_parse_duration`],
//! [`go_format_duration`]), the flag parser the launcher uses ([`go_flag`]) and the wording of
//! JSON type-mismatch errors ([`json_kind`], [`json_expected_found`]).

pub mod env;
mod exec;
mod go_err;
pub mod go_flag;
mod go_fmt;
pub mod go_mime;
mod go_path;
mod go_time;
pub mod go_url;
mod json_kind;
mod safe_subpath;
mod time;
mod tracking;
mod uuid;

pub use exec::{TaskRunner, TokioTaskRunner};
pub use futures::future::BoxFuture;
pub use go_err::{GoWrapped, go_errno_text};
pub use go_fmt::{
    go_format_float_g, go_format_float_v, go_query_escape, go_quote, go_quote_bytes,
    go_sprint_value, go_to_lower, go_to_upper, go_type_name,
};
pub use go_path::{
    go_filepath_is_local, go_filepath_rel, go_path_base, go_path_clean, go_path_dir, go_path_join,
};
pub use go_time::{
    GoDuration, go_format_duration, go_format_rfc3339_nano, go_parse_duration, go_parse_rfc3339,
};
pub use json_kind::{json_expected_found, json_kind, json_kind_article, json_text_kind_article};
pub use safe_subpath::{
    ERR_NOT_VALID_RELATIVE_PATH, ERR_SYMLINK_IN_RELATIVE_PATH, SafeSubpathError,
    SafeSubpathErrorKind, is_link_like, safe_subpath,
};
pub use time::TimeProvider;
pub use tracking::tracking_header;
pub use uuid::UuidProvider;

use chrono::{DateTime, Duration, TimeZone, Utc};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Explicit replacement for context-carried providers: the clock, UUID source and task runner.
#[derive(Clone)]
pub struct Platform {
    time: Arc<dyn TimeProvider>,
    uuid: Arc<dyn UuidProvider>,
    tasks: Arc<dyn TaskRunner>,
}

impl Default for Platform {
    fn default() -> Self {
        Self {
            time: Arc::new(time::SystemTime),
            uuid: Arc::new(uuid::RandomUuid),
            tasks: Arc::new(TokioTaskRunner),
        }
    }
}

impl std::fmt::Debug for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Platform")
    }
}

impl Platform {
    /// Current time from the configured [`TimeProvider`].
    pub fn now(&self) -> DateTime<Utc> {
        self.time.now()
    }

    /// New identifier from the configured [`UuidProvider`].
    pub fn new_uuid(&self) -> String {
        self.uuid.new_uuid()
    }

    /// Runs all tasks on the configured [`TaskRunner`] and waits for them.
    /// An empty task list returns immediately without invoking the runner.
    pub async fn run_tasks(&self, tasks: Vec<BoxFuture<'static, ()>>) {
        if tasks.is_empty() {
            return;
        }
        self.tasks.run(tasks).await;
    }

    /// Returns this platform with its time provider replaced.
    pub fn with_time(mut self, p: impl TimeProvider + 'static) -> Self {
        self.time = Arc::new(p);
        self
    }

    /// Returns this platform with its UUID provider replaced.
    pub fn with_uuid(mut self, p: impl UuidProvider + 'static) -> Self {
        self.uuid = Arc::new(p);
        self
    }

    /// Returns this platform with its task runner replaced.
    pub fn with_task_runner(mut self, r: impl TaskRunner + 'static) -> Self {
        self.tasks = Arc::new(r);
        self
    }

    /// Time starts at 2000-01-01T00:00:00Z + `seed` seconds and advances 1 ms per call;
    /// UUIDs are v4-formatted strings derived from a seeded counter.
    pub fn deterministic(seed: u64) -> Self {
        let base = Utc
            .with_ymd_and_hms(2000, 1, 1, 0, 0, 0)
            .single()
            .unwrap_or_default()
            + Duration::seconds(i64::try_from(seed % 1_000_000_000).unwrap_or(0));
        let ticks = Arc::new(AtomicU64::new(0));
        let ids = Arc::new(AtomicU64::new(0));
        Self::default()
            .with_time(move || {
                let ms = i64::try_from(ticks.fetch_add(1, Ordering::SeqCst)).unwrap_or(i64::MAX);
                Duration::try_milliseconds(ms)
                    .and_then(|d| base.checked_add_signed(d))
                    .unwrap_or(DateTime::<Utc>::MAX_UTC)
            })
            .with_uuid(move || {
                let n = ids.fetch_add(1, Ordering::SeqCst);
                let mut b = [0u8; 16];
                b[..8].copy_from_slice(&seed.to_be_bytes());
                b[8..].copy_from_slice(&n.to_be_bytes());
                ::uuid::Builder::from_random_bytes(b)
                    .into_uuid()
                    .to_string()
            })
    }
}
