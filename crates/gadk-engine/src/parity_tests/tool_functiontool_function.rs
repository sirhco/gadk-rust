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

//! Port of Go tool/functiontool/function_test.go.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use futures::StreamExt;
use futures::stream::BoxStream;
use gadk_agent::{
    AgentError, Context, EventStream, REQUEST_CONFIRMATION_FUNCTION_CALL_NAME, ToolError, pack_tool,
};
use gadk_genai::{Content, FunctionCall, FunctionResponse, Part};
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_session::{Event, ToolConfirmation};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::llm::{LlmAgentConfig, agent as llm_agent};
#[cfg(feature = "testutil")] // gemini_test_model and the replay test need the testutil feature
use crate::testutil::gemini_test_model;
use crate::testutil::{MockModel, TestAgentRunner};
use crate::tool::utils::convert_with_schema;
use crate::tool::{FunctionTool, FunctionToolConfig};

#[cfg(feature = "testutil")]
const REPLAY_SIMPLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/testdata/tool/functiontool/testdata/TestFunctionTool_Simple.httprr"
);

fn ctx() -> Context {
    Context::builder().build()
}

/// Port of Go readFirstResponse[*genai.FunctionCall].
async fn first_function_call(
    mut s: BoxStream<'static, Result<LlmResponse, ModelError>>,
) -> FunctionCall {
    let resp = s
        .next()
        .await
        .expect("no response message was received")
        .expect("GenerateContent failed");
    let content = resp.content.expect("encountered an empty response");
    content
        .parts
        .into_iter()
        .find_map(|p| p.function_call)
        .expect("response does not contain a function call")
}

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    city: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, Default, PartialEq)]
struct WeatherResult {
    report: String,
    status: String,
}

#[cfg(feature = "testutil")]
#[tokio::test]
async fn test_function_tool_simple() {
    let m = gemini_test_model("gemini-2.5-flash", REPLAY_SIMPLE).await;
    let result_set: Arc<HashMap<&'static str, WeatherResult>> = Arc::new(HashMap::from([
        (
            "london",
            WeatherResult {
                status: "success".into(),
                report: "The current weather in London is cloudy with a temperature of18 degrees Celsius and a chance of rain.".into(),
            },
        ),
        (
            "paris",
            WeatherResult {
                status: "success".into(),
                report: "The weather in Paris is sunny with a temperature of 25 derees Celsius.".into(),
            },
        ),
    ]));
    let rs = result_set.clone();
    let tool = FunctionTool::new(
        FunctionToolConfig {
            input_schema: Some(json!({
                "additionalProperties": false,
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
                "type": "object"
            })),
            output_schema: Some(json!({
                "additionalProperties": false,
                "properties": {"report": {"type": "string"}, "status": {"type": "string"}},
                "required": ["report", "status"],
                "type": "object"
            })),
            ..FunctionToolConfig::new(
                "get_weather_report",
                "Retrieves the current weather report for a specified city.",
            )
        },
        move |_ctx: Context, input: WeatherArgs| {
            let city = input.city.to_lowercase();
            let out = rs.get(city.as_str()).cloned().ok_or_else(|| {
                ToolError::Execution(
                    format!("weather information for {city:?} is not available").into(),
                )
            });
            async move { out }
        },
    )
    .unwrap();

    for (name, prompt, want, is_error) in [
        (
            "london",
            "Report the current weather of the capital city of U.K.",
            result_set["london"].clone(),
            false,
        ),
        (
            "paris",
            "How is the weather of Paris now?",
            result_set["paris"].clone(),
            false,
        ),
        (
            "new york",
            "Tell me about the current weather in New York",
            WeatherResult::default(),
            true,
        ),
    ] {
        let mut req = LlmRequest::default();
        pack_tool(&mut req, &tool).unwrap();
        assert_eq!(
            req.config.as_ref().map(|c| c.tools.len()),
            Some(1),
            "{name}: pack_tool did not configure tool info"
        );
        req.contents = gadk_genai::text(prompt);
        let fc = first_function_call(m.generate_content(req, false)).await;
        assert!(
            fc.name == "get_weather_report" && !fc.args.is_empty(),
            "{name}: unexpected function call {fc:?}"
        );
        let res = tool
            .as_runnable()
            .unwrap()
            .run(ctx(), Value::Object(fc.args.clone()))
            .await;
        if is_error {
            assert!(
                res.is_err(),
                "{name}: Run({:?}) expected to fail but got {res:?}",
                fc.args
            );
            continue;
        }
        let got: WeatherResult =
            convert_with_schema::<Map<String, Value>, WeatherResult>(&res.unwrap(), None)
                .unwrap_or_else(|e| panic!("{name}: ConvertToWithJSONSchema failed: {e}"));
        assert_eq!(got, want, "{name}");
    }
}

