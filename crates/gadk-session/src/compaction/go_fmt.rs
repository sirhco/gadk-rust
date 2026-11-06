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

//! Go `fmt` `%v` of a JSON-decoded value, for the summarizer transcript. A private copy of the
//! engine's helpers: gadk-session cannot depend on gadk-engine.

use serde_json::Value;

/// Go `fmt.Sprintf("%v", v)` for a JSON-decoded value:
/// - string → itself; bool → true/false; null → `<nil>`;
/// - integer numbers → decimal;
/// - other numbers → `go_format_float_v` (Go %v of float64 = %g with shortest digits);
/// - array → `[a b c]`; object → `map[k1:v1 k2:v2]` with keys sorted (Go fmt sorts map keys).
///
/// Apply [`as_go_floats`] first on values Go would have JSON-decoded.
pub(super) fn go_sprint(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n
            .as_i64()
            .map(|i| i.to_string())
            .or_else(|| n.as_u64().map(|u| u.to_string()))
            .unwrap_or_else(|| go_format_float_v(n.as_f64().unwrap_or_default())),
        Value::String(s) => s.clone(),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(go_sprint).collect::<Vec<_>>().join(" ")
        ),
        Value::Object(m) => format!(
            "map[{}]",
            m.iter()
                .map(|(k, v)| format!("{k}:{}", go_sprint(v)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

/// `v` with every number as a float: Go decodes tool arguments from JSON, so every number is a
/// `float64` and `fmt.Sprint` prints it in `%v` float form (`1e+06`, not `1000000`). A number
/// with no finite float form is kept as is.
pub(super) fn as_go_floats(v: &Value) -> Value {
    match v {
        Value::Number(n) => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| v.clone(), Value::Number),
        Value::Array(a) => Value::Array(a.iter().map(as_go_floats).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), as_go_floats(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Go `%v` of a float64 = `strconv.FormatFloat(f, 'g', -1, 64)`: shortest round-trip digits; `%e`
/// form when the decimal exponent is < -4 or >= 6, else plain decimal with no trailing zeros.
fn go_format_float_v(f: f64) -> String {
    let sci = format!("{f:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..6).contains(&exp) {
        format!(
            "{mant}e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.unsigned_abs()
        )
    } else {
        format!("{f}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn go_sprint_of_json_decoded_values_matches_go() {
        assert_eq!(go_sprint(&as_go_floats(&json!(3))), "3");
        assert_eq!(go_sprint(&as_go_floats(&json!(1_000_000))), "1e+06");
        assert_eq!(go_sprint(&as_go_floats(&json!(1.5))), "1.5");
        assert_eq!(go_sprint(&json!(null)), "<nil>");
        assert_eq!(go_sprint(&as_go_floats(&json!([1, "a"]))), "[1 a]");
        assert_eq!(
            go_sprint(&as_go_floats(&json!({"b": 2, "a": true}))),
            "map[a:true b:2]"
        );
    }
}
