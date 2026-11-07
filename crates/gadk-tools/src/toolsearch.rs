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

//! A gating toolset that hides all but a small core set of tools from the model until it calls
//! `search_tools` to discover them (port of Go `tool/toolsearch`).
//!
//! Gating decides which tools the model sees, not which tools a caller may use. Pass a base
//! toolset that is already filtered for the caller, since `search_tools` can reveal any tool in it
//! that the model can call.

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{Context, ReadonlyContext, Tool, ToolError, Toolset, go_quote};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use gadk_genai::FunctionDeclaration;
use gadk_model::LlmRequest;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};

use crate::go_fmt::url_query_escape;
use crate::ranksearch;

/// The name of the search tool exposed to the model (Go `ToolName`).
pub const TOOL_NAME: &str = "search_tools";

/// Go `defaultMaxResults`.
const DEFAULT_MAX_RESULTS: i32 = 8;
/// Go `stateKeyPrefix`.
pub(crate) const STATE_KEY_PREFIX: &str = "tool_search:discovered:";
/// Go `maxDescLen`.
const MAX_DESC_LEN: i32 = 200;
/// Go `minScoreRatio`: matches scoring below this fraction of the top match's BM25 score are
/// dropped, trimming the long tail of weak matches (e.g. tools that only matched a common verb
/// like "list") so the model is not handed near-irrelevant candidates.
const MIN_SCORE_RATIO: f64 = 0.1;

/// Go `toolDescription` (toolsearch.go:58-61), byte for byte.
const TOOL_DESCRIPTION: &str = concat!(
    "Discover tools that are not yet loaded. Describe your task in natural language, ",
    "use select:tool_a,tool_b for exact names, or a regex over tool names such as get_.*_file. ",
    "Natural-language results match tool names, descriptions, and argument metadata. ",
    "After searching, finish this model response. Call discovered tools in the next model response, not alongside search_tools."
);

/// Go `skillHint`: follows [`TOOL_DESCRIPTION`] only when skill annotations are set, since no
/// result carries a `connected_skill` otherwise.
const SKILL_HINT: &str = " Results may include an optional connected_skill hint; load the relevant skill if your application supports it.";

/// Go `searchArgs.Query`'s jsonschema description.
const QUERY_DESCRIPTION: &str = "A natural-language task, keyword, regex pattern, or 'select:name1,name2' to load tools by exact name. Natural phrases like 'find documents' are ranked by relevance. Regex patterns like 'get_.*_file' are supported for name matching. Max 200 characters.";

/// Go `searchMatch.ConnectedSkill`'s jsonschema description.
const CONNECTED_SKILL_DESCRIPTION: &str = "Load if potentially relevant; generally load one.";

/// Go executeSearch's default `next_step` (toolsearch.go:310-311).
const NEXT_STEP: &str = concat!(
    "The matched tools are now available. ",
    "End this response now and call the appropriate tool in your next response."
);

/// Go executeSearch's `next_step` when a match carries a `connected_skill` (toolsearch.go:313-315).
const NEXT_STEP_WITH_SKILL: &str = concat!(
    "End this model response now. In the next response, if connected_skill could be relevant to your task ",
    "and your application supports skills, load only the most relevant one unless the task requires multiple. ",
    "Then use the matched tools."
);

/// Tunes the gating toolset (Go `toolsearch.Config`).
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Always advertised alongside `search_tools`; everything else in the base toolset is gated
    /// until the model discovers it.
    pub core_tool_names: Vec<String>,
    /// The names of the tools hidden behind `search_tools`. When set, they are listed in the
    /// `search_tools` description so the model knows what it can search for (and can `select:`
    /// them by exact name). Names that are also core, and [`TOOL_NAME`], are left out. Entries
    /// should name callable base tools: any other name is still listed, and `select:` then
    /// reports it as not found.
    pub gated_tool_names: Vec<String>,
    /// Maps tool names to skill names, so search results carry a `connected_skill` hint pointing
    /// the model at the relevant skill.
    pub skill_annotations: BTreeMap<String, String>,
    /// Replaces the default description of the search tool, for example to name the app's own
    /// domains. Empty keeps the default, which also explains `connected_skill` when
    /// `skill_annotations` is set. `gated_tool_names` are appended to either.
    pub description: String,
    /// Caps BM25 and regex matches, not exact selections. Zero or negative means 8.
    pub max_results: i32,
}