#[tokio::test]
async fn test_function_tool_different_function_declarations_consolidated_in_one_gen_ai_tool() {
    #[derive(Deserialize, JsonSchema)]
    struct IntInput {
        x: i64,
    }
    #[derive(Serialize, JsonSchema)]
    struct IntOutput {
        result: i64,
    }
    #[derive(Deserialize, JsonSchema)]
    struct StringInput {
        value: String,
    }
    #[derive(Serialize, JsonSchema)]
    struct StringOutput {
        result: String,
    }
    let identity = FunctionTool::new(
        FunctionToolConfig::new("identity", "returns the input value"),
        |_ctx: Context, input: IntInput| async move {
            Ok::<_, ToolError>(IntOutput { result: input.x })
        },
    )
    .unwrap();
    let string_identity = FunctionTool::new(
        FunctionToolConfig::new("string_identity", "returns the input value"),
        |_ctx: Context, input: StringInput| async move {
            Ok::<_, ToolError>(StringOutput {
                result: input.value,
            })
        },
    )
    .unwrap();
    let mut req = LlmRequest::default();
    pack_tool(&mut req, &identity).unwrap();
    pack_tool(&mut req, &string_identity).unwrap();
    let cfg = req.config.unwrap();
    assert_eq!(cfg.tools.len(), 1, "number of tools should be one");
    assert_eq!(
        cfg.tools[0].function_declarations.len(),
        2,
        "number of function declarations should be two"
    );
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn fc_content(name: &str, args: Value) -> Content {
    Content::from_function_call(
        FunctionCall {
            name: name.into(),
            args: obj(args),
            ..Default::default()
        },
        "model",
    )
}

fn fr_content(name: &str, response: Value) -> Content {
    Content::from_function_response(
        FunctionResponse {
            name: name.into(),
            response: obj(response),
            ..Default::default()
        },
        "user",
    )
}

/// Go `cmpopts.IgnoreFields(genai.FunctionCall{}, "ID")` / `(genai.FunctionResponse{}, "ID")`, which also
/// reach the `*genai.FunctionCall` held in a confirmation call's `originalFunctionCall` argument.
fn strip_ids(mut c: Content) -> Content {
    for p in &mut c.parts {
        if let Some(fc) = p.function_call.as_mut() {
            fc.id.clear();
            if fc.name == REQUEST_CONFIRMATION_FUNCTION_CALL_NAME
                && let Some(Value::Object(orig)) = fc.args.get_mut("originalFunctionCall")
            {
                orig.remove("id");
            }
        }
        if let Some(fr) = p.function_response.as_mut() {
            fr.id.clear();
        }
    }
    c
}

/// Collects events until the first error (Go testutil.Collect* keep the partial result).
async fn drain(mut s: EventStream) -> (Vec<Event>, Option<AgentError>) {
    let mut events = Vec::new();
    while let Some(item) = s.next().await {
        match item {
            Ok(e) => events.push(e),
            Err(e) => return (events, Some(e)),
        }
    }
    (events, None)
}

#[tokio::test]
async fn test_function_tool_returns_basic_type() {
    let result_set: Arc<HashMap<&'static str, &'static str>> = Arc::new(HashMap::from([
        (
            "london",
            "The current weather in London is cloudy with a temperature of18 degrees Celsius and a chance of rain.",
        ),
        (
            "paris",
            "The weather in Paris is sunny with a temperature of 25 derees Celsius.",
        ),
    ]));
    let rs = result_set.clone();
    let weather_report_tool = FunctionTool::new(
        FunctionToolConfig::new(
            "get_weather_report",
            "Retrieves the current weather report for a specified city.",
        ),
        move |_ctx: Context, input: WeatherArgs| {
            let city = input.city.to_lowercase();
            let out = match rs.get(city.as_str()) {
                Some(r) => r.to_string(),
                None => format!(
                    "Weather information for {} is not available.",
                    gadk_agent::go_quote(&city)
                ),
            };
            async move { Ok::<String, ToolError>(out) }
        },
    )
    .unwrap_or_else(|e| panic!("NewFunctionTool failed: {e}"));

    for (name, args, want) in [
        (
            "london",
            json!({"city": "london"}),
            result_set["london"].to_string(),
        ),
        (
            "paris",
            json!({"city": "paris"}),
            result_set["paris"].to_string(),
        ),
        (
            "new york",
            json!({"city": "new york"}),
            r#"Weather information for "new york" is not available."#.to_string(),
        ),
    ] {
        let mut req = LlmRequest::default();
        pack_tool(&mut req, &weather_report_tool)
            .unwrap_or_else(|e| panic!("{name}: ProcessRequest failed: {e}"));
        assert_eq!(
            req.config.as_ref().map(|c| c.tools.len()),
            Some(1),
            "{name}: ProcessRequest did not configure tool info in LLMRequest"
        );
        let call_result = weather_report_tool
            .as_runnable()
            .unwrap()
            .run(ctx(), args)
            .await
            .unwrap_or_else(|e| panic!("{name}: weatherReportTool.Run failed: {e}"));
        let got =
            convert_with_schema::<Map<String, Value>, BTreeMap<String, String>>(&call_result, None)
                .unwrap_or_else(|e| {
                    panic!("{name}: Run returned unexpected result {call_result:?}: {e}")
                });
        let got_val = got
            .get("result")
            .unwrap_or_else(|| panic!("{name}: function response has no \"result\" value"));
        assert_eq!(
            got_val, &want,
            "{name}: weatherReportTool.Run returned unexpected result"
        );
    }
}

#[derive(Deserialize, JsonSchema)]
struct NoArgs {}

/// A value `serde_json` cannot represent (Go: NaN, ±Inf, a channel).
#[derive(JsonSchema)]
struct Unrepresentable {
    #[allow(dead_code)] // only its schema is used
    score: f64,
}

impl Serialize for Unrepresentable {
    fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("unsupported value: NaN"))
    }
}

