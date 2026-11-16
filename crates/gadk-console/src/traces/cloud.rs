// Copyright 2026 Chris Olson
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

//! The Cloud Trace v1 and Cloud Logging trace source.
//!
//! Placeholder until Task 3 replaces this file: it provides `CloudTraceSource::new` and a
//! `TraceSource` impl that always fails, so Task 5 can build the web shell in parallel.

use super::{SourceError, TraceDetail, TraceId, TracePage, TraceQuery, TraceSource};

/// Reads traces from Cloud Trace v1 and their log entries from Cloud Logging.
pub struct CloudTraceSource {
    _project: String,
    _log_id: String,
}

impl CloudTraceSource {
    /// Real endpoints, a reqwest client, lazily built ADC (cloud-platform scope).
    pub fn new(project: impl Into<String>, log_id: impl Into<String>) -> Self {
        Self {
            _project: project.into(),
            _log_id: log_id.into(),
        }
    }
}

#[async_trait::async_trait]
impl TraceSource for CloudTraceSource {
    async fn list(&self, _query: &TraceQuery) -> Result<TracePage, SourceError> {
        Err(SourceError::Credentials("not implemented (Task 3)".into()))
    }
    async fn get(&self, _id: &TraceId) -> Result<TraceDetail, SourceError> {
        Err(SourceError::Credentials("not implemented (Task 3)".into()))
    }
}
