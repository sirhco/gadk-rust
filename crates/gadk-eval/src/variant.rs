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

//! Named variants of the agent under test.

use std::collections::HashSet;

use gadk_prompts::Selector;

use crate::error::EvalError;

/// One configuration of the agent under test, passed to the `build` closure of
/// [`crate::Evaluator::run`]. Typically it names a prompt-registry [`Selector`]; the closure may
/// also use the name for other swaps (model, tools, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    name: String,
    selector: Option<Selector>,
}

impl Variant {
    /// A variant with no prompt selector.
    pub fn new(name: impl Into<String>) -> Self {
        Variant {
            name: name.into(),
            selector: None,
        }
    }

    /// A variant for the prompt version `selector` picks.
    pub fn prompt(name: impl Into<String>, selector: Selector) -> Self {
        Variant {
            name: name.into(),
            selector: Some(selector),
        }
    }

    /// The variant name (unique within a run).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The prompt selector, if any.
    pub fn selector(&self) -> Option<&Selector> {
        self.selector.as_ref()
    }
}

/// At least one variant; names non-empty (after trimming) and unique.
pub(crate) fn check_variants(variants: &[Variant]) -> Result<(), EvalError> {
    if variants.is_empty() {
        return Err(EvalError::Variant(
            "at least one variant is required".into(),
        ));
    }
    let mut seen = HashSet::new();
    for v in variants {
        if v.name.trim().is_empty() {
            return Err(EvalError::Variant("variant names must be non-empty".into()));
        }
        if !seen.insert(v.name.as_str()) {
            return Err(EvalError::Variant(format!(
                "duplicate variant name {:?}",
                v.name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gadk_prompts::Label;

    #[test]
    fn constructors_and_accessors() {
        let v = Variant::prompt("prod", Selector::Label(Label::production()));
        assert_eq!(v.name(), "prod");
        assert_eq!(v.selector(), Some(&Selector::Label(Label::production())));
        assert_eq!(Variant::new("plain").selector(), None);
    }

    #[test]
    fn names_must_be_present_non_empty_and_unique() {
        assert!(
            check_variants(&[
                Variant::new("a"),
                Variant::prompt("b", Selector::Version(7))
            ])
            .is_ok()
        );
        for (vs, needle) in [
            (vec![], "at least one"),
            (vec![Variant::new(" ")], "non-empty"),
            (
                vec![Variant::new("a"), Variant::new("a")],
                "duplicate variant name \"a\"",
            ),
        ] {
            let e = check_variants(&vs).unwrap_err().to_string();
            assert!(e.contains(needle), "{e}");
        }
    }
}
