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

//! Recognition of the artifact references the live flow caches for itself.

use gadk_genai::Part;

/// The path segment the live audio cache stores its blobs under
/// when it builds an `artifact://` reference. Callers that assemble such a URI and callers that
/// recognize one must agree on this value.
pub const LIVE_ARTIFACT_NAMESPACE: &str = "_adk_live";

const ARTIFACT_URI_SCHEME: &str = "artifact://";

/// Whether `part` is an `artifact://` reference to a blob the
/// live flow cached for itself, such as the recorded input and output audio the audio cache
/// saves.
///
/// These references are ADK bookkeeping rather than conversation content, which is what
/// separates them from the user-visible `artifact://` references the same history carries and
/// keeps. The API rejects them either way (replaying one answers generateContent with a 400
/// and closes the Live socket), so callers that hand a session's contents to a model drop them
/// first. `_adk_live` must be a whole path segment.
pub fn is_internal_live_artifact_part(part: &Part) -> bool {
    let Some(file_data) = &part.file_data else {
        return false;
    };
    let uri = &file_data.file_uri;
    uri.starts_with(ARTIFACT_URI_SCHEME) && uri.contains(&format!("/{LIVE_ARTIFACT_NAMESPACE}/"))
}
