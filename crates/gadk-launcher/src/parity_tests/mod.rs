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

//! Ports of ADK-Go's launcher tests that reach crate-private code (ledger `docs/parity/6.md`).

mod cmd_launcher_console_compaction;
mod cmd_launcher_console_hitl;
mod cmd_launcher_internal_telemetry_telemetry;
#[cfg(feature = "a2a")]
mod cmd_launcher_web_a2a_a2a;
#[cfg(feature = "web")]
mod cmd_launcher_web_api_api;
#[cfg(feature = "web")]
mod cmd_launcher_web_router_body_limit;
#[cfg(feature = "triggers")]
mod cmd_launcher_web_triggers_eventarc_eventarc;
#[cfg(feature = "triggers")]
mod cmd_launcher_web_triggers_internal_triggerauth_triggerauth;
#[cfg(feature = "triggers")]
mod cmd_launcher_web_triggers_pubsub_pubsub;
#[cfg(feature = "web")]
mod cmd_launcher_web_web;
#[cfg(feature = "webui")]
mod cmd_launcher_web_webui_webui;
mod internal_cli_util_text_helpers;
