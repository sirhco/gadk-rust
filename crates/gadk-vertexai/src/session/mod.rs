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

//! [`VertexAiSessionService`]: a [`SessionService`] backed by Vertex AI Agent Engine sessions
//! (port of ADK-Go `session/vertexai`).

pub(crate) mod client;
pub(crate) mod convert;
mod local;

use std::sync::Arc;

use async_trait::async_trait;
use gadk_model::HttpClient;
use gadk_platform::env::{EnvProvider, ProcessEnv};
use gadk_platform::go_quote;
use gadk_session::state_util::trim_temp_delta_state;
use gadk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionError,
    SessionService,
};
use google_cloud_auth::credentials::Credentials;

use crate::client::VertexClient;
use crate::config::{SESSION_ENDPOINT, resolve};
use crate::error::wrap;
use client::{SessionClient, prefix_error};

pub use local::VertexSession;

/// How a [`VertexAiSessionService`] reaches its Agent Engine (Go `VertexAIServiceConfig` plus
/// the `option.ClientOption`s Go passes to `aiplatform.NewSessionClient`).
#[derive(Clone)]
pub struct VertexAiSessionServiceConfig {
    /// The Google Cloud project ID (Go `ProjectID`).
    pub project_id: String,
    /// The engine's location (Go `Location`).
    pub location: String,
    /// The reasoning engine's numeric ID (Go `ReasoningEngine`); `None` resolves the engine
    /// from each request's app name: a numeric app name is the ID, and an app name of the form
    /// `projects/…/locations/…/reasoningEngines/{id}` names it.
    pub reasoning_engine: Option<String>,
    /// The base URL (Go `option.WithEndpoint`); `None` = `https://aiplatform.googleapis.com`,
    /// whatever the location (Go's session client default).
    pub endpoint: Option<String>,
    /// The credentials sent with every request (Go `option.WithAuthCredentials`); `None`
    /// resolves Application Default Credentials through [`env`](Self::env) with the
    /// `cloud-platform` scope.
    pub credentials: Option<Credentials>,
    /// Send no credentials (Go `option.WithoutAuthentication`).
    pub without_authentication: bool,
    /// The HTTP client (Go `option.WithHTTPClient`); `None` builds a reqwest client.
    pub http_client: Option<Arc<dyn HttpClient>>,
    /// Where `GOOGLE_APPLICATION_CREDENTIALS` is read.
    pub env: Arc<dyn EnvProvider>,
}

impl Default for VertexAiSessionServiceConfig {
    fn default() -> Self {
        Self {
            project_id: String::new(),
            location: String::new(),
            reasoning_engine: None,
            endpoint: None,
            credentials: None,
            without_authentication: false,
            http_client: None,
            env: Arc::new(ProcessEnv),
        }
    }
}

impl std::fmt::Debug for VertexAiSessionServiceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexAiSessionServiceConfig")
            .field("project_id", &self.project_id)
            .field("location", &self.location)
            .field("reasoning_engine", &self.reasoning_engine)
            .field("endpoint", &self.endpoint)
            .field("credentials", &self.credentials.is_some())
            .field("without_authentication", &self.without_authentication)
            .field("http_client", &self.http_client.is_some())
            .finish_non_exhaustive()
    }
}

/// A [`SessionService`] that keeps sessions and events in a Vertex AI Agent Engine (Go
/// `vertexAIService`).
pub struct VertexAiSessionService {
    client: client::SessionClient,
}

impl VertexAiSessionService {
    /// A service for `config` (Go `NewSessionService`). Synchronous; only Application Default
    /// Credentials need a Tokio runtime.
    ///
    /// # Errors
    ///
    /// [`SessionError::Backend`] with Go's text `failed to create Vertex AI client: could not
    /// establish connection to the aiplatform server: {adc error}` when Application Default
    /// Credentials cannot be built.
    pub fn new(config: VertexAiSessionServiceConfig) -> Result<Self, SessionError> {
        let resolved = resolve(
            config.endpoint.as_deref(),
            SESSION_ENDPOINT,
            config.credentials,
            config.without_authentication,
            config.http_client,
            config.env.as_ref(),
        )
        .map_err(|e| {
            SessionError::Backend(wrap(
                "failed to create Vertex AI client",
                wrap("could not establish connection to the aiplatform server", e),
            ))
        })?;
        Ok(Self {
            client: SessionClient::new(
                VertexClient::new(resolved),
                config.project_id,
                config.location,
                config.reasoning_engine.unwrap_or_default(),
            ),
        })
    }
}

