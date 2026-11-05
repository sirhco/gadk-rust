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

//! Quickstart: one non-streaming Gemini call through the `gadk` façade.
//!
//! Gemini API: `GOOGLE_API_KEY=... cargo run -p gadk --example quickstart`
//! Vertex AI:  `GOOGLE_GENAI_USE_VERTEXAI=true GOOGLE_CLOUD_PROJECT=... GOOGLE_CLOUD_LOCATION=us-central1 cargo run -p gadk --example quickstart`

use futures::StreamExt;
use gadk::gemini::{ClientConfig, GeminiModel};
use gadk::model::LlmRequest;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model_name = std::env::var("GADK_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string());
    let model = GeminiModel::new(&model_name, ClientConfig::default()).await?;
    let req = LlmRequest {
        contents: gadk::genai::text("Say hello in one sentence."),
        ..Default::default()
    };
    let mut out = model.generate_content(req, false);
    while let Some(resp) = out.next().await {
        let resp = resp?;
        for part in resp.content.map(|c| c.parts).unwrap_or_default() {
            print!("{}", part.text);
        }
    }
    println!();
    Ok(())
}