// parity: adapted: serde_json turns NaN/±Inf into null and Rust has no channel type, so the port uses an Out whose Serialize impl fails (custom impl returning S::Error::custom("unsupported value: NaN")) and asserts Run returns Err containing "unsupported value" and no result
#[tokio::test]
async fn test_function_tool_result_not_representable() {
    let tl = FunctionTool::new(
        FunctionToolConfig::new("nan_map_tool", "a tool whose result map holds NaN"),
        |_ctx: Context, _in: NoArgs| async move {
            Ok::<_, ToolError>(Unrepresentable { score: f64::NAN })
        },
    )
    .unwrap_or_else(|e| panic!("functiontool.New failed: {e}"));
    let got = tl
        .as_runnable()
        .expect("not runnable")
        .run(ctx(), json!({}))
        .await;
    match got {
        Ok(m) => panic!("Run returned result {m:?}, want an error"),
        Err(e) => assert!(
            e.to_string().contains("unsupported value"),
            "Run returned error {e}, want it to contain \"unsupported value\""
        ),
    }
}

#[derive(Serialize, JsonSchema)]
struct Player {
    name: String,
    score: f64,
}

fn thy() -> Player {
    Player {
        name: "THY".into(),
        score: 5.4186,
    }
}

/// Go `time.Time`: the inferred schema is an object, but it marshals to a string.
#[derive(JsonSchema)]
struct Clock {
    #[allow(dead_code)] // only its schema is used
    secs: i64,
}

impl Serialize for Clock {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("1970-01-01T00:00:00Z")
    }
}

