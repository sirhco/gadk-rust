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

//! The remote agent's per-invocation processor: partial-artifact aggregation, response
//! conversion and the request callbacks (port of ADK-Go
//! `agent/remoteagent/v2/a2a_agent_run_processor.go`).

use std::collections::HashMap;
use std::sync::Arc;

use gadk_agent::{Context, ContextDelta, InvocationContext};
use gadk_genai::{
    CitationMetadata, Content, GenerateContentResponseUsageMetadata, GroundingMetadata, Part,
    ROLE_MODEL,
};
use gadk_session::{Event, EventActions};
use serde_json::{Map, Value};

use super::agent::{A2AConfig, to_error_event};
use crate::Error;
use crate::convert::{
    artifact_update_visible_meta, is_partial_flag_set, new_remote_agent_event, stream_meta,
    to_adk_meta_key, to_map_structure, to_session_event_with_parts,
};

/// Go `converters.ToMapStructure`'s result (`None` = Go's nil map).
type MapStructure = Option<Map<String, Value>>;

/// Go `artifactAggregation`: the partial chunks of one artifact seen so far.
#[derive(Debug, Default)]
pub(crate) struct ArtifactAggregation {
    pub(crate) parts: Vec<Part>,
    pub(crate) citations: Option<CitationMetadata>,
    pub(crate) grounding: Option<GroundingMetadata>,
    pub(crate) custom_meta: Option<Map<String, Value>>,
    pub(crate) usage: Option<GenerateContentResponseUsageMetadata>,
}

/// One event [`RunProcessor::aggregate_partial`] asks the run loop to emit.
#[derive(Debug)]
pub(crate) struct ToEmit {
    /// The event.
    pub(crate) event: Event,
    /// Whether it is the event passed in (its after-request callbacks already ran), not one
    /// synthesized from buffered chunks (Go compares the pointers).
    pub(crate) passthrough: bool,
}

/// Go `a2aAgentRunProcessor`.
pub(crate) struct RunProcessor {
    config: Arc<A2AConfig>,
    request: a2a::SendMessageRequest,
    /// partial event contents emitted before the terminal event
    pub(crate) aggregations: HashMap<String, ArtifactAggregation>,
    /// used to emit aggregations in the order of last update
    pub(crate) aggregation_order: Vec<String>,
}

impl RunProcessor {
    /// Go `newRunProcessor`.
    pub(crate) fn new(config: Arc<A2AConfig>, request: a2a::SendMessageRequest) -> Self {
        Self {
            config,
            request,
            aggregations: HashMap::new(),
            aggregation_order: Vec::new(),
        }
    }

    /// Go `aggregatePartial`: stores the contents of partial artifact chunks so that they are
    /// emitted as one non-partial event with the last chunk or the terminal status update. It
    /// may modify `event` or add synthesized events before or after it.
    pub(crate) fn aggregate_partial(
        &mut self,
        ctx: &InvocationContext,
        a2a_event: Option<&a2a::StreamResponse>,
        mut event: Event,
    ) -> Vec<ToEmit> {
        let pass = |event: Event| ToEmit {
            event,
            passthrough: true,
        };
        // ADK partial events should be aggregated by ADK and emitted as a non-partial artifact
        // update. That's why we skip them regardless of the actual isPartial value. This is the
        // legacy output mode.
        if let Some(a2a_event) = a2a_event
            && is_partial_flag_set(stream_meta(a2a_event))
        {
            return vec![pass(event)];
        }

        match a2a_event {
            // RemoteAgent event stream finished, emit any aggregated events data we have before
            // the final event.
            Some(a2a::StreamResponse::StatusUpdate(update))
                if update.status.state.is_terminal() =>
            {
                let mut events: Vec<ToEmit> = self
                    .aggregation_order
                    .iter()
                    .filter_map(|aid| self.aggregations.get(aid))
                    .map(|agg| ToEmit {
                        event: build_non_partial_aggregation(ctx, agg),
                        passthrough: false,
                    })
                    .collect();
                events.push(pass(event));
                events
            }
            // RemoteAgent published a snapshot which should have all the data we potentially
            // aggregated. Reset the aggregation so that it is not published twice.
            Some(a2a::StreamResponse::Task(_)) => {
                self.aggregations.clear();
                self.aggregation_order.clear();
                vec![pass(event)]
            }
            Some(a2a::StreamResponse::ArtifactUpdate(update)) => {
                let aid = update.artifact.artifact_id.clone();
                let append = update.append.unwrap_or(false);
                let last_chunk = update.last_chunk.unwrap_or(false);
                if !append {
                    // non-append event resets aggregation
                    self.remove_aggregation(&aid);
                    if last_chunk {
                        // non-append event which is the last chunk must already be non-partial
                        event.response.partial = false;
                        return vec![pass(event)];
                    }
                }

                let mut aggregation = self.aggregations.remove(&aid).unwrap_or_default();
                self.update_aggregation(&aid, &mut aggregation, &event);

                if !last_chunk {
                    self.aggregations.insert(aid, aggregation);
                    return vec![pass(event)];
                }

                // emit partial last chunk and follow by the non-partial aggregated event
                self.remove_aggregation(&aid);
                vec![
                    pass(event),
                    ToEmit {
                        event: build_non_partial_aggregation(ctx, &aggregation),
                        passthrough: false,
                    },
                ]
            }
            // do not aggregate status updates
            _ => vec![pass(event)],
        }
    }

