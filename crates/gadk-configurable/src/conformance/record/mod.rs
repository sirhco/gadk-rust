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

//! The conformance record plugin (`record_plugin`), which writes each invocation's LLM and
//! tool interactions to `generated-recordings.yaml` (`generated-recordings-sse.yaml` for `sse`
//! streaming) in the
//! case directory named by the session state's `_adk_recordings_config`. The YAML
//! post-processing lives in [`clean`], the per-invocation state in `state`.
//!
//! The plugin callbacks are synchronous in effect: here each is a future that calls the
//! synchronous method of the same name, so neither the plugin's state map lock nor an
//! invocation state's lock is ever held across an `.await`.

pub mod clean;
pub(crate) mod state;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::FutureExt;
use gadk_agent::callback::{
    AfterModelCallback, AfterRunCallback, AfterToolCallback, BeforeModelCallback,
    BeforeRunCallback, BeforeToolCallback,
};
use gadk_agent::{AgentError, Context, InvocationContext, Plugin, PluginConfig, Tool, ToolError};
use gadk_genai::Content;
use gadk_model::{LlmRequest, LlmResponse, ModelError};
use gadk_platform::go_path_join;
use gadk_session::State;
use parking_lot::Mutex;
use serde_json::{Map, Value};

use self::state::InvocationRecordState;
use crate::check_within_base_dir;
use crate::conformance::gonode::{from_go_node_partial, to_go_node};
use crate::conformance::recording::{
    LlmRequestView, RecordedFunctionCall, RecordedFunctionResponse, Recordings,
};
use crate::govalue::json_map_to_go;

/// The session state key holding the record configuration.
const RECORDINGS_CONFIG_KEY: &str = "_adk_recordings_config";

/// The record plugin's state.
pub(crate) struct RecordPlugin {
    /// The invocation states, keyed by invocation id, behind a lock.
    pub(crate) invocation_states: Mutex<HashMap<String, Arc<InvocationRecordState>>>,
    /// The base directory the recording case directory must lie within.
    pub(crate) allowed_base_dir: PathBuf,
}

/// Builds the `record_plugin` plugin with before/after run, model and tool callbacks.
pub fn new(allowed_base_dir: impl Into<PathBuf>) -> Plugin {
    let p = Arc::new(RecordPlugin {
        invocation_states: Mutex::new(HashMap::new()),
        allowed_base_dir: allowed_base_dir.into(),
    });

    let before_run_callback: BeforeRunCallback = {
        let p = p.clone();
        Arc::new(move |ctx: InvocationContext| {
            let p = p.clone();
            async move { p.before_run(&ctx) }.boxed()
        })
    };
    let after_run_callback: AfterRunCallback = {
        let p = p.clone();
        Arc::new(move |ctx: InvocationContext| {
            let p = p.clone();
            async move { p.after_run(&ctx) }.boxed()
        })
    };
    let before_model_callback: BeforeModelCallback = {
        let p = p.clone();
        Arc::new(move |ctx: Context, req: LlmRequest| {
            let p = p.clone();
            async move {
                let r = p.before_model(&ctx, &req);
                (req, r)
            }
            .boxed()
        })
    };
    let after_model_callback: AfterModelCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: Context, resp: LlmResponse, err: Option<Arc<ModelError>>| {
                let p = p.clone();
                async move {
                    let r = p.after_model(&ctx, &resp, err.as_deref());
                    (resp, r)
                }
                .boxed()
            },
        )
    };
    let before_tool_callback: BeforeToolCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: Context, tool: Arc<dyn Tool>, args: Map<String, Value>| {
                let p = p.clone();
                async move {
                    let r = p.before_tool(&ctx, tool.as_ref(), &args);
                    (args, r)
                }
                .boxed()
            },
        )
    };
    let after_tool_callback: AfterToolCallback = {
        let p = p.clone();
        Arc::new(
            move |ctx: Context,
                  tool: Arc<dyn Tool>,
                  args: Map<String, Value>,
                  result: Map<String, Value>,
                  err: Option<Arc<ToolError>>| {
                let p = p.clone();
                async move { p.after_tool(&ctx, tool.as_ref(), &args, &result, err.as_deref()) }
                    .boxed()
            },
        )
    };

    Plugin::new(PluginConfig {
        name: "record_plugin".into(),
        before_run_callback: Some(before_run_callback),
        after_run_callback: Some(after_run_callback),
        before_model_callback: Some(before_model_callback),
        after_model_callback: Some(after_model_callback),
        before_tool_callback: Some(before_tool_callback),
        after_tool_callback: Some(after_tool_callback),
        ..Default::default()
    })
}

