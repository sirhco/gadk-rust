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

//! The crate error type.

/// Errors from loading an eval set, validating variants or writing a report.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// The eval set is invalid. `case` names the offending case (its id, or `#<n>` 1-based when
    /// it has no usable id); `None` for set-level problems.
    #[error("{}", load_text(.case, .message))]
    Load {
        /// The offending case, if any.
        case: Option<String>,
        /// What is wrong.
        message: String,
    },
    /// Reading an eval set or writing a report failed.
    #[error("{0}")]
    Io(String),
    /// The variants passed to `Evaluator::run` are invalid.
    #[error("variants: {0}")]
    Variant(String),
}

impl EvalError {
    /// A [`EvalError::Load`] for `case` (`None` for set-level errors).
    pub(crate) fn load(case: Option<&str>, message: impl Into<String>) -> Self {
        EvalError::Load {
            case: case.map(str::to_owned),
            message: message.into(),
        }
    }
}

fn load_text(case: &Option<String>, message: &str) -> String {
    match case {
        Some(c) => format!("eval set: case {c}: {message}"),
        None => format!("eval set: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_display_names_the_case() {
        assert_eq!(
            EvalError::load(Some("refund"), "zero turns").to_string(),
            "eval set: case refund: zero turns"
        );
        assert_eq!(
            EvalError::load(None, "missing `name`").to_string(),
            "eval set: missing `name`"
        );
    }
}
