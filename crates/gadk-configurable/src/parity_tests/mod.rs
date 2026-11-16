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

//! Ports of ADK-Go's `internal/configurable` test files (ledger 6c-2): one
//! module per Go file, named after the Go file's path
//! (`internal/configurable/conformance/recordplugin/yaml_clean_test.go` →
//! `internal_configurable_conformance_recordplugin_yaml_clean`), holding
//! `fn test_<snake of TestName>` (`go_test_to_rust`).

mod internal_configurable_configurable_utils;
mod internal_configurable_configurable_utils_windows;
mod internal_configurable_configurable_workflow;
mod internal_configurable_configurable_workflow_ref;
mod internal_configurable_conformance_recordplugin_record_plugin;
mod internal_configurable_conformance_recordplugin_yaml_clean;
mod internal_configurable_conformance_replayplugin_replay_plugin;
mod internal_configurable_conformance_replayplugin_replay_plugin_internal;
mod internal_configurable_conformance_replayplugin_yaml_utils;