/// Go `objectMarshaler`: marshals to an object whose key its inferred schema does not describe.
#[derive(JsonSchema)]
struct ObjectMarshaler {
    #[allow(dead_code)] // only its schema is used
    n: i64,
}

impl Serialize for ObjectMarshaler {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("other", &1)?;
        m.end()
    }
}

type RunResult = Result<Map<String, Value>, ToolError>;
type NewResult = Result<Arc<dyn gadk_agent::Tool>, ToolError>;

fn run_no_args<Out, F>(
    cfg: FunctionToolConfig,
    f: F,
) -> impl std::future::Future<Output = RunResult>
where
    Out: Serialize + JsonSchema + Send + 'static,
    F: Fn() -> Out + Send + Sync + 'static,
{
    let tl = FunctionTool::new(cfg, move |_ctx: Context, _in: NoArgs| {
        let out = f();
        async move { Ok::<_, ToolError>(out) }
    })
    .unwrap_or_else(|e| panic!("functiontool.New failed: {e}"));
    async move {
        tl.as_runnable()
            .expect("not runnable")
            .run(ctx(), json!({}))
            .await
    }
}

// parity: adapted: Rust analogs of Go value kinds; byte slices serialize as integer arrays, pre-encoded json.RawMessage has no FunctionTool analog (case dropped)
#[tokio::test]
async fn test_function_tool_wraps_non_map_result() {
    let player_json = json!([{"name": "THY", "score": 5.4186}]);
    let cases: Vec<(&str, RunResult, Value)> = vec![
        (
            "slice_of_structs",
            run_no_args(
                FunctionToolConfig::new("roster_tool", "a tool returning a slice of structs"),
                || vec![thy()],
            )
            .await,
            player_json.clone(),
        ),
        (
            "slice_of_struct_pointers",
            run_no_args(
                FunctionToolConfig::new(
                    "roster_ptr_tool",
                    "a tool returning a slice of struct pointers",
                ),
                || vec![Box::new(thy())],
            )
            .await,
            player_json,
        ),
        (
            "struct_marshaling_to_a_string",
            run_no_args(
                FunctionToolConfig::new("clock_tool", "a tool returning a time"),
                || Clock { secs: 0 },
            )
            .await,
            json!("1970-01-01T00:00:00Z"),
        ),
        (
            "struct_marshaling_to_other_keys",
            run_no_args(
                FunctionToolConfig::new(
                    "object_marshaler_tool",
                    "a tool returning a value with its own MarshalJSON",
                ),
                || ObjectMarshaler { n: 1 },
            )
            .await,
            json!({"other": 1}),
        ),
        (
            "struct_with_configured_output_schema",
            run_no_args(
                FunctionToolConfig {
                    output_schema: Some(json!({"type": "string"})),
                    ..FunctionToolConfig::new(
                        "configured_clock_tool",
                        "a tool returning a time, validated against the configured schema",
                    )
                },
                || Clock { secs: 0 },
            )
            .await,
            json!("1970-01-01T00:00:00Z"),
        ),
        (
            "byte_slice",
            run_no_args(
                FunctionToolConfig::new(
                    "bytes_tool",
                    "a tool returning bytes, which marshal to a base64 string",
                ),
                || b"ab".to_vec(),
            )
            .await,
            json!([97, 98]),
        ),
        (
            "nil_struct_pointer",
            run_no_args(
                FunctionToolConfig::new("absent_tool", "a tool returning no value"),
                || Option::<Player>::None,
            )
            .await,
            Value::Null,
        ),
    ];
    for (name, got, want) in cases {
        let got = got.unwrap_or_else(|e| panic!("{name}: Run returned error {e}, want nil"));
        assert_eq!(
            got,
            obj(json!({ "result": want })),
            "{name}: Run returned unexpected result"
        );
    }
}

// parity: adapted: jsonschema crate error text differs from jsonschema-go; asserts Err and no result
#[tokio::test]
async fn test_function_tool_output_schema_mismatch() {
    let got = run_no_args(
        FunctionToolConfig {
            output_schema: Some(json!({"type": "string", "maxLength": 3})),
            ..FunctionToolConfig::new(
                "short_text_tool",
                "a tool whose result must be at most three characters",
            )
        },
        || "abcdefgh".to_string(),
    )
    .await;
    assert!(got.is_err(), "Run returned {got:?}, want an error");
}

