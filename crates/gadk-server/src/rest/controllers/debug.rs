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

//! Port of Go `controllers/debug.go` and `internal/routers/debug.go` (plan 5a Task 12).

use std::collections::BTreeMap;
use std::sync::Arc;

use gadk_agent::Loader;
use gadk_session::{Event, GetRequest, SessionService};
use http::{Method, StatusCode};
use serde::Serialize;
use serde_json::Value;

use super::DotGraph;
use super::handlers::{forbid_unless_can_act_as, path_vars, session_error_text};
use crate::authz::{self, Authorizer};
use crate::rest::DebugTelemetry;
use crate::rest::agent_graph::get_agent_graph;
use crate::rest::debug_telemetry::{DebugLog, DebugSpan};
use crate::rest::http_error::{encode_json_response, http_error};
use crate::rest::models::session_id_from_http_parameters;
use crate::rest::routing::{DEV_PREFIX, Request, Response, Route, controller_handler, route};

/// semconv `gen_ai.operation.name`.
const OPERATION_NAME_ATTRIBUTE: &str = "gen_ai.operation.name";
/// The operations whose span the per-event trace returns (Go `wantedOperations`).
const WANTED_OPERATIONS: [&str; 2] = ["execute_tool", "generate_content"];

/// Port of Go `controllers.DebugAPIController`.
pub struct DebugApiController {
    sessions: Arc<dyn SessionService>,
    loader: Option<Arc<dyn Loader>>,
    telemetry: DebugTelemetry,
    authorizer: Arc<dyn Authorizer>,
}

impl DebugApiController {
    /// Go `NewDebugAPIController` (authorizer: [`authz::noop`]).
    pub fn new(
        sessions: Arc<dyn SessionService>,
        loader: Option<Arc<dyn Loader>>,
        telemetry: DebugTelemetry,
    ) -> Self {
        Self {
            sessions,
            loader,
            telemetry,
            authorizer: authz::noop(),
        }
    }

    /// Go `WithAuthorizer`.
    pub fn with_authorizer(mut self, a: Arc<dyn Authorizer>) -> Self {
        self.authorizer = a;
        self
    }

    /// Go `EventSpanHandler`: the event's `execute_tool` or `generate_content` span, flattened
    /// (its attributes beside the span fields), as the web UI's per-event trace view reads it.
    pub async fn event_span_handler(&self, req: Request) -> Response {
        let event_id = path_vars(&req)
            .0
            .get("event_id")
            .cloned()
            .unwrap_or_default();
        if event_id.is_empty() {
            return http_error(StatusCode::BAD_REQUEST, "event_id parameter is required");
        }
        let spans = self.telemetry.get_spans_by_event_id(&event_id);
        // A single event has at most one generate_content or execute_tool span: the first wins.
        let wanted = spans.iter().find(|s| {
            s.attributes
                .get(OPERATION_NAME_ATTRIBUTE)
                .and_then(Value::as_str)
                .is_some_and(|op| WANTED_OPERATIONS.contains(&op))
        });
        match wanted {
            Some(span) => encode_json_response(&convert_event_span(span), StatusCode::OK),
            None => http_error(
                StatusCode::NOT_FOUND,
                &format!("event not found: {event_id}"),
            ),
        }
    }

    /// Go `SessionSpansHandler`: every stored span of the session's traces.
    pub async fn session_spans_handler(&self, req: Request) -> Response {
        let session_id = path_vars(&req)
            .0
            .get("session_id")
            .cloned()
            .unwrap_or_default();
        if session_id.is_empty() {
            return http_error(StatusCode::BAD_REQUEST, "session_id parameter is required");
        }
        let spans = self.telemetry.get_spans_by_session_id(&session_id);
        encode_json_response(&spans, StatusCode::OK)
    }

