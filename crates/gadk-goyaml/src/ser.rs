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

//! The serializer: yaml.v3 v3.0.1 `encode.go`'s reflection path (`encoder.marshal`, `mapv`,
//! `structv`, `slicev`) as a serde `Serializer` emitting events through `encode.rs`, plus the
//! `Serialize` impls of [`Document`] (routed to `nodev`) and [`GoValue`].
// Bodies are Task 9's; Task 9 removes this line.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use serde::ser::{Serialize, Serializer};

use crate::Options;
use crate::encode::Encoder;
use crate::error::Error;
use crate::node::Document;
use crate::value::GoValue;

/// Go: `(*encoder).marshalDoc("", v)` for a value that is not a `*yaml.Node` of
/// `DocumentNode` kind: an implicit document wrapping `v` (`e.init()` included).
pub(crate) fn marshal_doc<T: Serialize + ?Sized>(
    e: &mut Encoder<'_>,
    v: &T,
    opts: &Options,
) -> Result<(), Error> {
    // Go: encode.go marshalDoc
    todo!("Task 9")
}

impl Serialize for Document {
    /// Go encodes a `yaml.Node` value through `nodev`; through gadk-goyaml's serializer the
    /// subtree is handed over by [`crate::node::DOCUMENT_MARKER`].
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        todo!("Task 9")
    }
}

impl Serialize for GoValue {
    /// Go's `encoder.marshal` of the dynamic value (`Timestamp` via `timev`, `Bytes` via
    /// `stringv`'s `!!binary`, maps sorted by `sorter.rs`).
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        todo!("Task 9")
    }
}
