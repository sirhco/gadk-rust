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

//! Go `internal/configurable/conformance/replayplugin/replay_plugin.go`: the conformance replay
//! plugin (`replay_plugin`), which answers model and tool calls from the case directory's
//! `generated-recordings.yaml` (named by the session state's `_adk_replay_config`), verifying
//! each request against the recording. [`normalize`] ports `yaml_utils.go`; `matching` holds
//! the request/tool-call comparison; `state` the per-invocation state.

pub(crate) mod matching;
pub mod normalize;
pub(crate) mod state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use futures::FutureExt;
use gadk_agent::{
    AfterRunCallback, AgentError, BeforeModelCallback, BeforeRunCallback, BeforeToolCallback,
    Context, InvocationContext, Plugin, PluginConfig, Tool,
};
use gadk_engine::tool::AgentTool;
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse};
use gadk_platform::{go_path_join, go_quote};
use gadk_session::State;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use self::matching::{verify_llm_request_match, verify_tool_call_match};
use self::state::{InvocationReplayState, ReplayInner};
use crate::conformance::gonode::from_go_node;
use crate::conformance::recording::{
    LlmRecording, LlmRequestView, RecordedFunctionCall, Recording, ToolRecording,
};
use crate::error::ConfigError;
use crate::govalue::go_map_to_json;
use crate::{check_within_base_dir, read_file};

/// The session state key holding the replay configuration (`dir`, `user_message_index`).
const REPLAY_CONFIG_KEY: &str = "_adk_replay_config";

/// The recordings file read from the case directory.
const RECORDINGS_FILE: &str = "generated-recordings.yaml";

/// Go `replayPlugin`.
pub(crate) struct ReplayPlugin {
    /// Go `mu` + `invocationStates`, keyed by invocation id.
    pub(crate) invocation_states: Mutex<HashMap<String, Arc<InvocationReplayState>>>,
    /// Go `allowedBaseDir`.
    pub(crate) allowed_base_dir: PathBuf,
}

/// Go `New(allowedBaseDir)` (and `MustNew`: Go's `plugin.New` error is always nil): the
/// `replay_plugin` plugin with before run/model/tool and after run callbacks.
pub fn new(allowed_base_dir: impl Into<PathBuf>) -> Plugin {
    // Go: replay_plugin.go New
    let p = Arc::new(ReplayPlugin {
        invocation_states: Mutex::new(HashMap::new()),
        allowed_base_dir: allowed_base_dir.into(),
    });
    let before_run: BeforeRunCallback = {
        let p = Arc::clone(&p);
        Arc::new(move |ctx| {
            let res = p.before_run(&ctx);
            async move { res }.boxed()
        })
    };
    let after_run: AfterRunCallback = {
        let p = Arc::clone(&p);
        Arc::new(move |ctx| {
            p.after_run(&ctx);
            async {}.boxed()
        })
    };
    let before_model: BeforeModelCallback = {
        let p = Arc::clone(&p);
        Arc::new(move |ctx, req| {
            let p = Arc::clone(&p);
            async move {
                let res = p.before_model(&ctx, &req).await;
                (req, res)
            }
            .boxed()
        })
    };
    let before_tool: BeforeToolCallback = {
        let p = Arc::clone(&p);
        Arc::new(move |ctx, tool, args| {
            let p = Arc::clone(&p);
            async move { p.before_tool(&ctx, tool, &args).await }.boxed()
        })
    };
    Plugin::new(PluginConfig {
        name: "replay_plugin".into(),
        before_run_callback: Some(before_run),
        after_run_callback: Some(after_run),
        before_model_callback: Some(before_model),
        before_tool_callback: Some(before_tool),
        ..Default::default()
    })
}

/// A Go error chain as the [`AgentError`] the callbacks return (its `Display` is Go's text).
fn agent_err(err: ConfigError) -> AgentError {
    AgentError::Other(Box::new(err))
}

