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

//! Parity port of Go internal/llminternal/audio_cache_manager_test.go.

use std::sync::Arc;

use async_trait::async_trait;
use gadk_agent::{AgentConfig, AgentError, Artifacts, InvocationContext, agent};
use gadk_artifact::{ListResponse, LoadResponse, SaveResponse};
use gadk_genai::Part;
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, InMemorySessionService, SessionService};
use parking_lot::Mutex;

use crate::llm::live::audio_cache::AudioCacheManager;

/// Go audioMockArtifacts: records the last save and returns version 1.
#[derive(Default)]
struct AudioMockArtifacts {
    saved: Mutex<Option<(String, Part)>>,
}

#[async_trait]
impl Artifacts for AudioMockArtifacts {
    async fn save(&self, name: &str, data: Part) -> Result<SaveResponse, AgentError> {
        *self.saved.lock() = Some((name.to_owned(), data));
        Ok(SaveResponse { version: 1 })
    }
    async fn list(&self) -> Result<ListResponse, AgentError> {
        Ok(ListResponse::default())
    }
    async fn load(&self, _name: &str) -> Result<LoadResponse, AgentError> {
        Ok(LoadResponse::default())
    }
    async fn load_version(&self, _name: &str, _version: i64) -> Result<LoadResponse, AgentError> {
        Ok(LoadResponse::default())
    }
}

impl AudioMockArtifacts {
    fn saved_part(&self) -> Part {
        self.saved
            .lock()
            .clone()
            .expect("Expected savedPart, got nil")
            .1
    }
}

type Verify = fn(&[Event], &AudioMockArtifacts);

struct Case {
    name: &'static str,
    inputs: Vec<(&'static [u8], &'static str)>,
    outputs: Vec<(&'static [u8], &'static str)>,
    flush_user: bool,
    flush_model: bool,
    expected_events_count: usize,
    verify: Option<Verify>,
}

fn file_mime(ev: &Event) -> String {
    ev.response.content.as_ref().unwrap().parts[0]
        .file_data
        .as_ref()
        .unwrap()
        .mime_type
        .clone()
}

fn role(ev: &Event) -> String {
    ev.response.content.as_ref().unwrap().role.clone()
}

fn inline_data(p: &Part) -> (Vec<u8>, String) {
    let b = p.inline_data.as_ref().expect("inline data");
    (b.data.to_vec(), b.mime_type.clone())
}

