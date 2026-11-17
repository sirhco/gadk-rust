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

//! `artifact://` reference URIs: one artifact naming a stored version of another. Crate-private;
//! only [`crate::InMemoryArtifactService`] understands references.

use gadk_genai::Part;

/// The URI scheme used to reference one artifact from another.
pub(crate) const ARTIFACT_URI_SCHEME: &str = "artifact://";

/// The result of parsing an artifact reference URI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParsedArtifactUri {
    pub(crate) app_name: String,
    pub(crate) user_id: String,
    /// Empty for a user-scoped artifact reference.
    pub(crate) session_id: String,
    pub(crate) file_name: String,
    pub(crate) version: i64,
}

/// One non-empty segment without `/`, followed by `/`: the segment and the rest.
fn segment(s: &str) -> Option<(&str, &str)> {
    let (seg, rest) = s.split_once('/')?;
    (!seg.is_empty()).then_some((seg, rest))
}

/// The `artifacts/<file>/versions/<version>` tail: the file name (non-empty, no `\n`) and the
/// positive version (no leading zero). The file name may hold `/` and the version does not, so
/// the split is at the last `/versions/`.
fn file_and_version(s: &str) -> Option<(&str, i64)> {
    let rest = s.strip_prefix("artifacts/")?;
    let (file, version) = rest.rsplit_once("/versions/")?;
    if file.is_empty() || file.contains('\n') {
        return None;
    }
    let mut digits = version.bytes();
    if !matches!(digits.next(), Some(b'1'..=b'9')) || !digits.all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((file, version.parse().ok()?))
}

/// Parses
/// `artifact://apps/{app}/users/{user}/sessions/{session}/artifacts/{file}/versions/{version}`
/// or its user-scoped variant (without the sessions segment); `None` when `uri` is not a
/// well-formed artifact URI.
///
/// The app, user and session segments are non-empty and hold no `/` (stricter than adk-python,
/// whose regex lets a segment hold `/`). A reference names a stored version, so the version must be
/// positive: version 0 is the "latest" sentinel here, and following a reference to
/// `versions/0` would resolve to whatever the target's newest version is at load time.
pub(crate) fn parse_artifact_uri(uri: &str) -> Option<ParsedArtifactUri> {
    let rest = uri.strip_prefix("artifact://apps/")?;
    let (app, rest) = segment(rest)?;
    let rest = rest.strip_prefix("users/")?;
    let (user, rest) = segment(rest)?;
    // A URI with a `sessions/` segment is session-scoped; one without it must have `artifacts/`
    // right after the user segment.
    let (session, rest) = match rest.strip_prefix("sessions/") {
        Some(rest) => {
            let (session, rest) = segment(rest)?;
            (session, rest)
        }
        None => ("", rest),
    };
    let (file, version) = file_and_version(rest)?;
    Some(ParsedArtifactUri {
        app_name: app.to_owned(),
        user_id: user.to_owned(),
        session_id: session.to_owned(),
        file_name: file.to_owned(),
        version,
    })
}

/// Builds a reference URI: a user-scoped one when `session_id` is empty. The segments are not
/// escaped. Used by tests only.
#[cfg(test)]
pub(crate) fn build_artifact_uri(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    file_name: &str,
    version: i64,
) -> String {
    if session_id.is_empty() {
        format!(
            "artifact://apps/{app_name}/users/{user_id}/artifacts/{file_name}/versions/{version}"
        )
    } else {
        format!(
            "artifact://apps/{app_name}/users/{user_id}/sessions/{session_id}/artifacts/{file_name}/versions/{version}"
        )
    }
}

/// Whether `part`'s `file_data.file_uri` uses the `artifact://`
/// scheme.
pub(crate) fn is_artifact_ref(part: Option<&Part>) -> bool {
    part.and_then(|p| p.file_data.as_ref())
        .is_some_and(|f| f.file_uri.starts_with(ARTIFACT_URI_SCHEME))
}

/// Checks that a reference cannot escape the caller's app and user, and that a session-scoped
/// one cannot escape the caller's session.
pub(crate) fn validate_artifact_reference_scope(
    app_name: &str,
    user_id: &str,
    session_id: &str,
    parsed: &ParsedArtifactUri,
) -> Result<(), String> {
    if parsed.app_name != app_name || parsed.user_id != user_id {
        return Err("artifact reference must stay within the same app and user scope".into());
    }
    if !parsed.session_id.is_empty() && parsed.session_id != session_id {
        return Err(
            "session-scoped artifact reference must stay within the same session scope".into(),
        );
    }
    Ok(())
}
