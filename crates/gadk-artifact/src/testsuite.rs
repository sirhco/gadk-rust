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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A reusable, backend-agnostic conformance suite for [`ArtifactService`] implementations.
//!
//! Enabled by the `testsuite` feature. Call [`run`] from a backend's tests with a factory that
//! returns a fresh service.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use gadk_genai::Part;
use serde_json::{Map, Value, json};

use crate::service::{
    ArtifactError, ArtifactService, ArtifactVersion, DeleteRequest, GetArtifactVersionRequest,
    ListRequest, LoadRequest, SaveRequest, VersionsRequest,
};

/// Fails the suite with the scenario/sub-step prefix.
macro_rules! fail {
    ($step:expr, $($arg:tt)*) => {
        panic!("artifact testsuite/{}: {}", $step, format!($($arg)*))
    };
}

/// Runs the artifact conformance suite against fresh services from `factory`, one per scenario;
/// panics with `artifact testsuite/<scenario>: <message>` on failure.
pub async fn run<F, Fut>(factory: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn ArtifactService>>,
{
    basic(factory().await).await;
    empty(factory().await).await;
    user_scoped(factory().await).await;
    list_session_named_user(factory().await).await;
    get_artifact_version(factory().await).await;
}

fn bytes(data: &str, mime_type: &str) -> Part {
    Part::from_bytes(data.as_bytes().to_vec().into(), mime_type)
}

fn meta(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

fn is_not_found<T>(r: &Result<T, ArtifactError>) -> bool {
    matches!(r, Err(ArtifactError::NotFound))
}

const APP: &str = "testapp";
const USER: &str = "testuser";
const SESSION: &str = "testsession";

fn save_req(session_id: &str, file_name: &str, part: Part, md: Map<String, Value>) -> SaveRequest {
    SaveRequest {
        app_name: APP.into(),
        user_id: USER.into(),
        session_id: session_id.into(),
        file_name: file_name.into(),
        part: Some(part),
        custom_metadata: md,
        ..Default::default()
    }
}

fn load_req(session_id: &str, file_name: &str, version: i64) -> LoadRequest {
    LoadRequest {
        app_name: APP.into(),
        user_id: USER.into(),
        session_id: session_id.into(),
        file_name: file_name.into(),
        version,
    }
}

fn delete_req(session_id: &str, file_name: &str, version: i64) -> DeleteRequest {
    DeleteRequest {
        app_name: APP.into(),
        user_id: USER.into(),
        session_id: session_id.into(),
        file_name: file_name.into(),
        version,
    }
}

fn list_req(session_id: &str) -> ListRequest {
    ListRequest {
        app_name: APP.into(),
        user_id: USER.into(),
        session_id: session_id.into(),
    }
}

fn versions_req(session_id: &str, file_name: &str) -> VersionsRequest {
    VersionsRequest {
        app_name: APP.into(),
        user_id: USER.into(),
        session_id: session_id.into(),
        file_name: file_name.into(),
    }
}

fn gav_req(session_id: &str, file_name: &str, version: i64) -> GetArtifactVersionRequest {
    GetArtifactVersionRequest {
        app_name: APP.into(),
        user_id: USER.into(),
        session_id: session_id.into(),
        file_name: file_name.into(),
        version,
    }
}

/// Deletes, failing the suite on error.
async fn must_delete(srv: &dyn ArtifactService, step: &str, req: DeleteRequest) {
    if let Err(e) = srv.delete(req).await {
        fail!(step, "Delete failed: {e}");
    }
}

/// Scenario: save, load, list, version and delete session-scoped artifacts.
async fn basic(srv: Arc<dyn ArtifactService>) {
    let srv = srv.as_ref();
    let s = "ArtifactService";

    // Save these artifacts for later subtests.
    let test_data = [
        // file1.
        ("file1", 1, bytes("file v1", "text/plain")),
        ("file1", 2, bytes("file v2", "text/plain")),
        ("file1", 3, bytes("file v3", "text/plain")),
        // file2.
        ("file2", 1, bytes("file v3", "text/plain")),
        // file3.
        ("file3", 1, Part::from_text("file v1")),
    ];
    for (i, (file_name, want_version, part)) in test_data.into_iter().enumerate() {
        match srv
            .save(save_req(SESSION, file_name, part, Map::new()))
            .await
        {
            Ok(got) if got.version == want_version => {}
            got => fail!(s, "[{i}] Save() = {got:?}, want Ok({want_version})"),
        }
    }

    // Load
    for (name, version, want) in [
        ("latest", 0, bytes("file v3", "text/plain")),
        ("ver=1", 1, bytes("file v1", "text/plain")),
        ("ver=2", 2, bytes("file v2", "text/plain")),
    ] {
        match srv.load(load_req(SESSION, "file1", version)).await {
            Ok(got) if got.part.as_ref() == Some(&want) => {}
            got => fail!(
                format!("{s}/Load/{name}"),
                "Load({version}) = {got:?}, want Ok({want:?})"
            ),
        }
    }

    // LoadNonexistentVersion
    let got = srv.load(load_req(SESSION, "file1", 99)).await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/LoadNonexistentVersion"),
            "Load('file1', ver=99) = {got:?}, want NotFound"
        );
    }

    // List
    let mut got = match srv.list(list_req(SESSION)).await {
        Ok(r) => r.file_names,
        Err(e) => fail!(format!("{s}/List"), "List() failed: {e}"),
    };
    got.sort();
    if got != ["file1", "file2", "file3"] {
        fail!(
            format!("{s}/List"),
            "List() = {got:?}, want [file1 file2 file3]"
        );
    }

    // Versions
    let mut got = match srv.versions(versions_req(SESSION, "file1")).await {
        Ok(r) => r.versions,
        Err(e) => fail!(format!("{s}/Versions"), "Versions() failed: {e}"),
    };
    got.sort_unstable();
    if got != [1, 2, 3] {
        fail!(
            format!("{s}/Versions"),
            "Versions('file1') = {got:?}, want [1 2 3]"
        );
    }

    // Delete file1 version 3
    must_delete(srv, s, delete_req(SESSION, "file1", 3)).await;

    // LoadAfterDeleteVersion3
    let step = format!("{s}/LoadAfterDeleteVersion3");
    match srv.load(load_req(SESSION, "file1", 0)).await {
        Ok(r) if r.part == Some(bytes("file v2", "text/plain")) => {}
        Ok(r) => fail!(step, "Load('file1') = {:?}, want file v2", r.part),
        Err(e) => fail!(step, "Load('file1') failed: {e}"),
    }
    let deleted = srv.load(load_req(SESSION, "file1", 3)).await;
    if !is_not_found(&deleted) {
        fail!(step, "Load('file1', ver=3) = {deleted:?}, want NotFound");
    }

    must_delete(srv, s, delete_req(SESSION, "file1", 0)).await;

    // LoadAfterDelete
    let got = srv.load(load_req(SESSION, "file1", 0)).await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/LoadAfterDelete"),
            "Load('file1') = {got:?}, want NotFound"
        );
    }

    // ListAfterDelete
    let mut got = match srv.list(list_req(SESSION)).await {
        Ok(r) => r.file_names,
        Err(e) => fail!(format!("{s}/ListAfterDelete"), "List() failed: {e}"),
    };
    got.sort();
    if got != ["file2", "file3"] {
        fail!(
            format!("{s}/ListAfterDelete"),
            "List() = {got:?}, want [file2 file3]"
        );
    }

    // VersionsAfterDelete
    let got = srv.versions(versions_req(SESSION, "file1")).await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/VersionsAfterDelete"),
            "Versions('file1') = {got:?}, want NotFound"
        );
    }

    // Clean up
    must_delete(srv, s, delete_req(SESSION, "file2", 0)).await;
    must_delete(srv, s, delete_req(SESSION, "file3", 0)).await;
}