    /// Go `EventGraphHandler`: the app's agent graph (dark theme) with the event's edges
    /// highlighted, as `{"dotSrc": …}`.
    pub async fn event_graph_handler(&self, req: Request) -> Response {
        let vars = path_vars(&req);
        let sid = match session_id_from_http_parameters(&vars) {
            Ok(sid) => sid,
            Err(e) => return http_error(StatusCode::BAD_REQUEST, &e),
        };
        // The route is user-scoped: a caller may only read their own session's event graph.
        if let Some(resp) =
            forbid_unless_can_act_as(&*self.authorizer, req.extensions(), &sid.user_id)
        {
            return resp;
        }
        let session = match self
            .sessions
            .get(GetRequest {
                app_name: sid.app_name.clone(),
                user_id: sid.user_id.clone(),
                session_id: sid.id.clone(),
                ..Default::default()
            })
            .await
        {
            Ok(session) => session,
            Err(e) => {
                return http_error(
                    StatusCode::BAD_REQUEST,
                    &session_error_text(&e, &sid.id, false),
                );
            }
        };
        let event_id = vars.0.get("event_id").cloned().unwrap_or_default();
        if event_id.is_empty() {
            return http_error(StatusCode::BAD_REQUEST, "event_id parameter is required");
        }
        let Some(event) = session.events().into_iter().find(|e| e.id == event_id) else {
            return http_error(StatusCode::NOT_FOUND, "event not found");
        };
        let highlighted_pairs = highlighted_pairs(&event);
        // parity: Go calls a nil loader and panics; gadk answers 500 instead.
        let Some(loader) = &self.loader else {
            return http_error(StatusCode::INTERNAL_SERVER_ERROR, "no agent loader");
        };
        let agent = match loader.load_agent(&sid.app_name) {
            Ok(agent) => agent,
            Err(e) => return http_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        };
        let dot_src = get_agent_graph(&*agent, &highlighted_pairs);
        encode_json_response(&DotGraph { dot_src }, StatusCode::OK)
    }
}

/// Go's highlighted pairs for an event: `[call name, author]` for each named function call, else
/// for each named function response, else `[author, author]`.
fn highlighted_pairs(event: &Event) -> Vec<Vec<String>> {
    let parts = event
        .response
        .content
        .as_ref()
        .map(|c| c.parts.as_slice())
        .unwrap_or_default();
    let calls: Vec<&str> = parts
        .iter()
        .filter_map(|p| p.function_call.as_ref().map(|f| f.name.as_str()))
        .collect();
    let responses: Vec<&str> = parts
        .iter()
        .filter_map(|p| p.function_response.as_ref().map(|f| f.name.as_str()))
        .collect();
    let names = if !calls.is_empty() {
        calls
    } else if !responses.is_empty() {
        responses
    } else {
        return vec![vec![event.author.clone(), event.author.clone()]];
    };
    names
        .into_iter()
        .filter(|name| !name.is_empty())
        .map(|name| vec![name.to_owned(), event.author.clone()])
        .collect()
}

/// A value of Go's flattened event span (`map[string]any`).
#[derive(Serialize)]
#[serde(untagged)]
enum FlatValue<'a> {
    Str(&'a str),
    Int(i64),
    Logs(&'a [DebugLog]),
    Attribute(&'a Value),
}

/// Go `convertEventSpan`: the span fields and its attributes in one object (keys sorted, an
/// attribute overriding a span field of the same name).
fn convert_event_span(span: &DebugSpan) -> BTreeMap<&str, FlatValue<'_>> {
    let mut flat = BTreeMap::from([
        ("name", FlatValue::Str(&span.name)),
        ("start_time", FlatValue::Int(span.start_time)),
        ("end_time", FlatValue::Int(span.end_time)),
        ("trace_id", FlatValue::Str(&span.trace_id)),
        ("span_id", FlatValue::Str(&span.span_id)),
        ("parent_span_id", FlatValue::Str(&span.parent_span_id)),
        ("logs", FlatValue::Logs(&span.logs)),
    ]);
    for (k, v) in &span.attributes {
        flat.insert(k.as_str(), FlatValue::Attribute(v));
    }
    flat
}

/// Go `DebugAPIRouter.Routes`.
pub(crate) fn debug_routes(c: Arc<DebugApiController>) -> Vec<Route> {
    vec![
        route(
            "GetSessionTraceDev",
            &[Method::GET],
            format!("{DEV_PREFIX}/debug/trace/session/{{session_id}}"),
            controller_handler!(c, session_spans_handler),
        ),
        route(
            "GetTraceDictDev",
            &[Method::GET],
            format!("{DEV_PREFIX}/debug/trace/{{event_id}}"),
            controller_handler!(c, event_span_handler),
        ),
        route(
            "GetEventGraphDev",
            &[Method::GET],
            format!(
                "{DEV_PREFIX}/users/{{user_id}}/sessions/{{session_id}}/events/{{event_id}}/graph"
            ),
            controller_handler!(c, event_graph_handler),
        ),
        route(
            "GetTraceDict",
            &[Method::GET],
            "/debug/trace/{event_id}",
            controller_handler!(c, event_span_handler),
        ),
        route(
            "GetEventGraph",
            &[Method::GET],
            "/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}/graph",
            controller_handler!(c, event_graph_handler),
        ),
        route(
            "GetSessionTrace",
            &[Method::GET],
            "/debug/trace/session/{session_id}",
            controller_handler!(c, session_spans_handler),
        ),
    ]
}
