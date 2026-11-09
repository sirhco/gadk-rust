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

//! Port of the Go internal/llminternal request/response processor chain (base_flow.go:87-112).

pub(crate) mod basic;
pub(crate) mod compaction;
pub(crate) mod confirmation;
pub(crate) mod contents;
pub(crate) mod identity;
pub(crate) mod instructions;
pub(crate) mod output_schema;
pub(crate) mod stubs;
pub(crate) mod tools;

use std::{error::Error as StdError, fmt};

use gadk_agent::AgentError;
use serde_json::Value;

use crate::{
    llm::flow::{RequestProcessor, ResponseProcessor},
    workflow::route::go_quote,
};

/// Port of base_flow.go:88 DefaultRequestProcessors, in Go order.
pub(crate) fn default_request_processors() -> Vec<RequestProcessor> {
    vec![
        basic::basic_request_processor as RequestProcessor,
        tools::tools_processor,
        stubs::auth_preprocessor,
        confirmation::request_confirmation_processor,
        instructions::instructions_request_processor,
        identity::identity_request_processor,
        compaction::compaction_request_processor,
        contents::contents_request_processor,
        stubs::nl_planning_request_processor,
        stubs::code_execution_request_processor,
        output_schema::output_schema_request_processor,
        crate::llm::transfer::agent_transfer_request_processor,
        contents::remove_display_name_if_exists,
    ]
}

/// Port of base_flow.go:109 DefaultResponseProcessors.
pub(crate) fn default_response_processors() -> Vec<ResponseProcessor> {
    vec![
        stubs::nl_planning_response_processor as ResponseProcessor,
        stubs::code_execution_response_processor as ResponseProcessor,
    ]
}

/// Go `fmt.Errorf("<msg>: %w", source)`: Display is "<msg>: <source>", and `source()` keeps the
/// chain for errors.Is ports.
#[derive(Debug)]
pub(crate) struct WrappedError {
    msg: String,
    source: Box<dyn StdError + Send + Sync>,
}

impl fmt::Display for WrappedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.msg, self.source)
    }
}

impl StdError for WrappedError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&*self.source)
    }
}

/// Go `fmt.Errorf("<msg>: %w", source)` as an [`AgentError::Other`].
pub(crate) fn wrap_err(
    msg: impl Into<String>,
    source: impl Into<Box<dyn StdError + Send + Sync>>,
) -> AgentError {
    AgentError::Other(Box::new(WrappedError {
        msg: msg.into(),
        source: source.into(),
    }))
}

