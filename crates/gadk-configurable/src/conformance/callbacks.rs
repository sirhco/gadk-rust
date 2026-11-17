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

//! The conformance agents' before/after agent callbacks, registered under
//! `callback_agent_00N.callbacks.*`.

use std::sync::Arc;

use futures::FutureExt;
use gadk_agent::{AgentError, Context};
use gadk_genai::{Content, Part};
use gadk_platform::go_type_name;
use gadk_session::SessionError;
use serde_json::Value;

use crate::{Callback, ConfigError, Registry};

/// Sets `before_agent_callback_state_key` to `value1`.
pub(crate) async fn before_agent_callback1(ctx: Context) -> Result<Option<Content>, AgentError> {
    ctx.state()
        .set("before_agent_callback_state_key", Value::from("value1"))?;
    Ok(None)
}

/// Appends `+value2` to `before_agent_callback_state_key`
/// (`state value for 'before_agent_callback_state_key' is not a string, but {type}`).
pub(crate) async fn before_agent_callback2(ctx: Context) -> Result<Option<Content>, AgentError> {
    append_value2(&ctx, "before_agent_callback_state_key")
}

/// Sets `conversation_limit_reached` to `True` on the first call, then answers `Sorry, you
/// have reached the limit of the conversation.` as the model.
pub(crate) async fn shortcut_agent_execution(ctx: Context) -> Result<Option<Content>, AgentError> {
    let val = match ctx.state().get("conversation_limit_reached") {
        Ok(val) => val,
        Err(SessionError::StateKeyNotExist) => {
            ctx.state()
                .set("conversation_limit_reached", Value::from("True"))?;
            return Ok(None);
        }
        Err(err) => return Err(err.into()),
    };
    if val.as_str() == Some("True") {
        return Ok(Some(Content {
            parts: vec![Part {
                text: "Sorry, you have reached the limit of the conversation.".to_owned(),
                ..Default::default()
            }],
            role: "model".to_owned(),
        }));
    }
    Ok(None)
}

/// Sets `after_agent_callback_state_key` to `value1`.
pub(crate) async fn after_agent_callback1(ctx: Context) -> Result<Option<Content>, AgentError> {
    ctx.state()
        .set("after_agent_callback_state_key", Value::from("value1"))?;
    Ok(None)
}

/// Appends `+value2` to `after_agent_callback_state_key`.
pub(crate) async fn after_agent_callback2(ctx: Context) -> Result<Option<Content>, AgentError> {
    append_value2(&ctx, "after_agent_callback_state_key")
}

/// The shared body of `before_agent_callback2` and `after_agent_callback2` (they differ only
/// in the key): reads the state value (errors returned as is), checks it is a string (the
/// error names its type), and stores it with `+value2` appended.
fn append_value2(ctx: &Context, key: &str) -> Result<Option<Content>, AgentError> {
    let val = ctx.state().get(key)?;
    let Some(s) = val.as_str() else {
        return Err(AgentError::other(format!(
            "state value for '{key}' is not a string, but {}",
            go_type_name(&val)
        )));
    };
    ctx.state().set(key, Value::from(format!("{s}+value2")))?;
    Ok(None)
}

