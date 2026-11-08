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

//! R16: `gadk::anthropic` (feature `anthropic`, not on by default) is `gadk_anthropic`, and
//! `gadk::gemini::apigee` (feature `gemini`) is `gadk_gemini::apigee`; models built through
//! the façade are gadk `Llm`s.

#[cfg(feature = "anthropic")]
mod anthropic {
    use std::collections::HashMap;
    use std::sync::Arc;

    use gadk::anthropic::{
        AnthropicError, AnthropicModel, ApiError, Backend,
        CACHE_CREATION_INPUT_TOKENS_METADATA_KEY, ClientConfig, DEFAULT_MAX_TOKENS, Effort,
        EnvProvider, ErrorKind, MapEnv, PromptCacheConfig, PromptCacheTtl, STATUS_OVERLOADED,
        STOP_REASON_METADATA_KEY,
    };
    use gadk::model::Llm;
    use pretty_assertions::assert_eq;

    /// Compiles only if each façade path names the `gadk_anthropic` item.
    #[test]
    fn facade_anthropic_paths_are_gadk_anthropic() {
        fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
        same::<AnthropicModel>(None, None::<&gadk_anthropic::AnthropicModel>);
        same::<ClientConfig>(None, None::<&gadk_anthropic::ClientConfig>);
        same::<Backend>(None, None::<&gadk_anthropic::Backend>);
        same::<Effort>(None, None::<&gadk_anthropic::Effort>);
        same::<PromptCacheConfig>(None, None::<&gadk_anthropic::PromptCacheConfig>);
        same::<PromptCacheTtl>(None, None::<&gadk_anthropic::PromptCacheTtl>);
        same::<AnthropicError>(None, None::<&gadk_anthropic::AnthropicError>);
        same::<ApiError>(None, None::<&gadk_anthropic::ApiError>);
        same::<ErrorKind>(None, None::<&gadk_anthropic::ErrorKind>);
        same::<dyn EnvProvider>(None, None::<&dyn gadk_anthropic::EnvProvider>);
        same::<MapEnv>(None, None::<&gadk_anthropic::MapEnv>);

        // Go's exported constants.
        assert_eq!(DEFAULT_MAX_TOKENS, 8192);
        assert_eq!(STATUS_OVERLOADED, 529);
        assert_eq!(STOP_REASON_METADATA_KEY, "anthropic_stop_reason");
        assert_eq!(
            CACHE_CREATION_INPUT_TOKENS_METADATA_KEY,
            "anthropic_cache_creation_input_tokens"
        );
        assert_eq!(Backend::default().to_string(), "anthropic");
        assert_eq!(Backend::Vertex.to_string(), "vertex");
    }

    #[tokio::test]
    async fn facade_anthropic_model_is_a_gadk_llm() {
        let cfg = || ClientConfig {
            api_key: Some("test-anthropic-key".into()),
            env: Arc::new(MapEnv(HashMap::new())),
            ..Default::default()
        };
        // `new` sends no request.
        let m: Arc<dyn Llm> = AnthropicModel::new("claude-sonnet-4-5-20250929", cfg())
            .await
            .unwrap();
        assert_eq!(m.name(), "claude-sonnet-4-5-20250929");
        assert!(m.as_any().downcast_ref::<AnthropicModel>().is_some());

        // Go's validation text, through the façade.
        let err = AnthropicModel::new("  ", cfg()).await.err().unwrap();
        assert!(err.is(ErrorKind::InvalidConfig), "{err}");
        assert_eq!(
            err.to_string(),
            "anthropicmodel: invalid configuration: model name must not be empty"
        );
    }
}

#[cfg(feature = "gemini")]
mod apigee {
    use std::collections::HashMap;
    use std::sync::Arc;

    use gadk::gemini::MapEnv;
    use gadk::gemini::apigee::{ApigeeConfig, ApigeeModel};
    use gadk::model::Llm;
    use pretty_assertions::assert_eq;

    /// Compiles only if each façade path names the `gadk_gemini::apigee` item.
    #[test]
    fn facade_apigee_paths_are_gadk_gemini_apigee() {
        fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
        same::<ApigeeModel>(None, None::<&gadk_gemini::apigee::ApigeeModel>);
        same::<ApigeeConfig>(None, None::<&gadk_gemini::apigee::ApigeeConfig>);
    }

    #[tokio::test]
    async fn facade_apigee_model_is_a_gadk_llm() {
        let env = HashMap::from([("GOOGLE_API_KEY".to_string(), "test-key".to_string())]);
        let cfg = ApigeeConfig {
            proxy_url: Some("https://test.apigee.net".into()),
            env: Arc::new(MapEnv(env)),
            ..Default::default()
        };
        // `new` sends no request.
        let m: Arc<dyn Llm> = Arc::new(
            ApigeeModel::new("apigee/gemini-2.5-flash", cfg)
                .await
                .unwrap(),
        );
        assert_eq!(m.name(), "apigee/gemini-2.5-flash");
        // Go's apigeeModel is not a GoogleLLM (ruling R-e).
        assert!(m.google_variant().is_none());

        let cfg = ApigeeConfig {
            env: Arc::new(MapEnv(HashMap::new())),
            ..Default::default()
        };
        let err = ApigeeModel::new("apigee/gemini-2.5-flash", cfg)
            .await
            .err()
            .unwrap();
        assert_eq!(
            err.to_string(),
            "APIGEE_PROXY_URL environment variable not set"
        );
    }
}