impl ReplayPlugin {
    /// Go `beforeRun`: loads the invocation's replay state when replay mode is on.
    pub(crate) fn before_run(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Option<Content>, AgentError> {
        // Go: replay_plugin.go beforeRun
        // (Go's `ctx.Session() == nil` check: a gadk invocation always has a session.)
        if !self.is_replay_mode_on(ctx.session().state())? {
            return Ok(None);
        }
        self.load_invocation_state(ctx)?;
        Ok(None)
    }

    /// Go `beforeModel`: the next LLM recording's first response, after verifying the request
    /// (`no LLM responses found in recording for agent %q`).
    pub(crate) async fn before_model(
        &self,
        ctx: &Context,
        req: &LlmRequest,
    ) -> Result<Option<LlmResponse>, AgentError> {
        // Go: replay_plugin.go beforeModel
        if !self.is_replay_mode_on(&*ctx.state())? {
            return Ok(None);
        }
        let invocation_state = self.get_invocation_state(ctx)?;

        let agent_name = ctx.agent_name();
        let recording = self
            .verify_and_get_next_llm_recording_for_agent(invocation_state, &agent_name, req)
            .await?;

        match recording.llm_responses.into_iter().next() {
            Some(first) => Ok(Some(first)),
            None => Err(AgentError::other(format!(
                "no LLM responses found in recording for agent {}",
                go_quote(&agent_name)
            ))),
        }
    }

    /// Go `beforeTool`: the matching tool recording's response; non-agent function tools are
    /// still run (errors printed as `Error calling tool: <err>`).
    pub(crate) async fn before_tool(
        &self,
        ctx: &Context,
        tool: Arc<dyn Tool>,
        args: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        // Go: replay_plugin.go beforeTool
        if !self.is_replay_mode_on(&*ctx.state())? {
            return Ok(None);
        }
        let invocation_state = self.get_invocation_state(ctx)?;

        let agent_name = ctx.agent_name();
        let recording = self.verify_and_get_next_tool_recording_for_agent(
            &invocation_state,
            &agent_name,
            &*tool,
            args,
        )?;
        // Go's `%T` suffix check for `*agenttool.agentTool`.
        if !tool.as_any().is::<AgentTool>() {
            // TODO (Go): support replay requests and responses from AgentTool.
            if let Some(ft) = tool.as_runnable()
                && let Err(err) = ft.run(ctx.clone(), Value::Object(args.clone())).await
            {
                println!("Error calling tool: {err}");
            }
        }

        // Divergence (ledger 6c-2): Go dereferences a nil `ToolResponse` (panic); gadk treats
        // a missing tool response like a nil response map (no override).
        let Some(response) = recording.tool_response.and_then(|r| r.response) else {
            return Ok(None);
        };
        // Divergence (ledger 6c-2): a recorded value JSON cannot hold (see `go_map_to_json`) is
        // an error; Go hands its `map[string]any` back as is.
        go_map_to_json(&response)
            .map(Some)
            .map_err(AgentError::other)
    }

    /// Go `afterRun`: drops the invocation's state.
    pub(crate) fn after_run(&self, ctx: &InvocationContext) {
        // Go: replay_plugin.go afterRun
        match self.is_replay_mode_on(ctx.session().state()) {
            Ok(true) => {}
            _ => return,
        }
        self.invocation_states.lock().remove(ctx.invocation_id());
    }

    /// Go `isReplayModeOn`: `invalid path format: %v`,
    /// `replay config error: 'dir' is not within the allowed base directory`.
    pub(crate) fn is_replay_mode_on(&self, session_state: &dyn State) -> Result<bool, AgentError> {
        // Go: replay_plugin.go isReplayModeOn
        // If the key doesn't exist or there's an error, we treat it as disabled.
        let Ok(config_val) = session_state.get(REPLAY_CONFIG_KEY) else {
            return Ok(false);
        };
        let Value::Object(config) = config_val else {
            return Ok(false);
        };
        let Some(Value::String(case_dir)) = config.get("dir") else {
            return Ok(false);
        };
        if case_dir.is_empty() {
            return Ok(false);
        }

        self.check_within_base_dir(case_dir).map_err(agent_err)?;

        match config.get("user_message_index") {
            None | Some(Value::Null) => Ok(false),
            Some(_) => Ok(true),
        }
    }

    /// Go's base-directory check shared by `isReplayModeOn` and `loadInvocationState`
    /// ([`check_within_base_dir`]). Returns `filepath.Abs(caseDir)`.
    fn check_within_base_dir(&self, case_dir: &str) -> Result<String, ConfigError> {
        check_within_base_dir(&self.allowed_base_dir, case_dir, "replay").map_err(ConfigError::msg)
    }

    /// Go `getInvocationState`: `replay state not initialized. ensure before_run created it`.
    pub(crate) fn get_invocation_state(
        &self,
        ctx: &Context,
    ) -> Result<Arc<InvocationReplayState>, AgentError> {
        // Go: replay_plugin.go getInvocationState
        self.invocation_states
            .lock()
            .get(ctx.invocation_id())
            .cloned()
            .ok_or_else(|| {
                AgentError::other("replay state not initialized. ensure before_run created it")
            })
    }

    /// Go `loadInvocationState`: validates `_adk_replay_config`, reads
    /// `generated-recordings.yaml`, `normalizeYAMLNode`, decodes, assigns per-message LLM
    /// indices (`-1` for tool recordings).
    pub(crate) fn load_invocation_state(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Arc<InvocationReplayState>, AgentError> {
        // Go: replay_plugin.go loadInvocationState
        let invocation_id = ctx.invocation_id().to_owned();

        // 1. Extract Configuration
        let config_val = ctx.session().state().get(REPLAY_CONFIG_KEY).map_err(|e| {
            agent_err(ConfigError::wrap(
                "replay config error",
                ConfigError::other(e),
            ))
        })?;
        let Value::Object(config) = config_val else {
            return Err(AgentError::other(
                "replay config error: '_adk_replay_config' is not a valid map",
            ));
        };

        // 2. Validate Parameters
        let case_dir = match config.get("dir") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => {
                return Err(AgentError::other(
                    "replay config error: 'dir' parameter is missing or empty",
                ));
            }
        };
        let requested_abs_path = self.check_within_base_dir(&case_dir).map_err(agent_err)?;

        // Go accepts an `int` or a `float64` (`int(v)` truncates); both are a JSON number here.
        // Divergence (ledger 6c-2): Go rejects other numeric types (`int64`, `uint`, …), which a
        // JSON state value cannot carry; a float out of `int` range saturates.
        let msg_index = match config.get("user_message_index") {
            Some(Value::Number(n)) => match n.as_i64() {
                Some(i) => i,
                #[allow(clippy::cast_possible_truncation)]
                None => n.as_f64().unwrap_or_default() as i64,
            },
            _ => {
                return Err(AgentError::other(
                    "replay config error: 'user_message_index' is missing or not a number",
                ));
            }
        };

        // 3. Load Recordings File
        let recordings_path = go_path_join(&[requested_abs_path.as_str(), RECORDINGS_FILE]);

        // Check if file exists (Go `os.IsNotExist` of `os.Stat`).
        if let Err(e) = std::fs::metadata(&recordings_path)
            && e.kind() == std::io::ErrorKind::NotFound
        {
            return Err(AgentError::other(format!(
                "replay config error: recordings file not found: {recordings_path}"
            )));
        }

        let data = read_file(Path::new(&recordings_path))
            .map_err(|e| agent_err(ConfigError::wrap("failed to read recordings file", e)))?;

        // Parse YAML
        let mut root = gadk_goyaml::unmarshal_node(&data).map_err(|e| {
            agent_err(ConfigError::wrap(
                format!("failed to parse recordings from {recordings_path}"),
                e,
            ))
        })?;
        let at = root.root();
        normalize::normalize_yaml_node(&mut root, at);

        let mut recordings = from_go_node(&root, at)
            .map_err(|e| agent_err(ConfigError::wrap("failed to decode recordings", e)))?;

        // Add index to each recording, based on user message index. Used for parallel
        // execution sync.
        let mut index = 0;
        let mut prev_message_id = 0;
        for rec in &mut recordings.recordings {
            if prev_message_id != rec.user_message_index {
                prev_message_id = rec.user_message_index;
                index = 0;
            }
            if rec.llm_recording.is_some() {
                rec.index = index;
                index += 1;
            } else {
                rec.index = -1; // Not used for sync
            }
        }

        // 4. Create and Store State
        let state = Arc::new(InvocationReplayState::new(case_dir, msg_index, recordings));
        self.invocation_states
            .lock()
            .insert(invocation_id, Arc::clone(&state));
        Ok(state)
    }

    /// Go `verifyAndGetNextLLMRecordingForAgent`: `expected LLM recording for agent '%s' at
    /// index %d, but found tool recording`, then `verifyLLMRequestMatch`.
    pub(crate) async fn verify_and_get_next_llm_recording_for_agent(
        &self,
        state: Arc<InvocationReplayState>,
        agent_name: &str,
        llm_request: &LlmRequest,
    ) -> Result<LlmRecording, AgentError> {
        // Go: replay_plugin.go verifyAndGetNextLLMRecordingForAgent
        let (current_agent_index, _) = state.get_agent_replay_index(agent_name);
        let expected_recording = get_next_recording_for_agent(state, agent_name).await?;

        let Some(llm_recording) = expected_recording.llm_recording else {
            return Err(AgentError::other(format!(
                "expected LLM recording for agent '{agent_name}' at index {current_agent_index}, but found tool recording"
            )));
        };

        // Strict verification of LLM request.
        // Divergence (ledger 6c-2): Go dereferences a nil recorded `llm_request` (panic); gadk
        // compares against an empty request.
        let fallback = LlmRequestView::default();
        let expected = llm_recording.llm_request.as_ref().unwrap_or(&fallback);
        verify_llm_request_match(expected, llm_request, agent_name, current_agent_index)
            .map_err(AgentError::other)?;

        Ok(llm_recording)
    }

    /// Go `verifyAndGetNextToolRecordingForAgent`.
    pub(crate) fn verify_and_get_next_tool_recording_for_agent(
        &self,
        state: &InvocationReplayState,
        agent_name: &str,
        tool: &dyn Tool,
        args: &Map<String, Value>,
    ) -> Result<ToolRecording, AgentError> {
        // Go: replay_plugin.go verifyAndGetNextToolRecordingForAgent
        // Divergence (ledger 6c-2): Go dereferences a nil recorded `tool_call` (panic); gadk
        // compares against an empty call (a tool name mismatch).
        let fallback = RecordedFunctionCall::default();
        let mut match_fn = |rec: &Recording, inner: &ReplayInner| -> Result<bool, String> {
            let Some(tool_recording) = &rec.tool_recording else {
                return Err(format!(
                    "expected tool recording for agent '{agent_name}', but found LLM recording"
                ));
            };
            let agent_index = inner
                .agent_replay_indices
                .get(agent_name)
                .copied()
                .unwrap_or_default();
            verify_tool_call_match(
                tool_recording.tool_call.as_ref().unwrap_or(&fallback),
                tool.name(),
                args,
                agent_name,
                agent_index,
            )
            .map(|()| true)
        };

        let expected_recording =
            get_next_tool_recording_for_agent(state, agent_name, &mut match_fn)?;

        *state
            .lock()
            .agent_replay_indices
            .entry(agent_name.to_owned())
            .or_default() += 1;

        Ok(expected_recording.tool_recording.unwrap_or_default())
    }
}

/// Go `getNextRecordingForAgent`: the agent's next recording in order (`runtime sent more
/// requests than expected for agent '%s' at user_message_index %d. Expected %d, but got request
/// at index %d`), waiting on the condition variable until `curIndex` reaches its index, then
/// sleeping `index × 10ms`.
pub(crate) async fn get_next_recording_for_agent(
    state: Arc<InvocationReplayState>,
    agent_name: &str,
) -> Result<Recording, AgentError> {
    // Go: replay_plugin.go getNextRecordingForAgent
    // Get current agent index
    let (current_agent_index, _) = state.get_agent_replay_index(agent_name);

    // Filter ALL recordings for this agent and user message index (strict order)
    let agent_recordings: Vec<&Recording> = state
        .recordings
        .recordings
        .iter()
        .filter(|r| r.agent_name == agent_name && r.user_message_index == state.user_message_index)
        .collect();

    // Check if we have enough recordings for this agent
    let expected_recording = match usize::try_from(current_agent_index)
        .ok()
        .and_then(|i| agent_recordings.get(i))
    {
        Some(rec) => (*rec).clone(),
        None => {
            return Err(AgentError::other(format!(
                "runtime sent more requests than expected for agent '{agent_name}' at user_message_index {}. Expected {}, but got request at index {current_agent_index}",
                state.user_message_index,
                agent_recordings.len()
            )));
        }
    };

    // Wait for the current index to match the expected index. This ensures that we process
    // recordings in the expected order, even if agents are executing in parallel. The wait (and
    // Go's sleep under the lock) runs on a blocking thread, never on a runtime worker.
    let index = expected_recording.index;
    let agent = agent_name.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut inner = state.lock();
        while inner.cur_index != index {
            inner = state
                .cond
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
        // FIXME (Go): remove this sleep, move curIndex++ and state cond.Broadcast() to onEvent
        // callback. This sleep is here to make the replay deterministic, but it's not ideal.
        std::thread::sleep(
            Duration::from_millis(10).saturating_mul(u32::try_from(index).unwrap_or(u32::MAX)),
        );

        *inner.agent_replay_indices.entry(agent).or_default() += 1;
        inner.cur_index += 1;

        drop(inner);
        state.cond.notify_all();
    })
    .await
    .map_err(|e| AgentError::other(e.to_string()))?;

