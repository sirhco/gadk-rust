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

//! Function-call ids and arguments, plus the argument decoding both APIs share. The decode
//! error texts are byte-identical to an ADK OpenAI model's JSON-decoding errors; see the
//! literals in the function bodies.

use gadk_genai::{FunctionCall, FunctionResponse};
use gadk_platform::go_quote;
use serde_json::{Map, Value};

use crate::error::{ErrorKind, OpenAiError, go_json_error};

/// Pairs function responses with the calls they answer, minting an id for a
/// call that has none (`adk-openai-call-<n>`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CallTracker {
    /// The number the next minted id carries.
    pub(crate) next_id: u64,
    /// The ids of calls not yet answered, oldest first.
    pub(crate) pending: Vec<String>,
}

impl CallTracker {
    /// The call id `fr` answers, consumed from
    /// [`pending`](Self::pending). An empty `fr.id` takes the oldest pending id (none:
    /// `openai: response for "<name>" missing call id`); a non-empty one must be pending
    /// (else `openai: received function response for unknown or already completed call id
    /// "<id>"`). Plain errors; names and ids quoted with [`gadk_platform::go_quote`].
    pub(crate) fn resolve_response_id(
        &mut self,
        fr: &FunctionResponse,
    ) -> Result<String, OpenAiError> {
        if fr.id.is_empty() {
            if self.pending.is_empty() {
                return Err(OpenAiError::plain(format!(
                    "openai: response for {} missing call id",
                    go_quote(&fr.name)
                )));
            }
            return Ok(self.pending.remove(0));
        }
        match self.pending.iter().position(|pending| *pending == fr.id) {
            Some(i) => {
                self.pending.remove(i);
                Ok(fr.id.clone())
            }
            None => Err(OpenAiError::plain(format!(
                "openai: received function response for unknown or already completed call id {}",
                go_quote(&fr.id)
            ))),
        }
    }

    /// `fc.id`, or a minted `adk-openai-call-<next_id>` when it is empty;
    /// either way appended to [`pending`](Self::pending).
    pub(crate) fn take_call_id(&mut self, fc: &FunctionCall) -> String {
        let call_id = if fc.id.is_empty() {
            let minted = format!("adk-openai-call-{}", self.next_id);
            self.next_id += 1;
            minted
        } else {
            fc.id.clone()
        };
        self.pending.push(call_id.clone());
        call_id
    }
}

/// The call's arguments as compact JSON
/// ([`gadk_model::to_go_json`]; a call without arguments is `{}`). A marshal failure is
/// `openai: marshal function args: <err>`.
pub(crate) fn marshal_function_args(args: &Map<String, Value>) -> Result<String, OpenAiError> {
    marshal("openai: marshal function args: ", args)
}

/// A function response's payload as both APIs send it:
/// [`gadk_model::to_go_json`] of `response`; a
/// marshal failure is `openai: marshal function response: <err>`.
pub(crate) fn marshal_function_response(
    response: &Map<String, Value>,
) -> Result<String, OpenAiError> {
    marshal("openai: marshal function response: ", response)
}

/// A JSON object as a string; a failure is `<prefix><err>`.
fn marshal(prefix: &str, map: &Map<String, Value>) -> Result<String, OpenAiError> {
    let encoded =
        gadk_model::to_go_json(map).map_err(|e| OpenAiError::plain(format!("{prefix}{e}")))?;
    String::from_utf8(encoded).map_err(|e| OpenAiError::plain(format!("{prefix}{e}")))
}

/// Decodes `raw` into a JSON object: `Ok(None)` for JSON
/// `null`, `Ok(Some(map))` for an object, else `Err` with the text
/// `unexpected end of JSON input` for truncated input, `expected a JSON object, found <kind>`
/// for a value of another kind (see [`go_map`]), and serde_json's text otherwise.
pub(crate) fn unmarshal_function_args(raw: &str) -> Result<Option<Map<String, Value>>, String> {
    let value: Value = serde_json::from_str(raw).map_err(|e| go_json_error(&e))?;
    go_map(value)
}

/// Stores a decoded value as a JSON object: an object is the map,
/// `null` is `None`, anything else is an `Err` naming the value's kind
/// (`expected a JSON object, found an array`).
pub(crate) fn go_map(value: Value) -> Result<Option<Map<String, Value>>, String> {
    match value {
        Value::Object(map) => Ok(Some(map)),
        Value::Null => Ok(None),
        other => Err(gadk_platform::json_expected_found("a JSON object", &other)),
    }
}

