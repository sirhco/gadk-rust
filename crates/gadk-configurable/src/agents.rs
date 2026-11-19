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

//! Conversion of the parsed YAML agent configs into engine agent configs (`to_*_agent_config`),
//! the sub-agent, tool and callback resolvers, and the agent factories (`new_llm_agent`,
//! `new_loop_agent`, `new_parallel_agent`, `new_sequential_agent`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gadk_agent::{Agent, AgentConfig, Tool, Toolset};
use gadk_engine::agents::loop_agent::{self, LoopAgentConfig};
use gadk_engine::agents::parallel_agent::{self, ParallelAgentConfig};
use gadk_engine::agents::sequential_agent::{self, SequentialAgentConfig};
use gadk_engine::llm::LlmAgentConfig;
use gadk_engine::llm_agent;
use gadk_gemini::{ClientConfig, GeminiModel};
use gadk_model::is_gemini_model;

use crate::config::{
    self, AgentRefConfig, CodeConfig, LlmAgentYamlConfig, LoopAgentYamlConfig,
    ParallelAgentYamlConfig, SequentialAgentYamlConfig, ToolConfig,
};
use crate::{AfterAgent, BeforeAgent, CallbackType, ConfigError, LoadCtx, Registry};

/// Builds the engine config for an LLM agent: `model {name} is not supported` for a non-Gemini
/// model, a [`GeminiModel`] using `GOOGLE_API_KEY` (`failed to create model: {cause}`), then
/// sub-agents, tools/toolsets and callbacks.
pub(crate) async fn to_llm_agent_config(
    c: &LlmAgentYamlConfig,
    registry: &Registry,
    ctx: &LoadCtx,
) -> Result<LlmAgentConfig, ConfigError> {
    if !is_gemini_model(&c.model) {
        return Err(ConfigError::msg(format!(
            "model {} is not supported",
            c.model
        )));
    }

    // The API key comes from `GOOGLE_API_KEY`; the client reads the rest of its settings from
    // the same environment.
    let env = registry.inner.read().env.clone();
    let model = GeminiModel::new(
        &c.model,
        ClientConfig {
            api_key: Some(env.var("GOOGLE_API_KEY").unwrap_or_default()),
            env,
            ..Default::default()
        },
    )
    .await
    .map_err(|e| ConfigError::wrap("failed to create model", ConfigError::other(e)))?;

    let sub_agents = resolve_sub_agents(registry, ctx, &c.config_path, &c.sub_agents).await?;

    let (tools, toolsets) = resolve_tools(registry, ctx, &c.config_path, &c.tools).await?;

    let before_callbacks = resolve_callbacks::<BeforeAgent>(registry, &c.before_agent_callbacks)?;

    let after_callbacks = resolve_callbacks::<AfterAgent>(registry, &c.after_agent_callbacks)?;

    Ok(LlmAgentConfig {
        name: c.name.clone(),
        description: c.description.clone(),
        sub_agents,
        model: Some(model),
        instruction: c.instruction.clone(),
        disallow_transfer_to_peers: c.disallow_transfer_to_peers,
        disallow_transfer_to_parent: c.disallow_transfer_to_parent,
        tools,
        toolsets,
        generate_content_config: c.generate_content_config.clone(),
        before_agent_callbacks: before_callbacks,
        after_agent_callbacks: after_callbacks,
        ..Default::default()
    })
}

/// Builds the engine config for a loop agent from its YAML config.
pub(crate) async fn to_loop_agent_config(
    c: &LoopAgentYamlConfig,
    registry: &Registry,
    ctx: &LoadCtx,
) -> Result<LoopAgentConfig, ConfigError> {
    let sub_agents = resolve_sub_agents(registry, ctx, &c.config_path, &c.sub_agents).await?;

    Ok(LoopAgentConfig {
        agent: AgentConfig {
            name: c.name.clone(),
            description: c.description.clone(),
            sub_agents,
            ..Default::default()
        },
        // `max_iterations` is a 64-bit count in the YAML config but a `u32` in gadk-engine, so
        // a value above `u32::MAX` saturates rather than erroring (either count outlasts any
        // real run).
        max_iterations: u32::try_from(c.max_iterations).unwrap_or(u32::MAX),
    })
}

