# 8a bug report — T7: D2b workflow, workflow agents, LLM agent

Group D2b = ADK-Go `workflow/`, `agent/workflowagents/{sequentialagent,parallelagent,loopagent}`,
`agent/workflowagent`, `agent/llmagent`, `internal/workflowinternal`, `internal/workflowroute`,
`internal/workflowwalk` (the last two have no change between `bb1fd51` and `0990f9e`). Every gadk
test named here is in `crates/gadk-engine` (`src/parity_tests/<module>.rs`, or `tests/<target>.rs`
for `agent_workflowagents_parallelagent_agent`). A failing port is written to Go's expectation and
carries `#[ignore = "8a bug: …"]`.

## Fix commits

| commit | gadk test(s) | verdict | cause |
|---|---|---|---|
| 67fc01b1 fix(llmagent): keep a node's parsed structured output on its event (#2291) | `agent_llmagent_llm_agent_wrapper::test_process_llm_agent_output` | *real gadk bug* | `process_llm_agent_output` always sets MessageAsOutput, so the runner drops a parsed output_schema value |
| 6522e5b4 fix(workflow): accept a workflow with no edges (#2286) | `workflow_workflow::test_workflow_with_no_edges_runs_to_completion`, `workflow_validation::test_start_node_present_empty_edges` | *real gadk bug* | `validate_start_node_present` returns NoStartNode on an empty edge set |
| 1d9ae5b9 fix(sequentialagent): return the sequential agent from FindAgent (#2288) | `agent_workflowagents_sequentialagent_agent::test_sequential_agent_find_agent_returns_sequential_agent` | *Go-only (guard kept)* | gadk's default `Agent::find_agent` already returns the sequential agent's own `Arc` |
| 983d689e fix(workflow): call the second use_as_output child a delegate in the error (#2277) | `workflow_run_node::test_run_node_with_use_as_output_second_delegation_returns_error` | *real gadk bug* | the sentinel text is still "parent already has a use_as_output child" (no Go test; the port's exact-text assert is the evidence) |
| bec16021 fix(workflowinternal): reject a null finish_task result (#1741) | `internal_workflowinternal_finish_task_tool::{test_finish_task_tool_run, test_finish_task_tool_declaration_result_matches_validation}`, `agent_llmagent_llm_agent_wrapper::test_task_agent_nullable_result_closes_delegation` | *real gadk bug* | the wrapped finish_task result keeps the user schema's `nullable: true`, so a null result is declared and accepted |
| e2614290 fix(workflow): honor UUID provider for input requests (#1915) | `workflow_hitl::test_scheduler_hitl_node_auto_generates_interrupt_id` (passes), `workflow_hitl::test_scheduler_hitl_node_empty_provider_uuid_still_pauses` | *real gadk bug* | gadk already draws from the provider, but an empty draw stays the interrupt ID and the node never pauses |
| be529e7b fix(workflow): pass a typed node's output schema to the model (#2194) | `workflow_agent_node_output_schema` (7 ignored, 6 pass, 1 skipped), `workflow_model_output_schema` (2 skipped) | *real gadk bug* | AgentNode never derives or binds a model output schema (no `modelOutputSchema` / `WithBoundOutputSchema` counterpart) |
| 51bc7ec5 fix(workflowagents): skip siblings when resuming a nested graph workflow (#2188) | `agent_workflowagents_sequentialagent_agent::test_sequential_agent_run_without_session_or_function_response` (passes; the resume test is `runner/workflow_function_response_test.go`, D2a) | *real gadk bug* (by inspection) | gadk's sequential, loop and parallel agents always run every sub-agent; there is no `GraphResumeIndexes` / `WithGraphResumeDone` counterpart |
| 8c2e2e2a fix(llmagent): write finish_task output to OutputKey for task agents (#1977) | `agent_llmagent_llmagent_saveoutput::test_llm_agent_maybe_save_output_to_state`, 6 `agent_llmagent_llmagent_task_outputkey` tests | *real gadk bug* | a task agent's text turns are saved to output_key, and `run_task` never writes the accepted finish_task output there |
| ab78c5ae fix(llminternal): keep a single_turn sub-agent's transcript out of the root agent's context (#2128) | `agent_llmagent_single_turn_context::test_single_turn_sub_agent_transcript_stays_out_of_coordinator_context`, T1's `agent_llmagent_llmagent_delegation::test_delegation_03_chat_to_single_turn`; guard `test_single_turn_sub_agent_reused_call_id_runs_twice` (passes) | *real gadk bug* | the single_turn sub-branch is keyed by a counter (`translator@1`), not the call id, and its events reach the coordinator's later turns |
| dc44b8c9 fix(workflow): return ctx.Err() instead of a partial ParallelWorker result (#2146) | `workflow_parallel_worker::{test_parallel_worker_cancel_during_dispatch, test_parallel_worker_cancel_after_dispatch, test_parallel_worker_timeout_during_dispatch_in_workflow}`; guard `test_parallel_worker_failure_before_cancel_is_returned` (passes) | *real gadk bug* | `ParallelWorker::run` lacks the "no error but the context is done → ctx.Err()" check |
| 010b91e9 fix(workflow): honor UUID provider for tool node call IDs (#2030) | `workflow_tool_node::test_tool_node_run` (case `function_call_id_from_uuid_provider`) | *Go-only (guard kept)* | gadk's ToolNode already takes the call ID from `ctx.platform().new_uuid()` |
| 82b64f45 fix(sequentialagent): stop RunLive on a sub-agent error, as Run does (#2151) | `agent_workflowagents_sequentialagent_agent::test_sequential_agent_run_live_stops_on_sub_agent_error` | *real gadk bug* | `sequential_live_stream` forwards a sub-agent's error and starts the next sub-agent |
| 6ae94e12 fix(llminternal): keep Partial and UsageMetadata on after-model callback replacements (#1763) | `agent_llmagent_llmagent::test_after_model_callback_replacement_keeps_streaming_fields` | *real gadk bug* | a replacement response is yielded as is, losing partial, turn_complete and usage_metadata |
| 543edd3d fix(parallelagent): keep the parent's isolation scope in each branch (#2054) | `agent_workflowagents_parallelagent_agent::{test_parallel_agent_propagates_isolation_scope, test_parallel_agent_scoped_branch_sees_its_own_history}` | *real gadk bug* | each branch context is a fresh invocation with an empty isolation scope, so a scoped branch's own history is filtered out |
| 31111f89 fix(agenttool): accept the input schema a workflow agent's declaration advertises (#2069) | none in D2b (its test is `tool/agenttool/agent_tool_test.go`, D3) | *real gadk bug* (by inspection) | `AgentTool::run` reads input_schema only from an LLM agent (`tool/agent_tool.rs:186-189`) while the declaration recurses into the first sub-agent (`tool/single_turn.rs:54-66`) |
| dd39c80a fix(sequentialagent): append the live completion instruction to an InstructionProvider (#2079) | `agent_workflowagents_sequentialagent_live_injection::test_sequential_agent_run_live_appends_completion_instruction_to_provider` | *real gadk bug* | the live `task_completed` suffix goes only to Instruction, which an InstructionProvider overrides |

Other fix commits these tests reveal (rows owned by their groups): `agent_llmagent_llmagent::test_agent_transfer`
fails on 1ced38ba (#2281, `transfer_to_agent` result `{"result": null}`) and c8fff670 (#2161, an
unknown `agent_name` is a tool error, not a run failure).

## Appendix: changed, new, removed Go tests

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