#[tokio::test]
async fn test_function_tool_map_input() {
    #[derive(Serialize, Deserialize, JsonSchema, PartialEq, Debug)]
    struct Output {
        sum: i64,
    }
    let sum_tool = FunctionTool::new(
        FunctionToolConfig::new("sum_map", "sums numbers provided in a map input"),
        |_ctx: Context, input: BTreeMap<String, i64>| async move {
            Ok::<_, ToolError>(Output {
                sum: input.get("a").copied().unwrap_or_default()
                    + input.get("b").copied().unwrap_or_default(),
            })
        },
    )
    .unwrap_or_else(|e| panic!("NewFunctionTool failed: {e}"));
    let call_result = sum_tool
        .as_runnable()
        .expect("sumTool is not runnable")
        .run(ctx(), json!({"a": 2, "b": 3}))
        .await
        .unwrap_or_else(|e| panic!("sumTool.Run failed: {e}"));
    let got = convert_with_schema::<Map<String, Value>, Output>(&call_result, None)
        .unwrap_or_else(|e| panic!("sumTool.Run returned unexpected result {call_result:?}: {e}"));
    assert_eq!(got, Output { sum: 5 });
}

// parity: adapted: the input schema is a literal (Go builds it with jsonschema.For and edits it)
#[tokio::test]
async fn test_function_tool_custom_schema() {
    #[derive(Deserialize, JsonSchema)]
    struct Args {
        /// Either apple or orange, nothing else.
        fruit: String,
    }
    let ischema = json!({
        "type": "object",
        "properties": {
            "fruit": {
                "type": "string",
                "description": "print the remaining quantity of the item.",
                "enum": ["mandarin", "kiwi"]
            }
        },
        "required": ["fruit"],
        "additionalProperties": false
    });
    let bad_fruit = Arc::new(AtomicBool::new(false));
    let bf = bad_fruit.clone();
    let inventory_tool = FunctionTool::new(
        FunctionToolConfig {
            input_schema: Some(ischema.clone()),
            ..FunctionToolConfig::new(
                "print_quantity",
                "print the remaining quantity of the given fruit.",
            )
        },
        move |_ctx: Context, input: Args| {
            let fruit = input.fruit.to_lowercase();
            if fruit != "mandarin" && fruit != "kiwi" {
                bf.store(true, SeqCst);
            }
            async move { Ok::<Value, ToolError>(Value::Null) } // always return nil.
        },
    )
    .unwrap_or_else(|e| panic!("NewFunctionTool failed: {e}"));

    // ProcessRequest
    {
        let mut req = LlmRequest::default();
        pack_tool(&mut req, &inventory_tool)
            .unwrap_or_else(|e| panic!("ProcessRequest: inventoryTool.ProcessRequest failed: {e}"));
        let decl = req
            .config
            .as_ref()
            .and_then(|c| c.tools.first())
            .and_then(|t| t.function_declarations.first())
            .unwrap_or_else(|| panic!("ProcessRequest: inventoryTool.ProcessRequest did not configure function declaration: {req:?}"));
        assert_eq!(
            decl.name,
            inventory_tool.name(),
            "ProcessRequest: function declaration name"
        );
        assert_eq!(
            decl.description,
            inventory_tool.description(),
            "ProcessRequest: function declaration description"
        );
        assert_eq!(
            decl.parameters_json_schema.as_ref(),
            Some(&ischema),
            "ProcessRequest: function declaration parameter json schema"
        );
        // Go: stringify(&jsonschema.Schema{}), the schema inferred for an `any` result.
        assert_eq!(
            decl.response_json_schema,
            Some(json!({})),
            "ProcessRequest: function response json schema"
        );
    }

    // Run
    for (name, input, want_err) in [
        ("valid_item", json!({"fruit": "mandarin"}), false),
        ("invalid_item", json!({"fruit": "banana"}), true),
        ("unexpected_type", json!({"fruit": 1}), true),
        ("nil", Value::Null, true),
    ] {
        let ret = inventory_tool
            .as_runnable()
            .expect("inventoryTool is not runnable")
            .run(ctx(), input)
            .await;
        if want_err {
            assert!(
                ret.is_err(),
                "Run/{name}: inventoryTool.Run = {ret:?}, want error"
            );
        } else if let Ok(m) = &ret {
            // Go: "for valid_item it returns an empty map instead of nil"; either way no content.
            assert!(
                m.is_empty(),
                "Run/{name}: inventoryTool.Run = {ret:?}, want (nil, nil)"
            );
        }
    }
    assert!(
        !bad_fruit.load(SeqCst),
        "unexpected fruit reached the handler"
    );
}

