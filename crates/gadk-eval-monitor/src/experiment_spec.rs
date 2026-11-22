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

//! Experiment specs: one Gemini LLM agent on Vertex AI, run against an eval set once per
//! variant by `gadk-eval experiment`.
//!
//! ```yaml
//! name: answer-style                 # [A-Za-z0-9_-]{1,64}; names the stored report
//! set: sets/smoke.yaml               # eval-set file; relative to this spec file's directory
//! project: my-proj                   # Vertex AI project
//! location: us                       # default us
//! runs: 1                            # runs per case, 1-20, default 1
//! prompts: file://prompts            # prompt store: file://<dir> or
//!                                    #   firestore://<project>/<database>[/<root>]; required
//!                                    #   if and only if agent.prompt is set; a relative
//!                                    #   file:// directory is relative to this spec file
//! model: gemini-3.8-flash            # default model for variants without one
//! judge:                             # optional; keys model, project, location (no timeout);
//!   model: gemini-3.8-flash          #   project defaults to `project`, location to
//!                                    #   `location`; without `judge`, the judge is the
//!                                    #   first variant's model
//! agent:
//!   name: assistant                  # [A-Za-z_][A-Za-z0-9_]{0,63}, not `user`
//!   description: Answers questions.  # optional, default ""
//!   prompt: exp-assistant            # exactly one of `prompt` (a prompt name) and
//!                                    #   `instruction` (the instruction text)
//! variants:                          # 2-10 variants with unique names ([A-Za-z0-9_-]{1,64})
//!   - name: flash-v1
//!     model: gemini-3.8-flash        # optional when the top-level `model` is set
//!     prompt: { version: 1 }         # optional, only with agent.prompt; or { label: production };
//!                                    #   without it the variant uses the latest version
//!   - name: hydrogen-v1
//!     model: gemini-4-hydrogen
//!     prompt: { version: 1 }
//! ```
//!
//! Errors name the offending key; variants are numbered from 1 (`variants[2].model`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gadk_eval::Variant;
use gadk_prompts::{Label, Selector, validate_name};
use serde_json::Value;

use crate::error::MonitorError;
use crate::spec::{
    DEFAULT_JUDGE_LOCATION, DEFAULT_JUDGE_TIMEOUT, JudgeSpec, check_keys, is_monitor_name,
    is_project_id, parse_judge, required, string_field, yaml_value,
};

/// The default `runs`.
pub const DEFAULT_EXPERIMENT_RUNS: u32 = 1;
/// The largest `runs`.
pub const MAX_EXPERIMENT_RUNS: u32 = 20;
/// The fewest variants an experiment spec may list.
pub const MIN_VARIANTS: usize = 2;
/// The most variants an experiment spec may list.
pub const MAX_VARIANTS: usize = 10;

/// A validated experiment spec, with every default filled in.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ExperimentSpec {
    /// The experiment name (`[A-Za-z0-9_-]{1,64}`); the stored report is named after it.
    pub name: String,
    /// The eval-set file: as written in [`from_yaml`](Self::from_yaml), joined to the spec
    /// file's directory by [`load`](Self::load).
    pub set: PathBuf,
    /// The Vertex AI project of the agents' models (`[A-Za-z0-9_-]+`).
    pub project: String,
    /// The Vertex AI location of the agents' models (default `us`).
    pub location: String,
    /// Runs per case and variant (1-20, default 1).
    pub runs: u32,
    /// The prompt store URL (`file://<dir>` or `firestore://…`); set exactly when the agent's
    /// instruction is a prompt. [`load`](Self::load) joins a relative `file://` directory to the
    /// spec file's directory.
    pub prompts: Option<String>,
    /// The judge model, with its project and location defaults filled in.
    pub judge: JudgeSpec,
    /// The agent every variant runs.
    pub agent: ExperimentAgent,
    /// The variants in spec order, each with its model set ([`Variant::model`]); under
    /// [`AgentInstruction::Prompt`] each also has a prompt selector ([`Selector::Latest`] when
    /// the variant names none).
    pub variants: Vec<Variant>,
}

