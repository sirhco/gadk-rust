// Copyright 2026 Chris Olson
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

//! The Go-reflection-shaped YAML encoding and decoding of [`Recordings`] (no single Go
//! source): [`to_go_node`] builds the node tree Go's `node.Encode(&recordings)` builds,
//! [`from_go_node`] decodes like Go's `node.Decode(&recordings)`.
//!
//! - [`roots`]: hand-written `Serialize`/`Deserialize` impls of the recording types in Go field
//!   order (yaml-tagged keys verbatim, the request view and tool call/response with every Go
//!   field).
//! - [`fixups`]: the type-path fixup table (Go `[]byte`, `TextMarshaler`, `time.Duration`,
//!   `time.Time`, `genai.Schema` titles, `*jsonschema.Schema`) and its inverse on decode.
//! - [`jsonschema`]: the reshaping of a JSON-schema value into jsonschema-go v0.4.3's
//!   `Schema` reflection.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

pub mod fixups;
pub mod jsonschema;
pub mod roots;

use gadk_goyaml::{Document, NodeId, TypeError};

use super::recording::Recordings;

/// Go `node.Encode(&recordings)`: the recordings' mapping node (as its own document, root =
/// the mapping), before `cleanYAMLNode`.
pub fn to_go_node(recordings: &Recordings) -> Result<Document, gadk_goyaml::Error> {
    // Go: record_plugin.go saveRecordings (node.Encode)
    todo!("Task 5")
}

/// Go `node.Decode(&recordings)` of node `at` (a document or mapping node), after any
/// `normalizeYAMLNode` the caller applied: the inverse fixups, then the Go-shaped decode;
/// yaml.v3's type errors fail it.
pub fn from_go_node(doc: &Document, at: NodeId) -> Result<Recordings, gadk_goyaml::Error> {
    // Go: replay_plugin.go loadInvocationState (root.Decode)
    todo!("Task 5")
}

/// As [`from_go_node`], keeping Go's partly decoded value next to the `*yaml.TypeError` (the
/// record plugin's merge path ignores the error and keeps the value, as Go's
/// `_ = yaml.Unmarshal(data, &existingRecordings)` does).
pub fn from_go_node_partial(
    doc: &Document,
    at: NodeId,
) -> Result<(Recordings, Option<TypeError>), gadk_goyaml::Error> {
    // Go: record_plugin.go saveRecordings (yaml.Unmarshal of the existing file)
    todo!("Task 5")
}