    /// Go `removeAggregation`.
    fn remove_aggregation(&mut self, aid: &str) {
        self.aggregations.remove(aid);
        self.remove_from_order(aid);
    }

    /// Go `removeFromOrder`.
    fn remove_from_order(&mut self, aid: &str) {
        self.aggregation_order.retain(|id| id != aid);
    }

    /// Go `updateAggregation`.
    fn update_aggregation(&mut self, aid: &str, agg: &mut ArtifactAggregation, event: &Event) {
        if let Some(content) = &event.response.content {
            for part in &content.parts {
                if !part.text.is_empty() {
                    // collapse small text-block parts to bigger text blocks
                    if let Some(last) = agg.parts.last_mut() {
                        // check if last part is a text block of the same 'Thought' type
                        if !last.text.is_empty() && last.thought == part.thought {
                            last.text.push_str(&part.text);
                            continue;
                        }
                    }
                    agg.parts.push(Part {
                        text: part.text.clone(),
                        thought: part.thought,
                        ..Default::default()
                    });
                } else {
                    agg.parts.push(part.clone());
                }
            }
        }

        if let Some(citation) = &event.response.citation_metadata {
            agg.citations
                .get_or_insert_with(CitationMetadata::default)
                .citations
                .extend(citation.citations.iter().cloned());
        }
        // Go copies a non-nil map; gadk's `Map` cannot tell empty from nil, so an empty one is
        // skipped (copying nothing either way).
        if !event.response.custom_metadata.is_empty() {
            agg.custom_meta.get_or_insert_with(Map::new).extend(
                event
                    .response
                    .custom_metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            );
        }
        if let Some(grounding) = &event.response.grounding_metadata {
            agg.grounding = Some(grounding.clone());
        }
        if let Some(usage) = &event.response.usage_metadata {
            // cumulative
            agg.usage = Some(usage.clone());
        }

        self.remove_from_order(aid);
        self.aggregation_order.push(aid.to_owned());
    }

    /// Go `convertToSessionEvent`: the session event for one remote response (an error event
    /// for a transport error or a conversion failure); `None` when nothing should be emitted.
    pub(crate) async fn convert_to_session_event(
        &self,
        ctx: &InvocationContext,
        a2a_event: Option<&a2a::StreamResponse>,
        err: Option<&a2a::A2AError>,
    ) -> Result<Option<Event>, Error> {
        if let Some(err) = err {
            let mut event = to_error_event(ctx, &err.to_string());
            self.update_custom_metadata(&mut event, None);
            return Ok(Some(event));
        }
        let Some(a2a_event) = a2a_event else {
            return Ok(None);
        };

        let event = match to_session_event_with_parts(
            ctx,
            a2a_event,
            self.config.a2a_part_converter.as_ref(),
        )
        .await
        {
            Ok(event) => event,
            Err(err) => {
                let mut event = to_error_event(ctx, &format!("failed to convert a2aEvent: {err}"));
                self.update_custom_metadata(&mut event, None);
                return Ok(Some(event));
            }
        };

        Ok(event.map(|mut event| {
            let view = converted_view(a2a_event);
            self.update_custom_metadata(&mut event, Some(view.as_ref().unwrap_or(a2a_event)));
            event
        }))
    }

