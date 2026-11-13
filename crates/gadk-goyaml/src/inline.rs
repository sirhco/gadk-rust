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

//! Go's `,inline` catch-all map (yaml.v3 v3.0.1 `yaml.go` `getStructInfo`, `structInfo.InlineMap`):
//! a struct field of type [`InlineMap`] named [`INLINE_FIELD`] receives every mapping key no
//! other field matched (decode) and is written after all fields, keys sorted (encode).
//!
//! The codec recognises the field by its serde name; the value crosses the serde boundary as
//! the newtype struct [`INLINE_MAP_MARKER`] wrapping a map, so the codec can also tell an
//! `InlineMap` from any other type given that field name. Through other serde formats an
//! `InlineMap` is a plain map.

use std::fmt;

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::value::GoValue;

/// The reserved serde field name of an inline catch-all:
/// `#[serde(rename = "$gadk_goyaml::inline")] extra: InlineMap`.
pub const INLINE_FIELD: &str = "$gadk_goyaml::inline";

/// The serde newtype-struct name an [`InlineMap`] serializes and deserializes through.
pub(crate) const INLINE_MAP_MARKER: &str = "$gadk_goyaml::InlineMap";

/// Go's `map[string]X` `,inline` field, with values decoded as Go's `interface{}`
/// ([`GoValue`]). Entries keep document order; encode sorts the keys.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InlineMap(pub Vec<(String, GoValue)>);

impl InlineMap {
    /// An empty map.
    pub fn new() -> InlineMap {
        InlineMap(Vec::new())
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no entries (Go's `m.Len() > 0` check skips an empty inline map).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value of the first entry with key `key`.
    pub fn get(&self, key: &str) -> Option<&GoValue> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// The entries as a serde map (`Some(len)`).
struct Entries<'a>(&'a [(String, GoValue)]);

impl Serialize for Entries<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (k, v) in self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl Serialize for InlineMap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(INLINE_MAP_MARKER, &Entries(&self.0))
    }
}

struct InlineMapVisitor;

impl<'de> Visitor<'de> for InlineMapVisitor {
    type Value = InlineMap;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an inline map")
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<InlineMap, D::Error> {
        d.deserialize_map(InlineMapVisitor)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<InlineMap, A::Error> {
        let mut entries = Vec::with_capacity(access.size_hint().unwrap_or(0));
        while let Some((k, v)) = access.next_entry::<String, GoValue>()? {
            entries.push((k, v));
        }
        Ok(InlineMap(entries))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<InlineMap, E> {
        Ok(InlineMap::new())
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<InlineMap, E> {
        Ok(InlineMap::new())
    }
}

impl<'de> Deserialize<'de> for InlineMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_newtype_struct(INLINE_MAP_MARKER, InlineMapVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(INLINE_FIELD, "$gadk_goyaml::inline");
        assert_eq!(INLINE_MAP_MARKER, "$gadk_goyaml::InlineMap");
    }

    #[test]
    fn accessors() {
        let m = InlineMap(vec![
            ("a".into(), GoValue::Int(1)),
            ("b".into(), GoValue::Null),
        ]);
        assert_eq!(m.len(), 2);
        assert!(!m.is_empty() && InlineMap::new().is_empty());
        assert_eq!(m.get("a"), Some(&GoValue::Int(1)));
        assert_eq!(m.get("c"), None);
    }

    #[test]
    fn maps_through_other_formats() {
        // Outside gadk-goyaml an InlineMap is a plain map of GoValues.
        assert_eq!(
            serde_json::to_string(&InlineMap::new()).ok().as_deref(),
            Some("{}")
        );
        let back: InlineMap = serde_json::from_str("{}").expect("empty map");
        assert!(back.is_empty());
        let m = InlineMap(vec![
            ("a".into(), GoValue::Int(1)),
            ("b".into(), GoValue::Null),
        ]);
        let json = serde_json::to_string(&m).expect("serialize");
        assert_eq!(json, r#"{"a":1,"b":null}"#);
        let back: InlineMap = serde_json::from_str(&json).expect("map");
        assert_eq!(back, m);
    }
}
