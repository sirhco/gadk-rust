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

//! The Responses model (port of ADK-Go `internal/responses/model.go`).

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::{self, BoxStream};
use gadk_genai::{BlockedReason, Content, FinishReason, FunctionCall, Part, ROLE_MODEL};
use gadk_model::{
    Llm, LlmRequest, LlmResponse, ModelError, StreamingResponseAggregator, llm_response_from_genai,
};
use serde_json::Value;

use crate::client::{OpenAiClient, RESPONSES_PATH, Reply, decode_body, encode_body, event_stream};
use crate::error::OpenAiError;
use crate::responses::consts::{RESPONSE_COMPLETED, RESPONSE_CREATED, RESPONSE_INCOMPLETE};
use crate::responses::request::build_params;
use crate::responses::response::{
    convert_function_call, convert_response, convert_usage, failed_response_error, finish_message,
    finish_reason, logprobs_for, reports_failure,
};
use crate::responses::stream::StreamTranslator;
use crate::responses::wire::{
    Response, ResponseNewParams, ResponseOutputItem, ResponseStreamEvent,
};
use crate::shared::config::request_timeout;
use crate::shared::response::{
    answer_text, carries_content, completed_content_supersedes, is_empty_output,
    parts_without_calls, single_error_sequence,
};

/// Go `FinishMessageKey` of this package: the custom-metadata key a turn that ended badly
/// reports the provider's account under; [`crate::FINISH_MESSAGE_KEY`] must equal it.
pub(crate) const FINISH_MESSAGE_KEY: &str = "openai_finish_message";

/// Go `responses.Model`: talks to the Responses API.
#[derive(Debug, Clone)]
pub(crate) struct Model {
    /// The client.
    pub(crate) client: Arc<OpenAiClient>,
    /// The model name a request is sent with when it names none.
    pub(crate) name: String,
}

impl Model {
    /// Go `responses.New`.
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

    /// Go `GenerateContent`: builds the params, then one blocking call (`generate`, errors
    /// wrapped `openai: call failed: %w`, metadata and finish signal attached) or one stream
    /// (`generateStream`: terminal-event tracking, partials through the aggregator, terminal
    /// tool-call adoption, `finalizeStreamResponse`). Nothing is sent before the stream is
    /// first polled; the `http_options.timeout` deadline starts then (D7), so each call of
    /// this method gets its own. Dropping the stream cancels the call (D9).
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
        if stream {
            generate_stream(self.client.clone(), params, timeout)
        } else {
            generate(self.client.clone(), params, timeout)
        }
    }
}

/// Go `generate`: one call, its errors wrapped `openai: call failed: %w`; the converted
/// response with metadata and the finish signal attached.
fn generate(
    client: Arc<OpenAiClient>,
    params: ResponseNewParams,
    timeout: Option<Duration>,
) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    stream::once(async move {
        generate_once(&client, &params, timeout)
            .await
            .map_err(ModelError::from)
    })
    .boxed()
}

/// The body of [`generate`].
async fn generate_once(
    client: &OpenAiClient,
    params: &ResponseNewParams,
    timeout: Option<Duration>,
) -> Result<LlmResponse, OpenAiError> {
    let body = encode_body(params, false).map_err(OpenAiError::call_failed)?;
    let resp: Response = match client
        .post(RESPONSES_PATH, body, false, timeout)
        .await
        .map_err(OpenAiError::call_failed)?
    {
        Reply::Body { content_type, body } => {
            decode_body(&content_type, &body).map_err(OpenAiError::call_failed)?
        }
        // `post` answers a non-streaming call with a body; an empty one reads as Go's.
        Reply::Stream(_) => {
            decode_body("application/json", b"").map_err(OpenAiError::call_failed)?
        }
    };
    let genai_resp = convert_response(Some(&resp))?;
    let mut llm_resp = llm_response_from_genai(&genai_resp);
    attach_metadata(&mut llm_resp, &resp);
    attach_finish_signal(&mut llm_resp, &resp, false);
    Ok(llm_resp)
}

/// Go `terminalEvent`: what the stream's terminal event said, as distinct from what the
/// response it carried repeated: a `response.incomplete` declares a turn cut short even when
/// its payload reports no status and no reason.
#[derive(Debug, Default)]
struct TerminalEvent {
    /// The last `response.created` response, replaced by the terminal event's.
    resp: Option<Response>,
    /// Set by a terminal event, the only kind that says why the turn ended; implies
    /// `resp.is_some()`.
    seen: bool,
    /// The terminal event was `response.incomplete`.
    incomplete: bool,
}