/// The LLM agent of an experiment spec.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ExperimentAgent {
    /// The agent name (`[A-Za-z_][A-Za-z0-9_]{0,63}`, not `user`).
    pub name: String,
    /// The agent description (default empty).
    pub description: String,
    /// Where the agent's instruction comes from.
    pub instruction: AgentInstruction,
}

/// Where an experiment agent's instruction comes from.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentInstruction {
    /// This instruction text, the same for every variant.
    Text(String),
    /// The prompt with this name in the spec's prompt store, at each variant's selector.
    Prompt(String),
}

impl ExperimentSpec {
    /// Reads and validates the experiment spec at `path`, then joins `set` (and a relative
    /// `file://` `prompts` directory) to the directory of `path`.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Spec`] (message prefix `experiment spec: `) when the file cannot be read
    /// or the spec is invalid.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, MonitorError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            MonitorError::Spec(format!("experiment spec: reading {}: {e}", path.display()))
        })?;
        let mut spec = Self::from_yaml(&text)?;
        let dir = path.parent().unwrap_or_else(|| Path::new(""));
        spec.set = dir.join(&spec.set);
        if let Some(url) = &spec.prompts
            && let Some(rest) = url.strip_prefix("file://")
            && Path::new(rest).is_relative()
        {
            spec.prompts = Some(format!("file://{}", dir.join(rest).display()));
        }
        Ok(spec)
    }

    /// Parses and validates an experiment spec from YAML text; `set` and `prompts` stay as
    /// written.
    ///
    /// # Errors
    ///
    /// [`MonitorError::Spec`] (message prefix `experiment spec: `) naming the offending key.
    pub fn from_yaml(yaml: &str) -> Result<Self, MonitorError> {
        yaml_value(yaml)
            .and_then(|doc| parse(&doc))
            .map_err(|e| MonitorError::Spec(format!("experiment spec: {e}")))
    }
}

