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

//! Automatic continuation of a generation the model pauses with a continuation token.
//!
//! Responses are merged by walking the JSON encoding of the gadk-genai response types, keyed
//! by the JSON field names; field names are compared case-insensitively, so
//! `ContinuationToken` and `continuationToken` are the same field.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use gadk_genai::{
    FinishReason, GenerateContentConfig, GenerateContentResponse,
    GenerateContentResponseUsageMetadata,
};
use gadk_model::ModelError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Number, Value};

/// Maximum number of consecutive streaming reconnects that do not receive a new
/// checkpoint continuation token before the latest error since the stream last made
/// progress is surfaced to the caller (or, if there was none, the stream ends normally).
pub(crate) const MAX_STREAM_RECONNECTS_WITHOUT_PROGRESS: usize = 3;

/// Whether automatic continuation is enabled (it is by default).
pub(crate) fn should_enable(cfg: &GenerateContentConfig) -> bool {
    cfg.automatic_continuation.unwrap_or(true)
}

/// Whether the finish reason is empty: absent, or decoded from an empty string.
fn is_empty_reason(r: Option<&FinishReason>) -> bool {
    match r {
        None => true,
        Some(FinishReason::Unknown(s)) => s.is_empty(),
        Some(_) => false,
    }
}

/// Whether the finish reason is set: neither empty nor `Unspecified`.
fn is_set_reason(r: Option<&FinishReason>) -> bool {
    !is_empty_reason(r) && r != Some(&FinishReason::Unspecified)
}

/// The token, when it is not empty.
fn non_empty(token: Option<&Bytes>) -> Option<&Bytes> {
    token.filter(|t| !t.is_empty())
}

/// Whether the finish reason is resumable: per the continuation protocol, when a continuation
/// token is present the backend only sets the finish reason to empty/UNSPECIFIED
/// (intermediate checkpoint chunk) or CONTINUATION (stream end).
pub(crate) fn is_resumable_finish_reason(r: Option<&FinishReason>) -> bool {
    !is_set_reason(r) || r == Some(&FinishReason::Continuation)
}

/// Whether the stream can be resumed: continuation is enabled, there is a non-empty token
/// and a resumable finish reason, and no prompt feedback.
pub(crate) fn can_resume_stream(
    enabled: bool,
    token: Option<&Bytes>,
    reason: Option<&FinishReason>,
    has_prompt_feedback: bool,
) -> bool {
    if has_prompt_feedback {
        return false;
    }
    enabled && non_empty(token).is_some() && is_resumable_finish_reason(reason)
}

/// Clears a resumable finish reason of the first
/// candidate, so a paused chunk does not end the turn.
fn clear_intermediate_finish_reason(
    chunk: &mut GenerateContentResponse,
    enabled: bool,
    token: Option<&Bytes>,
) {
    let has_prompt_feedback = chunk.prompt_feedback.is_some();
    let Some(cand) = chunk.candidates.first_mut() else {
        return;
    };
    let reason = cand.finish_reason.as_ref();
    if is_set_reason(reason) && can_resume_stream(enabled, token, reason, has_prompt_feedback) {
        cand.finish_reason = None;
    }
}

/// The per-hop part of a [`StreamContinuation`].
#[derive(Default)]
struct StreamHop {
    /// The continuation token this hop resumed from (`None` for the first hop).
    start_token: Option<Bytes>,
    /// The latest checkpoint continuation token observed in this hop.
    token: Option<Bytes>,
    /// Whether this hop received any chunk.
    received_any_chunk: bool,
    finish_reason: Option<FinishReason>,
    has_prompt_feedback: bool,
    has_yielded_content: bool,
    /// The usage for the whole run so far: the usage of all previous hops plus this hop's
    /// latest usage metadata (within a hop each chunk's usage is the server's running
    /// total, so it replaces the previous value).
    usage: Option<GenerateContentResponseUsageMetadata>,
}

impl StreamHop {
    /// Whether this hop observed a new checkpoint token.
    fn has_new_token(&self) -> bool {
        non_empty(self.token.as_ref()).is_some_and(|t| Some(t) != self.start_token.as_ref())
    }
}

