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

//! Port of Go plugin/functioncallmodifier: a plugin that adds configured arguments to selected
//! tools' declarations before each model call. After the call it moves those arguments out of
//! the model's function calls into session state under `<function_call_id>/<arg>`, so the tools
//! never see them.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::future::{BoxFuture, ready};
use gadk_agent::{
    AfterModelCallback, AgentError, BeforeModelCallback, Context, Plugin, PluginConfig,
};
use gadk_genai::{Schema, Type};
use gadk_model::{LlmRequest, LlmResponse, ModelError};

use crate::llm::flow::model_error_into_agent;
use crate::llm::processors::wrap_err;

const NAME: &str = "FunctionCallModifierPlugin";

type RequestResult = BoxFuture<'static, (LlmRequest, Result<Option<LlmResponse>, AgentError>)>;
type ResponseResult = BoxFuture<'static, (LlmResponse, Result<Option<LlmResponse>, AgentError>)>;

/// Go `functioncallmodifier.FunctionCallModifierConfig`.
#[derive(Clone)]
pub struct FunctionCallModifierConfig {
    /// Go `Predicate`: whether the named tool gets the extra arguments.
    pub predicate: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// Go `Args`: the extra arguments by name. They are added to each selected declaration's
    /// typed `parameters` and moved from its function calls into session state.
    pub args: BTreeMap<String, Schema>,
    /// Go `OverrideDescription`: maps a selected declaration's description to its new one.
    #[allow(clippy::type_complexity)] // the exact Appendix C8 field type
    pub override_description: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
}

/// Port of Go `functioncallmodifier.NewPlugin` (which never fails): a plugin named
/// `FunctionCallModifierPlugin` with a before-model and an after-model callback.
pub fn new_function_call_modifier_plugin(cfg: FunctionCallModifierConfig) -> Plugin {
    let cfg = Arc::new(cfg);
    let before_model: BeforeModelCallback = {
        let cfg = cfg.clone();
        Arc::new(move |_ctx: Context, mut req: LlmRequest| -> RequestResult {
            add_args(&cfg, &mut req);
            Box::pin(ready((req, Ok(None))))
        })
    };
    let after_model: AfterModelCallback = Arc::new(
        move |ctx: Context, resp: LlmResponse, err: Option<Arc<ModelError>>| -> ResponseResult {
            Box::pin(ready(strip_args(&cfg, &ctx, resp, err)))
        },
    );
    Plugin::new(PluginConfig {
        name: NAME.into(),
        before_model_callback: Some(before_model),
        after_model_callback: Some(after_model),
        ..Default::default()
    })
}

/// Port of plugin.go `beforeModelCallback`. Each declaration that is packed in `req.tools` and
/// passes the predicate gets typed `OBJECT` parameters if it has none, gets `cfg.args` copied
/// into its properties (overwriting, never touching `required`), and has its description
/// overridden.
fn add_args(cfg: &FunctionCallModifierConfig, req: &mut LlmRequest) {
    let LlmRequest { config, tools, .. } = req;
    let Some(config) = config.as_mut() else {
        return;
    };
    for tool in &mut config.tools {
        for decl in &mut tool.function_declarations {
            if !tools.contains_key(decl.name.as_str()) || !(cfg.predicate)(decl.name.as_str()) {
                continue;
            }
            let params = decl.parameters.get_or_insert_with(|| Schema {
                r#type: Some(Type::Object),
                ..Default::default()
            });
            params
                .properties
                .extend(cfg.args.iter().map(|(k, v)| (k.clone(), v.clone())));
            if let Some(override_description) = &cfg.override_description {
                decl.description = override_description(decl.description.as_str());
            }
        }
    }
}