    /// Go `runBeforeA2ARequestCallbacks`: the first callback returning an event or an error
    /// decides.
    pub(crate) async fn run_before_request_callbacks(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Option<Event>, Error> {
        let cctx = callback_context(ctx);
        for callback in &self.config.before_request_callbacks {
            if let Some(event) = callback(&cctx, &self.request).await? {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }

    /// Go `runAfterA2ARequestCallbacks`: `Ok(None)` when there is nothing to report or no
    /// callback returned an event or an error; otherwise the first callback's answer.
    pub(crate) async fn run_after_request_callbacks(
        &self,
        ctx: &InvocationContext,
        resp: Option<&Event>,
        err: Option<&a2a::A2AError>,
    ) -> Result<Option<Event>, Error> {
        if resp.is_none() && err.is_none() {
            return Ok(None);
        }
        let cctx = callback_context(ctx);
        for callback in &self.config.after_request_callbacks {
            if let Some(event) = callback(&cctx, &self.request, resp, err).await? {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }

    /// Go `updateCustomMetadata`: records the request (on the turn-complete event only, to avoid
    /// duplicating it on every streamed event) and the response under `a2a:request` /
    /// `a2a:response`, or the encode error under `a2a:<key>_codec_error`.
    pub(crate) fn update_custom_metadata(
        &self,
        event: &mut Event,
        response: Option<&a2a::StreamResponse>,
    ) {
        let mut to_add: Vec<(&str, Result<MapStructure, Error>)> = Vec::new();
        if event.response.turn_complete {
            to_add.push(("request", to_map_structure(&self.request)));
        }
        if let Some(response) = response {
            to_add.push(("response", stream_response_map(response)));
        }
        for (key, payload) in to_add {
            match payload {
                Ok(payload) => {
                    event.response.custom_metadata.insert(
                        to_adk_meta_key(key),
                        payload.map_or(Value::Null, Value::Object),
                    );
                }
                Err(err) => {
                    event.response.custom_metadata.insert(
                        to_adk_meta_key(&format!("{key}_codec_error")),
                        Value::String(err.to_string()),
                    );
                }
            }
        }
    }
}

/// The A2A event as Go's default conversion leaves it once it produced a session event: Go's
/// `processA2AMeta` merges an artifact update's artifact metadata into the update's own
/// metadata map in place when that map exists, so every later `Meta()` read (the partial flag,
/// the transfer-to-agent value, the `a2a:response` dump) sees the merged keys. `None` when the
/// event is unchanged.
pub(crate) fn converted_view(a2a_event: &a2a::StreamResponse) -> Option<a2a::StreamResponse> {
    let a2a::StreamResponse::ArtifactUpdate(update) = a2a_event else {
        return None;
    };
    update
        .artifact
        .metadata
        .as_ref()
        .filter(|m| !m.is_empty())?;
    let visible = artifact_update_visible_meta(update)?;
    let mut update = update.clone();
    update.metadata = Some(visible);
    Some(a2a::StreamResponse::ArtifactUpdate(update))
}

/// Go `converters.ToMapStructure(event)` of an `a2a.Event`: the task, message or update itself,
/// not a2a-rs's `StreamResponse` wrapper object.
fn stream_response_map(response: &a2a::StreamResponse) -> Result<MapStructure, Error> {
    match response {
        a2a::StreamResponse::Task(task) => to_map_structure(task),
        a2a::StreamResponse::Message(msg) => to_map_structure(msg),
        a2a::StreamResponse::StatusUpdate(update) => to_map_structure(update),
        a2a::StreamResponse::ArtifactUpdate(update) => to_map_structure(update),
    }
}

/// Go `buildNonPartialAggregation`.
fn build_non_partial_aggregation(ctx: &InvocationContext, agg: &ArtifactAggregation) -> Event {
    let mut result = new_remote_agent_event(ctx);
    result.response.content = Some(Content::from_parts(agg.parts.clone(), ROLE_MODEL));
    result.response.custom_metadata = agg.custom_meta.clone().unwrap_or_default();
    result.response.grounding_metadata = agg.grounding.clone();
    result.response.citation_metadata = agg.citations.clone();
    result.response.usage_metadata = agg.usage.clone();
    result
}

/// Go `icontext.NewCallbackContext(ctx)`: the invocation as a callback context with fresh
/// actions.
pub(crate) fn callback_context(ctx: &InvocationContext) -> Context {
    ctx.clone().into_context().with_delta(ContextDelta {
        actions: Some(Arc::new(parking_lot::Mutex::new(EventActions {
            state_delta: Some(Map::new()),
            artifact_delta: Some(Default::default()),
            ..Default::default()
        }))),
        ..Default::default()
    })
}
