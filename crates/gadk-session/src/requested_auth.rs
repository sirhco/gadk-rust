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

//! Port of ADK-Go `internal/sessionutils/requested_auth.go`: the append-time check of
//! [`crate::EventActions::requested_auth_configs`].

use std::collections::BTreeMap;

use serde_json::value::RawValue;

use crate::toolauth::Config;
use crate::toolauth::model_json::GoJson;

/// Go `sessionutils.CheckRequestedAuthConfigs`: an error when a request in `configs` would not
/// read back from storage as a [`Config`].
///
/// A [`Config`] built as a struct literal is not checked when it is built, and its encoder
/// accepts configs its decoder refuses (a zero scheme, an OAuth2 flow without its URLs, a
/// credential without an `authType`). The in-memory, database and Vertex AI session services
/// call this before they append an event, so none of them stores a request it could not load
/// again, and all of them refuse the same events.
///
/// The configs are read back with every number as a float64, as the Vertex AI service stores
/// them through `structpb`, so an integer such as an `expiresAt` that the conversion rounds out
/// of range is refused too. The error text is Go's.
pub fn check_requested_auth_configs(configs: &BTreeMap<String, Config>) -> Result<(), String> {
    if configs.is_empty() {
        return Ok(());
    }
    let encoded = serde_json::to_string(configs)
        .map_err(|e| format!("encode requested auth configs: {e}"))?;
    // Go `json.Unmarshal(b, &stored)` into `any`: every number becomes a float64.
    let stored = GoJson::parse(&encoded)
        .map_err(|e| format!("decode requested auth configs: {e}"))
        .and_then(|tree| {
            as_float64(tree).map_err(|e| format!("decode requested auth configs: {e}"))
        })?;
    let reencoded = stored.encode(true);
    // Go `json.Unmarshal(b, &decoded)` into `map[string]toolauth.Config`: keys in text order
    // (sorted), each value through `Config.UnmarshalJSON`, whose error is returned unwrapped.
    let entries: BTreeMap<String, Box<RawValue>> = serde_json::from_str(&reencoded)
        .map_err(|e| format!("requested auth configs do not decode: {e}"))?;
    for raw in entries.values() {
        Config::decode(raw).map_err(|e| format!("requested auth configs do not decode: {e}"))?;
    }
    Ok(())
}

/// Go's `any` decode of a JSON number: a float64, written back by `json.Marshal`.
fn as_float64(v: GoJson) -> Result<GoJson, String> {
    Ok(match v {
        GoJson::Number(text) => {
            let f = text
                .parse::<f64>()
                .ok()
                .filter(|f| f.is_finite())
                .ok_or_else(|| {
                    format!("json: cannot unmarshal number {text} into Go value of type float64")
                })?;
            GoJson::Number(go_json_float(f))
        }
        GoJson::Array(items) => GoJson::Array(
            items
                .into_iter()
                .map(as_float64)
                .collect::<Result<_, _>>()?,
        ),
        GoJson::Object(members) => GoJson::Object(
            members
                .into_iter()
                .map(|(k, v)| Ok((k, as_float64(v)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other,
    })
}

/// Go `encoding/json`'s float64 encoder (`floatEncoder.encode`): `strconv` format `'f'` with
/// the shortest digits, or `'e'` when the magnitude is below 1e-6 or at least 1e21, with the
/// exponent's sign always written and a negative one-digit exponent not zero-padded.
fn go_json_float(f: f64) -> String {
    let abs = f.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        let s = format!("{f:e}");
        match s.split_once('e') {
            Some((mantissa, exp)) if !exp.starts_with('-') => format!("{mantissa}e+{exp}"),
            _ => s,
        }
    } else {
        format!("{f}")
    }
}

#[cfg(test)]
mod tests {
    use super::go_json_float;

    #[test]
    fn go_json_float_matches_go() {
        for (f, want) in [
            (9007199254740993_f64, "9007199254740992"),
            (9223372036854775807_i64 as f64, "9223372036854776000"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1e-7, "1e-7"),
            (1.5e-10, "1.5e-10"),
            (0.000001, "0.000001"),
            (-0.0, "-0"),
            (0.0, "0"),
            (1.5, "1.5"),
        ] {
            assert_eq!(go_json_float(f), want, "{f}");
        }
    }
}
