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

//! The public constructor (port of ADK-Go `model/openaimodel/openaimodel.go`'s `NewModel` and
//! `FinishMessageKey`).

use std::sync::Arc;

use gadk_model::Llm;

use crate::client::OpenAiClient;
use crate::config::{Api, ClientConfig};
use crate::error::{ErrorKind, OpenAiError};
use crate::{completions, responses};

/// The [`LlmResponse::custom_metadata`](gadk_model::LlmResponse::custom_metadata) key under
/// which a turn that ended badly but still carries an answer reports the provider's own
/// account of why: a content filter's `content_filter`, an incomplete reason this crate does
/// not map, or a server error message (Go `FinishMessageKey`). Its `finish_reason` says the
/// turn was cut short; this says what the provider called it.
///
/// A turn left with nothing to read reports the same wording in `error_code` and
/// `error_message` instead.
pub const FINISH_MESSAGE_KEY: &str = "openai_finish_message";

/// Builds a model talking to the API [`ClientConfig::api`] names (Go `NewModel`).
///
/// Go's order: an empty `model_name` is [`ErrorKind::ModelNameRequired`]; then the client is
/// built from `cfg` (the environment is read now) and the API selects the model. Go's
/// `ErrUnsupportedAPI` for an unknown API value comes from parsing one: [`Api`]'s `FromStr`.
pub fn new_model(model_name: &str, cfg: ClientConfig) -> Result<Arc<dyn Llm>, OpenAiError> {
    if model_name.is_empty() {
        return Err(OpenAiError::sentinel(ErrorKind::ModelNameRequired));
    }
    let client = Arc::new(OpenAiClient::new(&cfg));
    Ok(match cfg.api {
        Api::Responses => Arc::new(responses::model::Model::new(client, model_name)),
        Api::ChatCompletions => Arc::new(completions::model::Model::new(client, model_name)),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use gadk_platform::env::MapEnv;

    fn cfg(api: Api) -> ClientConfig {
        ClientConfig {
            api_key: "k".into(),
            api,
            env: Arc::new(MapEnv::default()),
            ..ClientConfig::default()
        }
    }

    #[test]
    fn empty_model_name_is_rejected_first() {
        let err = match new_model("", cfg(Api::ChatCompletions)) {
            Err(e) => e,
            Ok(_) => panic!("want an error"),
        };
        assert!(err.is(ErrorKind::ModelNameRequired));
        assert_eq!(err.to_string(), "openai: model name is required");
    }

    #[test]
    fn each_api_builds_a_model_with_the_name() {
        let llm = new_model("gpt-4o-mini", cfg(Api::Responses)).unwrap();
        assert_eq!(llm.name(), "gpt-4o-mini");
        assert!(llm.as_any().is::<responses::model::Model>());
        let llm = new_model("gpt-4o-mini", cfg(Api::ChatCompletions)).unwrap();
        assert_eq!(llm.name(), "gpt-4o-mini");
        assert!(llm.as_any().is::<completions::model::Model>());
    }

    #[test]
    fn finish_message_key_matches_the_responses_path() {
        assert_eq!(FINISH_MESSAGE_KEY, responses::model::FINISH_MESSAGE_KEY);
    }
}
