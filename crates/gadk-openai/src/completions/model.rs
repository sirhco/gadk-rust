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

//! The Chat Completions model.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::{self, BoxStream};
use gadk_genai::{Content, FinishReason, Part};
use gadk_model::{
    BodyStream, Llm, LlmRequest, LlmResponse, ModelError, StreamingResponseAggregator,
    llm_response_from_genai,
};
use serde_json::Value;

use crate::client::{
    CHAT_COMPLETIONS_PATH, OpenAiClient, Reply, decode_body, encode_body, event_stream,
};
use crate::completions::request::build_params;
use crate::completions::response::{
    convert_completion, convert_logprobs, convert_usage, finish_reason,
};
use crate::completions::stream::StreamTranslator;
use crate::completions::wire::{
    ChatCompletion, ChatCompletionChunk, ChatCompletionNewParams, CompletionUsage,
};
use crate::error::OpenAiError;
use crate::shared::config::request_timeout;
use crate::shared::response::{
    carries_content, completed_content_supersedes, is_empty_output, parts_without_calls,
    single_error_sequence,
};

/// Talks to the Chat Completions API.
#[derive(Debug, Clone)]
pub(crate) struct Model {
    /// The client.
    pub(crate) client: Arc<OpenAiClient>,
    /// The model name a request is sent with when it names none.
    pub(crate) name: String,
}

impl Model {
    /// Creates the model.
    pub(crate) fn new(client: Arc<OpenAiClient>, model_name: &str) -> Self {
        Self {
            client,
            name: model_name.to_string(),
        }
    }
}

impl Llm for Model {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// Builds the params, then one blocking call (`generate`, errors
    /// wrapped `openai: call failed: …`) or one stream (`generate_stream`, with
    /// `stream_options.include_usage`, partials through the aggregator and a final response
    /// from the accumulated snapshot). Nothing is sent before the stream is first polled; the
    /// `http_options.timeout` deadline starts then.
    fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
        let params = match build_params(&self.name, &req) {
            Ok(params) => params,
            Err(err) => return single_error_sequence(err),
        };
        let timeout = request_timeout(req.http_options.as_ref());
        let client = Arc::clone(&self.client);
        if stream {
            generate_stream(client, params, timeout)
        } else {
            generate(client, params, timeout)
        }
    }
}

/// One blocking Chat Completions call. Failures of the call (encoding, transport,
/// API status, body decoding) are wrapped `openai: call failed: …`; conversion errors are not.
/// The call deadline is computed when the stream is first polled, so every run of a
/// re-issued call gets a fresh one.
fn generate(
    client: Arc<OpenAiClient>,
    params: ChatCompletionNewParams,
    timeout: Option<Duration>,
) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    stream::once(async move {
        let completion = call(&client, &params, timeout)
            .await
            .map_err(OpenAiError::call_failed)?;
        let converted = convert_completion(Some(&completion))?;
        let mut resp = llm_response_from_genai(&converted);
        attach_metadata(&mut resp, &completion);
        Ok(resp)
    })
    .map(|item: Result<LlmResponse, OpenAiError>| item.map_err(ModelError::from))
    .boxed()
}

/// The blocking call: encode, post, decode.
async fn call(
    client: &OpenAiClient,
    params: &ChatCompletionNewParams,
    timeout: Option<Duration>,
) -> Result<ChatCompletion, OpenAiError> {
    let body = encode_body(params, false)?;
    match client
        .post(CHAT_COMPLETIONS_PATH, body, false, timeout)
        .await?
    {
        Reply::Body { content_type, body } => decode_body(&content_type, &body),
        // `post` streams only when asked to; read a stream as the body it is.
        Reply::Stream(body) => {
            let bytes = read_all(body).await?;
            decode_body("application/json", &bytes)
        }
    }
}

/// The whole of a body stream.
async fn read_all(mut body: BodyStream) -> Result<Vec<u8>, OpenAiError> {
    let mut out = Vec::new();
    while let Some(chunk) = body.next().await {
        out.extend_from_slice(&chunk.map_err(OpenAiError::from_body_error)?);
    }
    Ok(out)
}

/// One streamed Chat Completions call, yielding a partial per delta and
/// one final response built from the accumulated turn. Errors are yielded as they are (not
/// wrapped) and end the stream. Dropping the stream closes the
/// connection.
fn generate_stream(
    client: Arc<OpenAiClient>,
    mut params: ChatCompletionNewParams,
    timeout: Option<Duration>,
) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    // Chat Completions reports usage on a streamed turn only when asked, so a streamed turn
    // would otherwise account for nothing.
    params.stream_options.include_usage = Some(true);
    stream::once(async move { open_stream(&client, &params, timeout).await })
        .flat_map(|opened| match opened {
            Ok(chunks) => translate(chunks),
            Err(err) => single_error_sequence(err),
        })
        .boxed()
}

/// The streaming call: encode, post, and the decoded chunks.
async fn open_stream(
    client: &OpenAiClient,
    params: &ChatCompletionNewParams,
    timeout: Option<Duration>,
) -> Result<BoxStream<'static, Result<ChatCompletionChunk, OpenAiError>>, OpenAiError> {
    let body = encode_body(params, true)?;
    let body = match client
        .post(CHAT_COMPLETIONS_PATH, body, true, timeout)
        .await?
    {
        Reply::Stream(body) => body,
        // `post` reads a body only when not streaming; decode one as the stream it holds.
        Reply::Body { body, .. } => stream::iter([Ok(body)]).boxed(),
    };
    Ok(event_stream::<ChatCompletionChunk>(body))
}

