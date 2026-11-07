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

//! The subset of Go's `mime.ParseMediaType` that `decodeTextBlob` reaches, and ADK-Go's
//! `hasMIMECharsetParameter` / `isTextMediaType` (`tool/mcptoolset/tool.go`).

use std::collections::BTreeMap;

/// Why [`parse_media_type`] failed.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaTypeError {
    /// The media type itself is malformed.
    Invalid,
    /// Go `mime.ErrInvalidMediaParameter`: the media type parsed, a parameter did not.
    InvalidParameter,
}

/// Go `mime.ParseMediaType`: the lowercased media type and its parameters. On
/// [`MediaTypeError::InvalidParameter`] the error carries the media type (Go returns it with no
/// parameters).
#[allow(unused_variables, dead_code, clippy::todo)]
pub(crate) fn parse_media_type(
    v: &str,
) -> Result<(String, BTreeMap<String, String>), (MediaTypeError, String)> {
    todo!("Task 3")
}

/// Go `hasMIMECharsetParameter`.
#[allow(unused_variables, dead_code, clippy::todo)]
pub(crate) fn has_mime_charset_parameter(value: &str) -> bool {
    todo!("Task 3")
}

/// Go `isTextMediaType`.
#[allow(unused_variables, dead_code, clippy::todo)]
pub(crate) fn is_text_media_type(media_type: &str) -> bool {
    todo!("Task 3")
}
