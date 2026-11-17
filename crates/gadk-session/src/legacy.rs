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

//! Case-insensitive key matching, for rows written with legacy keys.

use serde_json::{Map, Value};

/// Renames every key of `map` that is not in `canonical` but equals a canonical name
/// ignoring ASCII case.
///
/// When the canonical key is already present, the exact match wins and the case-folded
/// duplicate is dropped.
pub(crate) fn normalize_keys(map: &mut Map<String, Value>, canonical: &[&str]) {
    let renames: Vec<(String, &str)> = map
        .keys()
        .filter(|k| !canonical.contains(&k.as_str()))
        .filter_map(|k| {
            canonical
                .iter()
                .find(|c| c.eq_ignore_ascii_case(k))
                .map(|c| (k.clone(), *c))
        })
        .collect();
    for (from, to) in renames {
        if let Some(v) = map.remove(&from)
            && !map.contains_key(to)
        {
            map.insert(to.to_string(), v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renames_case_folded_keys_and_keeps_exact_matches() {
        let mut m = Map::new();
        m.insert("ID".into(), json!(1));
        m.insert("invocationId".into(), json!(2));
        m.insert("InvocationID".into(), json!(3));
        m.insert("other".into(), json!(4));
        normalize_keys(&mut m, &["id", "invocationId"]);
        assert_eq!(
            Value::Object(m),
            json!({"id": 1, "invocationId": 2, "other": 4})
        );
    }
}