impl RecordPlugin {
    /// Creates the invocation's record state when record mode is on.
    pub(crate) fn before_run(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Option<Content>, AgentError> {
        // A gadk invocation context always has a session.
        let session = ctx.session();
        if !self.is_record_mode_on(session.state())? {
            return Ok(None);
        }

        // Create fresh record state for this invocation
        self.create_invocation_state(ctx)?;
        Ok(None)
    }

    /// Starts an LLM recording with the sanitized request.
    pub(crate) fn before_model(
        &self,
        ctx: &Context,
        req: &LlmRequest,
    ) -> Result<Option<LlmResponse>, AgentError> {
        if !matches!(self.is_record_mode_on(ctx.state().as_ref()), Ok(true)) {
            return Ok(None);
        }

        let state = self.get_invocation_state(ctx.invocation_id())?;

        // Sanitize and clone the request (to prevent YAML serialization panics caused by
        // `http_options.extras_request_provider`)
        // The recording is a snapshot taken now, so a later mutation of the request or response
        // before `afterRun` does not show in the file.
        let sanitized_req = sanitize_llm_request(req);

        state.start_llm_recording(sanitized_req);
        Ok(None)
    }

    /// Appends the response; a non-partial response completes the recording.
    pub(crate) fn after_model(
        &self,
        ctx: &Context,
        resp: &LlmResponse,
        err: Option<&ModelError>,
    ) -> Result<Option<LlmResponse>, AgentError> {
        if !matches!(self.is_record_mode_on(ctx.state().as_ref()), Ok(true)) {
            return Ok(None);
        }

        // gadk always hands a response.
        if err.is_some() {
            return Ok(None);
        }

        let state = self.get_invocation_state(ctx.invocation_id())?;

        state.append_llm_response(resp.clone());

        // If it is a final non-partial response, complete the recording
        if !resp.partial {
            state.complete_llm_recording(&ctx.agent_name());
        }

        Ok(None)
    }

    /// Starts a tool recording keyed by the function call id.
    pub(crate) fn before_tool(
        &self,
        ctx: &Context,
        tool: &dyn Tool,
        args: &Map<String, Value>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        if !matches!(self.is_record_mode_on(ctx.state().as_ref()), Ok(true)) {
            return Ok(None);
        }

        let state = self.get_invocation_state(ctx.invocation_id())?;

        let fc = RecordedFunctionCall {
            id: ctx.function_call_id(),
            name: tool.name().to_owned(),
            args: Some(json_map_to_go(args)),
            ..Default::default()
        };

        state.start_tool_recording(&ctx.function_call_id(), fc);
        Ok(None)
    }

    /// Completes the tool recording with the result.
    pub(crate) fn after_tool(
        &self,
        ctx: &Context,
        tool: &dyn Tool,
        _args: &Map<String, Value>,
        result: &Map<String, Value>,
        _err: Option<&ToolError>,
    ) -> Result<Option<Map<String, Value>>, AgentError> {
        if !matches!(self.is_record_mode_on(ctx.state().as_ref()), Ok(true)) {
            return Ok(None);
        }

        let state = self.get_invocation_state(ctx.invocation_id())?;

        // gadk's flow hands a failed tool's missing result over as an empty map; yaml.v3
        // encodes a nil and an empty map the same, as `{}` (kept inside `tool_response`), so
        // the recording is the same.
        let resp = RecordedFunctionResponse {
            id: ctx.function_call_id(),
            name: tool.name().to_owned(),
            response: Some(json_map_to_go(result)),
            ..Default::default()
        };

        state.complete_tool_recording(&ctx.function_call_id(), &ctx.agent_name(), resp);
        Ok(None)
    }

    /// Saves the recordings and drops the invocation's state.
    pub(crate) fn after_run(&self, ctx: &InvocationContext) {
        let session = ctx.session();
        if !matches!(self.is_record_mode_on(session.state()), Ok(true)) {
            return;
        }

        let Ok(state) = self.get_invocation_state(ctx.invocation_id()) else {
            return;
        };

        // Serialize and save
        self.save_recordings(&state);

        // The state is dropped whether or not saving succeeded.
        self.invocation_states.lock().remove(ctx.invocation_id());
    }

    /// Returns `(case_dir, msg_index, streaming_mode)`, `("", 0, "")` when off;
    /// `invalid path format: {cause}`,
    /// `record config error: 'dir' is not within the allowed base directory`,
    /// `record config 'user_message_index' is not a number`.
    pub(crate) fn parse_record_config(
        &self,
        session_state: &dyn State,
    ) -> Result<(String, i64, String), AgentError> {
        // A gadk session always has a state.
        let off = || Ok((String::new(), 0, String::new()));

        let Ok(config_val) = session_state.get(RECORDINGS_CONFIG_KEY) else {
            return off();
        };

        let Value::Object(config) = config_val else {
            return off();
        };

        let case_dir = match config.get("dir") {
            Some(Value::String(dir)) if !dir.is_empty() => dir.clone(),
            _ => return off(),
        };

        check_within_base_dir(&self.allowed_base_dir, &case_dir, "record")
            .map_err(AgentError::other)?;

        let msg_index = match config.get("user_message_index") {
            None | Some(Value::Null) => return off(),
            // An integer JSON number.
            Some(Value::Number(n)) if n.is_i64() => n.as_i64().unwrap_or_default(),
            // A floating-point JSON number is truncated toward zero. An out-of-range value
            // saturates (the conversion is implementation-defined on other platforms). A u64
            // above i64::MAX is a JSON number read as a float.
            #[allow(clippy::cast_possible_truncation)]
            Some(Value::Number(n)) => n.as_f64().unwrap_or_default() as i64,
            Some(_) => {
                return Err(AgentError::other(
                    "record config 'user_message_index' is not a number",
                ));
            }
        };

        let streaming_mode = match config.get("streaming_mode") {
            Some(Value::String(mode)) if !mode.is_empty() => mode.clone(),
            _ => "none".to_owned(),
        };

        Ok((case_dir, msg_index, streaming_mode))
    }

    /// Whether record mode is on.
    pub(crate) fn is_record_mode_on(&self, session_state: &dyn State) -> Result<bool, AgentError> {
        let (case_dir, _, _) = self.parse_record_config(session_state)?;
        Ok(!case_dir.is_empty())
    }

    /// `record state not initialized for invocation: {id}` when there is none.
    pub(crate) fn get_invocation_state(
        &self,
        id: &str,
    ) -> Result<Arc<InvocationRecordState>, AgentError> {
        self.invocation_states
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| {
                AgentError::other(format!("record state not initialized for invocation: {id}"))
            })
    }

    /// `record state not configured` when record mode is off.
    pub(crate) fn create_invocation_state(
        &self,
        ctx: &InvocationContext,
    ) -> Result<Arc<InvocationRecordState>, AgentError> {
        let session = ctx.session();
        let (case_dir, msg_index, streaming_mode) = self.parse_record_config(session.state())?;
        if case_dir.is_empty() {
            return Err(AgentError::other("record state not configured"));
        }

        let state = Arc::new(InvocationRecordState::new(
            case_dir,
            msg_index,
            streaming_mode,
        ));

        self.invocation_states
            .lock()
            .insert(ctx.invocation_id().to_owned(), state.clone());

        Ok(state)
    }

