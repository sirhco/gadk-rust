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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

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
}
