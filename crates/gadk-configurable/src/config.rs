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

//! The YAML config structs for agents and workflows, decoded by gadk-goyaml with yaml.v3's
//! rules.
//!
//! Shape rules: every struct's serde name is its Rust path
//! (`gadk_configurable::config::LlmAgentYamlConfig`), so YAML type errors name the struct
//! unambiguously, and each is listed in [`options`]' `verbatim_structs` (each field's YAML
//! key equals the serde field name). Every struct is `#[serde(default)]` (absent keys are
//! zero-filled); an optional value is `Option<T>`. The fields of the base agent config are
//! repeated field by field in each concrete agent struct, with the catch-all of unknown keys
//! as an [`InlineMap`] under [`gadk_goyaml::INLINE_FIELD`]; the config directory is
//! `#[serde(skip)]`. A free-form map is `Option<GoMap>` (YAML `null` and an absent key are
//! both `None`, distinct from `{}`), and a list of raw YAML nodes is `Vec<Document>`. The
//! genai `GenerateContentConfig` is not a config struct: it decodes into gadk-genai's type with
//! default keys ([`gadk_goyaml::Options::key_for`]: the field name lowercased,
//! `systeminstruction`). `omitempty` only affects encoding and is not represented.

use std::path::PathBuf;
use std::sync::LazyLock;

use gadk_genai::GenerateContentConfig;
use gadk_goyaml::{Document, InlineMap, Options};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::govalue::GoMap;

/// A reference to a function or callback by registered name.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::CodeConfig")]
pub struct CodeConfig {
    /// The `name` key: the registered name (`my_pkg.security.Check`).
    pub name: String,
    /// The `params` key.
    pub params: Option<GoMap>,
}

/// A reference to a sub-agent.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::AgentRefConfig")]
pub struct AgentRefConfig {
    /// The `config_path` key: another agent's YAML file.
    pub config_path: String,
    /// The `code` key: an inline code reference (rejected when resolving).
    pub code: String,
}

/// A tool or toolset reference with its arguments.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::ToolConfig")]
pub struct ToolConfig {
    /// The `name` key.
    pub name: String,
    /// The `args` key: `None` when the key is absent or null (distinct from `{}`).
    pub args: Option<GoMap>,
}

