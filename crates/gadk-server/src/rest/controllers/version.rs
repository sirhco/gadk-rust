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

//! The version controller and its route.

use std::sync::Arc;

use http::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::rest::http_error::encode_json_response;
use crate::rest::routing::{Request, Response, Route, controller_handler, route};

/// The version handshake the web UI performs.
#[derive(Debug, Default)]
pub struct VersionApiController;

impl VersionApiController {
    /// A new controller.
    pub fn new() -> Self {
        Self
    }

    /// The version response with gadk's values (`language` is `rust`, `language_version` the
    /// rustc release gadk was built with).
    pub async fn version_handler(&self, _req: Request) -> Response {
        encode_json_response(&VersionResponse::gadk(), StatusCode::OK)
    }
}

/// The version response body (the field names are the ADK API contract).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionResponse {
    /// The release this server was built from.
    pub version: String,
    /// The ADK implementation language.
    pub language: String,
    /// The toolchain version.
    pub language_version: String,
}

impl VersionResponse {
    /// This build's values: the gadk release, `rust`, and the rustc release recorded by
    /// gadk-platform's build script (the tail of [`gadk_platform::tracking_header`],
    /// `gadk-rust/<version> rust/<rustc>`).
    fn gadk() -> Self {
        let header = gadk_platform::tracking_header();
        let language_version = header
            .rsplit_once(" rust/")
            .map(|(_, v)| v.to_owned())
            .unwrap_or_default();
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            language: "rust".to_owned(),
            language_version,
        }
    }
}

/// The version route (public: no authentication).
pub(crate) fn version_routes(c: Arc<VersionApiController>) -> Vec<Route> {
    let mut r = route(
        "GetVersion",
        &[Method::GET],
        "/version",
        controller_handler!(c, version_handler),
    );
    r.public = true;
    vec![r]
}