/// Whether `s` is a usable agent name: `[A-Za-z_][A-Za-z0-9_]{0,63}`.
fn is_agent_name(s: &str) -> bool {
    s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether `s` is a usable Vertex AI location: one or more of `[A-Za-z0-9-]`.
fn is_location(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn parse(doc: &Value) -> Result<ExperimentSpec, String> {
    let top = doc.as_object().ok_or("the document must be a mapping")?;
    check_keys(
        top,
        "",
        &[
            "name", "set", "project", "location", "runs", "prompts", "model", "judge", "agent",
            "variants",
        ],
    )?;
    let name = required(
        top,
        "name",
        is_monitor_name,
        "`name` must match [A-Za-z0-9_-]{1,64}",
    )?;
    let set = required(
        top,
        "set",
        |s| !s.trim().is_empty(),
        "`set` must be a non-empty path",
    )?;
    let project = required(
        top,
        "project",
        is_project_id,
        "`project` must match [A-Za-z0-9_-]+",
    )?;
    let location = string_field(
        top,
        "location",
        is_location,
        "`location` must match [A-Za-z0-9-]+",
    )?
    .unwrap_or_else(|| DEFAULT_JUDGE_LOCATION.to_owned());
    let runs = match top.get("runs") {
        None | Some(Value::Null) => DEFAULT_EXPERIMENT_RUNS,
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| (1..=MAX_EXPERIMENT_RUNS).contains(n))
            .ok_or("`runs` must be an integer from 1 to 20")?,
    };
    let model = string_field(
        top,
        "model",
        |s| !s.trim().is_empty(),
        "`model` must be a non-empty string",
    )?;
    let agent = parse_agent(top.get("agent"))?;
    let prompt = matches!(agent.instruction, AgentInstruction::Prompt(_));
    let prompts = string_field(
        top,
        "prompts",
        |s| s.starts_with("file://") || s.starts_with("firestore://"),
        "`prompts` must be a file://<dir> or firestore://<project>/<database>[/<root>] URL",
    )?;
    match (&prompts, prompt) {
        (None, true) => return Err("`agent.prompt` needs `prompts` (a prompt store URL)".into()),
        (Some(_), false) => return Err("`prompts` is set but `agent.prompt` is not".into()),
        _ => {}
    }
    let variants = parse_variants(top.get("variants"), model.as_deref(), prompt)?;
    let judge = match top.get("judge") {
        None | Some(Value::Null) => JudgeSpec {
            model: variants[0].model().unwrap_or_default().to_owned(),
            project: project.clone(),
            location: location.clone(),
            timeout: DEFAULT_JUDGE_TIMEOUT,
        },
        Some(v) => {
            let m = v.as_object().ok_or("`judge` must be a mapping")?;
            check_keys(m, "`judge`: ", &["model", "project", "location"])?;
            let mut judge = parse_judge(v, &project)?;
            if matches!(m.get("location"), None | Some(Value::Null)) {
                judge.location = location.clone();
            }
            judge
        }
    };
    Ok(ExperimentSpec {
        name,
        set: PathBuf::from(set),
        project,
        location,
        runs,
        prompts,
        judge,
        agent,
        variants,
    })
}

fn parse_agent(v: Option<&Value>) -> Result<ExperimentAgent, String> {
    let m = v
        .and_then(Value::as_object)
        .ok_or("`agent` must be a mapping")?;
    check_keys(
        m,
        "`agent`: ",
        &["name", "description", "prompt", "instruction"],
    )?;
    let name = required(
        m,
        "name",
        is_agent_name,
        "`agent.name` must match [A-Za-z_][A-Za-z0-9_]{0,63}",
    )?;
    if name == "user" {
        return Err("`agent.name` must not be `user` (it is reserved for end-user input)".into());
    }
    let description = string_field(
        m,
        "description",
        |_| true,
        "`agent.description` must be a string",
    )?
    .unwrap_or_default();
    let instruction = match (m.get("prompt"), m.get("instruction")) {
        (Some(p), None) => {
            let p = p.as_str().ok_or("`agent.prompt` must be a prompt name")?;
            validate_name(p).map_err(|e| format!("`agent.prompt`: {e}"))?;
            AgentInstruction::Prompt(p.to_owned())
        }
        (None, Some(i)) => match i.as_str() {
            Some(text) if !text.trim().is_empty() => AgentInstruction::Text(text.to_owned()),
            _ => return Err("`agent.instruction` must be a non-empty string".into()),
        },
        _ => return Err("`agent` must set exactly one of `prompt` and `instruction`".into()),
    };
    Ok(ExperimentAgent {
        name,
        description,
        instruction,
    })
}

/// The variants, each with its resolved model and, when `prompt`, its selector.
fn parse_variants(
    v: Option<&Value>,
    default_model: Option<&str>,
    prompt: bool,
) -> Result<Vec<Variant>, String> {
    let list = match v {
        Some(Value::Array(a)) if (MIN_VARIANTS..=MAX_VARIANTS).contains(&a.len()) => a,
        _ => return Err("`variants` must be a list of 2 to 10 variants".into()),
    };
    let mut seen = HashSet::new();
    let mut variants = Vec::with_capacity(list.len());
    for (i, v) in list.iter().enumerate() {
        let at = format!("variants[{}]", i + 1);
        let m = v
            .as_object()
            .ok_or_else(|| format!("`{at}` must be a mapping"))?;
        check_keys(m, &format!("`{at}`: "), &["name", "model", "prompt"])?;
        let name = required(
            m,
            "name",
            is_monitor_name,
            &format!("`{at}.name` must match [A-Za-z0-9_-]{{1,64}}"),
        )?;
        if !seen.insert(name.clone()) {
            return Err(format!("`{at}.name`: duplicate variant name {name:?}"));
        }
        let model = string_field(
            m,
            "model",
            |s| !s.trim().is_empty(),
            &format!("`{at}.model` must be a non-empty string"),
        )?
        .or_else(|| default_model.map(str::to_owned))
        .ok_or_else(|| format!("`{at}.model` is required when there is no top-level `model`"))?;
        let selector = match m.get("prompt") {
            None | Some(Value::Null) => None,
            Some(_) if !prompt => return Err(format!("`{at}.prompt` needs `agent.prompt`")),
            Some(p) => Some(parse_selector(p, &at)?),
        };
        let variant = if prompt {
            Variant::prompt(name, selector.unwrap_or(Selector::Latest))
        } else {
            Variant::new(name)
        };
        variants.push(variant.with_model(model));
    }
    Ok(variants)
}

/// `{ version: <n> }` (n >= 1) or `{ label: <label> }` at `<at>.prompt`.
fn parse_selector(v: &Value, at: &str) -> Result<Selector, String> {
    let m = v
        .as_object()
        .ok_or_else(|| format!("`{at}.prompt` must be a mapping"))?;
    check_keys(m, &format!("`{at}.prompt`: "), &["version", "label"])?;
    match (m.get("version"), m.get("label")) {
        (Some(n), None) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n >= 1)
            .map(Selector::Version)
            .ok_or_else(|| format!("`{at}.prompt.version` must be an integer from 1")),
        (None, Some(l)) => {
            let l = l
                .as_str()
                .ok_or_else(|| format!("`{at}.prompt.label` must be a string"))?;
            Label::new(l)
                .map(Selector::Label)
                .map_err(|e| format!("`{at}.prompt.label`: {e}"))
        }
        _ => Err(format!(
            "`{at}.prompt` must set exactly one of `version` and `label`"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const FULL: &str = r#"
name: answer-style
set: sets/smoke.yaml
project: my-proj
location: europe-west4
runs: 3
prompts: file://prompts
model: gemini-3.8-flash
judge:
  model: gemini-judge
  project: judge-proj
  location: us-central1
agent:
  name: assistant
  description: Answers questions.
  prompt: exp-assistant
variants:
  - name: flash-v1
    prompt: { version: 1 }
  - name: hydrogen-prod
    model: gemini-4-hydrogen
    prompt: { label: production }
  - name: flash-latest
"#;

    /// A valid instruction spec, one top-level line per key.
    const TEXT: &str = "name: exp\nset: s.yaml\nproject: p\nmodel: m1\n\
                        agent: { name: assistant, instruction: Be brief. }\n\
                        variants: [{ name: a }, { name: b }]\n";

    /// A valid prompt spec, one top-level line per key.
    const PROMPT: &str = "name: exp\nset: s.yaml\nproject: p\nmodel: m1\nprompts: file://prompts\n\
                          agent: { name: assistant, prompt: exp-assistant }\n\
                          variants: [{ name: a }, { name: b }]\n";

    /// `base` with its top-level `field` line set to `value` (removed when `value` is empty,
    /// appended when `base` has none).
    fn with(base: &str, field: &str, value: &str) -> String {
        let key = format!("{field}:");
        let mut lines: Vec<String> = base
            .lines()
            .filter(|l| !l.starts_with(&key))
            .map(str::to_owned)
            .collect();
        if !value.is_empty() {
            lines.push(format!("{field}: {value}"));
        }
        lines.join("\n")
    }

    fn spec_err(yaml: &str) -> String {
        match ExperimentSpec::from_yaml(yaml) {
            Err(MonitorError::Spec(m)) => m,
            other => panic!("expected a spec error, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_full_spec() {
        let s = ExperimentSpec::from_yaml(FULL).unwrap();
        assert_eq!(s.name, "answer-style");
        assert_eq!(s.set, PathBuf::from("sets/smoke.yaml"));
        assert_eq!(s.project, "my-proj");
        assert_eq!(s.location, "europe-west4");
        assert_eq!(s.runs, 3);
        assert_eq!(s.prompts.as_deref(), Some("file://prompts"));
        assert_eq!(
            s.judge,
            JudgeSpec {
                model: "gemini-judge".into(),
                project: "judge-proj".into(),
                location: "us-central1".into(),
                timeout: DEFAULT_JUDGE_TIMEOUT,
            }
        );
        assert_eq!(s.agent.name, "assistant");
        assert_eq!(s.agent.description, "Answers questions.");
        assert_eq!(
            s.agent.instruction,
            AgentInstruction::Prompt("exp-assistant".into())
        );
        let variants: Vec<(&str, Option<&str>, Option<&Selector>)> = s
            .variants
            .iter()
            .map(|v| (v.name(), v.model(), v.selector()))
            .collect();
        assert_eq!(
            variants,
            [
                (
                    "flash-v1",
                    Some("gemini-3.8-flash"),
                    Some(&Selector::Version(1))
                ),
                (
                    "hydrogen-prod",
                    Some("gemini-4-hydrogen"),
                    Some(&Selector::Label(Label::production()))
                ),
                (
                    "flash-latest",
                    Some("gemini-3.8-flash"),
                    Some(&Selector::Latest)
                ),
            ]
        );
    }

    #[test]
    fn applies_defaults() {
        let s = ExperimentSpec::from_yaml(TEXT).unwrap();
        assert_eq!(s.location, "us");
        assert_eq!(s.runs, DEFAULT_EXPERIMENT_RUNS);
        assert_eq!(s.prompts, None);
        assert_eq!(s.agent.description, "");
        assert_eq!(
            s.agent.instruction,
            AgentInstruction::Text("Be brief.".into())
        );
        // Without `judge`, the first variant's model judges, in the experiment's project and
        // location.
        assert_eq!(
            s.judge,
            JudgeSpec {
                model: "m1".into(),
                project: "p".into(),
                location: "us".into(),
                timeout: Duration::from_secs(120),
            }
        );
        let variants: Vec<(&str, Option<&str>, Option<&Selector>)> = s
            .variants
            .iter()
            .map(|v| (v.name(), v.model(), v.selector()))
            .collect();
        assert_eq!(variants, [("a", Some("m1"), None), ("b", Some("m1"), None)]);
    }

    #[test]
    fn the_judge_defaults_to_the_experiment_project_and_location() {
        let base = with(TEXT, "location", "europe-west4");
        let s = ExperimentSpec::from_yaml(&with(
            &base,
            "variants",
            "[{ name: a, model: m2 }, { name: b }]",
        ))
        .unwrap();
        assert_eq!(
            (
                s.judge.model.as_str(),
                s.judge.project.as_str(),
                s.judge.location.as_str()
            ),
            ("m2", "p", "europe-west4")
        );
        let s = ExperimentSpec::from_yaml(&with(&base, "judge", "{ model: j }")).unwrap();
        assert_eq!(
            (
                s.judge.model.as_str(),
                s.judge.project.as_str(),
                s.judge.location.as_str()
            ),
            ("j", "p", "europe-west4")
        );
        let s = ExperimentSpec::from_yaml(&with(
            &base,
            "judge",
            "{ model: j, project: q, location: us-east1 }",
        ))
        .unwrap();
        assert_eq!(
            (
                s.judge.model.as_str(),
                s.judge.project.as_str(),
                s.judge.location.as_str()
            ),
            ("j", "q", "us-east1")
        );
        assert_eq!(s.judge.timeout, DEFAULT_JUDGE_TIMEOUT);
    }

    #[test]
    fn prompt_variants_without_a_selector_use_the_latest_version() {
        let s = ExperimentSpec::from_yaml(PROMPT).unwrap();
        let selectors: Vec<Option<&Selector>> = s.variants.iter().map(Variant::selector).collect();
        assert_eq!(
            selectors,
            [Some(&Selector::Latest), Some(&Selector::Latest)]
        );
        let eleven = (1..=11)
            .map(|i| format!("{{ name: v{i} }}"))
            .collect::<Vec<_>>()
            .join(", ");
        let ten = (1..=10)
            .map(|i| format!("{{ name: v{i} }}"))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            ExperimentSpec::from_yaml(&with(TEXT, "variants", &format!("[{ten}]")))
                .unwrap()
                .variants
                .len(),
            MAX_VARIANTS
        );
        assert!(
            spec_err(&with(TEXT, "variants", &format!("[{eleven}]")))
                .contains("`variants` must be a list of 2 to 10 variants")
        );
    }

    #[test]
    fn every_invalid_field_is_named() {
        let cases = [
            (with(TEXT, "bogus", "1"), "unknown key `bogus`"),
            (with(TEXT, "name", ""), "`name` must match"),
            (with(TEXT, "name", "\"a b\""), "`name` must match"),
            (with(TEXT, "set", ""), "`set` must be a non-empty path"),
            (with(TEXT, "set", "\" \""), "`set` must be a non-empty path"),
            (with(TEXT, "project", ""), "`project` must match"),
            (with(TEXT, "project", "\"a b\""), "`project` must match"),
            (with(TEXT, "location", "\"\""), "`location` must match"),
            (with(TEXT, "location", "\"us/x\""), "`location` must match"),
            (
                with(TEXT, "runs", "0"),
                "`runs` must be an integer from 1 to 20",
            ),
            (with(TEXT, "runs", "21"), "`runs`"),
            (with(TEXT, "runs", "1.5"), "`runs`"),
            (with(TEXT, "runs", "two"), "`runs`"),
            (
                with(TEXT, "model", "\" \""),
                "`model` must be a non-empty string",
            ),
            (with(TEXT, "agent", ""), "`agent` must be a mapping"),
            (
                with(TEXT, "agent", "assistant"),
                "`agent` must be a mapping",
            ),
            (
                with(TEXT, "agent", "{ name: a, instruction: x, tools: [] }"),
                "`agent`: unknown key `tools`",
            ),
            (
                with(TEXT, "agent", "{ instruction: x }"),
                "`agent.name` must match",
            ),
            (
                with(TEXT, "agent", "{ name: 1abc, instruction: x }"),
                "`agent.name` must match",
            ),
            (
                with(TEXT, "agent", "{ name: user, instruction: x }"),
                "`agent.name` must not be `user`",
            ),
            (
                with(
                    TEXT,
                    "agent",
                    "{ name: a, description: [x], instruction: x }",
                ),
                "`agent.description` must be a string",
            ),
            (
                with(TEXT, "agent", "{ name: a }"),
                "`agent` must set exactly one of `prompt` and `instruction`",
            ),
            (
                with(PROMPT, "agent", "{ name: a, prompt: p, instruction: x }"),
                "`agent` must set exactly one of `prompt` and `instruction`",
            ),
            (
                with(TEXT, "agent", "{ name: a, instruction: \" \" }"),
                "`agent.instruction` must be a non-empty string",
            ),
            (
                with(PROMPT, "agent", "{ name: a, prompt: Bad_Name }"),
                "`agent.prompt`: invalid: prompt name \"Bad_Name\"",
            ),
            (
                with(PROMPT, "prompts", ""),
                "`agent.prompt` needs `prompts`",
            ),
            (
                with(TEXT, "prompts", "file://prompts"),
                "`prompts` is set but `agent.prompt` is not",
            ),
            (
                with(PROMPT, "prompts", "s3://bucket"),
                "`prompts` must be a file://",
            ),
            (
                with(TEXT, "judge", "{ model: j, timeout: 30s }"),
                "`judge`: unknown key `timeout` (expected one of: model, project, location)",
            ),
            (with(TEXT, "judge", "gemini"), "`judge` must be a mapping"),
            (with(TEXT, "judge", "{ project: q }"), "`judge.model`"),
            (
                with(TEXT, "judge", "{ model: j, location: \"us/x\" }"),
                "`judge.location`",
            ),
            (
                with(TEXT, "variants", ""),
                "`variants` must be a list of 2 to 10 variants",
            ),
            (
                with(TEXT, "variants", "[{ name: a }]"),
                "`variants` must be a list of 2 to 10 variants",
            ),
            (
                with(TEXT, "variants", "[a, b]"),
                "`variants[1]` must be a mapping",
            ),
            (
                with(TEXT, "variants", "[{ name: a, tools: [] }, { name: b }]"),
                "`variants[1]`: unknown key `tools`",
            ),
            (
                with(TEXT, "variants", "[{ name: a }, { name: a }]"),
                "`variants[2].name`: duplicate variant name \"a\"",
            ),
            (
                with(TEXT, "variants", "[{ name: a }, { name: \"b c\" }]"),
                "`variants[2].name` must match",
            ),
            (
                with(TEXT, "variants", "[{ name: a }, {}]"),
                "`variants[2].name` must match",
            ),
            (
                with(TEXT, "model", ""),
                "`variants[1].model` is required when there is no top-level `model`",
            ),
            (
                with(TEXT, "variants", "[{ name: a, model: \"\" }, { name: b }]"),
                "`variants[1].model` must be a non-empty string",
            ),
            (
                with(
                    TEXT,
                    "variants",
                    "[{ name: a, prompt: { version: 1 } }, { name: b }]",
                ),
                "`variants[1].prompt` needs `agent.prompt`",
            ),
            (
                with(PROMPT, "variants", "[{ name: a, prompt: 1 }, { name: b }]"),
                "`variants[1].prompt` must be a mapping",
            ),
            (
                with(
                    PROMPT,
                    "variants",
                    "[{ name: a, prompt: { tag: x } }, { name: b }]",
                ),
                "`variants[1].prompt`: unknown key `tag`",
            ),
            (
                with(PROMPT, "variants", "[{ name: a, prompt: {} }, { name: b }]"),
                "`variants[1].prompt` must set exactly one of `version` and `label`",
            ),
            (
                with(
                    PROMPT,
                    "variants",
                    "[{ name: a, prompt: { version: 1, label: production } }, { name: b }]",
                ),
                "`variants[1].prompt` must set exactly one of `version` and `label`",
            ),
            (
                with(
                    PROMPT,
                    "variants",
                    "[{ name: a }, { name: b, prompt: { version: 0 } }]",
                ),
                "`variants[2].prompt.version` must be an integer from 1",
            ),
            (
                with(
                    PROMPT,
                    "variants",
                    "[{ name: a, prompt: { label: Prod } }, { name: b }]",
                ),
                "`variants[1].prompt.label`: invalid: label \"Prod\"",
            ),
            (
                with(
                    PROMPT,
                    "variants",
                    "[{ name: a, prompt: { label: latest } }, { name: b }]",
                ),
                "`variants[1].prompt.label`: invalid: label \"latest\" is reserved",
            ),
            ("[1, 2]".to_owned(), "the document must be a mapping"),
            ("name: [".to_owned(), "invalid YAML"),
        ];
        for (yaml, needle) in cases {
            let msg = spec_err(&yaml);
            assert!(msg.starts_with("experiment spec: "), "{yaml}\n=> {msg}");
            assert!(msg.contains(needle), "{yaml}\n=> {msg}");
        }
    }

    #[test]
    fn load_resolves_paths_relative_to_the_spec_file() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("exp");
        std::fs::create_dir(&sub).unwrap();
        let path = sub.join("e.yaml");
        std::fs::write(&path, with(PROMPT, "set", "sets/smoke.yaml")).unwrap();
        let s = ExperimentSpec::load(&path).unwrap();
        assert_eq!(s.set, sub.join("sets/smoke.yaml"));
        assert_eq!(
            s.prompts,
            Some(format!("file://{}", sub.join("prompts").display()))
        );

        let abs = dir.path().join("abs-prompts");
        let yaml = with(
            &with(PROMPT, "set", &abs.join("s.yaml").display().to_string()),
            "prompts",
            &format!("file://{}", abs.display()),
        );
        std::fs::write(&path, yaml).unwrap();
        let s = ExperimentSpec::load(&path).unwrap();
        assert_eq!(s.set, abs.join("s.yaml"));
        assert_eq!(s.prompts, Some(format!("file://{}", abs.display())));

        std::fs::write(&path, with(PROMPT, "prompts", "firestore://proj/(default)")).unwrap();
        let s = ExperimentSpec::load(&path).unwrap();
        assert_eq!(s.prompts.as_deref(), Some("firestore://proj/(default)"));

        let missing = dir.path().join("missing.yaml");
        assert!(matches!(
            ExperimentSpec::load(&missing),
            Err(MonitorError::Spec(m)) if m.starts_with("experiment spec: reading ")
        ));
    }
}
