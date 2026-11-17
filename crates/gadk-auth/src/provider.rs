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

//! The credential-provider seam: what a provider learns about the invocation, the provider traits
//! and the closure adapter.

use std::sync::Arc;

use futures::future::BoxFuture;
use gadk_agent::{Context, Identity, InvocationContext, ReadonlyContext};

use crate::credential::Credential;
use crate::error::Error;

/// What a provider learns about the invocation: the invocation's identity. `identity: None` means
/// no ADK invocation identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthContext {
    /// The invocation's identity, when the call runs inside one.
    pub identity: Option<Identity>,
}

impl AuthContext {
    /// No invocation identity.
    pub fn anonymous() -> Self {
        Self { identity: None }
    }

    /// The context of an invocation acting for `identity`.
    pub fn for_identity(identity: Identity) -> Self {
        Self {
            identity: Some(identity),
        }
    }
}

impl From<&ReadonlyContext> for AuthContext {
    fn from(cx: &ReadonlyContext) -> Self {
        Self::for_identity(cx.identity())
    }
}

impl From<&InvocationContext> for AuthContext {
    fn from(cx: &InvocationContext) -> Self {
        Self::for_identity(cx.identity())
    }
}

impl From<&Context> for AuthContext {
    fn from(cx: &Context) -> Self {
        Self::for_identity(cx.identity())
    }
}

impl From<Identity> for AuthContext {
    fn from(identity: Identity) -> Self {
        Self::for_identity(identity)
    }
}

/// Resolves a [`Credential`] for the current invocation. A provider that needs interactive consent
/// returns an error carrying a [`ConsentRequiredError`](crate::ConsentRequiredError); find it with
/// [`find_consent`](crate::find_consent).
#[async_trait::async_trait]
pub trait CredentialProvider: Send + Sync {
    /// A ready-to-use credential.
    async fn credential(&self, cx: &AuthContext) -> Result<Arc<dyn Credential>, Error>;

    /// This provider as a [`RefreshingProvider`], when it is one. A `RefreshingProvider` overrides
    /// this to return `Some(self)`.
    fn as_refreshing(&self) -> Option<&dyn RefreshingProvider> {
        None
    }
}

/// A provider that can replace a rejected credential: after a credential is rejected downstream
/// (HTTP 401/403) and the request can be replayed, [`Transport`](crate::Transport) calls `refresh`
/// and retries once. `rejected` is the exact `Arc` the transport applied. It must not be returned,
/// and must not be left where a later call finds it.
#[async_trait::async_trait]
pub trait RefreshingProvider: CredentialProvider {
    /// A replacement for `rejected`.
    async fn refresh(
        &self,
        cx: &AuthContext,
        rejected: &Arc<dyn Credential>,
    ) -> Result<Arc<dyn Credential>, Error>;
}

/// The future a [`ProviderFn`] closure returns.
pub type CredentialFuture = BoxFuture<'static, Result<Arc<dyn Credential>, Error>>;

/// Adapts a closure to a [`CredentialProvider`]. The closure clones what it needs from `cx` before
/// its `async move` block; the future is `'static`.
pub struct ProviderFn<F>(F);

impl<F> ProviderFn<F>
where
    F: Fn(&AuthContext) -> CredentialFuture + Send + Sync + 'static,
{
    /// Wraps `f`.
    pub fn new(f: F) -> Self {
        Self(f)
    }

    /// `Arc::new(self)` as a provider.
    pub fn into_provider(self) -> Arc<dyn CredentialProvider> {
        Arc::new(self)
    }
}

#[async_trait::async_trait]
impl<F> CredentialProvider for ProviderFn<F>
where
    F: Fn(&AuthContext) -> CredentialFuture + Send + Sync + 'static,
{
    async fn credential(&self, cx: &AuthContext) -> Result<Arc<dyn Credential>, Error> {
        (self.0)(cx).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use futures::FutureExt as _;

    use super::*;
    use crate::credential::Bearer;

    #[tokio::test]
    async fn provider_fn_sees_the_auth_context() {
        let p = ProviderFn::new(|cx: &AuthContext| {
            let user = cx.identity.as_ref().map(|i| i.user_id.clone());
            async move {
                let token = user.ok_or_else(|| Error::msg("no user"))?;
                Ok(Arc::new(Bearer { token }) as Arc<dyn Credential>)
            }
            .boxed()
        })
        .into_provider();
        let cx = AuthContext::for_identity(Identity {
            user_id: "alice".into(),
            app_name: "app".into(),
            session_id: "s".into(),
        });
        let cred = p.credential(&cx).await.unwrap();
        assert_eq!(
            cred.downcast_ref::<Bearer>(),
            Some(&Bearer {
                token: "alice".into()
            })
        );
        let err = p.credential(&AuthContext::anonymous()).await.unwrap_err();
        assert_eq!(err.to_string(), "no user");
        assert!(p.as_refreshing().is_none());
    }

    #[test]
    fn auth_context_from_identity() {
        let id = Identity {
            user_id: "u".into(),
            app_name: "a".into(),
            session_id: "s".into(),
        };
        assert_eq!(AuthContext::from(id.clone()), AuthContext::for_identity(id));
        assert_eq!(AuthContext::default(), AuthContext::anonymous());
    }

    #[test]
    fn auth_context_from_each_context_type_carries_its_identity() {
        let ic = InvocationContext::builder()
            .invocation_id("inv-1")
            .build_invocation();
        let want = AuthContext::for_identity(ic.identity());
        let ctx = ic.clone().into_context();
        let ro: &ReadonlyContext = &ic;
        assert_eq!(AuthContext::from(&ic), want);
        assert_eq!(AuthContext::from(&ctx), want);
        assert_eq!(AuthContext::from(ro), want);
    }
}
