// Copyright 2026 Chris Olson
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

//! Scores of invocations: end-user feedback that an app records as `FEEDBACK` rows.
//!
//! A [`Score`] is a named value ([`ScoreValue`]: a thumbs up or down, a number or a category)
//! with an optional comment; a [`ScoreTarget`] names the invocation it scores. The row's
//! `content` is `{"name", "value", "data_type", "comment"}` (`data_type` is `bool`, `number` or
//! `category`; `comment` only when there is one).

use std::fmt;

use serde_json::{Map, Value};

use crate::event_types::EVENT_FEEDBACK;
use crate::plugin::{BigQueryAgentAnalyticsPlugin, EventData, Payload};
use crate::trace::{CallbackCtx, Identity, RowSpan};

/// The value of a [`Score`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ScoreValue {
    /// A thumbs up (`true`) or down (`false`).
    Bool(bool),
    /// A finite number.
    Number(f64),
    /// A category, such as `helpful`.
    Category(String),
}

impl ScoreValue {
    /// The `data_type` written with the value: `bool`, `number` or `category`.
    pub fn data_type(&self) -> &'static str {
        match self {
            ScoreValue::Bool(_) => "bool",
            ScoreValue::Number(_) => "number",
            ScoreValue::Category(_) => "category",
        }
    }
}

/// Why [`Score::new`] rejected a score.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScoreError {
    /// The name is empty after trimming.
    EmptyName,
    /// A category value is empty after trimming.
    EmptyCategory,
    /// A number value is NaN or infinite.
    NonFiniteNumber,
}

impl fmt::Display for ScoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ScoreError::EmptyName => "score name is empty",
            ScoreError::EmptyCategory => "score category is empty",
            ScoreError::NonFiniteNumber => "score number is not finite",
        })
    }
}

impl std::error::Error for ScoreError {}

/// A named score with an optional comment. Build one with [`Score::new`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Score {
    name: String,
    value: ScoreValue,
    comment: Option<String>,
}

impl Score {
    /// A score named `name` with `value` and no comment.
    ///
    /// # Errors
    ///
    /// [`ScoreError::EmptyName`] for a name that is empty after trimming,
    /// [`ScoreError::EmptyCategory`] for a category that is empty after trimming, and
    /// [`ScoreError::NonFiniteNumber`] for a NaN or infinite number.
    pub fn new(name: impl Into<String>, value: ScoreValue) -> Result<Score, ScoreError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(ScoreError::EmptyName);
        }
        match &value {
            ScoreValue::Category(c) if c.trim().is_empty() => {
                return Err(ScoreError::EmptyCategory);
            }
            ScoreValue::Number(n) if !n.is_finite() => return Err(ScoreError::NonFiniteNumber),
            _ => {}
        }
        Ok(Score {
            name,
            value,
            comment: None,
        })
    }

    /// The score with `comment`.
    pub fn with_comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    /// The name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The value.
    pub fn value(&self) -> &ScoreValue {
        &self.value
    }

    /// The comment, if any.
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }
}

/// The invocation a [`Score`] belongs to. Apps read the invocation id from the runner's events
/// (`Event::invocation_id`); the trace id is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScoreTarget {
    /// The app name (the runner's `app_name`).
    pub app_name: String,
    /// The user id of the session.
    pub user_id: String,
    /// The session id.
    pub session_id: String,
    /// The invocation id.
    pub invocation_id: String,
    /// The trace id (32 hex digits), when known.
    pub trace_id: Option<String>,
}

impl ScoreTarget {
    /// The invocation `invocation_id` of session `session_id` of `user_id` in `app_name`, with no
    /// trace id.
    pub fn invocation(
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        invocation_id: impl Into<String>,
    ) -> Self {
        ScoreTarget {
            app_name: app_name.into(),
            user_id: user_id.into(),
            session_id: session_id.into(),
            invocation_id: invocation_id.into(),
            trace_id: None,
        }
    }

