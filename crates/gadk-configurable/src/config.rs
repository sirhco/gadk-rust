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

//! Go `internal/configurable/configurable.go` and `configurable_workflow.go`: the yaml-tagged
//! config structs, decoded by gadk-goyaml with yaml.v3's rules.
//!
//! Go → Rust (the 6c-1 shape rules): every struct's serde name is Go's package-qualified type
//! name (`configurable.llmAgentYAMLConfig`), so yaml.v3 type errors name it as Go does, and is
//! listed in [`options`]' `verbatim_structs` (each Go field carries a `yaml:"<key>"` tag equal
//! to the serde field name); every struct is `#[serde(default)]` (Go zero-fills absent keys);
//! `*T` is `Option<T>`; Go's embedded `baseAgentConfig` `,inline` is repeated field by field in
//! each concrete struct, with its `AdditionalProperties map[string]any ,inline` as an
//! [`InlineMap`] under [`gadk_goyaml::INLINE_FIELD`]; `ConfigPath yaml:"-"` is
//! `#[serde(skip)]`; `map[string]any` is `Option<GoMap>` (yaml `null` and an absent key stay
//! Go's nil map, distinct from `{}`); `[]yaml.Node` is `Vec<Document>`. Untagged
//! `*genai.GenerateContentConfig` decodes into gadk-genai's type with Go's default keys
//! ([`gadk_goyaml::Options::key_for`]: the field name lowercased, `systeminstruction`).
//! `omitempty` only affects encoding and is not represented.

use std::path::PathBuf;
use std::sync::LazyLock;

use gadk_genai::GenerateContentConfig;
use gadk_goyaml::{Document, InlineMap, Options};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::govalue::GoMap;

/// Go `codeConfig`: a reference to a function or callback.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.codeConfig")]
pub struct CodeConfig {
    /// Go `Name yaml:"name"`: the registered name (`my_pkg.security.Check`).
    pub name: String,
    /// Go `Params map[string]any yaml:"params,omitempty"`.
    pub params: Option<GoMap>,
}

/// Go `agentRefConfig`: a reference to a sub-agent.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.agentRefConfig")]
pub struct AgentRefConfig {
    /// Go `ConfigPath yaml:"config_path,omitempty"`: another agent's YAML file.
    pub config_path: String,
    /// Go `Code yaml:"code,omitempty"`: an inline code reference (rejected, as in Go).
    pub code: String,
}

/// Go `ToolConfig`: a tool or toolset reference with its arguments.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.ToolConfig")]
pub struct ToolConfig {
    /// Go `Name yaml:"name"`.
    pub name: String,
    /// Go `Args map[string]any yaml:"args,omitempty"`: `None` is Go's nil map.
    pub args: Option<GoMap>,
}