/// Registers the five callbacks under their conformance names
/// (`error registering before agent callback 1: {cause}`, …); stops at the first failure.
pub fn register_callbacks(registry: &Registry) -> Result<(), ConfigError> {
    let callbacks = [
        (
            "callback_agent_001.callbacks.before_agent_callback1",
            Callback::BeforeAgent(Arc::new(|ctx| before_agent_callback1(ctx).boxed())),
            "error registering before agent callback 1",
        ),
        (
            "callback_agent_001.callbacks.before_agent_callback2",
            Callback::BeforeAgent(Arc::new(|ctx| before_agent_callback2(ctx).boxed())),
            "error registering before agent callback 2",
        ),
        (
            "callback_agent_002.callbacks.shortcut_agent_execution",
            Callback::BeforeAgent(Arc::new(|ctx| shortcut_agent_execution(ctx).boxed())),
            "error registering shortcut agent execution",
        ),
        (
            "callback_agent_003.callbacks.after_agent_callback1",
            Callback::AfterAgent(Arc::new(|ctx| after_agent_callback1(ctx).boxed())),
            "error registering after agent callback 1",
        ),
        (
            "callback_agent_003.callbacks.after_agent_callback2",
            Callback::AfterAgent(Arc::new(|ctx| after_agent_callback2(ctx).boxed())),
            "error registering after agent callback 2",
        ),
    ];
    for (name, callback, context) in callbacks {
        registry
            .register_callback(name, callback)
            .map_err(|e| ConfigError::wrap(context, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use gadk_agent::InvocationContext;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::tests::CfgErr;

    fn ctx() -> Context {
        InvocationContext::builder().build()
    }

    fn get(ctx: &Context, key: &str) -> Option<Value> {
        ctx.state().get(key).ok()
    }

    #[tokio::test]
    async fn before_agent_callbacks_chain_values() {
        let ctx = ctx();
        assert!(before_agent_callback1(ctx.clone()).await.unwrap().is_none());
        assert_eq!(
            get(&ctx, "before_agent_callback_state_key"),
            Some(json!("value1"))
        );
        assert!(before_agent_callback2(ctx.clone()).await.unwrap().is_none());
        assert_eq!(
            get(&ctx, "before_agent_callback_state_key"),
            Some(json!("value1+value2"))
        );
    }

    #[tokio::test]
    async fn after_agent_callbacks_chain_values() {
        let ctx = ctx();
        assert!(after_agent_callback1(ctx.clone()).await.unwrap().is_none());
        assert_eq!(
            get(&ctx, "after_agent_callback_state_key"),
            Some(json!("value1"))
        );
        assert!(after_agent_callback2(ctx.clone()).await.unwrap().is_none());
        assert!(after_agent_callback2(ctx.clone()).await.unwrap().is_none());
        assert_eq!(
            get(&ctx, "after_agent_callback_state_key"),
            Some(json!("value1+value2+value2"))
        );
    }

    #[tokio::test]
    async fn callback2_without_the_key_returns_the_state_error() {
        let err = before_agent_callback2(ctx()).await.unwrap_err();
        assert!(
            matches!(err, AgentError::Session(SessionError::StateKeyNotExist)),
            "{err}"
        );
        assert_eq!(err.to_string(), "state key does not exist");
        let err = after_agent_callback2(ctx()).await.unwrap_err();
        assert_eq!(err.to_string(), "state key does not exist");
    }

    #[tokio::test]
    async fn callback2_with_a_non_string_names_the_go_type() {
        for (value, go_type) in [
            (json!(1), "float64"),
            (json!(true), "bool"),
            (Value::Null, "<nil>"),
            (json!(["a"]), "[]interface {}"),
            (json!({ "a": 1 }), "map[string]interface {}"),
        ] {
            let ctx = ctx();
            ctx.state()
                .set("before_agent_callback_state_key", value.clone())
                .unwrap();
            ctx.state()
                .set("after_agent_callback_state_key", value.clone())
                .unwrap();
            assert_eq!(
                before_agent_callback2(ctx.clone())
                    .await
                    .unwrap_err()
                    .to_string(),
                format!(
                    "state value for 'before_agent_callback_state_key' is not a string, but \
                     {go_type}"
                )
            );
            assert_eq!(
                after_agent_callback2(ctx.clone())
                    .await
                    .unwrap_err()
                    .to_string(),
                format!(
                    "state value for 'after_agent_callback_state_key' is not a string, but \
                     {go_type}"
                )
            );
            // Unchanged.
            assert_eq!(get(&ctx, "before_agent_callback_state_key"), Some(value));
        }
    }

    #[tokio::test]
    async fn shortcut_sets_the_flag_then_answers_as_the_model() {
        let ctx = ctx();
        assert!(
            shortcut_agent_execution(ctx.clone())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(get(&ctx, "conversation_limit_reached"), Some(json!("True")));
        let content = shortcut_agent_execution(ctx.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            content,
            Content::from_text(
                "Sorry, you have reached the limit of the conversation.",
                "model"
            )
        );
        assert_eq!(
            serde_json::to_value(&content).unwrap(),
            json!({
                "parts": [{ "text": "Sorry, you have reached the limit of the conversation." }],
                "role": "model",
            })
        );
    }

    #[tokio::test]
    async fn shortcut_ignores_other_values() {
        for value in [json!("False"), json!("true"), json!(true), Value::Null] {
            let ctx = ctx();
            ctx.state()
                .set("conversation_limit_reached", value.clone())
                .unwrap();
            assert!(
                shortcut_agent_execution(ctx.clone())
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(get(&ctx, "conversation_limit_reached"), Some(value));
        }
    }

    #[tokio::test]
    async fn register_callbacks_registers_the_five_go_names() {
        let r = Registry::empty();
        register_callbacks(&r).unwrap();
        for (name, go_type) in [
            (
                "callback_agent_001.callbacks.before_agent_callback1",
                "agent.BeforeAgentCallback",
            ),
            (
                "callback_agent_001.callbacks.before_agent_callback2",
                "agent.BeforeAgentCallback",
            ),
            (
                "callback_agent_002.callbacks.shortcut_agent_execution",
                "agent.BeforeAgentCallback",
            ),
            (
                "callback_agent_003.callbacks.after_agent_callback1",
                "agent.AfterAgentCallback",
            ),
            (
                "callback_agent_003.callbacks.after_agent_callback2",
                "agent.AfterAgentCallback",
            ),
        ] {
            let cb = r.resolve_callback_reference(name).unwrap();
            assert_eq!(cb.go_type_name(), go_type, "{name}");
        }
        assert_eq!(r.inner.read().callbacks.len(), 5);

        // The registered functions are the ported ones.
        let ctx = ctx();
        let Callback::BeforeAgent(f) = r
            .resolve_callback_reference("callback_agent_002.callbacks.shortcut_agent_execution")
            .unwrap()
        else {
            panic!("want a before-agent callback");
        };
        assert!(f(ctx.clone()).await.unwrap().is_none());
        assert!(f(ctx).await.unwrap().is_some());
    }

    #[test]
    fn register_callbacks_twice_fails_with_go_text() {
        let r = Registry::empty();
        register_callbacks(&r).unwrap();
        assert_eq!(
            register_callbacks(&r).cfg_err().to_string(),
            "error registering before agent callback 1: RegisterCallback called twice for \
             callback callback_agent_001.callbacks.before_agent_callback1"
        );
    }

    #[test]
    fn register_callbacks_reports_the_failing_registration() {
        let r = Registry::empty();
        r.register_callback(
            "callback_agent_003.callbacks.after_agent_callback1",
            Callback::AfterAgent(Arc::new(|ctx| after_agent_callback2(ctx).boxed())),
        )
        .unwrap();
        assert_eq!(
            register_callbacks(&r).cfg_err().to_string(),
            "error registering after agent callback 1: RegisterCallback called twice for \
             callback callback_agent_003.callbacks.after_agent_callback1"
        );
        // Go stops at the first failure: the three before it are in, the last one is not.
        let g = r.inner.read();
        assert_eq!(g.callbacks.len(), 4);
        assert!(
            !g.callbacks
                .contains_key("callback_agent_003.callbacks.after_agent_callback2")
        );
    }
}