/// Go `fmt.Errorf("<msg>: %w", source)`: Display `"<msg>: <source>"`, `source()` = the cause.
#[derive(Debug)]
struct Wrapped {
    msg: &'static str,
    source: ToolError,
}

impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.msg, self.source)
    }
}

impl std::error::Error for Wrapped {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn wrap(msg: &'static str, source: ToolError) -> ToolError {
    ToolError::Execution(Box::new(Wrapped { msg, source }))
}

/// Exposes `cfg.core_tool_names` up front and gates the other base tools behind `search_tools`
/// (Go `toolsearch.New`). `search_tools` is omitted only when every base tool is already core.
///
/// [`TOOL_NAME`] is reserved for the search tool: it cannot be a core tool, and a base tool by
/// that name is never searchable or exposed. For the same reason an agent can use at most one
/// gating toolset, since two on one agent each expose a tool named [`TOOL_NAME`] and the
/// framework rejects every model step with a duplicate-tool error. Combine the catalogs into one
/// base toolset instead.
///
/// Give the agent each base tool only through the gating toolset. A base tool that also reaches
/// the agent another way, such as `LlmAgentConfig::tools`, is still searchable, and once
/// discovered it fails every later model step of the session with the same duplicate-tool error.
pub fn new(base: Arc<dyn Toolset>, cfg: Config) -> Result<Arc<dyn Toolset>, ToolError> {
    new_gating(base, cfg).map(|g| g as Arc<dyn Toolset>)
}

/// [`new`], returning the concrete toolset (the tests' Go `mustNew` type assertion).
pub(crate) fn new_gating(
    base: Arc<dyn Toolset>,
    cfg: Config,
) -> Result<Arc<GatingToolset>, ToolError> {
    if cfg.core_tool_names.iter().any(|n| n == TOOL_NAME) {
        return Err(ToolError::InvalidArgument(format!(
            "toolsearch: core tool name {} is reserved for the search tool",
            go_quote(TOOL_NAME)
        )));
    }
    let max_results = if cfg.max_results <= 0 {
        DEFAULT_MAX_RESULTS
    } else {
        cfg.max_results
    };
    let core_names: Arc<HashSet<String>> = Arc::new(cfg.core_tool_names.into_iter().collect());
    let skill_annotations = Arc::new(cfg.skill_annotations);
    let gated: Vec<String> = cfg
        .gated_tool_names
        .into_iter()
        .filter(|n| !core_names.contains(n) && n != TOOL_NAME)
        .collect();

    let mut description = cfg.description;
    if description.is_empty() {
        description = TOOL_DESCRIPTION.to_string();
        if !skill_annotations.is_empty() {
            description.push_str(SKILL_HINT);
        }
    }

    let search_tool = {
        let (base, core_names, skill_annotations) =
            (base.clone(), core_names.clone(), skill_annotations.clone());
        FunctionTool::new(
            FunctionToolConfig {
                name: TOOL_NAME.to_string(),
                description: ranksearch::describe_search(&description, &gated),
                input_schema: Some(input_schema()),
                output_schema: Some(output_schema()),
                ..Default::default()
            },
            move |ctx: Context, args: SearchArgs| {
                let (base, core_names, skill_annotations) =
                    (base.clone(), core_names.clone(), skill_annotations.clone());
                async move {
                    execute_search(
                        &ctx,
                        args,
                        base.as_ref(),
                        &core_names,
                        &skill_annotations,
                        max_results,
                    )
                    .await
                }
            },
        )
        .map_err(|e| wrap("toolsearch: build search tool", e))?
    };

    Ok(Arc::new(GatingToolset {
        base,
        core_names,
        search_tool,
        skill_annotations,
    }))
}

/// Go jsonschema-go's inferred schema for `searchArgs` (`jsonschema.For` + `Resolve`).
fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": QUERY_DESCRIPTION}
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

/// Go jsonschema-go's inferred schema for `searchOutput` (`jsonschema.For` + `Resolve`).
fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "matches": {
                "type": ["null", "array"],
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "description": {"type": "string"},
                        "connected_skill": {
                            "type": "string",
                            "description": CONNECTED_SKILL_DESCRIPTION
                        }
                    },
                    "required": ["name", "description"],
                    "additionalProperties": false
                }
            },
            "note": {"type": "string"},
            "next_step": {"type": "string"}
        },
        "required": ["matches"],
        "additionalProperties": false
    })
}

