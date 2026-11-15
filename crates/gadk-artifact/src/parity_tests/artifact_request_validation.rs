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

//! Parity target for Go artifact/request_validation_test.go.

use gadk_genai::{FileData, Part};
use serde_json::Map;

use crate::service::{
    ArtifactError, DeleteRequest, GetArtifactVersionRequest, ListRequest, LoadRequest,
    RequiredField, SaveRequest, VersionsRequest, validate_required_strings,
};

/// Port of Go executeValidatorTestCases: `want` is `None` for a valid request, else the exact
/// Go `err.Error()` text, compared against the `InvalidRequest` payload.
fn check(case: &str, got: Result<(), ArtifactError>, want: Option<&str>) {
    match (got, want) {
        (Ok(()), None) => {}
        (Err(ArtifactError::InvalidRequest(m)), Some(w)) => assert_eq!(m, w, "{case}"),
        (got, want) => panic!("{case}: Validate() = {got:?}, want {want:?}"),
    }
}

fn bytes_part() -> Option<Part> {
    Some(Part::from_bytes(b"data".to_vec().into(), "text/plain"))
}

#[test]
fn test_save_request_validate() {
    let full = |part: Option<Part>| SaveRequest {
        app_name: "MyApp".into(),
        user_id: "user-123".into(),
        session_id: "sess-abc".into(),
        file_name: "file.txt".into(),
        part,
        ..Default::default()
    };
    let cases: Vec<(&str, SaveRequest, Option<&str>)> = vec![
        ("Valid request from bytes", full(bytes_part()), None),
        (
            "Valid request from text",
            full(Some(Part::from_text("data"))),
            None,
        ),
        (
            "Missing AppName",
            SaveRequest {
                app_name: String::new(),
                ..full(bytes_part())
            },
            Some("invalid save request: missing required fields: AppName"),
        ),
        (
            "Missing multiple fields",
            SaveRequest {
                app_name: "MyApp".into(),
                part: bytes_part(),
                ..Default::default()
            },
            Some("invalid save request: missing required fields: UserID, SessionID, FileName"),
        ),
        (
            "Missing Part (nil slice)",
            full(None),
            Some("invalid save request: missing required fields: Part"),
        ),
        (
            "Missing Part.Inline (nil slice)",
            full(Some(Part::from_function_call("example", Map::new()))),
            Some("invalid save request: Part.InlineData or Part.Text has to be set"),
        ),
        (
            "Completely empty request",
            SaveRequest::default(),
            Some(
                "invalid save request: missing required fields: AppName, UserID, SessionID, FileName, Part",
            ),
        ),
        (
            "FileName with path separator",
            SaveRequest {
                file_name: "path/to/file.txt".into(),
                ..full(bytes_part())
            },
            Some("invalid name: filename cannot contain path separators"),
        ),
        (
            // An artifact reference part carries neither text nor inline data. The shared
            // contract must reject it so backends that don't understand references (e.g. GCS)
            // keep their behavior; only the in-memory service's Save tolerates it.
            "Artifact reference part is rejected by the shared contract",
            SaveRequest {
                file_name: "alias.txt".into(),
                ..full(Some(Part {
                    file_data: Some(FileData {
                        file_uri: "artifact://apps/MyApp/users/user-123/sessions/sess-abc/artifacts/file.txt/versions/1".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }))
            },
            Some("invalid save request: Part.InlineData or Part.Text has to be set"),
        ),
    ];
    for (name, req, want) in cases {
        check(&format!("SaveRequest_{name}"), req.validate(), want);
    }
}

#[test]
fn test_load_request_validate() {
    let full = LoadRequest {
        app_name: "MyApp".into(),
        user_id: "user-123".into(),
        session_id: "sess-abc".into(),
        file_name: "file.txt".into(),
        ..Default::default()
    };
    let cases: Vec<(&str, LoadRequest, Option<&str>)> = vec![
        ("Valid request", full.clone(), None),
        (
            "Missing AppName",
            LoadRequest {
                app_name: String::new(),
                ..full.clone()
            },
            Some("invalid load request: missing required fields: AppName"),
        ),
        (
            "Missing multiple fields",
            LoadRequest {
                app_name: "MyApp".into(),
                ..Default::default()
            },
            Some("invalid load request: missing required fields: UserID, SessionID, FileName"),
        ),
        (
            "Completely empty request",
            LoadRequest::default(),
            Some(
                "invalid load request: missing required fields: AppName, UserID, SessionID, FileName",
            ),
        ),
        (
            "FileName with path separator",
            LoadRequest {
                file_name: "a/b.txt".into(),
                ..full
            },
            Some("invalid name: filename cannot contain path separators"),
        ),
    ];
    for (name, req, want) in cases {
        check(&format!("LoadRequest_{name}"), req.validate(), want);
    }
}

#[test]
fn test_delete_request_validate() {
    let full = DeleteRequest {
        app_name: "MyApp".into(),
        user_id: "user-123".into(),
        session_id: "sess-abc".into(),
        file_name: "file.txt".into(),
        ..Default::default()
    };
    let cases: Vec<(&str, DeleteRequest, Option<&str>)> = vec![
        ("Valid request", full.clone(), None),
        (
            "Missing AppName",
            DeleteRequest {
                app_name: String::new(),
                ..full.clone()
            },
            Some("invalid delete request: missing required fields: AppName"),
        ),
        (
            "Missing multiple fields",
            DeleteRequest {
                app_name: "MyApp".into(),
                ..Default::default()
            },
            Some("invalid delete request: missing required fields: UserID, SessionID, FileName"),
        ),
        (
            "Completely empty request",
            DeleteRequest::default(),
            Some(
                "invalid delete request: missing required fields: AppName, UserID, SessionID, FileName",
            ),
        ),
        (
            "FileName with path separator",
            DeleteRequest {
                file_name: "dir/file.txt".into(),
                ..full
            },
            Some("invalid name: filename cannot contain path separators"),
        ),
    ];
    for (name, req, want) in cases {
        check(&format!("DeleteRequest_{name}"), req.validate(), want);
    }
}

#[test]
fn test_list_request_validate() {
    let full = ListRequest {
        app_name: "MyApp".into(),
        user_id: "user-123".into(),
        session_id: "sess-abc".into(),
    };
    let cases: Vec<(&str, ListRequest, Option<&str>)> = vec![
        ("Valid request", full.clone(), None),
        (
            "Missing AppName",
            ListRequest {
                app_name: String::new(),
                ..full
            },
            Some("invalid list request: missing required fields: AppName"),
        ),
        (
            "Missing multiple fields",
            ListRequest {
                app_name: "MyApp".into(),
                ..Default::default()
            },
            Some("invalid list request: missing required fields: UserID, SessionID"),
        ),
        (
            "Completely empty request",
            ListRequest::default(),
            Some("invalid list request: missing required fields: AppName, UserID, SessionID"),
        ),
    ];
    for (name, req, want) in cases {
        check(&format!("ListRequest_{name}"), req.validate(), want);
    }
}

#[test]
fn test_versions_request_validate() {
    let full = VersionsRequest {
        app_name: "MyApp".into(),
        user_id: "user-123".into(),
        session_id: "sess-abc".into(),
        file_name: "file.txt".into(),
    };
    let cases: Vec<(&str, VersionsRequest, Option<&str>)> = vec![
        ("Valid request", full.clone(), None),
        (
            "Missing AppName",
            VersionsRequest {
                app_name: String::new(),
                ..full.clone()
            },
            Some("invalid versions request: missing required fields: AppName"),
        ),
        (
            "Missing multiple fields",
            VersionsRequest {
                app_name: "MyApp".into(),
                ..Default::default()
            },
            Some("invalid versions request: missing required fields: UserID, SessionID, FileName"),
        ),
        (
            "Completely empty request",
            VersionsRequest::default(),
            Some(
                "invalid versions request: missing required fields: AppName, UserID, SessionID, FileName",
            ),
        ),
        (
            "FileName with path separator",
            VersionsRequest {
                file_name: "folder/file.txt".into(),
                ..full
            },
            Some("invalid name: filename cannot contain path separators"),
        ),
    ];
    for (name, req, want) in cases {
        check(&format!("VersionsRequest_{name}"), req.validate(), want);
    }
}

#[test]
fn test_get_artifact_version_request_validate() {
    let full = GetArtifactVersionRequest {
        app_name: "MyApp".into(),
        user_id: "user-123".into(),
        session_id: "sess-abc".into(),
        file_name: "file.txt".into(),
        ..Default::default()
    };
    let cases: Vec<(&str, GetArtifactVersionRequest, Option<&str>)> = vec![
        ("Valid request", full.clone(), None),
        (
            "Missing AppName",
            GetArtifactVersionRequest {
                app_name: String::new(),
                ..full.clone()
            },
            Some("invalid get artifact version request: missing required fields: AppName"),
        ),
        (
            "Missing multiple fields",
            GetArtifactVersionRequest {
                app_name: "MyApp".into(),
                ..Default::default()
            },
            Some(
                "invalid get artifact version request: missing required fields: UserID, SessionID, FileName",
            ),
        ),
        (
            "Completely empty request",
            GetArtifactVersionRequest::default(),
            Some(
                "invalid get artifact version request: missing required fields: AppName, UserID, SessionID, FileName",
            ),
        ),
        (
            "FileName with path separator",
            GetArtifactVersionRequest {
                file_name: "folder/file.txt".into(),
                ..full
            },
            Some("invalid name: filename cannot contain path separators"),
        ),
    ];
    for (name, req, want) in cases {
        check(
            &format!("GetArtifactVersionRequest_{name}"),
            req.validate(),
            want,
        );
    }
}

#[test]
fn test_validate_required_strings() {
    let cases: Vec<(&str, Vec<RequiredField<'_>>, Vec<&str>)> = vec![
        (
            "No missing fields",
            vec![
                RequiredField {
                    name: "FieldA",
                    value: "valueA",
                },
                RequiredField {
                    name: "FieldB",
                    value: "valueB",
                },
            ],
            vec![],
        ),
        (
            "One missing field",
            vec![
                RequiredField {
                    name: "FieldA",
                    value: "valueA",
                },
                RequiredField {
                    name: "FieldB",
                    value: "",
                },
            ],
            vec!["FieldB"],
        ),
        (
            "Multiple missing fields",
            vec![
                RequiredField {
                    name: "FieldA",
                    value: "",
                },
                RequiredField {
                    name: "FieldB",
                    value: "valueB",
                },
                RequiredField {
                    name: "FieldC",
                    value: "",
                },
            ],
            vec!["FieldA", "FieldC"],
        ),
        ("Empty input slice", vec![], vec![]),
    ];
    for (name, input, want) in cases {
        assert_eq!(validate_required_strings(&input), want, "{name}");
    }
}