/// The state of one automatically continued streaming run, made of one or more
/// streaming requests ("hops").
///
/// Usage:
///
/// ```text
/// let mut state = StreamContinuation::new(config, enabled);
/// loop {
///     let call_config = state.start_hop();
///     // yield state.observe(chunk) for each chunk; stop at the first error (hop_err)
///     match state.end_hop(hop_err) { Ok(true) => continue, Ok(false) => end, Err(e) => yield e and end }
/// }
/// ```
pub(crate) struct StreamContinuation {
    config: GenerateContentConfig,
    enabled: bool,
    /// The usage metadata of all finished hops, summed.
    accumulated_usage_across_hops: Option<GenerateContentResponseUsageMetadata>,
    /// The token the next hop resumes from (`None` before the first hop).
    resume_token: Option<Bytes>,
    reconnects_without_progress: usize,
    /// The latest hop error since the run last made progress.
    last_error: Option<ModelError>,
    hop: StreamHop,
}

impl StreamContinuation {
    /// Creates the state for a run with the original `config`.
    pub(crate) fn new(config: GenerateContentConfig, enabled: bool) -> Self {
        Self {
            config,
            enabled,
            accumulated_usage_across_hops: None,
            resume_token: None,
            reconnects_without_progress: 0,
            last_error: None,
            hop: StreamHop::default(),
        }
    }

    /// Starts a new hop and returns the config to send its request
    /// with: the original config for the first hop, or a copy carrying the resume token.
    pub(crate) fn start_hop(&mut self) -> GenerateContentConfig {
        self.hop = StreamHop {
            start_token: self.resume_token.clone(),
            ..Default::default()
        };
        match &self.hop.start_token {
            None => self.config.clone(),
            Some(token) => prepare_config(&self.config, token.clone()),
        }
    }

    /// Records the prompt feedback, content, finish reason and
    /// continuation token of one chunk, and returns the chunk to yield, with an
    /// intermediate finish reason cleared and its usage metadata (if any) set to this
    /// hop's latest usage plus the usage of all previous hops.
    pub(crate) fn observe(
        &mut self,
        mut chunk: GenerateContentResponse,
    ) -> Result<GenerateContentResponse, ModelError> {
        let h = &mut self.hop;
        h.received_any_chunk = true;
        if chunk.prompt_feedback.is_some() {
            h.has_prompt_feedback = true;
        }
        if let Some(cand) = chunk.candidates.first() {
            if cand.content.as_ref().is_some_and(|c| !c.parts.is_empty()) {
                h.has_yielded_content = true;
            }
            let reason = cand.finish_reason.as_ref();
            if is_set_reason(reason) {
                h.finish_reason = cand.finish_reason.clone();
            }
            if let Some(token) = non_empty(cand.continuation_token.as_ref()) {
                h.token = Some(token.clone());
            } else if is_set_reason(reason) && !is_resumable_finish_reason(reason) {
                h.token = None;
            }
            clear_intermediate_finish_reason(&mut chunk, self.enabled, h.token.as_ref());
        }
        if let Some(usage) = &chunk.usage_metadata {
            match &self.accumulated_usage_across_hops {
                Some(acc) => {
                    let merged = merge_usage_metadata(acc, usage)?;
                    chunk.usage_metadata = Some(merged.clone());
                    h.usage = Some(merged);
                }
                None => h.usage = Some(usage.clone()),
            }
        }
        Ok(chunk)
    }

    /// Finishes the current hop, which ended with `hop_err` (`None` if
    /// the stream ended without an error). `Ok(true)` resumes with another hop;
    /// `Ok(false)` ends the stream normally; `Err` ends it with that error.
    ///
    /// An API error that ends the hop before it received any chunk (a rejected request)
    /// is returned at once; transport errors and clean empty closes are retried from the
    /// hop's start token, at most [`MAX_STREAM_RECONNECTS_WITHOUT_PROGRESS`] times in a
    /// row. When the run gives up, the latest error since it last made progress is
    /// returned, unless the API ended the hop with a finish reason or prompt feedback.
    ///
    /// (A dropped Rust stream just stops being polled.)
    pub(crate) fn end_hop(&mut self, hop_err: Option<ModelError>) -> Result<bool, ModelError> {
        if let Some(usage) = &self.hop.usage {
            self.accumulated_usage_across_hops = Some(usage.clone());
        }
        if let Some(err) = hop_err {
            if !self.hop.received_any_chunk && matches!(err, ModelError::Api { .. }) {
                // The request was rejected; leave it to the HTTP retry options.
                return Err(err);
            }
            self.advance();
            if self.can_resume() {
                self.last_error = Some(err);
                return Ok(true);
            }
            return Err(err);
        }

        let made_progress = self.hop.has_yielded_content || self.hop.has_new_token();
        if made_progress {
            self.last_error = None;
        }
        self.advance();
        if self.can_resume() {
            return Ok(true);
        }
        let h = &self.hop;
        let gave_up = !is_set_reason(h.finish_reason.as_ref()) && !h.has_prompt_feedback;
        if gave_up && let Some(err) = self.last_error.take() {
            return Err(err);
        }
        Ok(false)
    }