impl TerminalEvent {
    /// Go `completed`: the turn ended on a `response.completed`, the only terminal event
    /// that states the turn's whole output.
    fn completed(&self) -> bool {
        self.seen && !self.incomplete
    }

    /// Go's first-terminal-object-wins tracking: a later terminal event, or a stray
    /// `response.created`, would relabel a truncated turn a clean stop. An event whose
    /// response never decoded is not one of those ([`carries_response`]).
    fn track(&mut self, event: &ResponseStreamEvent) {
        if self.seen || !carries_response(&event.response) {
            return;
        }
        match event.kind.as_str() {
            RESPONSE_CREATED => self.resp = Some(event.response.clone()),
            RESPONSE_COMPLETED => {
                self.resp = Some(event.response.clone());
                self.seen = true;
            }
            RESPONSE_INCOMPLETE => {
                self.resp = Some(event.response.clone());
                self.seen = true;
                self.incomplete = true;
            }
            _ => {}
        }
    }
}

/// Go `carriesResponse`: whether an event delivered the response object the schema marks
/// required. Any of `id`, `status`, `output`, `incomplete_details`, `error` or `model` having
/// decoded stands for its presence; a bare `{}` (or an omitted object) is rejected.
fn carries_response(resp: &Response) -> bool {
    resp.id.is_some()
        || resp.status.is_some()
        || resp.output.is_some()
        || resp.incomplete_details.is_some()
        || resp.error.is_some()
        || resp.model.is_some()
}

/// Where a [`StreamRun`] is.
enum Phase {
    /// Not yet polled: nothing has been sent.
    Start {
        client: Arc<OpenAiClient>,
        params: Box<ResponseNewParams>,
        timeout: Option<Duration>,
    },
    /// Reading the stream's events.
    Events(BoxStream<'static, Result<ResponseStreamEvent, OpenAiError>>),
    /// Over: only what is pending is still yielded.
    Done,
}

/// Go `generateStream`'s state between yields.
struct StreamRun {
    phase: Phase,
    aggregator: StreamingResponseAggregator,
    translator: StreamTranslator,
    term: TerminalEvent,
    pending: VecDeque<Result<LlmResponse, ModelError>>,
}

/// Go `generateStream`: the call starts (and its deadline with it) on the first poll; each
/// event is tracked for the terminal response, translated, and aggregated (partials carry
/// the response's metadata once known); the first error ends the stream; the end of the
/// stream yields the finalized turn ([`StreamRun::finish`]). The `http_options.timeout`
/// deadline bounds the whole stream, the consumer's time between polls included.
fn generate_stream(
    client: Arc<OpenAiClient>,
    params: ResponseNewParams,
    timeout: Option<Duration>,
) -> BoxStream<'static, Result<LlmResponse, ModelError>> {
    let run = StreamRun {
        phase: Phase::Start {
            client,
            params: Box::new(params),
            timeout,
        },
        aggregator: StreamingResponseAggregator::new(),
        translator: StreamTranslator::new(),
        term: TerminalEvent::default(),
        pending: VecDeque::new(),
    };
    stream::unfold(run, |mut run| async move {
        let item = run.next_item().await?;
        Some((item, run))
    })
    .boxed()
}