/// The fields every agent config shares; [`crate::Registry::from_config`]
/// decodes it first to read `agent_class`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::BaseAgentConfig")]
pub struct BaseAgentConfig {
    /// The `agent_class` key (empty → `LlmAgent`).
    pub agent_class: String,
    /// The `name` key.
    pub name: String,
    /// The `description` key.
    pub description: String,
    /// The `sub_agents` key.
    pub sub_agents: Vec<AgentRefConfig>,
    /// The `before_agent_callbacks` key.
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// The `after_agent_callbacks` key.
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// The directory of the config file this agent was loaded from, set by the factory after
    /// decoding (not read from YAML). Every config reference in the file must stay inside it.
    /// The field keeps its historical name `config_path` (a rename would break the public API).
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Every other key.
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// The LLM agent config: the base agent fields plus the LLM agent's own.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::LlmAgentYamlConfig")]
pub struct LlmAgentYamlConfig {
    /// Same as [`BaseAgentConfig::agent_class`].
    pub agent_class: String,
    /// Same as [`BaseAgentConfig::name`].
    pub name: String,
    /// Same as [`BaseAgentConfig::description`].
    pub description: String,
    /// Same as [`BaseAgentConfig::sub_agents`].
    pub sub_agents: Vec<AgentRefConfig>,
    /// Same as [`BaseAgentConfig::before_agent_callbacks`].
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Same as [`BaseAgentConfig::after_agent_callbacks`].
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// The config file's directory (not read from YAML); see [`BaseAgentConfig::config_path`].
    #[serde(skip)]
    pub config_path: PathBuf,
    /// The `model` key.
    pub model: String,
    /// The `instruction` key.
    pub instruction: String,
    /// The `tools` key.
    pub tools: Vec<ToolConfig>,
    /// The `disallow_transfer_to_peers` key.
    pub disallow_transfer_to_peers: bool,
    /// The `disallow_transfer_to_parent` key.
    pub disallow_transfer_to_parent: bool,
    /// The `generate_content_config` key (untagged genai fields use their default keys).
    pub generate_content_config: Option<GenerateContentConfig>,
    /// Same as [`BaseAgentConfig::additional_properties`].
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// The loop agent config: the base agent fields plus `max_iterations`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::LoopAgentYamlConfig")]
pub struct LoopAgentYamlConfig {
    /// Same as [`BaseAgentConfig::agent_class`].
    pub agent_class: String,
    /// Same as [`BaseAgentConfig::name`].
    pub name: String,
    /// Same as [`BaseAgentConfig::description`].
    pub description: String,
    /// Same as [`BaseAgentConfig::sub_agents`].
    pub sub_agents: Vec<AgentRefConfig>,
    /// Same as [`BaseAgentConfig::before_agent_callbacks`].
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Same as [`BaseAgentConfig::after_agent_callbacks`].
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// The config file's directory (not read from YAML); see [`BaseAgentConfig::config_path`].
    #[serde(skip)]
    pub config_path: PathBuf,
    /// The `max_iterations` key (64-bit).
    pub max_iterations: u64,
    /// Same as [`BaseAgentConfig::additional_properties`].
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// The parallel agent config: the base agent fields only.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::ParallelAgentYamlConfig")]
pub struct ParallelAgentYamlConfig {
    /// Same as [`BaseAgentConfig::agent_class`].
    pub agent_class: String,
    /// Same as [`BaseAgentConfig::name`].
    pub name: String,
    /// Same as [`BaseAgentConfig::description`].
    pub description: String,
    /// Same as [`BaseAgentConfig::sub_agents`].
    pub sub_agents: Vec<AgentRefConfig>,
    /// Same as [`BaseAgentConfig::before_agent_callbacks`].
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Same as [`BaseAgentConfig::after_agent_callbacks`].
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// The config file's directory (not read from YAML); see [`BaseAgentConfig::config_path`].
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Same as [`BaseAgentConfig::additional_properties`].
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// The sequential agent config: the base agent fields only.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(
    default,
    rename = "gadk_configurable::config::SequentialAgentYamlConfig"
)]
pub struct SequentialAgentYamlConfig {
    /// Same as [`BaseAgentConfig::agent_class`].
    pub agent_class: String,
    /// Same as [`BaseAgentConfig::name`].
    pub name: String,
    /// Same as [`BaseAgentConfig::description`].
    pub description: String,
    /// Same as [`BaseAgentConfig::sub_agents`].
    pub sub_agents: Vec<AgentRefConfig>,
    /// Same as [`BaseAgentConfig::before_agent_callbacks`].
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Same as [`BaseAgentConfig::after_agent_callbacks`].
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// The config file's directory (not read from YAML); see [`BaseAgentConfig::config_path`].
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Same as [`BaseAgentConfig::additional_properties`].
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// The workflow agent config: the base agent fields plus `edges` and `max_concurrency`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::WorkflowYamlConfig")]
pub struct WorkflowYamlConfig {
    /// Same as [`BaseAgentConfig::agent_class`].
    pub agent_class: String,
    /// Same as [`BaseAgentConfig::name`].
    pub name: String,
    /// Same as [`BaseAgentConfig::description`].
    pub description: String,
    /// Same as [`BaseAgentConfig::sub_agents`].
    pub sub_agents: Vec<AgentRefConfig>,
    /// Same as [`BaseAgentConfig::before_agent_callbacks`].
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Same as [`BaseAgentConfig::after_agent_callbacks`].
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// The config file's directory (not read from YAML); see [`BaseAgentConfig::config_path`].
    #[serde(skip)]
    pub config_path: PathBuf,
    /// The `edges` key: each element's root is the edge chain's node.
    pub edges: Vec<Document>,
    /// The `max_concurrency` key.
    pub max_concurrency: i64,
    /// Same as [`BaseAgentConfig::additional_properties`].
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// A function node of a workflow.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::FunctionNodeYamlConfig")]
pub struct FunctionNodeYamlConfig {
    /// The `name` key.
    pub name: String,
    /// The `func_code` key: a registered node-function name.
    pub func_code: String,
    /// The `rerun_on_resume` key.
    pub rerun_on_resume: Option<bool>,
    /// The `parallel_worker` key.
    pub parallel_worker: bool,
}

/// A join node of a workflow.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::JoinNodeYamlConfig")]
pub struct JoinNodeYamlConfig {
    /// The `name` key.
    pub name: String,
}

/// A tool node of a workflow.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "gadk_configurable::config::ToolNodeYamlConfig")]
pub struct ToolNodeYamlConfig {
    /// The `name` key.
    pub name: String,
    /// The `tool_code` key: a registered tool name.
    pub tool_code: String,
    /// The `rerun_on_resume` key.
    pub rerun_on_resume: Option<bool>,
    /// The `parallel_worker` key.
    pub parallel_worker: bool,
    /// The `args` key: `None` when the key is absent or null (distinct from `{}`).
    pub args: Option<GoMap>,
}

/// The serde names of every yaml-tagged config struct (their keys are the field names).
pub const VERBATIM_STRUCTS: [&str; 12] = [
    "gadk_configurable::config::CodeConfig",
    "gadk_configurable::config::AgentRefConfig",
    "gadk_configurable::config::ToolConfig",
    "gadk_configurable::config::BaseAgentConfig",
    "gadk_configurable::config::LlmAgentYamlConfig",
    "gadk_configurable::config::LoopAgentYamlConfig",
    "gadk_configurable::config::ParallelAgentYamlConfig",
    "gadk_configurable::config::SequentialAgentYamlConfig",
    "gadk_configurable::config::WorkflowYamlConfig",
    "gadk_configurable::config::FunctionNodeYamlConfig",
    "gadk_configurable::config::JoinNodeYamlConfig",
    "gadk_configurable::config::ToolNodeYamlConfig",
];

static OPTIONS: LazyLock<Options> = LazyLock::new(|| Options {
    verbatim_structs: VERBATIM_STRUCTS.into_iter().collect(),
    field_keys: Default::default(),
});

/// The gadk-goyaml options for the config structs: [`VERBATIM_STRUCTS`] verbatim, everything
/// else (genai types) with their default keys.
pub fn options() -> &'static Options {
    &OPTIONS
}

/// Decodes YAML into a config struct with yaml.v3's rules ([`options`]).
pub fn unmarshal<T: DeserializeOwned>(data: &[u8]) -> Result<T, gadk_goyaml::Error> {
    gadk_goyaml::unmarshal(data, options())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use gadk_goyaml::{GoValue, Kind};
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn base_config_peeks_agent_class_and_keeps_extra_keys() {
        let cfg: BaseAgentConfig = unmarshal(
            b"agent_class: LoopAgent\nname: loop\nmax_iterations: 3\nsub_agents:\n  - config_path: a.yaml\n",
        )
        .unwrap();
        assert_eq!(cfg.agent_class, "LoopAgent");
        assert_eq!(cfg.name, "loop");
        assert_eq!(
            cfg.sub_agents,
            vec![AgentRefConfig {
                config_path: "a.yaml".into(),
                code: String::new()
            }]
        );
        assert_eq!(
            cfg.additional_properties.0,
            vec![("max_iterations".to_string(), GoValue::Int(3))]
        );
        assert_eq!(cfg.config_path, PathBuf::new());
    }

    #[test]
    fn empty_document_decodes_to_the_zero_config() {
        let cfg: BaseAgentConfig = unmarshal(b"").unwrap();
        assert_eq!(cfg, BaseAgentConfig::default());
        assert_eq!(cfg.agent_class, "");
    }

    #[test]
    fn llm_config_decodes_every_field() {
        let yaml = br#"
name: root
description: The root agent.
model: gemini-2.5-flash
instruction: |
  Be brief.
disallow_transfer_to_peers: true
sub_agents:
  - config_path: sub.yaml
  - code: my.pkg.agent
before_agent_callbacks:
  - name: cb.before
after_agent_callbacks:
  - name: cb.after
    params: {k: 1}
tools:
  - name: google_search
  - name: AgentTool
    args:
      agent:
        config_path: helper.yaml
        skip_summarization: true
generate_content_config:
  temperature: 0.25
  maxoutputtokens: 128
  systeminstruction:
    parts:
      - text: sys
custom_key: kept
"#;
        let cfg: LlmAgentYamlConfig = unmarshal(yaml).unwrap();
        assert_eq!(cfg.name, "root");
        assert_eq!(cfg.description, "The root agent.");
        assert_eq!(cfg.model, "gemini-2.5-flash");
        assert_eq!(cfg.instruction, "Be brief.\n");
        assert!(cfg.disallow_transfer_to_peers);
        assert!(!cfg.disallow_transfer_to_parent);
        assert_eq!(cfg.sub_agents.len(), 2);
        assert_eq!(cfg.sub_agents[1].code, "my.pkg.agent");
        assert_eq!(cfg.before_agent_callbacks[0].name, "cb.before");
        assert_eq!(cfg.before_agent_callbacks[0].params, None);
        assert_eq!(
            cfg.after_agent_callbacks[0].params,
            Some(GoMap::from([("k".to_string(), GoValue::Int(1))]))
        );
        assert_eq!(cfg.tools[0].name, "google_search");
        assert_eq!(cfg.tools[0].args, None);
        let args = cfg.tools[1].args.as_ref().unwrap();
        assert_eq!(
            args["agent"],
            GoValue::Map(vec![
                ("config_path".into(), GoValue::String("helper.yaml".into())),
                ("skip_summarization".into(), GoValue::Bool(true)),
            ])
        );
        let gcc = cfg.generate_content_config.unwrap();
        assert_eq!(gcc.temperature, Some(0.25));
        assert_eq!(gcc.max_output_tokens, 128);
        assert_eq!(gcc.system_instruction.unwrap().parts[0].text, "sys");
        assert_eq!(
            cfg.additional_properties.0,
            vec![("custom_key".to_string(), GoValue::String("kept".into()))]
        );
    }

    #[test]
    fn genai_config_uses_go_default_keys_not_snake_case() {
        // Go's genai.GenerateContentConfig has no yaml tags: `max_output_tokens` is unknown.
        let cfg: LlmAgentYamlConfig =
            unmarshal(b"generate_content_config:\n  max_output_tokens: 5\n").unwrap();
        assert_eq!(cfg.generate_content_config.unwrap().max_output_tokens, 0);
    }

    #[test]
    fn tool_args_keep_nil_distinct_from_empty() {
        let cfg: LlmAgentYamlConfig =
            unmarshal(b"tools:\n  - name: a\n  - name: b\n    args:\n  - name: c\n    args: {}\n")
                .unwrap();
        assert_eq!(cfg.tools[0].args, None);
        assert_eq!(cfg.tools[1].args, None);
        assert_eq!(cfg.tools[2].args, Some(GoMap::new()));
    }

    #[test]
    fn loop_config_decodes_max_iterations() {
        let cfg: LoopAgentYamlConfig =
            unmarshal(b"agent_class: LoopAgent\nname: l\nmax_iterations: 4\n").unwrap();
        assert_eq!(cfg.max_iterations, 4);
        assert!(cfg.additional_properties.is_empty());
        let err = unmarshal::<LoopAgentYamlConfig>(b"max_iterations: -1\n").unwrap_err();
        // gadk-goyaml names the target by its serde entry point: `u64`.
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!int `-1` into u64"
        );
    }

