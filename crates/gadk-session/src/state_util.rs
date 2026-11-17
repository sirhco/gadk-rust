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

//! Helpers every [`crate::SessionService`] implementation shares: state-delta handling, the
//! required-field check text and RFC 3339 time formatting.

use chrono::{DateTime, Utc};
use gadk_platform::go_quote;
use serde_json::{Map, Value};

use crate::error::SessionError;
use crate::event::Event;
use crate::service::{KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER};

pub use crate::requested_auth::check_requested_auth_configs;

type StateMap = Map<String, Value>;

/// Splits a delta into app, user (both with their
/// prefix removed) and session parts; `temp:` keys are dropped. Prefixes do not compose:
/// `app:temp:x` is app-scoped.
pub fn extract_state_deltas(delta: &StateMap) -> (StateMap, StateMap, StateMap) {
    let (mut app, mut user, mut session) = (StateMap::new(), StateMap::new(), StateMap::new());
    for (key, value) in delta {
        if let Some(k) = key.strip_prefix(KEY_PREFIX_APP) {
            app.insert(k.to_string(), value.clone());
        } else if let Some(k) = key.strip_prefix(KEY_PREFIX_USER) {
            user.insert(k.to_string(), value.clone());
        } else if !key.starts_with(KEY_PREFIX_TEMP) {
            session.insert(key.clone(), value.clone());
        }
    }
    (app, user, session)
}

/// Session state plus app and user state with their prefixes
/// put back.
pub fn merge_states(
    app_state: Option<&StateMap>,
    user_state: Option<&StateMap>,
    session_state: &StateMap,
) -> StateMap {
    let mut merged = session_state.clone();
    for (k, v) in app_state.into_iter().flatten() {
        merged.insert(format!("{KEY_PREFIX_APP}{k}"), v.clone());
    }
    for (k, v) in user_state.into_iter().flatten() {
        merged.insert(format!("{KEY_PREFIX_USER}{k}"), v.clone());
    }
    merged
}

/// A copy of `event` without `temp:` keys in its state delta. A
/// delta whose every key is `temp:` becomes an empty map, not `None`.
pub fn trim_temp_delta_state(event: &Event) -> Event {
    let mut copy = event.clone();
    if let Some(delta) = copy.actions.state_delta.as_mut() {
        delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));
    }
    copy
}

/// The services' required-field check: `Ok` when every value is non-empty, else
/// `InvalidRequest("<a>, <b> are required, got <a>: %q, <b>: %q")` (`is` for one field), with
/// each value quoted by [`gadk_platform::go_quote`].
pub fn require(fields: &[(&str, &str)]) -> Result<(), SessionError> {
    if fields.iter().all(|(_, v)| !v.is_empty()) {
        return Ok(());
    }
    let names: Vec<&str> = fields.iter().map(|(n, _)| *n).collect();
    let got: Vec<String> = fields
        .iter()
        .map(|(n, v)| format!("{n}: {}", go_quote(v)))
        .collect();
    Err(SessionError::InvalidRequest(format!(
        "{} {} required, got {}",
        names.join(", "),
        if names.len() == 1 { "is" } else { "are" },
        got.join(", ")
    )))
}

/// Formats a UTC time as RFC 3339 with trailing-zero-trimmed nanoseconds
/// (`2026-07-17T10:00:00.0015Z`).
pub fn go_rfc3339_nano(t: &DateTime<Utc>) -> String {
    crate::event::go_time::format(t).unwrap_or_else(|| t.to_rfc3339())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::{TimeZone, Utc};
    use pretty_assertions::assert_eq;
    use serde_json::{Map, Value, json};

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn extract_state_deltas_routes_by_prefix_and_drops_temp() {
        let (app, user, session) = extract_state_deltas(&map(json!({
            "app:a": 1, "user:u": 2, "temp:t": 3, "s": 4, "app:temp:x": 5
        })));
        assert_eq!(app, map(json!({"a": 1, "temp:x": 5})));
        assert_eq!(user, map(json!({"u": 2})));
        assert_eq!(session, map(json!({"s": 4})));
    }

    #[test]
    fn merge_states_prefixes_app_and_user_keys() {
        let merged = merge_states(
            Some(&map(json!({"a": 1}))),
            Some(&map(json!({"u": 2}))),
            &map(json!({"s": 3})),
        );
        assert_eq!(merged, map(json!({"app:a": 1, "user:u": 2, "s": 3})));
        assert_eq!(
            merge_states(None, None, &map(json!({"s": 3}))),
            map(json!({"s": 3}))
        );
    }

    #[test]
    fn trim_temp_delta_state_copies_without_temp_keys() {
        let mut ev = crate::Event::default();
        ev.actions.state_delta = Some(map(json!({"temp:k": 1, "k": 2})));
        let trimmed = trim_temp_delta_state(&ev);
        assert_eq!(trimmed.actions.state_delta, Some(map(json!({"k": 2}))));
        assert_eq!(
            ev.actions.state_delta,
            Some(map(json!({"temp:k": 1, "k": 2})))
        );
    }

    #[test]
    fn require_renders_go_text_with_percent_q() {
        assert!(require(&[("app_name", "a")]).is_ok());
        let err =
            require(&[("app_name", ""), ("user_id", "u"), ("session_id", "é\n")]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid request: app_name, user_id, session_id are required, got app_name: \"\", user_id: \"u\", session_id: \"é\\n\""
        );
        let crate::SessionError::InvalidRequest(msg) = require(&[("app_name", "")]).unwrap_err()
        else {
            panic!("want InvalidRequest")
        };
        assert_eq!(msg, "app_name is required, got app_name: \"\"");
    }

    #[test]
    fn go_rfc3339_nano_matches_go() {
        let t = Utc.with_ymd_and_hms(2026, 7, 17, 10, 0, 0).unwrap();
        assert_eq!(go_rfc3339_nano(&t), "2026-07-17T10:00:00Z");
        let t = t + chrono::Duration::microseconds(1500);
        assert_eq!(go_rfc3339_nano(&t), "2026-07-17T10:00:00.0015Z");
    }
}
