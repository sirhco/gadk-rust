# 8a bug report: ADK-Go `bb1fd51` → `0990f9e`

gadk re-syncs from ADK-Go `bb1fd51` (v2.7.0-5) to ADK-Go main
`0990f9e2cb9df71c579db6c9731efa403641fc51` (`docs/parity/ADK_GO_COMMIT`). This report has one row
for each of the 106 commits printed by

```sh
git -C "$ADK_GO_DIR" log --format='%h %s' bb1fd51..0990f9e | grep -E '^[0-9a-f]+ +fix'
```

in that order (spec R7).

## Method

1. **Probes (T1).** Every `scripts/gen-go-*` probe, the copied httprr files, the configurable
   integration overlay and `encode_shapes.rs` were re-run at `0990f9e`. Each changed golden is
   listed under [Go output changed](#go-output-changed). A gadk test over a changed golden that
   now fails keeps the new golden and carries `#[ignore = "8a bug: …"]`.
2. **Detector (T5–T12, groups D1–D7).** Every new, missing or changed Go test of a group was ported
   against gadk before any fix (base `main` + T1–T4). A port that fails keeps Go's expectation and
   carries `#[ignore = "8a bug: <cause>"]`. A port that passes stays as a guard. A Go test that
   needs one of the 8a features (R9–R14) to compile was not written; it is listed for the fix
   task that adds the feature. A Go test that needs only an internal helper the fix adds is
   listed as *helper-deferred*.
3. **Assembly (T13).** The group fragments were merged here, one row per commit. A commit that
   several groups judged gets one row with every group's tests and causes, each tagged with its
   group. The verdict is the strongest one any group gave, in this order: *real gadk bug* >
   *real gadk bug (feature)* > *Go output changed* > *Go-only (guard kept)* > *not applicable*.
   No two groups gave one commit different verdicts. Every commit had at least one group verdict,
   so none was decided by T13's own inspection.

Verdicts:

- *real gadk bug*: a port fails on today's gadk (or, where marked "by inspection", gadk's code
  plainly lacks the behaviour). It is fixed to `0990f9e` behaviour in phase 3.
- *real gadk bug (feature)*: the Go tests need an 8a feature (R9–R14) to compile.
- *Go output changed*: a regenerated golden changed and the gadk test over it still passes.
- *Go-only (guard kept)*: the port passes on today's gadk; it stays as a guard.
- *not applicable*: the Go bug cannot occur in gadk (for example a Go nil pointer).

The *final state* column records how each commit ended after the fix tasks (T14–T23): *fixed by
Task N* (the plan task whose commit made the ports pass; several tasks when the fix spans owners),
*divergence (ledger …)* (an accepted divergence listed in that ledger and marked
`// Divergence (ledger …)` where it lives), *guard kept (Go-only)* or *not applicable*.

Test paths are relative to each group's test files. The [appendix](#appendix-by-group) names where
each group's ports live.

## Summary

| verdict | commits |
|---|---|
| *real gadk bug* | 90 |
| *real gadk bug (feature)* | 2 |
| *Go output changed* | 0 |
| *Go-only (guard kept)* | 12 |
| *not applicable* | 2 |
| **total** | **106** |

Final state (after the fix tasks; the workspace runs with 0 ignored tests and no `8a bug` ignore):

| final state | commits |
|---|---|
| fixed | 91 |
| — of which with an accepted divergence on a residual edge (1ced38ba ledger 1b, 7fd9f83f ledger 1b, 1a84f04f ledger 2c) | 3 |
| divergence only (869020a1, ledger 5c) | 1 |
| guard kept (Go-only) | 12 |
| not applicable | 2 |
| **total** | **106** |

Both *real gadk bug (feature)* commits are fixed (e1078219 R9, 24d33b35 R10). Two fixes changed a
public signature with the user's approval: e0cbfd30 (`BeforeToolCallback` returns the arguments)
and c377836c (`DebugSpan.attributes` holds JSON values); see `CHANGELOG.md`.