    Ok(expected_recording)
}

/// Go `getNextToolRecordingForAgent`: the first unconsumed recording of the agent and user
/// message that `match_fn` accepts (marked consumed), else the first match error, else
/// `no matching tool recording found for agent '%s' at user_message_index %d`. `match_fn` runs
/// under the state's lock (Go reads `agentReplayIndices` there) and receives the guarded fields.
pub(crate) fn get_next_tool_recording_for_agent(
    state: &InvocationReplayState,
    agent_name: &str,
    match_fn: &mut dyn FnMut(&Recording, &ReplayInner) -> Result<bool, String>,
) -> Result<Recording, AgentError> {
    // Go: replay_plugin.go getNextToolRecordingForAgent
    let mut inner = state.lock();

    let mut first_error: Option<String> = None;

    for (i, rec) in state.recordings.recordings.iter().enumerate() {
        if rec.user_message_index != state.user_message_index || rec.agent_name != agent_name {
            continue;
        }
        if inner.consumed_recordings.get(&i).copied().unwrap_or(false) {
            continue;
        }

        let (matched, err) = match match_fn(rec, &inner) {
            Ok(m) => (m, None),
            Err(e) => (false, Some(e)),
        };
        if matched {
            inner.consumed_recordings.insert(i, true);
            return Ok(rec.clone());
        }
        if first_error.is_none() && err.is_some() {
            first_error = err;
        }
    }

    if let Some(err) = first_error {
        return Err(AgentError::other(err));
    }

    Err(AgentError::other(format!(
        "no matching tool recording found for agent '{agent_name}' at user_message_index {}",
        state.user_message_index
    )))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gadk_agent::{AgentConfig, RunnableTool, ToolError, new_agent};
    use gadk_genai::Part;
    use serde_json::json;

    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/recordings");

    fn plugin(base: &Path) -> ReplayPlugin {
        ReplayPlugin {
            invocation_states: Mutex::new(HashMap::new()),
            allowed_base_dir: base.to_path_buf(),
        }
    }

    /// An invocation (id `inv`) whose session state holds `_adk_replay_config` = `config`.
    fn invocation(config: Value) -> InvocationContext {
        let inv = InvocationContext::builder()
            .invocation_id("inv")
            .build_invocation();
        inv.session()
            .state()
            .set(REPLAY_CONFIG_KEY, config)
            .unwrap();
        inv
    }

    /// A callback context of agent `agent` on `inv`'s session and invocation.
    fn agent_ctx(inv: &InvocationContext, agent: &str) -> Context {
        Context::builder()
            .session(inv.session())
            .invocation_id(inv.invocation_id())
            .agent(
                new_agent(AgentConfig {
                    name: agent.into(),
                    ..Default::default()
                })
                .unwrap(),
            )
            .build()
    }

    fn write_recordings(dir: &Path, yaml: &str) {
        std::fs::write(dir.join(RECORDINGS_FILE), yaml).unwrap();
    }

    fn request(model: &str, text: &str) -> LlmRequest {
        LlmRequest {
            model: model.into(),
            contents: vec![Content {
                role: "user".into(),
                parts: vec![Part::from_text(text)],
            }],
            ..Default::default()
        }
    }

    fn response_text(resp: &LlmResponse) -> String {
        resp.content.as_ref().unwrap().parts[0].text.clone()
    }

    /// One LLM recording of `agent` for user message `msg`: request `{m, user: text}`, response
    /// `model: text-resp`.
    fn llm_yaml(msg: i64, agent: &str, text: &str) -> String {
        format!(
            "  - user_message_index: {msg}\n    agent_name: {agent}\n    llm_recording:\n      llm_request:\n        model: m\n        contents:\n          - role: user\n            parts:\n              - text: {text}\n      llm_responses:\n        - content:\n            role: model\n            parts:\n              - text: {text}-resp\n"
        )
    }

    fn tool_yaml(msg: i64, agent: &str, name: &str, arg: &str, resp: &str) -> String {
        format!(
            "  - user_message_index: {msg}\n    agent_name: {agent}\n    tool_recording:\n      tool_call:\n        name: {name}\n        args:\n          p: \"{arg}\"\n      tool_response:\n        name: {name}\n        response:\n          r: \"{resp}\"\n"
        )
    }

    struct TestTool {
        name: String,
        runs: AtomicUsize,
        runnable: bool,
    }

    impl TestTool {
        fn new(name: &str, runnable: bool) -> Arc<TestTool> {
            Arc::new(TestTool {
                name: name.into(),
                runs: AtomicUsize::new(0),
                runnable,
            })
        }
    }

    impl Tool for TestTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "test tool"
        }
        fn as_runnable(&self) -> Option<&dyn RunnableTool> {
            if self.runnable { Some(self) } else { None }
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    impl RunnableTool for TestTool {
        fn run<'a, 'b>(
            &'a self,
            _ctx: Context,
            _args: Value,
        ) -> futures::future::BoxFuture<'b, Result<Map<String, Value>, ToolError>>
        where
            'a: 'b,
            Self: 'b,
        {
            self.runs.fetch_add(1, Ordering::SeqCst);
            async { Err(ToolError::InvalidArgument("boom".into())) }.boxed()
        }
    }

    fn args(p: &str) -> Map<String, Value> {
        json!({ "p": p }).as_object().unwrap().clone()
    }

    #[test]
    fn is_replay_mode_on_follows_go() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_string_lossy().into_owned();
        let p = plugin(dir.path());
        let on = |config: Option<Value>| {
            let inv = InvocationContext::builder().build_invocation();
            if let Some(c) = config {
                inv.session().state().set(REPLAY_CONFIG_KEY, c).unwrap();
            }
            p.is_replay_mode_on(inv.session().state())
                .map_err(|e| e.to_string())
        };
        assert_eq!(on(None), Ok(false));
        assert_eq!(on(Some(json!("x"))), Ok(false));
        assert_eq!(on(Some(json!({"user_message_index": 0}))), Ok(false));
        assert_eq!(
            on(Some(json!({"dir": "", "user_message_index": 0}))),
            Ok(false)
        );
        assert_eq!(
            on(Some(json!({"dir": 3, "user_message_index": 0}))),
            Ok(false)
        );
        assert_eq!(on(Some(json!({"dir": base}))), Ok(false));
        assert_eq!(
            on(Some(json!({"dir": base, "user_message_index": null}))),
            Ok(false)
        );
        assert_eq!(
            on(Some(json!({"dir": base, "user_message_index": "x"}))),
            Ok(true)
        );
        assert_eq!(
            on(Some(
                json!({"dir": format!("{base}/case"), "user_message_index": 0})
            )),
            Ok(true)
        );
        // The base-dir check runs before the `user_message_index` presence check.
        assert_eq!(
            on(Some(json!({"dir": "/etc"}))),
            Err("replay config error: 'dir' is not within the allowed base directory".into())
        );
    }

    #[test]
    fn load_invocation_state_go_errors() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_string_lossy().into_owned();
        let p = plugin(dir.path());
        let load = |config: Value| {
            p.load_invocation_state(&invocation(config))
                .map(|_| ())
                .map_err(|e| e.to_string())
        };
        assert_eq!(
            load(json!(1)),
            Err("replay config error: '_adk_replay_config' is not a valid map".into())
        );
        assert_eq!(
            load(json!({"dir": ""})),
            Err("replay config error: 'dir' parameter is missing or empty".into())
        );
        assert_eq!(
            load(json!({"dir": "/etc", "user_message_index": 0})),
            Err("replay config error: 'dir' is not within the allowed base directory".into())
        );
        assert_eq!(
            load(json!({"dir": base, "user_message_index": "0"})),
            Err("replay config error: 'user_message_index' is missing or not a number".into())
        );
        assert_eq!(
            load(json!({"dir": base, "user_message_index": 0})),
            Err(format!(
                "replay config error: recordings file not found: {base}/generated-recordings.yaml"
            ))
        );
        std::fs::create_dir(dir.path().join(RECORDINGS_FILE)).unwrap();
        assert_eq!(
            load(json!({"dir": base, "user_message_index": 0})),
            Err(format!(
                "failed to read recordings file: read {base}/generated-recordings.yaml: is a directory"
            ))
        );
        std::fs::remove_dir(dir.path().join(RECORDINGS_FILE)).unwrap();
        write_recordings(dir.path(), "recordings: [\n");
        let err = load(json!({"dir": base, "user_message_index": 0})).unwrap_err();
        assert!(
            err.starts_with(&format!(
                "failed to parse recordings from {base}/generated-recordings.yaml: yaml: "
            )),
            "{err}"
        );
        write_recordings(dir.path(), "recordings:\n  - user_message_index: x\n");
        assert_eq!(
            load(json!({"dir": base, "user_message_index": 0})),
            Err("failed to decode recordings: yaml: unmarshal errors:\n  line 2: cannot unmarshal !!str `x` into int64".into())
        );
    }

    #[test]
    fn load_invocation_state_wraps_go_date_abort() {
        // The Go probe's `citation-dates` case: Go's decode fails with civil.ParseDate's text.
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(
            format!("{FIXTURES}/citation-dates/{RECORDINGS_FILE}"),
            dir.path().join(RECORDINGS_FILE),
        )
        .unwrap();
        let p = plugin(dir.path());
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        let err = p.load_invocation_state(&inv).map(|_| ()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to decode recordings: parsing time \"0000-00-00\": month out of range"
        );
    }

    #[test]
    fn load_invocation_state_assigns_per_message_llm_indices() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = [
            "recordings:\n".to_owned(),
            llm_yaml(0, "a", "x"),
            tool_yaml(0, "a", "t", "1", "2"),
            llm_yaml(0, "b", "y"),
            llm_yaml(1, "a", "z"),
            tool_yaml(1, "a", "t", "1", "2"),
            llm_yaml(1, "a", "w"),
            llm_yaml(0, "a", "v"),
        ]
        .concat();
        write_recordings(dir.path(), &yaml);
        let p = plugin(dir.path());
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 1.9}));
        let state = p.load_invocation_state(&inv).unwrap();
        let indices: Vec<i64> = state
            .recordings
            .recordings
            .iter()
            .map(|r| r.index)
            .collect();
        assert_eq!(indices, [0, -1, 1, 0, -1, 1, 0]);
        // Go's `int(v)` of a float64 index truncates.
        assert_eq!(state.user_message_index, 1);
        assert_eq!(state.test_case_path, dir.path().to_string_lossy());
        assert!(Arc::ptr_eq(&state, &p.invocation_states.lock()["inv"]));
    }

    #[test]
    fn before_run_and_after_run_manage_the_state() {
        let dir = tempfile::tempdir().unwrap();
        write_recordings(dir.path(), "recordings: []");
        let p = plugin(dir.path());
        // Replay mode off: nothing is loaded, nothing to remove.
        let off = InvocationContext::builder()
            .invocation_id("inv")
            .build_invocation();
        assert!(p.before_run(&off).unwrap().is_none());
        assert!(p.invocation_states.lock().is_empty());

        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        assert!(p.before_run(&inv).unwrap().is_none());
        assert!(p.invocation_states.lock().contains_key("inv"));
        p.after_run(&off);
        assert!(p.invocation_states.lock().contains_key("inv"));
        p.after_run(&inv);
        assert!(p.invocation_states.lock().is_empty());
    }

    #[tokio::test]
    async fn get_invocation_state_requires_before_run() {
        let dir = tempfile::tempdir().unwrap();
        let p = plugin(dir.path());
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        let err = p
            .before_model(&agent_ctx(&inv, "a"), &request("m", "x"))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "replay state not initialized. ensure before_run created it"
        );
    }

    #[tokio::test]
    async fn before_model_errors_follow_go() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = format!(
            "recordings:\n{}  - user_message_index: 0\n    agent_name: a\n    llm_recording:\n      llm_request:\n        model: m\n        contents:\n          - role: user\n            parts:\n              - text: y\n",
            llm_yaml(0, "a", "x")
        );
        write_recordings(dir.path(), &yaml);
        let p = plugin(dir.path());
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        p.before_run(&inv).unwrap();
        let ctx = agent_ctx(&inv, "a");

        let first = p
            .before_model(&ctx, &request("m", "x"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response_text(&first), "x-resp");
        let err = p.before_model(&ctx, &request("m", "y")).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "no LLM responses found in recording for agent \"a\""
        );
        let err = p.before_model(&ctx, &request("m", "z")).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "runtime sent more requests than expected for agent 'a' at user_message_index 0. Expected 2, but got request at index 2"
        );
        let err = p
            .before_model(&agent_ctx(&inv, "other"), &request("m", "x"))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "runtime sent more requests than expected for agent 'other' at user_message_index 0. Expected 0, but got request at index 0"
        );
    }

    #[tokio::test]
    async fn before_model_mismatch_uses_go_prefix() {
        let dir = tempfile::tempdir().unwrap();
        write_recordings(
            dir.path(),
            &format!("recordings:\n{}", llm_yaml(0, "a", "x")),
        );
        let p = plugin(dir.path());
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        p.before_run(&inv).unwrap();
        let err = p
            .before_model(&agent_ctx(&inv, "a"), &request("m2", "x"))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("LLM request mismatch for agent 'a' (index 0):\n"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn before_tool_takes_first_unconsumed_match_and_runs_function_tools() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = [
            "recordings:\n".to_owned(),
            tool_yaml(0, "a", "t", "1", "one"),
            tool_yaml(0, "a", "t", "2", "two"),
            tool_yaml(0, "b", "t", "1", "other-agent"),
            tool_yaml(1, "a", "t", "1", "other-message"),
        ]
        .concat();
        write_recordings(dir.path(), &yaml);
        let p = plugin(dir.path());
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        p.before_run(&inv).unwrap();
        let ctx = agent_ctx(&inv, "a");
        let tool = TestTool::new("t", true);

        // Out of order: the second recording matches; the first stays unconsumed.
        let got = p.before_tool(&ctx, tool.clone(), &args("2")).await.unwrap();
        assert_eq!(got, Some(json!({"r": "two"}).as_object().unwrap().clone()));
        // The function tool still ran (its error is only printed).
        assert_eq!(tool.runs.load(Ordering::SeqCst), 1);
        let got = p.before_tool(&ctx, tool.clone(), &args("1")).await.unwrap();
        assert_eq!(got, Some(json!({"r": "one"}).as_object().unwrap().clone()));
        let state = p.get_invocation_state(&ctx).unwrap();
        assert_eq!(state.get_agent_replay_index("a"), (2, true));

        // Everything of agent `a` / message 0 consumed.
        let err = p
            .before_tool(&ctx, tool.clone(), &args("1"))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "no matching tool recording found for agent 'a' at user_message_index 0"
        );
        assert_eq!(tool.runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn before_tool_returns_the_first_match_error() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = [
            "recordings:\n".to_owned(),
            llm_yaml(0, "a", "x"),
            tool_yaml(0, "a", "t", "1", "one"),
            "  - user_message_index: 0\n    agent_name: a\n    tool_recording:\n      tool_call:\n        name: t\n        args:\n          p: \"3\"\n".to_owned(),
        ]
        .concat();
        write_recordings(dir.path(), &yaml);
        let p = plugin(dir.path());
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        p.before_run(&inv).unwrap();
        let ctx = agent_ctx(&inv, "a");
        let tool = TestTool::new("t", false);

        let err = p
            .before_tool(&ctx, tool.clone(), &args("9"))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected tool recording for agent 'a', but found LLM recording"
        );
        // A recording without a response map: Go's nil map, no override.
        assert_eq!(
            p.before_tool(&ctx, tool.clone(), &args("3")).await.unwrap(),
            None
        );
        let err = p
            .before_tool(&ctx, TestTool::new("u", false), &args("1"))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected tool recording for agent 'a', but found LLM recording"
        );
        // Not a replay invocation: no override, nothing run.
        let off = InvocationContext::builder().build_invocation();
        assert_eq!(
            p.before_tool(&agent_ctx(&off, "a"), tool.clone(), &args("1"))
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn before_tool_does_not_run_agent_tools() {
        let dir = tempfile::tempdir().unwrap();
        write_recordings(
            dir.path(),
            &format!("recordings:\n{}", tool_yaml(0, "a", "helper", "1", "done")),
        );
        let p = plugin(dir.path());
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        p.before_run(&inv).unwrap();
        let helper = new_agent(AgentConfig {
            name: "helper".into(),
            ..Default::default()
        })
        .unwrap();
        let tool = AgentTool::new(helper, None);
        // Running the helper agent (no run function) would fail; replay never runs it.
        let got = p
            .before_tool(&agent_ctx(&inv, "a"), tool, &args("1"))
            .await
            .unwrap();
        assert_eq!(got, Some(json!({"r": "done"}).as_object().unwrap().clone()));
    }

    /// A Go-generated recording golden replays through the plugin: every recording of the user
    /// message, in order, answers the request it recorded.
    #[tokio::test]
    async fn go_fixture_replays_in_order() {
        let base = PathBuf::from(FIXTURES);
        let case = base.join("mixed-sequence");
        let p = plugin(&base);
        let inv = invocation(json!({"dir": case, "user_message_index": 0}));
        p.before_run(&inv).unwrap();
        let state = p.get_invocation_state(&agent_ctx(&inv, "x")).unwrap();
        let recordings = state.recordings.recordings.clone();
        assert_eq!(recordings.len(), 4);
        let mut replayed = 0;
        for rec in &recordings {
            let ctx = agent_ctx(&inv, &rec.agent_name);
            if let Some(llm) = &rec.llm_recording {
                let view = llm.llm_request.clone().unwrap();
                let req = LlmRequest {
                    model: view.model,
                    contents: view.contents,
                    config: view.config,
                    ..Default::default()
                };
                let got = p.before_model(&ctx, &req).await.unwrap();
                assert_eq!(got.as_ref(), llm.llm_responses.first());
            } else {
                let tr = rec.tool_recording.as_ref().unwrap();
                let call = tr.tool_call.as_ref().unwrap();
                let live = go_map_to_json(call.args.as_ref().unwrap()).unwrap();
                let got = p
                    .before_tool(&ctx, TestTool::new(&call.name, false), &live)
                    .await
                    .unwrap();
                let want = go_map_to_json(
                    tr.tool_response
                        .as_ref()
                        .unwrap()
                        .response
                        .as_ref()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(got, Some(want));
            }
            replayed += 1;
        }
        assert_eq!(replayed, 4);
        let inner = state.lock();
        assert_eq!(inner.cur_index, 3);
        assert_eq!(inner.agent_replay_indices["root_agent"], 3);
        assert_eq!(inner.agent_replay_indices["helper_agent"], 1);
    }

    /// Two agents whose LLM recordings interleave (a parallel agent's sub-agents): each agent's
    /// call waits for the other's earlier recording, on a current-thread runtime, without
    /// deadlock, and each agent gets its own responses.
    #[tokio::test]
    async fn interleaved_agents_replay_without_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = [
            "recordings:\n".to_owned(),
            llm_yaml(0, "a", "a1"),
            llm_yaml(0, "b", "b1"),
            llm_yaml(0, "a", "a2"),
            llm_yaml(0, "b", "b2"),
        ]
        .concat();
        write_recordings(dir.path(), &yaml);
        let p = Arc::new(plugin(dir.path()));
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        p.before_run(&inv).unwrap();
        let run = |agent: &'static str| {
            let p = Arc::clone(&p);
            let ctx = agent_ctx(&inv, agent);
            tokio::spawn(async move {
                let mut got = Vec::new();
                for i in 1..=2 {
                    let req = request("m", &format!("{agent}{i}"));
                    let resp = p.before_model(&ctx, &req).await.unwrap().unwrap();
                    got.push(response_text(&resp));
                }
                got
            })
        };
        // `b` asks first: its first recording (index 1) waits for `a`'s (index 0), and the
        // wait does not block the runtime's only worker thread.
        let b = run("b");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!b.is_finished());
        assert_eq!(
            p.get_invocation_state(&agent_ctx(&inv, "b"))
                .unwrap()
                .lock()
                .cur_index,
            0
        );
        let a = run("a");
        let (a, b) = tokio::time::timeout(Duration::from_secs(10), async {
            (a.await.unwrap(), b.await.unwrap())
        })
        .await
        .expect("replay deadlocked");
        assert_eq!(a, ["a1-resp", "a2-resp"]);
        assert_eq!(b, ["b1-resp", "b2-resp"]);
        let state = p.get_invocation_state(&agent_ctx(&inv, "a")).unwrap();
        assert_eq!(state.lock().cur_index, 4);
    }

    #[tokio::test]
    async fn plugin_callbacks_are_wired() {
        let dir = tempfile::tempdir().unwrap();
        write_recordings(
            dir.path(),
            &[
                "recordings:\n".to_owned(),
                llm_yaml(0, "test_agent", "x"),
                tool_yaml(0, "test_agent", "t", "1", "one"),
            ]
            .concat(),
        );
        let plugin = new(dir.path());
        assert_eq!(plugin.name(), "replay_plugin");
        assert!(plugin.after_model_callback().is_none());
        assert!(plugin.after_tool_callback().is_none());
        let inv = invocation(json!({"dir": dir.path(), "user_message_index": 0}));
        assert!(
            (plugin.before_run_callback().unwrap())(inv.clone())
                .await
                .unwrap()
                .is_none()
        );
        let ctx = Context::builder()
            .session(inv.session())
            .invocation_id("inv")
            .build();
        let (req, resp) =
            (plugin.before_model_callback().unwrap())(ctx.clone(), request("m", "x")).await;
        assert_eq!(
            (req.model, req.contents),
            (request("m", "x").model, request("m", "x").contents)
        );
        assert_eq!(response_text(&resp.unwrap().unwrap()), "x-resp");
        let got = (plugin.before_tool_callback().unwrap())(
            ctx.clone(),
            TestTool::new("t", false),
            args("1"),
        )
        .await
        .unwrap();
        assert_eq!(got, Some(json!({"r": "one"}).as_object().unwrap().clone()));
        (plugin.after_run_callback().unwrap())(inv.clone()).await;
        let err =
            (plugin.before_tool_callback().unwrap())(ctx, TestTool::new("t", false), args("1"))
                .await
                .unwrap_err();
        assert_eq!(
            err.to_string(),
            "replay state not initialized. ensure before_run created it"
        );
    }
}
