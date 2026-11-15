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

//! Go `internal/configurable/configurable_utils.go` `init()`: the built-in tool factories
//! (`exit_loop`, `google_search`, `url_context`, `google_maps_grounding`, `AgentTool`,
//! `LongRunningFunctionTool`, `ExampleTool`) and the `McpToolset` toolset factory. Go's
//! anonymous factory closures are named after their registered names.
//!
//! Go's type assertions on the yaml-decoded `map[string]any` args map onto [`GoValue`]
//! variants: `.(map[string]any)` is [`GoValue::Map`] (a map with a non-string key decodes as
//! `map[interface{}]interface{}` and fails the assertion), `.([]any)` is [`GoValue::Seq`],
//! `.(string)` is [`GoValue::String`] or [`GoValue::Bytes`], `.(bool)` is [`GoValue::Bool`].
//! Indexing a nil map or a missing key yields Go's nil, which fails every assertion.

use std::sync::Arc;

use futures::FutureExt;
use gadk_agent::{Tool, Toolset};
use gadk_engine::tool::{AgentTool, AgentToolConfig};
use gadk_genai::Content;
use gadk_goyaml::GoValue;
use gadk_mcp::{CommandBuilder, McpTransport};
use gadk_tools::example::{self, Example, ExampleToolConfig};
use serde::Deserialize;

use crate::govalue::{go_sprint, go_type_name, go_value_to_json};
use crate::{ConfigError, GoMap, Registry, ToolCtx, ToolEntry};

/// The built-in factories in Go's `init()` registration order, for [`Registry::new`].
pub(crate) fn builtin_tool_factories() -> Vec<(&'static str, ToolEntry)> {
    // Go: configurable_utils.go init (tool and toolset factories)
    vec![
        (
            "exit_loop",
            ToolEntry::Tool(Arc::new(|r, c, a| exit_loop(r, c, a).boxed())),
        ),
        (
            "google_search",
            ToolEntry::Tool(Arc::new(|r, c, a| google_search(r, c, a).boxed())),
        ),
        (
            "url_context",
            ToolEntry::Tool(Arc::new(|r, c, a| url_context(r, c, a).boxed())),
        ),
        (
            "google_maps_grounding",
            ToolEntry::Tool(Arc::new(|r, c, a| google_maps_grounding(r, c, a).boxed())),
        ),
        (
            "AgentTool",
            ToolEntry::Tool(Arc::new(|r, c, a| agent_tool(r, c, a).boxed())),
        ),
        (
            "LongRunningFunctionTool",
            ToolEntry::Tool(Arc::new(|r, c, a| {
                long_running_function_tool(r, c, a).boxed()
            })),
        ),
        (
            "ExampleTool",
            ToolEntry::Tool(Arc::new(|r, c, a| example_tool(r, c, a).boxed())),
        ),
        (
            "McpToolset",
            ToolEntry::Toolset(Arc::new(|r, c, a| mcp_toolset(r, c, a).boxed())),
        ),
    ]
}

/// Go `exit_loop`: `exitlooptool.New()`.
pub(crate) async fn exit_loop(
    _registry: Registry,
    _ctx: ToolCtx,
    _args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (exit_loop)
    Ok(gadk_engine::tool::exit_loop())
}

/// Go `google_search`: `geminitool.GoogleSearch{}`.
pub(crate) async fn google_search(
    _registry: Registry,
    _ctx: ToolCtx,
    _args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (google_search)
    Ok(Arc::new(gadk_tools::gemini::GoogleSearch))
}

/// Go `url_context`: `geminitool.New("url_context", "url context", &genai.Tool{URLContext:
/// &genai.URLContext{}})`.
pub(crate) async fn url_context(
    _registry: Registry,
    _ctx: ToolCtx,
    _args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (url_context)
    Ok(gadk_tools::gemini::new(
        "url_context",
        "url context",
        gadk_genai::Tool {
            url_context: Some(gadk_genai::URLContext {}),
            ..Default::default()
        },
    ))
}

/// Go `google_maps_grounding`: `geminitool.New("google_maps_grounding", "google maps
/// grounding", &genai.Tool{GoogleMaps: &genai.GoogleMaps{}})`.
pub(crate) async fn google_maps_grounding(
    _registry: Registry,
    _ctx: ToolCtx,
    _args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (google_maps_grounding)
    Ok(gadk_tools::gemini::new(
        "google_maps_grounding",
        "google maps grounding",
        gadk_genai::Tool {
            google_maps: Some(gadk_genai::GoogleMaps::default()),
            ..Default::default()
        },
    ))
}

