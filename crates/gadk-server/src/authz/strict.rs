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

//! Port of Go `server/authz/strict.go`.

use std::sync::Arc;

use super::{Authorizer, AuthzError};
use crate::authn::Caller;

struct Strict;

/// Port of Go `authz.NewStrict`: the caller may act only as itself.
pub fn strict() -> Arc<dyn Authorizer> {
    Arc::new(Strict)
}

impl Authorizer for Strict {
    fn can_act_as_user(&self, caller: Option<&Caller>, user_id: &str) -> Result<(), AuthzError> {
        let Some(caller) = caller else {
            return Err(AuthzError(Some("no caller found in context".into())));
        };
        if caller.user_id == user_id {
            return Ok(());
        }
        Err(AuthzError(Some(
            "caller doesn't match the provided userID".into(),
        )))
    }
}