impl StreamRun {
    /// The next item to yield, or `None` once the stream is over.
    async fn next_item(&mut self) -> Option<Result<LlmResponse, ModelError>> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            match std::mem::replace(&mut self.phase, Phase::Done) {
                Phase::Done => return None,
                Phase::Start {
                    client,
                    params,
                    timeout,
                } => match start_stream(&client, &params, timeout).await {
                    Ok(events) => self.phase = Phase::Events(events),
                    Err(err) => return Some(Err(err.into())),
                },
                Phase::Events(mut events) => match events.next().await {
                    Some(Ok(event)) => {
                        if let Err(err) = self.on_event(&event) {
                            return Some(Err(err.into()));
                        }
                        self.phase = Phase::Events(events);
                    }
                    // Go `stream.Err()`, unwrapped.
                    Some(Err(err)) => return Some(Err(err.into())),
                    None => self.finish(),
                },
            }
        }
    }

    /// One event: terminal tracking, then translation and aggregation into `pending`.
    fn on_event(&mut self, event: &ResponseStreamEvent) -> Result<(), OpenAiError> {
        self.term.track(event);
        let Some(genai_resp) = self.translator.process(event)? else {
            return Ok(());
        };
        for mut item in self.aggregator.process_response(genai_resp) {
            if let (Ok(resp), Some(term_resp)) = (&mut item, &self.term.resp) {
                attach_metadata(resp, term_resp);
            }
            self.pending.push_back(item);
        }
        Ok(())
    }

    /// The end of a stream that did not fail: a failure stated in the terminal body, else
    /// the aggregated turn (rebuilt from or superseded by the terminal event's content where
    /// Go does), its tool calls adopted from the terminal event, finalized.
    fn finish(&mut self) {
        match self.finished() {
            Ok(Some(resp)) => self.pending.push_back(Ok(resp)),
            Ok(None) => {}
            Err(err) => self.pending.push_back(Err(err.into())),
        }
    }

    /// The body of [`finish`](Self::finish).
    fn finished(&mut self) -> Result<Option<LlmResponse>, OpenAiError> {
        let term = &self.term;
        // A failure stated in the terminal event's body rather than as a `response.failed`
        // event. Checked after the loop so a terminal event still overrides a
        // `response.created` that announced a failure, and deltas already yielded stay
        // yielded.
        if let Some(resp) = &term.resp
            && reports_failure(resp)
        {
            return Err(failed_response_error(resp));
        }

        let mut final_resp = self.aggregator.close();
        if !carries_content(final_resp.as_ref()) && term.seen {
            // The deltas contributed nothing that survived aggregation, but the terminal
            // event can still hold the whole turn: rebuild it the way the blocking path
            // would.
            match convert_response(term.resp.as_ref()) {
                Ok(genai_resp) => final_resp = Some(llm_response_from_genai(&genai_resp)),
                // Nothing to rebuild from, but the aggregator did produce a turn: report it
                // with the reason the event carries.
                Err(err) if final_resp.is_some() && is_empty_output(&err) => {}
                // Blocking fails the call on unusable output; match it.
                Err(err) => return Err(err),
            }
        } else if term.completed() {
            // A completed response can contain text that never arrived as a delta. Use its
            // content for the final response unless it is unusable or omits content that
            // already streamed.
            if let (Ok(genai_resp), Some(resp)) =
                (convert_response(term.resp.as_ref()), final_resp.as_mut())
            {
                let content = genai_resp
                    .candidates
                    .into_iter()
                    .next()
                    .and_then(|c| c.content);
                if completed_content_supersedes(resp.content.as_ref(), content.as_ref()) {
                    resp.content = content;
                }
            }
        }
        // No aggregated turn and nothing to rebuild one from.
        let Some(mut final_resp) = final_resp else {
            return Ok(None);
        };
        adopt_terminal_calls(&mut final_resp, term)?;
        finalize_stream_response(&mut final_resp, term);
        Ok(Some(final_resp))
    }
}

/// openai-go `Responses.NewStreaming` up to the first event: the body is encoded and sent
/// now, so the call's deadline starts now. Errors are not wrapped (Go's `stream.Err()`).
async fn start_stream(
    client: &OpenAiClient,
    params: &ResponseNewParams,
    timeout: Option<Duration>,
) -> Result<BoxStream<'static, Result<ResponseStreamEvent, OpenAiError>>, OpenAiError> {
    let body = encode_body(params, true)?;
    match client.post(RESPONSES_PATH, body, true, timeout).await? {
        Reply::Stream(body) => Ok(event_stream(body)),
        // `post` answers a streaming call below 400 with the live body; a body read whole
        // holds no events.
        Reply::Body { .. } => Ok(stream::empty().boxed()),
    }
}

