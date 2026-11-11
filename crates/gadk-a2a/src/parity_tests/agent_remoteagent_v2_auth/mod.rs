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

//! Ports of ADK-Go `agent/remoteagent/v2/auth_test.go`, split by what a test exercises:
//! [`transport`] holds the transport-level tests (mint groups, HTTP clients, redirects,
//! redaction, cleartext detection, credential application; plan 5b Task 4), [`agent`] the tests
//! that run an agent through `new_a2a` (plan 5b Task 8).

mod agent;
mod transport;
