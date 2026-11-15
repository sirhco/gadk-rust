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

//! Port of Go `models/session.go`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::event::Event;
use super::null_default;
use crate::rest::routing::PathVars;

/// Port of Go `models.Session`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Go `ID`.
    #[serde(default, deserialize_with = "null_default")]
    pub id: String,
    /// Go `AppName`.
    #[serde(rename = "appName", default, deserialize_with = "null_default")]
    pub app_name: String,
    /// Go `UserID`.
    #[serde(rename = "userId", default, deserialize_with = "null_default")]
    pub user_id: String,
    /// Go `UpdatedAt` (`lastUpdateTime`): Unix seconds.
    #[serde(rename = "lastUpdateTime", default, deserialize_with = "null_default")]
    pub updated_at: i64,
    /// The session's events.
    #[serde(default, deserialize_with = "null_default")]
    pub events: Vec<Event>,
    /// The session's state.
    #[serde(default, deserialize_with = "null_default")]
    pub state: Map<String, Value>,
}

impl Session {
    /// Port of Go `models.FromSession`: the REST view of `s`, then [`Session::validate`].
    pub fn from_session(s: &dyn gadk_session::Session) -> Result<Session, String> {
        let mapped = Session {
            id: s.id().to_owned(),
            app_name: s.app_name().to_owned(),
            user_id: s.user_id().to_owned(),
            updated_at: s.last_update_time().timestamp(),
            events: s.events().iter().map(Event::from_session_event).collect(),
            state: s.state().all().into_iter().collect(),
        };
        mapped.validate()?;
        Ok(mapped)
    }

    /// Port of Go `Session.Validate` (Go's `state is nil` and `events is nil` cannot happen).
    pub fn validate(&self) -> Result<(), String> {
        if self.app_name.is_empty() {
            return Err("app_name is empty in received session".into());
        }
        if self.user_id.is_empty() {
            return Err("user_id is empty in received session".into());
        }
        if self.id.is_empty() {
            return Err("session_id is empty in received session".into());
        }
        // `updated_at` is not checked (Go #1870): `updated_at == 0` never caught an unset time,
        // but rejected every last update in the first second of 1970.
        Ok(())
    }
}

/// Port of Go `models.CreateSessionRequest`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CreateSessionRequest {
    /// The initial state (`null` / absent: none).
    pub state: Option<Map<String, Value>>,
    /// Events appended after the session is created.
    pub events: Option<Vec<Event>>,
}

/// Port of Go `models.UpdateSessionRequest`: the state keys to add or overwrite; an empty or
/// absent delta is a no-op.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSessionRequest {
    /// Go `StateDelta` (`stateDelta`).
    #[serde(rename = "stateDelta")]
    pub state_delta: Option<Map<String, Value>>,
}

/// Port of Go `models.SessionID`: the session named by a request path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionId {
    /// `{session_id}`; empty when the route has none.
    pub id: String,
    /// `{app_name}`.
    pub app_name: String,
    /// `{user_id}`.
    pub user_id: String,
}

/// Port of Go `models.SessionIDFromHTTPParameters`.
pub fn session_id_from_http_parameters(vars: &PathVars) -> Result<SessionId, String> {
    let get = |k: &str| vars.0.get(k).cloned().unwrap_or_default();
    let id = SessionId {
        id: get("session_id"),
        app_name: get("app_name"),
        user_id: get("user_id"),
    };
    if id.app_name.is_empty() {
        return Err("app_name parameter is required".into());
    }
    if id.user_id.is_empty() {
        return Err("user_id parameter is required".into());
    }
    Ok(id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn session_bytes_match_go() {
        let s = Session {
            id: "s".into(),
            app_name: "app".into(),
            user_id: "u".into(),
            updated_at: 1_700_000_000,
            events: vec![],
            state: Map::from_iter([
                ("b".to_owned(), Value::from(1)),
                ("a".to_owned(), Value::from("x")),
            ]),
        };
        assert_eq!(
            String::from_utf8(gadk_model::to_go_json(&s).unwrap()).unwrap(),
            r#"{"id":"s","appName":"app","userId":"u","lastUpdateTime":1700000000,"events":[],"state":{"a":"x","b":1}}"#
        );
    }

    #[test]
    fn validate_reports_go_text_in_go_order() {
        let mut s = Session::default();
        assert_eq!(
            s.validate().unwrap_err(),
            "app_name is empty in received session"
        );
        s.app_name = "a".into();
        assert_eq!(
            s.validate().unwrap_err(),
            "user_id is empty in received session"
        );
        s.user_id = "u".into();
        assert_eq!(
            s.validate().unwrap_err(),
            "session_id is empty in received session"
        );
        s.id = "s".into();
        assert!(s.validate().is_ok(), "updated_at 0 is a valid last update");
    }

    #[test]
    fn session_id_from_path_vars() {
        let vars = |pairs: &[(&str, &str)]| {
            PathVars(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect::<BTreeMap<_, _>>(),
            )
        };
        assert_eq!(
            session_id_from_http_parameters(&vars(&[("user_id", "u")])).unwrap_err(),
            "app_name parameter is required"
        );
        assert_eq!(
            session_id_from_http_parameters(&vars(&[("app_name", "a")])).unwrap_err(),
            "user_id parameter is required"
        );
        assert_eq!(
            session_id_from_http_parameters(&vars(&[
                ("app_name", "a"),
                ("user_id", "u"),
                ("session_id", "s")
            ]))
            .unwrap(),
            SessionId {
                id: "s".into(),
                app_name: "a".into(),
                user_id: "u".into()
            }
        );
    }
}