/// Builds the engine config for a parallel agent from its YAML config.
pub(crate) async fn to_parallel_agent_config(
    c: &ParallelAgentYamlConfig,
    registry: &Registry,
    ctx: &LoadCtx,
) -> Result<ParallelAgentConfig, ConfigError> {
    let sub_agents = resolve_sub_agents(registry, ctx, &c.config_path, &c.sub_agents).await?;

    Ok(ParallelAgentConfig {
        agent: AgentConfig {
            name: c.name.clone(),
            description: c.description.clone(),
            sub_agents,
            ..Default::default()
        },
    })
}

/// Builds the engine config for a sequential agent from its YAML config.
pub(crate) async fn to_sequential_agent_config(
    c: &SequentialAgentYamlConfig,
    registry: &Registry,
    ctx: &LoadCtx,
) -> Result<SequentialAgentConfig, ConfigError> {
    let sub_agents = resolve_sub_agents(registry, ctx, &c.config_path, &c.sub_agents).await?;

    Ok(SequentialAgentConfig {
        agent: AgentConfig {
            name: c.name.clone(),
            description: c.description.clone(),
            sub_agents,
            ..Default::default()
        },
    })
}

/// Resolves each sub-agent reference: `failed to resolve agent reference {path}: {cause}`; an
/// inline `code` reference fails with `inline code agent references are not yet supported for
/// {code}`.
pub(crate) async fn resolve_sub_agents(
    registry: &Registry,
    ctx: &LoadCtx,
    config_dir: &Path,
    refs: &[AgentRefConfig],
) -> Result<Vec<Arc<dyn Agent>>, ConfigError> {
    let mut agents = Vec::new();
    for r in refs {
        if !r.config_path.is_empty() {
            let a = registry
                .resolve_agent_reference_with_ctx(ctx, config_dir, &r.config_path)
                .await
                .map_err(|e| {
                    ConfigError::wrap(
                        format!("failed to resolve agent reference {}", r.config_path),
                        e,
                    )
                })?;
            agents.push(a);
        } else if !r.code.is_empty() {
            return Err(ConfigError::msg(format!(
                "inline code agent references are not yet supported for {}",
                r.code
            )));
        }
    }
    Ok(agents)
}

/// Resolves each named tool or toolset, setting the parent path in the context
/// ([`LoadCtx::with_parent_path`]) to the config directory first;
/// `failed to resolve tool reference {name}: {cause}`.
pub(crate) async fn resolve_tools(
    registry: &Registry,
    ctx: &LoadCtx,
    config_dir: &Path,
    tool_configs: &[ToolConfig],
) -> Result<(Vec<Arc<dyn Tool>>, Vec<Arc<dyn Toolset>>), ConfigError> {
    let mut tools = Vec::new();
    let mut toolsets = Vec::new();
    for tc in tool_configs {
        if !tc.name.is_empty() {
            let ctx = ctx.with_parent_path(config_dir);
            let (a, ts) = registry
                .resolve_tool_reference(&ctx, &tc.name, tc.args.as_ref())
                .await
                .map_err(|e| {
                    ConfigError::wrap(format!("failed to resolve tool reference {}", tc.name), e)
                })?;
            if let Some(a) = a {
                tools.push(a);
            }
            if let Some(ts) = ts {
                toolsets.push(ts);
            }
        }
    }
    Ok((tools, toolsets))
}

/// Resolves each named callback to a `T`: `failed to resolve callback reference {name}:
/// {cause}`, and `callback {name} is of type {actual} and not {expected}` when the registered
/// callback is not a `T`.
pub(crate) fn resolve_callbacks<T: CallbackType>(
    registry: &Registry,
    callbacks: &[CodeConfig],
) -> Result<Vec<T::Fn>, ConfigError> {
    let mut cbs = Vec::new();
    for r in callbacks {
        if !r.name.is_empty() {
            let c = registry.resolve_callback_reference(&r.name).map_err(|e| {
                ConfigError::wrap(
                    format!("failed to resolve callback reference {}", r.name),
                    e,
                )
            })?;
            let Some(cb) = T::from_callback(&c) else {
                return Err(ConfigError::msg(format!(
                    "callback {} is of type {} and not {}",
                    r.name,
                    c.type_name(),
                    T::TYPE_NAME
                )));
            };
            cbs.push(cb);
        }
    }
    Ok(cbs)
}

