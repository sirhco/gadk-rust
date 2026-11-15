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

//! In-crate parity targets: ports of ADK-Go `session/vertexai` and
//! `tool/exampletool/vertexai` tests, which reach the crate-private clients, conversions and
//! test servers.

mod session_vertexai_append_event_temp_state;
mod session_vertexai_compaction_persistence;
mod session_vertexai_delete_ownership;
mod session_vertexai_filter;
mod session_vertexai_notfound;
mod session_vertexai_raw_event_crossruntime;
mod session_vertexai_service;
mod session_vertexai_session;
mod session_vertexai_vertexai;
mod tool_exampletool_vertexai_vertexai;