/// Adds tools to the session discovery state used by `search_tools`, so they become visible on
/// the calling agent's next model step (Go `RevealTools`). During a live session they appear only
/// in a later session. Discoveries are kept per agent, keyed by `ctx.agent_name()`. Empty and
/// already revealed names are ignored. A name is callable only while the base toolset returns a
/// tool by that name.
///
/// Each tool is stored under its own key, so reveals from several function calls in one model
/// response are all kept. Those calls run concurrently and share the session state, so a call may
/// or may not see a sibling's reveals; the relative order of tools revealed in one model response
/// is therefore unspecified, but once recorded it does not change.
pub fn reveal_tools(ctx: &Context, names: &[&str]) -> Result<(), ToolError> {
    let state = ctx.state();
    let agent_name = ctx.agent_name();
    let discovered = discovered_names(state.all(), &agent_name);
    let prefix = discovered_key_prefix(&agent_name);
    let mut next = discovered.len();
    let mut seen: HashSet<String> = discovered.into_iter().collect();
    for &name in names {
        if name.is_empty() || seen.contains(name) {
            continue;
        }
        state
            .set(&format!("{prefix}{name}"), Value::from(next))
            .map_err(|e| ToolError::Execution(Box::new(e)))?;
        seen.insert(name.to_string());
        next += 1;
    }
    Ok(())
}

/// Go `discoveredKeyPrefix`: the agent name is query-escaped so it holds no colon. Both agent and
/// tool names may contain one, and an unescaped agent "a" revealing tool "b:t" would write the
/// key agent "a:b" reads as tool "t".
fn discovered_key_prefix(agent_name: &str) -> String {
    format!("{STATE_KEY_PREFIX}{}:", url_query_escape(agent_name))
}

/// The agent's discovered tools in discovery order (Go `discoveredNames`). Calls in one model
/// response that each saw the same prior state can store equal positions; ties are broken by
/// name. A position is read as an integer or a float (a store that decodes JSON returns the
/// stored integer as a float); a tool with any other value is kept and sorted last.
pub(crate) fn discovered_names(all: Vec<(String, Value)>, agent_name: &str) -> Vec<String> {
    let prefix = discovered_key_prefix(agent_name);
    let mut entries: Vec<(String, f64)> = all
        .into_iter()
        .filter_map(|(key, value)| {
            let name = key.strip_prefix(&prefix)?;
            if name.is_empty() {
                return None;
            }
            let order = match &value {
                Value::Number(n) => n.as_f64().unwrap_or(f64::INFINITY),
                _ => f64::INFINITY,
            };
            Some((name.to_string(), order))
        })
        .collect();
    entries.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    entries.into_iter().map(|(name, _)| name).collect()
}

/// Go `searchArgs`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchArgs {
    pub(crate) query: String,
}

/// Go `searchOutput`. An empty `matches` marshals as `null`, as Go's nil slice does.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchOutput {
    #[serde(
        default,
        serialize_with = "nil_if_empty",
        deserialize_with = "empty_if_nil"
    )]
    #[schemars(with = "Option<Vec<SearchMatch>>")]
    pub(crate) matches: Vec<SearchMatch>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) note: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) next_step: String,
}

/// Go `searchMatch`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchMatch {
    pub(crate) name: String,
    pub(crate) description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) connected_skill: String,
}

/// Go encoding/json of a `[]searchMatch`: `null` when nil (empty), else the array.
fn nil_if_empty<S: Serializer>(matches: &[SearchMatch], s: S) -> Result<S::Ok, S::Error> {
    if matches.is_empty() {
        s.serialize_none()
    } else {
        s.collect_seq(matches)
    }
}

