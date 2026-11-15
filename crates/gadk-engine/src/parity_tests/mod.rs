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

//! Ports of ADK-Go tests that need crate-internal access, one module per Go test file (plan
//! Appendix A): Go internal-package tests, plus the external-package tests that Appendix A marks
//! `external→internal` (plan decision D2).
//! Go packages: `agent/...`, `internal/...`, `runner`, `tool/...`, `workflow`.
//! Each module's owning task fills it.

mod agent_llmagent_llm_agent_wrapper;
mod agent_llmagent_llmagent;
mod agent_llmagent_llmagent_clone;
mod agent_llmagent_llmagent_compaction;
mod agent_llmagent_llmagent_saveoutput;
mod agent_llmagent_llmagent_tail_retention;
mod agent_llmagent_state_agent;
mod agent_workflowagent_hitl;
mod agent_workflowagent_reentry;
mod agent_workflowagent_workflow;
mod agent_workflowagents_sequentialagent_agent;
mod agent_workflowagents_sequentialagent_live_injection;
mod internal_agent_compactionctx_compactionctx;
mod internal_agent_parentmap_map;
mod internal_artifact_artifacts;
mod internal_artifact_forwarding;
mod internal_compactioninternal_apply;
mod internal_compactioninternal_compactor;
mod internal_compactioninternal_summary_event;
mod internal_compactioninternal_tail_retention;
mod internal_compactioninternal_telemetry;
mod internal_compactioninternal_window;
mod internal_compactionvalidate_validate;
mod internal_llminternal_agent_transfer;
mod internal_llminternal_audio_cache_manager;
mod internal_llminternal_base_flow;
mod internal_llminternal_base_flow_live;
mod internal_llminternal_base_flow_live_config;
mod internal_llminternal_base_flow_resumable;
mod internal_llminternal_base_flow_telemetry;
mod internal_llminternal_basic_processor;
mod internal_llminternal_clone;
mod internal_llminternal_compaction_concurrent;
mod internal_llminternal_compaction_mode;
mod internal_llminternal_compaction_processor;
mod internal_llminternal_contents_processor;
mod internal_llminternal_contents_processor_compaction;
mod internal_llminternal_contents_processor_internal;
mod internal_llminternal_foreign_function_response;
mod internal_llminternal_foreign_payloads;
mod internal_llminternal_foreign_thoughts;
mod internal_llminternal_functions;
mod internal_llminternal_identity;
mod internal_llminternal_identity_request_processor;
mod internal_llminternal_instruction_processor;
mod internal_llminternal_live_injection;
mod internal_llminternal_live_reconnect;
mod internal_llminternal_mode;
mod internal_llminternal_mode_contents;
mod internal_llminternal_outputschema_processor;
mod internal_llminternal_request_confirmation_processor;
mod internal_llminternal_streaming_tool;
mod internal_llminternal_tool_sub_branch;
mod internal_llminternal_tools_processor;
mod internal_memory_memory;
mod internal_telemetry_converters;
mod internal_telemetry_experimental_semconv;
mod internal_telemetry_genaimessages;
mod internal_telemetry_inference;
mod internal_telemetry_instrumentation;
mod internal_telemetry_logger;
mod internal_telemetry_telemetry;
mod internal_telemetry_telemetrytest_span_digest;
mod internal_toolinternal_tool;
mod internal_typeutil_convert;
mod internal_utils_schema;
mod internal_utils_schema_canonical;
mod internal_utils_utils;
mod internal_workflowinternal_finish_task_tool;
mod internal_workflowinternal_single_turn_tool;
mod internal_workflowinternal_task_agent_tool;
mod internal_workflowroute_workflowroute;
mod plugin_retryandreflect_plugin;
mod runner_agent_node;
mod runner_before_run;
mod runner_compaction;
mod runner_find_active_task_isolation_scope;
mod runner_live_runner;
mod runner_max_llm_calls;
mod runner_mode_no_mutation;
mod runner_runner;
mod session_database_compaction;
mod tool_agenttool_agent_tool;
mod tool_agenttool_agent_tool_artifacts;
mod tool_agenttool_agent_tool_limits;
mod tool_agenttool_workflow_retry;
mod tool_functiontool_function;
mod tool_functiontool_long_running_function;
mod tool_functiontool_streaming_function;
mod tool_tool;
mod workflow_agent_node;
mod workflow_agent_node_mode_mutation;
mod workflow_agent_node_mode_placement;
mod workflow_agent_node_modes;
mod workflow_base_node;
mod workflow_branch;
mod workflow_branch_isolation;
mod workflow_config;
mod workflow_conformance_parity;
mod workflow_dynamic_node;
mod workflow_dynamic_scheduler;
mod workflow_edgebuilder;
mod workflow_error_identity;
mod workflow_errors;
mod workflow_failure_inventory;
mod workflow_function_node;
mod workflow_graph;
mod workflow_hitl;
mod workflow_join_node;
mod workflow_max_concurrency;
mod workflow_parallel_worker;
mod workflow_parallel_worker_branch;
mod workflow_persistence;
mod workflow_retry;
mod workflow_retry_loop;
mod workflow_review_cache_regressions;
mod workflow_review_cleanup_retry;
mod workflow_run_node;
mod workflow_run_node_raise_on_wait;
mod workflow_scheduler;
mod workflow_scheduler_branch;
mod workflow_state;
mod workflow_synthesize_agent_output;
mod workflow_tool_node;
mod workflow_validation;
mod workflow_walk;
mod workflow_workflow;
mod workflow_workflow_node;
