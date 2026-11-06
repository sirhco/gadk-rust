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

//! Port of Go internal/llminternal/audio_cache_manager.go.

use chrono::{DateTime, Utc};
use gadk_agent::{AgentError, InvocationContext};
use gadk_genai::{Content, FileData, Part};
use gadk_platform::Platform;
use gadk_session::Event;

use crate::runner::wrap;

/// Go AudioCacheManager's guarded fields.
struct Caches {
    input: Vec<Vec<u8>>,
    output: Vec<Vec<u8>>,
    input_start: DateTime<Utc>,
    output_start: DateTime<Utc>,
    input_mime: String,
    output_mime: String,
}

/// Port of Go AudioCacheManager: caches live audio and flushes it to the artifact service.
pub(crate) struct AudioCacheManager {
    inner: parking_lot::Mutex<Caches>,
}

impl Default for AudioCacheManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioCacheManager {
    /// Go NewAudioCacheManager: both MIME types default to `audio/pcm`.
    pub(crate) fn new() -> Self {
        Self {
            inner: parking_lot::Mutex::new(Caches {
                input: Vec::new(),
                output: Vec::new(),
                input_start: DateTime::<Utc>::default(),
                output_start: DateTime::<Utc>::default(),
                input_mime: "audio/pcm".into(),
                output_mime: "audio/pcm".into(),
            }),
        }
    }

    /// Go CacheInput: caches incoming user audio. A non-empty, non-`audio/` MIME type is ignored;
    /// the first chunk of an empty cache stamps the start time and (when non-empty) the MIME type.
    pub(crate) fn cache_input(&self, platform: &Platform, data: &[u8], mime_type: &str) {
        if !mime_type.is_empty() && !mime_type.starts_with("audio/") {
            return;
        }
        let mut c = self.inner.lock();
        if c.input.is_empty() {
            c.input_start = platform.now();
            if !mime_type.is_empty() {
                c.input_mime = mime_type.to_owned();
            }
        }
        c.input.push(data.to_vec());
    }

    /// Go CacheOutput: caches outgoing model audio, with the same rules as `cache_input`.
    pub(crate) fn cache_output(&self, platform: &Platform, data: &[u8], mime_type: &str) {
        if !mime_type.is_empty() && !mime_type.starts_with("audio/") {
            return;
        }
        let mut c = self.inner.lock();
        if c.output.is_empty() {
            c.output_start = platform.now();
            if !mime_type.is_empty() {
                c.output_mime = mime_type.to_owned();
            }
        }
        c.output.push(data.to_vec());
    }

    /// Go FlushCaches: flushes the selected caches (input first) to the artifact service and
    /// returns the created events.
    ///
    /// Go holds its mutex across the artifact save; a `parking_lot` guard cannot be held across
    /// an `.await`, so each cache is copied under the lock and saved after it is released. As in
    /// Go, a cache whose save failed is kept; on success exactly the saved chunks are removed
    /// (chunks cached during the save stay for the next flush).
    pub(crate) async fn flush_caches(
        &self,
        ctx: &InvocationContext,
        flush_user: bool,
        flush_model: bool,
    ) -> Result<Vec<Event>, AgentError> {
        let mut events = Vec::new();
        if flush_user {
            let snapshot = {
                let c = self.inner.lock();
                (!c.input.is_empty())
                    .then(|| (c.input.clone(), c.input_mime.clone(), c.input_start))
            };
            if let Some((chunks, mime, start)) = snapshot {
                let saved = chunks.len();
                // Go: on error the cache is left as it is.
                events.push(flush_cache(ctx, chunks, "input_audio", mime, start).await?);
                let mut c = self.inner.lock();
                let n = saved.min(c.input.len());
                c.input.drain(..n);
                if !c.input.is_empty() {
                    // Chunks cached during the save start a new cache (Go blocks them on the mutex).
                    c.input_start = ctx.platform().now();
                }
            }
        }
        if flush_model {
            let snapshot = {
                let c = self.inner.lock();
                (!c.output.is_empty())
                    .then(|| (c.output.clone(), c.output_mime.clone(), c.output_start))
            };
            if let Some((chunks, mime, start)) = snapshot {
                let saved = chunks.len();
                // Go: on error the cache is left as it is.
                events.push(flush_cache(ctx, chunks, "output_audio", mime, start).await?);
                let mut c = self.inner.lock();
                let n = saved.min(c.output.len());
                c.output.drain(..n);
                if !c.output.is_empty() {
                    // Chunks cached during the save start a new cache (Go blocks them on the mutex).
                    c.output_start = ctx.platform().now();
                }
            }
        }
        Ok(events)
    }
}

