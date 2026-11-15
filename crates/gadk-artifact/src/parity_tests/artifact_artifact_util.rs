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

//! Port of ADK-Go `artifact/artifact_util_test.go`.

use gadk_genai::{FileData, Part};

use crate::artifact_util::{
    ParsedArtifactUri, build_artifact_uri, is_artifact_ref, parse_artifact_uri,
    validate_artifact_reference_scope,
};

fn parsed(app: &str, user: &str, session: &str, file: &str, version: i64) -> ParsedArtifactUri {
    ParsedArtifactUri {
        app_name: app.into(),
        user_id: user.into(),
        session_id: session.into(),
        file_name: file.into(),
        version,
    }
}

#[test]
fn test_parse_artifact_uri() {
    let cases = [
        (
            "session-scoped",
            "artifact://apps/myapp/users/user-1/sessions/sess-1/artifacts/report.pdf/versions/3",
            Some(parsed("myapp", "user-1", "sess-1", "report.pdf", 3)),
        ),
        (
            "user-scoped",
            "artifact://apps/myapp/users/user-1/artifacts/user:profile.png/versions/1",
            Some(parsed("myapp", "user-1", "", "user:profile.png", 1)),
        ),
        ("not an artifact URI", "https://example.com/file.txt", None),
        ("empty", "", None),
        (
            "missing version",
            "artifact://apps/myapp/users/user-1/sessions/sess-1/artifacts/report.pdf/versions/",
            None,
        ),
        (
            "non-numeric version",
            "artifact://apps/myapp/users/user-1/sessions/sess-1/artifacts/report.pdf/versions/latest",
            None,
        ),
        // A reference must name a stored version; 0 is the "latest" sentinel.
        (
            "zero version rejected",
            "artifact://apps/myapp/users/user-1/sessions/sess-1/artifacts/report.pdf/versions/0",
            None,
        ),
        // Segments are [^/]+; a path separator inside app/user/session is not representable in
        // a reference URI (stricter than adk-python).
        (
            "slash in app segment not representable",
            "artifact://apps/my/app/users/user-1/sessions/sess-1/artifacts/report.pdf/versions/1",
            None,
        ),
    ];
    for (name, uri, want) in cases {
        assert_eq!(
            parse_artifact_uri(uri),
            want,
            "{name}: parseArtifactURI({uri:?})"
        );
    }
}

#[test]
fn test_build_artifact_uri() {
    let cases = [
        (
            "session-scoped",
            parsed("myapp", "user-1", "sess-1", "report.pdf", 3),
            "artifact://apps/myapp/users/user-1/sessions/sess-1/artifacts/report.pdf/versions/3",
        ),
        (
            "user-scoped",
            parsed("myapp", "user-1", "", "user:profile.png", 1),
            "artifact://apps/myapp/users/user-1/artifacts/user:profile.png/versions/1",
        ),
    ];
    for (name, tc, want) in cases {
        let got = build_artifact_uri(
            &tc.app_name,
            &tc.user_id,
            &tc.session_id,
            &tc.file_name,
            tc.version,
        );
        assert_eq!(got, want, "{name}: buildArtifactURI(...)");
        let round_trip = parse_artifact_uri(&got).unwrap_or_else(|| {
            panic!("{name}: parseArtifactURI({got:?}) failed to parse output of buildArtifactURI")
        });
        assert_eq!(round_trip, tc, "{name}: round trip mismatch");
    }
}

#[test]
fn test_is_artifact_ref() {
    let file = |uri: &str| Part {
        file_data: Some(FileData {
            file_uri: uri.into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let cases = [
        (
            "artifact ref",
            Some(file("artifact://apps/a/users/u/artifacts/f/versions/1")),
            true,
        ),
        (
            "non-artifact file data",
            Some(file("gs://bucket/file.txt")),
            false,
        ),
        ("text part", Some(Part::from_text("hello")), false),
        ("nil part", None, false),
    ];
    for (name, part, want) in cases {
        assert_eq!(
            is_artifact_ref(part.as_ref()),
            want,
            "{name}: isArtifactRef"
        );
    }
}

#[test]
fn test_validate_artifact_reference_scope() {
    let cases = [
        (
            "same scope, session-scoped",
            parsed("app", "user", "sess", "f", 1),
            false,
        ),
        (
            "same app/user, ref is user-scoped",
            parsed("app", "user", "", "user:f", 1),
            false,
        ),
        (
            "cross-app escape",
            parsed("other-app", "user", "sess", "f", 1),
            true,
        ),
        (
            "cross-user escape",
            parsed("app", "other-user", "sess", "f", 1),
            true,
        ),
        (
            "cross-session escape",
            parsed("app", "user", "other-sess", "f", 1),
            true,
        ),
    ];
    for (name, tc, want_err) in cases {
        let err = validate_artifact_reference_scope("app", "user", "sess", &tc);
        assert_eq!(
            err.is_err(),
            want_err,
            "{name}: validateArtifactReferenceScope(...) err = {err:?}"
        );
    }
}