/// Go `baseAgentConfig`: the fields every agent config shares; [`crate::Registry::from_config`]
/// decodes it first to read `agent_class`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.baseAgentConfig")]
pub struct BaseAgentConfig {
    /// Go `AgentClass yaml:"agent_class"` (empty → `LlmAgent`).
    pub agent_class: String,
    /// Go `Name yaml:"name"`.
    pub name: String,
    /// Go `Description yaml:"description,omitempty"`.
    pub description: String,
    /// Go `SubAgents yaml:"sub_agents,omitempty"`.
    pub sub_agents: Vec<AgentRefConfig>,
    /// Go `BeforeAgentCallbacks yaml:"before_agent_callbacks,omitempty"`.
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Go `AfterAgentCallbacks yaml:"after_agent_callbacks,omitempty"`.
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// Go `ConfigDir yaml:"-"` (0990f9e; was `ConfigPath`): the directory of the config file
    /// this agent was loaded from, set by the factory after decoding. Every config reference in
    /// the file must stay inside it. The field keeps its gadk name (a rename would break the
    /// public API).
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Go `AdditionalProperties map[string]any yaml:",inline"`: every other key.
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// Go `llmAgentYAMLConfig`: `baseAgentConfig` inline plus the LLM agent's fields.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.llmAgentYAMLConfig")]
pub struct LlmAgentYamlConfig {
    /// Inlined `baseAgentConfig.AgentClass`.
    pub agent_class: String,
    /// Inlined `baseAgentConfig.Name`.
    pub name: String,
    /// Inlined `baseAgentConfig.Description`.
    pub description: String,
    /// Inlined `baseAgentConfig.SubAgents`.
    pub sub_agents: Vec<AgentRefConfig>,
    /// Inlined `baseAgentConfig.BeforeAgentCallbacks`.
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.AfterAgentCallbacks`.
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.ConfigDir` (`yaml:"-"`): the config file's directory.
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Go `Model yaml:"model"`.
    pub model: String,
    /// Go `Instruction yaml:"instruction"`.
    pub instruction: String,
    /// Go `Tools yaml:"tools,omitempty"`.
    pub tools: Vec<ToolConfig>,
    /// Go `DisallowTransferToPeers yaml:"disallow_transfer_to_peers,omitempty"`.
    pub disallow_transfer_to_peers: bool,
    /// Go `DisallowTransferToParent yaml:"disallow_transfer_to_parent,omitempty"`.
    pub disallow_transfer_to_parent: bool,
    /// Go `GenerateContentConfig *genai.GenerateContentConfig
    /// yaml:"generate_content_config,omitempty"` (untagged genai fields: Go default keys).
    pub generate_content_config: Option<GenerateContentConfig>,
    /// Inlined `baseAgentConfig.AdditionalProperties` (`,inline`).
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// Go `loopAgentYAMLConfig`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.loopAgentYAMLConfig")]
pub struct LoopAgentYamlConfig {
    /// Inlined `baseAgentConfig.AgentClass`.
    pub agent_class: String,
    /// Inlined `baseAgentConfig.Name`.
    pub name: String,
    /// Inlined `baseAgentConfig.Description`.
    pub description: String,
    /// Inlined `baseAgentConfig.SubAgents`.
    pub sub_agents: Vec<AgentRefConfig>,
    /// Inlined `baseAgentConfig.BeforeAgentCallbacks`.
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.AfterAgentCallbacks`.
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.ConfigDir` (`yaml:"-"`): the config file's directory.
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Go `MaxIterations uint yaml:"max_iterations"` (Go `uint` is 64-bit).
    pub max_iterations: u64,
    /// Inlined `baseAgentConfig.AdditionalProperties` (`,inline`).
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// Go `parallelAgentYAMLConfig`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.parallelAgentYAMLConfig")]
pub struct ParallelAgentYamlConfig {
    /// Inlined `baseAgentConfig.AgentClass`.
    pub agent_class: String,
    /// Inlined `baseAgentConfig.Name`.
    pub name: String,
    /// Inlined `baseAgentConfig.Description`.
    pub description: String,
    /// Inlined `baseAgentConfig.SubAgents`.
    pub sub_agents: Vec<AgentRefConfig>,
    /// Inlined `baseAgentConfig.BeforeAgentCallbacks`.
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.AfterAgentCallbacks`.
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.ConfigDir` (`yaml:"-"`): the config file's directory.
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Inlined `baseAgentConfig.AdditionalProperties` (`,inline`).
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// Go `sequentialAgentYAMLConfig`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.sequentialAgentYAMLConfig")]
pub struct SequentialAgentYamlConfig {
    /// Inlined `baseAgentConfig.AgentClass`.
    pub agent_class: String,
    /// Inlined `baseAgentConfig.Name`.
    pub name: String,
    /// Inlined `baseAgentConfig.Description`.
    pub description: String,
    /// Inlined `baseAgentConfig.SubAgents`.
    pub sub_agents: Vec<AgentRefConfig>,
    /// Inlined `baseAgentConfig.BeforeAgentCallbacks`.
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.AfterAgentCallbacks`.
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.ConfigDir` (`yaml:"-"`): the config file's directory.
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Inlined `baseAgentConfig.AdditionalProperties` (`,inline`).
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// Go `workflowYAMLConfig` (`configurable_workflow.go`).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.workflowYAMLConfig")]
pub struct WorkflowYamlConfig {
    /// Inlined `baseAgentConfig.AgentClass`.
    pub agent_class: String,
    /// Inlined `baseAgentConfig.Name`.
    pub name: String,
    /// Inlined `baseAgentConfig.Description`.
    pub description: String,
    /// Inlined `baseAgentConfig.SubAgents`.
    pub sub_agents: Vec<AgentRefConfig>,
    /// Inlined `baseAgentConfig.BeforeAgentCallbacks`.
    pub before_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.AfterAgentCallbacks`.
    pub after_agent_callbacks: Vec<CodeConfig>,
    /// Inlined `baseAgentConfig.ConfigDir` (`yaml:"-"`): the config file's directory.
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Go `Edges []yaml.Node yaml:"edges"`: each element's root is the edge chain's node.
    pub edges: Vec<Document>,
    /// Go `MaxConcurrency int yaml:"max_concurrency,omitempty"`.
    pub max_concurrency: i64,
    /// Inlined `baseAgentConfig.AdditionalProperties` (`,inline`).
    #[serde(rename = "$gadk_goyaml::inline")]
    pub additional_properties: InlineMap,
}

/// Go `functionNodeYAMLConfig`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.functionNodeYAMLConfig")]
pub struct FunctionNodeYamlConfig {
    /// Go `Name yaml:"name"`.
    pub name: String,
    /// Go `FuncCode yaml:"func_code"`: a registered node-function name.
    pub func_code: String,
    /// Go `RerunOnResume *bool yaml:"rerun_on_resume,omitempty"`.
    pub rerun_on_resume: Option<bool>,
    /// Go `ParallelWorker bool yaml:"parallel_worker,omitempty"`.
    pub parallel_worker: bool,
}

/// Go `joinNodeYAMLConfig`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.joinNodeYAMLConfig")]
pub struct JoinNodeYamlConfig {
    /// Go `Name yaml:"name"`.
    pub name: String,
}

/// Go `toolNodeYAMLConfig`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, rename = "configurable.toolNodeYAMLConfig")]
pub struct ToolNodeYamlConfig {
    /// Go `Name yaml:"name"`.
    pub name: String,
    /// Go `ToolCode yaml:"tool_code"`: a registered tool name.
    pub tool_code: String,
    /// Go `RerunOnResume *bool yaml:"rerun_on_resume,omitempty"`.
    pub rerun_on_resume: Option<bool>,
    /// Go `ParallelWorker bool yaml:"parallel_worker,omitempty"`.
    pub parallel_worker: bool,
    /// Go `Args map[string]any yaml:"args,omitempty"`: `None` is Go's nil map.
    pub args: Option<GoMap>,
}

/// The serde names of every yaml-tagged config struct (their keys are the field names).
pub const VERBATIM_STRUCTS: [&str; 12] = [
    "configurable.codeConfig",
    "configurable.agentRefConfig",
    "configurable.ToolConfig",
    "configurable.baseAgentConfig",
    "configurable.llmAgentYAMLConfig",
    "configurable.loopAgentYAMLConfig",
    "configurable.parallelAgentYAMLConfig",
    "configurable.sequentialAgentYAMLConfig",
    "configurable.workflowYAMLConfig",
    "configurable.functionNodeYAMLConfig",
    "configurable.joinNodeYAMLConfig",
    "configurable.toolNodeYAMLConfig",
];

static OPTIONS: LazyLock<Options> = LazyLock::new(|| Options {
    verbatim_structs: VERBATIM_STRUCTS.into_iter().collect(),
    field_keys: Default::default(),
});

/// The gadk-goyaml options for the config structs: [`VERBATIM_STRUCTS`] verbatim, everything
/// else (genai types) with Go's default keys.
pub fn options() -> &'static Options {
    &OPTIONS
}

/// Go `yaml.Unmarshal(data, &cfg)` for a config struct.
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
        // Go names its `uint` field type `uint`; gadk-goyaml names a u64 `uint64` (the 6c-1
        // serde-entry-point type names).
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!int `-1` into uint64"
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
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!str `x` into configurable.agentRefConfig"
        );
        let err = unmarshal::<BaseAgentConfig>(b"name: [a]\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "yaml: unmarshal errors:\n  line 1: cannot unmarshal !!seq into string"
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
            opts.key_for("configurable.llmAgentYAMLConfig", "generate_content_config"),
            "generate_content_config"
        );
        assert_eq!(
            opts.key_for("GenerateContentConfig", "systemInstruction"),
            "systeminstruction"
        );
    }
}
