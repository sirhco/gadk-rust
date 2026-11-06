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

//! Live (bidirectional streaming) types (port of Go `agent/live.go`).

use std::any::Any;

use async_trait::async_trait;
use gadk_genai::{
    AudioTranscriptionConfig, Content, Modality, ProactivityConfig, RealtimeInputConfig,
    SessionResumptionConfig, SpeechConfig,
};
use gadk_model::RealtimeInput;

use crate::error::AgentError;

/// Port of Go `agent.LiveRequest`: one client message of a live session.
///
/// `realtime_input` is Go's `RealtimeInput any` (a `*genai.Blob`, `*genai.ActivityStart` or
/// `*genai.ActivityEnd`); `content` is standard text or multimodal content, or a reply to a tool
/// call when it carries a function-response part.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveRequest {
    /// Realtime input (audio/video blob or activity signal).
    pub realtime_input: Option<RealtimeInput>,
    /// Content from the user.
    pub content: Option<Content>,
}

/// Port of Go `agent.LiveSession`: the caller's handle on a running live session.
#[async_trait]
pub trait LiveSession: Send + Sync {
    /// Go `Send`: delivers `req` to the session.
    async fn send(&self, req: LiveRequest) -> Result<(), AgentError>;
    /// Go `Close`: ends the session.
    async fn close(&self) -> Result<(), AgentError>;
    /// Downcasting support (the engine reaches its own session type through it).
    fn as_any(&self) -> &dyn Any;
}

/// Port of Go `agent.LiveRunConfig`: the configuration of one live run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveRunConfig {
    /// Go `ResponseModalities`.
    pub response_modalities: Vec<Modality>,
    /// Go `SpeechConfig`.
    pub speech_config: Option<SpeechConfig>,
    /// Go `InputAudioTranscription`.
    pub input_audio_transcription: Option<AudioTranscriptionConfig>,
    /// Go `OutputAudioTranscription`.
    pub output_audio_transcription: Option<AudioTranscriptionConfig>,
    /// Go `RealtimeInputConfig` (carried; Go's flow does not send it).
    pub realtime_input_config: Option<RealtimeInputConfig>,
    /// Go `EnableAffectiveDialog` (carried; Go's flow does not send it).
    pub enable_affective_dialog: bool,
    /// Go `Proactivity` (carried; Go's flow does not send it).
    pub proactivity: Option<ProactivityConfig>,
    /// Go `SessionResumption`.
    pub session_resumption: Option<SessionResumptionConfig>,
    /// Go `SaveLiveBlob`: cache model audio and save both caches as artifacts on flush.
    pub save_live_blob: bool,
    /// Go `MaxLLMCalls` (carried; Go's `RunLive` does not apply it).
    pub max_llm_calls: i32,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::any::Any;
    use std::sync::Arc;

    use async_trait::async_trait;
    use gadk_genai::{Blob, Content};
    use gadk_model::RealtimeInput;
    use parking_lot::Mutex;

    use super::{LiveRequest, LiveRunConfig, LiveSession};
    use crate::error::AgentError;
    use crate::run_config::{RunConfig, StreamingMode};

    #[derive(Default)]
    struct Recording(Mutex<Vec<LiveRequest>>);

    #[async_trait]
    impl LiveSession for Recording {
        async fn send(&self, req: LiveRequest) -> Result<(), AgentError> {
            self.0.lock().push(req);
            Ok(())
        }
        async fn close(&self) -> Result<(), AgentError> {
            Ok(())
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[tokio::test]
    async fn live_session_is_object_safe_and_downcastable() {
        let s: Arc<dyn LiveSession> = Arc::new(Recording::default());
        let req = LiveRequest {
            realtime_input: Some(RealtimeInput::Blob(Blob {
                mime_type: "audio/pcm".into(),
                ..Default::default()
            })),
            content: Some(Content::from_text("hi", "user")),
        };
        s.send(req.clone()).await.unwrap();
        s.close().await.unwrap();
        let rec = s.as_any().downcast_ref::<Recording>().expect("downcast");
        assert_eq!(rec.0.lock().clone(), vec![req]);
    }

    #[test]
    fn defaults_match_go_zero_values() {
        assert_eq!(
            LiveRequest::default(),
            LiveRequest {
                realtime_input: None,
                content: None
            }
        );
        let c = LiveRunConfig::default();
        assert!(c.response_modalities.is_empty());
        assert!(!c.save_live_blob && !c.enable_affective_dialog);
        assert_eq!(c.max_llm_calls, 0);
        let rc = RunConfig {
            streaming_mode: StreamingMode::Bidi,
            live: Some(c.clone()),
            ..Default::default()
        };
        assert_eq!(rc.live, Some(c));
        assert_eq!(RunConfig::default().live, None);
        assert_eq!(StreamingMode::default(), StreamingMode::None);
    }
}
