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
//! Go `structpb.NewStruct` + `json.NewEncoder(sb).Encode(*structpb.Struct)`: the method
//! metadata as `NewHandler` logs it.

use serde_json::{Map, Value};

/// Go `json.NewEncoder(sb).Encode(m)` of `structpb.NewStruct(map)`, newline included.
/// `*structpb.Struct` has a `MarshalJSON` (protojson), so Go writes the map itself as JSON:
/// `{"api_mode":"async","description":"…","name":"…","parameters":{…}}`, keys sorted, compacted
/// and HTML-escaped by the encoder (`gadk_model::to_go_json`), plus `\n`. An encoding failure
/// is the encoder's error text.
pub(crate) fn structpb_json(m: &Map<String, Value>) -> Result<String, String> {
    let bytes = gadk_model::to_go_json(m).map_err(|e| e.to_string())?;
    let mut text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    #[test]
    fn writes_the_map_as_go_encoder_json() {
        let m = json!({"name": "n", "b": true, "html": "<&>", "list": [1, 2.5, null], "nested": {"z": 1, "a": "x"}});
        assert_eq!(
            structpb_json(m.as_object().unwrap()).unwrap(),
            "{\"b\":true,\"html\":\"\\u003c\\u0026\\u003e\",\"list\":[1,2.5,null],\"name\":\"n\",\"nested\":{\"a\":\"x\",\"z\":1}}\n"
        );
    }
}
