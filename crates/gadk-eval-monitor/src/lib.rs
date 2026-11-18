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

//! Production eval monitors: grade real agent runs with gadk-eval's scorers.
//!
//! A [`TranscriptSource`] lists recent runs as [`ProductionRun`]s (the [`source::bigquery`]
//! source reads the agent-analytics table). A monitor ([`monitor`]) scores each run against a
//! YAML monitor spec ([`spec`]) and returns a report ([`report`]), which a report store
//! ([`store`]) keeps in a local directory or a `gs://` bucket. The `gadk-eval` binary ([`cli`])
//! runs all of it.

mod error;
#[cfg(test)]
mod testutil;

pub mod cli;
pub mod gcp;
pub mod judge;
pub mod monitor;
pub mod report;
pub mod source;
pub mod spec;
pub mod store;

pub use error::MonitorError;
pub use source::{ProductionRun, TranscriptQuery, TranscriptSource};