    /// Sets the resume token and reconnect count from [`next_resume_token`](Self::next_resume_token).
    fn advance(&mut self) {
        let (token, reconnects) = self.next_resume_token();
        self.resume_token = token;
        self.reconnects_without_progress = reconnects;
    }

    fn can_resume(&self) -> bool {
        can_resume_stream(
            self.enabled,
            self.resume_token.as_ref(),
            self.hop.finish_reason.as_ref(),
            self.hop.has_prompt_feedback,
        )
    }

    /// The token to resume the stream with after the current hop
    /// (`None` if it should not be resumed) and the updated count of reconnects without
    /// progress.
    fn next_resume_token(&self) -> (Option<Bytes>, usize) {
        let h = &self.hop;
        let reconnects = self.reconnects_without_progress;
        if h.has_prompt_feedback {
            return (None, 0);
        }
        if is_set_reason(h.finish_reason.as_ref()) {
            if h.finish_reason == Some(FinishReason::Continuation) && h.has_new_token() {
                return (h.token.clone(), 0);
            }
            return (None, reconnects);
        }
        if h.has_new_token() {
            return (h.token.clone(), 0);
        }
        if h.has_yielded_content || reconnects >= MAX_STREAM_RECONNECTS_WITHOUT_PROGRESS {
            return (None, reconnects);
        }
        (h.start_token.clone(), reconnects + 1)
    }
}

/// The token to continue a unary generation with.
pub(crate) fn should_continue(resp: &GenerateContentResponse) -> Option<Bytes> {
    let cand = resp.candidates.first()?;
    let token = non_empty(cand.continuation_token.as_ref())?;
    is_resumable_finish_reason(cand.finish_reason.as_ref()).then(|| token.clone())
}