/// Go `adoptTerminalCalls`: makes the terminal event's tool calls the turn's own. They
/// replace what streamed rather than merging with it; when the two lists are the same length
/// each takes the slot of the streamed call it replaces, else they go where the turn's first
/// call sat (or first, when none streamed and the event leads with a call). An incomplete
/// event naming fewer calls than streamed changes nothing; fields an item leaves unstated
/// are restored from its streamed counterpart ([`restore_unstated`]).
fn adopt_terminal_calls(
    final_resp: &mut LlmResponse,
    term: &TerminalEvent,
) -> Result<(), OpenAiError> {
    let (true, Some(resp)) = (term.seen, &term.resp) else {
        return Ok(());
    };
    let items: Vec<&ResponseOutputItem> = resp
        .output_items()
        .iter()
        .filter(|item| item.kind == "function_call")
        .collect();
    if items.is_empty() {
        // Naming no calls says nothing about them: what streamed stands.
        return Ok(());
    }
    let (kept, streamed_calls, mut at) = parts_without_calls(final_resp.content.as_ref());
    if term.incomplete && items.len() < streamed_calls.len() {
        // A shorter list on an incomplete event is the truncation showing.
        return Ok(());
    }
    // Paired before anything is replaced, so an unstated field is restored the same way
    // whichever branch below reports the call.
    let paired = items.len() == streamed_calls.len();
    let mut stated = Vec::with_capacity(items.len());
    // Seeded with every ID the event states, so lending one cannot collide with an ID a
    // later item claims for itself.
    let mut lent: HashSet<String> = items
        .iter()
        .filter(|item| !item.call_id.is_empty())
        .map(|item| item.call_id.clone())
        .collect();
    let mut restored = vec![false; streamed_calls.len()];
    for (nth, item) in items.iter().enumerate() {
        let mut streamed = streamed_counterpart(item, nth, &streamed_calls, paired);
        let (mut part, args_usable) = match convert_function_call(item) {
            Ok(part) => (part, item.arguments.is_some()),
            // Nothing else states this call, so it is as unusable here as it is to blocking.
            Err(err) if streamed.is_none() => return Err(err),
            // Unparseable arguments lose what unstated ones lose, and the deltas built this
            // call usably: take the fallback below.
            Err(_) => (
                Part {
                    function_call: Some(FunctionCall {
                        name: item.name.clone(),
                        id: item.call_id.clone(),
                        ..FunctionCall::default()
                    }),
                    ..Part::default()
                },
                false,
            ),
        };
        if streamed.is_some_and(|i| restored[i]) {
            // A streamed call restores onto one call only.
            streamed = None;
        }
        let id_free = streamed.is_some_and(|i| {
            !streamed_calls[i].id.is_empty() && !lent.contains(&streamed_calls[i].id)
        });
        restore_unstated(
            &mut part,
            streamed.map(|i| &streamed_calls[i]),
            args_usable,
            id_free,
        );
        if let Some(i) = streamed {
            restored[i] = true;
            if id_free
                && part
                    .function_call
                    .as_ref()
                    .is_some_and(|call| call.id == streamed_calls[i].id)
            {
                lent.insert(streamed_calls[i].id.clone());
            }
        }
        stated.push(part);
    }
    if paired {
        // One slot per stated call: the nth call the turn reported takes the nth the event
        // states, keeping interleaved text interleaved.
        if let Some(content) = final_resp.content.as_mut() {
            let mut stated = stated.into_iter();
            for part in &mut content.parts {
                if part.function_call.is_some()
                    && let Some(next) = stated.next()
                {
                    *part = next;
                }
            }
        }
        return Ok(());
    }
    if streamed_calls.is_empty() && event_leads_with_call(resp.output_items()) {
        // No call survived aggregation, so the event's own ordering is the only one.
        at = 0;
    }
    let content = final_resp.content.get_or_insert_with(|| Content {
        role: ROLE_MODEL.to_string(),
        parts: Vec::new(),
    });
    let mut parts = Vec::with_capacity(kept.len() + stated.len());
    let mut kept = kept.into_iter();
    parts.extend(kept.by_ref().take(at));
    parts.extend(stated);
    parts.extend(kept);
    content.parts = parts;
    Ok(())
}

/// Go `streamedCounterpart`: the index of the call the deltas built for the tool intent the
/// terminal item states, or `None` when the lists cannot be paired. Tried in order: the call
/// ID; the tool's name when exactly one streamed call bears it; the position, when paired.
/// [`names_agree`] gates every tier.
fn streamed_counterpart(
    item: &ResponseOutputItem,
    nth: usize,
    streamed: &[FunctionCall],
    paired: bool,
) -> Option<usize> {
    if !item.call_id.is_empty()
        && let Some(i) = streamed
            .iter()
            .position(|call| call.id == item.call_id && names_agree(item, call))
    {
        return Some(i);
    }
    if !item.name.is_empty() {
        let mut named = streamed
            .iter()
            .enumerate()
            .filter(|(_, call)| call.name == item.name);
        // A name that narrows to two identifies neither.
        if let (Some((i, _)), None) = (named.next(), named.next()) {
            return Some(i);
        }
    }
    if paired && nth < streamed.len() && names_agree(item, &streamed[nth]) {
        return Some(nth);
    }
    None
}