/// Scenario: `user:` artifacts are shared across sessions of one user.
async fn user_scoped(srv: Arc<dyn ArtifactService>) {
    let srv = srv.as_ref();
    let s = "ArtifactService_UserScoped";

    // Save these artifacts for later subtests.
    let test_data = [
        // file1.
        (
            "user:file1",
            "save-session-1",
            1,
            bytes("file v1", "text/plain"),
            meta(json!({"version": "v1"})),
        ),
        (
            "user:file1",
            "save-session-2",
            2,
            bytes("file v2", "text/plain"),
            meta(json!({"version": "v2"})),
        ),
        (
            "user:file1",
            "save-session-3",
            3,
            bytes("file v3", "text/plain"),
            meta(json!({"version": "v3"})),
        ),
        // file2.
        (
            "file2",
            SESSION,
            1,
            bytes("file v3", "text/plain"),
            Map::new(),
        ),
        // file3.
        (
            "user:file3",
            "another-save-session",
            1,
            Part::from_text("file v1"),
            Map::new(),
        ),
    ];
    for (i, (file_name, session_id, want_version, part, md)) in test_data.into_iter().enumerate() {
        match srv.save(save_req(session_id, file_name, part, md)).await {
            Ok(got) if got.version == want_version => {}
            got => fail!(s, "[{i}] Save() = {got:?}, want Ok({want_version})"),
        }
    }

    // Load
    for (name, version, want) in [
        ("latest", 0, bytes("file v3", "text/plain")),
        ("ver=1", 1, bytes("file v1", "text/plain")),
        ("ver=2", 2, bytes("file v2", "text/plain")),
    ] {
        match srv
            .load(load_req(
                "'user' should be used instead",
                "user:file1",
                version,
            ))
            .await
        {
            Ok(got) if got.part.as_ref() == Some(&want) => {}
            got => fail!(
                format!("{s}/Load/{name}"),
                "Load({version}) = {got:?}, want Ok({want:?})"
            ),
        }
    }

    // List
    let got = match srv.list(list_req(SESSION)).await {
        Ok(r) => r.file_names,
        Err(e) => fail!(format!("{s}/List"), "List() failed: {e}"),
    };
    if got != ["file2", "user:file1", "user:file3"] {
        fail!(
            format!("{s}/List"),
            "List() = {got:?}, want [file2 user:file1 user:file3]"
        );
    }

    // Versions
    let mut got = match srv.versions(versions_req(SESSION, "user:file1")).await {
        Ok(r) => r.versions,
        Err(e) => fail!(format!("{s}/Versions"), "Versions() failed: {e}"),
    };
    got.sort_unstable();
    if got != [1, 2, 3] {
        fail!(
            format!("{s}/Versions"),
            "Versions('user:file1') = {got:?}, want [1 2 3]"
        );
    }

    // GetArtifactVersion
    for (name, version, want_version, want_md) in [
        ("latest", 0, 3, meta(json!({"version": "v3"}))),
        ("specific", 2, 2, meta(json!({"version": "v2"}))),
    ] {
        let step = format!("{s}/GetArtifactVersion/{name}");
        let req = gav_req("'user' should be used instead", "user:file1", version);
        match srv.get_artifact_version(req).await {
            Ok(r) => assert_artifact_version(
                &step,
                r.artifact_version.as_ref(),
                want_version,
                "text/plain",
                &want_md,
            ),
            Err(e) => fail!(step, "GetArtifactVersion({version}) failed: {e}"),
        }
    }

    // Delete user:file1 version 3
    must_delete(srv, s, delete_req(SESSION, "user:file1", 3)).await;

    // GetArtifactVersionAfterDeleteLatest
    let step = format!("{s}/GetArtifactVersionAfterDeleteLatest");
    match srv
        .get_artifact_version(gav_req("another-read-session", "user:file1", 0))
        .await
    {
        Ok(r) => assert_artifact_version(
            &step,
            r.artifact_version.as_ref(),
            2,
            "text/plain",
            &meta(json!({"version": "v2"})),
        ),
        Err(e) => fail!(step, "GetArtifactVersion() failed: {e}"),
    }

    // LoadAfterDeleteVersion3
    let step = format!("{s}/LoadAfterDeleteVersion3");
    match srv.load(load_req(SESSION, "user:file1", 0)).await {
        Ok(r) if r.part == Some(bytes("file v2", "text/plain")) => {}
        Ok(r) => fail!(step, "Load('user:file1') = {:?}, want file v2", r.part),
        Err(e) => fail!(step, "Load('user:file1') failed: {e}"),
    }

    must_delete(srv, s, delete_req(SESSION, "user:file1", 0)).await;

    // LoadAfterDelete
    let got = srv.load(load_req(SESSION, "user:file1", 0)).await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/LoadAfterDelete"),
            "Load('user:file1') = {got:?}, want NotFound"
        );
    }

    // ListAfterDelete
    let mut got = match srv.list(list_req(SESSION)).await {
        Ok(r) => r.file_names,
        Err(e) => fail!(format!("{s}/ListAfterDelete"), "List() failed: {e}"),
    };
    got.sort();
    if got != ["file2", "user:file3"] {
        fail!(
            format!("{s}/ListAfterDelete"),
            "List() = {got:?}, want [file2 user:file3]"
        );
    }

    // VersionsAfterDelete
    let got = srv.versions(versions_req(SESSION, "user:file1")).await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/VersionsAfterDelete"),
            "Versions('user:file1') = {got:?}, want NotFound"
        );
    }

    // Clean up
    must_delete(srv, s, delete_req(SESSION, "file2", 0)).await;
    must_delete(srv, s, delete_req(SESSION, "user:file3", 0)).await;
}