    #[test]
    fn parallel_and_sequential_configs_decode_base_fields() {
        let p: ParallelAgentYamlConfig = unmarshal(
            b"agent_class: ParallelAgent\nname: p\nsub_agents: [{config_path: a.yaml}]\n",
        )
        .unwrap();
        assert_eq!(p.name, "p");
        assert_eq!(p.sub_agents[0].config_path, "a.yaml");
        let s: SequentialAgentYamlConfig =
            unmarshal(b"agent_class: SequentialAgent\nname: s\ndescription: d\n").unwrap();
        assert_eq!((s.name.as_str(), s.description.as_str()), ("s", "d"));
    }

    #[test]
    fn workflow_config_keeps_edges_as_nodes() {
        let cfg: WorkflowYamlConfig = unmarshal(
            b"agent_class: Workflow\nname: wf\nmax_concurrency: 2\nedges:\n  - [START, upper_fn, suffix_fn]\n  - - router.yaml\n    - {ALPHA: a.yaml, default: b.yaml}\n  - lone\n",
        )
        .unwrap();
        assert_eq!(cfg.max_concurrency, 2);
        assert_eq!(cfg.edges.len(), 3);
        let first = &cfg.edges[0];
        let root = &first[first.root()];
        assert_eq!(root.kind, Kind::Sequence);
        let values: Vec<&str> = root
            .content
            .iter()
            .map(|&c| first[c].value.as_str())
            .collect();
        assert_eq!(values, ["START", "upper_fn", "suffix_fn"]);
        let second = &cfg.edges[1];
        let items = &second[second.root()].content;
        assert_eq!(second[items[0]].kind, Kind::Scalar);
        assert_eq!(second[items[1]].kind, Kind::Mapping);
        let routes: std::collections::BTreeMap<String, String> =
            gadk_goyaml::from_node(second, items[1], options()).unwrap();
        assert_eq!(routes["ALPHA"], "a.yaml");
        assert_eq!(routes["default"], "b.yaml");
        let third = &cfg.edges[2];
        assert_eq!(third[third.root()].kind, Kind::Scalar);
    }