/// Go encoding/json into a `[]searchMatch`: `null` decodes to a nil (empty) slice.
fn empty_if_nil<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<SearchMatch>, D::Error> {
    Ok(Option::<Vec<SearchMatch>>::deserialize(d)?.unwrap_or_default())
}

/// Go `executeSearch`: ranks the base catalog against `args.query`, reveals every match and
/// tells the model what to do next.
pub(crate) async fn execute_search(
    ctx: &Context,
    args: SearchArgs,
    base: &dyn Toolset,
    core_names: &HashSet<String>,
    skill_annotations: &BTreeMap<String, String>,
    max_results: i32,
) -> Result<SearchOutput, ToolError> {
    let base_tools = base
        .tools(ctx.as_readonly())
        .await
        .map_err(|e| wrap("toolsearch: list base tools", e))?;

    // Tools the model already has are excluded from results, which would waste result slots, and
    // select: reports them as already available. They are taken from what `tools` returns, so a
    // core or discovered name the base does not return is reported as not found.
    let discovered = discovered_names(ctx.readonly_state().all(), &ctx.agent_name());
    let already_available: HashSet<String> = available_tools(&base_tools, core_names, &discovered)
        .iter()
        .map(|t| t.name().to_string())
        .collect();

    let (matches, note) = ranksearch::rank(
        &build_items(&base_tools),
        &args.query,
        &already_available,
        &ranksearch::Config {
            item_noun: "tool".to_string(),
            max_results,
            min_score_ratio: MIN_SCORE_RATIO,
            max_desc_len: MAX_DESC_LEN,
        },
    );
    if matches.is_empty() {
        return Ok(SearchOutput {
            note,
            ..Default::default()
        });
    }

    let mut has_connected_skill = false;
    let search_matches: Vec<SearchMatch> = matches
        .into_iter()
        .map(|m| {
            let connected_skill = skill_annotations.get(&m.name).cloned().unwrap_or_default();
            has_connected_skill |= !connected_skill.is_empty();
            SearchMatch {
                name: m.name,
                description: m.description,
                connected_skill,
            }
        })
        .collect();
    let matched_names: Vec<&str> = search_matches.iter().map(|m| m.name.as_str()).collect();
    reveal_tools(ctx, &matched_names)
        .map_err(|e| wrap("toolsearch: persist discovered tools", e))?;

    let next_step = if has_connected_skill {
        NEXT_STEP_WITH_SKILL
    } else {
        NEXT_STEP
    };
    Ok(SearchOutput {
        matches: search_matches,
        note,
        next_step: next_step.to_string(),
    })
}

/// Go `buildItems`: the catalog tools the model can call, as ranksearch items. A tool must
/// declare a function (Go's `callable`; every Rust tool has `process_request`). A model-side
/// built-in such as `gemini::GoogleSearch` declares none, so a call to it would be rejected as an
/// unknown tool: search never offers one, list it in `core_tool_names` instead. A base tool named
/// [`TOOL_NAME`] is skipped because it would collide with the search tool.
pub(crate) fn build_items(catalog: &[Arc<dyn Tool>]) -> Vec<ranksearch::Item> {
    let mut items = Vec::with_capacity(catalog.len());
    for t in catalog {
        if t.name() == TOOL_NAME {
            continue;
        }
        let Some(decl) = t.declaration() else {
            continue;
        };
        let extra = arg_tokens(&decl);
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        items.push(ranksearch::Item {
            name: t.name().to_string(),
            description: t.description().to_string(),
            tokens: ranksearch::build_tokens(t.name(), t.description(), &extra),
        });
    }
    items
}

/// Go `argTokens`: a declaration's argument names and descriptions, for the search token bag.
/// `parameters_json_schema` covers both Go shapes (functiontool's `*jsonschema.Schema` and the
/// `map[string]any` MCP decodes); a `null` or non-object schema contributes nothing. The typed
/// `parameters` schema is read too, in case a tool sets it instead.
fn arg_tokens(decl: &FunctionDeclaration) -> Vec<String> {
    let mut extra = Vec::new();
    if let Some(Value::Object(props)) = decl
        .parameters_json_schema
        .as_ref()
        .and_then(|js| js.get("properties"))
    {
        for (arg_name, arg_schema) in props {
            extra.push(arg_name.clone());
            extra.push(
                arg_schema
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            );
        }
    }
    if let Some(params) = &decl.parameters {
        for (arg_name, arg_schema) in &params.properties {
            extra.push(arg_name.clone());
            extra.push(arg_schema.description.clone());
        }
    }
    extra
}