/// Port of plugin.go `afterModelCallback`.
fn strip_args(
    cfg: &FunctionCallModifierConfig,
    ctx: &Context,
    mut resp: LlmResponse,
    err: Option<Arc<ModelError>>,
) -> (LlmResponse, Result<Option<LlmResponse>, AgentError>) {
    if let Some(err) = err {
        // Go: return nil, llmResponseError (pass through). The flow never reaches this: it
        // passes `None` here and routes model errors to the on-model-error callbacks. A caller
        // going through the PluginManager keeps its own clone of the `Arc`, so the error is
        // shared and surfaces as `AgentError::Other` (same text); only the `Arc`'s sole owner
        // gets `AgentError::Model` back.
        return (resp, Err(model_error_into_agent(err)));
    }
    if resp.content.as_ref().is_none_or(|c| c.parts.is_empty()) {
        // Go: return llmResponse, nil. That is a non-nil response, which ends Go's after-model
        // chain (plugin_manager.go RunAfterModelCallback).
        let same = resp.clone();
        return (resp, Ok(Some(same)));
    }

    let state = ctx.state();
    let mut failure = None;
    if let Some(content) = resp.content.as_mut() {
        'parts: for part in &mut content.parts {
            let Some(fc) = part.function_call.as_mut() else {
                continue;
            };
            if !(cfg.predicate)(fc.name.as_str()) {
                continue;
            }
            for name in cfg.args.keys() {
                let Some(arg) = fc.args.remove(name) else {
                    continue;
                };
                let state_key = format!("{}/{}", fc.id, name);
                if let Err(e) = state.set(&state_key, arg) {
                    failure = Some(wrap_err("failed to set state", e));
                    break 'parts;
                }
            }
        }
    }
    match failure {
        // Go: return nil, fmt.Errorf("failed to set state: %w", err).
        Some(e) => (resp, Err(e)),
        // Go: return nil, nil after editing *llmResponse in place.
        None => (resp, Ok(None)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use gadk_genai::{Content, FunctionCall, Part};
    use serde_json::{Map, json};

    use super::*;

    fn cfg() -> FunctionCallModifierConfig {
        FunctionCallModifierConfig {
            predicate: Arc::new(|_: &str| true),
            args: BTreeMap::from([("skill_id".to_string(), Schema::default())]),
            override_description: None,
        }
    }

    #[test]
    fn plugin_is_named_like_go() {
        let p = new_function_call_modifier_plugin(cfg());
        assert_eq!(p.name(), "FunctionCallModifierPlugin");
        assert!(p.before_model_callback().is_some());
        assert!(p.after_model_callback().is_some());
    }

    // Called directly with the sole `Arc`, so the error comes back as `AgentError::Model`.
    #[tokio::test]
    async fn after_model_passes_a_model_error_through() {
        let cb = new_function_call_modifier_plugin(cfg())
            .after_model_callback()
            .unwrap();
        let (_, res) = cb(
            Context::builder().build(),
            LlmResponse::default(),
            Some(Arc::new(ModelError::EmptyResponse)),
        )
        .await;
        let err = res.unwrap_err();
        assert!(
            matches!(err, AgentError::Model(ModelError::EmptyResponse)),
            "{err:?}"
        );
        assert_eq!(err.to_string(), "empty response");
    }

    // Go returns the response itself when it has no parts, which ends the after-model chain.
    #[tokio::test]
    async fn after_model_returns_a_partless_response_as_the_replacement() {
        let cb = new_function_call_modifier_plugin(cfg())
            .after_model_callback()
            .unwrap();
        let (resp, res) = cb(Context::builder().build(), LlmResponse::default(), None).await;
        assert_eq!(res.unwrap(), Some(resp));
    }

    #[tokio::test]
    async fn after_model_hands_back_the_stripped_response_with_ok_none() {
        let cb = new_function_call_modifier_plugin(cfg())
            .after_model_callback()
            .unwrap();
        let resp = LlmResponse {
            content: Some(Content {
                parts: vec![Part {
                    function_call: Some(FunctionCall {
                        id: "id1".into(),
                        name: "t".into(),
                        args: json!({"skill_id": "s", "x": 1})
                            .as_object()
                            .cloned()
                            .unwrap(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let ctx = Context::builder().build();
        let (got, res) = cb(ctx.clone(), resp, None).await;
        assert!(matches!(res, Ok(None)), "{res:?}");
        let fc = got.content.unwrap().parts[0].function_call.clone().unwrap();
        let want: Map<String, serde_json::Value> = json!({"x": 1}).as_object().cloned().unwrap();
        assert_eq!(fc.args, want);
        assert_eq!(ctx.state().get("id1/skill_id").unwrap(), json!("s"));
    }

    #[test]
    fn before_model_adds_object_parameters_only_to_packed_selected_declarations() {
        use gadk_genai::{FunctionDeclaration, GenerateContentConfig, Tool as GenaiTool};
        let mut req = LlmRequest {
            config: Some(GenerateContentConfig {
                tools: vec![GenaiTool {
                    function_declarations: vec![
                        FunctionDeclaration {
                            name: "packed".into(),
                            ..Default::default()
                        },
                        FunctionDeclaration {
                            name: "unpacked".into(),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        req.tools.insert(
            "packed".into(),
            Arc::new(()) as Arc<dyn std::any::Any + Send + Sync>,
        );
        add_args(&cfg(), &mut req);
        let decls = &req.config.as_ref().unwrap().tools[0].function_declarations;
        let p = decls[0].parameters.as_ref().unwrap();
        assert_eq!(p.r#type, Some(Type::Object));
        assert!(p.properties.contains_key("skill_id"));
        assert!(p.required.is_empty());
        assert!(decls[1].parameters.is_none());
    }
}
