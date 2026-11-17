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

//! gadk-server: the ADK REST API server (including its Pub/Sub and Eventarc trigger endpoints),
//! its inbound authentication ([`authn`]) and authorization ([`authz`]). Wire-compatible with ADK
//! REST clients such as the ADK web UI.
//!
//! [`rest::Server`] serves the REST routes as an [`axum::Router`]; mount it on any axum or hyper
//! server. [`mux`] provides the handler types, path templates and path cleaning that its route
//! table and the launcher's web router share.

#[cfg(feature = "agentengine")]
pub mod agentengine;
pub mod authn;
pub mod authz;
pub mod mux;
pub mod rest;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;
