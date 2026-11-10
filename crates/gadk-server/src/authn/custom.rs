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

//! Port of Go `server/authn/custom.go`.

use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;

use super::{Authenticator, AuthnError, Caller};

type AuthFn = dyn Fn(&http::request::Parts) -> BoxFuture<'static, Result<Option<Caller>, AuthnError>>
    + Send
    + Sync;

struct Custom {
    f: Box<AuthFn>,
}

/// Port of Go `authn.NewCustom`: an [`Authenticator`] that calls `f`.
pub fn custom<F, Fut>(f: F) -> Arc<dyn Authenticator>
where
    F: Fn(&http::request::Parts) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<Caller>, AuthnError>> + Send + 'static,
{
    Arc::new(Custom {
        f: Box::new(move |parts| Box::pin(f(parts))),
    })
}

#[async_trait::async_trait]
impl Authenticator for Custom {
    async fn authenticate(&self, req: &http::request::Parts) -> Result<Option<Caller>, AuthnError> {
        (self.f)(req).await
    }
}
