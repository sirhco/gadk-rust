// Copyright 2026 Chris Olson
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

//! The reflection-shaped YAML encoding and decoding of [`Recordings`]: [`to_go_node`] builds
//! the node tree that ADK's recorder produces by encoding its recordings, and [`from_go_node`]
//! decodes the same shape.
//!
//! - [`roots`]: the recording types' encoding (yaml-tagged keys verbatim, the request view
//!   and tool call/response with every field) and their serde mirrors named after the
//!   recorded types, for decoding.
//! - [`fixups`]: the type-path fixup table (byte slices, text-marshalled values, durations,
//!   timestamps, `Schema` titles, JSON-schema values) and the struct shapes, applied by
//!   the node builder (zero-filling every field, declaration field order, the leaf classes),
//!   and their inverse on decode.
//! - [`jsonschema`]: the reshaping of a JSON-schema value into the reflection of the
//!   JSON-schema struct.
//! - [`shapes`]: the tables, generated from the fixture `tests/fixtures/fixups.json`.
//!
//! gadk types reach the builder as gadk serde's JSON (their JSON names are the recorded
//! types' JSON tags), with the exact durations laid over it; numbers in `any` positions are
//! `float64`s (`GoValue::Float`), as the recorded format's JSON decoding made them.

pub mod fixups;
pub mod jsonschema;
pub mod roots;
pub mod shapes;

use gadk_goyaml::{Document, NodeId, TypeError};

use super::recording::Recordings;

/// Encodes the recordings' mapping node (as its own document, root = the mapping), before
/// any cleaning of the YAML node.
pub fn to_go_node(recordings: &Recordings) -> Result<Document, gadk_goyaml::Error> {
    to_go_node_unmapped(recordings).map(|(doc, _)| doc)
}

/// [`to_go_node`], also returning the gadk serde keys no recorded field consumed
/// (`<type>.<json key>`; empty when gadk's serde names all match the recorded JSON tags).
pub fn to_go_node_unmapped(
    recordings: &Recordings,
) -> Result<(Document, Vec<String>), gadk_goyaml::Error> {
    let mut b = fixups::Builder::new();
    let root = roots::encode_recordings(&mut b, recordings)?;
    let mut doc = b.doc.subtree(root);
    let root = doc.root();
    doc.set_root(root);
    Ok((doc, b.unmapped))
}

/// Decodes node `at` (a document or mapping node) into recordings, after any normalization
/// the caller applied: the inverse fixups, then the reflection-shaped decode; yaml.v3's type
/// errors fail it.
pub fn from_go_node(doc: &Document, at: NodeId) -> Result<Recordings, gadk_goyaml::Error> {
    match from_go_node_partial(doc, at)? {
        (_, Some(terr)) => Err(gadk_goyaml::Error::Type(terr)),
        (recs, None) => Ok(recs),
    }
}