    /// Merges with the existing file (dropping the current `user_message_index`), encodes,
    /// cleans the YAML node, marshals and writes `0o644`; every error is ignored.
    pub(crate) fn save_recordings(&self, state: &InvocationRecordState) {
        let filename = if state.streaming_mode == "sse" {
            "generated-recordings-sse.yaml"
        } else {
            "generated-recordings.yaml"
        };

        let file_path = go_path_join(&[state.case_dir.as_str(), filename]);

        // Load existing recordings if the file exists (type errors keep the partly decoded
        // value; a syntax error or a decode abort leaves none).
        let mut existing_recordings = Recordings::default();
        if let Ok(data) = std::fs::read(&file_path)
            && let Ok(doc) = gadk_goyaml::unmarshal_node(&data)
            && let Ok((recs, _)) = from_go_node_partial(&doc, doc.root())
        {
            existing_recordings = recs;
        }

        // Filter out any existing recordings for the CURRENT user_message_index
        let mut filtered: Vec<_> = existing_recordings
            .recordings
            .into_iter()
            .filter(|r| r.user_message_index != state.user_message_index)
            .collect();

        // Append newly completed recordings in this turn
        filtered.extend(state.inner.lock().recordings.iter().cloned());

        let output_recordings = Recordings {
            recordings: filtered,
        };

        // Write YAML back to disk using AST traversal to prune nulls/defaults
        let Ok(mut doc) = to_go_node(&output_recordings) else {
            return;
        };

        let root = doc.root();
        clean::clean_yaml_node(&mut doc, root, false);

        let Ok(out) = gadk_goyaml::marshal_node(&doc, doc.root()) else {
            return;
        };

        let _ = go_write_file(Path::new(&file_path), &out, 0o644);
    }
}

/// The request without its `tools` and `http_options`, as recorded.
pub(crate) fn sanitize_llm_request(req: &LlmRequest) -> LlmRequestView {
    // The recorded view carries neither the tools nor the HTTP options (gadk keeps
    // `http_options` beside `config`).
    LlmRequestView::from(req)
}