    #[test]
    fn node_configs_decode() {
        let f: FunctionNodeYamlConfig = unmarshal(
            b"agent_class: FunctionNode\nname: f\nfunc_code: upper_fn\nrerun_on_resume: false\nparallel_worker: true\n",
        )
        .unwrap();
        assert_eq!(
            f,
            FunctionNodeYamlConfig {
                name: "f".into(),
                func_code: "upper_fn".into(),
                rerun_on_resume: Some(false),
                parallel_worker: true,
            }
        );
        let j: JoinNodeYamlConfig = unmarshal(b"agent_class: JoinNode\nname: j\n").unwrap();
        assert_eq!(j.name, "j");
        let t: ToolNodeYamlConfig = unmarshal(
            b"agent_class: ToolNode\nname: t\ntool_code: AgentTool\nargs:\n  agent: {config_path: x.yaml}\n",
        )
        .unwrap();
        assert_eq!(t.tool_code, "AgentTool");
        assert_eq!(t.rerun_on_resume, None);
        assert!(t.args.unwrap().contains_key("agent"));
    }

    #[test]
    fn type_errors_name_go_types() {
        let err = unmarshal::<LlmAgentYamlConfig>(b"sub_agents: [x]\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `x` into gadk_configurable::config::AgentRefConfig"
        );
        let err = unmarshal::<BaseAgentConfig>(b"name: [a]\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!seq into String"
        );
    }

    #[test]
    fn syntax_errors_are_yaml_v3_texts() {
        let err = unmarshal::<BaseAgentConfig>(b"name: [\n").unwrap_err();
        assert!(err.to_string().starts_with("yaml: line "), "{err}");
    }

    #[test]
    fn options_list_every_config_struct() {
        let opts = options();
        assert_eq!(opts.verbatim_structs.len(), VERBATIM_STRUCTS.len());
        assert_eq!(
            opts.key_for(
                "gadk_configurable::config::LlmAgentYamlConfig",
                "generate_content_config"
            ),
            "generate_content_config"
        );
        assert_eq!(
            opts.key_for("GenerateContentConfig", "systemInstruction"),
            "systeminstruction"
        );
    }
}