/// Go `json.Marshal` of a decoded JSON value (keys sorted, HTML-escaped strings, Go float
/// format), through the workspace's one Go-JSON encoder, [`gadk_model::to_go_json`]. Used where
/// Go json.Marshal output reaches the model or an event (contents.go stringify, outputschema
/// retrieveStructuredModelResponse, nodeInputToContent).
pub(crate) fn go_json_marshal(v: &Value) -> String {
    gadk_model::to_go_json(v)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Go `fmt.Sprintf("%v", v)` for a JSON-decoded value:
/// - string → itself; bool → true/false; null → `<nil>`;
/// - integer numbers → decimal (Go's native ints; a Go value JSON-decoded into float64 would print
///   %g, e.g. 1e+06, but Rust keeps integer JSON numbers as integers);
/// - other numbers → `go_format_float_v` (Go %v of float64 = %g with shortest digits);
/// - array → `[a b c]`; object → `map[k1:v1 k2:v2]` with keys sorted (Go fmt sorts map keys).
///
/// Callers formatting a value Go would have JSON-decoded (tool args, model output) must apply
/// [`as_go_floats`] first, so numbers print as Go `float64` (`1e+06`, not `1000000`).
pub(crate) fn go_sprint(v: &Value) -> String {
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
/// with no finite float form is kept as is. Apply before [`go_sprint`] on JSON-decoded values.
pub(crate) fn as_go_floats(v: &Value) -> Value {
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
/// form (`d.ddde±XX`, at least two exponent digits) when the decimal exponent is < -4 or >= 6
/// (strconv ftoa.go `%g`: `if shortest { eprec = 6 }; exp := digs.dp - 1; if exp < -4 || exp >=
/// eprec` → %e), else plain decimal with no trailing zeros. So 1e6 → "1e+06", 1234567.0 →
/// "1.234567e+06", 123456.0 → "123456", 1.5 → "1.5", 0.0001 → "0.0001", 0.00001 → "1e-05".
fn go_format_float_v(f: f64) -> String {
    let sci = format!("{f:e}"); // Rust: shortest round-trip mantissa, e.g. "1.234567e6", "1e-5", "0e0"
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..6).contains(&exp) {
        format!(
            "{mant}e{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.unsigned_abs()
        )
    } else {
        format!("{f}") // Rust Display: shortest round-trip, never exponent form
    }
}

/// Go `%q` of a `[]string` (log lines): `["a" "b"]`, each element quoted by Go
/// `strconv.Quote` ([`go_quote`]).
pub(crate) fn go_quote_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| go_quote(s))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
mod test_support {
    use std::{any::Any, future::Future, sync::Arc};

    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use gadk_agent::{
        Agent, AgentConfig, AgentState, AgentType, Context, RunnableTool, Tool, ToolError, agent,
    };
    use gadk_genai::FunctionDeclaration;
    use gadk_model::{Backend, Llm, LlmRequest, LlmResponse, ModelError};
    use serde_json::{Map, Value};
    use tracing_subscriber::{Layer, layer::SubscriberExt};

    use crate::llm::{config::LlmAgentConfig, state::LlmAgentState};

    /// Go mockLLMAgent{Agent: agent.New(Config{Name, Description, SubAgents}), s: &State{...}}.
    pub(crate) fn test_llm_agent(cfg: LlmAgentConfig) -> Arc<dyn Agent> {
        let base = AgentConfig {
            name: cfg.name.clone(),
            description: cfg.description.clone(),
            sub_agents: cfg.sub_agents.clone(),
            ..Default::default()
        };
        let state = Arc::new(LlmAgentState::new(cfg));
        agent::new_with_state(
            base,
            AgentState {
                agent_type: AgentType::LlmAgent,
                config: Some(state),
                on_before_agent_callback_event: None,
                live: None,
            },
        )
        .expect("test agent")
    }

    /// Go mockLLM (outputschema_processor_test.go:45): name + google variant (default Gemini API).
    pub(crate) struct TestLlm {
        name: String,
        variant: Backend,
    }

    impl TestLlm {
        pub(crate) fn named(name: &str) -> Arc<dyn Llm> {
            Self::with_variant(name, Backend::GeminiApi)
        }

        pub(crate) fn with_variant(name: &str, variant: Backend) -> Arc<dyn Llm> {
            Arc::new(Self {
                name: name.into(),
                variant,
            })
        }
    }

    impl Llm for TestLlm {
        fn name(&self) -> &str {
            &self.name
        }
        fn generate_content(
            &self,
            _: LlmRequest,
            _: bool,
        ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
            Box::pin(futures::stream::empty())
        }
        fn google_variant(&self) -> Option<Backend> {
            Some(self.variant)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Go mockTool: declaration {name}, Run returns an empty result.
    pub(crate) struct TestTool {
        name: String,
    }

    impl TestTool {
        pub(crate) fn named(name: &str) -> Arc<dyn Tool> {
            Arc::new(Self { name: name.into() })
        }
    }

    impl Tool for TestTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "mock tool"
        }
        fn declaration(&self) -> Option<FunctionDeclaration> {
            Some(FunctionDeclaration {
                name: self.name.clone(),
                ..Default::default()
            })
        }
        fn as_runnable(&self) -> Option<&dyn RunnableTool> {
            Some(self)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[async_trait]
    impl RunnableTool for TestTool {
        async fn run(&self, _: Context, _: Value) -> Result<Map<String, Value>, ToolError> {
            Ok(Map::new())
        }
    }

    /// Go captureLog: records the `message` of every tracing event with target "adk", one line
    /// each.
    pub(crate) async fn capture_logs<F: Future>(fut: F) -> (F::Output, String) {
        crate::llm::flow::span_capture::pin_tracing_interest();
        let buf = Arc::new(parking_lot::Mutex::new(String::new()));
        let sub = tracing_subscriber::registry().with(MessageCapture(buf.clone()));
        let _g = tracing::subscriber::set_default(sub);
        let out = fut.await;
        let log = buf.lock().clone();
        (out, log)
    }

    struct MessageCapture(Arc<parking_lot::Mutex<String>>);

    impl<S: tracing::Subscriber> Layer<S> for MessageCapture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() != "adk" {
                return;
            }
            struct V(String);
            impl tracing::field::Visit for V {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        self.0 = format!("{v:?}");
                    }
                }
                fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
                    if f.name() == "message" {
                        self.0 = v.to_owned();
                    }
                }
            }
            let mut v = V(String::new());
            event.record(&mut v);
            let mut b = self.0.lock();
            b.push_str(&v.0);
            b.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gadk_agent::InvocationContext;
    use gadk_model::LlmRequest;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::llm::{config::LlmAgentConfig, flow::Flow};

    #[test]
    fn go_sprint_formats_like_go_percent_v() {
        assert_eq!(go_sprint(&json!(30)), "30");
        assert_eq!(go_sprint(&json!(1.5)), "1.5");
        assert_eq!(go_sprint(&json!(1e6)), "1e+06");
        assert_eq!(go_sprint(&json!(1234567.0)), "1.234567e+06");
        assert_eq!(go_sprint(&json!(123456.0)), "123456");
        assert_eq!(go_sprint(&json!(0.0001)), "0.0001");
        assert_eq!(go_sprint(&json!(0.00001)), "1e-05");
        assert_eq!(go_sprint(&json!([1, "a"])), "[1 a]");
        assert_eq!(go_sprint(&json!({"b": 1, "a": true})), "map[a:true b:1]");
        assert_eq!(go_sprint(&json!(null)), "<nil>");
    }

    #[test]
    fn go_json_marshal_escapes_html() {
        assert_eq!(
            go_json_marshal(&json!({"b": "<x>&", "a": 1})),
            r#"{"a":1,"b":"\u003cx\u003e\u0026"}"#
        );
    }

    /// Expected strings are the output of Go 1.x `json.Marshal` on the same float64 values
    /// (byte-exact, not round-tripped through a parser).
    #[test]
    fn go_json_marshal_formats_floats_like_go() {
        for (v, want) in [
            (1.0, "1"),
            (100.0, "100"),
            (1e21, "1e+21"),
            (1.5e21, "1.5e+21"),
            (1e20, "100000000000000000000"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (-1.5e-10, "-1.5e-10"),
            (123.456, "123.456"),
            (0.1, "0.1"),
            (-0.0, "-0"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
        ] {
            assert_eq!(go_json_marshal(&json!(v)), want, "float {v:e}");
        }
        // Go: json.Unmarshal(`{"b":[1.0,2.5,"<x>&\u2028"],"a":3}`) then json.Marshal.
        let v: Value = serde_json::from_str(r#"{"b":[1.0,2.5,"<x>&\u2028"],"a":3}"#).unwrap();
        assert_eq!(
            go_json_marshal(&v),
            r#"{"a":3,"b":[1,2.5,"\u003cx\u003e\u0026\u2028"]}"#
        );
        // Go: json.Marshal("a\bb\fc\nd\re\tf\x01g\x7fh") — serde's control escapes match Go's.
        assert_eq!(
            go_json_marshal(&json!("a\u{8}b\u{c}c\nd\re\tf\u{1}g\u{7f}h")),
            "\"a\\bb\\fc\\nd\\re\\tf\\u0001g\u{7f}h\""
        );
    }

    #[tokio::test]
    async fn default_chain_wire_order() {
        let child = test_llm_agent(LlmAgentConfig {
            name: "child".into(),
            description: "kid".into(),
            ..Default::default()
        });
        let root = test_llm_agent(LlmAgentConfig {
            name: "root".into(),
            model: Some(TestLlm::named("gemini-2.5-flash")),
            instruction: "Be {tone?}.".into(),
            global_instruction: "Global.".into(),
            tools: vec![TestTool::named("user_tool")],
            sub_agents: vec![child],
            ..Default::default()
        });
        let pm = Arc::new(gadk_agent::ParentMap::new(&root).unwrap());
        let ctx = InvocationContext::builder()
            .agent(root)
            .parent_map(pm)
            .build_invocation();
        let flow = Flow {
            request_processors: default_request_processors(),
            ..Default::default()
        };
        let mut req = LlmRequest::default();
        for p in &flow.request_processors {
            p(&ctx, &mut req, &flow).await.unwrap();
        }
        let tools = flow.tools.read().clone();
        tools::tool_preprocess(&ctx, &mut req, &tools)
            .await
            .unwrap();
        tools::toolset_preprocess(&ctx, &mut req).await.unwrap();
        let cfg = req.config.unwrap();
        let names: Vec<_> = cfg
            .tools
            .iter()
            .flat_map(|t| t.function_declarations.iter().map(|d| d.name.clone()))
            .collect();
        assert_eq!(names, ["transfer_to_agent", "user_tool"]);
        let si = cfg.system_instruction.unwrap().parts[0].text.clone();
        let (g, i, id, tr) = (
            si.find("Global.").unwrap(),
            si.find("Be .").unwrap(),
            si.find("You are an agent.").unwrap(),
            si.find("You have a list of other agents").unwrap(),
        );
        assert!(g < i && i < id && id < tr, "SI order: {si:?}");
        assert_eq!(default_request_processors().len(), 13);
        assert_eq!(default_response_processors().len(), 2);
    }

    /// Go base_flow.go:951 toolPreprocess packs nothing itself, and a Go tool without a
    /// declaration never packs itself, so `tool_preprocess` packs only declared tools. The
    /// declaration-less tool's `process_request` still runs, in order.
    #[tokio::test]
    async fn tool_preprocess_packs_only_declared_tools() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

        use gadk_agent::{Context, Tool, ToolError, tool_util::append_instructions};

        struct Undeclared(Arc<AtomicBool>);

        #[async_trait::async_trait]
        impl Tool for Undeclared {
            fn name(&self) -> &str {
                "undeclared"
            }
            fn description(&self) -> &str {
                "a tool without a declaration"
            }
            async fn process_request(
                &self,
                _ctx: &Context,
                req: &mut LlmRequest,
            ) -> Result<(), ToolError> {
                self.0.store(true, SeqCst);
                append_instructions(req, &["from undeclared"]);
                Ok(())
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let ran = Arc::new(AtomicBool::new(false));
        let root = test_llm_agent(LlmAgentConfig {
            name: "root".into(),
            ..Default::default()
        });
        let ctx = InvocationContext::builder().agent(root).build_invocation();
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(Undeclared(ran.clone())),
            TestTool::named("declared"),
        ];
        let mut req = LlmRequest::default();
        tools::tool_preprocess(&ctx, &mut req, &tools)
            .await
            .unwrap();

        assert!(ran.load(SeqCst), "undeclared tool's process_request ran");
        assert!(
            !req.tools.contains_key("undeclared"),
            "a declaration-less tool must not be packed; req.tools keys: {:?}",
            req.tools.keys().collect::<Vec<_>>()
        );
        assert!(gadk_agent::tool_from_request(&req, "declared").is_some());
        assert_eq!(req.tools.len(), 1);
        let cfg = req.config.unwrap();
        let names: Vec<_> = cfg
            .tools
            .iter()
            .flat_map(|t| t.function_declarations.iter().map(|d| d.name.clone()))
            .collect();
        assert_eq!(names, ["declared"]);
        assert_eq!(
            cfg.system_instruction.unwrap().parts[0].text,
            "from undeclared"
        );
    }
}