/// Go flushCache: saves the combined chunks as an artifact and builds the event referencing it.
async fn flush_cache(
    ctx: &InvocationContext,
    chunks: Vec<Vec<u8>>,
    cache_type: &str,
    mime_type: String,
    start: DateTime<Utc>,
) -> Result<Event, AgentError> {
    let combined: Vec<u8> = chunks.concat();
    let ext = if mime_type == "audio/mp3" {
        "mp3"
    } else {
        "pcm"
    };
    let filename = format!(
        "adk_live_audio_storage_{cache_type}_{}.{ext}",
        start.timestamp_millis()
    );

    let part = Part::from_bytes(combined.into(), &mime_type);
    // Go calls ctx.Artifacts().Save unconditionally (a nil service panics); Rust reports it.
    let artifacts = ctx.artifacts().ok_or_else(|| {
        AgentError::other("failed to save audio artifact: artifact service is not configured")
    })?;
    let resp = artifacts
        .save(&filename, part)
        .await
        .map_err(|e| wrap("failed to save audio artifact", e))?;

    let sess = ctx.session();
    let artifact_ref = format!(
        "artifact://{}/{}/{}/_adk_live/{}#{}",
        sess.app_name(),
        sess.user_id(),
        sess.id(),
        filename,
        resp.version
    );

    let (author, role) = if cache_type == "input_audio" {
        ("user".to_owned(), "user")
    } else {
        (ctx.agent().name().to_owned(), "model")
    };

    let mut ev = Event::new(ctx.platform(), ctx.invocation_id());
    ev.author = author;
    ev.timestamp = start;
    ev.response.content = Some(Content {
        role: role.into(),
        parts: vec![Part {
            file_data: Some(FileData {
                file_uri: artifact_ref,
                mime_type,
                ..Default::default()
            }),
            ..Default::default()
        }],
    });
    Ok(ev)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use gadk_agent::{AgentConfig, Artifacts, agent};
    use gadk_artifact::{ListResponse, LoadResponse, SaveResponse};
    use gadk_platform::Platform;
    use gadk_session::{CreateRequest, InMemorySessionService, SessionService};

    use super::*;

    struct Versioned(i64);

    #[async_trait]
    impl Artifacts for Versioned {
        async fn save(&self, _: &str, _: Part) -> Result<SaveResponse, AgentError> {
            Ok(SaveResponse { version: self.0 })
        }
        async fn list(&self) -> Result<ListResponse, AgentError> {
            Ok(ListResponse::default())
        }
        async fn load(&self, _: &str) -> Result<LoadResponse, AgentError> {
            Ok(LoadResponse::default())
        }
        async fn load_version(&self, _: &str, _: i64) -> Result<LoadResponse, AgentError> {
            Ok(LoadResponse::default())
        }
    }

    struct Failing;

    #[async_trait]
    impl Artifacts for Failing {
        async fn save(&self, _: &str, _: Part) -> Result<SaveResponse, AgentError> {
            Err(AgentError::other("disk full"))
        }
        async fn list(&self) -> Result<ListResponse, AgentError> {
            Ok(ListResponse::default())
        }
        async fn load(&self, _: &str) -> Result<LoadResponse, AgentError> {
            Ok(LoadResponse::default())
        }
        async fn load_version(&self, _: &str, _: i64) -> Result<LoadResponse, AgentError> {
            Ok(LoadResponse::default())
        }
    }

    async fn ctx(artifacts: Arc<dyn Artifacts>, platform: Platform) -> InvocationContext {
        let sess = InMemorySessionService::new()
            .create(CreateRequest {
                app_name: "app1".into(),
                user_id: "user1".into(),
                session_id: Some("sess1".into()),
                state: None,
            })
            .await
            .unwrap();
        InvocationContext::builder()
            .artifacts(artifacts)
            .session(sess)
            .invocation_id("inv1")
            .platform(platform)
            .agent(
                agent::new(AgentConfig {
                    name: "agent1".into(),
                    ..Default::default()
                })
                .unwrap(),
            )
            .build_invocation()
    }

    #[tokio::test]
    async fn flush_names_the_artifact_and_stamps_the_start_time() {
        let platform = Platform::deterministic(7);
        let mgr = AudioCacheManager::new();
        mgr.cache_output(&platform, b"a", "audio/mp3");
        let start = mgr.inner.lock().output_start;
        mgr.cache_output(&platform, b"b", "audio/pcm"); // MIME is fixed by the first chunk
        let ctx = ctx(Arc::new(Versioned(3)), platform).await;
        let events = mgr.flush_caches(&ctx, true, true).await.unwrap();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        let file = format!(
            "adk_live_audio_storage_output_audio_{}.mp3",
            start.timestamp_millis()
        );
        let fd = ev.response.content.as_ref().unwrap().parts[0]
            .file_data
            .clone()
            .unwrap();
        assert_eq!(
            fd.file_uri,
            format!("artifact://app1/user1/sess1/_adk_live/{file}#3")
        );
        assert_eq!(fd.mime_type, "audio/mp3");
        assert_eq!(ev.timestamp, start);
        assert_eq!(ev.invocation_id, "inv1");
        // The cache was emptied.
        assert!(mgr.flush_caches(&ctx, true, true).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn save_failure_is_reported_with_go_text() {
        let mgr = AudioCacheManager::new();
        mgr.cache_input(&Platform::default(), b"x", "audio/pcm");
        let ctx = ctx(Arc::new(Failing), Platform::default()).await;
        let err = mgr.flush_caches(&ctx, true, false).await.unwrap_err();
        assert_eq!(err.to_string(), "failed to save audio artifact: disk full");
    }
}