#[derive(Deserialize, JsonSchema)]
struct SimpleArgs {
    #[serde(rename = "Num")]
    num: i64,
}

async fn ok_func(_ctx: Context, _args: SimpleArgs) -> Result<String, ToolError> {
    Ok("ok".into())
}

const CONFIRM_HINT: &str = "Please approve or reject the tool call test_tool() by responding with a FunctionResponse with an expected ToolConfirmation payload.";

fn confirmation_call(n: i64) -> Content {
    fc_content(
        REQUEST_CONFIRMATION_FUNCTION_CALL_NAME,
        json!({
            "originalFunctionCall": serde_json::to_value(FunctionCall { name: "test_tool".into(), args: obj(json!({"Num": n})), ..Default::default() }).unwrap(),
            "toolConfirmation": serde_json::to_value(ToolConfirmation { hint: CONFIRM_HINT.into(), confirmed: false, payload: None }).unwrap(),
        }),
    )
}

/// How a confirmation case builds its tool: a static `require_confirmation`, or the `Num < 5` provider.
#[derive(Clone, Copy)]
enum Build {
    Static(bool),
    Conditional,
}

/// (name, build, num, confirmation reply, want)
type ConfirmationCase = (&'static str, Build, i64, Option<bool>, Vec<Content>);

#[tokio::test]
async fn test_tool_confirmation() {
    let requires = || {
        fr_content(
            "test_tool",
            json!({"error": "error tool \"test_tool\" requires confirmation, please approve or reject"}),
        )
    };
    let ok = || fr_content("test_tool", json!({"result": "ok"}));
    let rejected = || {
        fr_content(
            "test_tool",
            json!({"error": "error tool \"test_tool\" call is rejected"}),
        )
    };

    // (name, build, num, confirm reply, want)
    let cases: Vec<ConfirmationCase> = vec![
        (
            "No Confirmation Required",
            Build::Static(false),
            1,
            None,
            vec![fc_content("test_tool", json!({"Num": 1})), ok()],
        ),
        (
            "Confirmation Required",
            Build::Static(true),
            1,
            None,
            vec![
                fc_content("test_tool", json!({"Num": 1})),
                requires(),
                confirmation_call(1),
            ],
        ),
        (
            "Confirmation Required and is confirmed",
            Build::Static(true),
            1,
            Some(true),
            vec![
                fc_content("test_tool", json!({"Num": 1})),
                requires(),
                confirmation_call(1),
                ok(),
            ],
        ),
        (
            "Confirmation Required and is rejected",
            Build::Static(true),
            1,
            Some(false),
            vec![
                fc_content("test_tool", json!({"Num": 1})),
                requires(),
                confirmation_call(1),
                rejected(),
            ],
        ),
        (
            "Conditional Confirmation Not Required",
            Build::Conditional,
            7,
            None,
            vec![fc_content("test_tool", json!({"Num": 7})), ok()],
        ),
        (
            "Conditional Confirmation Required",
            Build::Conditional,
            4,
            None,
            vec![
                fc_content("test_tool", json!({"Num": 4})),
                requires(),
                confirmation_call(4),
            ],
        ),
        (
            "Conditional Confirmation Required and is confirmed",
            Build::Conditional,
            4,
            Some(true),
            vec![
                fc_content("test_tool", json!({"Num": 4})),
                requires(),
                confirmation_call(4),
                ok(),
            ],
        ),
        (
            "Conditional Confirmation Required and is rejected",
            Build::Conditional,
            4,
            Some(false),
            vec![
                fc_content("test_tool", json!({"Num": 4})),
                requires(),
                confirmation_call(4),
                rejected(),
            ],
        ),
    ];

    for (name, build, n, reply, want) in cases {
        let model = MockModel::from_contents(vec![fc_content("test_tool", json!({"Num": n}))]);
        let my_tool = match build {
            Build::Static(require_confirmation) => FunctionTool::new(
                FunctionToolConfig {
                    require_confirmation,
                    ..FunctionToolConfig::new("test_tool", "")
                },
                ok_func,
            ),
            Build::Conditional => FunctionTool::with_confirmation_provider(
                FunctionToolConfig::new("test_tool", ""),
                ok_func,
                |a: &SimpleArgs| a.num < 5,
            ),
        }
        .unwrap_or_else(|e| panic!("{name}: Failed to create tool: {e}"));
        let a = llm_agent::new(LlmAgentConfig {
            name: "simple agent".into(),
            model: Some(model),
            tools: vec![my_tool],
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{name}: failed to create llm agent: {e}"));
        let runner = TestAgentRunner::new(a).await;

        let mut got = Vec::new();
        let (events, err) = drain(runner.run("message")).await;
        if let Some(e) = err {
            assert!(
                matches!(e, AgentError::Model(_)),
                "{name}: runner returned unexpected error: {e}"
            );
        }
        got.extend(events);

        let confirm_call_id = got
            .iter()
            .filter_map(|e| e.response.content.as_ref())
            .flat_map(|c| c.parts.iter())
            .filter_map(|p| p.function_call.as_ref())
            .filter(|fc| fc.name == REQUEST_CONFIRMATION_FUNCTION_CALL_NAME)
            .map(|fc| fc.id.clone())
            .last();
        if let (Some(id), Some(confirmed)) = (confirm_call_id, reply) {
            let reply_content = Content {
                parts: vec![Part {
                    function_response: Some(FunctionResponse {
                        id,
                        name: REQUEST_CONFIRMATION_FUNCTION_CALL_NAME.into(),
                        response: obj(json!({"confirmed": confirmed})),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let (events, err) = drain(runner.run_content(reply_content)).await;
            if let Some(e) = err {
                assert!(
                    matches!(e, AgentError::Model(_)),
                    "{name}: runner returned unexpected error: {e}"
                );
            }
            got.extend(events);
        }

        assert!(
            got.len() <= want.len(),
            "{name}: stream generated more values than the expected {}: {got:?}",
            want.len()
        );
        for (i, (ev, want)) in got.iter().zip(&want).enumerate() {
            let content = strip_ids(
                ev.response
                    .content
                    .clone()
                    .unwrap_or_else(|| panic!("{name}: event {i} has no content")),
            );
            assert_eq!(&content, want, "{name}: event {i} mismatch");
        }
        assert_eq!(got.len(), want.len(), "{name}: unexpected stream length");
    }
}

#[tokio::test]
async fn test_tool_confirmation_yields_function_response_before_confirmation() {
    let model = MockModel::from_contents(vec![fc_content("test_tool", json!({"Num": 1}))]);
    let my_tool = FunctionTool::new(
        FunctionToolConfig {
            require_confirmation: true,
            ..FunctionToolConfig::new("test_tool", "")
        },
        ok_func,
    )
    .unwrap_or_else(|e| panic!("Failed to create tool: {e}"));
    let a = llm_agent::new(LlmAgentConfig {
        name: "simple agent".into(),
        model: Some(model),
        tools: vec![my_tool],
        ..Default::default()
    })
    .unwrap_or_else(|e| panic!("failed to create llm agent: {e}"));
    let runner = TestAgentRunner::new(a).await;

    let mut saw_function_response = false;
    let mut s = runner.run("message");
    // The mock model emits a sentinel error once exhausted; treat any error as end-of-stream.
    while let Some(Ok(got)) = s.next().await {
        for part in got.response.content.iter().flat_map(|c| c.parts.iter()) {
            if part
                .function_response
                .as_ref()
                .is_some_and(|fr| fr.name == "test_tool")
            {
                saw_function_response = true;
            }
            if part
                .function_call
                .as_ref()
                .is_some_and(|fc| fc.name == REQUEST_CONFIRMATION_FUNCTION_CALL_NAME)
            {
                assert!(
                    saw_function_response,
                    "confirmation was yielded before the tool function response"
                );
                return;
            }
        }
    }
    panic!("expected confirmation event");
}

#[derive(Deserialize, JsonSchema)]
struct TestArgs {
    #[allow(dead_code)] // only its schema is used
    name: String,
}

#[derive(Serialize, JsonSchema)]
struct TestResult {
    value: i64,
}

async fn dummy_handler(_ctx: Context, _args: TestArgs) -> Result<TestResult, ToolError> {
    Ok(TestResult { value: 1 })
}

// parity: adapted: Rust's provider is typed (P: Fn(&Args) -> bool), so every Go invalid-signature case is a compile error; the port asserts the two valid cases construct
#[tokio::test]
async fn test_new_require_confirmation_provider_validation() {
    let nil_provider = FunctionTool::new(FunctionToolConfig::new("", ""), dummy_handler);
    assert!(
        nil_provider.is_ok(),
        "Valid: Nil provider is allowed: unexpected error: {:?}",
        nil_provider.err()
    );
    let correct = FunctionTool::with_confirmation_provider(
        FunctionToolConfig::new("", ""),
        dummy_handler,
        |_: &TestArgs| true,
    );
    assert!(
        correct.is_ok(),
        "Valid: Correct function signature: unexpected error: {:?}",
        correct.err()
    );
}

#[tokio::test]
async fn test_new_invalid_input_type() {
    let cases: Vec<(&str, NewResult)> = vec![
        (
            "string_input",
            FunctionTool::new(
                FunctionToolConfig::new("string_tool", "a tool with string input"),
                |_ctx: Context, input: String| async move { Ok::<_, ToolError>(input) },
            ),
        ),
        (
            "int_input",
            FunctionTool::new(
                FunctionToolConfig::new("int_tool", "a tool with int input"),
                |_ctx: Context, input: i64| async move { Ok::<_, ToolError>(input) },
            ),
        ),
        (
            "bool_input",
            FunctionTool::new(
                FunctionToolConfig::new("bool_tool", "a tool with bool input"),
                |_ctx: Context, input: bool| async move { Ok::<_, ToolError>(input) },
            ),
        ),
    ];
    for (name, res) in cases {
        match res {
            Ok(_) => panic!("{name}: functiontool.New() succeeded, want an error"),
            Err(e) => assert!(
                matches!(e, ToolError::InvalidArgument(_)),
                "{name}: functiontool.New() error = {e:?}, want ErrInvalidArgument"
            ),
        }
    }
}

// parity: adapted: no Go stack trace in Rust; asserts the remaining message parts
#[tokio::test]
async fn test_function_tool_panic_recovery() {
    #[derive(Deserialize, JsonSchema)]
    struct Args {
        #[allow(dead_code)] // only its schema is used
        value: String,
    }
    let panic_tool = FunctionTool::new(
        FunctionToolConfig::new("panic_tool", "a tool that always panics"),
        |_ctx: Context, _input: Args| async move {
            if true {
                panic!("intentional panic for testing");
            }
            Ok::<String, ToolError>(String::new())
        },
    )
    .unwrap_or_else(|e| panic!("NewFunctionTool failed: {e}"));
    let result = panic_tool
        .as_runnable()
        .expect("panicTool is not runnable")
        .run(ctx(), json!({"value": "test"}))
        .await;
    let err = match result {
        Ok(m) => panic!("expected error from panic recovery, got result {m:?}"),
        Err(e) => e.to_string(),
    };
    for part in [
        "panic in tool",
        "panic_tool",
        "intentional panic for testing",
    ] {
        assert!(
            err.contains(part),
            "expected error to contain {part:?}, but it did not. Error: {err}"
        );
    }
}
