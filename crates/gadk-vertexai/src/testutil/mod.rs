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

//! Test servers for the Vertex AI services (feature `testutil`): [`ReplayServer`] serves Go's
//! recorded Agent Engine conversations strictly, and [`FakeAgentEngine`] is a stateful Agent
//! Engine fake. Both run on gadk-model's 127.0.0.1 fake HTTP server.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

pub mod engine;
pub mod replay;

use std::collections::HashMap;
use std::sync::Arc;

use gadk_platform::env::MapEnv;

pub use engine::FakeAgentEngine;
pub use replay::ReplayServer;

use crate::session::VertexAiSessionServiceConfig;

/// The project of Go's recordings (Go `ProjectID`).
pub const PROJECT_ID: &str = "adk-go-e2e";
/// The location of Go's recordings (Go `Location`).
pub const LOCATION: &str = "us-central1";
/// The reasoning engine of Go's recordings (Go `EngineID`).
pub const ENGINE_ID: &str = "1491331942182813696";
/// The second reasoning engine Go's tests name (Go `EngineID2`).
pub const ENGINE_ID2: &str = "6857370898194759680";

/// The replay fixture stem of a shared-suite case (Go `sanitizeFilename` without the
/// extension, prefixed): `"Test_vertexaiService_" + case` with `" "`, `,` and `/` each
/// replaced by `_` — e.g. `fixture_stem("Create/when_already_exists,_it_fails")` =
/// `"Test_vertexaiService_Create_when_already_exists__it_fails"`.
pub fn fixture_stem(case: &str) -> String {
    format!(
        "Test_vertexaiService_{}",
        case.replace([' ', ',', '/'], "_")
    )
}

/// A session config for a fake at `url`: project [`PROJECT_ID`], location [`LOCATION`], no
/// reasoning engine, `without_authentication`, `endpoint: Some(url)`, and an empty
/// environment.
pub fn session_config(url: &str) -> VertexAiSessionServiceConfig {
    VertexAiSessionServiceConfig {
        project_id: PROJECT_ID.to_owned(),
        location: LOCATION.to_owned(),
        reasoning_engine: None,
        endpoint: Some(url.to_owned()),
        credentials: None,
        without_authentication: true,
        http_client: None,
        env: Arc::new(MapEnv(HashMap::new())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn fixture_stems_name_committed_fixtures() {
        assert_eq!(
            fixture_stem("Create/when_already_exists,_it_fails"),
            "Test_vertexaiService_Create_when_already_exists__it_fails"
        );
        assert_eq!(fixture_stem("Get/ok"), "Test_vertexaiService_Get_ok");
        assert_eq!(fixture_stem("a b"), "Test_vertexaiService_a_b");
        for case in [
            "Create/full_key",
            "Create/generated_session_id",
            "Create/when_already_exists,_it_fails",
            "Get/ok",
            "List",
            "Delete",
            "delete_session_respects_user_id",
            "AppendEvent/with_all_fields",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata/replays")
                .join(format!("{}.json", fixture_stem(case)));
            assert!(path.is_file(), "{}", path.display());
        }
    }

    #[test]
    fn session_config_targets_the_fake() {
        let config = session_config("http://127.0.0.1:9");
        assert_eq!(config.project_id, "adk-go-e2e");
        assert_eq!(config.location, "us-central1");
        assert_eq!(config.reasoning_engine, None);
        assert_eq!(config.endpoint.as_deref(), Some("http://127.0.0.1:9"));
        assert!(config.without_authentication);
        assert_eq!(config.env.var("GOOGLE_APPLICATION_CREDENTIALS"), None);
    }
}