/// As [`from_go_node`], keeping the partly decoded value next to the yaml.v3 type error (the
/// record plugin's merge path ignores the error and keeps the value).
///
/// A civil-date text failure returns `Err` with no value, although yaml.v3 aborts but leaves
/// what it filled so far, which the merge path would keep (reachable only with default keys:
/// recorded files snake-case `citation_metadata`).
pub fn from_go_node_partial(
    doc: &Document,
    at: NodeId,
) -> Result<(Recordings, Option<TypeError>), gadk_goyaml::Error> {
    let mut work = doc.subtree(at);
    let root = work.root();
    let ours = fixups::invert(&mut work, root)?;
    let (decoded, theirs) = gadk_goyaml::from_node_partial::<roots::RecordingsDe>(
        &work,
        root,
        &roots::decode_options(),
    )?;
    Ok((decoded.into(), fixups::merge_type_errors(ours, theirs)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::conformance::recording::{LlmRecording, LlmRequestView, Recording, ToolRecording};

    fn marshal(recs: &Recordings) -> String {
        let doc = to_go_node(recs).unwrap();
        String::from_utf8(gadk_goyaml::marshal_node(&doc, doc.root()).unwrap()).unwrap()
    }

    // Expected texts: Go `yaml.Marshal` of `node.Encode(&recordings)` (no cleaning), and Go's
    // `yaml.Unmarshal` errors (an `UnmarshalText` error aborts the decode, type errors are
    // collected; Go's `into int` is gadk-goyaml's `into int64`, ledger 6c-1).
    #[test]
    fn empty_roots_encode_as_go() {
        assert_eq!(marshal(&Recordings::default()), "recordings: []\n");
        let recs = Recordings {
            recordings: vec![Recording {
                agent_name: "a".into(),
                llm_recording: Some(LlmRecording::default()),
                tool_recording: Some(ToolRecording::default()),
                ..Default::default()
            }],
        };
        assert_eq!(
            marshal(&recs),
            "recordings:\n    - user_message_index: 0\n      agent_name: a\n      llm_recording: {}\n      tool_recording: {}\n"
        );
    }

    #[test]
    fn decode_round_trips_the_roots() {
        let recs = Recordings {
            recordings: vec![Recording {
                user_message_index: 3,
                agent_name: "a".into(),
                tool_recording: Some(ToolRecording::default()),
                ..Default::default()
            }],
        };
        let doc = to_go_node(&recs).unwrap();
        assert_eq!(from_go_node(&doc, doc.root()).unwrap(), recs);
    }

    #[test]
    fn date_failures_abort_with_go_s_error() {
        let data = b"recordings:\n  - user_message_index: oops\n    agent_name: a\n    llm_recording:\n      llm_responses:\n        - citationmetadata:\n            citations:\n              - publicationdate: 2024-02-30\n";
        let doc = gadk_goyaml::unmarshal_node(data).unwrap();
        let err = from_go_node_partial(&doc, doc.root()).unwrap_err();
        assert_eq!(
            err.to_string(),
            r#"parsing time "2024-02-30": day out of range"#
        );
        let ok = b"recordings:\n  - user_message_index: oops\n    agent_name: a\n";
        let doc = gadk_goyaml::unmarshal_node(ok).unwrap();
        let (recs, terr) = from_go_node_partial(&doc, doc.root()).unwrap();
        assert_eq!(recs.recordings[0].agent_name, "a");
        assert_eq!(recs.recordings[0].user_message_index, 0);
        assert_eq!(terr.unwrap().errors.len(), 1);
    }

    /// Go `yaml.Unmarshal` into `recording.Recordings` (probe at ADK-Go `0990f9e`):
    /// an invalid `!!binary` scalar aborts with yaml.v3's error at a `[]byte` position (also as
    /// an element of a `[]byte` sequence) and at a `civil.Date` one, despite the earlier type
    /// error; a valid one at a `[]byte` position is a type error.
    #[test]
    fn invalid_binary_aborts_like_yaml_v3() {
        let head = "recordings:\n  - user_message_index: oops\n    agent_name: a\n    llm_recording:\n      llm_responses:\n";
        for tail in [
            "        - content:\n            parts:\n              - inlinedata:\n                  data: !!binary '@@@'\n",
            "        - content:\n            parts:\n              - inlinedata:\n                  data: [1, !!binary '@@@']\n",
            "        - citationmetadata:\n            citations:\n              - publicationdate: !!binary '@@@'\n",
        ] {
            let doc = gadk_goyaml::unmarshal_node(format!("{head}{tail}").as_bytes()).unwrap();
            let err = from_go_node_partial(&doc, doc.root()).unwrap_err();
            assert_eq!(
                err.to_string(),
                "yaml: !!binary value contains invalid base64 data",
                "{tail}"
            );
        }
        let ok = "recordings:\n  - user_message_index: 0\n    agent_name: a\n    llm_recording:\n      llm_responses:\n        - content:\n            parts:\n              - inlinedata:\n                  data: !!binary aGk=\n";
        let doc = gadk_goyaml::unmarshal_node(ok.as_bytes()).unwrap();
        let (_, terr) = from_go_node_partial(&doc, doc.root()).unwrap();
        assert_eq!(
            terr.unwrap().to_string(),
            "yaml: unmarshal errors:\n  line 9: cannot unmarshal !!binary `aGk=` into Vec<u8>"
        );
    }

    /// A recordings document whose response schema nests `{items: …}` `n` times (flow style,
    /// within the parser's 10000 levels); the schema's mapping is the 6th collection.
    fn deep_schema_doc(n: usize) -> Document {
        let data = format!(
            "recordings:\n  - user_message_index: 0\n    agent_name: a\n    llm_recording:\n      llm_request:\n        config:\n          responseschema: {}{{}}{}\n",
            "{items: ".repeat(n),
            "}".repeat(n)
        );
        gadk_goyaml::unmarshal_node(data.as_bytes()).unwrap()
    }

    // Plan decision D7: the inverse fixups' walk over recursive Go types (genai.Schema) is
    // capped like gadk-goyaml's walks, so a deep file fails instead of overflowing the stack.
    #[test]
    fn deep_recordings_fail_to_decode_instead_of_overflowing() {
        for n in [200, 10_000 - 10] {
            let doc = deep_schema_doc(n);
            let err = from_go_node_partial(&doc, doc.root()).unwrap_err();
            assert_eq!(
                err.to_string(),
                "yaml: exceeded max depth of 127",
                "n = {n}"
            );
        }
        // 127 collections in all (6 above the innermost schema's 121) still decode.
        let doc = deep_schema_doc(120);
        let recs = from_go_node(&doc, doc.root()).unwrap();
        let mut schema = recs.recordings[0]
            .llm_recording
            .as_ref()
            .and_then(|l| l.llm_request.as_ref())
            .and_then(|r| r.config.as_ref())
            .and_then(|c| c.response_schema.as_ref());
        let mut depth = 0;
        while let Some(s) = schema {
            depth += 1;
            schema = s.items.as_deref();
        }
        assert_eq!(depth, 121);
        let doc = deep_schema_doc(121);
        let err = from_go_node(&doc, doc.root()).unwrap_err();
        assert_eq!(err.to_string(), "yaml: exceeded max depth of 127");
    }

    // Plan decision D7: the builder's walk over recursive Go types is capped the same way.
    #[test]
    fn deep_recordings_fail_to_encode() {
        let deep = |n: usize| {
            let mut schema = gadk_genai::Schema::default();
            for _ in 0..n {
                schema = gadk_genai::Schema {
                    items: Some(Box::new(schema)),
                    ..Default::default()
                };
            }
            Recordings {
                recordings: vec![Recording {
                    agent_name: "a".into(),
                    llm_recording: Some(LlmRecording {
                        llm_request: Some(LlmRequestView {
                            config: Some(gadk_genai::GenerateContentConfig {
                                response_schema: Some(schema),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }
        };
        let err = to_go_node(&deep(200)).unwrap_err();
        assert_eq!(err.to_string(), "yaml: exceeded max depth of 127");
        assert!(to_go_node(&deep(20)).is_ok());
    }
}
