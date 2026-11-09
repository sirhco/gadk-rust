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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! R16: `gadk::openai` (feature `openai`, not on by default) is `gadk_openai`; models built
//! through the façade are gadk `Llm`s that answer through a configured client (a scripted fake
//! here: no network).

use std::sync::Arc;

use futures::StreamExt;
use gadk::genai::{Content, ROLE_USER};
use gadk::model::rest::test_util::ScriptedClient;
use gadk::model::{HttpClient, Llm, LlmRequest, ModelError};
use gadk::openai::{
    Api, ApiError, ClientConfig, DEFAULT_BASE_URL, EnvProvider, ErrorKind, FINISH_MESSAGE_KEY,
    MapEnv, OpenAiError, OpenAiErrorKind, ProcessEnv, new_model,
};
use pretty_assertions::assert_eq;

/// Compiles only if each façade path names the `gadk_openai` item.
#[test]
fn facade_openai_paths_are_gadk_openai() {
    fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
    same::<Api>(None, None::<&gadk_openai::Api>);
    same::<ClientConfig>(None, None::<&gadk_openai::ClientConfig>);
    same::<OpenAiError>(None, None::<&gadk_openai::OpenAiError>);
    same::<ApiError>(None, None::<&gadk_openai::ApiError>);
    same::<ErrorKind>(None, None::<&gadk_openai::ErrorKind>);
    same::<OpenAiErrorKind>(None, None::<&gadk_openai::ErrorKind>);
    same::<dyn EnvProvider>(None, None::<&dyn gadk_openai::EnvProvider>);
    same::<MapEnv>(None, None::<&gadk_openai::MapEnv>);
    same::<ProcessEnv>(None, None::<&gadk_openai::ProcessEnv>);

    // Go's exported constants and API values.
    assert_eq!(FINISH_MESSAGE_KEY, "openai_finish_message");
    assert_eq!(DEFAULT_BASE_URL, "https://api.openai.com/v1/");
    assert_eq!(Api::default(), Api::Responses);
    assert_eq!(Api::Responses.to_string(), "responses");
    assert_eq!(
        "chat_completions".parse::<Api>().unwrap(),
        Api::ChatCompletions
    );
}

#[tokio::test]
async fn facade_openai_model_is_a_gadk_llm() {
    let bodies = [
        (
            Api::Responses,
            "/v1/responses",
            r#"{"id":"r","object":"response","status":"completed","output":[{"type":"message","id":"m","role":"assistant","status":"completed","content":[{"type":"output_text","text":"hi","annotations":[]}]}]}"#,
        ),
        (
            Api::ChatCompletions,
            "/v1/chat/completions",
            r#"{"id":"c","object":"chat.completion","model":"m","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hi"}}]}"#,
        ),
    ];
    for (api, path, body) in bodies {
        let client = ScriptedClient::json(body);
        let http_client: Arc<dyn HttpClient> = client.clone();
        let cfg = ClientConfig {
            api_key: "test-openai-key".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            api,
            http_client: Some(http_client),
            env: Arc::new(MapEnv::default()),
            ..ClientConfig::default()
        };
        let llm: Arc<dyn Llm> = new_model("gpt-4o-mini", cfg).unwrap();
        assert_eq!(llm.name(), "gpt-4o-mini", "{api}");

        let req = LlmRequest {
            contents: vec![Content::from_text("hi", ROLE_USER)],
            ..LlmRequest::default()
        };
        let got: Vec<_> = llm.generate_content(req, false).collect().await;
        let [Ok(resp)] = got.as_slice() else {
            panic!("{api}: GenerateContent() = {got:?}, want one response");
        };
        let content = resp.content.as_ref().expect("content");
        assert_eq!(content.parts[0].text, "hi", "{api}");

        let sent = client.last_request();
        assert_eq!(sent.path, path, "{api}");
        assert_eq!(
            sent.header("Authorization"),
            "Bearer test-openai-key",
            "{api}"
        );
    }
}

#[tokio::test]
async fn facade_openai_errors_carry_go_text() {
    // Go's validation text, through the façade.
    let cfg = || ClientConfig {
        api_key: "k".into(),
        env: Arc::new(MapEnv::default()),
        ..ClientConfig::default()
    };
    let err = new_model("", cfg()).err().unwrap();
    assert!(err.is(ErrorKind::ModelNameRequired), "{err}");
    assert_eq!(err.to_string(), "openai: model name is required");

    let err = "grpc".parse::<Api>().unwrap_err();
    assert_eq!(err.to_string(), r#"openai: unsupported API: "grpc""#);

    // An endpoint error reaches the caller as `ModelError::Other(OpenAiError)`; an empty
    // request fails before any request is sent.
    let client = ScriptedClient::json("{}");
    let http_client: Arc<dyn HttpClient> = client.clone();
    let llm = new_model(
        "m",
        ClientConfig {
            api: Api::ChatCompletions,
            http_client: Some(http_client),
            ..cfg()
        },
    )
    .unwrap();
    let got: Vec<_> = llm
        .generate_content(LlmRequest::default(), false)
        .collect()
        .await;
    let [Err(ModelError::Other(inner))] = got.as_slice() else {
        panic!("GenerateContent() = {got:?}, want one ModelError::Other");
    };
    let err = inner.downcast_ref::<OpenAiError>().expect("an OpenAiError");
    assert!(err.is(ErrorKind::NoContents), "{err}");
    assert_eq!(client.hits(), 0);
}
