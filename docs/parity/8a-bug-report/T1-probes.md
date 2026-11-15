# 8a bug report — T1: Go probes re-run at `0990f9e`

Every `scripts/gen-go-*` probe (and the copied ADK-Go httprr files, the configurable integration
overlay and `encode_shapes.rs`) was re-run with ADK-Go at `0990f9e2cb9df71c579db6c9731efa403641fc51`.
No fixture count changed. Each changed golden is *Go output changed*; the last column names the gadk
test over it and whether it still passes (a failing test keeps the new golden and carries
`#[ignore = "8a bug: …"]`). Fixtures not listed here were reproduced byte for byte (the 80
telemetry-schema goldens, the 22 Vertex replays, the 10 `go_events`, the 29 server Agent Engine
fixtures, the goyaml corpus/goldens and float table, the 2 configurable integration goldens, the
other 44 httprr files and the remaining wire/launcher/agentanalytics fixtures).

## Changed fixtures

| fixture | Go cause | verdict | gadk test |
|---|---|---|---|
| `crates/gadk-engine/tests/testdata/agent/llmagent/testdata/TestDelegation_03_ChatToSingleTurn.httprr` | ab78c5ae (#2128) re-recorded: a single_turn sub-agent's transcript stays out of the root agent's context | *Go output changed* | `agent_llmagent_llmagent_delegation::test_delegation_03_chat_to_single_turn` — fails, ignored (*real gadk bug*) |
| `crates/gadk-engine/tests/testdata/agent/llmagent/testdata/TestDelegation_05_ChatChainedThroughChatToTask.httprr` | 1ced38ba (#2281): `transfer_to_agent`'s empty result is sent as `{"result": null}` | *Go output changed* | `agent_llmagent_llmagent_delegation::test_delegation_05_chat_chained_through_chat_to_task` — fails, ignored (*real gadk bug*) |
| `crates/gadk-engine/tests/testdata/agent/llmagent/testdata/TestDelegation_09_ChatPeerTransferAcrossSiblings.httprr` | 1ced38ba (#2281) | *Go output changed* | `agent_llmagent_llmagent_delegation::test_delegation_09_chat_peer_transfer_across_siblings` — fails, ignored (*real gadk bug*) |
| `crates/gadk-engine/tests/testdata/plugin/functioncallmodifier/testdata/TestPluginCallbackIntegration_transfer_to_agent_tool.httprr` | 1ced38ba (#2281) | *Go output changed* | `plugin_functioncallmodifier_integration::test_plugin_callback_integration::case_3_transfer_to_agent_tool` — fails, ignored (*real gadk bug*) |
| `crates/gadk-server/testdata/wire/main_03_version.json`, `strict_02_version_no_credentials.json` | 826b5e2c (#2136): ADK-Go version 2.7.0 → 2.7.1 | *Go output changed* | `wire::wire_main_matches_go` / `wire_strict_matches_go` — these two fixtures pass (the tests are ignored for the rows below) |
| `crates/gadk-server/testdata/wire/main_08_session_list.json` | 8be7e250 (#1919): session List ordered by last update time ascending | *Go output changed* | `server_adkrest_controllers_sessions::session_handlers_match_go_wire_fixtures`, `wire::wire_main_matches_go` — fail, ignored (*real gadk bug*) |
| `crates/gadk-server/testdata/wire/main_09_session_patch.json`, `main_12_run_hello.json`, `main_13_run_save_note.json`, `main_14_run_save_note_again.json`, `main_18_run_sse_hello.json`, `main_23_run_pipeline.json` | 4db0bd99 (#1870): REST `Event` carries `timestamp` | *Go output changed* | `server_adkrest_controllers_sessions::session_handlers_match_go_wire_fixtures`, `wire::wire_main_matches_go` — fail, ignored (*real gadk bug*) |
| `crates/gadk-server/testdata/wire/strict_11_run.json` | 4db0bd99 (#1870) | *Go output changed* | `wire::wire_strict_matches_go` — fails, ignored (*real gadk bug*) |
| `crates/gadk-server/testdata/wire/main_32_debug_trace_event.json`, `main_35_debug_trace_session.json`, `main_36_debug_trace_session_dev.json` | c377836c (#1988): debug span attributes keep their types (ints, string arrays) instead of strings; trace/span ids and wall-clock times re-recorded | *Go output changed* | `wire::wire_main_matches_go` — fails, ignored (*real gadk bug*) |
| `crates/gadk-server/testdata/wire/main_33_debug_trace_event_dev.json` | re-recorded trace/span ids and wall-clock times only (no attribute of a changed type) | *Go output changed* (cosmetic) | `wire::wire_main_matches_go` — this fixture passes |
| `crates/gadk-launcher/testdata/wire/full_28_a2a_card.json` | 1c354701 (#2110): agent card skills no longer append the LLM agent's instruction | *Go output changed* | `wire_replay::wire_fixtures_replay`, `web::a2a::tests::agent_card_is_gos_without_the_0_3_fields` — fail, ignored (*real gadk bug*) |
| `crates/gadk-launcher/testdata/wire/full_30_a2a_v1_send_without_version.json` | a2a-go v2.6.0 → v2.7.0 (ADK-Go deps bump d9d7c3ba #2108 / ab42390c #2208): a request without `A2A-Version` is refused with `-32009 VERSION_NOT_SUPPORTED` (`requested "0.3", served "1.0"`) | *Go output changed* | `wire_replay::wire_fixtures_replay` — fails, ignored (*real gadk bug*) |
| `crates/gadk-launcher/testdata/agentengine/wire_agentengine_02_stream_query.json` | c00807e6 (#2199): Agent Engine event timestamps keep fractional seconds (so the probe records `body_length: null`) | *Go output changed* | `web::agentengine::tests::the_api_mount_matches_go_wire_fixtures` — fails, ignored (*real gadk bug*) |
| `crates/gadk-a2a/testdata/wire/agent_card_workflow.json` | 1c354701 (#2110) skill descriptions; d6bb1e86 (#2102) the remote agent's finished-task event has `skipSummarization: true` | *Go output changed* | `wire_replay::wire_agent_card_workflow` — fails, ignored (*real gadk bug*) |
| `crates/gadk-a2a/testdata/wire/cancel_input_required_with_child.json` | 1ced38ba (#2281) `{"result": null}`; a2a-go v2.7.0 client `Accept` headers and REST `application/a2a+json` | *Go output changed* | `wire_replay::wire_cancel_input_required_with_child` — fails, ignored (*real gadk bug*) |
| `crates/gadk-a2a/testdata/wire/failed_task.json`, `streaming_partials_per_event.json`, `streaming_partials_per_run.json` | 1c354701 (#2110) card skill descriptions; a2a-go v2.7.0 client `Accept` (`text/event-stream, application/json`; `application/a2a+json, application/json`) and REST `Content-Type: application/a2a+json` | *Go output changed* | `wire_replay::wire_failed_task`, `wire_streaming_partials_per_event`, `wire_streaming_partials_per_run` — fail, ignored (*real gadk bug*; also stale `*.deltas.json` Content-Type entries) |
| `crates/gadk-a2a/testdata/wire/input_required_resume.json`, `text_reply.json` | as above, plus d6bb1e86 (#2102) `skipSummarization` | *Go output changed* | `wire_replay::wire_input_required_resume`, `wire_text_reply` — fail, ignored (*real gadk bug*) |
| `crates/gadk-session-database/tests/testdata/rows.sqlite.sql` | 7af0f5f0 (#1774): `Delete` removes the session's events explicitly (the `e-orphan` row is gone) | *Go output changed* | `go_compat_rows::sqlite_rows_match_go` — fails, ignored (*real gadk bug*) |
| `crates/gadk-agentanalytics/tests/fixtures/helpers/path_segment.json`, `object_path.json`, `gcs_object_path.json` | 11f4e29f (#2018): sanitized GCS path segments get a `~<sha256>` suffix | *Go output changed* | `offload::uploads::tests::{path_segment_matches_go, object_path_matches_go, gcs_object_path_matches_go}` — fail, ignored (*real gadk bug*) |
| `crates/gadk-agentanalytics/tests/fixtures/plugin/transfer.json` | 1ced38ba (#2281) | *Go output changed* | `golden_plugin_rows::golden_transfer` — fails, ignored (*real gadk bug*) |
| `crates/gadk-agentanalytics/tests/fixtures/redaction/text_corpus.json` | 5b3d897b (#2249), f55660a6 (#2246): `json_formatter_test.go` gained string literals (1030 → 1341 cases) | *Go output changed* | `redaction::tests::golden_text_corpus_matches_go`, `golden_formatter::smart_truncate_matches_redaction_corpus` — pass with the case count updated to 1341 (*Go-only*) |
| `crates/gadk-agentanalytics/tests/fixtures/gapic/metadata.json`, `gcs/*.json` (12) | ADK-Go deps bump (#2208) through MVS: bigquery v1.79.0 → v1.85.0, storage v1.69.0 → v1.70.0, api v0.304.0 → v0.310.0, gax v2.26.2 → v2.27.0, grpc v1.84.0 → v1.85.0 (version strings in `x-goog-api-client` / `user-agent`) | *Go output changed* (cosmetic) | gapic / gcs offloader tests — pass |
| `crates/gadk-configurable/tests/fixtures/fixups.json`, `recordings/tool-args/generated-recordings.yaml` | genai v1.75.0 → v1.77.0 (#2108): `GenerateContentConfig.GenerationOutputConfig`, `FunctionResponsePart.MediaProcessing` (120 shapes, 218 shape paths) | *Go output changed* | `recordings_differential::{fixup_tables_equal_probe, shapes_rs_is_generated_from_fixture, record_bytes_equal_go}`, `conformance::record::tests::save_recordings_writes_go_bytes_for_the_corpus` — pass after regenerating `src/conformance/gonode/shapes.rs` (`GADK_GONODE_REGEN=1`) |
| `crates/gadk-server/src/agentengine/encode_shapes.rs` (generated) | genai v1.77.0 `FunctionResponsePart.MediaProcessing`; 5de88a7f (#2114) `EventActions.RequestedAuthConfigs` (`map[string]toolauth.Config`) and the 10 `toolauth` shapes: 78 → 88 structs, 349 → 435 fields | *Go output changed* | all gadk-server Agent Engine tests pass (the encoder is shape-driven; gadk events have no `requestedAuthConfigs` yet) |

## Probe changes

- Every probe module (`gen-go-{a2a,adkrest,agentengine,configurable,events,launcher,sessiondb}`)
  re-tidied against ADK-Go `0990f9e` (genai v1.77.0, a2a-go v2.7.0, grpc v1.85.0, …);
  `gen-go-agentanalytics/module.go.{mod,sum}` refreshed by its documented procedure;
  `gen-go-vertexreplay` and `gen-go-goyaml` unchanged.
- `gen-go-a2a`: its raw server-direction requests mirror a2aclient v2.7.0's `Accept` headers
  (the probe documents "exactly the headers a2a-go's own client sends").
- No overlay needed a fix: the agentanalytics overlay tests, the configurable integration overlay
  and the agentengine shapes overlay all applied unchanged.