/// Scenario: operations on an empty service.
async fn empty(srv: Arc<dyn ArtifactService>) {
    let srv = srv.as_ref();
    let s = "ArtifactService_Empty";
    let req = |file: &str| (APP, "user", "session", file.to_owned());

    let (app, user, session, file) = req("file");
    let got = srv
        .load(LoadRequest {
            app_name: app.into(),
            user_id: user.into(),
            session_id: session.into(),
            file_name: file,
            version: 0,
        })
        .await;
    if !is_not_found(&got) {
        fail!(format!("{s}/Load"), "Load() = {got:?}, want NotFound");
    }

    let (app, user, session, _) = req("");
    if let Err(e) = srv
        .list(ListRequest {
            app_name: app.into(),
            user_id: user.into(),
            session_id: session.into(),
        })
        .await
    {
        fail!(format!("{s}/List"), "List() failed: {e}");
    }

    let (app, user, session, file) = req("file1");
    if let Err(e) = srv
        .delete(DeleteRequest {
            app_name: app.into(),
            user_id: user.into(),
            session_id: session.into(),
            file_name: file,
            version: 0,
        })
        .await
    {
        fail!(format!("{s}/Delete"), "Delete() failed: {e}");
    }

    let (app, user, session, file) = req("file1");
    let got = srv
        .versions(VersionsRequest {
            app_name: app.into(),
            user_id: user.into(),
            session_id: session.into(),
            file_name: file,
        })
        .await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/Versions"),
            "Versions() = {got:?}, want NotFound"
        );
    }

    let (app, user, session, file) = req("file1");
    let got = srv
        .get_artifact_version(GetArtifactVersionRequest {
            app_name: app.into(),
            user_id: user.into(),
            session_id: session.into(),
            file_name: file,
            version: 0,
        })
        .await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/GetArtifactVersion"),
            "GetArtifactVersion() = {got:?}, want NotFound"
        );
    }
}

