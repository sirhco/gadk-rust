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

//! Port of Go `server/authn/noop.go`.

use std::sync::Arc;

use super::{Authenticator, AuthnError, Caller};

struct Noop;

/// Port of Go `authn.NewNoop`: authenticates every request as an empty caller.
pub fn noop() -> Arc<dyn Authenticator> {
    Arc::new(Noop)
}

#[async_trait::async_trait]
impl Authenticator for Noop {
    async fn authenticate(
        &self,
        _req: &http::request::Parts,
    ) -> Result<Option<Caller>, AuthnError> {
        Ok(Some(Caller::default()))
    }
}