/// The state of one streamed turn.
struct StreamState {
    chunks: BoxStream<'static, Result<ChatCompletionChunk, OpenAiError>>,
    translator: StreamTranslator,
    aggregator: StreamingResponseAggregator,
    pending: VecDeque<Result<LlmResponse, ModelError>>,
    done: bool,
}

/// The loop over the decoded chunks, then its close-out.
fn translate(
    chunks: BoxStream<'static, Result<ChatCompletionChunk, OpenAiError>>,
) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    let state = StreamState {
        chunks,
        translator: StreamTranslator::new(),
        aggregator: StreamingResponseAggregator::new(),
        pending: VecDeque::new(),
        done: false,
    };
    stream::unfold(state, |mut s| async move {
        loop {
            if let Some(item) = s.pending.pop_front() {
                return Some((item, s));
            }
            if s.done {
                return None;
            }
            match s.chunks.next().await {
                Some(Ok(chunk)) => match s.translator.process(chunk) {
                    Ok(Some(genai_resp)) => {
                        for mut item in s.aggregator.process_response(genai_resp) {
                            if let Ok(resp) = item.as_mut() {
                                attach_metadata(resp, s.translator.completion());
                            }
                            s.pending.push_back(item);
                        }
                    }
                    Ok(None) => {}
                    Err(err) => {
                        s.done = true;
                        s.pending.push_back(Err(err.into()));
                    }
                },
                Some(Err(err)) => {
                    s.done = true;
                    s.pending.push_back(Err(err.into()));
                }
                None => {
                    s.done = true;
                    if let Some(item) = close_stream(&mut s.aggregator, &s.translator) {
                        s.pending.push_back(item.map_err(ModelError::from));
                    }
                }
            }
        }
    })
    .boxed()
}

/// The end of the stream: the final response from the aggregate and the accumulated
/// snapshot, or the snapshot's error; `None` when there is nothing to report.
fn close_stream(
    aggregator: &mut StreamingResponseAggregator,
    translator: &StreamTranslator,
) -> Option<Result<LlmResponse, OpenAiError>> {
    let mut final_resp = aggregator.close();
    let completion = translator.completion();
    match convert_completion(Some(completion)) {
        Ok(converted) if !carries_content(final_resp.as_ref()) => {
            // The deltas contributed nothing that survived aggregation, which is what a turn
            // of nothing but tool calls looks like. The snapshot holds the whole turn.
            final_resp = Some(llm_response_from_genai(&converted));
        }
        Ok(converted) => {
            // Only the snapshot states the turn's tool calls, so it replaces the aggregate
            // whenever it retains everything already streamed, and otherwise the streamed
            // text stands with the snapshot's calls added.
            let content = converted
                .candidates
                .into_iter()
                .next()
                .and_then(|candidate| candidate.content);
            if let Some(resp) = final_resp.as_mut() {
                if completed_content_supersedes(resp.content.as_ref(), content.as_ref()) {
                    resp.content = content;
                } else {
                    let (mut kept, _, _) = parts_without_calls(resp.content.as_ref());
                    kept.extend(function_call_parts(content.as_ref()));
                    let role = resp
                        .content
                        .as_ref()
                        .map(|c| c.role.clone())
                        .unwrap_or_default();
                    resp.content = Some(Content { role, parts: kept });
                }
            }
        }
        // The snapshot holds nothing but the deltas produced a turn, so report it rather than
        // fail a call the model answered. An unusable snapshot, such as a tool call whose
        // arguments do not parse, fails as blocking does: only it states the calls, so
        // dropping one would pass the turn off as whole.
        Err(err) if carries_content(final_resp.as_ref()) && is_empty_output(&err) => {}
        Err(err) => return Some(Err(err)),
    }
    let mut final_resp = final_resp?;
    finalize_stream_response(&mut final_resp, completion, translator.usage());
    Some(Ok(final_resp))
}

/// The parts of `content` that carry a function call.
fn function_call_parts(content: Option<&Content>) -> Vec<Part> {
    content
        .map(|content| {
            content
                .parts
                .iter()
                .filter(|part| part.function_call.is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Closes out a streamed turn. Deltas carry no finish reason or
/// usage, so this is the one response that reports them: the finish reason from the
/// accumulated snapshot, and usage from the latest report, if any arrived.
fn finalize_stream_response(
    final_resp: &mut LlmResponse,
    completion: &ChatCompletion,
    usage: Option<&CompletionUsage>,
) {
    final_resp.turn_complete = true;
    // A provider that ignores stream_options reports no usage, and zeros would say a turn that
    // did real work cost nothing; the Responses path leaves usage unset in the same case.
    final_resp.usage_metadata = usage.map(convert_usage);
    attach_metadata(final_resp, completion);
    if !completion.model.is_empty() {
        final_resp.model_version.clone_from(&completion.model);
    }
    let Some(choice) = completion.choices.first() else {
        // Nothing said why the turn ended, and reading that as a clean stop would have a
        // caller that retries on anything but STOP accept a partial answer as final.
        final_resp.finish_reason = Some(FinishReason::Unspecified);
        return;
    };
    final_resp.finish_reason = Some(finish_reason(&choice.finish_reason));
    final_resp.logprobs_result = convert_logprobs(&choice.logprobs);
}

/// Records the response ID and model under the keys the Responses path
/// uses (`openai_response_id`, `openai_model`, both strings). Nothing without an ID.
fn attach_metadata(resp: &mut LlmResponse, completion: &ChatCompletion) {
    if completion.id.is_empty() {
        return;
    }
    resp.custom_metadata.insert(
        "openai_response_id".to_string(),
        Value::String(completion.id.clone()),
    );
    resp.custom_metadata.insert(
        "openai_model".to_string(),
        Value::String(completion.model.clone()),
    );
}