#[async_trait]
impl SessionService for VertexAiSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Arc<dyn Session>, SessionError> {
        if req.app_name.is_empty() || req.user_id.is_empty() {
            return Err(SessionError::InvalidRequest(format!(
                "app_name and user_id are required, got app_name: {}, user_id: {}",
                go_quote(&req.app_name),
                go_quote(&req.user_id)
            )));
        }
        let session = self
            .client
            .create_session(&req)
            .await
            .map_err(|e| prefix_error("failed to create session", e))?;
        Ok(Arc::new(session))
    }

    /// Go `Get`: `GetSession` and `ListEvents` run concurrently; a not-found from either is
    /// [`SessionError::NotFound`], else the session's error is reported before the events'.
    async fn get(&self, req: GetRequest) -> Result<Arc<dyn Session>, SessionError> {
        validate_session_request(&req.app_name, &req.user_id, &req.session_id)?;
        let (session, events) = tokio::join!(
            self.client.get_session(&req),
            self.client.list_session_events(
                &req.app_name,
                &req.session_id,
                req.after,
                req.num_recent_events,
            ),
        );
        if matches!(session, Err(SessionError::NotFound))
            || matches!(events, Err(SessionError::NotFound))
        {
            return Err(SessionError::NotFound);
        }
        let session = session.map_err(|e| prefix_error("failed to get session", e))?;
        let events = events.map_err(|e| prefix_error("failed to list session events", e))?;
        session.set_events(events);
        Ok(Arc::new(session))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Arc<dyn Session>>, SessionError> {
        if req.app_name.is_empty() {
            return Err(SessionError::InvalidRequest(format!(
                "app_name is required, got app_name: {}",
                go_quote(&req.app_name)
            )));
        }
        let sessions = self
            .client
            .list_sessions(&req)
            .await
            .map_err(|e| prefix_error("failed to request sessions list", e))?;
        Ok(sessions
            .into_iter()
            .map(|s| Arc::new(s) as Arc<dyn Session>)
            .collect())
    }

    async fn delete(&self, req: DeleteRequest) -> Result<(), SessionError> {
        validate_session_request(&req.app_name, &req.user_id, &req.session_id)?;
        self.client
            .delete_session(&req)
            .await
            .map_err(|e| prefix_error("failed to delete session", e))
    }

    /// Go `AppendEvent`: persists the event without its `temp:` state keys (a partial event
    /// is not sent), then applies the untrimmed event to the local session, which must be a
    /// [`VertexSession`]. `event` is not changed.
    async fn append_event(
        &self,
        session: &dyn Session,
        event: &mut Event,
    ) -> Result<(), SessionError> {
        if session.id().is_empty() {
            // Go's `event == nil` half cannot happen: `event` is a reference.
            return Err(SessionError::InvalidRequest(format!(
                "session_id and event are required, got session_id: {}, event_id: false",
                go_quote(session.id())
            )));
        }
        // temp: keys are scoped to the current invocation and must not reach storage. The
        // local session below still receives the untrimmed event, which keeps them readable
        // for the rest of the invocation.
        self.client
            .append_event(
                session.app_name(),
                session.id(),
                &trim_temp_delta_state(event),
            )
            .await
            .map_err(|e| prefix_error("failed to append event", e))?;
        let Some(local) = session.as_any().downcast_ref::<VertexSession>() else {
            // Go prints the session's dynamic type with `%T`.
            return Err(SessionError::InvalidRequest(
                "AppendEvent for Vertex AI service only supports sessions created by it, got \
                 another Session implementation"
                    .to_owned(),
            ));
        };
        local
            .append_event(event)
            .map_err(|e| prefix_error("failed to append event", e))
    }
}

