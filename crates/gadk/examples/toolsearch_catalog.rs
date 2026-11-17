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

//! The tool catalog and agent of the toolsearch example (`new_agent`, `Catalog`,
//! `catalog_tools`), shared by the `toolsearch` example and
//! `crates/gadk/tests/examples_toolsearch_main.rs` through `#[path]`. Not an example target:
//! `crates/gadk/Cargo.toml` sets `autoexamples = false`.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use gadk::agent::{Agent, Context, ReadonlyContext, Tool, ToolError, Toolset};
use gadk::engine::llm::LlmAgentConfig;
use gadk::engine::tool::{FunctionTool, FunctionToolConfig};
use gadk::model::Llm;
use gadk::tools::toolsearch;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The one tool the model sees before any search.
pub const CORE_TOOL: &str = "get_current_time";

/// An agent that sees `get_current_time` and `search_tools` up front.
/// Every other catalog tool is declared to the model only after `search_tools` has returned it.
pub fn new_agent(model: Arc<dyn Llm>) -> anyhow::Result<Arc<dyn Agent>> {
    let tools = catalog_tools()?;
    let gated_tool_names: Vec<String> = tools
        .iter()
        .map(|t| t.name().to_string())
        .filter(|name| name != CORE_TOOL)
        .collect();

    let gated = toolsearch::new(
        Arc::new(Catalog(tools)),
        toolsearch::Config {
            core_tool_names: vec![CORE_TOOL.to_string()],
            // Optional. Listing the gated names in the search_tools description lets the model
            // select one by exact name instead of guessing keywords.
            gated_tool_names,
            ..Default::default()
        },
    )?;

    Ok(gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "toolsearch_agent".to_string(),
        model: Some(model),
        description: "Agent that discovers its tools on demand.".to_string(),
        instruction: "You help with time, arithmetic, unit conversion and text questions. Use a tool for every calculation.".to_string(),
        toolsets: vec![gated],
        ..Default::default()
    })?)
}

/// The base toolset toolsearch wraps. In a real agent it would typically
/// be one or more MCP toolsets with many tools.
struct Catalog(Vec<Arc<dyn Tool>>);

#[async_trait]
impl Toolset for Catalog {
    fn name(&self) -> &str {
        "catalog"
    }

    async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        Ok(self.0.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The two numbers of an arithmetic tool.
#[derive(Deserialize, JsonSchema)]
struct Numbers {
    /// The first number.
    a: f64,
    /// The second number.
    b: f64,
}

/// The result of an arithmetic tool.
#[derive(Serialize, JsonSchema)]
struct NumberResult {
    value: f64,
}

/// The input of `convert_temperature`.
#[derive(Deserialize, JsonSchema)]
struct Temperature {
    /// The temperature to convert.
    value: f64,
    /// The unit to convert from: celsius or fahrenheit.
    from: String,
}

/// The converted temperature and its unit.
#[derive(Serialize, JsonSchema)]
struct TemperatureResult {
    value: f64,
    unit: String,
}

/// The input of the text tools.
#[derive(Deserialize, JsonSchema)]
struct Text {
    /// The text to process.
    text: String,
}

/// The result of a text tool; each field is omitted when empty or zero.
#[derive(Serialize, JsonSchema)]
struct TextResult {
    #[serde(skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(skip_serializing_if = "is_zero")]
    count: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The input of a tool that takes no arguments.
#[derive(Deserialize, JsonSchema)]
struct Empty {}

/// The result of `get_current_time`.
#[derive(Serialize, JsonSchema)]
struct TimeResult {
    time: String,
}

async fn get_current_time(_ctx: Context, _args: Empty) -> Result<TimeResult, ToolError> {
    // RFC 3339 in UTC: the platform clock is UTC.
    let now = gadk::platform::Platform::default().now();
    Ok(TimeResult {
        time: now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

async fn add_numbers(_ctx: Context, n: Numbers) -> Result<NumberResult, ToolError> {
    Ok(NumberResult { value: n.a + n.b })
}

async fn multiply_numbers(_ctx: Context, n: Numbers) -> Result<NumberResult, ToolError> {
    Ok(NumberResult { value: n.a * n.b })
}

async fn convert_temperature(
    _ctx: Context,
    t: Temperature,
) -> Result<TemperatureResult, ToolError> {
    if t.from.eq_ignore_ascii_case("fahrenheit") {
        return Ok(TemperatureResult {
            value: (t.value - 32.0) * 5.0 / 9.0,
            unit: "celsius".to_string(),
        });
    }
    Ok(TemperatureResult {
        value: t.value * 9.0 / 5.0 + 32.0,
        unit: "fahrenheit".to_string(),
    })
}

async fn count_words(_ctx: Context, t: Text) -> Result<TextResult, ToolError> {
    Ok(TextResult {
        text: String::new(),
        count: t.text.split_whitespace().count(),
    })
}

async fn reverse_text(_ctx: Context, t: Text) -> Result<TextResult, ToolError> {
    Ok(TextResult {
        text: t.text.chars().rev().collect(),
        count: 0,
    })
}

/// The six function tools of the catalog.
fn catalog_tools() -> Result<Vec<Arc<dyn Tool>>, ToolError> {
    Ok(vec![
        FunctionTool::new(
            FunctionToolConfig::new(CORE_TOOL, "Return the current time in RFC 3339 format."),
            get_current_time,
        )?,
        FunctionTool::new(
            FunctionToolConfig::new("add_numbers", "Add two numbers and return their sum."),
            add_numbers,
        )?,
        FunctionTool::new(
            FunctionToolConfig::new(
                "multiply_numbers",
                "Multiply two numbers and return their product.",
            ),
            multiply_numbers,
        )?,
        FunctionTool::new(
            FunctionToolConfig::new(
                "convert_temperature",
                "Convert a temperature between Celsius and Fahrenheit.",
            ),
            convert_temperature,
        )?,
        FunctionTool::new(
            FunctionToolConfig::new("count_words", "Count the words in a text."),
            count_words,
        )?,
        FunctionTool::new(
            FunctionToolConfig::new("reverse_text", "Reverse the characters of a text."),
            reverse_text,
        )?,
    ])
}
