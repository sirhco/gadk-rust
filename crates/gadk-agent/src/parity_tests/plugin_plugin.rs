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

use std::sync::Arc;

use futures::FutureExt;

use crate::error::AgentError;
use crate::plugin::{Plugin, PluginConfig};

// parity: adapted: Go's Plugin.Close() method becomes the close() accessor (always Some; Plugin::new installs a no-op) whose future is awaited
#[tokio::test]
async fn test_new() {
    // Successfully maps all fields; default close is Ok
    let p = Plugin::new(PluginConfig {
        on_user_message_callback: Some(Arc::new(|_ctx, _msg| async { Ok(None) }.boxed())),
        ..Default::default()
    });
    assert!(p.on_user_message_callback().is_some());
    assert!((p.close().unwrap())().await.is_ok());
    // Handles a missing close function gracefully
    let p = Plugin::new(PluginConfig::default());
    assert!((p.close().unwrap())().await.is_ok());
    // Executes the provided close function
    let p = Plugin::new(PluginConfig {
        close: Some(Arc::new(|| {
            async { Err(AgentError::other("close error")) }.boxed()
        })),
        ..Default::default()
    });
    assert_eq!(
        (p.close().unwrap())().await.unwrap_err().to_string(),
        "close error"
    );
}

/// Go `RunAfterModelCallback` hands every callback the same `*model.LLMResponse`, so an in-place
/// edit made by a callback that returns `(nil, nil)` reaches the next callback. Rust threads the
/// handed-back response: plugin-0's edit reaches plugin-1, and the manager hands it back.
#[tokio::test]
async fn after_model_response_edit_reaches_the_next_plugin() {
    use gadk_genai::Content;
    use gadk_model::LlmResponse;

    use crate::context::Context;
    use crate::plugin_manager::PluginManager;

    let seen = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let text_of = |r: &LlmResponse| {
        r.content
            .as_ref()
            .map(|c| c.parts[0].text.clone())
            .unwrap_or_default()
    };
    let editor = Plugin::new(PluginConfig {
        name: "editor".into(),
        after_model_callback: Some(Arc::new(move |_ctx, mut resp: LlmResponse, _err| {
            async move {
                resp.content = Some(Content::from_text("edited", "model"));
                (resp, Ok(None))
            }
            .boxed()
        })),
        ..Default::default()
    });
    let observer = {
        let seen = seen.clone();
        Plugin::new(PluginConfig {
            name: "observer".into(),
            after_model_callback: Some(Arc::new(move |_ctx, resp: LlmResponse, _err| {
                seen.lock().push(text_of(&resp));
                async move { (resp, Ok(None)) }.boxed()
            })),
            ..Default::default()
        })
    };
    let pm = PluginManager::new(vec![editor, observer], std::time::Duration::from_secs(1)).unwrap();
    let resp = LlmResponse {
        content: Some(Content::from_text("original", "model")),
        ..Default::default()
    };
    let (handed_back, res) = pm
        .run_after_model(Context::builder().build(), resp, None)
        .await;
    assert!(matches!(res, Ok(None)), "{res:?}");
    assert_eq!(*seen.lock(), ["edited"]);
    assert_eq!(text_of(&handed_back), "edited");
}

/// A callback's `Some` replacement short-circuits the remaining callbacks; its handed-back
/// response is returned alongside the replacement.
#[tokio::test]
async fn after_model_replacement_short_circuits() {
    use gadk_genai::Content;
    use gadk_model::LlmResponse;

    use crate::context::Context;
    use crate::plugin_manager::PluginManager;

    let later_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let replacer = Plugin::new(PluginConfig {
        name: "replacer".into(),
        after_model_callback: Some(Arc::new(|_ctx, resp: LlmResponse, _err| {
            async move {
                let replacement = LlmResponse {
                    content: Some(Content::from_text("replacement", "model")),
                    ..Default::default()
                };
                (resp, Ok(Some(replacement)))
            }
            .boxed()
        })),
        ..Default::default()
    });
    let later = {
        let later_ran = later_ran.clone();
        Plugin::new(PluginConfig {
            name: "later".into(),
            after_model_callback: Some(Arc::new(move |_ctx, resp: LlmResponse, _err| {
                later_ran.store(true, std::sync::atomic::Ordering::SeqCst);
                async move { (resp, Ok(None)) }.boxed()
            })),
            ..Default::default()
        })
    };
    let pm = PluginManager::new(vec![replacer, later], std::time::Duration::from_secs(1)).unwrap();
    let (_, res) = pm
        .run_after_model(Context::builder().build(), LlmResponse::default(), None)
        .await;
    let got = res.unwrap().unwrap();
    assert_eq!(got.content.unwrap().parts[0].text, "replacement");
    assert!(!later_ran.load(std::sync::atomic::Ordering::SeqCst));
}