/// Scenario: a session literally named `user` keeps its private files, which other sessions do
/// not see, and lists each file name once however many versions it has.
async fn list_session_named_user(srv: Arc<dyn ArtifactService>) {
    let srv = srv.as_ref();
    let s = "ArtifactService_ListSessionNamedUser";
    for (session_id, file_name) in [
        ("user", "private.txt"),
        ("user", "private.txt"), // Multiple versions must not duplicate filenames.
        ("user", "user:shared.txt"),
        ("other-session", "user:shared.txt"),
        ("other-session", "other.txt"),
    ] {
        let req = SaveRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: session_id.into(),
            file_name: file_name.into(),
            part: Some(Part::from_text("fixture")),
            ..Default::default()
        };
        if let Err(e) = srv.save(req).await {
            fail!(s, "Save failed: {e}");
        }
    }

    for (name, session_id, want) in [
        (
            "owner retains private and shared artifacts",
            "user",
            vec!["private.txt", "user:shared.txt"],
        ),
        (
            "other session sees only its own and shared artifacts",
            "other-session",
            vec!["other.txt", "user:shared.txt"],
        ),
        (
            "empty session sees only shared artifacts",
            "empty-session",
            vec!["user:shared.txt"],
        ),
    ] {
        let step = format!("{s}/{name}");
        let req = ListRequest {
            app_name: "app".into(),
            user_id: "u".into(),
            session_id: session_id.into(),
        };
        match srv.list(req).await {
            Ok(r) if r.file_names == want => {}
            Ok(r) => fail!(
                step,
                "List must return only the session's own and user-scoped filenames, sorted and without duplicates: got {:?}, want {want:?}",
                r.file_names
            ),
            Err(e) => fail!(step, "List failed: {e}"),
        }
    }
}

