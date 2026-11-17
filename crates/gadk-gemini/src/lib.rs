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

//! gadk-gemini: Gemini API and Vertex AI model implementation.

pub mod apigee;
mod auth;
mod config;
mod continuation;
mod error;
mod live;
mod model;
mod request;
mod transport;

pub use config::{Auth, CLOUD_PLATFORM_SCOPE, ClientConfig, ResolvedConfig, resolve};
pub use error::GeminiError;
pub use gadk_platform::env::{EnvProvider, MapEnv, ProcessEnv};
pub use live::{LiveConnection, RealtimeInput};
pub use model::GeminiModel;

/// Internals exposed for the integration tests and the engine's tests. Not part of the
/// public API.
#[doc(hidden)]
pub mod internal {
    pub use crate::live::{
        classify_ws_error, close_frame_error, gorilla_close_text, wrap_conn_error,
    };
    pub use crate::request::{build_request_body, preserve_empty_text_thought_signature_parts};
    pub use tokio_tungstenite::tungstenite;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    /// R2: the environment types moved to `gadk_platform::env`; the old paths name the same types.
    #[test]
    fn env_reexports_are_the_platform_types() {
        let env: gadk_platform::env::MapEnv =
            crate::MapEnv(HashMap::from([("K".to_string(), "v".to_string())]));
        let provider: &dyn crate::EnvProvider = &env;
        assert_eq!(provider.var("K").as_deref(), Some("v"));
        let _: gadk_platform::env::ProcessEnv = crate::ProcessEnv;
    }
}