/// Go `namesAgree`: a terminal item and a streamed call name the same tool, a name either
/// side leaves out counting as no disagreement.
fn names_agree(item: &ResponseOutputItem, call: &FunctionCall) -> bool {
    item.name.is_empty() || call.name.is_empty() || call.name == item.name
}

/// Go `eventLeadsWithCall`: the terminal event states a function call ahead of any message
/// or reasoning item.
fn event_leads_with_call(items: &[ResponseOutputItem]) -> bool {
    for item in items {
        match item.kind.as_str() {
            "function_call" => return true,
            "message" | "reasoning" => return false,
            _ => {}
        }
    }
    false
}

/// Go `restoreUnstated`: keeps what streamed for the fields the terminal item left unstated
/// (arguments when `args_usable` is false and some streamed; an empty name; an empty ID when
/// `id_free`).
fn restore_unstated(
    part: &mut Part,
    streamed: Option<&FunctionCall>,
    args_usable: bool,
    id_free: bool,
) {
    let (Some(call), Some(streamed)) = (part.function_call.as_mut(), streamed) else {
        return;
    };
    if !args_usable && !streamed.args.is_empty() {
        call.args = streamed.args.clone();
    }
    if call.name.is_empty() {
        call.name = streamed.name.clone();
    }
    if call.id.is_empty() && id_free {
        // Lent once, because two calls under one ID cannot both be answered.
        call.id = streamed.id.clone();
    }
}

/// Go `finalizeStreamResponse`: closes out a streamed turn on the aggregated response
/// (`turn_complete`; metadata and model version from the last response seen; with a terminal
/// event, its usage, finish reason, logprobs and finish signal; without one,
/// FINISH_REASON_UNSPECIFIED and no usage).
fn finalize_stream_response(final_resp: &mut LlmResponse, term: &TerminalEvent) {
    final_resp.turn_complete = true;
    if let Some(resp) = &term.resp {
        attach_metadata(final_resp, resp);
        final_resp.model_version = resp.model_str().to_string();
    }
    let (true, Some(resp)) = (term.seen, &term.resp) else {
        // The model never said why it stopped; usage is left alone too (only
        // `response.created` is in hand, and its counts are zero).
        final_resp.finish_reason = Some(FinishReason::Unspecified);
        return;
    };
    final_resp.usage_metadata = Some(convert_usage(&resp.usage));
    final_resp.finish_reason = Some(finish_reason(Some(resp), term.incomplete));
    final_resp.logprobs_result =
        logprobs_for(Some(resp), &answer_text(final_resp.content.as_ref()));
    attach_finish_signal(final_resp, resp, term.incomplete);
}

/// Go `attachFinishSignal`: why a SAFETY or OTHER turn did not end cleanly: beside content,
/// the provider's wording under [`FINISH_MESSAGE_KEY`]; with nothing to read, `error_code`
/// (`SAFETY` as genai's blocked reason, else `OTHER`) and `error_message`.
fn attach_finish_signal(resp: &mut LlmResponse, openai_resp: &Response, incomplete_event: bool) {
    let safety = match resp.finish_reason {
        Some(FinishReason::Safety) => true,
        Some(FinishReason::Other) => false,
        // MAX_TOKENS and STOP say all there is to say by themselves.
        _ => return,
    };
    let msg = finish_message(Some(openai_resp), incomplete_event);
    if carries_content(Some(resp)) {
        if !msg.is_empty() {
            resp.custom_metadata
                .insert(FINISH_MESSAGE_KEY.to_string(), Value::String(msg));
        }
        return;
    }
    resp.error_code = if safety {
        // The code model/gemini reports for a blocked prompt.
        BlockedReason::Safety.as_str().to_string()
    } else {
        FinishReason::Other.as_str().to_string()
    };
    resp.error_message = msg;
}

/// Go `attachMetadata`: `openai_response_id` and `openai_model` in the custom metadata.
fn attach_metadata(resp: &mut LlmResponse, openai_resp: &Response) {
    resp.custom_metadata.insert(
        "openai_response_id".to_string(),
        Value::String(openai_resp.id_str().to_string()),
    );
    resp.custom_metadata.insert(
        "openai_model".to_string(),
        Value::String(openai_resp.model_str().to_string()),
    );
}