/// Go's Get and Delete validation.
fn validate_session_request(
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> Result<(), SessionError> {
    if app_name.is_empty() || user_id.is_empty() || session_id.is_empty() {
        return Err(SessionError::InvalidRequest(format!(
            "app_name, user_id and session_id are required, got app_name: {}, user_id: {}, \
             session_id: {}",
            go_quote(app_name),
            go_quote(user_id),
            go_quote(session_id)
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use gadk_model::gcp_auth::NO_RUNTIME;
    use gadk_model::rest::test_util::{FakeResponse, RecordedRequest, ScriptedClient};
    use gadk_platform::env::MapEnv;
    use gadk_session::State;
    use pretty_assertions::assert_eq;
    use serde_json::{Map, Value, json};
    use tokio::time::Instant;

    use super::*;

    const ENGINE: &str = "projects/p/locations/l/reasoningEngines/1";
    const SESSION: &str = "projects/p/locations/l/reasoningEngines/1/sessions/s1";

    /// A service for project `p`, location `l`, no configured engine, sending through
    /// `http` to the default endpoint.
    fn service(http: &Arc<ScriptedClient>) -> VertexAiSessionService {
        VertexAiSessionService::new(VertexAiSessionServiceConfig {
            project_id: "p".into(),
            location: "l".into(),
            without_authentication: true,
            http_client: Some(http.clone()),
            env: Arc::new(MapEnv(HashMap::new())),
            ..Default::default()
        })
        .unwrap()
    }

    fn google_error(status: u16, code: &str, message: &str) -> FakeResponse {
        FakeResponse::json_status(
            status,
            json!({"error": {"code": status, "message": message, "status": code}}).to_string(),
        )
    }

    fn not_found() -> FakeResponse {
        google_error(404, "NOT_FOUND", "Session not found.")
    }

    fn session_json(user: &str) -> FakeResponse {
        FakeResponse::json(
            json!({
                "name": SESSION,
                "userId": user,
                "sessionState": {"a": 1, "gone": null},
                "updateTime": "2025-01-02T03:04:05.120Z",
            })
            .to_string(),
        )
    }

    fn create_op() -> FakeResponse {
        FakeResponse::json(
            json!({"name": format!("{SESSION}/operations/9"), "done": true}).to_string(),
        )
    }

    /// `(method, path)` of a request, the path without `/v1beta1/`.
    fn route(req: &RecordedRequest) -> (String, String) {
        let uri: http::Uri = req.url.parse().unwrap();
        let path = uri.path().trim_start_matches("/v1beta1/").to_owned();
        (req.method.clone(), path)
    }

    fn create_request() -> CreateRequest {
        CreateRequest {
            app_name: "1".into(),
            user_id: "u".into(),
            ..Default::default()
        }
    }

    fn get_request() -> GetRequest {
        GetRequest {
            app_name: "1".into(),
            user_id: "u".into(),
            session_id: "s1".into(),
            ..Default::default()
        }
    }

    fn backend_text(err: &SessionError) -> String {
        assert!(matches!(err, SessionError::Backend(_)), "{err:?}");
        err.to_string()
    }

    #[test]
    fn config_default_and_debug() {
        let config = VertexAiSessionServiceConfig::default();
        assert_eq!(config.project_id, "");
        assert_eq!(config.reasoning_engine, None);
        assert_eq!(config.endpoint, None);
        assert!(!config.without_authentication);
        assert_eq!(
            format!("{config:?}"),
            "VertexAiSessionServiceConfig { project_id: \"\", location: \"\", reasoning_engine: \
             None, endpoint: None, credentials: false, without_authentication: false, \
             http_client: false, .. }"
        );
        let config = VertexAiSessionServiceConfig {
            project_id: "p".into(),
            location: "l".into(),
            reasoning_engine: Some("1".into()),
            endpoint: Some("http://127.0.0.1:1".into()),
            without_authentication: true,
            env: Arc::new(MapEnv(HashMap::new())),
            ..Default::default()
        };
        assert_eq!(
            format!("{config:?}"),
            "VertexAiSessionServiceConfig { project_id: \"p\", location: \"l\", reasoning_engine: \
             Some(\"1\"), endpoint: Some(\"http://127.0.0.1:1\"), credentials: false, \
             without_authentication: true, http_client: false, .. }"
        );
    }

    /// R2: `without_authentication` needs no runtime; the ADC path outside a runtime is an
    /// error carrying Go's two wrappers, not a panic.
    #[test]
    fn new_outside_a_runtime() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let config = VertexAiSessionServiceConfig {
            without_authentication: true,
            env: Arc::new(MapEnv(HashMap::new())),
            ..Default::default()
        };
        assert!(VertexAiSessionService::new(config).is_ok());

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("creds.json");
        std::fs::write(&file, r#"{"type":"authorized_user"}"#).unwrap();
        let config = VertexAiSessionServiceConfig {
            env: Arc::new(MapEnv(HashMap::from([(
                "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
                file.to_string_lossy().into_owned(),
            )]))),
            ..Default::default()
        };
        let err = VertexAiSessionService::new(config).err().unwrap();
        assert_eq!(
            backend_text(&err),
            format!(
                "failed to create Vertex AI client: could not establish connection to the \
                 aiplatform server: {NO_RUNTIME}"
            )
        );
    }

    /// R2: the default endpoint is the global host whatever the location, and
    /// `without_authentication` sends no `authorization` header.
    #[tokio::test]
    async fn default_endpoint_and_no_credentials() {
        let http = ScriptedClient::json(r#"{"sessions":[]}"#);
        let svc = service(&http);
        let list = svc
            .list(ListRequest {
                app_name: "1".into(),
                user_id: "a\"b".into(),
            })
            .await
            .unwrap();
        assert!(list.is_empty());
        let req = http.last_request();
        assert_eq!(
            req.url,
            "https://aiplatform.googleapis.com/v1beta1/projects/p/locations/l/reasoningEngines/1/sessions?%24alt=json%3Benum-encoding%3Dint&filter=userId%3D%22a%5C%22b%22"
        );
        assert!(req.headers.get("authorization").is_none());
        assert!(req.headers.get("x-goog-request-params").is_none());
    }

    /// R4: Go's validation texts, byte for byte, as `InvalidRequest`; nothing is sent.
    #[tokio::test]
    async fn validation_texts() {
        let http = ScriptedClient::json("{}");
        let svc = service(&http);
        let invalid = |err: SessionError| match err {
            SessionError::InvalidRequest(text) => text,
            other => panic!("want InvalidRequest, got {other:?}"),
        };
        let err = svc
            .create(CreateRequest {
                app_name: "a\n".into(),
                ..Default::default()
            })
            .await
            .err()
            .unwrap();
        assert_eq!(
            invalid(err),
            r#"app_name and user_id are required, got app_name: "a\n", user_id: """#
        );
        let err = svc
            .get(GetRequest {
                app_name: "a".into(),
                user_id: "u".into(),
                ..Default::default()
            })
            .await
            .err()
            .unwrap();
        assert_eq!(
            invalid(err),
            r#"app_name, user_id and session_id are required, got app_name: "a", user_id: "u", session_id: """#
        );
        let err = svc
            .delete(DeleteRequest {
                session_id: "s".into(),
                ..Default::default()
            })
            .await
            .err()
            .unwrap();
        assert_eq!(
            invalid(err),
            r#"app_name, user_id and session_id are required, got app_name: "", user_id: "", session_id: "s""#
        );
        let err = svc.list(ListRequest::default()).await.err().unwrap();
        assert_eq!(invalid(err), r#"app_name is required, got app_name: """#);
        let local = VertexSession::new(
            "1".into(),
            "u".into(),
            String::new(),
            Map::new(),
            Utc::now(),
        );
        let err = svc
            .append_event(&local, &mut Event::default())
            .await
            .err()
            .unwrap();
        assert_eq!(
            invalid(err),
            r#"session_id and event are required, got session_id: "", event_id: false"#
        );
        assert_eq!(http.hits(), 0);
    }

    /// R3: an app name that is neither an `Atoi` number nor an engine resource name is an
    /// invalid request, wrapped by the method's Go prefix.
    #[tokio::test]
    async fn engine_id_resolution() {
        let http = ScriptedClient::json("{}");
        let svc = service(&http);
        let client = &svc.client;
        assert_eq!(client.reasoning_engine_id("+42").unwrap(), "+42");
        assert_eq!(client.reasoning_engine_id("-7").unwrap(), "-7");
        assert_eq!(
            client
                .reasoning_engine_id("projects/p/locations/l/reasoningEngines/77")
                .unwrap(),
            "77"
        );
        for bad in [
            "",
            "+",
            "9223372036854775808",
            "1 ",
            "\u{0661}\u{0662}",
            "projects/p/locations/l/reasoningEngines/\u{0661}",
            "projects/p.q/locations/l/reasoningEngines/1",
        ] {
            assert!(
                matches!(
                    client.reasoning_engine_id(bad),
                    Err(SessionError::InvalidRequest(_))
                ),
                "{bad:?}"
            );
        }
        let err = svc
            .create(CreateRequest {
                app_name: "my-app".into(),
                user_id: "u".into(),
                ..Default::default()
            })
            .await
            .err()
            .unwrap();
        assert_eq!(
            err.to_string(),
            "invalid request: failed to create session: app name \"my-app\" is not valid. It \
             should be the full ReasoningEngine resource name or the reasoning engine numeric ID"
        );
        assert_eq!(http.hits(), 0);
    }

    /// R5: create sends the session (state as a Struct, `sessionId` only when given), then
    /// polls GetSession: not-found attempts sleep `min(i² s, 5 s)` before the next.
    #[tokio::test(start_paused = true)]
    async fn create_waits_for_the_session_with_go_delays() {
        let http = ScriptedClient::sequence(vec![
            create_op(),
            not_found(),
            not_found(),
            not_found(),
            session_json("u"),
        ]);
        let svc = service(&http);
        let start = Instant::now();
        let session = svc
            .create(CreateRequest {
                session_id: Some("s1".into()),
                state: Some(json!({"a": 1, "b": null}).as_object().cloned().unwrap()),
                ..create_request()
            })
            .await
            .unwrap();
        assert_eq!(session.id(), "s1");
        assert_eq!(session.state().get("a").unwrap(), json!(1));
        assert!(session.state().get("gone").is_err());
        assert_eq!(
            session.last_update_time(),
            "2025-01-02T03:04:05.120Z".parse::<DateTime<Utc>>().unwrap()
        );

        let reqs = http.requests();
        assert_eq!(reqs.len(), 5);
        assert_eq!(
            route(&reqs[0]),
            ("POST".into(), format!("{ENGINE}/sessions"))
        );
        assert!(reqs[0].url.contains("&sessionId=s1"), "{}", reqs[0].url);
        assert_eq!(
            reqs[0].json(),
            json!({"userId": "u", "sessionState": {"a": 1, "b": null}})
        );
        assert_eq!(
            reqs[0].header("x-goog-request-params"),
            "parent=projects%2Fp%2Flocations%2Fl%2FreasoningEngines%2F1"
        );
        for req in &reqs[1..] {
            assert_eq!(route(req), ("GET".into(), SESSION.into()));
        }
        let offsets: Vec<Duration> = reqs.iter().map(|r| r.at - start).collect();
        let secs = Duration::from_secs;
        assert_eq!(offsets, [secs(0), secs(0), secs(0), secs(1), secs(5)]);
    }

    /// R5: ten not-found attempts (sleeping 0+1+4+5×6 s) end in not-found; another error
    /// ends the wait at once with Go's wrappers.
    #[tokio::test(start_paused = true)]
    async fn create_wait_gives_up() {
        let http = ScriptedClient::sequence(vec![create_op(), not_found()]);
        let svc = service(&http);
        let start = Instant::now();
        let err = svc.create(create_request()).await.err().unwrap();
        assert!(matches!(err, SessionError::NotFound), "{err:?}");
        let reqs = http.requests();
        assert_eq!(reqs.len(), 11);
        assert!(reqs[0].url.ends_with("?%24alt=json%3Benum-encoding%3Dint"));
        assert_eq!(reqs[0].json(), json!({"userId": "u"}));
        assert_eq!(reqs[10].at - start, Duration::from_secs(35));

        let http = ScriptedClient::sequence(vec![
            create_op(),
            google_error(403, "PERMISSION_DENIED", "no"),
        ]);
        let err = service(&http).create(create_request()).await.err().unwrap();
        assert_eq!(
            backend_text(&err),
            "failed to create session: LRO for CreateSession failed: error getting operation \
             's1': error fetching session: rpc error: code = PermissionDenied desc = no"
        );

        let http = ScriptedClient::json(json!({"name": "operations/1"}).to_string());
        let err = service(&http).create(create_request()).await.err().unwrap();
        assert_eq!(
            backend_text(&err),
            "failed to create session: error creating session: invalid operation name \
             \"operations/1\": missing \"/sessions/\""
        );
    }

    #[test]
    fn operation_name_parsing() {
        use super::client::session_id_by_operation_name as parse;
        assert_eq!(parse("a/sessions/s1/operations/2").unwrap(), "s1");
        assert_eq!(
            parse("a/operations/2").unwrap_err(),
            r#"invalid operation name "a/operations/2": missing "/sessions/""#
        );
        assert_eq!(
            parse("a/sessions/s1").unwrap_err(),
            r#"invalid operation name "a/sessions/s1": missing "/operations/""#
        );
        assert_eq!(
            parse("a/operations/x/sessions/s").unwrap_err(),
            r#"invalid operation name "a/operations/x/sessions/s": structure malformed or segments out of order"#
        );
        assert_eq!(
            parse("a/sessions//operations/2").unwrap_err(),
            r#"invalid operation name "a/sessions//operations/2": empty session ID"#
        );
        use super::client::session_id_by_session_name as name;
        assert_eq!(name("a/b").unwrap(), "b");
        assert_eq!(
            name("ab").unwrap_err(),
            r#"invalid session name format "ab": missing separator '/'"#
        );
        assert_eq!(
            name("a/").unwrap_err(),
            r#"invalid session name "a/": empty session ID"#
        );
    }

    /// R13: a 400 / `INVALID_ARGUMENT` anywhere is `InvalidRequest` with Go's whole text;
    /// a 403 is `Backend`.
    #[tokio::test]
    async fn status_mapping() {
        let http = ScriptedClient::always(google_error(400, "INVALID_ARGUMENT", "bad"));
        let err = service(&http).create(create_request()).await.err().unwrap();
        assert!(matches!(err, SessionError::InvalidRequest(_)), "{err:?}");
        assert_eq!(
            err.to_string(),
            "invalid request: failed to create session: error creating session: rpc error: \
             code = InvalidArgument desc = bad"
        );

        let http = ScriptedClient::always(google_error(403, "PERMISSION_DENIED", "no"));
        let err = service(&http)
            .list(ListRequest {
                app_name: "1".into(),
                ..Default::default()
            })
            .await
            .err()
            .unwrap();
        assert_eq!(
            backend_text(&err),
            "failed to request sessions list: error creating session list: rpc error: code = \
             PermissionDenied desc = no"
        );
    }

    /// R6: Get reads the session and its events concurrently (the events filtered by Go's
    /// `timestamp>=` layout); a not-found from either call wins, else the session's error.
    #[tokio::test]
    async fn get_prefers_not_found_then_the_sessions_error() {
        let respond = |session: FakeResponse, events: FakeResponse| {
            ScriptedClient::new(move |req: &RecordedRequest| {
                if route(req).1.ends_with("/events") {
                    events.clone()
                } else {
                    session.clone()
                }
            })
        };
        let events = FakeResponse::json(
            json!({"sessionEvents": [
                {"name": format!("{SESSION}/events/e1"), "author": "a"},
                {"name": format!("{SESSION}/events/e2"), "author": "b"},
                {"name": format!("{SESSION}/events/e3"), "author": "c"},
            ]})
            .to_string(),
        );

        let http = respond(session_json("u"), events.clone());
        let session = service(&http)
            .get(GetRequest {
                num_recent_events: 2,
                after: Some("2025-01-01T00:00:01.5Z".parse().unwrap()),
                ..get_request()
            })
            .await
            .unwrap();
        let ids: Vec<String> = session.events().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["e2", "e3"]);
        let list = http
            .requests()
            .into_iter()
            .find(|r| route(r).1.ends_with("/events"))
            .unwrap();
        assert!(
            list.url
                .ends_with("&filter=timestamp%3E%3D%222025-01-01T00%3A00%3A01%2B00%3A00%22"),
            "{}",
            list.url
        );
        assert!(list.headers.get("x-goog-request-params").is_none());

        let http = respond(session_json("u"), not_found());
        let err = service(&http).get(get_request()).await.err().unwrap();
        assert!(matches!(err, SessionError::NotFound), "{err:?}");

        let http = respond(session_json("other"), events);
        let err = service(&http).get(get_request()).await.err().unwrap();
        assert!(matches!(err, SessionError::NotFound), "{err:?}");

        let http = respond(
            google_error(403, "PERMISSION_DENIED", "no"),
            google_error(500, "INTERNAL", "boom"),
        );
        let err = service(&http).get(get_request()).await.err().unwrap();
        assert_eq!(
            backend_text(&err),
            "failed to get session: error fetching session: rpc error: code = PermissionDenied \
             desc = no"
        );

        let http = respond(session_json("u"), google_error(500, "INTERNAL", "boom"));
        let err = service(&http).get(get_request()).await.err().unwrap();
        assert_eq!(
            backend_text(&err),
            "failed to list session events: error fetching session events: rpc error: code = \
             Internal desc = boom"
        );
    }

    /// R6: an `after` of Go's zero time sends no `filter` (Go `if !after.IsZero()`), as the
    /// in-memory and database services treat it.
    #[tokio::test]
    async fn get_with_go_zero_after_sends_no_filter() {
        let http = ScriptedClient::new(|req: &RecordedRequest| {
            if route(req).1.ends_with("/events") {
                FakeResponse::json("{}")
            } else {
                session_json("u")
            }
        });
        service(&http)
            .get(GetRequest {
                after: Some(gadk_session::zero_time()),
                ..get_request()
            })
            .await
            .unwrap();
        let list = http
            .requests()
            .into_iter()
            .find(|r| route(r).1.ends_with("/events"))
            .unwrap();
        assert!(
            list.url
                .ends_with("/events?%24alt=json%3Benum-encoding%3Dint"),
            "{}",
            list.url
        );
    }

    /// R8: an operation done with neither `error` nor `response` fails as Go's longrunning
    /// `Poll` does.
    #[tokio::test]
    async fn delete_done_without_result() {
        let done = |op: Value| FakeResponse::json(op.to_string());
        let name = format!("{SESSION}/operations/1");
        let http = ScriptedClient::sequence(vec![
            session_json("u"),
            done(json!({"name": name, "done": true})),
        ]);
        let req = DeleteRequest {
            app_name: "1".into(),
            user_id: "u".into(),
            session_id: "s1".into(),
        };
        let err = service(&http).delete(req.clone()).await.err().unwrap();
        assert_eq!(
            backend_text(&err),
            "failed to delete session: unsupported result type <nil>: <nil>"
        );
        let reqs = http.requests();
        assert_eq!(route(&reqs[1]), ("DELETE".into(), SESSION.into()));
        assert_eq!(
            reqs[1].header("x-goog-request-params"),
            "name=projects%2Fp%2Flocations%2Fl%2FreasoningEngines%2F1%2Fsessions%2Fs1"
        );

        let http = ScriptedClient::sequence(vec![
            session_json("u"),
            done(
                json!({"name": name, "done": true, "response": {"@type": "type.googleapis.com/google.protobuf.Empty"}}),
            ),
        ]);
        service(&http).delete(req.clone()).await.unwrap();

        let http = ScriptedClient::sequence(vec![
            session_json("u"),
            done(json!({"name": name, "done": true, "error": {"code": 9, "message": "busy"}})),
        ]);
        let err = service(&http).delete(req).await.err().unwrap();
        assert_eq!(
            backend_text(&err),
            "failed to delete session: rpc error: code = FailedPrecondition desc = busy"
        );
    }

    /// A session of another implementation.
    struct Foreign;

    impl Session for Foreign {
        fn id(&self) -> &str {
            "s1"
        }
        fn app_name(&self) -> &str {
            "1"
        }
        fn user_id(&self) -> &str {
            "u"
        }
        fn state(&self) -> &dyn State {
            unimplemented!()
        }
        fn events(&self) -> Vec<Event> {
            Vec::new()
        }
        fn last_update_time(&self) -> DateTime<Utc> {
            Utc::now()
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// R9: the event is sent first, then a session of another implementation is refused
    /// with the fixed phrase standing for Go's `%T`; a partial event sends nothing.
    #[tokio::test]
    async fn append_event_order() {
        let http = ScriptedClient::json("{}");
        let svc = service(&http);
        let mut event = Event {
            author: "a".into(),
            invocation_id: "i".into(),
            ..Default::default()
        };
        let err = svc.append_event(&Foreign, &mut event).await.err().unwrap();
        assert_eq!(
            err.to_string(),
            "invalid request: AppendEvent for Vertex AI service only supports sessions created \
             by it, got another Session implementation"
        );
        assert_eq!(http.hits(), 1);
        assert_eq!(
            route(&http.last_request()),
            ("POST".into(), format!("{SESSION}:appendEvent"))
        );

        event.response.partial = true;
        let local = VertexSession::new("1".into(), "u".into(), "s1".into(), Map::new(), Utc::now());
        svc.append_event(&local, &mut event).await.unwrap();
        assert_eq!(http.hits(), 1, "a partial event is not sent");
        assert!(local.events().is_empty(), "a partial event is not stored");
    }
}
