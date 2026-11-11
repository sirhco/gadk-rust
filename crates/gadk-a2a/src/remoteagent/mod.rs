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

//! A remote A2A agent as a gadk agent (port of ADK-Go `agent/remoteagent/v2` and
//! `internal/agent/remoteagent`): [`new_a2a`] builds it from an [`A2AConfig`].

pub(crate) mod agent;
pub(crate) mod auth;
pub(crate) mod card;
pub(crate) mod client;
pub(crate) mod config;
pub(crate) mod history;
pub(crate) mod run_processor;

pub use agent::*;
pub use auth::{CredentialScope, credential_scope, current_credential_scope};
pub use card::*;
pub use client::*;
pub use config::*;
