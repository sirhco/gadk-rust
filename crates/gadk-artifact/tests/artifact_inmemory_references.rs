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

//! Port of ADK-Go `artifact/inmemory_references_test.go`: `artifact://` reference parts in the
//! in-memory service.

use gadk_artifact::{
    ArtifactError, ArtifactService, GetArtifactVersionRequest, InMemoryArtifactService,
    LoadRequest, SaveRequest,
};
use gadk_genai::{FileData, Part};

/// Go `refPart`: a part whose `file_data` references the named artifact.
fn ref_part(app: &str, user: &str, session: &str, file: &str, version: i64) -> Part {
    uri_part(&format!(
        "artifact://apps/{app}/users/{user}/sessions/{session}/artifacts/{file}/versions/{version}"
    ))
}

fn uri_part(uri: &str) -> Part {
    Part {
        file_data: Some(FileData {
            file_uri: uri.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn save_req(session: &str, file: &str, part: Part) -> SaveRequest {
    SaveRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        session_id: session.into(),
        file_name: file.into(),
        part: Some(part),
        ..Default::default()
    }
}

fn load_req(session: &str, file: &str) -> LoadRequest {
    LoadRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        session_id: session.into(),
        file_name: file.into(),
        version: 0,
    }
}

fn text_of(part: Option<Part>) -> String {
    part.map(|p| p.text).unwrap_or_default()
}

/// Saves a target artifact, then an alias that references it, and loads the alias back.
async fn save_and_load(
    svc: &InMemoryArtifactService,
    file_name: &str,
    target_name: &str,
    content: &str,
) -> Option<Part> {
    let target = svc
        .save(save_req("sess", target_name, Part::from_text(content)))
        .await
        .unwrap_or_else(|e| panic!("Save(target {target_name:?}) failed: {e}"));
    svc.save(save_req(
        "sess",
        file_name,
        ref_part("app", "user", "sess", target_name, target.version),
    ))
    .await
    .unwrap_or_else(|e| panic!("Save(alias {file_name:?}) failed: {e}"));
    svc.load(load_req("sess", file_name))
        .await
        .unwrap_or_else(|e| panic!("Load({file_name:?}) failed: {e}"))
        .part
}

/// Go `makeChain`: stores f0..f{hops} in one session; f{hops} holds data and each earlier file
/// references the next, producing `hops` reference hops.
async fn make_chain(svc: &InMemoryArtifactService, hops: i64) {
    for i in (0..=hops).rev() {
        let name = format!("f{i}");
        let part = if i == hops {
            Part::from_text("chain end")
        } else {
            ref_part("app", "user", "s", &format!("f{}", i + 1), 1)
        };
        svc.save(save_req("s", &name, part))
            .await
            .unwrap_or_else(|e| panic!("Save({name:?}) failed: {e}"));
    }
}

#[tokio::test]
async fn test_in_memory_artifact_service_references() {
    // session-scoped reference resolves on load
    {
        let svc = InMemoryArtifactService::new();
        let part = save_and_load(&svc, "alias.txt", "target.txt", "target contents").await;
        assert_eq!(text_of(part), "target contents", "Load(alias)");
    }

    // user-scoped URI resolves a user-scoped target: a user-scoped URI (no sessions segment)
    // may point at a user-scoped file, which is stored under the shared user key. Both the
    // alias and the target are visible from every session of the user.
    {
        let svc = InMemoryArtifactService::new();
        let target = svc
            .save(save_req(
                "s1",
                "user:target.txt",
                Part::from_text("shared contents"),
            ))
            .await
            .unwrap();
        svc.save(save_req(
            "s1",
            "user:alias.txt",
            uri_part(&format!(
                "artifact://apps/app/users/user/artifacts/user:target.txt/versions/{}",
                target.version
            )),
        ))
        .await
        .unwrap();
        for session in ["s1", "s2"] {
            let resp = svc
                .load(load_req(session, "user:alias.txt"))
                .await
                .unwrap_or_else(|e| panic!("Load(user:alias) from {session:?} failed: {e}"));
            assert_eq!(
                text_of(resp.part),
                "shared contents",
                "Load(user:alias) from {session:?}"
            );
        }
    }

    // save rejects a malformed reference URI
    for uri in [
        "artifact://nonsense",
        "artifact://apps/app/users/user/sessions/s/artifacts/f.txt/versions/0",
    ] {
        let svc = InMemoryArtifactService::new();
        let err = svc
            .save(save_req("s", "alias.txt", uri_part(uri)))
            .await
            .expect_err(&format!("Save(alias) with URI {uri:?} succeeded"));
        assert!(
            err.to_string().contains("invalid artifact reference URI"),
            "Save(alias) error = {err}, want an invalid-reference-URI error"
        );
    }

    // save rejects out-of-scope references
    for (name, app, user, sess, file) in [
        ("cross-app", "other-app", "user", "sess", "target.txt"),
        ("cross-user", "app", "other-user", "sess", "target.txt"),
        ("cross-session", "app", "user", "other-sess", "target.txt"),
    ] {
        let svc = InMemoryArtifactService::new();
        assert!(
            svc.save(save_req(
                "sess",
                "alias.txt",
                ref_part(app, user, sess, file, 1)
            ))
            .await
            .is_err(),
            "Save(alias) with {name} reference succeeded, want error"
        );
    }

    // user-scoped alias to a session-scoped target loads for the owner
    {
        let svc = InMemoryArtifactService::new();
        let target = svc
            .save(save_req("s1", "target.txt", Part::from_text("hello")))
            .await
            .unwrap();
        svc.save(save_req(
            "s1",
            "user:alias",
            ref_part("app", "user", "s1", "target.txt", target.version),
        ))
        .await
        .unwrap();
        let resp = svc
            .load(load_req("s1", "user:alias"))
            .await
            .unwrap_or_else(|e| panic!("Load(user:alias) for owner failed: {e}"));
        assert_eq!(text_of(resp.part), "hello", "Load(user:alias)");
    }

    // user-scoped alias to a private target rejects a foreign session: the alias is reachable
    // from another session of the same user, but its target is private to s1.
    {
        let svc = InMemoryArtifactService::new();
        let target = svc
            .save(save_req("s1", "target.txt", Part::from_text("secret")))
            .await
            .unwrap();
        svc.save(save_req(
            "s1",
            "user:alias",
            ref_part("app", "user", "s1", "target.txt", target.version),
        ))
        .await
        .unwrap();
        let err = svc
            .load(load_req("s2", "user:alias"))
            .await
            .expect_err("Load(user:alias) from a foreign session succeeded, want a scope error");
        assert!(
            !matches!(err, ArtifactError::NotFound),
            "Load(user:alias) from a foreign session returned not-found, want a scope error: {err}"
        );
    }

    // user-scoped chain refuses a session-scoped hop: a hop through a user-scoped URI carries
    // no session, so a later session-scoped reference must fail the scope check even when it
    // names the session that saved the chain.
    {
        let svc = InMemoryArtifactService::new();
        let secret = svc
            .save(save_req("s1", "secret.txt", Part::from_text("secret")))
            .await
            .unwrap();
        let inner = svc
            .save(save_req(
                "s1",
                "user:inner.txt",
                ref_part("app", "user", "s1", "secret.txt", secret.version),
            ))
            .await
            .unwrap();
        svc.save(save_req(
            "s1",
            "user:alias.txt",
            uri_part(&format!(
                "artifact://apps/app/users/user/artifacts/user:inner.txt/versions/{}",
                inner.version
            )),
        ))
        .await
        .unwrap();
        for session in ["s1", "s2"] {
            let err = svc
                .load(load_req(session, "user:alias.txt"))
                .await
                .expect_err(&format!(
                    "Load(user:alias) from {session:?} succeeded, want a scope error"
                ));
            assert!(
                !matches!(err, ArtifactError::NotFound),
                "Load(user:alias) from {session:?} returned not-found: {err}"
            );
            assert!(
                err.to_string().contains(
                    "session-scoped artifact reference must stay within the same session scope"
                ),
                "Load(user:alias) from {session:?} error = {err}, want the session-scope error"
            );
        }
    }

    // a chain within the depth limit resolves
    {
        let svc = InMemoryArtifactService::new();
        make_chain(&svc, 5).await; // 5 hops, at the limit
        let resp = svc
            .load(load_req("s", "f0"))
            .await
            .unwrap_or_else(|e| panic!("Load(f0) within depth failed: {e}"));
        assert_eq!(text_of(resp.part), "chain end", "Load(f0)");
    }

    // a chain past the depth limit errors
    {
        let svc = InMemoryArtifactService::new();
        make_chain(&svc, 6).await; // 6 hops, one past the limit
        let err = svc
            .load(load_req("s", "f0"))
            .await
            .expect_err("Load over the depth limit succeeded, want the depth error");
        assert!(
            err.to_string().contains("maximum depth of 5"),
            "Load over the depth limit error = {err}, want the maximum-depth error"
        );
    }

    // a self-referencing chain is bounded by the depth limit
    {
        let svc = InMemoryArtifactService::new();
        svc.save(save_req(
            "s",
            "loop.txt",
            ref_part("app", "user", "s", "loop.txt", 1),
        ))
        .await
        .unwrap();
        let err = svc
            .load(load_req("s", "loop.txt"))
            .await
            .expect_err("Load of a self-referencing artifact succeeded, want the depth error");
        assert!(
            err.to_string().contains("maximum depth of 5"),
            "Load of a self-referencing artifact error = {err}, want the maximum-depth error"
        );
    }
}

// TestInMemoryArtifactVersionReferenceMimeType pins that a stored reference has no media type
// of its own: the target's type is only known once Load follows the reference. The reference
// carries a file_data.mime_type of its own so the assertion cannot pass by accident.
#[tokio::test]
async fn test_in_memory_artifact_version_reference_mime_type() {
    let svc = InMemoryArtifactService::new();
    let target = svc
        .save(save_req(
            "s",
            "target.png",
            Part::from_bytes(b"png".to_vec().into(), "image/png"),
        ))
        .await
        .unwrap();
    let mut alias_part = ref_part("app", "user", "s", "target.png", target.version);
    alias_part.file_data.as_mut().unwrap().mime_type = "image/png".into();
    let alias = svc
        .save(save_req("s", "alias.txt", alias_part))
        .await
        .unwrap();

    let got = svc
        .get_artifact_version(GetArtifactVersionRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: "s".into(),
            file_name: "alias.txt".into(),
            version: alias.version,
        })
        .await
        .unwrap();
    assert_eq!(
        got.artifact_version.unwrap().mime_type,
        "",
        "GetArtifactVersion(alias).MimeType: want empty for a reference"
    );

    let loaded = svc.load(load_req("s", "alias.txt")).await.unwrap();
    assert_eq!(
        loaded.part.unwrap().inline_data.unwrap().mime_type,
        "image/png",
        "Load(alias).Part.InlineData.MIMEType"
    );
}