    /// The target with the trace id `trace_id`.
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }
}

/// The `content` of a `FEEDBACK` row: `{"name", "value", "data_type", "comment"}`, `comment`
/// only when the score has one.
pub(crate) fn feedback_content(score: &Score) -> Value {
    let value = match &score.value {
        ScoreValue::Bool(b) => Value::Bool(*b),
        ScoreValue::Number(n) => Value::from(*n),
        ScoreValue::Category(c) => Value::from(c.as_str()),
    };
    let mut content = Map::new();
    content.insert("name".to_owned(), Value::from(score.name.as_str()));
    content.insert("value".to_owned(), value);
    content.insert("data_type".to_owned(), Value::from(score.value.data_type()));
    if let Some(comment) = &score.comment {
        content.insert("comment".to_owned(), Value::from(comment.as_str()));
    }
    Value::Object(content)
}

impl BigQueryAgentAnalyticsPlugin {
    /// Queues a `FEEDBACK` row scoring `target` with `score`, written by `author` (an end user's
    /// id, say). Never blocks and never fails: like the callbacks' rows it honours the event
    /// denylist and allowlist (an allowlist without `FEEDBACK` drops every score), a disabled
    /// plugin writes nothing, and after the plugin closed the row counts as a shutdown drop.
    ///
    /// The row has `event_type` `FEEDBACK`, the target's session, invocation and user ids (no
    /// agent), its trace id when given, `content` `{"name", "value", "data_type", "comment"}`, and
    /// `attributes` with `"source": "app"` and `"author"` next to the usual `adk` envelope.
    pub fn record_score(&self, target: &ScoreTarget, score: &Score, author: &str) {
        let Some(analytics) = &self.state.analytics else {
            return;
        };
        let id = Identity {
            agent_name: String::new(),
            session_id: target.session_id.clone(),
            invocation_id: target.invocation_id.clone(),
            user_id: target.user_id.clone(),
            app_name: target.app_name.clone(),
            ..Identity::default()
        };
        let trace_id = target.trace_id.clone().unwrap_or_default();
        let content = feedback_content(score);
        let mut span_fn = || RowSpan {
            trace_id: trace_id.clone(),
            ..RowSpan::default()
        };
        let mut build = |d: &mut EventData| {
            d.content = Some(Payload::Json(content.clone()));
            d.attributes.insert("source".to_owned(), Value::from("app"));
            d.attributes
                .insert("author".to_owned(), Value::from(author));
        };
        analytics.log_event(
            &CallbackCtx::default(),
            &id,
            EVENT_FEEDBACK,
            Some(&mut span_fn),
            Some(&mut build),
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn valid_scores_keep_their_name_value_and_comment() {
        let s = Score::new("thumbs", ScoreValue::Bool(true)).unwrap();
        assert_eq!(s.name(), "thumbs");
        assert_eq!(s.value(), &ScoreValue::Bool(true));
        assert_eq!(s.comment(), None);
        let s = Score::new("rating", ScoreValue::Number(4.5))
            .unwrap()
            .with_comment("good answer");
        assert_eq!(s.value(), &ScoreValue::Number(4.5));
        assert_eq!(s.comment(), Some("good answer"));
        let s = Score::new(" quality ", ScoreValue::Category("helpful".into())).unwrap();
        assert_eq!(s.name(), " quality ");
        assert_eq!(s.value().data_type(), "category");
    }

    #[test]
    fn invalid_scores_are_rejected() {
        let cases = [
            ("", ScoreValue::Bool(true), ScoreError::EmptyName),
            ("  ", ScoreValue::Number(1.0), ScoreError::EmptyName),
            (
                "quality",
                ScoreValue::Category(" \t".into()),
                ScoreError::EmptyCategory,
            ),
            (
                "rating",
                ScoreValue::Number(f64::NAN),
                ScoreError::NonFiniteNumber,
            ),
            (
                "rating",
                ScoreValue::Number(f64::INFINITY),
                ScoreError::NonFiniteNumber,
            ),
        ];
        for (name, value, want) in cases {
            assert_eq!(Score::new(name, value).unwrap_err(), want, "{name:?}");
        }
    }

    #[test]
    fn score_errors_read_as_text() {
        assert_eq!(ScoreError::EmptyName.to_string(), "score name is empty");
        assert_eq!(
            ScoreError::EmptyCategory.to_string(),
            "score category is empty"
        );
        assert_eq!(
            ScoreError::NonFiniteNumber.to_string(),
            "score number is not finite"
        );
        let boxed: Box<dyn std::error::Error> = Box::new(ScoreError::EmptyName);
        assert_eq!(boxed.to_string(), "score name is empty");
    }

    #[test]
    fn targets_name_the_invocation_and_optionally_the_trace() {
        let t = ScoreTarget::invocation("app", "user-1", "session-1", "e-123");
        assert_eq!(t.app_name, "app");
        assert_eq!(t.user_id, "user-1");
        assert_eq!(t.session_id, "session-1");
        assert_eq!(t.invocation_id, "e-123");
        assert_eq!(t.trace_id, None);
        let t = t.with_trace_id("0123456789abcdef0123456789abcdef");
        assert_eq!(
            t.trace_id.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn feedback_content_per_value_type() {
        let up = Score::new("thumbs", ScoreValue::Bool(true)).unwrap();
        assert_eq!(
            feedback_content(&up),
            json!({"name": "thumbs", "value": true, "data_type": "bool"})
        );
        let rating = Score::new("rating", ScoreValue::Number(4.0))
            .unwrap()
            .with_comment("ok");
        assert_eq!(
            feedback_content(&rating),
            json!({"name": "rating", "value": 4.0, "data_type": "number", "comment": "ok"})
        );
        let cat = Score::new("quality", ScoreValue::Category("helpful".into())).unwrap();
        assert_eq!(
            feedback_content(&cat),
            json!({"name": "quality", "value": "helpful", "data_type": "category"})
        );
    }
}

#[cfg(test)]
mod record_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::config::{Config, DropStats};
    use crate::context::Context;
    use crate::parity_tests::plugin_agentanalytics_bigquery_agent_analytics_plugin::support::{
        TestPlugin, json_object_column, new_test_plugin_with_config, rows_of_type,
        setup_test_plugin, str_col,
    };
    use crate::plugin::{
        as_bigquery_agent_analytics_plugin, new_bigquery_agent_analytics_plugin_with_config,
    };
    use crate::row::Row;

    const TRACE: &str = "0123456789abcdef0123456789abcdef";

    /// One filter case: name, config change, `FEEDBACK` rows written.
    type FilterCase = (&'static str, fn(&mut Config), usize);

    fn analytics(tp: &TestPlugin) -> BigQueryAgentAnalyticsPlugin {
        as_bigquery_agent_analytics_plugin(&tp.plugin).unwrap()
    }

    fn target() -> ScoreTarget {
        ScoreTarget::invocation("shop", "user-1", "session-1", "e-123")
    }

    fn thumbs_up() -> Score {
        Score::new("thumbs", ScoreValue::Bool(true)).unwrap()
    }

    /// The `FEEDBACK` rows the server received once the plugin closed.
    async fn feedback_rows(tp: &TestPlugin) -> Vec<Row> {
        tp.close().await.unwrap();
        rows_of_type(&tp.rows(), EVENT_FEEDBACK)
    }

    /// The row whose `content.name` is `name`.
    fn named<'a>(rows: &'a [Row], name: &str) -> &'a Row {
        rows.iter()
            .find(|r| json_object_column(r, "content")["name"] == json!(name))
            .unwrap_or_else(|| panic!("no {name} row"))
    }

    #[tokio::test]
    async fn record_score_writes_one_feedback_row_per_score() {
        let tp = setup_test_plugin().await;
        let a = analytics(&tp);
        a.record_score(&target().with_trace_id(TRACE), &thumbs_up(), "user-1");
        let rating = Score::new("rating", ScoreValue::Number(4.5))
            .unwrap()
            .with_comment("fast");
        a.record_score(&target(), &rating, "user-1");
        let quality = Score::new("quality", ScoreValue::Category("helpful".into())).unwrap();
        a.record_score(&target(), &quality, "reviewer");
        let rows = feedback_rows(&tp).await;
        assert_eq!(rows.len(), 3);

        let up = named(&rows, "thumbs");
        assert_eq!(
            json_object_column(up, "content"),
            json!({"name": "thumbs", "value": true, "data_type": "bool"})
        );
        assert_eq!(str_col(up, "event_type"), EVENT_FEEDBACK);
        assert_eq!(str_col(up, "trace_id"), TRACE);
        assert_eq!(str_col(up, "invocation_id"), "e-123");
        assert_eq!(str_col(up, "session_id"), "session-1");
        assert_eq!(str_col(up, "user_id"), "user-1");
        assert_eq!(str_col(up, "agent"), "");
        assert_eq!(str_col(up, "status"), "OK");
        let attrs = json_object_column(up, "attributes");
        assert_eq!(attrs["source"], json!("app"));
        assert_eq!(attrs["author"], json!("user-1"));
        assert_eq!(attrs["adk"]["app_name"], json!("shop"));
        assert_eq!(attrs["adk"]["invocation_id"], json!("e-123"));
        assert_eq!(attrs["adk"]["schema_version"], json!("1"));

        let r = named(&rows, "rating");
        assert_eq!(
            json_object_column(r, "content"),
            json!({"name": "rating", "value": 4.5, "data_type": "number", "comment": "fast"})
        );
        assert_eq!(str_col(r, "trace_id"), "", "no trace id was given");

        let q = named(&rows, "quality");
        assert_eq!(
            json_object_column(q, "content"),
            json!({"name": "quality", "value": "helpful", "data_type": "category"})
        );
        assert_eq!(
            json_object_column(q, "attributes")["author"],
            json!("reviewer")
        );
    }

    #[tokio::test]
    async fn record_score_honours_the_event_filters() {
        let cases: [FilterCase; 3] = [
            (
                "denylisted",
                |c| c.event_denylist = vec![EVENT_FEEDBACK.into()],
                0,
            ),
            (
                "not allowlisted",
                |c| c.event_allowlist = vec![crate::event_types::EVENT_LLM_REQUEST.into()],
                0,
            ),
            (
                "allowlisted",
                |c| c.event_allowlist = vec![EVENT_FEEDBACK.into()],
                1,
            ),
        ];
        for (name, mutate, want) in cases {
            let tp = new_test_plugin_with_config(mutate).await;
            analytics(&tp).record_score(&target(), &thumbs_up(), "user-1");
            assert_eq!(feedback_rows(&tp).await.len(), want, "{name}");
        }
    }

    #[tokio::test]
    async fn a_disabled_plugin_records_nothing() {
        let p = new_bigquery_agent_analytics_plugin_with_config(
            &Context::background(),
            Config {
                enabled: false,
                ..Config::default()
            },
        )
        .await
        .unwrap();
        let a = as_bigquery_agent_analytics_plugin(&p).unwrap();
        a.record_score(&target(), &thumbs_up(), "user-1");
        assert_eq!(a.drop_stats(), DropStats::default());
    }

    #[tokio::test]
    async fn a_score_after_close_is_a_shutdown_drop() {
        let tp = setup_test_plugin().await;
        tp.close().await.unwrap();
        let a = analytics(&tp);
        a.record_score(&target(), &thumbs_up(), "user-1");
        assert_eq!(a.drop_stats().shutdown_drops, 1);
        assert!(rows_of_type(&tp.rows(), EVENT_FEEDBACK).is_empty());
    }
}
