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

//! The crate error type.

/// Errors from loading a monitor spec, listing runs, building the judge or storing reports.
#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    /// The monitor spec is invalid; the message names the field.
    #[error("monitor spec: {0}")]
    Spec(String),
    /// Listing runs from the transcript source failed.
    #[error("transcript source: {0}")]
    Source(String),
    /// The judge model could not be built.
    #[error("judge: {0}")]
    Judge(String),
    /// Reading or writing a stored report failed, or a report name is invalid.
    #[error("report store: {0}")]
    Store(String),
    /// A local file could not be read or written.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_the_stage() {
        assert_eq!(
            MonitorError::Spec("`window` must be <n>m, <n>h or <n>d".into()).to_string(),
            "monitor spec: `window` must be <n>m, <n>h or <n>d"
        );
        assert_eq!(
            MonitorError::Source("HTTP 403: denied".into()).to_string(),
            "transcript source: HTTP 403: denied"
        );
        assert_eq!(
            MonitorError::Store("x".into()).to_string(),
            "report store: x"
        );
    }
}
