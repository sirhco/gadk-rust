// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

//! [`GoValue`]: what yaml.v3 v3.0.1 decodes into a Go `interface{}` (`decode.go` `scalar`,
//! `sequence` and `mapping` with an interface target), and the decode of a node into it,
//! sharing `de.rs`'s [`DecodeState`] (alias limits, merges, type errors). Its `Serialize` impl
//! (Go's `encoder.marshal` of those values) is in `ser.rs`.
// Decode bodies are Task 8's; Task 8 removes this line.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use chrono::{DateTime, FixedOffset};
use serde::de::{Deserialize, Deserializer};

use crate::de::DecodeState;
use crate::error::Error;
use crate::node::{Document, NodeId};

/// A Go value of dynamic type, as yaml.v3 decodes into `interface{}`.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum GoValue {
    /// `nil`.
    #[default]
    Null,
    /// `bool`.
    Bool(bool),
    /// `int` (64-bit).
    Int(i64),
    /// `uint64` (integers above `math.MaxInt64`).
    Uint(u64),
    /// `float64`.
    Float(f64),
    /// `string` (valid UTF-8), also for `!!binary` whose decoded bytes are valid UTF-8.
    String(String),
    /// A Go `string` that is not valid UTF-8 (only from `!!binary`).
    // Divergence (ledger 6c-1): Go yields a `string`; a Rust `String` must be UTF-8.
    Bytes(Vec<u8>),
    /// `time.Time` (an instant with nanoseconds and its offset).
    Timestamp(DateTime<FixedOffset>),
    /// `[]interface{}`.
    Seq(Vec<GoValue>),
    /// `map[string]interface{}` (every key a string), in document order (encode sorts).
    Map(Vec<(String, GoValue)>),
    /// `map[interface{}]interface{}`, in document order (encode sorts).
    MapAny(Vec<(GoValue, GoValue)>),
}

/// Go: `(*decoder).unmarshal(n, out)` with `out` an `interface{}`: the decoded value, or
/// `None` where Go's `unmarshal` returns `good == false` (the `interface{}` stays nil and
/// callers skip it, as Go's `sequence`/`mapping` do).
pub(crate) fn unmarshal_value(
    d: &mut DecodeState<'_>,
    doc: &Document,
    n: NodeId,
) -> Result<Option<GoValue>, Error> {
    // Go: decode.go unmarshal (interface target)
    todo!("Task 8")
}

impl<'de> Deserialize<'de> for GoValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        todo!("Task 8")
    }
}