/// Go `AgentTool`: `args is nil`, `agent not found in args`, `parentPath not found in context`,
/// `config_path not found in args`; resolves the agent inside the context's config directory
/// (Go's `parentPath` value) and wraps it in `agenttool.New` with `skip_summarization`.
pub(crate) async fn agent_tool(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (AgentTool)
    let Some(args) = args else {
        return Err(ConfigError::msg("args is nil"));
    };
    let Some(GoValue::Map(a)) = args.get("agent") else {
        return Err(ConfigError::msg("agent not found in args"));
    };
    let skip_summarization = skip_summarization(a);
    let Some(config_dir) = ctx.parent_path.clone() else {
        return Err(ConfigError::msg("parentPath not found in context"));
    };
    let Some(config_path) = go_string(map_get(a, "config_path")) else {
        return Err(ConfigError::msg("config_path not found in args"));
    };
    let ag = registry
        .resolve_agent_reference_with_ctx(&ctx, &config_dir, &config_path)
        .await?;
    Ok(AgentTool::new(
        ag,
        Some(AgentToolConfig { skip_summarization }),
    ))
}

/// Go `AgentTool`'s `a["skip_summarization"].(bool)`, `false` unless it is a bool.
fn skip_summarization(a: &[(String, GoValue)]) -> bool {
    matches!(map_get(a, "skip_summarization"), Some(GoValue::Bool(true)))
}

/// Go `LongRunningFunctionTool`: `args is nil`, `func not found in args`, resolves `func` with
/// the same args, `tool '%s' not found` when it names a toolset.
pub(crate) async fn long_running_function_tool(
    registry: Registry,
    ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (LongRunningFunctionTool)
    let Some(map) = &args else {
        return Err(ConfigError::msg("args is nil"));
    };
    let Some(func_name) = go_string(map.get("func")) else {
        return Err(ConfigError::msg("func not found in args"));
    };
    let (tool, _) = registry
        .resolve_tool_reference(&ctx, &func_name, args.as_ref())
        .await?;
    match tool {
        Some(tool) => Ok(tool),
        None => Err(ConfigError::msg(format!("tool '{func_name}' not found"))),
    }
}

/// Go `ExampleTool`: `args is nil`, `examples not found in args`, `examples is not a list`;
/// wraps a non-list `output` in a list, round-trips through JSON
/// (`failed to decode normalized examples: %w`) into `exampletool.Example`s.
pub(crate) async fn example_tool(
    _registry: Registry,
    _ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Tool>, ConfigError> {
    // Go: configurable_utils.go init (ExampleTool)
    let Some(args) = args else {
        return Err(ConfigError::msg("args is nil"));
    };
    let Some(raw) = args.get("examples") else {
        return Err(ConfigError::msg("examples not found in args"));
    };
    let GoValue::Seq(examples_slice) = raw else {
        return Err(ConfigError::msg("examples is not a list"));
    };
    let normalized = GoValue::Seq(normalize_examples(examples_slice.clone()));
    let examples = decode_examples(&normalized)?;
    Ok(example::new(ExampleToolConfig { examples }))
}

/// Go `ExampleTool` step 2: every `map[string]any` item whose `output` is neither nil nor a
/// list gets `output: [output]`; other items are left as they are.
fn normalize_examples(mut items: Vec<GoValue>) -> Vec<GoValue> {
    for item in &mut items {
        let GoValue::Map(m) = item else {
            continue;
        };
        let Some((_, output)) = m.iter_mut().find(|(k, _)| k == "output") else {
            continue;
        };
        if matches!(output, GoValue::Null | GoValue::Seq(_)) {
            continue;
        }
        *output = GoValue::Seq(vec![std::mem::take(output)]);
    }
    items
}

/// Go `exampletool.Example` as `json.Unmarshal` fills a `[]*exampletool.Example`
/// (`json:"input"`, `json:"output"`): a JSON `null` element is a nil pointer.
#[derive(Deserialize)]
struct ExampleJson {
    #[serde(default)]
    input: Option<Content>,
    #[serde(default)]
    output: Option<Vec<Option<Content>>>,
}

/// Go `ExampleTool` step 3: `bytes, _ := json.Marshal(examplesSlice)` then
/// `json.Unmarshal(bytes, &examples)`.
fn decode_examples(normalized: &GoValue) -> Result<Vec<Example>, ConfigError> {
    let wrap = |e: ConfigError| ConfigError::wrap("failed to decode normalized examples", e);
    // Go ignores json.Marshal's error: the nil bytes then fail to unmarshal.
    let Ok(json) = go_value_to_json(normalized) else {
        return Err(wrap(ConfigError::msg("unexpected end of JSON input")));
    };
    // Divergence (ledger 6c-2): decoding uses gadk-genai's serde, so keys match exactly (Go's
    // encoding/json also accepts case-insensitive keys) and a type error has serde_json's text
    // instead of Go's `json: cannot unmarshal …`.
    // Divergence (ledger 6c-2): an explicit null for a non-pointer field inside an example's
    // content (`role: ~`, `parts: ~`, a part's `text: ~`) is a no-op in Go's encoding/json but
    // a serde type error here (nulls are not stripped: free-form maps such as
    // `FunctionCall.args` keep them).
    let decoded: Vec<Option<ExampleJson>> =
        serde_json::from_value(json).map_err(|e| wrap(ConfigError::other(e)))?;
    // Divergence (ledger 6c-2): a null example or null output element is Go's nil pointer, which
    // panics later in `buildExamplesSystemInstruction`; gadk's `Example` holds values, so it
    // becomes an empty example or content.
    Ok(decoded
        .into_iter()
        .map(|e| {
            let e = e.unwrap_or(ExampleJson {
                input: None,
                output: None,
            });
            Example {
                input: e.input.unwrap_or_default(),
                output: e
                    .output
                    .unwrap_or_default()
                    .into_iter()
                    .map(Option::unwrap_or_default)
                    .collect(),
            }
        })
        .collect())
}

/// Go `McpToolset`: `stdio_connection_params not found in args`,
/// `server_params not found in stdio_connection_params`, `command not found in server_params`,
/// `args not found in server_params`, `tool_filter not found in args`,
/// `server_params.args[%d]: expected string, got %T (%v)`,
/// `tool_filter[%d]: expected string, got %T (%v)`; a command transport with a string
/// predicate filter (`failed to create mcp toolset: %v`).
pub(crate) async fn mcp_toolset(
    _registry: Registry,
    _ctx: ToolCtx,
    args: Option<GoMap>,
) -> Result<Arc<dyn Toolset>, ConfigError> {
    // Go: configurable_utils.go init (McpToolset)
    // Go indexes the args map without a nil check: a nil map reads nil.
    let args = args.unwrap_or_default();
    let Some(GoValue::Map(stdio_connection_params)) = args.get("stdio_connection_params") else {
        return Err(ConfigError::msg(
            "stdio_connection_params not found in args",
        ));
    };
    let Some(GoValue::Map(server_params)) = map_get(stdio_connection_params, "server_params")
    else {
        return Err(ConfigError::msg(
            "server_params not found in stdio_connection_params",
        ));
    };
    let Some(command) = go_string(map_get(server_params, "command")) else {
        return Err(ConfigError::msg("command not found in server_params"));
    };
    let Some(GoValue::Seq(server_args)) = map_get(server_params, "args") else {
        return Err(ConfigError::msg("args not found in server_params"));
    };
    let Some(GoValue::Seq(tool_filter)) = args.get("tool_filter") else {
        return Err(ConfigError::msg("tool_filter not found in args"));
    };
    let server_args_str = string_elements(server_args, "server_params.args")?;
    let tool_filter_str = string_elements(tool_filter, "tool_filter")?;

    let mut cfg = gadk_mcp::Config::new(McpTransport::Command(command_builder(
        command,
        server_args_str,
    )));
    let filter: Vec<&str> = tool_filter_str.iter().map(String::as_str).collect();
    cfg.tool_filter = Some(gadk_agent::string_predicate(&filter));
    let mcp_set = gadk_mcp::new(cfg)
        .map_err(|e| ConfigError::msg(format!("failed to create mcp toolset: {e}")))?;
    Ok(Arc::new(mcp_set))
}

/// Go `McpToolset`'s element loops: each element must be a Go string, else
/// `<what>[%d]: expected string, got %T (%v)`.
fn string_elements(items: &[GoValue], what: &str) -> Result<Vec<String>, ConfigError> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            go_string(Some(item)).ok_or_else(|| {
                ConfigError::msg(format!(
                    "{what}[{i}]: expected string, got {} ({})",
                    go_type_name(item),
                    go_sprint(item)
                ))
            })
        })
        .collect()
}

