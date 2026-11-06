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

//! Port of Go session/compaction/compaction.go `SummarizeResult` and `Summarizer`.

use gadk_genai::{Content, GenerateContentResponseUsageMetadata};
use gadk_model::Backend;

use crate::Event;

/// What one summarization attempt produced (Go `compaction.SummarizeResult`). The two states are
/// told apart by `content` alone: `Some` is a summary, `None` is a decline.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SummarizeResult {
    /// The summary, or `None` for a decline.
    pub content: Option<Content>,
    /// The token usage the attempt cost, or `None` when unknown. Meaningful on a decline too.
    pub usage: Option<GenerateContentResponseUsageMetadata>,
}

/// A summarization failure that still reports what it spent (Go returns
/// `SummarizeResult{Usage: u}` alongside the error). Return it boxed from
/// [`Summarizer::summarize_events`] when a model call was paid for and failed; the framework
/// records the usage on the compaction span. Display and `source()` are the wrapped error's.
#[derive(Debug)]
pub struct SummarizeFailure {
    /// The token usage the failed attempt cost.
    pub usage: Option<GenerateContentResponseUsageMetadata>,
    /// The failure.
    pub source: Box<dyn std::error::Error + Send + Sync>,
}

impl std::fmt::Display for SummarizeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.source, f)
    }
}

impl std::error::Error for SummarizeFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

/// Condenses a range of events into a single piece of content (Go `compaction.Summarizer`).
///
/// An implementation returns only the summary. The framework builds the event that carries it,
/// derives the range it covers from the events it handed over, and appends it.
///
/// **Cancellation.** The framework cancels a summarization by dropping its future. Post-invocation
/// compaction can outlast a consumer that stopped reading events, and the framework bounds only
/// the summarizer it installs by default, so an implementation that calls a model should carry its
/// own deadline.
#[async_trait::async_trait]
pub trait Summarizer: Send + Sync {
    /// Summarizes `events` into one piece of content, with the token usage the summary cost when
    /// known. The events passed in are copies; modifying them changes nothing.
    ///
    /// `Ok` with `content: None` is a decline: the range was not summarized and history is left
    /// alone. `Err` is a failure, which is reported and traced; return a boxed
    /// [`SummarizeFailure`] to report the usage a failure cost.
    async fn summarize_events(
        &self,
        events: &[Event],
    ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>>;

    /// The summarizer's bare type name, put on the compaction span as
    /// `gen_ai.compaction.summarizer_type` (Go `summarizerTypeName`). Defaults to the last `::`
    /// segment of the implementing type's path, without generic arguments.
    fn type_name(&self) -> &str {
        bare_type_name(std::any::type_name::<Self>())
    }

    /// The Google backend the summarizer's model talks to, used for the span's `gen_ai.system`
    /// (Go `summarizerBackend`). `None` omits the attribute.
    fn google_variant(&self) -> Option<Backend> {
        None
    }
}

/// The bare name of a `std::any::type_name` path: generic arguments removed, then the last `::`
/// segment (`a::b::Foo<c::D>` → `Foo`).
#[doc(hidden)]
pub fn bare_type_name(full: &'static str) -> &'static str {
    let base = full.split('<').next().unwrap_or(full);
    base.rsplit("::").next().unwrap_or(base)
}

/// The usage a failed summarization reported: `Some` when `err` is a [`SummarizeFailure`] that
/// carries usage.
#[doc(hidden)]
pub fn failure_usage<'a>(
    err: &'a (dyn std::error::Error + Send + Sync + 'static),
) -> Option<&'a GenerateContentResponseUsageMetadata> {
    err.downcast_ref::<SummarizeFailure>()
        .and_then(|f| f.usage.as_ref())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Decliner;

    #[async_trait::async_trait]
    impl Summarizer for Decliner {
        async fn summarize_events(
            &self,
            _events: &[Event],
        ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
            Ok(SummarizeResult {
                content: None,
                usage: Some(GenerateContentResponseUsageMetadata {
                    prompt_token_count: 7,
                    ..Default::default()
                }),
            })
        }
    }

    struct Generic<T>(std::marker::PhantomData<T>);

    #[async_trait::async_trait]
    impl<T: Send + Sync> Summarizer for Generic<T> {
        async fn summarize_events(
            &self,
            events: &[Event],
        ) -> Result<SummarizeResult, Box<dyn std::error::Error + Send + Sync>> {
            Ok(SummarizeResult {
                content: Some(Content::from_text(
                    format!("{} events", events.len()),
                    "model",
                )),
                usage: None,
            })
        }
        fn google_variant(&self) -> Option<Backend> {
            Some(Backend::VertexAi)
        }
    }

    #[test]
    fn bare_type_name_strips_path_and_generics() {
        assert_eq!(bare_type_name("Foo"), "Foo");
        assert_eq!(bare_type_name("a::b::fakeSummarizer"), "fakeSummarizer");
        assert_eq!(bare_type_name("a::b::Foo<c::D<e::F>>"), "Foo");
    }

    #[tokio::test]
    async fn summarizer_is_object_safe_and_names_itself() {
        let s: Arc<dyn Summarizer> = Arc::new(Decliner);
        assert_eq!(s.type_name(), "Decliner");
        assert_eq!(s.google_variant(), None);
        let res = s.summarize_events(&[]).await.unwrap();
        assert!(res.content.is_none(), "a decline has no content");
        assert_eq!(res.usage.unwrap().prompt_token_count, 7);

        let g: Arc<dyn Summarizer> = Arc::new(Generic::<String>(std::marker::PhantomData));
        assert_eq!(g.type_name(), "Generic");
        assert_eq!(g.google_variant(), Some(Backend::VertexAi));
        let res = g.summarize_events(&[Event::default()]).await.unwrap();
        assert_eq!(res.content.unwrap().parts[0].text, "1 events");
    }

    #[test]
    fn failure_usage_reads_a_summarize_failure() {
        let usage = GenerateContentResponseUsageMetadata {
            prompt_token_count: 4000,
            candidates_token_count: 8,
            ..Default::default()
        };
        let err: Box<dyn std::error::Error + Send + Sync> = Box::new(SummarizeFailure {
            usage: Some(usage.clone()),
            source: "summarizer stopped before finishing (finish reason \"MAX_TOKENS\"), so the summary is incomplete".into(),
        });
        assert_eq!(failure_usage(&*err), Some(&usage));
        assert_eq!(
            err.to_string(),
            "summarizer stopped before finishing (finish reason \"MAX_TOKENS\"), so the summary is incomplete"
        );
        assert_eq!(err.source().unwrap().to_string(), err.to_string());

        let plain: Box<dyn std::error::Error + Send + Sync> = "boom".into();
        assert_eq!(failure_usage(&*plain), None);
        let no_usage: Box<dyn std::error::Error + Send + Sync> = Box::new(SummarizeFailure {
            usage: None,
            source: "boom".into(),
        });
        assert_eq!(failure_usage(&*no_usage), None);
    }
}