/// Writes a file: creates it (with `perm`, before the umask) or truncates it, then writes; an
/// existing file keeps its mode.
fn go_write_file(path: &Path, data: &[u8], perm: u32) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, perm);
    #[cfg(not(unix))]
    let _ = perm;
    options.open(path)?.write_all(data)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::any::Any;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use gadk_agent::ContextDelta;
    use gadk_genai::{GenerateContentConfig, Part};
    use gadk_model::HttpOptions;
    use gadk_session::SessionError;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::GoMap;
    use crate::conformance::recording::{
        LlmRecording, RecordedFunctionCall, RecordedFunctionResponse, Recording, Recordings,
        ToolRecording,
    };
    use crate::govalue::json_map_to_go;
    use crate::tests::{relative_to_cwd, temp_dir};

    const NOT_A_NUMBER: &str = "record config 'user_message_index' is not a number";
    const OUTSIDE: &str = "record config error: 'dir' is not within the allowed base directory";

    /// A session state map (Go's `MockState`, but absent keys are an error as in gadk).
    #[derive(Default)]
    struct MapState(parking_lot::RwLock<Map<String, Value>>);

    impl State for MapState {
        fn get(&self, key: &str) -> Result<Value, SessionError> {
            self.0
                .read()
                .get(key)
                .cloned()
                .ok_or(SessionError::StateKeyNotExist)
        }

        fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
            self.0.write().insert(key.to_owned(), value);
            Ok(())
        }

        fn all(&self) -> Vec<(String, Value)> {
            self.0
                .read()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }
    }

    fn state_with(config: Value) -> MapState {
        let s = MapState::default();
        s.set("_adk_recordings_config", config).unwrap();
        s
    }

    fn plugin(base: &Path) -> RecordPlugin {
        RecordPlugin {
            invocation_states: Mutex::new(HashMap::new()),
            allowed_base_dir: base.to_path_buf(),
        }
    }

    fn parse(p: &RecordPlugin, config: Value) -> Result<(String, i64, String), String> {
        p.parse_record_config(&state_with(config))
            .map_err(|e| e.to_string())
    }

    fn off() -> Result<(String, i64, String), String> {
        Ok((String::new(), 0, String::new()))
    }

    fn on(dir: &str, index: i64, mode: &str) -> Result<(String, i64, String), String> {
        Ok((dir.to_owned(), index, mode.to_owned()))
    }

    fn s(p: &Path) -> String {
        p.to_str().unwrap().to_owned()
    }

    /// An invocation context whose session state holds `config` (when given).
    fn invocation(id: &str, config: Option<Value>) -> InvocationContext {
        let inv = InvocationContext::builder()
            .invocation_id(id)
            .build_invocation();
        if let Some(c) = config {
            inv.session()
                .state()
                .set("_adk_recordings_config", c)
                .unwrap();
        }
        inv
    }

    struct NamedTool(&'static str);

    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            ""
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    fn text_response(text: &str, partial: bool) -> LlmResponse {
        LlmResponse {
            content: Some(Content {
                parts: vec![Part::from_text(text)],
                role: "model".to_owned(),
            }),
            partial,
            ..Default::default()
        }
    }

    // ---- parseRecordConfig ----

    #[test]
    fn parse_record_config_is_off_without_a_usable_config() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        assert_eq!(
            p.parse_record_config(&MapState::default())
                .map_err(|e| e.to_string()),
            off()
        );
        let dir = s(&base);
        for config in [
            json!(null),
            json!("dir"),
            json!([1]),
            json!({}),
            json!({"dir": 1, "user_message_index": 0}),
            json!({"dir": "", "user_message_index": 0}),
            json!({"dir": null, "user_message_index": 0}),
            json!({"dir": dir}),
            json!({"dir": dir, "user_message_index": null}),
        ] {
            assert_eq!(parse(&p, config.clone()), off(), "{config}");
        }
    }

    #[test]
    fn parse_record_config_user_message_index_int_float_other() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let dir = s(&base);
        // Go `int`.
        assert_eq!(
            parse(&p, json!({"dir": dir, "user_message_index": 3})),
            on(&dir, 3, "none")
        );
        assert_eq!(
            parse(&p, json!({"dir": dir, "user_message_index": -2})),
            on(&dir, -2, "none")
        );
        // Go `float64` → `int(v)` truncates toward zero.
        for (f, want) in [(2.0, 2), (2.9, 2), (-1.5, -1), (0.4, 0)] {
            assert_eq!(
                parse(&p, json!({"dir": dir, "user_message_index": f})),
                on(&dir, want, "none"),
                "{f}"
            );
        }
        // Anything else.
        for v in [json!("1"), json!(true), json!([0]), json!({"i": 0})] {
            assert_eq!(
                parse(&p, json!({"dir": dir, "user_message_index": v})),
                Err(NOT_A_NUMBER.to_owned()),
                "{v}"
            );
        }
    }

    /// Out-of-range `float64` → `int`: Go's conversion is implementation-defined (arm64
    /// saturates, amd64 gives `math.MinInt64`); Rust's `as` saturates.
    #[test]
    fn parse_record_config_saturates_out_of_range_floats() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let dir = s(&base);
        assert_eq!(
            parse(&p, json!({"dir": dir, "user_message_index": 1e300})),
            on(&dir, i64::MAX, "none")
        );
        assert_eq!(
            parse(&p, json!({"dir": dir, "user_message_index": u64::MAX})),
            on(&dir, i64::MAX, "none")
        );
    }

    #[test]
    fn parse_record_config_streaming_mode() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let dir = s(&base);
        for (mode, want) in [
            (json!("sse"), "sse"),
            (json!("bidi"), "bidi"),
            (json!(""), "none"),
            (json!(null), "none"),
            (json!(1), "none"),
        ] {
            assert_eq!(
                parse(
                    &p,
                    json!({"dir": dir, "user_message_index": 0, "streaming_mode": mode})
                ),
                on(&dir, 0, want)
            );
        }
    }

    #[test]
    fn parse_record_config_base_dir_check() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let cfg = |dir: String| json!({"dir": dir, "user_message_index": 0});
        // Inside: the base itself, a child, a lexically cleaned child; returned as given.
        for dir in [
            s(&base),
            s(&base.join("case")),
            format!("{}/./a/../case/", s(&base)),
            s(&base.join("missing/deeper")),
        ] {
            assert_eq!(parse(&p, cfg(dir.clone())), on(&dir, 0, "none"), "{dir}");
        }
        // Outside.
        for dir in [
            "/etc".to_owned(),
            "/".to_owned(),
            format!("{}/..", s(&base)),
            format!("{}/case/../../x", s(&base)),
            format!("{}-sibling", s(&base)),
            // Go's `strings.HasPrefix(rel, "..")` also rejects a child named `..x`.
            format!("{}/..x", s(&base)),
        ] {
            assert_eq!(
                parse(&p, cfg(dir.clone())),
                Err(OUTSIDE.to_owned()),
                "{dir}"
            );
        }
    }

    /// The base-dir check runs before `user_message_index` is read.
    #[test]
    fn parse_record_config_checks_the_dir_first() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        assert_eq!(
            parse(&p, json!({"dir": "/etc", "user_message_index": "x"})),
            Err(OUTSIDE.to_owned())
        );
        assert_eq!(parse(&p, json!({"dir": "/etc"})), Err(OUTSIDE.to_owned()));
    }

    /// Relative `dir` and base are resolved against the working directory (Go `filepath.Abs`).
    #[test]
    fn parse_record_config_resolves_relative_paths() {
        let (_g, base) = temp_dir();
        let rel_case = relative_to_cwd(&base.join("case"));
        let p = plugin(Path::new(&relative_to_cwd(&base)));
        assert_eq!(
            parse(&p, json!({"dir": rel_case, "user_message_index": 1})),
            on(&rel_case, 1, "none")
        );
        let p = plugin(&base);
        assert_eq!(
            parse(&p, json!({"dir": "relative/case", "user_message_index": 1})),
            Err(OUTSIDE.to_owned())
        );
    }

    #[test]
    fn is_record_mode_on_follows_parse_record_config() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let ok = |c: Value| {
            p.is_record_mode_on(&state_with(c))
                .map_err(|e| e.to_string())
        };
        assert_eq!(
            ok(json!({"dir": s(&base), "user_message_index": 0})),
            Ok(true)
        );
        assert_eq!(ok(json!({"dir": s(&base)})), Ok(false));
        assert_eq!(
            ok(json!({"dir": "/etc", "user_message_index": 0})),
            Err(OUTSIDE.to_owned())
        );
        assert_eq!(
            ok(json!({"dir": s(&base), "user_message_index": "0"})),
            Err(NOT_A_NUMBER.to_owned())
        );
    }

    // ---- invocation states ----

    #[test]
    fn get_and_create_invocation_state() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        assert_eq!(
            p.get_invocation_state("inv-x").err().map(|e| e.to_string()),
            Some("record state not initialized for invocation: inv-x".to_owned())
        );

        let inv = invocation(
            "inv-1",
            Some(json!({"dir": s(&base), "user_message_index": 4.0, "streaming_mode": "sse"})),
        );
        let created = p.create_invocation_state(&inv).unwrap();
        assert_eq!(
            (
                created.case_dir.clone(),
                created.user_message_index,
                created.streaming_mode.clone()
            ),
            (s(&base), 4, "sse".to_owned())
        );
        assert!(Arc::ptr_eq(
            &created,
            &p.get_invocation_state("inv-1").unwrap()
        ));

        let off = invocation("inv-2", None);
        assert_eq!(
            p.create_invocation_state(&off).err().map(|e| e.to_string()),
            Some("record state not configured".to_owned())
        );
        let bad = invocation(
            "inv-3",
            Some(json!({"dir": s(&base), "user_message_index": "x"})),
        );
        assert_eq!(
            p.create_invocation_state(&bad).err().map(|e| e.to_string()),
            Some(NOT_A_NUMBER.to_owned())
        );
    }

    // ---- callbacks ----

    #[test]
    fn before_run_returns_config_errors_and_creates_state() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let bad = invocation("bad", Some(json!({"dir": "/etc", "user_message_index": 0})));
        assert_eq!(
            p.before_run(&bad).map_err(|e| e.to_string()),
            Err(OUTSIDE.to_owned())
        );
        let off = invocation("off", None);
        assert!(p.before_run(&off).unwrap().is_none());
        assert!(p.invocation_states.lock().is_empty());
        let good = invocation(
            "good",
            Some(json!({"dir": s(&base), "user_message_index": 0})),
        );
        assert!(p.before_run(&good).unwrap().is_none());
        assert!(p.invocation_states.lock().contains_key("good"));
    }

    /// The model and tool callbacks swallow config errors (`err != nil || !on` → no-op) but
    /// return a missing invocation state's error.
    #[test]
    fn model_and_tool_callbacks_swallow_config_errors() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let tool = NamedTool("t");
        let args = Map::new();

        let bad =
            invocation("bad", Some(json!({"dir": "/etc", "user_message_index": 0}))).into_context();
        assert!(
            p.before_model(&bad, &LlmRequest::default())
                .unwrap()
                .is_none()
        );
        assert!(
            p.after_model(&bad, &text_response("x", false), None)
                .unwrap()
                .is_none()
        );
        assert!(p.before_tool(&bad, &tool, &args).unwrap().is_none());
        assert!(
            p.after_tool(&bad, &tool, &args, &args, None)
                .unwrap()
                .is_none()
        );

        let no_state = invocation(
            "nostate",
            Some(json!({"dir": s(&base), "user_message_index": 0})),
        )
        .into_context();
        let want = Some("record state not initialized for invocation: nostate".to_owned());
        let text = |r: Result<_, AgentError>| r.err().map(|e: AgentError| e.to_string());
        assert_eq!(
            text(
                p.before_model(&no_state, &LlmRequest::default())
                    .map(|_| ())
            ),
            want
        );
        assert_eq!(
            text(
                p.after_model(&no_state, &text_response("x", false), None)
                    .map(|_| ())
            ),
            want
        );
        assert_eq!(
            text(p.before_tool(&no_state, &tool, &args).map(|_| ())),
            want
        );
        assert_eq!(
            text(
                p.after_tool(&no_state, &tool, &args, &args, None)
                    .map(|_| ())
            ),
            want
        );
    }

    #[test]
    fn after_model_skips_model_errors_and_completes_on_non_partial() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let inv = invocation(
            "inv",
            Some(json!({"dir": s(&base), "user_message_index": 0})),
        );
        p.before_run(&inv).unwrap();
        let ctx = inv.clone().into_context();
        p.before_model(
            &ctx,
            &LlmRequest {
                model: "m".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let err = ModelError::Transport("boom".into());
        p.after_model(&ctx, &text_response("ignored", false), Some(&err))
            .unwrap();
        p.after_model(&ctx, &text_response("p1", true), None)
            .unwrap();
        p.after_model(&ctx, &text_response("p2", true), None)
            .unwrap();
        let st = p.get_invocation_state("inv").unwrap();
        {
            let inner = st.inner.lock();
            assert!(
                inner.recordings.is_empty(),
                "partial responses do not complete"
            );
            assert_eq!(inner.pending_llm_responses.len(), 2);
        }
        p.after_model(&ctx, &text_response("final", false), None)
            .unwrap();
        let inner = st.inner.lock();
        let llm = inner.recordings[0].llm_recording.as_ref().unwrap();
        assert_eq!(
            llm.llm_responses,
            vec![
                text_response("p1", true),
                text_response("p2", true),
                text_response("final", false)
            ]
        );
        assert_eq!(inner.recordings[0].agent_name, "test_agent");
    }

    #[test]
    fn tool_callbacks_record_call_and_response_by_function_call_id() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let inv = invocation(
            "inv",
            Some(json!({"dir": s(&base), "user_message_index": 2})),
        );
        p.before_run(&inv).unwrap();
        let tool_ctx = |id: &str| {
            inv.clone().into_context().with_delta(ContextDelta {
                function_call_id: Some(id.to_owned()),
                ..Default::default()
            })
        };
        let (c1, c2) = (tool_ctx("c1"), tool_ctx("c2"));
        let tool = NamedTool("lookup");
        let args = json!({"q": "x", "n": 2}).as_object().unwrap().clone();
        let result = json!({"hits": [1]}).as_object().unwrap().clone();
        p.before_tool(&c1, &tool, &args).unwrap();
        p.before_tool(&c2, &tool, &Map::new()).unwrap();
        p.after_tool(&c2, &tool, &Map::new(), &result, None)
            .unwrap();
        // A failed tool: gadk hands an empty result (Go a nil map; both encode as `{}`).
        let terr = ToolError::InvalidArgument("bad".into());
        p.after_tool(&c1, &tool, &args, &Map::new(), Some(&terr))
            .unwrap();
        let st = p.get_invocation_state("inv").unwrap();
        let inner = st.inner.lock();
        assert_eq!(
            inner.recordings,
            vec![
                Recording {
                    user_message_index: 2,
                    agent_name: "test_agent".into(),
                    llm_recording: None,
                    tool_recording: Some(ToolRecording {
                        tool_call: Some(RecordedFunctionCall {
                            id: "c2".into(),
                            args: Some(GoMap::new()),
                            name: "lookup".into(),
                            ..Default::default()
                        }),
                        tool_response: Some(RecordedFunctionResponse {
                            id: "c2".into(),
                            name: "lookup".into(),
                            response: Some(json_map_to_go(&result)),
                            ..Default::default()
                        }),
                    }),
                    index: 0,
                },
                Recording {
                    user_message_index: 2,
                    agent_name: "test_agent".into(),
                    llm_recording: None,
                    tool_recording: Some(ToolRecording {
                        tool_call: Some(RecordedFunctionCall {
                            id: "c1".into(),
                            args: Some(json_map_to_go(&args)),
                            name: "lookup".into(),
                            ..Default::default()
                        }),
                        tool_response: Some(RecordedFunctionResponse {
                            id: "c1".into(),
                            name: "lookup".into(),
                            response: Some(GoMap::new()),
                            ..Default::default()
                        }),
                    }),
                    index: 0,
                },
            ]
        );
    }

    #[test]
    fn after_run_saves_and_drops_the_state() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let inv = invocation(
            "inv",
            Some(json!({"dir": s(&base), "user_message_index": 0})),
        );
        // No state yet: nothing happens.
        p.after_run(&inv);
        assert!(!base.join("generated-recordings.yaml").exists());
        p.before_run(&inv).unwrap();
        p.after_run(&inv);
        assert!(p.invocation_states.lock().is_empty());
        assert_eq!(
            std::fs::read_to_string(base.join("generated-recordings.yaml")).unwrap(),
            "{}\n"
        );
        // Record mode switched off before afterRun: the state is kept (Go returns early).
        p.before_run(&inv).unwrap();
        inv.session()
            .state()
            .set("_adk_recordings_config", json!(null))
            .unwrap();
        p.after_run(&inv);
        assert!(p.invocation_states.lock().contains_key("inv"));
    }

    // ---- sanitizeLLMRequest ----

    #[test]
    fn sanitize_llm_request_drops_tools_and_http_options() {
        let mut tools: HashMap<String, Arc<dyn Any + Send + Sync>> = HashMap::new();
        tools.insert("t".into(), Arc::new(1_u8));
        let config = GenerateContentConfig {
            temperature: Some(0.5),
            ..Default::default()
        };
        let req = LlmRequest {
            model: "m".into(),
            contents: vec![Content {
                parts: vec![Part::from_text("hi")],
                role: "user".into(),
            }],
            config: Some(config.clone()),
            http_options: Some(HttpOptions {
                base_url: Some("https://example.com".into()),
                ..Default::default()
            }),
            tools,
        };
        assert_eq!(
            sanitize_llm_request(&req),
            LlmRequestView {
                model: "m".into(),
                contents: req.contents.clone(),
                config: Some(config),
                tools: GoMap::new(),
            }
        );
    }

    /// The plugin's before-model callback records the sanitized view and hands the request
    /// back untouched.
    #[tokio::test]
    async fn before_model_callback_returns_the_request_unchanged() {
        let (_g, base) = temp_dir();
        let plugin = new(&base);
        let inv = invocation(
            "inv",
            Some(json!({"dir": s(&base), "user_message_index": 0})),
        );
        (plugin.before_run_callback().unwrap())(inv.clone())
            .await
            .unwrap();
        let mut tools: HashMap<String, Arc<dyn Any + Send + Sync>> = HashMap::new();
        tools.insert("t".into(), Arc::new(1_u8));
        let req = LlmRequest {
            model: "m".into(),
            http_options: Some(HttpOptions::default()),
            tools,
            ..Default::default()
        };
        let (back, r) =
            (plugin.before_model_callback().unwrap())(inv.clone().into_context(), req).await;
        assert!(r.unwrap().is_none());
        assert!(back.http_options.is_some());
        assert!(back.tools.contains_key("t"));
        let resp = text_response("x", false);
        let (back, r) = (plugin.after_model_callback().unwrap())(
            inv.clone().into_context(),
            resp.clone(),
            None,
        )
        .await;
        assert!(r.unwrap().is_none());
        assert_eq!(back, resp);
        assert_eq!(plugin.name(), "record_plugin");
        assert!(plugin.before_agent_callback().is_none());
        assert!(plugin.on_model_error_callback().is_none());
    }

    // ---- saveRecordings ----
    //
    // The non-corpus expectations below (empty file `{}`, unreadable existing files, modes,
    // lexical join, empty tool maps) were checked against Go's `saveRecordings` (the probe's
    // vendored copy, yaml.v3 v3.0.1) with a throwaway program.

    fn read_json(p: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    fn from_json<T: serde::de::DeserializeOwned + Default>(v: Option<&Value>) -> T {
        v.map(|v| serde_json::from_value(v.clone()).unwrap())
            .unwrap_or_default()
    }

    fn opt_map(v: Option<&Value>) -> Option<GoMap> {
        v.and_then(Value::as_object).map(json_map_to_go)
    }

    /// A recording of the probe's `input.json` (as `tests/recordings_differential.rs` reads it).
    fn recording_from_json(r: &Value) -> Recording {
        let llm_recording = r.get("llm_recording").map(|l| LlmRecording {
            llm_request: l.get("llm_request").map(|q| {
                sanitize_llm_request(&LlmRequest {
                    model: q["model"].as_str().unwrap_or_default().to_owned(),
                    contents: from_json(q.get("contents")),
                    config: q
                        .get("config")
                        .map(|c| serde_json::from_value(c.clone()).unwrap()),
                    ..Default::default()
                })
            }),
            llm_responses: from_json(l.get("llm_responses")),
        });
        let tool_recording = r.get("tool_recording").map(|t| ToolRecording {
            tool_call: t.get("tool_call").map(|c| RecordedFunctionCall {
                id: from_json(c.get("id")),
                args: opt_map(c.get("args")),
                name: from_json(c.get("name")),
                partial_args: from_json(c.get("partialArgs")),
                will_continue: from_json(c.get("willContinue")),
            }),
            tool_response: t.get("tool_response").map(|c| RecordedFunctionResponse {
                will_continue: from_json(c.get("willContinue")),
                scheduling: from_json(c.get("scheduling")),
                parts: from_json(c.get("parts")),
                id: from_json(c.get("id")),
                name: from_json(c.get("name")),
                response: opt_map(c.get("response")),
            }),
        });
        Recording {
            user_message_index: r["user_message_index"].as_i64().unwrap(),
            agent_name: r["agent_name"].as_str().unwrap().to_owned(),
            llm_recording,
            tool_recording,
            index: 0,
        }
    }

    /// Every probe corpus case through `save_recordings` on the file system: the existing
    /// file (if any) at the record file name, the case's recordings as the turn's completed
    /// recordings; the written bytes equal Go's (including the six merge cases and the SSE
    /// file name).
    #[test]
    fn save_recordings_writes_go_bytes_for_the_corpus() {
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/recordings");
        let mut cases: Vec<_> = std::fs::read_dir(&corpus)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        cases.sort();
        assert_eq!(cases.len(), 44);
        let p = plugin(&corpus);
        let mut merges = 0;
        for case in cases {
            let input = read_json(&case.join("input.json"));
            let mode = input["streaming_mode"].as_str().unwrap();
            let name = if mode == "sse" {
                "generated-recordings-sse.yaml"
            } else {
                "generated-recordings.yaml"
            };
            let (_g, dir) = temp_dir();
            if let Some(existing) = input["existing"].as_str() {
                std::fs::copy(case.join(existing), dir.join(name)).unwrap();
                merges += 1;
            }
            let state = InvocationRecordState::new(
                s(&dir),
                input["user_message_index"].as_i64().unwrap(),
                mode.to_owned(),
            );
            state.inner.lock().recordings = input["recordings"]
                .as_array()
                .unwrap()
                .iter()
                .map(recording_from_json)
                .collect();
            p.save_recordings(&state);
            assert_eq!(
                String::from_utf8(std::fs::read(dir.join(name)).unwrap()).unwrap(),
                String::from_utf8(std::fs::read(case.join(name)).unwrap()).unwrap(),
                "{}",
                case.display()
            );
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "only {name}");
        }
        assert_eq!(merges, 6);
    }

    fn one_turn_state(dir: &str, index: i64, mode: &str, text: &str) -> InvocationRecordState {
        let state = InvocationRecordState::new(dir.to_owned(), index, mode.to_owned());
        state.start_llm_recording(LlmRequestView {
            model: "m".into(),
            ..Default::default()
        });
        state.append_llm_response(text_response(text, false));
        state.complete_llm_recording("a");
        state
    }

    fn decode(path: &Path) -> Recordings {
        let doc = gadk_goyaml::unmarshal_node(&std::fs::read(path).unwrap()).unwrap();
        crate::conformance::gonode::from_go_node(&doc, doc.root()).unwrap()
    }

    /// A tool recording with empty args and an empty (gadk) / nil (Go) response: Go's bytes
    /// (yaml.v3 encodes a nil map as `{}`, and empty values inside `tool_response` are kept).
    #[test]
    fn save_recordings_empty_tool_maps_match_go() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let state = InvocationRecordState::new(s(&base), 0, "none".into());
        state.start_tool_recording(
            "c",
            RecordedFunctionCall {
                id: "c".into(),
                name: "t".into(),
                args: Some(GoMap::new()),
                ..Default::default()
            },
        );
        state.complete_tool_recording(
            "c",
            "a",
            RecordedFunctionResponse {
                id: "c".into(),
                name: "t".into(),
                response: Some(GoMap::new()),
                ..Default::default()
            },
        );
        p.save_recordings(&state);
        assert_eq!(
            std::fs::read_to_string(base.join("generated-recordings.yaml")).unwrap(),
            "recordings:
    - user_message_index: 0
      agent_name: a
      tool_recording:
        tool_call:
            id: c
            args: {}
            name: t
        tool_response:
            scheduling: \"\"
            id: c
            name: t
            response: {}
"
        );
    }

    #[test]
    fn save_recordings_file_name_by_streaming_mode() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        for (mode, name) in [
            ("sse", "generated-recordings-sse.yaml"),
            ("none", "generated-recordings.yaml"),
            ("bidi", "generated-recordings.yaml"),
        ] {
            let dir = base.join(mode);
            std::fs::create_dir(&dir).unwrap();
            p.save_recordings(&one_turn_state(&s(&dir), 0, mode, "hi"));
            let names: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            assert_eq!(names, vec![name.to_owned()], "{mode}");
        }
    }

    /// Go `filepath.Join(caseDir, filename)` cleans the joined path lexically.
    #[test]
    fn save_recordings_joins_the_case_dir_lexically() {
        let (_g, base) = temp_dir();
        std::fs::create_dir(base.join("real")).unwrap();
        let p = plugin(&base);
        let dir = format!("{}/real/./sub/..", s(&base));
        p.save_recordings(&one_turn_state(&dir, 0, "none", "hi"));
        assert!(base.join("real/generated-recordings.yaml").exists());
    }

    #[test]
    fn save_recordings_merges_other_user_messages() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let dir = s(&base);
        p.save_recordings(&one_turn_state(&dir, 0, "none", "zero"));
        p.save_recordings(&one_turn_state(&dir, 1, "none", "one"));
        p.save_recordings(&one_turn_state(&dir, 0, "none", "zero again"));
        let recs = decode(&base.join("generated-recordings.yaml"));
        let got: Vec<(i64, String)> = recs
            .recordings
            .iter()
            .map(|r| {
                let resp = &r.llm_recording.as_ref().unwrap().llm_responses[0];
                let text = resp.content.as_ref().unwrap().parts[0].text.clone();
                (r.user_message_index, text)
            })
            .collect();
        assert_eq!(
            got,
            vec![(1, "one".to_owned()), (0, "zero again".to_owned())]
        );
    }

    #[test]
    fn save_recordings_treats_unreadable_existing_files_as_empty() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let path = base.join("generated-recordings.yaml");
        for existing in [
            "",
            "null\n",
            "recordings: [\n",
            "- just\n- a list\n",
            "recordings: 5\n",
        ] {
            std::fs::write(&path, existing).unwrap();
            p.save_recordings(&one_turn_state(&s(&base), 0, "none", "new"));
            let recs = decode(&path);
            assert_eq!(recs.recordings.len(), 1, "{existing:?}");
        }
    }

    /// Every error is swallowed: a missing case directory, a directory at the file name, an
    /// unwritable file.
    #[cfg(unix)]
    #[test]
    fn save_recordings_swallows_errors() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let missing = base.join("missing");
        p.save_recordings(&one_turn_state(&s(&missing), 0, "none", "x"));
        assert!(!missing.exists());

        let blocked = base.join("blocked");
        std::fs::create_dir_all(blocked.join("generated-recordings.yaml")).unwrap();
        p.save_recordings(&one_turn_state(&s(&blocked), 0, "none", "x"));
        assert!(blocked.join("generated-recordings.yaml").is_dir());

        let readonly = base.join("readonly");
        std::fs::create_dir(&readonly).unwrap();
        let file = readonly.join("generated-recordings.yaml");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        p.save_recordings(&one_turn_state(&s(&readonly), 0, "none", "x"));
        // Root may still write it; otherwise the old bytes stay.
        let now = std::fs::read_to_string(&file).unwrap();
        assert!(now == "old" || now.contains("recordings:"), "{now}");
    }

    /// Go `os.WriteFile(path, out, 0o644)`: a new file gets `0o644 &^ umask`; an existing
    /// file keeps its mode and is truncated.
    #[cfg(unix)]
    #[test]
    fn save_recordings_writes_mode_0644() {
        let (_g, base) = temp_dir();
        let p = plugin(&base);
        let probe = base.join("probe");
        std::fs::write(&probe, "").unwrap();
        let umask = 0o666 & !std::fs::metadata(&probe).unwrap().permissions().mode();

        let fresh = base.join("fresh");
        std::fs::create_dir(&fresh).unwrap();
        p.save_recordings(&one_turn_state(&s(&fresh), 0, "none", "x"));
        let mode = std::fs::metadata(fresh.join("generated-recordings.yaml"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644 & !umask);

        let kept = base.join("kept");
        std::fs::create_dir(&kept).unwrap();
        let file = kept.join("generated-recordings.yaml");
        std::fs::write(&file, "x".repeat(10_000)).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        p.save_recordings(&one_turn_state(&s(&kept), 0, "none", "x"));
        let meta = std::fs::metadata(&file).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert!(meta.len() < 10_000, "truncated");
    }

    // ---- concurrency ----

    /// Many invocations run the whole callback sequence at once on a multi-threaded runtime:
    /// the state map is only locked inside the synchronous methods (never across an
    /// `.await`) and each invocation writes its own case directory.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_invocations_are_independent() {
        let (_g, base) = temp_dir();
        let plugin = new(&base);
        let mut tasks = Vec::new();
        for i in 0..32_i64 {
            let plugin = plugin.clone();
            let dir = base.join(format!("case-{i}"));
            std::fs::create_dir(&dir).unwrap();
            tasks.push(tokio::spawn(async move {
                let inv = invocation(
                    &format!("inv-{i}"),
                    Some(json!({"dir": s(&dir), "user_message_index": i})),
                );
                (plugin.before_run_callback().unwrap())(inv.clone())
                    .await
                    .unwrap();
                let ctx = inv.clone().into_context();
                for turn in 0..3 {
                    let req = LlmRequest {
                        model: format!("m-{i}-{turn}"),
                        ..Default::default()
                    };
                    let (_, r) = (plugin.before_model_callback().unwrap())(ctx.clone(), req).await;
                    r.unwrap();
                    tokio::task::yield_now().await;
                    let (_, r) = (plugin.after_model_callback().unwrap())(
                        ctx.clone(),
                        text_response(&format!("r-{i}-{turn}"), false),
                        None,
                    )
                    .await;
                    r.unwrap();
                }
                let tool: Arc<dyn Tool> = Arc::new(NamedTool("t"));
                let tctx = ctx.with_delta(ContextDelta {
                    function_call_id: Some(format!("call-{i}")),
                    ..Default::default()
                });
                (plugin.before_tool_callback().unwrap())(tctx.clone(), tool.clone(), Map::new())
                    .await
                    .1
                    .unwrap();
                tokio::task::yield_now().await;
                (plugin.after_tool_callback().unwrap())(tctx, tool, Map::new(), Map::new(), None)
                    .await
                    .unwrap();
                (plugin.after_run_callback().unwrap())(inv).await;
                (i, dir)
            }));
        }
        for t in tasks {
            let (i, dir) = t.await.unwrap();
            let recs = decode(&dir.join("generated-recordings.yaml"));
            assert_eq!(recs.recordings.len(), 4);
            assert!(recs.recordings.iter().all(|r| r.user_message_index == i));
            let models: Vec<String> = recs.recordings[..3]
                .iter()
                .map(|r| {
                    r.llm_recording
                        .as_ref()
                        .unwrap()
                        .llm_request
                        .as_ref()
                        .unwrap()
                        .model
                        .clone()
                })
                .collect();
            assert_eq!(
                models,
                (0..3).map(|t| format!("m-{i}-{t}")).collect::<Vec<_>>()
            );
            assert_eq!(
                recs.recordings[3]
                    .tool_recording
                    .as_ref()
                    .unwrap()
                    .tool_call
                    .as_ref()
                    .unwrap()
                    .id,
                format!("call-{i}")
            );
        }
    }
}