/// Parses and builds an LLM agent: `failed to parse LLM agent config: {cause}`,
/// `'name' is required`, `'model' is required for LlmAgent`,
/// `failed to create LLM agent config: {cause}`, then [`llm_agent::new`].
pub(crate) async fn new_llm_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_dir: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    let mut cfg: LlmAgentYamlConfig = config::unmarshal(&data)
        .map_err(|e| ConfigError::wrap("failed to parse LLM agent config", e))?;

    if cfg.name.is_empty() {
        return Err(ConfigError::msg("'name' is required"));
    }
    if cfg.model.is_empty() {
        return Err(ConfigError::msg("'model' is required for LlmAgent"));
    }

    cfg.config_path = config_dir;

    let agent_config = to_llm_agent_config(&cfg, &registry, &ctx)
        .await
        .map_err(|e| ConfigError::wrap("failed to create LLM agent config", e))?;

    llm_agent::new(agent_config).map_err(ConfigError::other)
}

/// Parses and builds a loop agent: `failed to parse Loop agent config: {cause}`,
/// `'name' is required`, `failed to create Loop agent config: {cause}`, then [`loop_agent::new`].
pub(crate) async fn new_loop_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_dir: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    let mut cfg: LoopAgentYamlConfig = config::unmarshal(&data)
        .map_err(|e| ConfigError::wrap("failed to parse Loop agent config", e))?;

    if cfg.name.is_empty() {
        return Err(ConfigError::msg("'name' is required"));
    }

    cfg.config_path = config_dir;

    let agent_config = to_loop_agent_config(&cfg, &registry, &ctx)
        .await
        .map_err(|e| ConfigError::wrap("failed to create Loop agent config", e))?;

    loop_agent::new(agent_config).map_err(ConfigError::other)
}

/// Parses and builds a parallel agent: `failed to parse Parallel agent config: {cause}`,
/// `'name' is required`, `failed to create Parallel agent config: {cause}`, then
/// [`parallel_agent::new`].
pub(crate) async fn new_parallel_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_dir: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    let mut cfg: ParallelAgentYamlConfig = config::unmarshal(&data)
        .map_err(|e| ConfigError::wrap("failed to parse Parallel agent config", e))?;

    if cfg.name.is_empty() {
        return Err(ConfigError::msg("'name' is required"));
    }

    cfg.config_path = config_dir;

    let agent_config = to_parallel_agent_config(&cfg, &registry, &ctx)
        .await
        .map_err(|e| ConfigError::wrap("failed to create Parallel agent config", e))?;

    parallel_agent::new(agent_config).map_err(ConfigError::other)
}