All 12 fix commits whose goldens changed (see [Go output changed](#go-output-changed)) also have
failing ports, so their rows are *real gadk bug*.

## Fix commits

| commit | gadk test(s) | verdict | cause | final state |
|---|---|---|---|---|
| `0990f9e2` fix(runner): start a new invocation for answers spanning invocations (#2312) | D2a: `runner_run_node::test_runner_function_responses_start_new_invocation` (case `agent_path_calls_of_two_invocations` ignored; 4 cases pass), `test_runner_function_responses_resume_their_invocation` (pass), `runner_hitl_integration::test_runner_workflow_agent_answers_to_two_paused_runs_resume_the_first` (pass) | *real gadk bug* | on the agent path, answers to calls of two invocations resume the first call's invocation instead of starting a new one | fixed by Task 18 |
| `3188db98` fix(llminternal): keep streamed function-call and event ids stable (#2308) | D1: `aggregator::test_streaming_function_call_continuation_carries_call_id`, `aggregator_flow::test_progressive_sse_partials_share_event_id`, `test_progressive_sse_text_stream_shares_event_id`, `base_flow::test_generate_content_draws_one_id_per_event` (ignored); `aggregator_flow::test_complete_responses_of_one_model_call_get_distinct_event_i_ds`, `test_recovered_model_error_gets_its_own_event_id` (pass) | *real gadk bug* | `flow::generate_content` draws a fresh event id for every model stream item (partials and the aggregated response of one call get different ids), and the aggregator does not give a nameless continuation chunk the streamed call's id | fixed by Task 15 and Task 17 (aggregator; flow event IDs) |
| `e0cbfd30` fix(llminternal): give tool callbacks a copy of the call arguments (#2313) | D1: `base_flow::test_handle_function_calls_callback_arg_edits_do_not_reach_the_call_event` (ignored) | *real gadk bug* | gadk callbacks already get an owned copy (the call event is never edited), but a before-tool callback's argument edits never reach the tool: `BeforeToolCallback` has no way to hand edited args back | fixed by Task 23 (breaking `BeforeToolCallback` returns `(args, result)`, user-approved) |
| `d642dbfd` fix(runner): keep the original event's identity when a plugin replaces it (#2306) | D2a: `runner_run_node::test_runner_plugin_replacement_keeps_event_identity`, `test_runner_plugin_shared_replacement_is_not_modified`, `test_runner_stamps_before_run_replacement`, `runner_live_runner::test_runner_run_live_plugin_replacement_keeps_event_identity` (all ignored) | *real gadk bug* | `from_plugin` keeps the replacement's ID, invocation ID, timestamp and empty author instead of the original event's | fixed by Task 18 |
| `fc6de07d` fix(llminternal): keep responses that carry only grounding metadata (#2310) | D1: `base_flow::test_run_keeps_grounding_only_response`, `contents_processor::test_contents_request_processor_include_contents_none_skips_empty_pivot` (ignored) | *real gadk bug* | `Flow::run` drops a response that carries only grounding metadata, and IncludeContents None starts the turn at another agent's reply with nothing to read | fixed by Task 17 |
| `5b3d897b` fix(agentanalytics): redact credential-named types and guard omitzero in JSONFormatter (#2249) | D7: `plugin_agentanalytics_json_formatter::credentials::{test_json_formatter_credential_named_types, test_json_formatter_credential_byte_slice, test_json_formatter_plain_credential_struct_map_keys, test_json_formatter_colliding_map_keys}` (ignored); T1: `redaction::tests::golden_text_corpus_matches_go`, `golden_formatter::smart_truncate_matches_redaction_corpus` (pass, corpus 1341); T1 goldens: see *Go output changed* below | *real gadk bug* | the serde sanitizer redacts by key name only: credential-named types (serde newtype / struct names), their map keys and colliding keys are not handled; the invalid-UTF-8 half of the fix is *not applicable* (Rust strings are UTF-8) | fixed by Task 16 |
| `67fc01b1` fix(llmagent): keep a node's parsed structured output on its event (#2291) | D2a: `runner_output_schema_resume::test_parsed_schema_reply_survives_resume_on_database` (ignored), `test_null_schema_reply_survives_resume` (pass); D2b: `agent_llmagent_llm_agent_wrapper::test_process_llm_agent_output` | *real gadk bug* | D2a: `process_llm_agent_output` sets `message_as_output` even when the output schema parsed a record, so the runner clears the parsed output and the resumed join gets the reply text / D2b: `process_llm_agent_output` always sets MessageAsOutput, so the runner drops a parsed output_schema value | fixed by Task 22 |
| `f1f0d2de` fix(loadartifactstool): return an error result for non-string names (#2307) | D3: gadk-tools `tests/tool_loadartifactstool_load_artifacts_tool.rs` `test_load_artifacts_tool_run` | *real gadk bug* | `load_artifacts` returns `ToolError::InvalidArgument` for a non-list / non-string / null `artifact_names` instead of the result `{"error": "'artifact_names' must be a list of strings.", "error_code": "INVALID_ARGUMENTS"}` (a null element also decodes to `""`) | fixed by Task 20 |
| `5fd47b21` fix(adka2a): carry video metadata under adk_video_metadata (#2303) | D5: `server_adka2a_v2_parts::{test_parts_two_way_conversion, test_to_gen_ai_parts_reads_video_metadata, test_to_gen_ai_parts_rejects_malformed_video_metadata}` (gadk-a2a/tests) — fail, ignored | *real gadk bug* | `to_a2a_file_part` writes the unprefixed `video_metadata` key and `to_genai_part` never reads video metadata back (nor rejects a malformed one) | fixed by Task 21 |
| `10f84869` fix(runner): refuse a user message that contains a function call (#2302) | D2a: `runner_runner::test_runner_rejects_user_function_call` (5 cases ignored), `test_runner_user_function_call_checked_after_plugin` (case `plugin_adds_a_call` ignored, other passes), `runner_live_runner::test_runner_run_live_send_rejects_user_function_call` (ignored) | *real gadk bug* | a user message carrying a function call is stored and run (and forwarded by the live session) instead of failing with `user message cannot contain function calls` | fixed by Task 18 |
| `1ced38ba` fix: send {"result": null} when a tool returns nothing (#2281) | D1: `handle_function_calls_async::test_handle_function_calls_fr_skip_gates` (ignored); `test_handle_function_calls_no_args_reach_tool_as_empty_map`, `…_no_args_reach_streaming_tool_as_empty_map`, `…_no_args_reach_on_tool_error_for_unknown_tool`, `…_set_model_response_without_args`, `streaming_tool::test_handle_function_calls_no_args_reach_live_streaming_tool_as_empty_map`, `…_no_args_reach_on_tool_error_for_non_function_tool` (pass); T1's 4 golden ignores (`agent_llmagent_llmagent_delegation` 05/09, `plugin_functioncallmodifier_integration` case 3); D3: gadk-engine `tests/tool_exitlooptool_tool.rs` `test_exit_loop_tool_exits_loop_agent` (ignored), `test_exit_loop_tool_after_tool_callback_sees_no_result`, `test_exit_loop_tool_after_tool_callback_replaces_result` (pass); T1 ignored `plugin_functioncallmodifier_integration::test_plugin_callback_integration` over the same change; D2b: `agent_llmagent_llmagent::test_agent_transfer` (ignored); T1 goldens: see *Go output changed* below | *real gadk bug* | D1: a tool that returns nothing gets `{}` instead of `{"result": null}`; gadk args are always a map, so the empty-args half already holds / D3: `exit_loop` returns an empty map and the flow never wraps a nothing-result as `{"result": null}`, so the model is sent `{}` (the flow side, `handleFunctionCalls`, is D1's) | fixed by Task 17 and Task 22 (flow; agent-analytics row); divergence (ledger 1b): an empty result map is Go's nil |
| `8cddb9d4` fix(llminternal): skip live responses that carry nothing (#2301) | D1: `base_flow_live::test_run_live_skips_empty_responses` (ignored); `test_run_live_no_goroutine_leak`, `test_run_live_close_stops_idle_session`, `live_reconnect::test_run_live_ceiling_ignores_consumer_latency` (updated, pass) | *real gadk bug* | `run_live` yields an event for a live response that carries nothing (setupComplete, empty serverContent) | fixed by Task 17 |
| `a8a9b936` fix(agenttool): answer an empty sub-agent reply with {"result": ""} (#2287) | D3: gadk-engine `parity_tests::tool_agenttool_agent_tool` `test_agent_tool_run_code_parts`, `_all_thought_parts_returns_empty`, `_all_thought_parts_returns_empty_with_output_schema`, `_empty_model_response`, `_no_content_returns_empty_result` | *real gadk bug* | `AgentTool::run` returns `{}` (not `{"result": ""}`) when the sub-agent yields no content or no text | fixed by Task 20 |
| `6522e5b4` fix(workflow): accept a workflow with no edges (#2286) | D2b: `workflow_workflow::test_workflow_with_no_edges_runs_to_completion`, `workflow_validation::test_start_node_present_empty_edges` | *real gadk bug* | `validate_start_node_present` returns NoStartNode on an empty edge set | fixed by Task 19 |
| `1d9ae5b9` fix(sequentialagent): return the sequential agent from FindAgent (#2288) | D2b: `agent_workflowagents_sequentialagent_agent::test_sequential_agent_find_agent_returns_sequential_agent` | *Go-only (guard kept)* | gadk's default `Agent::find_agent` already returns the sequential agent's own `Arc` | guard kept (Go-only) |
| `d9ecb345` fix(session): count a final error event as a final response (#2285) | D4: `gadk-session` `event::test_is_final_response_error_event` — fails, ignored | *real gadk bug* | `Event::is_final_response` has no "complete error event without function calls is final" rule, so an error event carrying a function response or a trailing code execution result is not final | fixed by Task 14 |
| `8ed375d1` fix(skilltoolset): list skills under "result" and trim SKILL.md instructions (#2283) | D3: gadk-tools `tool_skilltoolset_internal_skilltool_tools::test_list_skills`, `::declarations_match_go`, `tool_skilltoolset_skill_filesystem_source::test_file_system_source_load_instructions` | *real gadk bug* | `ListSkillsResult` serializes (and declares) its XML under `"skills"`, not `"result"`; `FileSystemSource::load_instructions` returns the SKILL.md body untrimmed | fixed by Task 20 |
| `58429928` fix(adka2a): refuse a cancel request without a task id (#2284) | D5: `server_adka2a_v2_executor::test_executor_cancel_without_task_id` (gadk-a2a src/parity_tests) — fails, ignored | *real gadk bug* | `Executor::cancel` yields a `canceled` status update for an empty task id instead of only an error | fixed by Task 21 |
| `e1078219` fix(plugin): suppress cancellation causes in agent error callback (#2260) | D2a: `agent/agent_on_error_test.go` (feature-deferred, R9); D3: — (Go tests in `agent/agent_on_error_test.go`, group D2a) | *real gadk bug (feature)* | D2a: gadk has no `on_agent_error_callback`, so nothing suppresses cancellation causes in it / D3: gadk has no `OnAgentErrorCallback` (R9); the plugin field and the cancellation filter arrive with R9 | fixed by Task 18 (feature R9, `PluginConfig.on_agent_error_callback`) |
| `86620857` fix(llminternal): look past transfer_to_agent hand-overs for the current turn's start (#2248) | D1: `contents_processor::test_contents_request_processor_include_contents_none_transferred_from_parent` (ignored) | *real gadk bug* | IncludeContents None starts the current turn at the parent's transfer_to_agent hand-over, leaving the user's request out | fixed by Task 17 |
| `51e8fe4c` fix(llminternal): resume tool confirmations only from adk_request_confirmation calls (#2279) | D1: `request_confirmation_processor::test_request_confirmation_resumes_only_request_confirmation_calls` (ignored) | *real gadk bug* | the confirmation processor resumes any call whose args carry `originalFunctionCall`, not only `adk_request_confirmation` calls | fixed by Task 17 |
| `ead2778f` fix(adka2a): do not panic on a status update with no convertible parts (#2276) | D5: new case "non-final task status update with an empty message is skipped" in `server_adka2a_v2_events::test_to_session_event` (passes alone; the test is ignored for d6bb1e86) | *Go-only (guard kept)* | gadk already returns `Ok(None)` for a non-final status update whose message has no parts (ledger 5b divergence "Go dereferences a nil Content" is now obsolete) | guard kept (Go-only); the obsolete ledger 5b nil-`Content` divergence was dropped |
| `983d689e` fix(workflow): call the second use_as_output child a delegate in the error (#2277) | D2b: `workflow_run_node::test_run_node_with_use_as_output_second_delegation_returns_error` | *real gadk bug* | the sentinel text is still "parent already has a use_as_output child" (no Go test; the port's exact-text assert is the evidence) | fixed by Task 22 |
| `11f4e29f` fix(agentanalytics): sanitize unsafe GCSPathPrefix values and cap GCS object names at 1,024 bytes (#2018) | D7: `plugin_agentanalytics_gcs_offloader::{test_gcs_object_path, helpers::test_path_segment, sanitization::{test_clean_gcs_prefix, test_gcs_object_path_sanitization_and_bounds, test_batch_processor_object_path_caches_effective_prefix}, plugin_level::test_gcs_path_prefix_rewrite_warning_and_disjoint_namespace}` (ignored); `plugin_level::test_gcs_offload_rewritten_prefix_fits_hierarchical_namespace_folder_limit` (passes); T1: `offload::uploads::tests::{path_segment_matches_go, object_path_matches_go, gcs_object_path_matches_go}` (ignored, see Go output changed); T1 goldens: see *Go output changed* below | *real gadk bug* | no `~<sha256>` suffix on altered / dot-only / long path segments, no 128-byte segment / 1,024-byte name caps, no unsafe-extension hashing, no `cleanGCSPrefix` rewrite of unsafe prefixes, no effective-prefix cache or rewrite warning | fixed by Task 16 |
| `f55660a6` fix(agentanalytics): cap response summary and function name summary length (#2246) | D7: `plugin_agentanalytics_json_formatter::summary_caps::{test_response_message_summary_length_cap, test_response_message_summary_function_name_cap}`, `plugin_agentanalytics_doc::test_doc_parity_response_summary_and_function_name_bounds` (ignored); T1 goldens: see *Go output changed* below | *real gadk bug* | `MessageSummary` renders a response summary uncapped (no MaxContentLen + 4096 slack) and shows function call/response names raw (no redaction, no 256-rune cap) | fixed by Task 16 |
| `00313e5f` fix(llminternal): keep the resumption handle out of the caller's LiveRunConfig (#2244) | D1: `base_flow_live::test_run_live_keeps_resumption_handle_out_of_caller_config` (pass) | *Go-only (guard kept)* | gadk clones the RunConfig into the invocation, so the caller's LiveRunConfig never sees the handle; setup frames carry `""` then `server-handle` | guard kept (Go-only) |
| `b5cec673` fix(llminternal): send LiveRunConfig realtime input, affective dialog and proactivity (#2216) | D1: `base_flow_live_config::test_run_live_sends_live_run_config_fields` (ignored) | *real gadk bug* | `run_live` does not send LiveRunConfig's `realtimeInputConfig`, `enableAffectiveDialog` and `proactivity` in the setup message (nor leave the last two out for Gemini 3.x Live) | fixed by Task 17 |
| `7fd9f83f` fix(configurable): apply config_path containment to workflow node refs (#1299) | D2a: `internal/utils/fs_test.go`, `fs_disk_test.go` (deferred: need a `safe_subpath` helper); D7: `internal_configurable_configurable_utils::{test_resolve_agent_reference_rejects_escaping_config_path, test_resolve_agent_reference_allows_paths_inside_agent_dir, test_resolve_agent_reference_symlinked_parent_dir, test_resolve_config_reference_accepts_links_that_stay_inside, test_resolve_config_reference_aliased_directory_share_key, test_resolve_config_reference_empty_ref, test_resolve_config_reference_os_specific_refs, test_from_config_refuses_escaping_agent_references, test_from_config_dot_dot_after_link_in_target}`, `internal_configurable_configurable_workflow_ref::{test_workflow_node_reference_rejects_escaping_path, test_workflow_route_map_reference_is_checked, test_workflow_node_registry_does_not_leak_across_directories, test_workflow_node_reference_cache_does_not_bypass_check}` (ignored); `test_resolve_config_reference_volume_qualified_refs`, `test_resolve_config_reference_below_non_directory`, `test_workflow_node_reference_allows_paths_inside_workflow_dir` (pass) | *real gadk bug* | D2a: by inspection: gadk-configurable `resolve_node_like` (workflow.rs:211) accepts absolute and `..`-escaping YAML node refs and follows links; there is no shared containment helper / D7: references are resolved against the parent of the config *file* (Go now passes the config directory) with a lexical `EvalSymlinks` prefix check and old refusal texts instead of `utils.SafeSubpath` (sentinels `ErrNotValidRelativePath` / `ErrSymlinkInRelativePath`, wrapped `config reference "<ref>": …`, real-path keys); workflow edge `.yaml` refs (linear and route-map) get no containment check at all, absolute ones are loaded, and the node cache is consulted by name and keyed by the lexical path | fixed by Task 16 (`gadk_platform::safe_subpath`); divergence (ledger 1b): Unix path rules only |
| `f2dc5d3a` fix(llminternal): carry Live API usage metadata on live responses (#2215) | D1: `googlellm_live_connection::test_live_connection_recv_carries_usage_metadata`, `test_live_connection_recv_keeps_usage_on_buffered_turn_complete`, `base_flow_live::test_run_live_events_carry_usage_metadata` (ignored) | *real gadk bug* | `LiveConnection::recv` drops the server message's `usageMetadata` (Go maps it, `responseTokenCount`/`Details` → `candidatesTokenCount`/`Details`) | fixed by Task 15 |
| `bec16021` fix(workflowinternal): reject a null finish_task result (#1741) | D2b: `internal_workflowinternal_finish_task_tool::{test_finish_task_tool_run, test_finish_task_tool_declaration_result_matches_validation}`, `agent_llmagent_llm_agent_wrapper::test_task_agent_nullable_result_closes_delegation` | *real gadk bug* | the wrapped finish_task result keeps the user schema's `nullable: true`, so a null result is declared and accepted | fixed by Task 19 |
| `3193128b` fix(adkrest): drop nil parts when serializing events and scanning the event graph  (#2192) | D5: `server_adkrest_internal_models_event::test_marshal_event_drops_nil_parts`, `server_adkrest_controllers_debug::test_event_graph_handler_tolerates_nil_parts` (both adapted: no nil parts in Rust) | *Go-only (guard kept)* | gadk's `Content.parts` is `Vec<Part>` (a JSON `null` part already fails gadk-genai's decode, ledger 5c), so no nil part reaches the encoder or the graph scan; the adapted guards pass | guard kept (Go-only) |
| `2278f8f5` fix(adkrest): treat MaxConcurrentRuns<=0 as unlimited in the Eventarc trigger (#2191) | D5: `server_adkrest_controllers_triggers_eventarc::{test_eventarc_trigger_handler_non_positive_max_concurrent_runs_is_unlimited, test_eventarc_trigger_handler_enforces_max_concurrent_runs}` | *Go-only (guard kept)* | gadk's `EventarcController::new` already builds no semaphore for a non-positive limit (`rest/triggers/mod.rs` `semaphore`) | guard kept (Go-only) |
| `46ff99b8` fix(llminternal): drop nil parts in the streaming aggregator (#2190) | D1: `aggregator::test_process_response_nil_part` (adapted, pass) | *Go-only (guard kept)* | a Go nil `*genai.Part` panicked the reflect check; Rust has no nil part, and the degenerate `Part::default()` is already skipped without splitting text | guard kept (Go-only) |
| `75ae9208` fix(session): skip nil parts in IsFinalResponse classification (#2189) | D4: `gadk-session` `event::test_is_final_response_nil_parts` (adapted: nil entries dropped) — passes | *Go-only (guard kept)* | a gadk `Content` holds `Part` values, so there is no nil part to skip; the non-nil classification already matches | guard kept (Go-only) |
| `ff9e77c5` fix(preloadmemorytool): read all text parts from the query (#2220) | D3: gadk-tools `tests/tool_preloadmemorytool_tool.rs` `test_preload_memory_tool_process_request` | *real gadk bug* | `preload_memory` searches only the first user part's text; Go joins every non-empty text part with spaces | fixed by Task 20 |
| `e2614290` fix(workflow): honor UUID provider for input requests (#1915) | D2b: `workflow_hitl::test_scheduler_hitl_node_auto_generates_interrupt_id` (passes), `workflow_hitl::test_scheduler_hitl_node_empty_provider_uuid_still_pauses` | *real gadk bug* | gadk already draws from the provider, but an empty draw stays the interrupt ID and the node never pauses | fixed by Task 19 |
| `be529e7b` fix(workflow): pass a typed node's output schema to the model (#2194) | D1: `internal/llminternal/output_schema_binding_test.go` (3 tests, helper-deferred: need `WithBoundOutputSchema` / `outputSchemaFor`); D2b: `workflow_agent_node_output_schema` (7 ignored, 6 pass, 1 skipped), `workflow_model_output_schema` (2 skipped) | *real gadk bug* | D1: by inspection: gadk has no bound output schema, so a typed workflow node's output schema never reaches an LLM agent that declares none / D2b: AgentNode never derives or binds a model output schema (no `modelOutputSchema` / `WithBoundOutputSchema` counterpart) | fixed by Task 19 and Task 22 (model output schema; binding) |
| `dba8afa2` fix(llminternal): show the user's answer to another agent's call as context (#2202) | D1: `foreign_function_response::test_build_contents_user_response_to_other_agent_call_is_context`, `test_build_contents_user_text_with_response_to_other_agent_call`, `test_build_contents_response_in_suffix_to_other_agent_call_before_it` (ignored); D2a: `runner_workflow_function_response::test_run_function_response_reaches_llm_agent_after_workflow` (ignored) | *real gadk bug* | D1: a user's function response to another agent's call stays a raw response (`no function call event found …`) instead of "For context:" / D2a: the contents processor fails with `no function call event found` on the user's answer to another agent's call instead of showing it as context | fixed by Task 17 |
| `4d841aba` fix(remoteagent): return errors instead of error events (#1740) | D5: `agent_remoteagent_v2_a2a_agent::remote_runs::{test_remote_agent_error_if_no_compatible_transport, test_remote_agent_error_on_server_error, test_remote_agent_after_request_callback_gets_nil_event_on_default_converter_error, test_remote_agent_error_if_agent_card_resolution_fails, test_remote_agent_error_if_message_creation_fails, test_remote_agent_error_if_default_conversion_fails}`, `agent_remoteagent_v2_auth::agent::served::{test_remote_agent_auth_accepted_by_enforcing_server, test_remote_agent_auth_refuses_cross_origin_redirect, test_remote_agent_auth_refuses_a_different_hostname, test_remote_agent_auth_fails_closed, test_remote_agent_auth_refuses_an_insecure_card_source}` — fail, ignored | *real gadk bug* | the remote agent yields `to_error_event` events (card resolution, sender/message creation, transport error, default conversion failure) instead of returning the error from `run`; the after-request callback gets that event and no error | fixed by Task 21 |
| `c00807e6` fix(agentengine): preserve fractional seconds in event timestamp encoding (#2199) | D5: `server_agentengine_internal_helper_encode::test_time_fractional_seconds`; T1: `web::agentengine::tests::the_api_mount_matches_go_wire_fixtures`; T1 goldens: see *Go output changed* below | *real gadk bug* | `convert_snake` writes `time.Time` as integer seconds (`GoKind::Time => GoValue::Int(time_secs(v))`, `crates/gadk-server/src/agentengine/encode.rs:459,520`); Go writes `float64(UnixMicro()) / 1e6` | fixed by Task 21 |
| `51bc7ec5` fix(workflowagents): skip siblings when resuming a nested graph workflow (#2188) | D2a: `runner_workflow_function_response::test_run_function_response_skips_siblings_of_graph_workflow`, `test_run_function_response_narrows_only_first_loop_pass`, `test_run_function_responses_select_every_caller` (ignored); `test_run_function_response_inside_graph_workflow_resumes_workflow`, `test_run_function_response_escalator_in_loop_terminates` (pass); D2b: `agent_workflowagents_sequentialagent_agent::test_sequential_agent_run_without_session_or_function_response` (passes; the resume test is `runner/workflow_function_response_test.go`, D2a) | *real gadk bug* | D2a: sequential, parallel and loop agents re-run every sub-agent on a resume instead of narrowing to the sub-agents holding the callers (no `GraphResumeIndexes` port) / D2b: gadk's sequential, loop and parallel agents always run every sub-agent; there is no `GraphResumeIndexes` / `WithGraphResumeDone` counterpart [by inspection] | fixed by Task 22 |
| `b08c5c22` fix(functioncallmodifier): skip nil parts in the after-model scan (#2193) | D3: gadk-engine `tests/plugin_functioncallmodifier_plugin.rs` `test_after_model_callback` (case "nil part before the function call", an empty `Part`) | *not applicable* | a Rust `Vec<Part>` cannot hold nil; the adapted case (an empty part first) passes as a guard | not applicable |
| `8be7e250` fix(session): define List order as last update time ascending (#1919) | D4: `gadk-session` `inmemory::test_in_memory_service_list_ordered_by_last_update_time`, `inmemory::test_compare_by_last_update`; `gadk-session-database` `session_database_service::test_database_service_list_ordered_by_last_update_time`, `::test_database_service_list_tie_breaks_by_user_then_id` — fail, ignored; `::test_database_service_list_orders_mixed_offsets_by_instant` — passes; `gadk-vertexai` `parity_tests::session_vertexai_session::test_sort_sessions_by_last_update` — fails, ignored; T1's `server_adkrest_controllers_sessions::session_handlers_match_go_wire_fixtures` and `wire::wire_main_matches_go` (golden `main_08_session_list.json`); T1 goldens: see *Go output changed* below | *real gadk bug* | no backend orders `List`: in-memory returns BTreeMap key order, database returns storage order, Vertex AI returns RPC order; Go sorts by last update time ascending, then user ID, then session ID | fixed by Task 14 (REST goldens un-ignored by Task 21) |
| `dce89da5` fix(runner): route a function response to the calling sub-agent under a workflow root (#1808) | D2a: `runner_workflow_function_response::test_run_function_response_resumes_workflow_sub_agent` (ignored) | *real gadk bug* | the agent path always runs the root agent; a function response is not routed to the calling sub-agent on its branch | fixed by Task 18 |
| `33a587e2` fix(llminternal): send live tool responses through the sender goroutine (#1903) | D1: `base_flow_live::test_run_live_tool_response_shares_sender_goroutine` (ignored: intermittent); `test_run_live_tool_response_clears_pending_send` (pass); TestSendAfterToolWriteFailure skipped (NaN) | *real gadk bug* | `run_live`'s consumer loop writes the tool response on the connection itself, so it can overtake user input queued on the sender task (gadk's sink mutex rules out Go's concurrent-writer panic, not the reordering) | fixed by Task 17 |
| `8c2e2e2a` fix(llmagent): write finish_task output to OutputKey for task agents (#1977) | D2b: `agent_llmagent_llmagent_saveoutput::test_llm_agent_maybe_save_output_to_state`, 6 `agent_llmagent_llmagent_task_outputkey` tests | *real gadk bug* | a task agent's text turns are saved to output_key, and `run_task` never writes the accepted finish_task output there | fixed by Task 19 and Task 22 (declared mode; `output_key`) |
| `1b4f6ee0` fix(artifact): copy parts in the in-memory service on save and load (#1782) | D4: `gadk-artifact` `artifact_inmemory::test_in_memory_service_does_not_alias_stored_parts`, `::test_in_memory_service_does_not_alias_stored_part_fields`, `::test_in_memory_service_review_repro` (adapted) — pass | *Go-only (guard kept)* | gadk's `Save` takes the part by value and `Load` returns an owned clone, so a caller cannot alias the stored part | guard kept (Go-only) |
| `22ff1839`     fix(llminternal): drop internal artifact parts from live history replay (#1765) | D1: `contents_processor::test_contents_request_processor`, `googlellm_live_connection::test_live_connection_send_history_filters_internal_parts`, `test_live_connection_send_history_all_filtered` (ignored); `googlellm/live_artifacts_test.go` TestIsInternalLiveArtifactPart (helper-deferred) | *real gadk bug* | internal `artifact://…/_adk_live/…` parts reach the request and the live history replay (a foreign event made only of them becomes a bare "For context:" turn) | fixed by Task 15 and Task 17 (gadk-model/gemini; contents) |
| `ca7db168` fix(runner): only auto-create a session when Get reports session.ErrNotFound (#1899) | D2a: `runner_runner::test_runner_auto_create_session_get_error` (cases `transient_error_is_returned_without_create`, `unwrapped_not_found_is_returned_without_create` ignored; 2 pass), `test_runner_auto_create_session_empty_session_id` (pass); D5: `server_adkrest_controllers_sessions::test_get_session` (D5 part: `internal/fakes`) | *real gadk bug* | D2a: with auto-create on, the runner creates the session on any `get` error, not only `SessionError::NotFound` / D5: the testutil `FakeSessionService::get` answers `SessionError::Backend("not found")` for a missing session (`crates/gadk-server/src/rest/testutil.rs:219`); Go's fake now wraps `session.ErrNotFound` (the runner half is group D2a's) | fixed by Task 18 and Task 21 (runner; REST test fake) |
| `7fafcbbc` fix(retryandreflect): drop invocation failure counts when the run ends (#2024) | D3: gadk-engine `parity_tests::plugin_retryandreflect_plugin` `test_retry_and_reflect_after_run_drops_invocation_counts`, `_after_run_keeps_counts_of_paused_invocation`, `_after_run_drops_counts_of_every_per_invocation_scope`, `_after_run_scans_events_only_of_invocation_with_counts`, `_runner_continues_counts_of_answered_run` (ignored); `_after_run_keeps_global_counts` (passes) | *real gadk bug* | `RetryAndReflect` registers no AfterRunCallback, so an ended invocation's failure counts are never dropped | fixed by Task 20 |
| `4cc7ce5f` fix(agenttool): set SkipSummarization only after the input is valid (#2049) | D3: gadk-engine `parity_tests::tool_agenttool_agent_tool::test_agent_tool_run_skip_summarization_not_set_on_invalid_args` | *real gadk bug* | `AgentTool::run` sets `skip_summarization` before validating the arguments, so an argument error ends the parent's turn | fixed by Task 20 |
| `d8e47448` fix(llminternal): stop sending Gemini 3.x Live audio and images as mediaChunks (#2118) | D1: `googlellm_live_connection::test_live_connection_routes_input_by_model`, `test_live_connection_rejects_other_mime_types_on3x_live` (ignored); `googlellm/variant_test.go` TestIsGemini3xLive (helper-deferred) | *real gadk bug* | `LiveConnection` routes realtime input and single-text content only for `gemini-3.1` models on the Gemini API; Go routes every Gemini 3.x Live model (not Live Translate) on both backends, and Live Translate audio to `audio` | fixed by Task 15 |
| `4417f2d4` fix(retryandreflect): populate MaxRetries in retry-exceeded guidance template (#2174) | D3: gadk-engine `parity_tests::plugin_retryandreflect_plugin::test_retry_and_reflect_max_retries` | *real gadk bug* | `retry_exceeded_msg` renders the exceeded template with RetryCount/MaxRetries 0 ("failed consecutively 0 times") | fixed by Task 20 |
| `1a84f04f` fix(mcptoolset): let a CommandTransport reconnect (#2115) | D3: gadk-mcp `parity_tests::tool_mcptoolset_transport_config::test_command_transport_reconnect` | *real gadk bug* | reconnecting already works (a `CommandBuilder` re-run per connect), but every connect drops the command's stderr: `CommandFactory::connect` uses `TokioChildProcess::new`, which forces `Stdio::inherit()` (Go keeps `cmd.Stderr`) | fixed by Task 20 (own child-process transport); divergences (ledger 2c): close kills without SIGTERM, and a stderr the caller left unset is inherited from the host where Go discards it |
| `2e842548` fix(agent): list a multi-agent loader's agents in sorted order (#2164) | D2a: `agent_loader::test_multi_loader_list_agents_sorted` (ignored) | *real gadk bug* | `MultiLoader::list_agents` returns registration order, not sorted names | fixed by Task 18 |
| `ab78c5ae` fix(llminternal): keep a single_turn sub-agent's transcript out of the root agent's context (#2128) | D1: `tool_sub_branch::test_build_contents_skips_tool_sub_branch_events` (ignored); TestIsToolSubBranchEvent (helper-deferred); T1's golden ignore `agent_llmagent_llmagent_delegation::test_delegation_03_chat_to_single_turn`; D2b: `agent_llmagent_single_turn_context::test_single_turn_sub_agent_transcript_stays_out_of_coordinator_context`, T1's `agent_llmagent_llmagent_delegation::test_delegation_03_chat_to_single_turn`; guard `test_single_turn_sub_agent_reused_call_id_runs_twice` (passes); T1 goldens: see *Go output changed* below | *real gadk bug* | D1: a tool sub-agent's transcript on its `<agent>@<call id>` sub-branch reaches the calling agent's contents (and starts its current turn) / D2b: the single_turn sub-branch is keyed by a counter (`translator@1`), not the call id, and its events reach the coordinator's later turns | fixed by Task 17 and Task 19 (contents; single_turn branch) |
| `e83ab5b0` fix(functiontool): keep tool panics out of the model's function response (#2120) | D1: `base_flow::test_call_tool`, `streaming_tool::test_handle_function_calls_streaming_tool_errors` (ignored); D3: gadk-engine `parity_tests::plugin_retryandreflect_plugin::test_retry_and_reflect_tool_panic`, `::test_tool_panic_through_runner` | *real gadk bug* | D1: a tool panic's text (`panic in tool …`) reaches the model instead of ``Invoking `<tool>()` failed with an internal error.``; `llm/functions.rs` `call_tool` / D2a: by inspection: `llm/functions.rs:518` puts `ToolError::Panic`'s text (`panic in tool …`) in the function response; gadk has no `ModelErrorText` / `PanicError` equivalent (tests: none in D2a (T5/T8 ports cover it)) / D3: `RetryAndReflect` puts a tool panic's text (the panic value) and `ToolError::Panic` into `error_details` / `error_type` / guidance instead of `"panic"` and ``"Invoking `<tool>()` failed with an internal error."``; the flow-side substitution (`toolinternal.ModelErrorText` in `handleFunctionCalls`) is D1's | fixed by Tasks 17, 20 and 23 (flow; tool and plugin; shared `model_error_text`) |
| `c8fff670` fix(llmagent): return an error to the model when transfer_to_agent names an unknown agent (#2161) | D1: `agent_transfer::test_transfer_to_agent_tool_run` (ignored); D2b: `agent_llmagent_llmagent::test_agent_transfer` (ignored) | *real gadk bug* | `transfer_to_agent` accepts a name it cannot transfer to (no error naming the available agents), and a null argument errors with `missing argument` instead of `empty agent_name` | fixed by Task 19 |
| `869020a1` fix(agentregistry): speak A2A 0.3 to 0.3 interfaces (#2145) | D5: `agentregistry_factory::test_a2_a_client_factory_compat_protocol_version`, `agentregistry_remoteagent_e2e::test_remote_agent_on_the_wire` — fail, ignored; `test_remote_agent_e2_e` (1.0 cases) passes | *real gadk bug* | `compat_card` rewrites every non-1.x JSONRPC/HTTP+JSON interface to 1.0, so a 0.3 (or versionless) record is spoken to in A2A 1.0 (`A2A-Version: 1.0`, no `/v1` REST prefix) and an unknown major (2.0) still yields a client; the fix needs A2A 0.3 client transports, which a2a-rs lacks | divergence (ledger 5c): 0.x interfaces are spoken to in A2A 1.0 (a2a-rs has no 0.3 client); unknown majors refused as in Go by Task 21 |
| `dc44b8c9` fix(workflow): return ctx.Err() instead of a partial ParallelWorker result (#2146) | D2b: `workflow_parallel_worker::{test_parallel_worker_cancel_during_dispatch, test_parallel_worker_cancel_after_dispatch, test_parallel_worker_timeout_during_dispatch_in_workflow}`; guard `test_parallel_worker_failure_before_cancel_is_returned` (passes) | *real gadk bug* | `ParallelWorker::run` lacks the "no error but the context is done → ctx.Err()" check | fixed by Task 19 |
| `010b91e9` fix(workflow): honor UUID provider for tool node call IDs (#2030) | D2b: `workflow_tool_node::test_tool_node_run` (case `function_call_id_from_uuid_provider`) | *Go-only (guard kept)* | gadk's ToolNode already takes the call ID from `ctx.platform().new_uuid()` | guard kept (Go-only) |
| `8be9129e` fix(llminternal): keep ModelVersion on the aggregated streaming response (#2070) | D1: `aggregator::test_close_keeps_model_version`, `parallel_function_call::test_parallel_function_calls` (3 cases, ignored) | *real gadk bug* | `StreamingResponseAggregator::close` leaves `model_version` empty | fixed by Task 15 |
| `8ce8f01c` fix(adkrest): answer 500, not 404, when /run cannot read the session (#2107) | D5: `server_adkrest_controllers_runtime::test_run_handlers_session_lookup_status` | *real gadk bug* | `validate_session_exists` maps every `get` error to 404 (`crates/gadk-server/src/rest/controllers/runtime.rs:301-320`, and `/run_sse` the same); Go answers 404 only for `ErrNotFound` or an incomplete session name, else 500 | fixed by Task 21 |
| `82b64f45` fix(sequentialagent): stop RunLive on a sub-agent error, as Run does (#2151) | D2b: `agent_workflowagents_sequentialagent_agent::test_sequential_agent_run_live_stops_on_sub_agent_error` | *real gadk bug* | `sequential_live_stream` forwards a sub-agent's error and starts the next sub-agent | fixed by Task 19 |
| `ded357e3` fix(openaimodel): send executable code parts as text (#1956) | D6: `completions::request::test_build_params_code_execution_as_text`, `completions::media::test_build_params_media_on_the_wire_executable_code_before_an_inline_image_keeps_its_order`, `responses::request::build_params::{test_build_params_code_execution_as_text, test_build_params_drops_replayed_thoughts_thought_text_riding_on_code_drops_only_the_text}` — all fail, ignored; `shared` unit tests (`TestCodeExecutionText`, `PartFieldCodeExecution` rows) not written, see below | *real gadk bug* | both `build_params` report `ExecutableCode`/`CodeExecutionResult` as `unsupported content part`; Go passes `PartFieldCodeExecution` to `UnsupportedPayload` and appends `shared.CodeExecutionText(part)` (fenced `Code:```python`/`Execution Result:```code_output`). gadk lacks `shared::contents::PartField::CodeExecution` and `code_execution_text` | fixed by Task 15 |
| `de7088bf` fix(runner): treat the inbound message as a user turn (#1877) | D2a: `runner_runner::test_runner_inbound_message_is_user_turn` (cases `empty_role`, `model_role` ignored; `user_role` passes), `test_runner_inbound_role_normalized_for_context_and_plugin` (ignored), `test_runner_save_input_blobs_does_not_mutate_caller_message` (pass) | *real gadk bug* | the inbound (or `on_user_message`) message keeps its role instead of becoming a user turn | fixed by Task 18 |
| `123fea94` fix(triggers): use timer-and-select for context-aware retry backoff (#2052) | D5: `server_adkrest_controllers_triggers_pubsub::test_pub_sub_trigger_handler_cancel_during_backoff` (adapted: cancellation is a dropped future) | *Go-only (guard kept)* | gadk's backoff is a `tokio::time::sleep` inside the handler future, so a dropped request stops the wait and no retry runs | guard kept (Go-only) |
| `7af0f5f0` fix(session/database): delete a session's events explicitly in Delete (#1774) | D4: `gadk-session-database` `session_database_service::test_database_service_delete_removes_session_events` — fails, ignored; T1's `go_compat_rows::sqlite_rows_match_go` (golden `rows.sqlite.sql`) — fails, ignored; T1 goldens: see *Go output changed* below | *real gadk bug* | `delete` removes only the `sessions` row and relies on `ON DELETE CASCADE`, but the sqlite pool runs with `foreign_keys(false)`, so the events stay behind | fixed by Task 14 |
| `c377836c` fix(adkrest): preserve debug span attribute types (#1988) | D5: `server_adkrest_controllers_debug::test_debug_handlers_preserve_attribute_types`; T1: `wire::wire_main_matches_go` (`main_32/35/36`); T1 goldens: see *Go output changed* below | *real gadk bug* | the debug store keeps attributes as Go `Emit` strings (`DebugSpan.attributes: BTreeMap<String, String>`, `apply`/`emit` in `crates/gadk-server/src/rest/debug_telemetry.rs:109,428-450,848`); Go keeps `FromLogValue` JSON values and drops values JSON cannot encode | fixed by Task 21 (breaking `DebugSpan.attributes` → JSON values, user-approved) |
| `275e190e` fix(openaimodel): refuse non-function Tool fields genai adds (#1983) | D6: `shared_tools::test_ensure_function_tool_only` — fails, ignored | *real gadk bug* | `shared/tools.rs::ensure_function_tool_only` tests a fixed list of eight fields (so `FileSearch`, `ParallelAISearch`, `MCPServers`, `ExaAISearch` pass) and returns a plain error; Go refuses everything but `FunctionDeclarations` (reflective `rejectUnsupportedFields`) and wraps `ErrUnsupportedConfigField` | fixed by Task 15 |
| `7c87fdff` fix(remoteagent): resume tasks with mixed tool responses (#2148) | D5: `agent_remoteagent_v2_utils::test_get_user_function_call_at_mixed_response_tasks`, `agent_remoteagent_v2_a2a_e2e::test_a2_a_runner_resumes_mixed_function_responses` (local-first) — fail, ignored | *real gadk bug* | `get_user_function_call_at` reads only the first function response of the user event and returns `None` at a BeforeRun-cached call instead of trying the next response | fixed by Task 21 |
| `d0c978b0` fix(agent): reject an empty agent name in agent.New (#2124) | D2a: `gadk/tests/agent_agent_name::test_constructors_reject_empty_name` (ignored) | *real gadk bug* | `gadk_agent::new_agent` (and so every constructor) accepts an empty name | fixed by Task 18 |
| `450b4a54` fix(openaimodel): trace recovered terminal conversion failures (#2056) | D6: `responses_diagnostics::test_model_generate_stream_terminal_conversion_diagnostic` — fails, ignored (every non-event assertion of its 16-case table passes) | *real gadk bug* | gadk-openai has no tracing; `responses/model.rs::finished` swallows the failed terminal conversion silently. Go adds span event `openai.terminal_response_conversion_failed` with only `error.type` ∈ {`unsupported_output_item`, `unsupported_message_content`, `invalid_function_call_arguments`} | fixed by Task 15 |
| `f25d1424` fix(agenttool): report a sub-agent paused on an unresolved long-running call (#1856) | D3: gadk-engine `parity_tests::tool_agenttool_agent_tool` `test_agent_tool_run_pending_confirmation_returns_error`, `_skip_summarization_pending_error_reaches_parent`, `_streamed_args_unanswered_long_running_call_returns_error` (ignored); `_answered_long_running_call_succeeds`, `_streamed_args_answered_long_running_call_succeeds` (pass) | *real gadk bug* | `AgentTool::run` tracks no pending long-running call IDs, so a sub-run paused on an unanswered call returns a result instead of the "unresolved long-running tool calls" error, and `skip_summarization` is not cleared | fixed by Task 20 |
| `24d33b35` fix(runner): propagate parent plugins to agenttool sub-runners (#1777) | D2a: `runner_subagent_plugins::test_runner_parent_plugins_reach_agent_tool_llm_sub_agent`, `test_runner_parent_plugins_reach_agent_tool_sequential_sub_agent` (ignored); `test_runner_parent_plugins_do_not_own_sub_run_callbacks`, `test_runner_plugin_callback_runner_does_not_inherit_plugins`, `test_runner_separate_runner_does_not_inherit_context_plugins`, `test_runner_judge_inside_agent_tool_sub_run_does_not_inherit_plugins` (pass); `internal/plugininternal/plugin_manager_context_test.go` (feature-deferred, R10); D3: — (Go tests in runner / agenttool plugin propagation, group D2a) | *real gadk bug (feature)* | D2a: AgentTool's sub-runner gets no plugins and inherits none from the parent run (R10) / D3: `AgentTool::run` builds its sub-runner with `PluginConfig::default()`; propagation is R10 (#1777) | fixed by Task 18 and Task 22 (feature R10: runner and context; AgentTool) |
| `2f3b2c23` fix(session/database): lock app and user state rows in Create (#1930) | D4: `gadk-session-database` `session_database_service::test_database_service_create_locks_shared_state_rows` — fails, ignored; `::test_database_service_create_keeps_existing_shared_state` — passes | *real gadk bug* | `create` reads the app/user state rows with a plain select; Go takes the same insert-on-conflict-do-nothing + select-for-update lock as `AppendEvent` when the request state has `app:` / `user:` keys | fixed by Task 14 |
| `4db0bd99` fix(adkrest): include timestamp in REST Event model (#1870) | D5: `server_adkrest_internal_models_event::{test_event_round_trip_preserves_workflow_fields, test_to_session_event_timestamp_bounds_match_session_decoder}`, `server_adkrest_internal_models_session::test_from_session_accepts_last_update_in_first_second_of1970`, `server_adkrest_sessions_integration::test_create_session_event_timestamps`; T1: `wire::{wire_main_matches_go, wire_strict_matches_go}`, `server_adkrest_controllers_sessions::session_handlers_match_go_wire_fixtures`; T1 goldens: see *Go output changed* below | *real gadk bug* | `models::Event` has no `timestamp` (`crates/gadk-server/src/rest/models/event.rs:67,135,175`: not written, not read, no years 1..9999 bound, `to_session_event` cannot fail) and `Session::validate` rejects `updated_at == 0` (`rest/models/session.rs:74`) | fixed by Task 21 |
| `6caa3b85` fix(llminternal): omit metadata-only foreign parts from context (#1895) | D1: `foreign_thoughts::test_contents_request_processor_foreign_thoughts`, `test_contents_request_processor_foreign_thought_current_turn`, `foreign_payloads::test_convert_foreign_event_retains_payload_parts` (ignored); `foreign_payloads::test_convert_foreign_event_drops_thought_payload_parts` (pass) | *real gadk bug* | `convert_foreign_event` keeps a foreign part that carries only metadata (empty, signature, part/video/speech metadata, media resolution or processing) | fixed by Task 17 |
| `a1603c7f` fix(openaimodel): retain terminal text after call ID fallback (#1897) | D6: `responses::model::stream_content::test_model_generate_stream_completed_text_after_call_id_fallback` — fails, ignored; `…_completed_content_supersedes_before_call_adoption` passes | *real gadk bug* | `responses/model.rs::finished` tries `completed_content_supersedes` only before `adopt_terminal_calls`; Go retries it after adoption (an item-ID fallback corrected by adoption can make the snapshot supersede), so text only in the completed response is lost. The before-adoption ordering the second test pins already holds [half *Go-only (guard kept)*] | fixed by Task 15 |
| `550bb4c2` fix(llminternal): do not rerun a confirmed tool on repeated approval (#1865) | D1: `request_confirmation_processor::test_request_confirmation_repeated_approval_does_not_rerun_tool` (ignored); `test_request_confirmation_first_approval_placeholder_after_request` (pass) | *real gadk bug* | a repeated approval re-runs a confirmed tool whose result is already in the session | fixed by Task 17 |
| `0d223572` fix(llminternal): skip nil pointers in live realtime input (#1854) | D1: TestRunLiveSkipsNilRealtimeInput, TestHasRealtimeInput (skipped: inexpressible) | *not applicable* | Go dereferenced a typed nil `*genai.Blob`/`*ActivityStart`/`*ActivityEnd`; gadk's `RealtimeInput` is an enum with no nil value | not applicable |
| `6ae94e12` fix(llminternal): keep Partial and UsageMetadata on after-model callback replacements (#1763) | D2b: `agent_llmagent_llmagent::test_after_model_callback_replacement_keeps_streaming_fields` | *real gadk bug* | D1: by inspection: `Flow::call_model` (`llm/flow.rs:404`) yields an after-model callback's replacement as is; Go inherits Partial, TurnComplete and UsageMetadata from the replaced response (tests: none in D1 (tests in `agent/llmagent/llmagent_test.go`, T7)) / D2b: a replacement response is yielded as is, losing partial, turn_complete and usage_metadata | fixed by Task 17 |
| `71b0761f` fix(apigee): send the model ID, not the apigee/ model string, to the proxy (#2067) | D6: `model_apigee_apigee::test_generate_content_with_request_model` — cases 1–4 and 6 fail (5 passes), ignored as one rstest | *real gadk bug* | `apigee.rs::ApigeeModel::generate_content` forwards `LlmRequest.model` (the flow sets it to `Name()`, `apigee/…`) to the delegate, which prefers it over its own model ID; Go trims `req.Model` to the last path component | fixed by Task 15 |
| `f0af46d1` fix(functioncallmodifier): modify a copy of each tool declaration (#2081) | D3: gadk-engine `tests/plugin_functioncallmodifier_plugin.rs` `test_before_model_callback_leaves_tool_declaration_unchanged` | *Go-only (guard kept)* | gadk's `Tool::declaration()` returns an owned value, so the plugin already edits only the request's copy | guard kept (Go-only) |
| `e847ba7d` fix(llminternal): do not start the current turn at a posted-back tool result (#2083) | D1: `contents_processor::test_contents_request_processor_include_contents_none_submitted_tool_result` (ignored) | *real gadk bug* | IncludeContents None starts the current turn at a user-posted tool result, cutting out the call it answers | fixed by Task 17 |
| `10395f01` fix(session): keep in-memory Get from returning events before After (#2089) | D4: `gadk-session` `inmemory::test_in_memory_service_get_after_with_out_of_order_timestamps` — fails, ignored | *real gadk bug* | `InMemorySessionService::get` binary-searches `after` (`partition_point`) over events kept in append order, so out-of-order timestamps drop events at or after `After` or keep ones before it | fixed by Task 14 |
| `2649b4ad` fix(runner): stamp a timestamp on events agents yield without one (#2091) | D2a: `runner_run_node::test_runner_stamps_events_yielded_without_timestamp`, `test_runner_stamps_before_run_replacement`, `runner_live_runner::test_runner_run_live_stamps_events_yielded_without_timestamp` (ignored) | *real gadk bug* | an event yielded (or returned by `on_event`) without a timestamp is stored with the zero time | fixed by Task 18 |
| `860c7dac` fix(llminternal): inject text artifacts loaded as inline data into instructions (#2101) | D1: `instruction_processor::test_inject_session_state`, `test_artifact_text` (adapted; ignored) | *real gadk bug* | `inject_session_state` renders a text artifact loaded as inline data as empty; gadk has no artifactText rule set | fixed by Task 17 |
| `d6bb1e86` fix(adka2a): mark a finished A2A task as the final response (#2102) | D5: `server_adka2a_v2_events::{test_to_session_event, test_to_session_event_final_response_by_task_state}` — fail, ignored; T1 goldens: see *Go output changed* below | *real gadk bug* | `task_to_event` does not set `actions.skip_summarization` for a completed/failed/canceled task with parts, so `is_final_response()` is false when its artifacts replay tool calls | fixed by Task 21 |
| `0d95aed4` fix(llminternal): accept a decoded value in the tool confirmation response wrapper (#2103) | D1: `request_confirmation_processor::test_request_confirmation_request_processor` (ignored) | *real gadk bug* | the confirmation response wrapper only accepts a JSON string; a decoded object is an error (`'response' key found but value is not a string`) | fixed by Task 17 |
| `543edd3d` fix(parallelagent): keep the parent's isolation scope in each branch (#2054) | D2b: `agent_workflowagents_parallelagent_agent::{test_parallel_agent_propagates_isolation_scope, test_parallel_agent_scoped_branch_sees_its_own_history}` | *real gadk bug* | each branch context is a fresh invocation with an empty isolation scope, so a scoped branch's own history is filtered out | fixed by Task 19 |
| `15e9d4cd` fix(model/gemini): report a blocked prompt instead of failing with "empty response" (#2068) | D6: `gemini::test_model_generate_without_candidates` — unary cases fail, ignored; streaming cases pass; `test_model_maybe_append_user_content` passes | *real gadk bug* | the unary path of `GeminiModel` yields `ModelError::EmptyResponse` for a candidate-less response; Go converts it (`PromptFeedback` → `error_code`/`error_message`, else an empty turn) as streaming already does. The gadk-only extra test `gemini::model_empty_candidates_errors` pins the old behaviour and must be replaced by the fix task | fixed by Task 15 |
| `31111f89` fix(agenttool): accept the input schema a workflow agent's declaration advertises (#2069) | D3: gadk-engine `parity_tests::tool_agenttool_agent_tool::test_agent_tool_run_composite_agent_input_schema` | *real gadk bug* | D2b: `AgentTool::run` reads input_schema only from an LLM agent (`tool/agent_tool.rs:186-189`) while the declaration recurses into the first sub-agent (`tool/single_turn.rs:54-66`) [by inspection] (tests: none in D2b (its test is `tool/agenttool/agent_tool_test.go`, D3)) / D3: the declaration already uses the first sub-agent's InputSchema, but `AgentTool::run` reads the input schema only of an LLM agent (`reveal`), so a sequential agent's call is validated for `request` | fixed by Task 20 |
| `28201d64` fix(llminternal): keep a streamed text chunk's thought signature on the merged part (#2071) | D1: `aggregator::test_text_thought_signature_survives_merge` (ignored) | *real gadk bug* | `flush_text_buffer_to_sequence` drops the thought signature streamed on a text chunk | fixed by Task 15 |
| `5199d9fe` fix(exampletool): give Gemini models plain fences, not the tool_code ones (#2072) | D3: gadk-tools `tool_exampletool_tool::test_example_tool_process_request` | *real gadk bug* | fences chosen by `model.contains("gemini-2")`; Go: `model == "" \|\| contains("gemini-")` → plain fences, otherwise `tool_code`/`tool_outputs` | fixed by Task 20 |
| `e342d2f6` fix(exampletool): write example call arguments in a stable order (#2073) | D3: gadk-tools `tool_exampletool_tool::test_example_tool_process_request_stable_argument_order` | *Go-only (guard kept)* | gadk already sorts function-call args by key | guard kept (Go-only) |
| `dd39c80a` fix(sequentialagent): append the live completion instruction to an InstructionProvider (#2079) | D2b: `agent_workflowagents_sequentialagent_live_injection::test_sequential_agent_run_live_appends_completion_instruction_to_provider` | *real gadk bug* | the live `task_completed` suffix goes only to Instruction, which an InstructionProvider overrides | fixed by Task 22 |
| `128135a8` fix(functioncallmodifier): leave content-less responses to the rest of the chain (#2080) | D3: gadk-engine `tests/plugin_functioncallmodifier_plugin.rs` `test_after_model_callback_contentless_response_keeps_chain` | *real gadk bug* | the after-model callback returns `Ok(Some(resp))` for a content-less response, ending the after-model chain before the agent's own callbacks | fixed by Task 20 |
| `96790099` fix(telemetry): leave gcp.project_id off the resource when no project is known (#2082) | D7: `telemetry_telemetry::test_resolve_resource_project_id` (ignored) | *real gadk bug* | `resolve_resource` always adds `gcp.project_id`, empty when no project is known, blanking one from `OTEL_RESOURCE_ATTRIBUTES` | fixed by Task 16 |
| `ee8d5ca4` fix(adka2a): convert empty A2A text parts instead of failing (#2088) | D5: `server_adka2a_v2_parts::test_to_gen_ai_parts_empty_text`, `server_adka2a_v2_events::test_to_session_event_empty_text_part` — fail, ignored | *real gadk bug* | `to_genai_part` matches `Text(text) if !text.is_empty()`, so an empty text part fails with `unknown part type` | fixed by Task 21 |
| `0159a2e7` fix(adkrest): answer [] from /run when a run yields no events (#2090) | D5: `server_adkrest_controllers_runtime::test_run_handler_no_events_returns_empty_list` | *real gadk bug* | `run` encodes `None` (`null`) for an empty run (`crates/gadk-server/src/rest/controllers/runtime.rs:197`) | fixed by Task 21 |
| `0c109f7d` fix(session/vertexai): drop temp: keys from the initial state on Create (#2100) | D4: `gadk-vertexai` `parity_tests::session_vertexai_append_event_temp_state::test_create_drops_temp_keys_from_initial_state` — fails, ignored | *real gadk bug* | `SessionClient::create_session` sends `CreateRequest.state` unfiltered as `sessionState`, so `temp:` keys reach Agent Engine and the created session | fixed by Task 14 |
| `074b0879` fix(llminternal): end the turn when a preprocessed tool response is final (#2104) | D1: `parallel_function_call_hitl::test_confirmed_tool_with_skip_summarization_ends_the_turn` (ignored) | *real gadk bug* | a final response event yielded by preprocessing (a confirmed tool that sets `skip_summarization`) does not end the step; the model is called again | fixed by Task 17 |
| `dbaca475` fix(runner): run OnEvent and AfterRun for a BeforeRun reply in RunLive (#2105) | D2a: `runner_live_runner::test_runner_run_live_early_exit_runs_on_event_and_after_run` (ignored) | *real gadk bug* | `run_live`'s before-run early exit skips `on_event` and `after_run` | fixed by Task 18 |
| `f6da03ab` fix(adkrest): let AutoCreateSession reach /run and /run_sse (#2106) | D5: `server_adkrest_controllers_runtime::test_run_handlers_auto_create_session` | *real gadk bug* | `validate_session_exists` runs even with `auto_create_session` (`crates/gadk-server/src/rest/controllers/runtime.rs:301`), so a missing session is a 404 before the runner can create it | fixed by Task 21 |
| `1c354701` fix(adka2a): keep LLM agent instructions out of agent card skills (#2110) | D5: `server_adka2a_v2_agent_card::{test_get_agent_skills_llm_agent, test_build_agent_skills_omits_instructions}` — fail, ignored (also T1's `wire_agent_card_workflow` etc.); T1 goldens: see *Go output changed* below | *real gadk bug* | the LLM `model` skill description appends the pronoun-replaced instruction and global instruction (`build_description_from_instructions`) | fixed by Task 21 |

## Go output changed

Every `scripts/gen-go-*` probe (and the copied ADK-Go httprr files, the configurable integration
overlay and `encode_shapes.rs`) was re-run with ADK-Go at `0990f9e2cb9df71c579db6c9731efa403641fc51`.
No fixture count changed. Each changed golden is *Go output changed*; the last column names the gadk
test over it and whether it still passes (a failing test keeps the new golden and carries
`#[ignore = "8a bug: …"]`). Fixtures not listed here were reproduced byte for byte (the 80
telemetry-schema goldens, the 22 Vertex replays, the 10 `go_events`, the 29 server Agent Engine
fixtures, the goyaml corpus/goldens and float table, the 2 configurable integration goldens, the
other 44 httprr files and the remaining wire/launcher/agentanalytics fixtures).

### Changed fixtures

**Final state (Task 24).** The "gadk test" column below records the detector phase (T1–T13). Every
"fails, ignored" entry was fixed by the fix task named in that commit's row, so all listed tests
now pass and none is ignored. One part of the gadk-a2a wire rows is not reproduced: a2a-go
v2.7.0's client `Accept` headers. a2a-rs sends its own, which is a ledger 5b divergence
([5b.md](5b.md), "a2a-rs wire differences"). Those header differences are recorded in each
fixture's `*.deltas.json`, so the replay tests pass.

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

### Probe changes

- Every probe module (`gen-go-{a2a,adkrest,agentengine,configurable,events,launcher,sessiondb}`)
  re-tidied against ADK-Go `0990f9e` (genai v1.77.0, a2a-go v2.7.0, grpc v1.85.0, …);
  `gen-go-agentanalytics/module.go.{mod,sum}` refreshed by its documented procedure;
  `gen-go-vertexreplay` and `gen-go-goyaml` unchanged.
- `gen-go-a2a`: its raw server-direction requests mirror a2aclient v2.7.0's `Accept` headers
  (the probe documents "exactly the headers a2a-go's own client sends").
- No overlay needed a fix: the agentanalytics overlay tests, the configurable integration overlay
  and the agentengine shapes overlay all applied unchanged.

## Appendix by group

Each group's notes as its detector wrote them: where its ports live, deferred and skipped tests,
changed Go tests (*semantic (port updated)* or *cosmetic*), new Go test files, and removed or
renamed Go tests.

### Removed or renamed Go tests (all groups)

Spec R6 names 5. Each follows Go: the port and its ledger row are removed or renamed.

| Go test | group | what was done |
|---|---|---|
| `workflow/agent_node_mode_placement_test.go` TestAgentNode_UnnamedAgent_IsStillPlacedSingleTurn | D2b | removed in Go with d0c978b0 (`agent.New` rejects an empty name); port `workflow_agent_node_mode_placement::test_agent_node_unnamed_agent_is_still_placed_single_turn` removed |
| `agent/remoteagent/v2/a2a_agent_test.go` TestRemoteAgent_ErrorEventIfNoCompatibleTransport → TestRemoteAgent_ErrorIfNoCompatibleTransport | D5 | port renamed and rewritten to Go's expectation (ignored, 4d841aba) |
| `agent/remoteagent/v2/a2a_agent_test.go` TestRemoteAgent_ErrorEventOnServerError → TestRemoteAgent_ErrorOnServerError | D5 | port renamed and rewritten to Go's expectation (ignored, 4d841aba) |
| `server/adka2a/v2/agent_card_test.go` TestReplacePronouns | D5 | removed in Go with `replacePronouns` (1c354701); port `test_replace_pronouns` deleted |
| `internal/configurable/configurable_utils_test.go` TestResolveAgentReferenceRelativeParentPath | D7 | removed in Go (#1299: references resolve against the config directory); port `test_resolve_agent_reference_relative_parent_path` removed |

### D1 — LLM flow (T5)

Detector run against `plan/8a-b2base` with ADK-Go at `0990f9e2cb9df71c579db6c9731efa403641fc51`.
gadk ports of these packages live in `crates/gadk-engine/src/parity_tests/internal_llminternal_*.rs`,
`crates/gadk-engine/tests/{aggregator_flow,internal_llminternal_*}.rs`,
`crates/gadk-model/tests/{aggregator,variant}.rs` and (new)
`crates/gadk-gemini/tests/internal_llminternal_googlellm_live_connection.rs`. Test paths below are
relative to those files (`base_flow::` = `internal_llminternal_base_flow.rs`, and so on).

Counts: 26 *real gadk bug*, 2 *Go-only (guard kept)*, 1 *not applicable*.

Tests: of the 61 Go tests to port (MAP's 43 `missing` + 18 in the 7 new Go test files), 46 are ported
(31 fail and are ignored, 15 pass as guards), 4 are R11 feature-deferred, 7 helper-deferred and 4
skipped as inexpressible (none are 8b–8d). With the 9 changed tests whose updated ports fail, 40
test functions carry `#[ignore = "8a bug: …"]` (gadk-engine 31, gadk-model 3, gadk-gemini 6; the
rstest `test_parallel_function_calls` has 3 cases).

#### Feature-deferred tests (not written; for the D1 fix task)

- R11 `internal/llminternal/functions_test.go`: `TestGenerateAuthEvent`, `TestGenerateAuthEventOneCallPerCredentialKey`,
  `TestGenerateAuthEventWithoutRequests`, `TestGenerateAuthEventRequestThatDoesNotEncode` (#2114; need
  `EventActions.requested_auth_configs` and a `generate_auth_event`).
- R11 `internal/llminternal/base_flow_test.go` `TestMergeEventActions`: the two new rows
  `requested auth configs from both calls kept` and `requested auth configs kept when only the later call asks`
  (the port stays at its 18 rows until `requested_auth_configs` exists).

#### Helper-deferred tests (not written; not R9–R14, need an internal gadk helper the fix adds)

- `internal/llminternal/googlellm/variant_test.go` `TestIsGemini3xLive` (d8e47448; `is_gemini_3x_live`).
- `internal/llminternal/googlellm/live_artifacts_test.go` `TestIsInternalLiveArtifactPart` (22ff1839;
  `is_internal_live_artifact_part`, `LIVE_ARTIFACT_NAMESPACE`).
- `internal/llminternal/base_flow_live_test.go` `TestIsEmptyLiveResponse` (8cddb9d4; `is_empty_live_response`).
- `internal/llminternal/tool_sub_branch_test.go` `TestIsToolSubBranchEvent` (ab78c5ae; `is_tool_sub_branch_event`).
- `internal/llminternal/output_schema_binding_test.go` `TestOutputSchemaFor`, `TestOutputSchemaForCopiesBoundSchema`,
  `TestWithBoundOutputSchemaNilShadowsOuter` (be529e7b; `with_bound_output_schema`, `output_schema_for`).

#### Skipped tests

- `stream_aggregator_test.go` `TestStreamingResponseAggregatorDoesNotRecopyText`: measures Go `runtime.MemStats`
  allocation growth (`strings.Builder` vs `+=`); gadk's `String::push_str` is amortized linear by construction.
- `base_flow_live_test.go` `TestRunLiveSkipsNilRealtimeInput`, `TestHasRealtimeInput`: Go typed nil pointers as
  `RealtimeInput`; gadk's is an enum.
- `base_flow_live_test.go` `TestSendAfterToolWriteFailure`: the failing write comes from `json.Marshal` rejecting
  NaN; a `serde_json::Value` cannot hold NaN.

#### Appendix: changed Go tests

| Go test | kind | what was done |
|---|---|---|
| agent_transfer_test.go TestTransferToAgentToolRun | semantic (port updated) | tool built over a real sub-agent; new `UnknownAgent` row; every invalid-argument row wants `empty agent_name`; fails (c8fff670) |
| base_flow_live_test.go TestRunLiveNoGoroutineLeak | semantic (port updated) | the parent-cancel server sends a model turn first (8cddb9d4: setupComplete no longer surfaces); passes |
| base_flow_live_test.go TestRunLiveCloseStopsIdleSession | semantic (port updated) | the server sends one model turn before idling; passes |
| base_flow_telemetry_test.go TestGenerateContentTracing, TestGenerateContentTracingNoFinalResponse, TestLoggingSpanIDPropagation, TestInferenceEventDescribesTheRequestTheSpanDoes | semantic (port updated) | a `generateContent` item error now fails the test; pass |
| base_flow_telemetry_test.go TestGenerateContentTracingError | cosmetic (nolint comment) | port unchanged |
| base_flow_test.go TestCallTool | semantic (port updated) | two panic rows (e83ab5b0); 16 rows; fails |
| base_flow_test.go TestMergeEventActions | semantic (R11, deferred) | port unchanged until `requested_auth_configs` exists (two rows, see above) |
| clone_test.go TestClone | cosmetic (#2290) | port unchanged |
| compaction_concurrent_test.go TestCompactionSurvivesAConcurrentWriter | cosmetic (#2282 `wg.Go`) | port unchanged |
| compaction_processor_test.go TestCompactionProcessorSkipsOnCancelledContext | cosmetic (nolint comment) | port unchanged |
| contents_processor_test.go TestContentsRequestProcessor | semantic (port updated) | five `_adk_live` rows (22ff1839); fails |
| foreign_thoughts_test.go TestContentsRequestProcessor_ForeignThoughts, TestContentsRequestProcessor_ForeignThoughtCurrentTurn | semantic (port updated) | seven metadata-only rows each (6caa3b85); fail |
| functions_test.go TestGenerateRequestConfirmationEventResponseOrder | cosmetic (#2282 `range 10`) | port unchanged |
| handle_function_calls_async_test.go TestHandleFunctionCalls_FRSkipGates | semantic (port updated) | asserts the response (`{"result": null}` for a nil result); fails (1ced38ba) |
| instruction_processor_test.go TestInjectSessionState | semantic (port updated) | six inline-data artifact rows (860c7dac); fails |
| live_reconnect_test.go TestRunLiveReconnectSurvivesATransientDrop, TestRunLiveCeilingSparesLongLivedConnections, TestRunLiveCeilingSparesAnIntermediaryCycledSession, TestRunLiveConsecutiveBudgetSparesASilentSession | cosmetic (#2282 `wg.Go`, comment) | ports unchanged |
| live_reconnect_test.go TestRunLiveCeilingIgnoresConsumerLatency | semantic (port updated) | each connection sends a turnComplete frame (8cddb9d4); passes |
| outputschema_processor_test.go TestOutputSchemaRequestProcessor | cosmetic (#2290) | port unchanged |
| request_confirmation_processor_test.go TestRequestConfirmationRequestProcessor | semantic (port updated) | four decoded-wrapper rows, `wantErr` (0d95aed4); fails |
| request_confirmation_processor_test.go TestRequestConfirmationResumeOrderIsStable | cosmetic (#2282) | port unchanged |
| stream_aggregator_test.go TestProgressiveSSEStreamingFunctionCallArguments, TestProgressiveSSEPreservesThoughtSignature, TestProgressiveSSEHandlesEmptyFunctionCall, TestStreamingFCChunkWithWillContinueButNoPartialArgs, TestStreamedArrayOfObjectsArgument, TestStreamedArrayOfStringsArgument, TestStreamedArrayArgumentScalarTypes | cosmetic (#2290 `ptr` → `new`) | ports unchanged |
| parallel_function_call_test.go TestParallelFunctionCalls (not in MAP's list: only its package-level expectations changed) | semantic (port updated) | the aggregated responses carry `ModelVersion` (8be9129e); fails (3 cases) |

Totals: 34 MAP `changed` tests = 16 semantic + 18 cosmetic (plus TestParallelFunctionCalls, semantic).

Removed or renamed tests in D1: none.

New Go test files (Task 13 adds them to ledger in-scope blocks): `base_flow_live_config_test.go`,
`foreign_function_response_test.go`, `foreign_payloads_test.go`, `output_schema_binding_test.go`,
`tool_sub_branch_test.go`, `googlellm/live_artifacts_test.go`, `googlellm/live_connection_test.go`.

### D2a — runner, agent, plugininternal (T6)

Detector run against `plan/8a-b2base` with ADK-Go at `0990f9e2cb9df71c579db6c9731efa403641fc51`.
D2b commits of MAP's `## D2` block (workflow, workflowagents, llmagent, internal/workflow*) are in
group D2b's section. 67fc01b1 is listed here too because a D2a test (`runner/output_schema_resume_test.go`)
is its evidence.

#### Feature-deferred tests (not written; for the R9/R10 fix task)

- R9 `agent/agent_on_error_test.go`: `TestAgentRunOnAgentErrorCallback`, `TestAgentRunOnAgentErrorCallbackRecoversPanic`, `TestAgentRunOnAgentErrorCallbackRunsBeforeYield`, `TestAgentRunOnAgentErrorCallbackNotifiesEveryPlugin` (#2229, #2260).
- R10 `internal/plugininternal/plugin_manager_context_test.go`: `TestPluginManager_HasPlugins`, `TestFromContext`, `TestTakeInheritedPlugins` (#1777).
- Helper-deferred (not R9–R14; need the `safe_subpath` helper the #1299 fix adds): `internal/utils/fs_test.go` `TestSafeSubpath_Windows`, `TestSafeSubpath_NonWindows`, `TestIsLinkLike`; `internal/utils/fs_disk_test.go` `TestSafeSubpath_OnDisk`, `TestSafeSubpath_FilesystemRoot`, `TestSafeSubpath_RootedLinkTargetWithoutDrive`, `TestSafeSubpath_DotDotAfterLinkInTarget`.

#### Appendix: changed Go tests

| Go test | kind | what was done |
|---|---|---|
| agent/common_context_delta_test.go TestNilDeltaStillReachesTheInvocation | cosmetic (#2290 `new(expr)`) | port unchanged |
| agent/common_context_test.go TestCommonContext_ContextFallbackDelegation | cosmetic (#2290) | port unchanged |
| agent/identity_concurrency_test.go TestIdentityIsRaceFreeAcrossConcurrentReaders, TestIdentityReadsTheSessionOnceAndNeverTears, TestIdentityAgainstASessionThatChangesUnderIt | cosmetic (#2282 `wg.Go`) | ports unchanged |
| agent/identity_matrix_test.go TestEveryContextProducingMethodIsTabulated, TestDeltaDerivationsReachWithICDeltaOnly | cosmetic (#2282 `reflect.TypeFor`, #2290) | ports unchanged |
| agent/identity_test.go TestEmptyDeltaCostsASessionlessForwarderItsUnwrap | cosmetic (#2290) | port unchanged |
| internal/adkcontext/adkcontext_test.go TestSourceMethodIsUnexported | cosmetic (#2282) | port unchanged |
| internal/agent/compactionctx/compactionctx_test.go TestMarkCompactedIsSafeUnderConcurrency | cosmetic (#2282) | port unchanged |
| internal/agent/runconfig/limits_test.go TestIncrementAndEnforceLLMCallsLimit | cosmetic (#2282) | port unchanged |
| internal/context/from_context_test.go TestIdentityDecisionMatrixCrossPackage | cosmetic (#2290) | port unchanged |
| internal/context/identity_concurrency_test.go TestIdentityReadsRaceTheInvocationMutators | cosmetic (#2282) | port unchanged |
| internal/utils/schema_test.go TestMatchType | semantic (port updated) | new case `nil value not nullable` (bec16021); passes |
| runner/compaction_test.go TestCompactionRunsWhenConsumerStopsEarly | semantic (port updated) | the first item's error now fails the test (#2282); passes |
| runner/compaction_test.go TestBeforeRunPluginCannotPlantACompactionRecord | cosmetic (#2290) | port unchanged |
| runner/run_node_test.go TestRunner_WorkflowNode_OutputAndPersistence | cosmetic (#2282 `slices.Contains`) | port unchanged |

Removed or renamed tests in D2a: none. (Go removed the helper `sessionHasOutput` only.) `runner/hitl_integration_test.go`'s
`newHitlAsker` change is cosmetic (#2290).

Inexpressible skip: `runner/runner_test.go` `TestRunner_UserMessageWithNilPartRuns`. `Content.parts` is a `Vec<Part>`,
so a message cannot hold a nil part.

### D2b — workflow, workflow agents, LLM agent (T7)

Group D2b = ADK-Go `workflow/`, `agent/workflowagents/{sequentialagent,parallelagent,loopagent}`,
`agent/workflowagent`, `agent/llmagent`, `internal/workflowinternal`, `internal/workflowroute`,
`internal/workflowwalk` (the last two have no change between `bb1fd51` and `0990f9e`). Every gadk
test named here is in `crates/gadk-engine` (`src/parity_tests/<module>.rs`, or `tests/<target>.rs`
for `agent_workflowagents_parallelagent_agent`). A failing port is written to Go's expectation and
carries `#[ignore = "8a bug: …"]`.

Other fix commits these tests reveal (rows owned by their groups): `agent_llmagent_llmagent::test_agent_transfer`
fails on 1ced38ba (#2281, `transfer_to_agent` result `{"result": null}`) and c8fff670 (#2161, an
unknown `agent_name` is a tool error, not a run failure).

#### Appendix: changed, new, removed Go tests

New Go test files (all tests ported): `agent/llmagent/llmagent_task_outputkey_test.go` (7: 6
ignored, 1 passes), `agent/llmagent/single_turn_context_test.go` (2: 1 ignored, 1 passes),
`workflow/agent_node_output_schema_test.go` (15: 7 ignored, 6 pass, 1 skipped
`TestAgentNode_InnerUntypedPlacementClearsOuterSchema` — needs the bound-output-schema API),
`workflow/model_output_schema_test.go` (2 skipped: `TestModelOutputSchema`, `TestModelOutputSchemaNil`
— need `workflow::model_output_schema`; ported by the be529e7b fix task).

Changed tests:
- `agent/llmagent/llm_agent_wrapper_test.go TestProcessLLMAgentOutput`: semantic (port updated) — 3 output_schema MessageAsOutput subcases; ignored.
- `agent/llmagent/llmagent_delegation_test.go TestDelegation_09_ChatPeerTransferAcrossSiblings`: cosmetic — `slices.Contains` / `slices.Backward`; its httprr was re-recorded and the test ignored by T1.
- `agent/llmagent/llmagent_saveoutput_test.go TestLlmAgent_MaybeSaveOutputToState`: semantic (port updated) — task-mode case; ignored.
- `agent/llmagent/llmagent_test.go TestAgentTransfer`: semantic (port updated) — `{"result": null}` and the unknown-agent subtest; ignored.
- `agent/workflowagents/parallelagent/agent_test.go TestNewParallelAgent`: semantic (port updated) — Timestamp is ignored too (the runner stamps it after 2649b4ad); passes.
- `agent/workflowagents/parallelagent/agent_test.go TestParallelAgent_PropagatesContextError`: cosmetic — `//nolint:errcheck` comment.
- `internal/workflowinternal/finish_task_tool_test.go TestFinishTaskTool_Run`: semantic (port updated) — 3 nullable/null cases; ignored.
- `workflow/hitl_test.go TestScheduler_HitlNode_AutoGeneratesInterruptID`: semantic (port updated) — counting UUID provider; passes.
- `workflow/tool_node_test.go TestToolNode_Run`: semantic (port updated) — new `function_call_id_from_uuid_provider` case; passes.
- `workflow/validation_test.go TestStartNodePresent`: semantic (port updated) — the empty-edges case is the separate ignored `test_start_node_present_empty_edges`.
- `workflow/scheduler_test.go TestScheduler_CallerBreakStopsScheduling`: cosmetic — the port already asserts the first item is Ok.
- `workflow/run_node_test.go` `TestRunNode_WithRunID_ConcurrentSharesInterrupt`, `TestRunNode_WithRunID_WaiterUnblocksOnCancel`, `TestRunNode_WithRunID_ConcurrentSharesPanicAsFailure`, `TestRunNode_ConcurrentChildren_NoRace`: cosmetic (`wg.Go`, `slices.Backward`, `new(true)`).
- `workflow/parallel_worker_test.go` `TestParallelWorker_Concurrency`, `_FailFast`, `_SchedulerDoesNotRetryOnFailure`, `_PerItemSpans`, `_RetryEmitsSpanPerAttempt`, `_MaxConcurrencyFailFast`: cosmetic (`atomic.Int32`, nolint comments).
- `workflow/agent_node_mode_mutation_test.go` (3 tests): cosmetic (`wg.Go`).
- `workflow/base_node_test.go` (2), `workflow/function_node_test.go`, `workflow/persistence_test.go` (2), `workflow/parallel_worker_branch_test.go`: cosmetic (`new(expr)`).
- `workflow/dynamic_scheduler_test.go`, `workflow/max_concurrency_test.go` (2), `workflow/review_cache_regressions_test.go` (4), `workflow/review_cleanup_retry_test.go`, `workflow/scheduler_test.go TestScheduler_ValidateOutput_NoOutputSkipsValidation`, `workflow/workflow_node_test.go TestNestedWorkflow_Cancellation`: cosmetic (range-over-int, `wg.Go`, `new(expr)`).
- `agent/workflowagent/hitl_test.go`, `agent/workflowagent/reentry_test.go`: helper-only cosmetic changes (`maps.Copy`, `new(true)`); no test body changed.

Removed: `workflow/agent_node_mode_placement_test.go TestAgentNode_UnnamedAgent_IsStillPlacedSingleTurn`
— Go removed it with d0c978b0 (`agent.New` rejects an empty name; D2a); the port
`workflow_agent_node_mode_placement::test_agent_node_unnamed_agent_is_still_placed_single_turn` is removed.

Feature-deferred (R9–R14): none in D2b.

### D3 — tools and plugins (T8)

ADK-Go `tool/*` (agenttool, functiontool, exitlooptool, exampletool, mcptoolset, skilltoolset,
loadartifactstool, loadmemorytool, preloadmemorytool, toolsearch, geminitool, toolauth,
toolconfirmation) and `plugin/{functioncallmodifier, retryandreflect, loggingplugin}` + `plugin/*.go`,
`bb1fd51` → `0990f9e`. Every new or changed Go test was ported against today's gadk: a failing port
keeps Go's expectation and carries `#[ignore = "8a bug: …"]`; a passing one is kept as a guard.
`tool/bigquerytoolset` (8d) and the Vertex Example Store provider (8b) are out of scope.

Summary: 14 *real gadk bug*, 2 *real gadk bug (feature)* (R9, R10), 2 *Go-only (guard kept)*, 1 *not applicable*.

#### Appendix A — changed Go tests

| Go test | classification | what was done |
|---|---|---|
| plugin/functioncallmodifier/plugin_test.go TestAfterModelCallback | semantic (port updated) | case "nil part before the function call" (an empty `Part` first) and the first-function-call-part check added; passes |
| plugin/retryandreflect/plugin_test.go TestRetryAndReflect_MaxRetries | semantic (port updated) | asserts "failed consecutively 2 times"; ignored (4417f2d4) |
| tool/agenttool/agent_tool_test.go TestAgentTool_Run_CodeParts | semantic (port updated) | "empty and thought parts" wants `{"result": ""}`; ignored (a8a9b936) |
| tool/agenttool/agent_tool_test.go TestAgentTool_Run_AllThoughtPartsReturnsEmpty | semantic (port updated) | wants `{"result": ""}`; ignored (a8a9b936) |
| tool/agenttool/agent_tool_test.go TestAgentTool_Run_EmptyModelResponse | semantic (port updated) | wants `{"result": ""}`; ignored (a8a9b936) |
| tool/agenttool/agent_tool_test.go TestAgentTool_Run_SkipSummarization | semantic (port updated) | two input-schema cases added; passes |
| tool/exampletool/tool_test.go TestExampleTool_ProcessRequest | semantic (port updated) | 9 cases added, FunctionCallExample_Gemini2 expects plain fences; ignored (5199d9fe) |
| tool/exitlooptool/tool_test.go TestExitLoopToolExitsLoopAgent | semantic (port updated) | the exit_loop response is `{"result": null}`; ignored (1ced38ba) |
| tool/functiontool/function_test.go TestFunctionTool_OutputSchemaMismatch | cosmetic | `new(3)` (#2290); no change |
| tool/functiontool/streaming_function_test.go TestStreamingFunctionToolStopsHandlerWhenConsumerStops | semantic (port already asserted it) | Go now fails on an error item; the port already asserts `Some(Ok("first"))`; no change |
| tool/functiontool/streaming_function_test.go TestStreamingFunctionToolPropagatesCleanupPanicAfterStop | semantic (port updated) | asserts the first item is `Ok`; passes |
| tool/loadartifactstool/load_artifacts_tool_test.go TestLoadArtifactsTool_Run | semantic (port updated) | error cases expect the INVALID_ARGUMENTS result, 2 cases added; ignored (f1f0d2de) |
| tool/mcptoolset/client_cancellation_test.go TestSessionWaitCancellation | cosmetic | Fatalf message only; no change |
| tool/mcptoolset/client_cancellation_test.go TestCloseWaitsForConnectionInProgress | cosmetic | Fatalf message only; no change |
| tool/mcptoolset/tool_test.go TestMCPToolRunContent | cosmetic | fixture sets the new `originalName` (R14); no behaviour change; no change |
| tool/preloadmemorytool/tool_test.go TestPreloadMemoryTool_ProcessRequest | semantic (port updated) | search log, multi-part query cases; ignored (ff9e77c5) |
| tool/skilltoolset/instruction_test.go TestDefaultInstructionMatchesLoadSkillSchema | cosmetic | #2282 modernize; no change |
| tool/skilltoolset/internal/skilltool/tools_test.go TestListSkills | semantic (port updated) | `"skills"` → `"result"`; ignored (8ed375d1); gadk-only `declarations_match_go` updated and ignored too |
| tool/skilltoolset/skill/filesystem_source_test.go TestFileSystemSource_LoadInstructions | semantic (port updated) | "Trims surrounding whitespace" case; ignored (8ed375d1) |
| tool/toolsearch/toolsearch_test.go TestSearch_OverlappingParallelCallsReportAlreadyAvailable | cosmetic | `for i := range 20` (#2282); no change |

14 semantic, 6 cosmetic. Removed / renamed tests: none.

#### Appendix B — new Go tests

| Go test | result |
|---|---|
| tool/agenttool TestAgentTool_Run_CompositeAgentInputSchema | ported, ignored (31111f89) |
| tool/agenttool TestAgentTool_Run_AllThoughtPartsReturnsEmpty_WithOutputSchema | ported, ignored (a8a9b936) |
| tool/agenttool TestAgentTool_Run_NoContentReturnsEmptyResult | ported, ignored (a8a9b936) |
| tool/agenttool TestAgentTool_Run_PendingConfirmationReturnsError | ported, ignored (f25d1424) |
| tool/agenttool TestAgentTool_Run_AnsweredLongRunningCallSucceeds | ported, passes |
| tool/agenttool TestAgentTool_Run_SkipSummarizationPendingErrorReachesParent | ported, ignored (f25d1424) |
| tool/agenttool TestAgentTool_Run_StreamedArgsAnsweredLongRunningCallSucceeds | ported, passes |
| tool/agenttool TestAgentTool_Run_StreamedArgsUnansweredLongRunningCallReturnsError | ported, ignored (f25d1424) |
| tool/agenttool TestAgentTool_Run_SkipSummarizationNotSetOnInvalidArgs | ported, ignored (4cc7ce5f) |
| tool/exitlooptool TestExitLoopToolAfterToolCallbackSeesNoResult | ported (adapted: nil result = empty map), passes |
| tool/exitlooptool TestExitLoopToolAfterToolCallbackReplacesResult | ported, passes |
| plugin/functioncallmodifier TestAfterModelCallback_ContentlessResponseKeepsChain | ported, ignored (128135a8) |
| plugin/functioncallmodifier TestBeforeModelCallback_LeavesToolDeclarationUnchanged | ported (adapted: no task-agent-tool case), passes |
| plugin/retryandreflect TestRetryAndReflect_ToolPanic | ported (adapted), ignored (e83ab5b0) |
| plugin/retryandreflect TestRetryAndReflect_ReturnedErrorReachesTheModel | ported (adapted: `ToolError::Execution` for `%T`), passes |
| plugin/retryandreflect TestToolPanicThroughRunner | ported, ignored (e83ab5b0) |
| plugin/retryandreflect TestRetryAndReflect_AfterRunDropsInvocationCounts | ported, ignored (7fafcbbc) |
| plugin/retryandreflect TestRetryAndReflect_AfterRunKeepsCountsOfPausedInvocation | ported, ignored (7fafcbbc) |
| plugin/retryandreflect TestRetryAndReflect_AfterRunKeepsGlobalCounts | ported, passes |
| plugin/retryandreflect TestRetryAndReflect_AfterRunDropsCountsOfEveryPerInvocationScope | ported (adapted: closed enum), ignored (7fafcbbc) |
| plugin/retryandreflect TestRetryAndReflect_AfterRunScansEventsOnlyOfInvocationWithCounts | ported, ignored (7fafcbbc) |
| plugin/retryandreflect TestRetryAndReflect_RunnerContinuesCountsOfAnsweredRun | ported, ignored (7fafcbbc) |
| tool/exampletool TestExampleTool_ProcessRequestStableArgumentOrder | ported, passes (e342d2f6 guard) |
| tool/exampletool TestNew_BothExamplesAndProviderError | skipped: 8b — example providers |
| tool/exampletool TestExampleTool_ProcessRequest_Provider | skipped: 8b — example providers |
| tool/mcptoolset TestCommandTransportReconnect | ported (adapted), ignored (1a84f04f) |
| tool/mcptoolset TestCommandTransportReconnectKeepsSettings | skipped: no copied exec.Cmd in gadk |
| tool/mcptoolset TestCommandTransportSpecIsIndependent | skipped: no saved command spec in gadk |
| tool/mcptoolset TestCommandTransportHelper | skipped: Go's child-process helper |

New Go test files:
- `tool/mcptoolset/prefix_test.go` (TestMCPToolNamePrefixRunner, TestMCPToolNamePrefixRegistration,
  TestMCPToolNamePrefixFilter, TestMCPToolNamePrefixConfirmation) and `tool/mcptoolset/conversion_test.go`
  (TestToMCPTool, TestToMCPToolDoesNotModifyCallerSchema, TestToMCPToolServerRegistration,
  TestGeminiToJSONSchema): *feature-deferred* to R14, not written.
- `tool/exampletool/example_test.go`: only `ExampleNew_provider` (no Test func; not a parity-check test).
- `tool/exampletool/vertexai/vertexai_test.go`: 8b (Vertex Example Store; a new package no ledger tracks).

Counts: 24 ported (16 ignored as *real gadk bug*, 8 passing), 8 feature-deferred (R14), 2 skipped as
8b, 3 skipped as inexpressible.

### D4 — session, memory, artifact (T9)

Group D4 = ADK-Go `session/` (in-memory, `session.go`, `service.go`, `database`, `vertexai`,
`compaction`, `sessiontestsuite`), `memory/` (not `memory/vertexairag`, which is 8d), `artifact/`
(+ `internal/artifact`), `internal/sessionutils`, `internal/compactioninternal`, `internal/memory`,
diffed `bb1fd51..0990f9e`. Every new or changed Go test was ported against gadk as of
`plan/8a-b2base`; a port that fails keeps Go's expectation and carries
`#[ignore = "8a bug: <cause>"]`.

Totals: 6 *real gadk bug*, 2 *Go-only (guard kept)*.

#### Feature tests deferred (need an 8a feature to compile; not written)

| feature | Go file | Go tests | fix task |
|---|---|---|---|
| R11 RequestedAuthConfigs (5de88a7f #2114) | `session/event_json_test.go` | TestEventJSONEncodesRequestedAuthConfigsAsGiven | R11 session parts (T9 area) |
| R11 | `session/inmemory_test.go` | TestInMemoryService_AppendEvent_CopiesRequestedAuthConfigs; Test_inMemoryService (`ChecksRequestedAuthConfigs: true`) | R11 session parts |
| R11 | `session/sessiontestsuite/service_suite.go` | cases `requested_auth_configs_round_trip`, `requested_auth_config_that_does_not_decode_is_refused` (option `ChecksRequestedAuthConfigs`) | R11 session parts |
| R11 | `session/database/service_test.go` | Test_databaseService, Test_databaseServiceFromDB (`ChecksRequestedAuthConfigs: true`) | R11 session parts |
| R11 | `session/vertexai/service_test.go` | Test_vertexaiService (`ChecksRequestedAuthConfigs: true`; two new `casesWithoutReplay` entries) | R11 session parts |
| R11 | `session/vertexai/vertexai_test.go` | TestVertexAiClientRequestedAuthConfigsMatchPython, TestVertexAiClientRejectsRequestedAuthConfigThatDoesNotDecode, TestVertexAiClientRejectsRequestedAuthConfigThatDoesNotEncode, TestAppendEvent_requestedAuthConfigThatDoesNotDecode_leavesSessionUntouched | R11 session parts |
| R11 | `internal/sessionutils/requested_auth_test.go` (new) | TestCheckRequestedAuthConfigsReadsNumbersAsFloat64 | R11 session parts |
| R11 | `internal/compactioninternal/window_test.go` | new cases of TestLongestSelfContainedPrefix ("unresolved credential request blocks the prefix", "credential request resolved by the rerun is fine") and TestSelectSlidingWindowSurvivesBlockedHead ("unanswered credential request at the head is stepped over", "a tail answering a credential request left in the head is refused"); helper `withAuthRequest` | R11 session parts (gadk-engine compaction window) |
| R12 artifact:// references (640eb4f1 #1164) | `artifact/artifact_util_test.go` (new) | TestParseArtifactURI, TestBuildArtifactURI, TestIsArtifactRef, TestValidateArtifactReferenceScope | R12 artifact |
| R12 | `artifact/inmemory_references_test.go` (new) | TestInMemoryArtifactService_References, TestInMemoryArtifactVersionReferenceMimeType | R12 artifact |
| R13 incremental memory (9eb1e3b7 #2278) | `memory/inmemory_test.go` | TestAddEventsToMemory_BasicAndEntryFields, TestAddEventsToMemory_CustomMetadataIgnored, TestAddEventsToMemory_DefaultBucketDoesNotCollideWithUnknownSessionID, TestAddEventsToMemory_DeduplicationScopeAndWordlessReservations, TestAddEventsToMemory_AppendReplaceAndEmptyBucketOrder, Test_inMemoryService_SearchMemory_ConcurrentAddEvents, TestAddEventsToMemory_UnsupportedService, TestAddEventsToMemory_UnsupportedServiceNamesGoFallback, TestAddEventsToMemory_NilRequests | R13 memory |
| R13 | `memory/inmemory_search_test.go` | Test_inMemoryService_SearchMemory_PreservesBlobPartsAndSkipsThoughtsInAddedEvents, Test_inMemoryService_SearchMemory_TokenizationNFCAndScriptSplitting, Test_inMemoryService_SearchMemory_FullUnicodeLowercasing (all add through `AddEventsToMemory`; the NFC / full-case / script-split word rules also change `AddSessionToMemory` search) | R13 memory |
| R13 | `memory/example_test.go` (new) | ExampleAddEventsToMemory, ExampleAddEventsToMemory_unsupported, ExampleEventAdder, ExampleInMemoryService, ExampleInMemoryService_sessionBuckets (Go examples; parity-check does not track them) | R13 memory |

25 Go tests deferred (+ 5 Go examples, + the R11 suite cases and window cases above).

#### Appendix: changed and new Go tests

Changed (MAP `changed`, 21):

| Go test | classification | action |
|---|---|---|
| artifact/gcsartifact/gcs_test.go TestGCSArtifactServiceConcurrentSave | cosmetic (`wg.Go`, #2282) | none |
| artifact/request_validation_test.go TestSaveRequest_Validate | semantic (port updated) | new case "Artifact reference part is rejected by the shared contract" added to `gadk-artifact` `parity_tests::artifact_request_validation::test_save_request_validate`; passes |
| internal/compactioninternal/window_test.go TestLongestSelfContainedPrefix | semantic (feature-deferred, R11) | two credential-request cases need `EventActions.requested_auth_configs`; port unchanged |
| internal/compactioninternal/window_test.go TestSelectSlidingWindowSurvivesBlockedHead | semantic (feature-deferred, R11) | two credential-request cases need `requested_auth_configs`; port unchanged |
| memory/inmemory_search_test.go Test_inMemoryService_SearchMemory_LimitAndSessionOrder | cosmetic (`range` ints, #2282) | none |
| memory/inmemory_search_test.go Test_inMemoryService_SearchMemory_ReplacedSessionOrder | cosmetic (#2282) | none |
| memory/inmemory_test.go Test_inMemoryService_SearchMemory_Concurrent | cosmetic (`wg.Go`, same two goroutines per worker) | none |
| session/compaction/llm_summarizer_test.go TestLLMSummarizerTranscriptCannotForgeTurns | cosmetic (`strings.SplitSeq`) | none |
| session/compaction/llm_summarizer_test.go TestFormatEventsEscapesAuthorAndToolNames | cosmetic (`strings.SplitSeq`) | none |
| session/compaction/llm_summarizer_test.go TestConcurrentSummarizationsDoNotShareOneGenConfig | cosmetic (`wg.Go`) | none |
| session/database/service_test.go Test_databaseService | semantic (feature-deferred, R11) | `ChecksRequestedAuthConfigs` suite option |
| session/database/service_test.go Test_databaseServiceFromDB | semantic (feature-deferred, R11) | same |
| session/database/service_test.go TestEventsSharingATimestampComeBackInAStableOrder | semantic (port updated) | IDs are now `event-{i}` as in Go (e9aba9e2 #2186); the port still asserts the exact order (adapted note updated) |
| session/database/service_test.go TestDatabaseService_AppendEvent_LocksSharedStateRows | cosmetic (recorder moved to `recordStateRowStatements`, which also labels `on conflict do update`; expected statements unchanged) | none |
| session/database/session_test.go TestLocalSessionEventsConcurrentAppend | cosmetic (`wg.Go`) | none |
| session/inmemory_test.go Test_inMemoryService | semantic (feature-deferred, R11) | `ChecksRequestedAuthConfigs` suite option |
| session/inmemory_test.go Test_inMemoryService_CreateConcurrentAccess | cosmetic (`wg.Go`) | none |
| session/inmemory_test.go TestInMemoryState_All_ConcurrentSet | cosmetic (`wg.Go`) | none |
| session/vertexai/service_test.go Test_vertexaiService | semantic (feature-deferred, R11) | suite option + two `casesWithoutReplay` entries |
| session/vertexai/session_test.go TestLocalSessionEventsConcurrentAppend | cosmetic (`wg.Go`) | none |
| session/vertexai/vertexai_test.go TestEventActionsConverters | cosmetic (Go's converter now returns an error for the R11 decode; same cases) | none |

8 semantic (2 ports updated, 6 feature-deferred), 13 cosmetic.

Shared suite (non-test Go file): `internal/artifact/tests/service_suite.go` (569e96d1 #2131) adds
`LoadNonexistentVersion` and a `Load(version 3)` after deleting version 3; ported into
`gadk-artifact` `src/testsuite.rs` (`basic`), passing for the in-memory and GCS services.
`session/sessiontestsuite/service_suite.go` adds only the two R11 cases (deferred above) and a
`maps.Insert` rewrite.

New Go test files:

| Go file | Go tests | action |
|---|---|---|
| session/inmemory_internal_test.go | TestCompareByLastUpdate | ported, adapted (through `List` across users; gadk has no comparator) in `crates/gadk-session/tests/inmemory.rs` `test_compare_by_last_update`; fails, ignored (8be7e250) |
| artifact/clone_test.go | TestClonePartCopiesEveryField, TestClonePartKeepsNilAndEmptyDistinct, TestClonePartStopsAtDepthBound, TestClonePartSelfReferencingMetadata | skipped (Go reflection / pointer identity / nil maps / self-referencing maps; a gadk `Part` is an owned value with a derived `Clone`) in `crates/gadk-artifact/tests/artifact_clone.rs` |
| artifact/artifact_util_test.go, artifact/inmemory_references_test.go | 6 tests | feature-deferred (R12) |
| internal/sessionutils/requested_auth_test.go | 1 test | feature-deferred (R11) |
| memory/example_test.go | 5 examples | feature-deferred (R13); not parity-tracked |

Removed or renamed Go tests: none.

### D5 — servers, remote agent v2, agent registry, launcher (T10)

Group D5 = ADK-Go `server/` (adkrest incl. controllers, triggers, `internal/{models,routers,services}`;
adka2a v2; agentengine; authn; authz), `agent/remoteagent/v2`, `agentregistry`, `internal/originguard`,
`cmd/launcher/*`. A2A v1 (`agent/remoteagent` root, `server/adka2a` root) is not ported. The 19 MAP
fix commits, each judged by the Go tests ported against gadk at `plan/8a-b2base` (main 6776321 + T1–T4).
T1's ignores over regenerated goldens ([Go output changed](#go-output-changed)) are referenced, not repeated.

Summary: 15 *real gadk bug*, 4 *Go-only (guard kept)* (3193128b, 2278f8f5, 123fea94, ead2778f).

#### Not fix commits, but their tests are in D5

| commit | Go tests | verdict | note |
|---|---|---|---|
| 5de88a7f feat(session): add RequestedAuthConfigs to event actions (#2114) | `server/adkrest/internal/models/event_test.go` `TestRequestedAuthConfigsGoOutWithoutSecrets`, `TestRequestedAuthConfigsEncodeFailureFailsTheEvent`, `TestRequestedAuthConfigsAreNotTakenFromRequests` | *real gadk bug (feature)* | need R11 (`EventActions.requested_auth_configs`) to compile; not written — the R11 REST-redaction fix task (D5) ports them with `models/redact.go` |
| 1523862f feat(auth): let tools ask the end user to sign in (#2231) | `server/adkrest/internal/models/event_test.go` `TestRequestCredentialPartsGoOutWithoutSecrets`, `TestOtherFunctionPartsKeepSecretLookingFields`, `TestRequestCredentialRedactionLeavesTheStoredEventAlone`, `TestStateDeltaCredentialsGoOutWithoutSecrets`; `server/adkrest/internal/models/session_test.go` `TestSessionStateCredentialsGoOutWithoutSecrets`; `server/adkrest/controllers/sessions_test.go` `TestUpdateSessionEchoedRedactedCredentials` | 8c skip | sign-in's REST credential redaction (parts, state, rename echo): `// parity: skipped …: 8c — sign-in`. Its `TestUpdateSessionWritesAnUnchangedPlainKey` needs no sign-in code and is ported (passes) |

#### Appendix: changed Go tests

- server/adkrest/controllers/debug_test.go TestSessionSpansHandlerEmptyContainersSerializeAsJSONContainers — *cosmetic* (body identical; the hunk is the new test after it).
- server/adkrest/controllers/runtime_live_test.go TestRunLiveHandler_PingWriteDeadlineIsKeepaliveTimeout — *cosmetic* (`//nolint` comment).
- server/adkrest/controllers/runtime_test.go TestRunSSEHandler — *cosmetic* (body identical; new tests follow it).
- server/adkrest/controllers/sessions_test.go TestGetSession — *semantic (port updated)*: the fake wraps `session.ErrNotFound`, so a missing session is 404 `session not found: "testSession"` (ca7db168); fails, ignored.
- server/adkrest/controllers/sessions_test.go TestUpdateSession — *cosmetic* (`map[string]any(got.State)` after the `stateValues` type).
- server/adkrest/controllers/triggers/pubsub_test.go TestPubSubTriggerActuallyRunsCompaction, TestPubSubTriggerHandler_ZeroMaxConcurrentRunsIsUnlimited — *cosmetic* (#2290 `new(0)`).
- server/adkrest/controllers/triggers/pubsub_test.go TestPubSubTriggerHandler_EnforcesMaxConcurrentRuns — *cosmetic* (`wg.Go`).
- server/adkrest/handler_security_test.go TestMaxBytesMiddlewarePinsTheDefaultLimit — *cosmetic* (`errors.AsType`).
- server/adkrest/internal/models/event_actions_test.go TestToSessionEvent_EventActions, TestFromSessionEvent_RoundTrip — *cosmetic* (`ToSessionEvent` now also returns an error).
- server/adkrest/internal/models/event_test.go TestEventRoundTripPreservesWorkflowFields — *semantic (port updated)*: timestamp round trip (4db0bd99); fails, ignored.
- server/adkrest/internal/models/event_test.go TestCompactionIsReadOnlyOverREST — *cosmetic* (`ToSessionEvent` error return).
- server/adkrest/internal/routers/auth_test.go TestRouterAuthentication — *cosmetic* (loop-variable copy removed).
- server/adkrest/internal/services/agentgraphgenerator_test.go TestEdgeHighlighted — *cosmetic* (#2290 `new(true)`).
- server/adkrest/internal/services/debugtelemetry_test.go TestDebugTelemetryGetSpansBySessionID, TestDebugTelemetryGetSpansByEventID, TestConvertRecordsSynthesizesEventIDForFailedGenerateContent — *cosmetic* (attribute map typed `map[string]any`, every expected value still a string; the c377836c fix retypes the port's `BTreeMap<String, String>`).
- server/adkrest/internal/services/debugtelemetry_test.go TestInferenceDetailsPartsMatchWebUI — *cosmetic* (iterator error now checked).
- server/adkrest/sessions_integration_test.go TestCreateSessionRejectsMalformedChunkedBody — *cosmetic* (body identical; new test follows it).
- cmd/launcher/web/web_test.go TestRunDoesNotAnnounceURLWhenTelemetryInitFails — *cosmetic* (`strings.Cut`).
- agent/remoteagent/a2a_agent_compat_test.go TestCompat_A2ACleanupPropagation — *not applicable* (A2A v1 compat, out of scope: ledger 5b deferred).
- agent/remoteagent/v2/a2a_agent_run_processor_test.go TestA2AAgentRunProcessor_aggregatePartial — *semantic (port updated)*: two cases (thought and text each streamed in chunks; text around a file part), agent named `test`; passes.
- agent/remoteagent/v2/a2a_agent_test.go TestRemoteAgent_AfterCallbackRunsOnAggregatedArtifact — *cosmetic* (`slices.Contains`).
- agent/remoteagent/v2/a2a_e2e_test.go TestA2AMultiHopInputRequired — *semantic (port updated)*: transfer response `{"result": null}` (1ced38ba); fails, ignored.
- agent/remoteagent/v2/a2a_e2e_test.go TestA2ACleanupPropagation — *cosmetic* (`wg.Go`).
- agent/remoteagent/v2/a2a_e2e_test.go TestA2ASingleHopFinalResponse — *semantic (port updated)*: the "internal error" case uses a named agent and a session service whose Create fails (`failed to create a session: session creation failed`); passes.
- agent/remoteagent/v2/auth_test.go TestRemoteAgent_AuthAcceptedByEnforcingServer, TestRemoteAgent_AuthRefusesCrossOriginRedirect, TestRemoteAgent_AuthRefusesADifferentHostname, TestRemoteAgent_AuthFailsClosed, TestRemoteAgent_AuthRefusesAnInsecureCardSource — *semantic (port updated)*: the failure is `run`'s error, not an error event (4d841aba); fail, ignored.
- agent/remoteagent/v2/auth_test.go TestRedactTokenError, TestRedactTokenErrorSeesThroughAWrapper — *cosmetic* (comment, `errors.AsType`).
- agent/remoteagent/v2/auth_test.go TestRemoteAgent_AuthConcurrentInvocations — *cosmetic* (`wg.Go`).
- agent/remoteagent/v2/auth_test.go TestAuthTransportSharesAMintWithinAScope — *cosmetic* (nolint comment).
- agentregistry/factory_test.go TestA2AClientFactory_CompatProtocolVersion — *semantic (port updated)*: unknown-major (2.0) cases want an error; fails, ignored.
- agentregistry/mcptoolset_e2e_test.go TestMCPToolset_E2E — *cosmetic* (`slices.Contains`).
- agentregistry/remoteagent_e2e_test.go TestRemoteAgent_E2E — *semantic (port updated)*: Go's 9-case table (binding × version); the 1.0 cases run, the 0.3 cases need an A2A 0.3 server (adapted); passes.
- agentregistry/requester_test.go TestRestRequester_Get_DecodeError — *cosmetic* (`errors.AsType`).
- server/adka2a/v2/agent_card_test.go TestGetAgentSkills_LLMAgent — *semantic (port updated)*: description is the agent's own (`Test llm.`); fails, ignored.
- server/adka2a/v2/events_test.go TestToSessionEvent — *semantic (port updated)*: `SkipSummarization` on four terminal-task cases, new empty-message status-update case; fails, ignored (d6bb1e86).
- server/adka2a/v2/parts_test.go TestPartsTwoWayConversion — *semantic (port updated)*: "file bytes with video metadata" case; fails, ignored (5fd47b21).

#### Appendix: removed / renamed Go tests

- agent/remoteagent/v2/a2a_agent_test.go TestRemoteAgent_ErrorEventIfNoCompatibleTransport → TestRemoteAgent_ErrorIfNoCompatibleTransport, TestRemoteAgent_ErrorEventOnServerError → TestRemoteAgent_ErrorOnServerError: ports renamed and rewritten to Go's expectation (ignored, 4d841aba).
- server/adka2a/v2/agent_card_test.go TestReplacePronouns: removed with `replacePronouns`; the port `test_replace_pronouns` is deleted (gadk's `replace_pronouns` and its unit test `replace_pronouns_respects_ascii_word_boundaries_and_unicode_folding` in `crates/gadk-a2a/src/server/agent_card.rs` go with the 1c354701 fix).

#### Appendix: new Go test file

- `server/adkrest/internal/models/session_test.go` (new; ledger 5a in-scope addition): `TestFromSessionAcceptsLastUpdateInFirstSecondOf1970` ported to `crates/gadk-server/src/parity_tests/server_adkrest_internal_models_session.rs` (fails, ignored: 4db0bd99); `TestSessionStateCredentialsGoOutWithoutSecrets` skipped (8c — sign-in).

### D6 — models (T11)

Detector for ADK-Go `model/` at `0990f9e` against gadk at the T1–T4 base. Verdicts follow BRIEF.md.
`gadk` ignores carry `#[ignore = "8a bug: …"]`.

No `model/anthropicmodel` fix commit is in the group; its changed Go tests are below.

#### Missing tests (MAP `missing`)

| Go test | gadk | verdict |
|---|---|---|
| `gemini_test.go TestModel_GenerateWithoutCandidates` | `gemini::test_model_generate_without_candidates` | ignored (*real gadk bug*, 15e9d4cd) |
| `gemini_test.go TestModel_MaybeAppendUserContent` | `gemini::test_model_maybe_append_user_content` (adapted: the private fn is observed through the request body) | passes (*Go-only (guard kept)*) |
| `apigee_test.go TestGenerateContentWithRequestModel` | `model_apigee_apigee::test_generate_content_with_request_model` | ignored (*real gadk bug*, 71b0761f) |
| `completions/media_test.go TestBuildParams_ToolResponseBeforeMedia` | `completions_media::test_build_params_tool_response_before_media` | passes (guard) |
| `completions/media_test.go TestBuildParams_FunctionCallBeforeMediaRoleError` | `completions_media::test_build_params_function_call_before_media_role_error` | passes (guard) |
| `completions/request_test.go TestBuildParams_CodeExecutionAsText` | `completions_request::test_build_params_code_execution_as_text` | ignored (ded357e3) |
| `responses/model_test.go TestModel_GenerateStream_CompletedTextAfterCallIDFallback` | `…stream_content::test_model_generate_stream_completed_text_after_call_id_fallback` | ignored (a1603c7f) |
| `responses/model_test.go TestModel_GenerateStream_CompletedContentSupersedesBeforeCallAdoption` | `…stream_content::test_model_generate_stream_completed_content_supersedes_before_call_adoption` | passes (guard) |
| `responses/request_test.go TestBuildParams_CodeExecutionAsText` | `responses_request::build_params::test_build_params_code_execution_as_text` | ignored (ded357e3) |
| `shared/contents_test.go TestCodeExecutionText` | **not written** (needs `shared::contents::code_execution_text`, which does not exist; the file would not compile) | deferred to the ded357e3 fix task |
| new file `responses/diagnostics_test.go TestModel_GenerateStream_TerminalConversionDiagnostic` | `responses_diagnostics::test_model_generate_stream_terminal_conversion_diagnostic` (new target file; tracing dev-deps added) | ignored (450b4a54) |

Totals over the 11 rows: 6 ignored, 4 passing guards, 1 deferred (compile).

#### Appendix: changed Go tests

*semantic (port updated)*

- `responses/request_test.go TestBuildParams_UnsupportedPart` — the unsupported part is now a `ToolCall` (code is sent); port updated, passes.
- `responses/request_test.go TestBuildParams_DropsReplayedThoughts` — case `thought_text_riding_on_code_is_still_rejected` became `…_drops_only_the_text`. The changed case is its own ignored test (`…_thought_text_riding_on_code_drops_only_the_text`) so the rest of the table stays a live guard (ded357e3).
- `responses/model_test.go TestModel_GenerateStream_TerminalToolCallsAreAuthoritative` — "the event's calls keep the place…" now streams `terminal` text the snapshot cannot use to mask adoption's ordering; port updated, passes.
- `completions/media_test.go TestBuildParams_MediaOnTheWire` — new case "executable code before an inline image keeps its order": its own ignored test (the other eight cases keep guarding) (ded357e3).
- `responses/request_test.go TestUnsupportedPayload_WalksEveryPartField`, `TestReplayedReasoning_EveryUnaccountedFieldDisqualifies` — `accountedForFields` gains `ExecutableCode`, `CodeExecutionResult` and `UnsupportedPayload` is called with `PartFieldCodeExecution`: **port not updated** (needs `PartField::CodeExecution`, absent; the old ports still pass and will fail once the fix adds the variant). Deferred to the ded357e3 fix task.
- `shared/contents_test.go TestUnsupportedPayload_EmittingMediaDoesNotHideAnythingElse` — one new assertion with `PartFieldCodeExecution`: **not updated**, same reason, same task.
- `shared/tools_test.go TestEnsureFunctionToolOnly` — new cases (FileSearch, ParallelAISearch, MCPServers, ExaAISearch alone / beside functions) and `errors.Is(ErrUnsupportedConfigField)`; port updated, ignored (275e190e).
- `anthropicmodel_test.go TestGenerateContentConfigValidation` (cases `generation_output_config`, `store`) and `TestGenaiConfigReflectionCompleteness` (two new fields) — semantic; already ported by T2 (`behavior.rs`), not redone.

*cosmetic (no port change)*

`new(expr)` / `reflect.TypeFor` / `reflect.TypeAssert` / `wg.Go` / `typ.Fields()` / loop-variable / `//nolint` rewrites (#2282, #2290):
anthropicmodel `TestNewModel_AnthropicCredentialsAndConfig`, `TestToolsAndToolConfig`, `TestConcurrencyAndNonMutation`, `TestTruncatedStreamedToolCall`, `TestMaxRetriesAndInFlightNonStreamingCancellation`; gemini `TestContinuation_AutomaticContinuationOff`, `TestModel_RespectsRequestModel`; completions `TestApplyGenerationConfig_{EndpointOnlyFields,TranslatedFields,AcceptsAutomaticContinuation,RejectedFields,RefusesFieldsOutsideTheSupportedLists,SeedAccepted}`, `TestApplyThinkingConfig_RejectsNonsenseBudget`; responses `TestReplayedReasoning`, `TestApplyGenerationConfig{RejectsUnsupportedFields,RefusesFieldsOutsideTheSupportedLists,AcceptsAutomaticContinuation,ThinkingConfig,OmitsReasoningSummaryUnlessAsked,RejectsNegativeThinkingBudget,UnspecifiedLevelYieldsToABudget,RejectsUntranslatableValues,AcceptsBoundaryValues,RejectsExplicitOff,RejectsNonPositiveTimeout,KeepsNamedErrorPrecedence}`, `TestTranslatedFieldsAreNotRejected`, `TestBuildParams_SystemInstructionWalksEveryPartField`; shared `TestRequestTimeoutGuardsNonPositiveItself`; `openaimodel_test.go TestNewModel_SelectsAPI`; `registry_test.go TestRegistryConcurrentAccess`.

- `responses/request_test.go TestBuildParams_Media` — comment only (the expectation, `inline_data` fallback filename, is unchanged; gadk already sends it) — cosmetic.
- `responses/model_test.go TestModel_GenerateStream_AdoptsTerminalToolCalls` — MAP lists it, but no hunk touches its body (hunk header context only) — cosmetic.

Non-test Go refactors with no behaviour change, not ported: anthropicmodel `cache.go` (`slices.Backward`), `errors.go` (`errors.AsType`), `response.go` (`min(max(…))`), apigee `maps.Copy`, gemini `new(*cfg.HTTPClient)`, completions `stream.go` (`new(chunk.Usage)`), docs-only comments (`media.go`, `errors.go`, completions `newMediaPart`, `doc.go`).

Removed or renamed Go tests: none.

### D7 — agentanalytics, configurable, telemetry, CLI, auth (T12)

Go packages: `plugin/agentanalytics`, `internal/configurable` (+ conformance), `internal/telemetry`,
`telemetry`, `cmd/adkgo`, `internal/cli`, `auth` (+ `auth/gcp`), `cmd/internal/adkcli`, from ADK-Go
`bb1fd51` to `0990f9e`. Every missing / new Go test is ported (or skipped with a reason); a port that
fails on today's gadk carries `#[ignore = "8a bug: …"]`.

Other D7 commits: ba5d42b8 (#2042 MeterProvider) and c7087e3e (#1981 Agent Engine deployment
environment) are 8b features (tests skipped `8b — …`); 1edb0572 (#2176) is docs plus pinning tests
(`plugin_agentanalytics_doc`, 12 pass as Go-only guards); e5f60ec6 (#2282) and 13acad55 (#2290) are
cosmetic; f06004dc (go-jose bump) is *not applicable*.

#### Appendix A — new Go tests

| Go file | tests | result |
|---|---|---|
| plugin/agentanalytics/json_formatter_test.go | TestResponseMessageSummaryLengthCap, TestResponseMessageSummaryFunctionNameCap | ported, ignored (#2246) |
| | TestJSONFormatter_CredentialNamedTypes, _CredentialByteSlice, _PlainCredentialStructMapKeys, _CollidingMapKeys | ported (adapted: Go defined types → Rust newtypes), ignored (#2249) |
| | TestJSONFormatter_InvalidUTF8, _OmitEmptyAndOmitZeroGuards, _ConcurrentCredentialStructNoRace, _EmbeddedRedactsWholeNoShadow, _DefinedUserinfoTypes, _CircularReferenceBudget | skipped (inexpressible: invalid UTF-8, omitzero reflection, -race, embedding, url.Userinfo, cycles) |
| plugin/agentanalytics/gcs_offloader_test.go | TestCleanGCSPrefix, TestGCSObjectPath_SanitizationAndBounds, TestBatchProcessor_ObjectPathCachesEffectivePrefix, TestGCSPathPrefix_RewriteWarningAndDisjointNamespace | ported (adapted, test-local mirrors of Go's unexported helpers), ignored (#2018) |
| | TestGCSOffload_RewrittenPrefixFitsHierarchicalNamespaceFolderLimit | ported, passes (Go-only guard) |
| plugin/agentanalytics/doc_test.go (new file) | 13 TestDocParity_* | 12 pass (Go-only guards), TestDocParity_ResponseSummaryAndFunctionNameBounds ignored (#2246) |
| | TestDocGo_ParityBulletsReferenceExistingTests | skipped (inexpressible: Go AST of doc.go) |
| internal/configurable/configurable_utils_test.go | TestResolveAgentReferenceSymlinkedParentDir, TestResolveConfigReferenceAcceptsLinksThatStayInside, TestResolveConfigReferenceAliasedDirectoryShareKey, TestResolveConfigReferenceEmptyRef, TestResolveConfigReferenceOSSpecificRefs, TestFromConfigRefusesEscapingAgentReferences, TestFromConfigDotDotAfterLinkInTarget | ported (adapted: `resolveConfigReference` driven through `resolve_agent_reference`; sentinels by text), ignored (#1299) |
| | TestResolveConfigReferenceVolumeQualifiedRefs, TestResolveConfigReferenceBelowNonDirectory | ported (adapted), pass (Go-only guards) |
| internal/configurable/configurable_workflow_ref_test.go (new file) | TestWorkflowNodeReferenceRejectsEscapingPath, TestWorkflowRouteMapReferenceIsChecked, TestWorkflowNodeRegistryDoesNotLeakAcrossDirectories, TestWorkflowNodeReferenceCacheDoesNotBypassCheck | ported, ignored (#1299) |
| | TestWorkflowNodeReferenceAllowsPathsInsideWorkflowDir | ported, passes (Go-only guard) |
| internal/configurable/configurable_utils_windows_test.go (new file) | TestResolveConfigReferenceRefusesJunction, TestResolveConfigReferenceAcceptsJunctionsInside | skipped (Windows-only) |
| telemetry/telemetry_test.go | TestResolveResourceProjectID | ported (adapted: injected environment), ignored (#2082) |
| | TestResolveResourceServiceInstanceID, TestTelemetryCustomMeterProvider, TestTelemetryShutdownReturnsReaderErrors, TestNewErrorLeavesNoReaderRunning, TestTelemetryNoReadersNoMeterProvider, TestOtlpEnvMetricShutdownDoesNotExport, TestGcpExporters, TestPeriodicReader | skipped `8b — MeterProvider (#2042)` |
| cmd/adkgo/internal/deploy/agentengine/environment_test.go (new file) | TestEnvironmentFlagsRegistered, TestEnvironmentFlagsRejectInvalidBeforePreparingDeployment, TestEnvironmentFlagsRejectStrayArguments, TestEnvironmentDeploymentRequests | skipped `8b — CLI Agent Engine deployment environment (#1981)` |

#### Appendix B — changed Go tests (83)

Semantic (7):

| Go test | what was done |
|---|---|
| configurable_utils TestResolveAgentReferenceRejectsEscapingConfigPath | port updated (config dir argument, link made by `linkOutside`, sentinel texts) → ignored |
| configurable_utils TestResolveAgentReferenceAllowsPathsInsideAgentDir | port updated (config dir argument) → ignored |
| gcs_offloader TestGCSObjectPath | port updated (hash-suffixed ids, 9 new prefix cases) → ignored |
| gcs_offloader TestPathSegment | port updated (hash suffixes, 128-byte cap, `~`) → ignored |
| json_formatter TestPartsCollector_FileDataObjectRefs | new invalid-UTF-8 URI case left out (adapted); port passes |
| telemetry TestTelemetrySmoke | the new metric-reader checks are 8b (#2042); port unchanged, passes |
| telemetry TestConfigureExporters | the metric-exporter counts and supplied-provider cases are 8b (#2042); port unchanged, passes |

Cosmetic (76; #2282 modernize `wg.Go` / `range n` / `SplitSeq` / `errors.AsType` / `t.Context()` /
`slices.Backward` / `//nolint:errcheck` and error-checking range loops, #2290 `new(expr)`, or no body
change at all — listed only because a diff hunk follows them): auth/gcp/provider_internal_test.go (2),
auth/gcp/provider_test.go (3), auth/providers_internal_test.go (1), auth/store_internal_test.go (1),
auth/transport_test.go (1), cmd/adkgo agentengine_test.go (1), cloudrun_test.go (2),
internal/cli/util/text_helpers_test.go (2), internal/telemetry instrumentation_test.go (4),
logger_test.go (1), telemetry_test.go TestSchemaURL (1), agentanalytics batch_processor_test.go (5),
bigquery_agent_analytics_plugin_test.go (3), events_test.go (1), gcs_offloader_test.go (8: TestGCSOffloading_ManyCallersAndFlushes, TestIsSafeExtension, TestGCSOffloading_PreservesBracketLedProse, TestResolveUploads_DeadlinesPauseTheUploads, TestGCSOffloading_RunEndContinuousArrivalsDoNotPauseHealthyStorage, TestGCSOffloading_CloseLogsUploadFailuresUnderCallerDeadline, TestGCSOffloading_LaterRunEndBatchLogsUploadFailures, TestReleaseRowUploads_ConcurrentCallbackLogsDoNotRace),
json_formatter_test.go (3: TestFormatContentParts, TestContentSummaries, TestNormalizeFunctionResponse_PythonSnakeCaseParity),
trace_manager_test.go (36, incl. TestAgentStartingData's subtest rename by 1edb0572), views_test.go (1).

#### Removed Go tests

- internal/configurable/configurable_utils_test.go TestResolveAgentReferenceRelativeParentPath — removed
  in Go (#1299: references resolve against the config directory); the port
  `test_resolve_agent_reference_relative_parent_path` is removed.
