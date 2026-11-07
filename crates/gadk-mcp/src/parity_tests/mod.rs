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

//! Ports of ADK-Go's `tool/mcptoolset` tests that need crate-private access, one module per Go
//! file: the internal tests (package `mcptoolset`) and `auth_session_test.go` (package
//! `mcptoolset_test`, which uses `export_test.go`'s hooks).

mod tool_mcptoolset_auth_session;
mod tool_mcptoolset_client;
mod tool_mcptoolset_client_cancellation;
mod tool_mcptoolset_tool;
mod tool_mcptoolset_tracking;
mod tool_mcptoolset_transport_config;