/// A copy of `cfg` carrying `token`.
pub(crate) fn prepare_config(cfg: &GenerateContentConfig, token: Bytes) -> GenerateContentConfig {
    GenerateContentConfig {
        continuation_token: Some(token),
        ..cfg.clone()
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, ModelError> {
    serde_json::to_value(v).map_err(|e| ModelError::Other(Box::new(e)))
}

fn from_value<T: DeserializeOwned>(v: Value) -> Result<T, ModelError> {
    serde_json::from_value(v).map_err(|e| ModelError::Other(Box::new(e)))
}

/// Drops the continuation token, and a
/// CONTINUATION finish reason with its message.
fn clear_stale_continuation_on_candidate(cand: &Value) -> Value {
    let Value::Object(c) = cand else {
        return cand.clone();
    };
    let has_token = c.get("continuationToken").is_some_and(|t| !json_is_zero(t));
    let is_continuation =
        c.get("finishReason").and_then(Value::as_str) == Some(FinishReason::Continuation.as_str());
    if !has_token && !is_continuation {
        return cand.clone();
    }
    let mut cp = c.clone();
    cp.remove("continuationToken");
    if is_continuation {
        cp.remove("finishReason");
        cp.remove("finishMessage");
    }
    Value::Object(cp)
}

/// Merges candidates element-wise.
fn merge_candidates(prev: &[Value], curr: &[Value]) -> Result<Vec<Value>, ModelError> {
    if curr.is_empty() {
        return Ok(prev
            .iter()
            .map(clear_stale_continuation_on_candidate)
            .collect());
    }
    let max_len = prev.len().max(curr.len());
    (0..max_len)
        .map(|i| match curr.get(i).filter(|c| !c.is_null()) {
            None => Ok(prev
                .get(i)
                .map(clear_stale_continuation_on_candidate)
                .unwrap_or(Value::Null)),
            Some(c) => Ok(merge_values(prev.get(i), Some(c), "candidate", false)?
                .unwrap_or_else(|| Value::Object(Map::new()))),
        })
        .collect()
}

/// Merges the responses of every hop into one.
/// Parts and other lists are concatenated, usage metadata and counts summed, and the
/// latest hop's scalars and terminal fields (continuation token, finish reason and
/// message) win.
pub(crate) fn merge_responses(
    responses: Vec<GenerateContentResponse>,
) -> Result<GenerateContentResponse, ModelError> {
    let mut responses = responses.into_iter();
    let Some(first) = responses.next() else {
        return Ok(GenerateContentResponse::default());
    };
    let mut rest = responses.peekable();
    if rest.peek().is_none() {
        return Ok(first);
    }
    let mut merged = to_value(&first)?;
    for curr in rest {
        let curr = to_value(&curr)?;
        let prev_candidates = candidates_of(&merged).to_vec();
        merged = merge_values(Some(&merged), Some(&curr), "", false)?
            .unwrap_or_else(|| Value::Object(Map::new()));
        if !prev_candidates.is_empty()
            && candidates_of(&curr).is_empty()
            && let Value::Object(m) = &mut merged
        {
            m.insert(
                "candidates".into(),
                Value::Array(merge_candidates(&prev_candidates, &[])?),
            );
        }
    }
    from_value(merged)
}

fn candidates_of(v: &Value) -> &[Value] {
    v.get("candidates")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Merges usage metadata: every number is summed, and token details are grouped by
/// modality.
fn merge_usage_metadata(
    prev: &GenerateContentResponseUsageMetadata,
    curr: &GenerateContentResponseUsageMetadata,
) -> Result<GenerateContentResponseUsageMetadata, ModelError> {
    let merged = merge_values(
        Some(&to_value(prev)?),
        Some(&to_value(curr)?),
        "usageMetadata",
        true,
    )?;
    from_value(merged.unwrap_or_else(|| Value::Object(Map::new())))
}

/// Fields that are byte strings at a level the merge recurses into: their JSON is
/// base64, but the merge concatenates the bytes
/// (`SearchEntryPoint.SDKBlob`; `continuationToken` is a terminal hop field instead).
fn is_bytes_field(field: &str) -> bool {
    field.eq_ignore_ascii_case("sdkblob") || field.eq_ignore_ascii_case("sdk_blob")
}

/// Concatenates two base64-encoded byte strings.
fn concat_base64(field: &str, prev: &str, curr: &str) -> Result<Value, ModelError> {
    let decode = |s: &str| {
        BASE64.decode(s).map_err(|e| ModelError::Decode {
            message: format!("merging continuation responses: invalid base64 in {field}: {e}"),
            body_prefix: String::new(),
        })
    };
    let mut bytes = decode(prev)?;
    bytes.extend(decode(curr)?);
    Ok(Value::String(BASE64.encode(bytes)))
}

/// Whether `field` is one whose latest-hop value wins.
fn is_terminal_hop_field(field: &str) -> bool {
    let f = field.to_ascii_lowercase();
    matches!(
        f.as_str(),
        "continuationtoken"
            | "continuation_token"
            | "finishreason"
            | "finish_reason"
            | "finishmessage"
            | "finish_message"
    )
}

/// Whether `field` is a token-details list.
fn is_tokens_details_field(field: &str) -> bool {
    let f = field.to_ascii_lowercase();
    f.ends_with("tokensdetails") || f.ends_with("tokens_details")
}

/// Whether `field` is the safety-ratings list.
fn is_safety_ratings_field(field: &str) -> bool {
    let f = field.to_ascii_lowercase();
    f == "safetyratings" || f == "safety_ratings"
}

/// Whether `field` is the candidates list.
fn is_candidates_field(field: &str) -> bool {
    field.eq_ignore_ascii_case("candidates")
}

/// Whether `field` is a count or sum, which are summed.
fn is_count_or_sum_field(field: &str) -> bool {
    let f = field.to_ascii_lowercase();
    f == "tokencount" || f == "token_count" || f.ends_with("count") || f.ends_with("sum")
}

fn is_usage_metadata_field(field: &str) -> bool {
    field.eq_ignore_ascii_case("usagemetadata") || field.eq_ignore_ascii_case("usage_metadata")
}

/// Whether the encoded value is zero: absent fields, `null`, `false`, `0`,
/// `""` and empty lists. Objects are never zero.
fn json_is_zero(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(_) => false,
    }
}

/// Merges two JSON values; `None` is an absent field. Fails only on
/// an invalid base64 byte field.
fn merge_values(
    prev: Option<&Value>,
    curr: Option<&Value>,
    field: &str,
    in_usage: bool,
) -> Result<Option<Value>, ModelError> {
    // Rule 1: terminal hop fields always take curr (even if zero / absent).
    if is_terminal_hop_field(field) {
        return Ok(curr.cloned());
    }

    // Preserved candidates are merged against an empty candidate when curr has none, so
    // terminal continuation state is cleared before rule 2.
    if is_candidates_field(field)
        && let Some(Value::Array(p)) = prev
        && !p.is_empty()
        && !matches!(curr, Some(Value::Array(c)) if !c.is_empty())
    {
        let empty = Value::Object(Map::new());
        let merged = p
            .iter()
            .map(|pc| {
                Ok(merge_values(Some(pc), Some(&empty), "candidate", in_usage)?
                    .unwrap_or(Value::Null))
            })
            .collect::<Result<Vec<_>, ModelError>>()?;
        return Ok(Some(Value::Array(merged)));
    }

    // Rule 2: null / absent handling: keep the non-zero side.
    let curr = match curr.filter(|c| !json_is_zero(c)) {
        None => return Ok(prev.cloned()),
        Some(c) => c,
    };
    let prev = match prev.filter(|p| !json_is_zero(p)) {
        None => return Ok(Some(curr.clone())),
        Some(p) => p,
    };

    Ok(match (prev, curr) {
        // Objects.
        (Value::Object(p), Value::Object(c)) => {
            let shifted;
            let c = if field == "candidate" {
                shifted = shift_candidate_grounding_supports(p, c);
                shifted.as_ref().unwrap_or(c)
            } else {
                c
            };
            let in_usage = in_usage || is_usage_metadata_field(field);
            let mut out = Map::new();
            let keys = p.keys().chain(c.keys().filter(|k| !p.contains_key(*k)));
            for key in keys {
                if let Some(v) = merge_values(p.get(key), c.get(key), key, in_usage)?
                    && !v.is_null()
                {
                    out.insert(key.clone(), v);
                }
            }
            Some(Value::Object(out))
        }
        (Value::Array(p), Value::Array(c)) => {
            // Keyed-list exception 1: candidates, merged element-wise.
            if is_candidates_field(field) {
                return Ok(Some(Value::Array(merge_candidates(p, c)?)));
            }
            // Keyed-list exception 2: modality token counts, grouped by modality.
            if is_tokens_details_field(field) {
                return Ok(Some(merge_modality_token_counts(p, c)));
            }
            // Keyed-list exception 3: safety ratings, latest per category.
            if is_safety_ratings_field(field) {
                return Ok(Some(merge_safety_ratings(p, c)));
            }
            // Default: concatenate prev + curr in order.
            Some(Value::Array(p.iter().chain(c).cloned().collect()))
        }
        // Byte-string fields (base64 strings here): concatenated.
        (Value::String(p), Value::String(c)) if is_bytes_field(field) => {
            Some(concat_base64(field, p, c)?)
        }
        // Numbers: summed inside usage metadata and in token count / *Count / *Sum fields.
        (Value::Number(p), Value::Number(c)) if in_usage || is_count_or_sum_field(field) => {
            Some(sum_numbers(p, c).unwrap_or_else(|| Value::Number(c.clone())))
        }
        // Other scalars: the latest hop wins.
        (_, c) => Some(c.clone()),
    })
}

fn sum_numbers(p: &Number, c: &Number) -> Option<Value> {
    if let (Some(a), Some(b)) = (p.as_i64(), c.as_i64()) {
        return Some(Value::from(a.wrapping_add(b)));
    }
    if let (Some(a), Some(b)) = (p.as_u64(), c.as_u64()) {
        return Some(Value::from(a.wrapping_add(b)));
    }
    Number::from_f64(p.as_f64()? + c.as_f64()?).map(Value::Number)
}

/// Groups by modality, in first-seen order, summing
/// token counts.
fn merge_modality_token_counts(prev: &[Value], curr: &[Value]) -> Value {
    let mut order: Vec<(String, i64)> = Vec::new();
    for item in prev.iter().chain(curr) {
        let Value::Object(item) = item else {
            continue;
        };
        let modality = item
            .get("modality")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let count = item
            .get("tokenCount")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        match order.iter_mut().find(|(m, _)| m == modality) {
            Some((_, total)) => *total = total.wrapping_add(count),
            None => order.push((modality.to_string(), count)),
        }
    }
    Value::Array(
        order
            .into_iter()
            .map(|(modality, count)| {
                let mut out = Map::new();
                if !modality.is_empty() {
                    out.insert("modality".into(), Value::String(modality));
                }
                if count != 0 {
                    out.insert("tokenCount".into(), Value::from(count));
                }
                Value::Object(out)
            })
            .collect(),
    )
}

/// Groups by category, in first-seen order, keeping the
/// latest rating.
fn merge_safety_ratings(prev: &[Value], curr: &[Value]) -> Value {
    let mut order: Vec<(String, Value)> = Vec::new();
    for item in prev.iter().chain(curr) {
        let Value::Object(obj) = item else {
            continue;
        };
        let category = obj
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match order.iter_mut().find(|(c, _)| c == category) {
            Some((_, latest)) => *latest = item.clone(),
            None => order.push((category.to_string(), item.clone())),
        }
    }
    Value::Array(order.into_iter().map(|(_, v)| v).collect())
}

fn array_len(v: Option<&Value>) -> usize {
    v.and_then(Value::as_array).map_or(0, Vec::len)
}

/// Offsets curr's grounding supports by prev's
/// grounding chunks and content parts, which come first in the merged candidate.
/// `None` means curr is unchanged.
fn shift_candidate_grounding_supports(
    prev: &Map<String, Value>,
    curr: &Map<String, Value>,
) -> Option<Map<String, Value>> {
    let supports = curr
        .get("groundingMetadata")?
        .get("groundingSupports")?
        .as_array()
        .filter(|s| !s.is_empty())?;
    let chunk_offset = array_len(
        prev.get("groundingMetadata")
            .and_then(|g| g.get("groundingChunks")),
    ) as i64;
    let part_offset = array_len(prev.get("content").and_then(|c| c.get("parts"))) as i64;
    if chunk_offset == 0 && part_offset == 0 {
        return None;
    }
    let shifted: Vec<Value> = supports
        .iter()
        .map(|s| {
            let Value::Object(s) = s else {
                return s.clone();
            };
            let mut s = s.clone();
            if chunk_offset > 0
                && let Some(Value::Array(indices)) = s.get_mut("groundingChunkIndices")
            {
                for idx in indices.iter_mut() {
                    if let Some(i) = idx.as_i64() {
                        *idx = Value::from(i + chunk_offset);
                    }
                }
            }
            if part_offset > 0
                && let Some(Value::Object(seg)) = s.get_mut("segment")
            {
                let part_index = seg.get("partIndex").and_then(Value::as_i64).unwrap_or(0);
                seg.insert("partIndex".into(), Value::from(part_index + part_offset));
            }
            Value::Object(s)
        })
        .collect();
    let mut out = curr.clone();
    if let Some(Value::Object(gm)) = out.get_mut("groundingMetadata") {
        gm.insert("groundingSupports".into(), Value::Array(shifted));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn resp(v: Value) -> GenerateContentResponse {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn resumable_finish_reasons() {
        assert!(is_resumable_finish_reason(None));
        assert!(is_resumable_finish_reason(Some(&FinishReason::Unspecified)));
        assert!(is_resumable_finish_reason(Some(
            &FinishReason::Continuation
        )));
        assert!(is_resumable_finish_reason(Some(&FinishReason::Unknown(
            String::new()
        ))));
        assert!(!is_resumable_finish_reason(Some(&FinishReason::Stop)));
        assert!(!is_resumable_finish_reason(Some(&FinishReason::MaxTokens)));
    }

    #[test]
    fn can_resume_stream_rules() {
        let t = Bytes::from_static(b"t");
        assert!(can_resume_stream(true, Some(&t), None, false));
        assert!(!can_resume_stream(true, Some(&t), None, true));
        assert!(!can_resume_stream(false, Some(&t), None, false));
        assert!(!can_resume_stream(true, Some(&Bytes::new()), None, false));
        assert!(!can_resume_stream(true, None, None, false));
        assert!(!can_resume_stream(
            true,
            Some(&t),
            Some(&FinishReason::Stop),
            false
        ));
    }

    #[test]
    fn should_enable_defaults_on() {
        assert!(should_enable(&GenerateContentConfig::default()));
        let off = GenerateContentConfig {
            automatic_continuation: Some(false),
            ..Default::default()
        };
        assert!(!should_enable(&off));
    }

    #[test]
    fn merge_usage_and_token_details() {
        let a = resp(json!({"usageMetadata": {
            "promptTokenCount": 10, "trafficType": "ON_DEMAND",
            "promptTokensDetails": [{"modality": "TEXT", "tokenCount": 3}, {"modality": "IMAGE", "tokenCount": 1}]
        }}));
        let b = resp(json!({"usageMetadata": {
            "promptTokenCount": 5, "candidatesTokenCount": 2,
            "promptTokensDetails": [{"modality": "TEXT", "tokenCount": 4}, {"modality": "AUDIO", "tokenCount": 2}]
        }}));
        let merged = merge_responses(vec![a, b]).unwrap();
        assert_eq!(
            serde_json::to_value(merged.usage_metadata).unwrap(),
            json!({
                "promptTokenCount": 15, "candidatesTokenCount": 2, "trafficType": "ON_DEMAND",
                "promptTokensDetails": [
                    {"modality": "TEXT", "tokenCount": 7},
                    {"modality": "IMAGE", "tokenCount": 1},
                    {"modality": "AUDIO", "tokenCount": 2}
                ]
            })
        );
    }

    #[test]
    fn merge_candidates_terminal_fields_and_lists() {
        let a = resp(json!({"modelVersion": "v1", "candidates": [{
            "content": {"role": "model", "parts": [{"text": "a"}]},
            "finishReason": "CONTINUATION", "finishMessage": "paused", "continuationToken": "dA==",
            "tokenCount": 2, "index": 0, "avgLogprobs": 0.5,
            "safetyRatings": [{"category": "HARM_CATEGORY_HARASSMENT", "probability": "LOW"},
                              {"category": "HARM_CATEGORY_HATE_SPEECH", "probability": "LOW"}]
        }]}));
        let b = resp(json!({"modelVersion": "v2", "candidates": [{
            "content": {"role": "model", "parts": [{"text": "b"}]},
            "finishReason": "STOP", "tokenCount": 3, "avgLogprobs": 0.25,
            "safetyRatings": [{"category": "HARM_CATEGORY_HARASSMENT", "probability": "HIGH"}]
        }]}));
        let merged = merge_responses(vec![a, b]).unwrap();
        assert_eq!(
            serde_json::to_value(merged).unwrap(),
            json!({"modelVersion": "v2", "candidates": [{
                "content": {"role": "model", "parts": [{"text": "a"}, {"text": "b"}]},
                "finishReason": "STOP", "tokenCount": 5, "avgLogprobs": 0.25,
                "safetyRatings": [{"category": "HARM_CATEGORY_HARASSMENT", "probability": "HIGH"},
                                  {"category": "HARM_CATEGORY_HATE_SPEECH", "probability": "LOW"}]
            }]})
        );
    }

    #[test]
    fn merge_without_curr_candidates_clears_stale_continuation() {
        let a = resp(json!({"candidates": [{
            "content": {"role": "model", "parts": [{"text": "a"}]},
            "finishReason": "CONTINUATION", "finishMessage": "m", "continuationToken": "dA=="
        }]}));
        let b = resp(json!({"usageMetadata": {"promptTokenCount": 1}}));
        let merged = merge_responses(vec![a, b]).unwrap();
        assert_eq!(
            serde_json::to_value(merged).unwrap(),
            json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "a"}]}}],
                   "usageMetadata": {"promptTokenCount": 1}})
        );
    }

    #[test]
    fn merge_shifts_grounding_supports() {
        let a = resp(json!({"candidates": [{
            "content": {"role": "model", "parts": [{"text": "a"}, {"text": "b"}]},
            "groundingMetadata": {"groundingChunks": [{"web": {"uri": "u1"}}]}
        }]}));
        let b = resp(json!({"candidates": [{
            "content": {"role": "model", "parts": [{"text": "c"}]},
            "groundingMetadata": {
                "groundingChunks": [{"web": {"uri": "u2"}}],
                "groundingSupports": [{"groundingChunkIndices": [0], "segment": {"startIndex": 1}}]
            }
        }]}));
        let merged = serde_json::to_value(merge_responses(vec![a, b]).unwrap()).unwrap();
        let gm = &merged["candidates"][0]["groundingMetadata"];
        assert_eq!(
            gm["groundingSupports"],
            json!([{"groundingChunkIndices": [1], "segment": {"startIndex": 1, "partIndex": 2}}])
        );
        assert_eq!(
            gm["groundingChunks"],
            json!([{"web": {"uri": "u1"}}, {"web": {"uri": "u2"}}])
        );
    }

    #[test]
    fn merge_concatenates_sdk_blob_bytes() {
        let hop = |text: &str, blob: &'static [u8]| {
            resp(json!({"candidates": [{
                "content": {"role": "model", "parts": [{"text": text}]},
                "groundingMetadata": {"searchEntryPoint": {
                    "renderedContent": text, "sdkBlob": BASE64.encode(blob)
                }}
            }]}))
        };
        let merged = merge_responses(vec![hop("a", b"\x01\x02"), hop("b", b"\x03")]).unwrap();
        let sep = merged.candidates[0]
            .grounding_metadata
            .as_ref()
            .unwrap()
            .search_entry_point
            .as_ref()
            .unwrap();
        assert_eq!(sep.sdk_blob, Bytes::from_static(b"\x01\x02\x03"));
        assert_eq!(sep.rendered_content, "b", "other scalars: latest wins");

        // A hop without a blob keeps the previous one (rule 2).
        let without = resp(json!({"candidates": [{
            "content": {"role": "model", "parts": [{"text": "c"}]},
            "groundingMetadata": {"searchEntryPoint": {"renderedContent": "c"}}
        }]}));
        let merged = merge_responses(vec![hop("a", b"\x01"), without]).unwrap();
        let sep = merged.candidates[0]
            .grounding_metadata
            .as_ref()
            .unwrap()
            .search_entry_point
            .as_ref()
            .unwrap();
        assert_eq!(sep.sdk_blob, Bytes::from_static(b"\x01"));
    }

    #[test]
    fn merge_invalid_base64_bytes_is_an_error() {
        let got = merge_values(
            Some(&json!({"sdkBlob": "AQ=="})),
            Some(&json!({"sdkBlob": "not base64!"})),
            "searchEntryPoint",
            false,
        );
        assert!(matches!(got, Err(ModelError::Decode { .. })), "{got:?}");
    }

    #[test]
    fn merge_single_and_empty() {
        let a = resp(json!({"responseId": "r"}));
        assert_eq!(merge_responses(vec![a.clone()]).unwrap(), a);
        assert_eq!(
            merge_responses(vec![]).unwrap(),
            GenerateContentResponse::default()
        );
    }

    #[test]
    fn should_continue_rules() {
        let paused = resp(
            json!({"candidates": [{"finishReason": "CONTINUATION", "continuationToken": "dA=="}]}),
        );
        assert_eq!(should_continue(&paused), Some(Bytes::from_static(b"t")));
        let stopped = resp(
            json!({"candidates": [{"finishReason": "MAX_TOKENS", "continuationToken": "dA=="}]}),
        );
        assert_eq!(should_continue(&stopped), None);
        assert_eq!(should_continue(&GenerateContentResponse::default()), None);
    }

    #[test]
    fn stream_usage_accumulates_across_hops() {
        let mut s = StreamContinuation::new(GenerateContentConfig::default(), true);
        let cfg = s.start_hop();
        assert_eq!(cfg.continuation_token, None);
        let c = s
            .observe(resp(json!({
                "candidates": [{"content": {"parts": [{"text": "a"}]}, "finishReason": "CONTINUATION", "continuationToken": "dA=="}],
                "usageMetadata": {"promptTokenCount": 10}
            })))
            .unwrap();
        assert_eq!(
            c.candidates[0].finish_reason, None,
            "intermediate reason cleared"
        );
        assert!(s.end_hop(None).unwrap());
        let cfg = s.start_hop();
        assert_eq!(cfg.continuation_token, Some(Bytes::from_static(b"t")));
        let c = s
            .observe(resp(json!({
                "candidates": [{"content": {"parts": [{"text": "b"}]}, "finishReason": "STOP"}],
                "usageMetadata": {"promptTokenCount": 10}
            })))
            .unwrap();
        assert_eq!(c.usage_metadata.unwrap().prompt_token_count, 20);
        assert!(!s.end_hop(None).unwrap());
    }

    #[test]
    fn stream_clean_empty_closes_surface_last_error() {
        let mut s = StreamContinuation::new(GenerateContentConfig::default(), true);
        s.start_hop();
        s.observe(resp(
            json!({"candidates": [{"content": {"parts": [{"text": "a"}]}, "continuationToken": "dA=="}]}),
        ))
        .unwrap();
        assert!(s.end_hop(None).unwrap());
        // A drop, then clean empty closes until the budget runs out.
        s.start_hop();
        assert!(
            s.end_hop(Some(ModelError::Transport("drop".into())))
                .unwrap()
        );
        s.start_hop();
        assert!(s.end_hop(None).unwrap());
        s.start_hop();
        assert!(s.end_hop(None).unwrap());
        s.start_hop();
        assert!(matches!(s.end_hop(None), Err(ModelError::Transport(_))));
    }
}