/// Parses and builds a sequential agent: `failed to parse Sequential agent config: {cause}`,
/// `'name' is required`, `failed to create Sequential agent config: {cause}`, then
/// [`sequential_agent::new`].
pub(crate) async fn new_sequential_agent(
    registry: Registry,
    ctx: LoadCtx,
    data: Vec<u8>,
    config_dir: PathBuf,
) -> Result<Arc<dyn Agent>, ConfigError> {
    let mut cfg: SequentialAgentYamlConfig = config::unmarshal(&data)
        .map_err(|e| ConfigError::wrap("failed to parse Sequential agent config", e))?;

    if cfg.name.is_empty() {
        return Err(ConfigError::msg("'name' is required"));
    }

    cfg.config_path = config_dir;

    let agent_config = to_sequential_agent_config(&cfg, &registry, &ctx)
        .await
        .map_err(|e| ConfigError::wrap("failed to create Sequential agent config", e))?;

    sequential_agent::new(agent_config).map_err(ConfigError::other)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use futures::FutureExt;
    use gadk_platform::env::MapEnv;

    use super::*;
    use crate::tests::{
        CfgErr, Seen, ToolSeen, noop_before, recording_factory, temp_dir, test_registry,
        test_tool_factory, test_toolset_factory, write,
    };
    use crate::{Callback, GoMap, ToolFactory};

    const LLM: &str = "name: root\nmodel: gemini-2.0-flash\n";

    async fn load(r: &Registry, path: &Path) -> Result<Arc<dyn Agent>, ConfigError> {
        r.from_config(path).await
    }

    #[tokio::test]
    async fn llm_agent_required_fields_and_parse_errors() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        for (yaml, want) in [
            ("", "'name' is required"),
            ("model: gemini-2.0-flash\n", "'name' is required"),
            ("name: a\n", "'model' is required for LlmAgent"),
            (
                "name: a\nmodel: gpt-4o\n",
                "failed to create LLM agent config: model gpt-4o is not supported",
            ),
            (
                "name: a\nmodel: [x]\n",
                "failed to parse LLM agent config: yaml: unmarshal errors:\n  line 2: cannot unmarshal !!seq into String",
            ),
        ] {
            let p = write(&base, "a.yaml", yaml);
            assert_eq!(load(&r, &p).await.cfg_err().to_string(), want, "{yaml:?}");
        }
    }

    #[tokio::test]
    async fn llm_agent_builds_offline_with_a_fake_key() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        let p = write(
            &base,
            "root.yaml",
            "name: root\ndescription: d\nmodel: models/gemini-2.0-flash\ninstruction: be brief\n\
             disallow_transfer_to_peers: true\ngenerate_content_config:\n  temperature: 0.5\n",
        );
        let a = load(&r, &p).await.unwrap();
        assert_eq!((a.name(), a.description()), ("root", "d"));

        let mut cfg: LlmAgentYamlConfig = config::unmarshal(&std::fs::read(&p).unwrap()).unwrap();
        cfg.config_path = p.clone();
        let c = to_llm_agent_config(&cfg, &r, &LoadCtx::default())
            .await
            .unwrap();
        assert_eq!(c.name, "root");
        assert_eq!(c.instruction, "be brief");
        assert!(c.disallow_transfer_to_peers && !c.disallow_transfer_to_parent);
        assert_eq!(c.model.unwrap().name(), "models/gemini-2.0-flash");
        assert_eq!(c.generate_content_config.unwrap().temperature, Some(0.5));
        assert!(c.tools.is_empty() && c.toolsets.is_empty() && c.sub_agents.is_empty());
    }

    #[tokio::test]
    async fn llm_agent_model_creation_failure_is_wrapped() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        r.set_env(Arc::new(MapEnv(Default::default())));
        let p = write(&base, "root.yaml", LLM);
        assert_eq!(
            load(&r, &p).await.cfg_err().to_string(),
            "failed to create LLM agent config: failed to create model: api key is required for Google AI backend.\nYou can get the API key from https://ai.google.dev/gemini-api/docs/api-key"
        );
    }

    #[tokio::test]
    async fn workflow_agents_require_a_name_and_wrap_parse_errors() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        for class in ["LoopAgent", "ParallelAgent", "SequentialAgent"] {
            let p = write(&base, "a.yaml", &format!("agent_class: {class}\n"));
            assert_eq!(
                load(&r, &p).await.cfg_err().to_string(),
                "'name' is required"
            );
        }
        // Every field of these three configs is a base field, so a type error is caught by
        // FromConfig's base peek first (`invalid YAML content: …`); the factories' own parse
        // errors are reached by calling them directly. serde exposes no element type, so
        // the target is `a sequence`.
        let p = write(
            &base,
            "a.yaml",
            "agent_class: LoopAgent\nname: a\nsub_agents: x\n",
        );
        let elem_err =
            "yaml: unmarshal errors:\n  line 3: cannot unmarshal !!str `x` into a sequence";
        assert_eq!(
            load(&r, &p).await.cfg_err().to_string(),
            format!("invalid YAML content: {elem_err}")
        );
        let data = b"name: a\nagent_class: x\nsub_agents: x\n".to_vec();
        let ctx = LoadCtx::default();
        let path = base.join("a.yaml");
        assert_eq!(
            new_loop_agent(r.clone(), ctx.clone(), data.clone(), path.clone())
                .await
                .cfg_err()
                .to_string(),
            format!("failed to parse Loop agent config: {elem_err}")
        );
        assert_eq!(
            new_parallel_agent(r.clone(), ctx.clone(), data.clone(), path.clone())
                .await
                .cfg_err()
                .to_string(),
            format!("failed to parse Parallel agent config: {elem_err}")
        );
        assert_eq!(
            new_sequential_agent(r.clone(), ctx.clone(), data, path)
                .await
                .cfg_err()
                .to_string(),
            format!("failed to parse Sequential agent config: {elem_err}")
        );
        // A u64 field is named `u64`.
        let p = write(
            &base,
            "a.yaml",
            "agent_class: LoopAgent\nname: a\nmax_iterations: -1\n",
        );
        assert_eq!(
            load(&r, &p).await.cfg_err().to_string(),
            "failed to parse Loop agent config: yaml: unmarshal errors:\n  line 3: cannot unmarshal !!int `-1` into u64"
        );
    }

    #[tokio::test]
    async fn workflow_agents_resolve_sub_agents_in_order() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        write(&base, "s1.yaml", "agent_class: SequentialAgent\nname: s1\n");
        write(
            &base,
            "sub/p1.yaml",
            "agent_class: ParallelAgent\nname: p1\n",
        );
        let p = write(
            &base,
            "root.yaml",
            "agent_class: LoopAgent\nname: root\ndescription: loop\nmax_iterations: 3\n\
             sub_agents:\n  - config_path: s1.yaml\n  - {}\n  - config_path: sub/p1.yaml\n",
        );
        let a = load(&r, &p).await.unwrap();
        assert_eq!(a.description(), "loop");
        let names: Vec<&str> = a.sub_agents().iter().map(|s| s.name()).collect();
        assert_eq!(names, ["s1", "p1"]);
        let cfg = a
            .agent_state()
            .config
            .as_ref()
            .unwrap()
            .downcast_ref::<LoopAgentConfig>()
            .unwrap();
        assert_eq!(cfg.max_iterations, 3);
        // The sub-agents are cached by absolute path.
        assert!(Arc::ptr_eq(
            &a.sub_agents()[0],
            &r.resolve_agent_reference(&base, "s1.yaml").await.unwrap()
        ));
    }

    #[tokio::test]
    async fn loop_agent_max_iterations_above_u32_saturates() {
        let r = test_registry();
        let cfg = LoopAgentYamlConfig {
            name: "l".into(),
            max_iterations: 5_000_000_000,
            ..Default::default()
        };
        let c = to_loop_agent_config(&cfg, &r, &LoadCtx::default())
            .await
            .unwrap();
        assert_eq!(c.max_iterations, u32::MAX);
        let cfg = LoopAgentYamlConfig {
            max_iterations: u64::from(u32::MAX),
            ..cfg
        };
        let c = to_loop_agent_config(&cfg, &r, &LoadCtx::default())
            .await
            .unwrap();
        assert_eq!(c.max_iterations, u32::MAX);
    }

    #[tokio::test]
    async fn sub_agent_failures_have_go_wrapping_chains() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        write(&base, "noname.yaml", "model: gemini-2.0-flash\n");
        let cases = [
            (
                "agent_class: SequentialAgent\nname: r\nsub_agents:\n  - config_path: missing.yaml\n",
                format!(
                    "failed to create Sequential agent config: failed to resolve agent reference missing.yaml: config file not found: {}",
                    base.join("missing.yaml").display()
                ),
            ),
            (
                "agent_class: ParallelAgent\nname: r\nsub_agents:\n  - config_path: ../x.yaml\n",
                "failed to create Parallel agent config: failed to resolve agent reference ../x.yaml: config reference \"../x.yaml\": path is not a relative path inside the base directory: rel path is not local".to_owned(),
            ),
            (
                "agent_class: LoopAgent\nname: r\nsub_agents:\n  - code: pkg.Agent\n",
                "failed to create Loop agent config: inline code agent references are not yet supported for pkg.Agent".to_owned(),
            ),
            (
                "name: r\nmodel: gemini-2.0-flash\nsub_agents:\n  - config_path: noname.yaml\n",
                "failed to create LLM agent config: failed to resolve agent reference noname.yaml: 'name' is required".to_owned(),
            ),
            (
                "name: r\nmodel: gemini-2.0-flash\nsub_agents:\n  - config_path: /abs.yaml\n",
                "failed to create LLM agent config: failed to resolve agent reference /abs.yaml: config reference \"/abs.yaml\": path is not a relative path inside the base directory: rel path is not local".to_owned(),
            ),
        ];
        for (yaml, want) in cases {
            let p = write(&base, "root.yaml", yaml);
            assert_eq!(load(&r, &p).await.cfg_err().to_string(), want, "{yaml}");
        }
    }

    #[tokio::test]
    async fn agent_constructor_errors_are_returned_unwrapped() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        write(&base, "s.yaml", "agent_class: SequentialAgent\nname: s\n");
        // The cached sub-agent appears twice.
        let p = write(
            &base,
            "root.yaml",
            "name: r\nmodel: gemini-2.0-flash\nsub_agents:\n  - config_path: s.yaml\n  - config_path: ./s.yaml\n",
        );
        assert_eq!(
            load(&r, &p).await.cfg_err().to_string(),
            "failed to create agent: error creating agent: subagent \"s\" appears multiple times in subAgents"
        );
        let p = write(
            &base,
            "root.yaml",
            "agent_class: LoopAgent\nname: r\nsub_agents:\n  - config_path: s.yaml\n  - config_path: s.yaml\n",
        );
        assert_eq!(
            load(&r, &p).await.cfg_err().to_string(),
            "failed to create base agent: error creating agent: subagent \"s\" appears multiple times in subAgents"
        );
    }

    #[tokio::test]
    async fn llm_agent_resolves_tools_and_toolsets_with_parent_path() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        let seen = ToolSeen::default();
        r.register_tool_factory("my_tool", test_tool_factory(seen.clone()))
            .unwrap();
        r.register_toolset_factory("my_set", test_toolset_factory("set", seen.clone()))
            .unwrap();
        let p = write(
            &base,
            "root.yaml",
            "name: root\nmodel: gemini-2.0-flash\ntools:\n  - name: my_set\n    args: {}\n\
             \x20 - name: ''\n  - name: my_tool\n    args:\n      k: 1\n  - name: my_tool\n    args:\n",
        );
        let mut cfg: LlmAgentYamlConfig = config::unmarshal(&std::fs::read(&p).unwrap()).unwrap();
        cfg.config_path = p.clone();
        let c = to_llm_agent_config(&cfg, &r, &LoadCtx::default())
            .await
            .unwrap();
        assert_eq!(c.tools.len(), 2);
        assert_eq!(c.toolsets.len(), 1);
        assert_eq!(c.toolsets[0].name(), "set");
        let mut k1 = GoMap::new();
        k1.insert("k".into(), crate::GoValue::Int(1));
        assert_eq!(
            *seen.lock(),
            [
                (Some(p.clone()), Some(GoMap::new())),
                (Some(p.clone()), Some(k1)),
                (Some(p.clone()), None),
            ]
        );
        load(&r, &p).await.unwrap();
    }

    #[tokio::test]
    async fn tool_failures_have_go_wrapping_chains() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        let failing: ToolFactory =
            Arc::new(|_r, _c, _a| async { Err(ConfigError::msg("args is nil")) }.boxed());
        r.register_tool_factory("bad", failing).unwrap();
        for (tool, want) in [
            (
                "nope",
                "failed to create LLM agent config: failed to resolve tool reference nope: tool 'nope' not found",
            ),
            (
                "bad",
                "failed to create LLM agent config: failed to resolve tool reference bad: args is nil",
            ),
        ] {
            let p = write(
                &base,
                "root.yaml",
                &format!("{LLM}tools:\n  - name: {tool}\n"),
            );
            assert_eq!(load(&r, &p).await.cfg_err().to_string(), want);
        }
    }

    #[tokio::test]
    async fn parent_path_is_inherited_by_nested_loads() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        let seen = Seen::default();
        r.register("Test", recording_factory(seen.clone())).unwrap();
        // A tool factory that loads an agent relative to the context's parent path, as Go's
        // AgentTool does; the nested factory sees the same parent path.
        let loader: ToolFactory = Arc::new(|reg, ctx, _a| {
            async move {
                let parent = ctx.parent_path.clone().unwrap();
                reg.resolve_agent_reference_with_ctx(&ctx, &parent, "helper.yaml")
                    .await?;
                Ok(gadk_engine::tool::exit_loop())
            }
            .boxed()
        });
        r.register_tool_factory("loader", loader).unwrap();
        write(&base, "helper.yaml", "agent_class: Test\nname: helper\n");
        write(&base, "direct.yaml", "agent_class: Test\nname: direct\n");
        let p = write(
            &base,
            "root.yaml",
            &format!("{LLM}sub_agents:\n  - config_path: direct.yaml\ntools:\n  - name: loader\n"),
        );
        load(&r, &p).await.unwrap();
        let seen = seen.lock();
        // Sub-agents are resolved with the caller's context (no parent path at top level);
        // the tool's nested load inherits the parent path set by resolveTools: the config
        // directory (Go 0990f9e), which is also what each factory receives.
        assert_eq!(seen[0].0, None);
        assert_eq!(
            (seen[0].1.as_slice(), &seen[0].2),
            (&b"agent_class: Test\nname: direct\n"[..], &base)
        );
        assert_eq!(seen[1].0.as_deref(), Some(base.as_path()));
        assert_eq!(
            (seen[1].1.as_slice(), &seen[1].2),
            (&b"agent_class: Test\nname: helper\n"[..], &base)
        );
    }

    #[tokio::test]
    async fn callbacks_resolve_with_go_type_assertions() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        r.register_callback("b1", Callback::BeforeAgent(noop_before()))
            .unwrap();
        r.register_callback("b2", Callback::BeforeAgent(noop_before()))
            .unwrap();
        r.register_callback("a1", Callback::AfterAgent(noop_before()))
            .unwrap();
        let p = write(
            &base,
            "root.yaml",
            &format!(
                "{LLM}before_agent_callbacks:\n  - name: b1\n  - name: ''\n  - name: b2\nafter_agent_callbacks:\n  - name: a1\n"
            ),
        );
        let mut cfg: LlmAgentYamlConfig = config::unmarshal(&std::fs::read(&p).unwrap()).unwrap();
        cfg.config_path = p.clone();
        let c = to_llm_agent_config(&cfg, &r, &LoadCtx::default())
            .await
            .unwrap();
        assert_eq!(c.before_agent_callbacks.len(), 2);
        assert_eq!(c.after_agent_callbacks.len(), 1);

        for (section, name, want) in [
            (
                "before_agent_callbacks",
                "missing",
                "failed to create LLM agent config: failed to resolve callback reference missing: callback 'missing' not found",
            ),
            (
                "before_agent_callbacks",
                "a1",
                "failed to create LLM agent config: callback a1 is of type AfterAgentCallback and not BeforeAgentCallback",
            ),
            (
                "after_agent_callbacks",
                "b1",
                "failed to create LLM agent config: callback b1 is of type BeforeAgentCallback and not AfterAgentCallback",
            ),
        ] {
            let p = write(
                &base,
                "root.yaml",
                &format!("{LLM}{section}:\n  - name: {name}\n"),
            );
            assert_eq!(load(&r, &p).await.cfg_err().to_string(), want);
        }
    }

    #[test]
    fn resolve_callbacks_direct() {
        let r = Registry::empty();
        r.register_callback("a", Callback::AfterAgent(noop_before()))
            .unwrap();
        let refs = [CodeConfig {
            name: "a".into(),
            params: None,
        }];
        assert_eq!(resolve_callbacks::<AfterAgent>(&r, &refs).unwrap().len(), 1);
        match resolve_callbacks::<BeforeAgent>(&r, &refs) {
            Ok(_) => panic!("expected an error"),
            Err(e) => assert_eq!(
                e.to_string(),
                "callback a is of type AfterAgentCallback and not BeforeAgentCallback"
            ),
        }
        assert!(
            resolve_callbacks::<BeforeAgent>(&r, &[])
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn loop_parallel_sequential_ignore_callbacks_like_go() {
        let (_d, base) = temp_dir();
        let r = test_registry();
        let p = write(
            &base,
            "root.yaml",
            "agent_class: SequentialAgent\nname: s\nbefore_agent_callbacks:\n  - name: missing\n",
        );
        assert_eq!(load(&r, &p).await.unwrap().name(), "s");
    }
}
