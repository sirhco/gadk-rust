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

//! A two-variant experiment: one eval set run against two versions of a prompt, saved to a
//! report store for the console's Experiments pages.
//!
//! It creates (if absent) versions 1 and 2 of the prompt `exp-assistant` in a file prompt store,
//! with different instructions; loads a three-case eval set (`contains` and `llm_judge`
//! scorers); runs it with `run_experiment("exp-e2e", …)` on `gemini-3.8-flash` (Vertex AI,
//! location `us`) as both agent and judge, with variant `v1` on prompt version 1 and `v2` on
//! version 2 (each recording the model with `Variant::with_model`); stores the result with
//! `ReportStore::put_experiment`; and prints the summary and the stored name.
//!
//! Without Rust code, `gadk-eval experiment <spec.yaml>` runs the same kind of experiment from
//! a YAML spec (see the crate README).
//!
//! `GOOGLE_CLOUD_PROJECT=<project> GADK_EXPERIMENT_PROMPTS=<dir> GADK_EXPERIMENT_OUT=<url> \
//!   cargo run -p gadk-eval-monitor --example experiment`
//!
//! `GADK_EXPERIMENT_OUT` is a report-store URL (a directory, `file://<dir>` or
//! `gs://<bucket>[/<prefix>]`); the experiment lands under its `experiments/`. Uses Application
//! Default Credentials for Gemini on Vertex AI (and for a `gs://` store). View it with
//! `gadk-console --project <project> --reports <url>` at `/experiments`.

use std::error::Error;
use std::sync::Arc;

use gadk_engine::llm::{LlmAgentConfig, agent as llm_agent};
use gadk_eval::{EvalSet, Evaluator, Variant};
use gadk_eval_monitor::{ReportStore, run_experiment};
use gadk_gemini::{ClientConfig, GeminiModel};
use gadk_model::Backend;
use gadk_prompts::{FileStore, NewVersion, PromptRegistry, PromptStore, Selector};

const MODEL: &str = "gemini-3.8-flash";
const PROMPT: &str = "exp-assistant";
/// The instructions of prompt versions 1 and 2.
const TEMPLATES: [&str; 2] = [
    "You are a concise assistant. Answer in one or two sentences.",
    "You are a precise assistant. Answer in a single short sentence, without preamble, and \
     name the key fact first.",
];
const SET: &str = r#"
name: exp-smoke
judge_rubric: "The reply answers the question correctly in at most two sentences."
cases:
  - id: capital-france
    turns: ["What is the capital of France?"]
    expect:
      - contains: { text: "paris", ignore_case: true }
      - llm_judge: {}
  - id: water-boiling
    turns: ["At what temperature in degrees Celsius does water boil at sea level?"]
    expect:
      - contains: "100"
      - llm_judge: {}
  - id: largest-planet
    turns: ["Which planet in our solar system is the largest?"]
    expect:
      - contains: { text: "jupiter", ignore_case: true }
      - llm_judge: "Names Jupiter and adds no unrelated facts."
"#;

/// The value of the environment variable `name`.
fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}

/// Creates versions 1 and 2 of prompt `PROMPT` (from `TEMPLATES`) unless they exist.
async fn ensure_prompt(store: &dyn PromptStore) -> Result<(), Box<dyn Error>> {
    let snap = store.load_all().await?;
    let have = snap.prompts.get(PROMPT).map_or(0, |p| p.versions.len());
    for template in TEMPLATES.iter().skip(have) {
        store
            .create_version(NewVersion {
                name: PROMPT.to_string(),
                template: (*template).to_string(),
                config: None,
                author: "experiment example".to_string(),
                message: "experiment variant".to_string(),
            })
            .await?;
    }
    let snap = store.load_all().await?;
    let versions = snap.prompts.get(PROMPT).map_or(0, |p| p.versions.len());
    if versions < 2 {
        return Err(format!("prompt {PROMPT} has {versions} version(s), expected 2").into());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let project = env("GOOGLE_CLOUD_PROJECT")?;
    let prompts_dir = env("GADK_EXPERIMENT_PROMPTS")?;
    let out = env("GADK_EXPERIMENT_OUT")?;
    // Opened first, so a bad URL fails before any model call.
    let reports = ReportStore::open(&out)?;

    let store: Arc<dyn PromptStore> = Arc::new(FileStore::new(prompts_dir));
    ensure_prompt(store.as_ref()).await?;
    let registry = PromptRegistry::builder(store).load().await?;

    let model = GeminiModel::new(
        MODEL,
        ClientConfig {
            backend: Some(Backend::VertexAi),
            project: Some(project),
            location: Some("us".to_string()),
            ..Default::default()
        },
    )
    .await?;
    let set = EvalSet::from_yaml(SET)?;
    // Both variants use MODEL; recording it puts a Model row on the console's comparison page.
    let variants = [
        Variant::prompt("v1", Selector::Version(1)).with_model(MODEL),
        Variant::prompt("v2", Selector::Version(2)).with_model(MODEL),
    ];
    // The agent's model also judges the `llm_judge` scorers.
    let evaluator = Evaluator::new(model.clone());
    let report = run_experiment("exp-e2e", &evaluator, &set, &variants, |v| {
        let selector = v.selector().cloned().unwrap_or(Selector::Latest);
        llm_agent::new(LlmAgentConfig {
            name: "exp_assistant".to_string(),
            model: Some(model.clone()),
            description: "Answers short questions.".to_string(),
            instruction_provider: Some(registry.instruction(PROMPT, selector).into_provider()),
            ..Default::default()
        })
    })
    .await?;
    registry.close();

    print!("{}", report.summary());
    let stored = reports.put_experiment(&report).await?;
    println!("stored {stored}");
    Ok(())
}
