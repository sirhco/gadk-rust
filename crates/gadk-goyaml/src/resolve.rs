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

//! Port of yaml.v3 v3.0.1 `resolve.go`: implicit tag resolution, short/long tags, `!!binary`
//! base64 and timestamps.
// Bodies are Task 5's; Task 5 removes this line.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use chrono::{DateTime, FixedOffset};

use crate::error::Error;
use crate::value::GoValue;

/// Go: `nullTag`.
pub(crate) const NULL_TAG: &str = "!!null";
/// Go: `boolTag`.
pub(crate) const BOOL_TAG: &str = "!!bool";
/// Go: `strTag`.
pub(crate) const STR_TAG: &str = "!!str";
/// Go: `intTag`.
pub(crate) const INT_TAG: &str = "!!int";
/// Go: `floatTag`.
pub(crate) const FLOAT_TAG: &str = "!!float";
/// Go: `timestampTag`.
pub(crate) const TIMESTAMP_TAG: &str = "!!timestamp";
/// Go: `seqTag`.
pub(crate) const SEQ_TAG: &str = "!!seq";
/// Go: `mapTag`.
pub(crate) const MAP_TAG: &str = "!!map";
/// Go: `binaryTag`.
pub(crate) const BINARY_TAG: &str = "!!binary";
/// Go: `mergeTag`.
pub(crate) const MERGE_TAG: &str = "!!merge";

/// Go: `longTagPrefix`.
pub(crate) const LONG_TAG_PREFIX: &str = "tag:yaml.org,2002:";

/// Go: resolve.go `shortTag` — `tag:yaml.org,2002:x` → `!!x`; other tags unchanged.
pub(crate) fn short_tag(tag: &str) -> String {
    // Go: resolve.go shortTag
    todo!("Task 5")
}

/// Go: resolve.go `longTag` — `!!x` → `tag:yaml.org,2002:x`; other tags unchanged.
pub(crate) fn long_tag(tag: &str) -> String {
    // Go: resolve.go longTag
    todo!("Task 5")
}

/// Go: resolve.go `resolvableTag`.
pub(crate) fn resolvable_tag(tag: &str) -> bool {
    // Go: resolve.go resolvableTag
    todo!("Task 5")
}

/// Go: resolve.go `resolve` — the resolved short tag and Go value of the scalar text `input`
/// under `tag` (Go's `interface{}` result as a [`GoValue`]: `nil` → `Null`, `int`/`int64` →
/// `Int`, `uint64` → `Uint`, `float64` → `Float`, `string` → `String`, `time.Time` →
/// `Timestamp`). The `Err` is the deferred `failf("cannot decode %s `%s` as a %s", …)`.
pub(crate) fn resolve(tag: &str, input: &str) -> Result<(String, GoValue), Error> {
    // Go: resolve.go resolve
    todo!("Task 5")
}

/// Go: resolve.go `encodeBase64` — base64 broken into 70-character lines (each followed by a
/// newline when there is more than one line). `s` is a Go string: any bytes.
pub(crate) fn encode_base64(s: &[u8]) -> String {
    // Go: resolve.go encodeBase64
    todo!("Task 5")
}

/// Go: resolve.go `parseTimestamp` — `Some` for the `allowedTimestampFormats`
/// (no zone = UTC).
pub(crate) fn parse_timestamp(s: &str) -> Option<DateTime<FixedOffset>> {
    // Go: resolve.go parseTimestamp
    todo!("Task 5")
}