#[tokio::test]
async fn test_audio_cache_manager() {
    let cases = vec![
        Case {
            name: "FlushBoth_PCM",
            inputs: vec![(b"input1", "audio/pcm"), (b"input2", "audio/pcm")],
            outputs: vec![(b"output1", "audio/pcm"), (b"output2", "audio/pcm")],
            flush_user: true,
            flush_model: true,
            expected_events_count: 2,
            verify: Some(|events, _| {
                let ev1 = &events[0];
                assert!(
                    ev1.author == "user" && role(ev1) == "user",
                    "ev1 author/role mismatch: author={:?}, role={:?}",
                    ev1.author,
                    role(ev1)
                );
                assert_eq!(file_mime(ev1), "audio/pcm", "ev1 mimeType mismatch");
                let ev2 = &events[1];
                assert!(
                    ev2.author == "agent1" && role(ev2) == "model",
                    "ev2 author/role mismatch: author={:?}, role={:?}",
                    ev2.author,
                    role(ev2)
                );
                assert_eq!(file_mime(ev2), "audio/pcm", "ev2 mimeType mismatch");
            }),
        },
        Case {
            name: "FlushSelective_InputOnly",
            inputs: vec![(b"input1", "audio/pcm")],
            outputs: vec![(b"output1", "audio/pcm")],
            flush_user: true,
            flush_model: false,
            expected_events_count: 1,
            verify: Some(|events, _| assert_eq!(events[0].author, "user", "Expected author user")),
        },
        Case {
            name: "FlushSelective_OutputOnly",
            inputs: vec![(b"input1", "audio/pcm")],
            outputs: vec![(b"output1", "audio/pcm")],
            flush_user: false,
            flush_model: true,
            expected_events_count: 1,
            verify: Some(|events, _| {
                assert_eq!(events[0].author, "agent1", "Expected author agent1")
            }),
        },
        Case {
            name: "FlushEmpty",
            inputs: vec![],
            outputs: vec![],
            flush_user: true,
            flush_model: true,
            expected_events_count: 0,
            verify: None,
        },
        Case {
            name: "VerifyCombinedData",
            inputs: vec![(b"chunk1", "audio/pcm"), (b"chunk2", "audio/pcm")],
            outputs: vec![],
            flush_user: true,
            flush_model: false,
            expected_events_count: 1,
            verify: Some(|_, art| {
                assert_eq!(
                    inline_data(&art.saved_part()).0,
                    b"chunk1chunk2".to_vec(),
                    "Expected combined data"
                );
            }),
        },
        Case {
            name: "MimeTypeFallback",
            inputs: vec![(b"input1", "")],
            outputs: vec![],
            flush_user: true,
            flush_model: false,
            expected_events_count: 1,
            verify: Some(|_, art| {
                assert_eq!(
                    inline_data(&art.saved_part()).1,
                    "audio/pcm",
                    "Expected fallback MIMEType audio/pcm"
                );
            }),
        },
        Case {
            name: "DifferentMimeTypes",
            inputs: vec![(b"input1", "audio/pcm")],
            outputs: vec![(b"output1", "audio/mp3")],
            flush_user: true,
            flush_model: true,
            expected_events_count: 2,
            verify: Some(|events, _| {
                assert_eq!(
                    file_mime(&events[0]),
                    "audio/pcm",
                    "Expected input MIMEType audio/pcm"
                );
                assert_eq!(
                    file_mime(&events[1]),
                    "audio/mp3",
                    "Expected output MIMEType audio/mp3"
                );
            }),
        },
        Case {
            name: "FiltersNonAudio",
            inputs: vec![
                (b"input1", "video/mp4"),
                (b"input2", "image/png"),
                (b"audio_input", "audio/pcm"),
            ],
            outputs: vec![(b"output1", "video/h264")],
            flush_user: true,
            flush_model: true,
            expected_events_count: 1,
            verify: Some(|events, art| {
                assert_eq!(events[0].author, "user", "Expected author user");
                assert_eq!(
                    inline_data(&art.saved_part()).0,
                    b"audio_input".to_vec(),
                    "Expected only 'audio_input' to be saved"
                );
            }),
        },
    ];

    for tt in cases {
        let platform = Platform::default();
        let mgr = AudioCacheManager::new();
        for (data, mime) in &tt.inputs {
            mgr.cache_input(&platform, data, mime);
        }
        for (data, mime) in &tt.outputs {
            mgr.cache_output(&platform, data, mime);
        }

        let mock_art = Arc::new(AudioMockArtifacts::default());
        let sessions = InMemorySessionService::new();
        let sess = sessions
            .create(CreateRequest {
                app_name: "app1".into(),
                user_id: "user1".into(),
                session_id: Some("sess1".into()),
                state: None,
            })
            .await
            .unwrap();
        let ctx: InvocationContext = InvocationContext::builder()
            .artifacts(mock_art.clone())
            .session(sess)
            .invocation_id("inv1")
            .agent(
                agent::new(AgentConfig {
                    name: "agent1".into(),
                    ..Default::default()
                })
                .unwrap(),
            )
            .build_invocation();

        let events = mgr
            .flush_caches(&ctx, tt.flush_user, tt.flush_model)
            .await
            .unwrap_or_else(|e| panic!("{}: FlushCaches failed: {e}", tt.name));
        assert_eq!(
            events.len(),
            tt.expected_events_count,
            "{}: event count",
            tt.name
        );
        if let Some(verify) = tt.verify {
            verify(&events, &mock_art);
        }
    }
}
