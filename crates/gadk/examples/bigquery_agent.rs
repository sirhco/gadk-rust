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

//! An agent that answers questions about BigQuery data with the BigQuery toolset.
//!
//! `GOOGLE_CLOUD_PROJECT=<project> cargo run -p gadk --example bigquery_agent \
//!   --features gemini,launcher,bigquery`
//!
//! Uses Application Default Credentials for Gemini on Vertex AI (location `us`) and BigQuery
//! (BigQuery Data Viewer and Job User on the project).

use std::sync::Arc;

use gadk::agent::SingleLoader;
use gadk::bigquery::{BigQueryToolConfig, BigQueryToolset};
use gadk::engine::llm::LlmAgentConfig;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::launcher::golog::Logger;
use gadk::launcher::{CancellationToken, Config, Launcher, full};
use gadk::model::Backend;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let project = std::env::var("GOOGLE_CLOUD_PROJECT")?;
    let model = GeminiModel::new(
        "gemini-3.8-flash",
        ClientConfig {
            backend: Some(Backend::VertexAi),
            project: Some(project.clone()),
            location: Some("us".to_string()),
            ..Default::default()
        },
    )
    .await?;

    let bigquery = Arc::new(BigQueryToolset::new(BigQueryToolConfig::new())?);
    let agent = gadk::engine::llm::agent::new(LlmAgentConfig {
        name: "bigquery_agent".to_string(),
        model: Some(model),
        description: "Answers questions about data in BigQuery.".to_string(),
        instruction: format!(
            "You answer questions about data in BigQuery project {project}. Explore before you \
             query: list datasets and tables, read a table's schema with get_table_info, then \
             write one GoogleSQL query with execute_sql. Qualify tables with their dataset \
             (for example gadk_examples.orders). Explain the result in plain language."
        ),
        toolsets: vec![bigquery],
        ..Default::default()
    })?;

    let config = Config::new(Arc::new(SingleLoader::new(agent)));
    let mut l = full::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = l.execute(CancellationToken::new(), config, args).await {
        Logger::stderr().print(&format!("Run failed: {e}\n\n{}", l.command_line_syntax()));
        std::process::exit(1);
    }
    Ok(())
}