/// Scenario: metadata of specific versions, the latest version, and missing versions or files.
async fn get_artifact_version(srv: Arc<dyn ArtifactService>) {
    let srv = srv.as_ref();
    let s = "ArtifactService_GetArtifactVersion";
    let file_name = "verfile";

    for (i, (part, md)) in [
        (bytes("v1", "text/plain"), Map::new()),
        (
            bytes("v2", "image/png"),
            meta(json!({"version": "v2", "count": 2})),
        ),
        (
            Part::from_text("v3"),
            meta(json!({"version": "v3", "enabled": true})),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let want_version = i64::try_from(i + 1).unwrap();
        match srv.save(save_req(SESSION, file_name, part, md)).await {
            Ok(got) if got.version == want_version => {}
            got => fail!(s, "[{i}] Save() = {got:?}, want Ok({want_version})"),
        }
    }

    let mut canonical_uris: BTreeMap<i64, String> = BTreeMap::new();
    for (name, version, want_version, want_mime, want_md) in [
        (
            "latest",
            0,
            3,
            "text/plain",
            meta(json!({"version": "v3", "enabled": "true"})),
        ),
        (
            "specific",
            2,
            2,
            "image/png",
            meta(json!({"version": "v2", "count": "2"})),
        ),
        ("empty metadata", 1, 1, "text/plain", Map::new()),
    ] {
        let step = format!("{s}/GetArtifactVersion_{name}");
        match srv
            .get_artifact_version(gav_req(SESSION, file_name, version))
            .await
        {
            Ok(r) => {
                let av = r.artifact_version.as_ref();
                assert_artifact_version(&step, av, want_version, want_mime, &want_md);
                if let Some(av) = av {
                    canonical_uris.insert(want_version, av.canonical_uri.clone());
                }
            }
            Err(e) => fail!(step, "GetArtifactVersion({version}) failed: {e}"),
        }
    }
    for (version, uri) in &canonical_uris {
        for (other_version, other_uri) in &canonical_uris {
            if version < other_version && uri == other_uri {
                fail!(
                    s,
                    "versions {version} and {other_version} have the same CanonicalURI"
                );
            }
        }
    }
    for (name, session_id, file) in [
        ("different session", "another-session", file_name),
        ("different file", SESSION, "another-file"),
    ] {
        let step = format!("{s}/GetArtifactVersion_URI_{name}");
        if let Err(e) = srv
            .save(save_req(
                session_id,
                file,
                Part::from_text("other"),
                Map::new(),
            ))
            .await
        {
            fail!(step, "Save() failed: {e}");
        }
        let got = match srv.get_artifact_version(gav_req(session_id, file, 0)).await {
            Ok(r) => r
                .artifact_version
                .map(|av| av.canonical_uri)
                .unwrap_or_default(),
            Err(e) => fail!(step, "GetArtifactVersion() failed: {e}"),
        };
        for (version, uri) in &canonical_uris {
            if &got == uri {
                fail!(
                    step,
                    "distinct artifact shares CanonicalURI with version {version}"
                );
            }
        }
    }

    let got = srv
        .get_artifact_version(gav_req(SESSION, file_name, 99))
        .await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/GetArtifactVersion_nonexistent-version"),
            "GetArtifactVersion(99) = {got:?}, want NotFound"
        );
    }
    let got = srv
        .get_artifact_version(gav_req(SESSION, "does-not-exist", 0))
        .await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/GetArtifactVersion_nonexistent-file"),
            "GetArtifactVersion(missing file) = {got:?}, want NotFound"
        );
    }

    must_delete(srv, s, delete_req(SESSION, file_name, 3)).await;
    let step = format!("{s}/GetArtifactVersionAfterDeleteLatest");
    match srv
        .get_artifact_version(gav_req(SESSION, file_name, 0))
        .await
    {
        Ok(r) => assert_artifact_version(
            &step,
            r.artifact_version.as_ref(),
            2,
            "image/png",
            &meta(json!({"version": "v2", "count": "2"})),
        ),
        Err(e) => fail!(step, "GetArtifactVersion() failed: {e}"),
    }

    must_delete(srv, s, delete_req(SESSION, file_name, 0)).await;
    let got = srv
        .get_artifact_version(gav_req(SESSION, file_name, 0))
        .await;
    if !is_not_found(&got) {
        fail!(
            format!("{s}/GetArtifactVersionAfterDelete"),
            "GetArtifactVersion() = {got:?}, want NotFound"
        );
    }
}

/// Asserts the version, media type and custom metadata of an [`ArtifactVersion`].
fn assert_artifact_version(
    step: &str,
    got: Option<&ArtifactVersion>,
    want_version: i64,
    want_mime_type: &str,
    want_metadata: &Map<String, Value>,
) {
    let Some(got) = got else {
        fail!(
            step,
            "GetArtifactVersion().ArtifactVersion = None, want Some"
        );
    };
    if got.version != want_version {
        fail!(
            step,
            "ArtifactVersion.Version = {}, want {want_version}",
            got.version
        );
    }
    if got.mime_type != want_mime_type {
        fail!(
            step,
            "ArtifactVersion.MimeType = {:?}, want {want_mime_type:?}",
            got.mime_type
        );
    }
    if &got.custom_metadata != want_metadata {
        fail!(
            step,
            "ArtifactVersion.CustomMetadata = {:?}, want {want_metadata:?}",
            got.custom_metadata
        );
    }
    if got.canonical_uri.is_empty() {
        fail!(step, "ArtifactVersion.CanonicalURI is empty");
    }
    if got.create_time == DateTime::<Utc>::default() {
        fail!(step, "ArtifactVersion.CreateTime is zero");
    }
}
