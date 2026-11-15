# 8a bug report — T6: D2a (runner, agent, plugininternal, internal/{agent,context,adkcontext,utils,typeutil,toolinternal})

Detector run against `plan/8a-b2base` with ADK-Go at `0990f9e2cb9df71c579db6c9731efa403641fc51`.
D2b commits of MAP's `## D2` block (workflow, workflowagents, llmagent, internal/workflow*) are in
T7's fragment. 67fc01b1 is listed here too because a D2a test (`runner/output_schema_resume_test.go`)
is its evidence.

## Fix commits

| commit | gadk test(s) | verdict | cause |
|---|---|---|---|
| 0990f9e2 (#2312) | `runner_run_node::test_runner_function_responses_start_new_invocation` (case `agent_path_calls_of_two_invocations` ignored; 4 cases pass), `test_runner_function_responses_resume_their_invocation` (pass), `runner_hitl_integration::test_runner_workflow_agent_answers_to_two_paused_runs_resume_the_first` (pass) | *real gadk bug* | on the agent path, answers to calls of two invocations resume the first call's invocation instead of starting a new one |
| d642dbfd (#2306) | `runner_run_node::test_runner_plugin_replacement_keeps_event_identity`, `test_runner_plugin_shared_replacement_is_not_modified`, `test_runner_stamps_before_run_replacement`, `runner_live_runner::test_runner_run_live_plugin_replacement_keeps_event_identity` (all ignored) | *real gadk bug* | `from_plugin` keeps the replacement's ID, invocation ID, timestamp and empty author instead of the original event's |
| 10f84869 (#2302) | `runner_runner::test_runner_rejects_user_function_call` (5 cases ignored), `test_runner_user_function_call_checked_after_plugin` (case `plugin_adds_a_call` ignored, other passes), `runner_live_runner::test_runner_run_live_send_rejects_user_function_call` (ignored) | *real gadk bug* | a user message carrying a function call is stored and run (and forwarded by the live session) instead of failing with `user message cannot contain function calls` |
| e1078219 (#2260) | `agent/agent_on_error_test.go` (feature-deferred, R9) | *real gadk bug (feature)* | gadk has no `on_agent_error_callback`, so nothing suppresses cancellation causes in it |
| 7fd9f83f (#1299) | `internal/utils/fs_test.go`, `fs_disk_test.go` (deferred: need a `safe_subpath` helper); configurable tests in T12's fragment | *real gadk bug* | by inspection: gadk-configurable `resolve_node_like` (workflow.rs:211) accepts absolute and `..`-escaping YAML node refs and follows links; there is no shared containment helper |
| dba8afa2 (#2202) | `runner_workflow_function_response::test_run_function_response_reaches_llm_agent_after_workflow` (ignored) | *real gadk bug* | the contents processor fails with `no function call event found` on the user's answer to another agent's call instead of showing it as context |
| 51bc7ec5 (#2188) | `runner_workflow_function_response::test_run_function_response_skips_siblings_of_graph_workflow`, `test_run_function_response_narrows_only_first_loop_pass`, `test_run_function_responses_select_every_caller` (ignored); `test_run_function_response_inside_graph_workflow_resumes_workflow`, `test_run_function_response_escalator_in_loop_terminates` (pass) | *real gadk bug* | sequential, parallel and loop agents re-run every sub-agent on a resume instead of narrowing to the sub-agents holding the callers (no `GraphResumeIndexes` port) |
| dce89da5 (#1808) | `runner_workflow_function_response::test_run_function_response_resumes_workflow_sub_agent` (ignored) | *real gadk bug* | the agent path always runs the root agent; a function response is not routed to the calling sub-agent on its branch |
| ca7db168 (#1899) | `runner_runner::test_runner_auto_create_session_get_error` (cases `transient_error_is_returned_without_create`, `unwrapped_not_found_is_returned_without_create` ignored; 2 pass), `test_runner_auto_create_session_empty_session_id` (pass) | *real gadk bug* | with auto-create on, the runner creates the session on any `get` error, not only `SessionError::NotFound` |
| 2e842548 (#2164) | `agent_loader::test_multi_loader_list_agents_sorted` (ignored) | *real gadk bug* | `MultiLoader::list_agents` returns registration order, not sorted names |
| e83ab5b0 (#2120) | none in D2a (T5/T8 ports cover it) | *real gadk bug* | by inspection: `llm/functions.rs:518` puts `ToolError::Panic`'s text (`panic in tool …`) in the function response; gadk has no `ModelErrorText` / `PanicError` equivalent |
| de7088bf (#1877) | `runner_runner::test_runner_inbound_message_is_user_turn` (cases `empty_role`, `model_role` ignored; `user_role` passes), `test_runner_inbound_role_normalized_for_context_and_plugin` (ignored), `test_runner_save_input_blobs_does_not_mutate_caller_message` (pass) | *real gadk bug* | the inbound (or `on_user_message`) message keeps its role instead of becoming a user turn |
| d0c978b0 (#2124) | `gadk/tests/agent_agent_name::test_constructors_reject_empty_name` (ignored) | *real gadk bug* | `gadk_agent::new_agent` (and so every constructor) accepts an empty name |
| 24d33b35 (#1777) | `runner_subagent_plugins::test_runner_parent_plugins_reach_agent_tool_llm_sub_agent`, `test_runner_parent_plugins_reach_agent_tool_sequential_sub_agent` (ignored); `test_runner_parent_plugins_do_not_own_sub_run_callbacks`, `test_runner_plugin_callback_runner_does_not_inherit_plugins`, `test_runner_separate_runner_does_not_inherit_context_plugins`, `test_runner_judge_inside_agent_tool_sub_run_does_not_inherit_plugins` (pass); `internal/plugininternal/plugin_manager_context_test.go` (feature-deferred, R10) | *real gadk bug (feature)* | AgentTool's sub-runner gets no plugins and inherits none from the parent run (R10) |
| 2649b4ad (#2091) | `runner_run_node::test_runner_stamps_events_yielded_without_timestamp`, `test_runner_stamps_before_run_replacement`, `runner_live_runner::test_runner_run_live_stamps_events_yielded_without_timestamp` (ignored) | *real gadk bug* | an event yielded (or returned by `on_event`) without a timestamp is stored with the zero time |
| dbaca475 (#2105) | `runner_live_runner::test_runner_run_live_early_exit_runs_on_event_and_after_run` (ignored) | *real gadk bug* | `run_live`'s before-run early exit skips `on_event` and `after_run` |
| 67fc01b1 (#2291, D2b) | `runner_output_schema_resume::test_parsed_schema_reply_survives_resume_on_database` (ignored), `test_null_schema_reply_survives_resume` (pass) | *real gadk bug* | `process_llm_agent_output` sets `message_as_output` even when the output schema parsed a record, so the runner clears the parsed output and the resumed join gets the reply text |

## Feature-deferred tests (not written; for the R9/R10 fix task)

- R9 `agent/agent_on_error_test.go`: `TestAgentRunOnAgentErrorCallback`, `TestAgentRunOnAgentErrorCallbackRecoversPanic`, `TestAgentRunOnAgentErrorCallbackRunsBeforeYield`, `TestAgentRunOnAgentErrorCallbackNotifiesEveryPlugin` (#2229, #2260).
- R10 `internal/plugininternal/plugin_manager_context_test.go`: `TestPluginManager_HasPlugins`, `TestFromContext`, `TestTakeInheritedPlugins` (#1777).
- Helper-deferred (not R9–R14; need the `safe_subpath` helper the #1299 fix adds): `internal/utils/fs_test.go` `TestSafeSubpath_Windows`, `TestSafeSubpath_NonWindows`, `TestIsLinkLike`; `internal/utils/fs_disk_test.go` `TestSafeSubpath_OnDisk`, `TestSafeSubpath_FilesystemRoot`, `TestSafeSubpath_RootedLinkTargetWithoutDrive`, `TestSafeSubpath_DotDotAfterLinkInTarget`.

## Appendix: changed Go tests

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
