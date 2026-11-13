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

//! Go `internal/configurable/conformance/nodes.go`: the `conformance.uppercase_formatter`
//! node function.

use std::sync::Arc;

use futures::FutureExt;
use gadk_agent::{AgentError, Context};
use gadk_platform::go_to_upper;
use serde_json::{Map, Value};

use crate::{ConfigError, NodeOutput, Registry};

/// Go `uppercaseFormatter`: a string is uppercased; a map has its string values uppercased;
/// any other input goes through `typeutil.ConvertToWithJSONSchema[any, map[string]any]`
/// (JSON `null` → an empty map), else `node_input must be a string or a map/dict`.
pub(crate) async fn uppercase_formatter(_ctx: Context, input: Value) -> Result<Value, AgentError> {
    // Go: nodes.go uppercaseFormatter
    match input {
        Value::String(v) => Ok(Value::String(go_to_upper(&v))),
        Value::Object(v) => Ok(Value::Object(upper_values(v))),
        // Go's default branch: `json.Marshal(input)` then `json.Unmarshal` into a
        // `map[string]any` (no schema). For a JSON-typed input only `null` decodes (to a nil
        // map, ranged as empty); numbers, bools and arrays fail.
        Value::Null => Ok(Value::Object(upper_values(Map::new()))),
        _ => Err(AgentError::other(
            "node_input must be a string or a map/dict",
        )),
    }
}

/// The map loop of Go `uppercaseFormatter`: string values `strings.ToUpper`ed, others kept.
fn upper_values(v: Map<String, Value>) -> Map<String, Value> {
    v.into_iter()
        .map(|(k, val)| match val {
            Value::String(s) => (k, Value::String(go_to_upper(&s))),
            val => (k, val),
        })
        .collect()
}

/// Go `RegisterNodeFunctions`: registers `conformance.uppercase_formatter` (Go's
/// `RegisterNodeFunction` overwrites and never fails, so the error is always nil).
pub fn register_node_functions(registry: &Registry) -> Result<(), ConfigError> {
    // Go: nodes.go RegisterNodeFunctions
    registry.register_node_function(
        "conformance.uppercase_formatter",
        Arc::new(|ctx, input| {
            async move { uppercase_formatter(ctx, input).await.map(NodeOutput::Value) }.boxed()
        }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use gadk_agent::InvocationContext;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn ctx() -> Context {
        InvocationContext::builder().build()
    }

    // Expected values from Go (a `go test -overlay` probe calling `uppercaseFormatter`).

    #[tokio::test]
    async fn uppercases_a_string_with_go_simple_case_mapping() {
        // Go's `strings.ToUpper` keeps `ß` (no single-rune uppercase); Rust's would give `SS`.
        assert_eq!(
            uppercase_formatter(ctx(), json!("hello wörld ß"))
                .await
                .unwrap(),
            json!("HELLO WÖRLD ß")
        );
        assert_eq!(
            uppercase_formatter(ctx(), json!("")).await.unwrap(),
            json!("")
        );
    }

    #[tokio::test]
    async fn uppercases_the_string_values_of_a_map() {
        assert_eq!(
            uppercase_formatter(ctx(), json!({ "a": "x", "b": 1, "c": null, "d": ["y"] }))
                .await
                .unwrap(),
            json!({ "a": "X", "b": 1, "c": null, "d": ["y"] })
        );
        assert_eq!(
            uppercase_formatter(ctx(), json!({})).await.unwrap(),
            json!({})
        );
    }

    #[tokio::test]
    async fn null_converts_to_an_empty_map() {
        assert_eq!(
            uppercase_formatter(ctx(), Value::Null).await.unwrap(),
            json!({})
        );
    }

    #[tokio::test]
    async fn other_inputs_fail_with_go_text() {
        for input in [json!(1.5), json!(0), json!(true), json!(["a"])] {
            assert_eq!(
                uppercase_formatter(ctx(), input.clone())
                    .await
                    .unwrap_err()
                    .to_string(),
                "node_input must be a string or a map/dict",
                "{input}"
            );
        }
    }

    #[tokio::test]
    async fn register_node_functions_registers_uppercase_formatter() {
        let r = Registry::empty();
        register_node_functions(&r).unwrap();
        // Go's `RegisterNodeFunction` overwrites: a second call succeeds too.
        register_node_functions(&r).unwrap();
        let f = r
            .inner
            .read()
            .node_functions
            .get("conformance.uppercase_formatter")
            .cloned()
            .unwrap();
        assert_eq!(r.inner.read().node_functions.len(), 1);
        let NodeOutput::Value(v) = f(ctx(), json!({ "k": "v" })).await.unwrap() else {
            panic!("want a value");
        };
        assert_eq!(v, json!({ "k": "V" }));
        assert_eq!(
            f(ctx(), json!(2)).await.unwrap_err().to_string(),
            "node_input must be a string or a map/dict"
        );
    }
}
