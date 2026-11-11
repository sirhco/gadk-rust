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

//! gadk-server: the ADK REST API server (port of ADK-Go `server/adkrest`, incl.
//! `controllers/triggers`), its inbound authentication ([`authn`], Go `server/authn`) and
//! authorization ([`authz`], Go `server/authz`), wire-compatible with an ADK-Go server.
//!
//! [`rest::Server`] serves the routes ADK-Go's `adkrest.NewServer` registers as an
//! [`axum::Router`]; mount it on any axum or hyper server. [`mux`] is the gorilla/mux v1.8.1
//! port (handler types, path templates, `cleanPath`) its route table and the launcher's web
//! router share.

pub mod authn;
pub mod authz;
pub mod mux;
pub mod rest;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod parity_tests;