/// Decodes the raw argument payload (both APIs): `""` and JSON
/// `null` are a call that takes no arguments (`{}`); an undecodable payload is
/// `openai: parse function call args: <err>` (kind
/// [`ErrorKind::FunctionCallArgs`](crate::error::ErrorKind::FunctionCallArgs)).
pub(crate) fn function_call_args(raw: &str) -> Result<Map<String, Value>, OpenAiError> {
    if raw.is_empty() {
        return Ok(Map::new());
    }
    match unmarshal_function_args(raw) {
        Ok(args) => Ok(args.unwrap_or_default()),
        Err(err) => Err(OpenAiError::new(ErrorKind::FunctionCallArgs, err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    fn response(id: &str, name: &str) -> FunctionResponse {
        FunctionResponse {
            id: id.to_string(),
            name: name.to_string(),
            ..FunctionResponse::default()
        }
    }

    #[test]
    fn call_ids_are_minted_and_paired_oldest_first() {
        let mut tracker = CallTracker::default();
        let unnamed = FunctionCall::default();
        let named = FunctionCall {
            id: "call_x".to_string(),
            ..FunctionCall::default()
        };
        assert_eq!(tracker.take_call_id(&unnamed), "adk-openai-call-0");
        assert_eq!(tracker.take_call_id(&named), "call_x");
        assert_eq!(tracker.take_call_id(&unnamed), "adk-openai-call-1");
        assert_eq!(tracker.next_id, 2);

        // An id answers its own call; an empty one takes the oldest pending.
        assert_eq!(
            tracker
                .resolve_response_id(&response("call_x", "f"))
                .unwrap(),
            "call_x"
        );
        assert_eq!(
            tracker.resolve_response_id(&response("", "f")).unwrap(),
            "adk-openai-call-0"
        );
        assert_eq!(tracker.pending, vec!["adk-openai-call-1".to_string()]);
    }

    #[test]
    fn unpaired_responses_are_plain_errors() {
        let mut tracker = CallTracker::default();
        let err = tracker
            .resolve_response_id(&response("", "get_weather"))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"openai: response for "get_weather" missing call id"#
        );
        assert!(err.kinds().is_empty());
        let err = tracker
            .resolve_response_id(&response("call\n1", "f"))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"openai: received function response for unknown or already completed call id "call\n1""#
        );
    }

    #[test]
    fn marshalling_writes_go_json() {
        assert_eq!(marshal_function_args(&Map::new()).unwrap(), "{}");
        let args = object(json!({"z": "<b>&", "a": 1.5, "n": 3.0, "nested": {"y": null}}));
        assert_eq!(
            marshal_function_args(&args).unwrap(),
            r#"{"a":1.5,"n":3,"nested":{"y":null},"z":"\u003cb\u003e\u0026"}"#
        );
        assert_eq!(
            marshal_function_response(&object(json!({"ok": true}))).unwrap(),
            r#"{"ok":true}"#
        );
    }

    #[test]
    fn unmarshalling_names_the_kind_found() {
        assert_eq!(unmarshal_function_args("null").unwrap(), None);
        assert_eq!(
            unmarshal_function_args(r#"{"a":1}"#).unwrap(),
            Some(object(json!({"a": 1})))
        );
        for (raw, want) in [
            ("{", "unexpected end of JSON input"),
            ("[1]", "expected a JSON object, found an array"),
            (r#""s""#, "expected a JSON object, found a string"),
            ("1", "expected a JSON object, found a number"),
            ("true", "expected a JSON object, found a boolean"),
        ] {
            assert_eq!(unmarshal_function_args(raw).unwrap_err(), want, "{raw}");
        }
    }

    #[test]
    fn function_call_args_reads_empty_and_null_as_no_arguments() {
        assert_eq!(function_call_args("").unwrap(), Map::new());
        assert_eq!(function_call_args("null").unwrap(), Map::new());
        assert_eq!(
            function_call_args(r#"{"city":"SF"}"#).unwrap(),
            object(json!({"city": "SF"}))
        );
        let err = function_call_args(r#"{"city":"#).unwrap_err();
        assert!(err.is(ErrorKind::FunctionCallArgs));
        assert_eq!(
            err.to_string(),
            "openai: parse function call args: unexpected end of JSON input"
        );
    }
}
