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

//! The refresh-and-retry transport (port of ADK-Go `auth/transport.go`) as a
//! `reqwest_middleware::Middleware`. Task 1 placeholder: Task 4 replaces every `todo!` body.

use std::sync::Arc;

use crate::error::ConsentRequiredError;
use crate::provider::{AuthContext, CredentialProvider};

/// Go `Transport` (transport.go:39) as a `reqwest_middleware::Middleware`. Go's per-request
/// context is the `AuthContext` fixed at construction.
#[allow(dead_code)]
#[derive(Clone)]
pub struct Transport {
    provider: Arc<dyn CredentialProvider>,
    cx: AuthContext,
}

impl Transport {
    /// A transport applying `provider`'s credential, resolved for `cx`, to every request.
    #[allow(unused_variables, clippy::todo)]
    pub fn new(provider: Arc<dyn CredentialProvider>, cx: AuthContext) -> Self {
        todo!("Task 4")
    }
}

impl std::fmt::Debug for Transport {
    #[allow(unused_variables, clippy::todo)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("Task 4")
    }
}

#[async_trait::async_trait]
impl reqwest_middleware::Middleware for Transport {
    #[allow(unused_variables, clippy::todo)]
    async fn handle(
        &self,
        req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        todo!("Task 4")
    }
}

/// `ClientBuilder::new(base).with(Transport::new(provider, cx)).build()`.
#[allow(unused_variables, clippy::todo)]
pub fn client(
    base: reqwest::Client,
    provider: Arc<dyn CredentialProvider>,
    cx: AuthContext,
) -> reqwest_middleware::ClientWithMiddleware {
    todo!("Task 4")
}

/// Finds a consent error in what `ClientWithMiddleware::send` returned: `Middleware(e)` walks
/// `e.chain()`, `Reqwest(e)` goes through `find_consent`.
#[allow(unused_variables, clippy::todo)]
pub fn find_consent_in(err: &reqwest_middleware::Error) -> Option<&ConsentRequiredError> {
    todo!("Task 4")
}

/// transport.go:164 `maxDrainBytes`.
#[allow(dead_code)]
pub(crate) const MAX_DRAIN_BYTES: usize = 4 << 10;