/// Go `gatingToolset`.
pub(crate) struct GatingToolset {
    pub(crate) base: Arc<dyn Toolset>,
    pub(crate) core_names: Arc<HashSet<String>>,
    pub(crate) search_tool: Arc<dyn Tool>,
    /// The search tool's closure holds its own clone; this one is kept for the tests, which pass
    /// it to `execute_search` as Go's tests pass `g.skillAnnotations`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Go's tests read g.skillAnnotations; the closure has its own clone"
        )
    )]
    pub(crate) skill_annotations: Arc<BTreeMap<String, String>>,
}

/// Go `availableTools`: the base tools exposed next to the search tool, the core tools in
/// catalog order, then the discovered ones in discovery order. A discovered name that is core,
/// that the base no longer returns, or that is [`TOOL_NAME`] (it would duplicate the search
/// tool) is skipped. Go also skips tools the flow cannot pack; every Rust tool can be packed.
fn available_tools(
    base_tools: &[Arc<dyn Tool>],
    core_names: &HashSet<String>,
    discovered: &[String],
) -> Vec<Arc<dyn Tool>> {
    let by_name: HashMap<&str, &Arc<dyn Tool>> = base_tools.iter().map(|t| (t.name(), t)).collect();
    let mut available: Vec<Arc<dyn Tool>> = base_tools
        .iter()
        .filter(|t| core_names.contains(t.name()))
        .cloned()
        .collect();
    for name in discovered {
        if core_names.contains(name) || name == TOOL_NAME {
            continue;
        }
        if let Some(t) = by_name.get(name.as_str()) {
            available.push(Arc::clone(t));
        }
    }
    available
}

#[async_trait]
impl Toolset for GatingToolset {
    fn name(&self) -> &str {
        "SearchGatedToolset"
    }

    /// Go `gatingToolset.Tools`: `search_tools`, the core tools, and the tools the calling agent
    /// has discovered. The flow calls this before every model step, so a tool discovered by
    /// `search_tools` is callable on the next step of the same invocation. A live session is the
    /// exception: it resolves its tools once, before it first connects, so tools discovered
    /// during it appear only in a later session.
    ///
    /// Discovered tools follow the core tools in discovery order, not catalog order. While the
    /// base catalog stays the same the list only grows at the tail across turns, which preserves
    /// as much of the provider's prompt-cache prefix as a client-side toolset can. A discovered
    /// tool the base stops returning drops out of the list.
    async fn tools(&self, ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
        let base_tools = self.base.tools(ctx).await?;

        // Degenerate guard: if every base tool is already core, return them all without
        // search_tools: there is nothing left to discover.
        if base_tools
            .iter()
            .all(|t| self.core_names.contains(t.name()))
        {
            return Ok(base_tools);
        }

        let discovered = discovered_names(ctx.readonly_state().all(), &ctx.agent_name());
        let mut tools = vec![self.search_tool.clone()];
        tools.extend(available_tools(&base_tools, &self.core_names, &discovered));
        Ok(tools)
    }