/// Go `&mcp.CommandTransport{Command: exec.Command(command, serverArgsStr...)}`: gadk-mcp
/// builds a fresh command for every connect.
fn command_builder(command: String, args: Vec<String>) -> CommandBuilder {
    Arc::new(move || {
        let mut cmd = tokio::process::Command::new(&command);
        cmd.args(&args);
        cmd
    })
}

/// Go `m[key]` of a yaml-decoded `map[string]any` (`None` = Go's nil for a missing key).
fn map_get<'a>(m: &'a [(String, GoValue)], key: &str) -> Option<&'a GoValue> {
    m.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Go `v.(string)`: a yaml-decoded Go string.
fn go_string(v: Option<&GoValue>) -> Option<String> {
    match v? {
        GoValue::String(s) => Some(s.clone()),
        // Divergence (ledger 6c-2): a non-UTF-8 Go string (only from `!!binary`) is used with
        // each invalid byte replaced by U+FFFD; Go passes the raw bytes on.
        GoValue::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use gadk_agent::{AgentConfig, Context, new_agent};
    use gadk_engine::tool::AgentTool;
    use gadk_genai::{Content, FunctionCall, Part};
    use gadk_model::LlmRequest;
    use gadk_tools::example::{self, Example, ExampleToolConfig};

    use super::*;
    use crate::{AgentFactory, LoadCtx};

    fn s(v: &str) -> GoValue {
        GoValue::String(v.to_owned())
    }

    fn m(entries: &[(&str, GoValue)]) -> GoValue {
        GoValue::Map(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    fn args(entries: &[(&str, GoValue)]) -> Option<GoMap> {
        Some(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        )
    }

    fn parent_ctx() -> ToolCtx {
        LoadCtx::default().with_parent_path(std::path::Path::new("/agents"))
    }

    async fn tool_err(
        f: impl std::future::Future<Output = Result<Arc<dyn Tool>, ConfigError>>,
    ) -> String {
        match f.await {
            Ok(t) => panic!("got tool {}, want an error", t.name()),
            Err(e) => e.to_string(),
        }
    }

    async fn toolset_err(
        f: impl std::future::Future<Output = Result<Arc<dyn Toolset>, ConfigError>>,
    ) -> String {
        match f.await {
            Ok(t) => panic!("got toolset {}, want an error", t.name()),
            Err(e) => e.to_string(),
        }
    }

    // ---- builtin_tool_factories ----

    #[test]
    fn builtin_tool_factories_are_go_init_order_and_kinds() {
        let got: Vec<(&str, &str)> = builtin_tool_factories()
            .iter()
            .map(|(name, entry)| {
                let kind = match entry {
                    ToolEntry::Tool(_) => "tool",
                    ToolEntry::Toolset(_) => "toolset",
                };
                (*name, kind)
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("exit_loop", "tool"),
                ("google_search", "tool"),
                ("url_context", "tool"),
                ("google_maps_grounding", "tool"),
                ("AgentTool", "tool"),
                ("LongRunningFunctionTool", "tool"),
                ("ExampleTool", "tool"),
                ("McpToolset", "toolset"),
            ]
        );
    }

    #[tokio::test]
    async fn builtin_tool_factory_entries_call_their_factories() {
        let registry = Registry::empty();
        let mut built = Vec::new();
        for (name, entry) in builtin_tool_factories() {
            let got = match entry {
                ToolEntry::Tool(f) => match f(registry.clone(), LoadCtx::default(), None).await {
                    Ok(t) => t.name().to_owned(),
                    Err(e) => e.to_string(),
                },
                ToolEntry::Toolset(f) => {
                    match f(registry.clone(), LoadCtx::default(), None).await {
                        Ok(t) => t.name().to_owned(),
                        Err(e) => e.to_string(),
                    }
                }
            };
            built.push((name, got));
        }
        assert_eq!(
            built,
            vec![
                ("exit_loop", "exit_loop".to_owned()),
                ("google_search", "google_search".to_owned()),
                ("url_context", "url_context".to_owned()),
                ("google_maps_grounding", "google_maps_grounding".to_owned()),
                ("AgentTool", "args is nil".to_owned()),
                ("LongRunningFunctionTool", "args is nil".to_owned()),
                ("ExampleTool", "args is nil".to_owned()),
                (
                    "McpToolset",
                    "stdio_connection_params not found in args".to_owned()
                ),
            ]
        );
    }

    // ---- exit_loop, google_search, url_context, google_maps_grounding ----

    #[tokio::test]
    async fn exit_loop_is_the_engine_exit_loop_tool_and_ignores_args() {
        let t = exit_loop(
            Registry::empty(),
            LoadCtx::default(),
            args(&[("x", s("y"))]),
        )
        .await
        .unwrap();
        assert_eq!(t.name(), "exit_loop");
        assert!(t.as_runnable().is_some(), "exit_loop must be runnable");
    }

    #[tokio::test]
    async fn google_search_is_the_gemini_google_search_tool() {
        let t = google_search(Registry::empty(), LoadCtx::default(), None)
            .await
            .unwrap();
        assert!(t.as_any().is::<gadk_tools::gemini::GoogleSearch>());
    }

    /// The `genai::Tool` a Gemini built-in tool adds to a request.
    async fn added_genai_tool(t: &Arc<dyn Tool>) -> gadk_genai::Tool {
        let ctx = Context::builder().build();
        let mut req = LlmRequest {
            model: "gemini-2.5-flash".into(),
            ..Default::default()
        };
        t.process_request(&ctx, &mut req).await.unwrap();
        let tools = req.config.unwrap().tools;
        assert_eq!(tools.len(), 1, "{tools:?}");
        tools.into_iter().next().unwrap()
    }

    #[tokio::test]
    async fn url_context_is_a_gemini_tool_with_url_context() {
        let t = url_context(Registry::empty(), LoadCtx::default(), None)
            .await
            .unwrap();
        assert_eq!(t.name(), "url_context");
        assert_eq!(t.description(), "url context");
        assert_eq!(
            added_genai_tool(&t).await,
            gadk_genai::Tool {
                url_context: Some(gadk_genai::URLContext {}),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn google_maps_grounding_is_a_gemini_tool_with_google_maps() {
        let t = google_maps_grounding(Registry::empty(), LoadCtx::default(), None)
            .await
            .unwrap();
        assert_eq!(t.name(), "google_maps_grounding");
        assert_eq!(t.description(), "google maps grounding");
        assert_eq!(
            added_genai_tool(&t).await,
            gadk_genai::Tool {
                google_maps: Some(gadk_genai::GoogleMaps::default()),
                ..Default::default()
            }
        );
    }

    // ---- AgentTool ----

    #[tokio::test]
    async fn agent_tool_rejects_nil_args() {
        let got = tool_err(agent_tool(Registry::empty(), parent_ctx(), None)).await;
        assert_eq!(got, "args is nil");
    }

    #[tokio::test]
    async fn agent_tool_requires_an_agent_map() {
        for (name, a) in [
            ("empty args", args(&[])),
            ("agent is a string", args(&[("agent", s("sub.yaml"))])),
            ("agent is null", args(&[("agent", GoValue::Null)])),
            (
                "agent has non-string keys",
                args(&[("agent", GoValue::MapAny(vec![(GoValue::Int(1), s("x"))]))]),
            ),
        ] {
            let got = tool_err(agent_tool(Registry::empty(), parent_ctx(), a)).await;
            assert_eq!(got, "agent not found in args", "{name}");
        }
    }

    #[tokio::test]
    async fn agent_tool_requires_a_parent_path_before_config_path() {
        for (name, agent) in [
            ("with config_path", m(&[("config_path", s("sub.yaml"))])),
            ("without config_path", m(&[])),
        ] {
            let got = tool_err(agent_tool(
                Registry::empty(),
                LoadCtx::default(),
                args(&[("agent", agent)]),
            ))
            .await;
            assert_eq!(got, "parentPath not found in context", "{name}");
        }
    }

    #[tokio::test]
    async fn agent_tool_requires_a_string_config_path() {
        for (name, agent) in [
            ("absent", m(&[("skip_summarization", GoValue::Bool(true))])),
            ("int", m(&[("config_path", GoValue::Int(1))])),
            ("null", m(&[("config_path", GoValue::Null)])),
        ] {
            let got = tool_err(agent_tool(
                Registry::empty(),
                parent_ctx(),
                args(&[("agent", agent)]),
            ))
            .await;
            assert_eq!(got, "config_path not found in args", "{name}");
        }
    }

    #[test]
    fn agent_tool_skip_summarization_is_a_bool_or_false() {
        let cases = [
            (vec![], false),
            (vec![("skip_summarization", GoValue::Bool(true))], true),
            (vec![("skip_summarization", GoValue::Bool(false))], false),
            (vec![("skip_summarization", s("true"))], false),
            (vec![("skip_summarization", GoValue::Int(1))], false),
        ];
        for (entries, want) in cases {
            let GoValue::Map(a) = m(&entries) else {
                unreachable!()
            };
            assert_eq!(skip_summarization(&a), want, "{entries:?}");
        }
    }

    /// A registry whose `Fake` agent class builds a plain agent named after the config's `name`.
    fn registry_with_fake_agent_class() -> Registry {
        let registry = Registry::empty();
        let factory: AgentFactory = Arc::new(|_r, _c, data, _p| {
            async move {
                let cfg: crate::config::BaseAgentConfig = crate::config::unmarshal(&data)?;
                new_agent(AgentConfig {
                    name: cfg.name,
                    ..Default::default()
                })
                .map_err(ConfigError::other)
            }
            .boxed()
        });
        registry
            .inner
            .write()
            .agent_classes
            .insert("Fake".to_owned(), factory);
        registry
    }

    #[tokio::test]
    async fn agent_tool_wraps_the_referenced_agent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(
            dir.path().join("sub.yaml"),
            "agent_class: Fake\nname: helper\n",
        )
        .unwrap();
        let ctx = LoadCtx::default().with_parent_path(&root);
        let t = agent_tool(
            registry_with_fake_agent_class(),
            ctx,
            args(&[(
                "agent",
                m(&[
                    ("config_path", s("sub.yaml")),
                    ("skip_summarization", GoValue::Bool(true)),
                ]),
            )]),
        )
        .await
        .unwrap();
        assert!(t.as_any().is::<AgentTool>());
        assert_eq!(t.name(), "helper");
    }

    #[tokio::test]
    async fn agent_tool_returns_the_resolve_error_unwrapped() {
        let got = tool_err(agent_tool(
            registry_with_fake_agent_class(),
            parent_ctx(),
            args(&[("agent", m(&[("config_path", s("/abs/sub.yaml"))]))]),
        ))
        .await;
        assert_eq!(
            got,
            "config reference \"/abs/sub.yaml\": path is not a relative path inside the base \
             directory: rel path is not local"
        );
    }

    // ---- LongRunningFunctionTool ----

    #[tokio::test]
    async fn long_running_function_tool_rejects_nil_args() {
        let got = tool_err(long_running_function_tool(
            Registry::empty(),
            LoadCtx::default(),
            None,
        ))
        .await;
        assert_eq!(got, "args is nil");
    }

    #[tokio::test]
    async fn long_running_function_tool_requires_a_string_func() {
        for (name, a) in [
            ("empty args", args(&[])),
            ("func is an int", args(&[("func", GoValue::Int(1))])),
            ("func is null", args(&[("func", GoValue::Null)])),
        ] {
            let got = tool_err(long_running_function_tool(
                Registry::empty(),
                LoadCtx::default(),
                a,
            ))
            .await;
            assert_eq!(got, "func not found in args", "{name}");
        }
    }

    /// A registry with tool `my_tool` (records the args it was built with) and toolset
    /// `my_toolset`.
    fn registry_with_function_tools(seen: Arc<parking_lot::Mutex<Option<GoMap>>>) -> Registry {
        let registry = Registry::empty();
        let tool: crate::ToolFactory = Arc::new(move |_r, _c, a| {
            *seen.lock() = a;
            async { Ok(gadk_engine::tool::exit_loop()) }.boxed()
        });
        let toolset: crate::ToolsetFactory = Arc::new(|_r, _c, a| {
            async move { mcp_toolset(Registry::empty(), LoadCtx::default(), a).await }.boxed()
        });
        let mut inner = registry.inner.write();
        inner
            .tools
            .insert("my_tool".to_owned(), ToolEntry::Tool(tool));
        inner
            .tools
            .insert("my_toolset".to_owned(), ToolEntry::Toolset(toolset));
        drop(inner);
        registry
    }

    #[tokio::test]
    async fn long_running_function_tool_resolves_func_with_the_same_args() {
        let seen = Arc::new(parking_lot::Mutex::new(None));
        let a = args(&[("func", s("my_tool")), ("extra", GoValue::Int(7))]);
        let t = long_running_function_tool(
            registry_with_function_tools(seen.clone()),
            LoadCtx::default(),
            a.clone(),
        )
        .await
        .unwrap();
        assert_eq!(t.name(), "exit_loop");
        assert_eq!(*seen.lock(), a);
    }

    #[tokio::test]
    async fn long_running_function_tool_rejects_toolsets_and_unknown_names() {
        let mcp_args = |func: &str| {
            args(&[
                ("func", s(func)),
                (
                    "stdio_connection_params",
                    m(&[(
                        "server_params",
                        m(&[("command", s("echo")), ("args", GoValue::Seq(vec![]))]),
                    )]),
                ),
                ("tool_filter", GoValue::Seq(vec![])),
            ])
        };
        let seen = Arc::new(parking_lot::Mutex::new(None));
        let registry = registry_with_function_tools(seen);
        let got = tool_err(long_running_function_tool(
            registry.clone(),
            LoadCtx::default(),
            mcp_args("my_toolset"),
        ))
        .await;
        assert_eq!(got, "tool 'my_toolset' not found");
        let got = tool_err(long_running_function_tool(
            registry,
            LoadCtx::default(),
            mcp_args("missing"),
        ))
        .await;
        assert_eq!(got, "tool 'missing' not found");
    }

    // ---- ExampleTool ----

    fn text_content(role: &str, text: &str) -> Content {
        Content {
            role: role.to_owned(),
            parts: vec![Part {
                text: text.to_owned(),
                ..Default::default()
            }],
        }
    }

    fn yaml_content(role: &str, text: &str) -> GoValue {
        m(&[
            ("role", s(role)),
            ("parts", GoValue::Seq(vec![m(&[("text", s(text))])])),
        ])
    }

    #[tokio::test]
    async fn example_tool_rejects_nil_args() {
        let got = tool_err(example_tool(Registry::empty(), LoadCtx::default(), None)).await;
        assert_eq!(got, "args is nil");
    }

    #[tokio::test]
    async fn example_tool_requires_examples() {
        let got = tool_err(example_tool(
            Registry::empty(),
            LoadCtx::default(),
            args(&[]),
        ))
        .await;
        assert_eq!(got, "examples not found in args");
    }

    #[tokio::test]
    async fn example_tool_requires_a_list() {
        for (name, v) in [
            ("null", GoValue::Null),
            ("string", s("x")),
            ("map", m(&[("input", s("x"))])),
        ] {
            let got = tool_err(example_tool(
                Registry::empty(),
                LoadCtx::default(),
                args(&[("examples", v)]),
            ))
            .await;
            assert_eq!(got, "examples is not a list", "{name}");
        }
    }

    #[test]
    fn normalize_examples_wraps_a_non_list_output() {
        let items = vec![
            m(&[("input", s("a")), ("output", yaml_content("model", "x"))]),
            m(&[("output", GoValue::Seq(vec![s("kept")]))]),
            m(&[("output", GoValue::Null)]),
            m(&[("input", s("no output"))]),
            m(&[("output", s("scalar"))]),
            s("not a map"),
            GoValue::MapAny(vec![(s("output"), s("untouched"))]),
        ];
        let want = vec![
            m(&[
                ("input", s("a")),
                ("output", GoValue::Seq(vec![yaml_content("model", "x")])),
            ]),
            m(&[("output", GoValue::Seq(vec![s("kept")]))]),
            m(&[("output", GoValue::Null)]),
            m(&[("input", s("no output"))]),
            m(&[("output", GoValue::Seq(vec![s("scalar")]))]),
            s("not a map"),
            GoValue::MapAny(vec![(s("output"), s("untouched"))]),
        ];
        assert_eq!(normalize_examples(items), want);
    }

    #[test]
    fn decode_examples_round_trips_through_json() {
        let items = vec![
            m(&[
                ("input", yaml_content("user", "hi")),
                (
                    "output",
                    GoValue::Seq(vec![
                        yaml_content("model", "hello"),
                        m(&[(
                            "parts",
                            GoValue::Seq(vec![m(&[(
                                "functionCall",
                                m(&[("name", s("f")), ("args", m(&[("n", GoValue::Int(1))]))]),
                            )])]),
                        )]),
                    ]),
                ),
            ]),
            m(&[]),
            GoValue::Null,
            m(&[("output", GoValue::Seq(vec![GoValue::Null]))]),
        ];
        let got = decode_examples(&GoValue::Seq(items)).unwrap();
        let mut fc_args = serde_json::Map::new();
        fc_args.insert("n".to_owned(), serde_json::json!(1));
        let want = vec![
            Example {
                input: text_content("user", "hi"),
                output: vec![
                    text_content("model", "hello"),
                    Content {
                        role: String::new(),
                        parts: vec![Part {
                            function_call: Some(FunctionCall {
                                name: "f".to_owned(),
                                args: fc_args,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }],
                    },
                ],
            },
            Example::default(),
            Example::default(),
            Example {
                input: Content::default(),
                output: vec![Content::default()],
            },
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn decode_examples_reports_a_marshal_failure_as_go_does() {
        for (name, v) in [
            (
                "non-string map keys",
                GoValue::MapAny(vec![(GoValue::Int(1), s("x"))]),
            ),
            ("NaN", m(&[("input", GoValue::Float(f64::NAN))])),
        ] {
            let got = decode_examples(&GoValue::Seq(vec![v]))
                .unwrap_err()
                .to_string();
            assert_eq!(
                got, "failed to decode normalized examples: unexpected end of JSON input",
                "{name}"
            );
        }
    }

    #[test]
    fn decode_examples_wraps_a_decode_failure() {
        for (name, v) in [
            ("string example", s("not a map")),
            ("string input", m(&[("input", s("x"))])),
        ] {
            let got = decode_examples(&GoValue::Seq(vec![v]))
                .unwrap_err()
                .to_string();
            assert!(
                got.starts_with("failed to decode normalized examples: "),
                "{name}: {got}"
            );
        }
    }

    #[tokio::test]
    async fn example_tool_builds_the_example_tool_from_normalized_examples() {
        let t = example_tool(
            Registry::empty(),
            LoadCtx::default(),
            args(&[(
                "examples",
                GoValue::Seq(vec![m(&[
                    ("input", yaml_content("user", "ping")),
                    ("output", yaml_content("model", "pong")),
                ])]),
            )]),
        )
        .await
        .unwrap();
        assert_eq!(t.name(), "example_tool");
        let want = example::new(ExampleToolConfig {
            examples: vec![Example {
                input: text_content("user", "ping"),
                output: vec![text_content("model", "pong")],
            }],
        });
        let ctx = Context::builder()
            .user_content(text_content("user", "question"))
            .build();
        let mut got_req = LlmRequest::default();
        t.process_request(&ctx, &mut got_req).await.unwrap();
        let mut want_req = LlmRequest::default();
        want.process_request(&ctx, &mut want_req).await.unwrap();
        assert!(want_req.config.is_some());
        assert_eq!(got_req.config, want_req.config);
    }

    #[tokio::test]
    async fn example_tool_accepts_an_empty_list() {
        let t = example_tool(
            Registry::empty(),
            LoadCtx::default(),
            args(&[("examples", GoValue::Seq(vec![]))]),
        )
        .await
        .unwrap();
        assert_eq!(t.name(), "example_tool");
    }

    // ---- McpToolset ----

    fn mcp_args(server_args: GoValue, tool_filter: GoValue) -> Option<GoMap> {
        args(&[
            (
                "stdio_connection_params",
                m(&[(
                    "server_params",
                    m(&[("command", s("echo")), ("args", server_args)]),
                )]),
            ),
            ("tool_filter", tool_filter),
        ])
    }

    #[tokio::test]
    async fn mcp_toolset_requires_each_field_in_go_order() {
        let server = |entries: &[(&str, GoValue)]| m(&[("server_params", m(entries))]);
        let cases = [
            (
                "nil args",
                None,
                "stdio_connection_params not found in args",
            ),
            (
                "stdio_connection_params not a map",
                args(&[("stdio_connection_params", s("x"))]),
                "stdio_connection_params not found in args",
            ),
            (
                "server_params missing",
                args(&[("stdio_connection_params", m(&[]))]),
                "server_params not found in stdio_connection_params",
            ),
            (
                "server_params a list",
                args(&[(
                    "stdio_connection_params",
                    m(&[("server_params", GoValue::Seq(vec![]))]),
                )]),
                "server_params not found in stdio_connection_params",
            ),
            (
                "command missing",
                args(&[("stdio_connection_params", server(&[]))]),
                "command not found in server_params",
            ),
            (
                "command an int",
                args(&[(
                    "stdio_connection_params",
                    server(&[("command", GoValue::Int(1))]),
                )]),
                "command not found in server_params",
            ),
            (
                "args missing",
                args(&[("stdio_connection_params", server(&[("command", s("echo"))]))]),
                "args not found in server_params",
            ),
            (
                "args a string",
                args(&[(
                    "stdio_connection_params",
                    server(&[("command", s("echo")), ("args", s("a b"))]),
                )]),
                "args not found in server_params",
            ),
            (
                "tool_filter missing (checked before the args elements)",
                args(&[(
                    "stdio_connection_params",
                    server(&[
                        ("command", s("echo")),
                        ("args", GoValue::Seq(vec![GoValue::Int(1)])),
                    ]),
                )]),
                "tool_filter not found in args",
            ),
            (
                "tool_filter null",
                mcp_args(GoValue::Seq(vec![]), GoValue::Null),
                "tool_filter not found in args",
            ),
        ];
        for (name, a, want) in cases {
            let got = toolset_err(mcp_toolset(Registry::empty(), LoadCtx::default(), a)).await;
            assert_eq!(got, want, "{name}");
        }
    }

    #[tokio::test]
    async fn mcp_toolset_rejects_non_string_elements_with_go_type_and_value() {
        let cases = [
            (
                mcp_args(
                    GoValue::Seq(vec![s("a"), GoValue::Int(1), GoValue::Int(2)]),
                    GoValue::Seq(vec![s("a")]),
                ),
                "server_params.args[1]: expected string, got int (1)",
            ),
            (
                mcp_args(
                    GoValue::Seq(vec![GoValue::Float(1.5)]),
                    GoValue::Seq(vec![]),
                ),
                "server_params.args[0]: expected string, got float64 (1.5)",
            ),
            (
                mcp_args(GoValue::Seq(vec![GoValue::Null]), GoValue::Seq(vec![])),
                "server_params.args[0]: expected string, got <nil> (<nil>)",
            ),
            (
                mcp_args(
                    GoValue::Seq(vec![GoValue::Seq(vec![s("a"), s("b")])]),
                    GoValue::Seq(vec![]),
                ),
                "server_params.args[0]: expected string, got []interface {} ([a b])",
            ),
            (
                mcp_args(
                    GoValue::Seq(vec![m(&[("k", s("v"))])]),
                    GoValue::Seq(vec![]),
                ),
                "server_params.args[0]: expected string, got map[string]interface {} (map[k:v])",
            ),
            (
                mcp_args(
                    GoValue::Seq(vec![s("a")]),
                    GoValue::Seq(vec![GoValue::Bool(true)]),
                ),
                "tool_filter[0]: expected string, got bool (true)",
            ),
            (
                mcp_args(
                    GoValue::Seq(vec![GoValue::Int(1)]),
                    GoValue::Seq(vec![GoValue::Bool(true)]),
                ),
                "server_params.args[0]: expected string, got int (1)",
            ),
        ];
        for (a, want) in cases {
            let got = toolset_err(mcp_toolset(Registry::empty(), LoadCtx::default(), a)).await;
            assert_eq!(got, want);
        }
    }

    #[tokio::test]
    async fn mcp_toolset_with_string_args_is_valid_and_does_not_connect() {
        for filter in [GoValue::Seq(vec![s("t1")]), GoValue::Seq(vec![])] {
            let t = mcp_toolset(
                Registry::empty(),
                LoadCtx::default(),
                mcp_args(GoValue::Seq(vec![s("a"), s("b")]), filter),
            )
            .await
            .unwrap();
            assert_eq!(t.name(), "mcp_tool_set");
            assert!(t.as_any().is::<gadk_mcp::McpToolset>());
        }
    }

    #[test]
    fn mcp_command_builder_runs_the_command_with_the_args() {
        let build = command_builder(
            "my-server".to_owned(),
            vec!["--a".to_owned(), "b c".to_owned()],
        );
        for _ in 0..2 {
            let cmd = build();
            let std_cmd = cmd.as_std();
            assert_eq!(std_cmd.get_program(), "my-server");
            assert_eq!(std_cmd.get_args().collect::<Vec<_>>(), vec!["--a", "b c"]);
        }
    }

    #[tokio::test]
    async fn mcp_toolset_spawns_the_command_on_first_use() {
        let a = args(&[
            (
                "stdio_connection_params",
                m(&[(
                    "server_params",
                    m(&[
                        ("command", s("/nonexistent/gadk-configurable-mcp-server")),
                        ("args", GoValue::Seq(vec![])),
                    ]),
                )]),
            ),
            ("tool_filter", GoValue::Seq(vec![])),
        ]);
        let t = mcp_toolset(Registry::empty(), LoadCtx::default(), a)
            .await
            .unwrap();
        let ctx = Context::builder().invocation_id("inv-1").build();
        let err = match t.tools(&ctx).await {
            Ok(tools) => panic!("listed {} tools from a missing server", tools.len()),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("failed to init MCP session"), "{err}");
    }

    #[test]
    fn go_string_accepts_go_strings_only() {
        assert_eq!(go_string(Some(&s("x"))), Some("x".to_owned()));
        assert_eq!(
            go_string(Some(&GoValue::Bytes(vec![b'a', 0xff]))),
            Some("a\u{fffd}".to_owned())
        );
        assert_eq!(go_string(Some(&GoValue::Int(1))), None);
        assert_eq!(go_string(Some(&GoValue::Null)), None);
        assert_eq!(go_string(None), None);
    }
}