    /// Go `gatingToolset.ProcessRequest`: forwards to the base toolset, so the base can still
    /// inject its own request state, such as instructions. Tools need no help here: the flow
    /// packs whatever `tools` returns.
    async fn process_request(&self, ctx: &Context, req: &mut LlmRequest) -> Result<(), ToolError> {
        self.base
            .process_request(ctx, req)
            .await
            .map_err(|e| wrap("toolsearch: base toolset ProcessRequest", e))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// `json.Marshal` of Go's resolved `jsonschema.For[searchArgs]` (jsonschema-go v0.4.3).
    const GO_INPUT_SCHEMA: &str = r#"{"type":"object","properties":{"query":{"type":"string","description":"A natural-language task, keyword, regex pattern, or 'select:name1,name2' to load tools by exact name. Natural phrases like 'find documents' are ranked by relevance. Regex patterns like 'get_.*_file' are supported for name matching. Max 200 characters."}},"required":["query"],"additionalProperties":false}"#;

    /// `json.Marshal` of Go's resolved `jsonschema.For[searchOutput]` (jsonschema-go v0.4.3).
    const GO_OUTPUT_SCHEMA: &str = r#"{"type":"object","properties":{"matches":{"type":["null","array"],"items":{"type":"object","properties":{"name":{"type":"string"},"description":{"type":"string"},"connected_skill":{"type":"string","description":"Load if potentially relevant; generally load one."}},"required":["name","description"],"additionalProperties":false}},"note":{"type":"string"},"next_step":{"type":"string"}},"required":["matches"],"additionalProperties":false}"#;

    struct EmptyToolset;

    #[async_trait]
    impl Toolset for EmptyToolset {
        fn name(&self) -> &str {
            "empty"
        }

        async fn tools(&self, _ctx: &ReadonlyContext) -> Result<Vec<Arc<dyn Tool>>, ToolError> {
            Ok(Vec::new())
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn search_tool_declaration_matches_go_schemas() {
        let g = new_gating(Arc::new(EmptyToolset), Config::default()).unwrap();
        let decl = g.search_tool.declaration().unwrap();
        assert_eq!(decl.name, TOOL_NAME);
        assert_eq!(decl.description, TOOL_DESCRIPTION);
        assert_eq!(
            decl.parameters_json_schema,
            Some(serde_json::from_str::<Value>(GO_INPUT_SCHEMA).unwrap())
        );
        assert_eq!(
            decl.response_json_schema,
            Some(serde_json::from_str::<Value>(GO_OUTPUT_SCHEMA).unwrap())
        );
        assert!(decl.parameters.is_none());
    }

    #[test]
    fn search_output_marshals_like_go() {
        // Go: json.Marshal(searchOutput{}) == {"matches":null}.
        assert_eq!(
            serde_json::to_string(&SearchOutput::default()).unwrap(),
            r#"{"matches":null}"#
        );
        let out = SearchOutput {
            matches: vec![
                SearchMatch {
                    name: "a".into(),
                    description: "d".into(),
                    connected_skill: String::new(),
                },
                SearchMatch {
                    name: "b".into(),
                    description: "e".into(),
                    connected_skill: "s".into(),
                },
            ],
            note: "n".into(),
            next_step: "x".into(),
        };
        assert_eq!(
            serde_json::to_string(&out).unwrap(),
            r#"{"matches":[{"name":"a","description":"d"},{"name":"b","description":"e","connected_skill":"s"}],"note":"n","next_step":"x"}"#
        );
        let back: SearchOutput = serde_json::from_str(r#"{"matches":null,"note":"n"}"#).unwrap();
        assert!(back.matches.is_empty());
        assert_eq!(back.note, "n");
    }

    #[test]
    fn new_names_the_toolset_like_go() {
        let ts = new(Arc::new(EmptyToolset), Config::default()).unwrap();
        assert_eq!(ts.name(), "SearchGatedToolset");
    }

    #[test]
    fn discovered_key_prefix_escapes_the_agent_name() {
        assert_eq!(
            discovered_key_prefix("test_agent"),
            "tool_search:discovered:test_agent:"
        );
        assert_eq!(
            discovered_key_prefix("a:b"),
            "tool_search:discovered:a%3Ab:"
        );
    }

    #[test]
    fn skill_hint_and_next_steps_are_go_text() {
        assert_eq!(
            SKILL_HINT,
            " Results may include an optional connected_skill hint; load the relevant skill if your application supports it."
        );
        assert_eq!(
            NEXT_STEP,
            "The matched tools are now available. End this response now and call the appropriate tool in your next response."
        );
        assert_eq!(
            NEXT_STEP_WITH_SKILL,
            "End this model response now. In the next response, if connected_skill could be relevant to your task and your application supports skills, load only the most relevant one unless the task requires multiple. Then use the matched tools."
        );
    }
}
