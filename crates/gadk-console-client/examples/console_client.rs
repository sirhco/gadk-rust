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

//! Prints a console's usage breakdown and its first sessions page.
//!
//! ```sh
//! cargo run -p gadk-console-client --example console_client -- http://127.0.0.1:8765
//! ```

use gadk_console_client::{Client, Error, GetUsageBreakdownQuery, ListSessionsQuery};

#[tokio::main]
async fn main() {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:8765".to_owned());
    if let Err(e) = run(&base).await {
        eprintln!("console_client: {e}");
        if e.is_needs_bigquery() {
            eprintln!("start the console with the BigQuery trace source");
        }
        std::process::exit(1);
    }
}

async fn run(base: &str) -> Result<(), Error> {
    let client = Client::new(base)?;

    let usage = client
        .get_usage_breakdown(&GetUsageBreakdownQuery::new())
        .await?;
    let (mut calls, mut input, mut output) = (0, 0, 0);
    println!("usage breakdown (24h):");
    for cell in &usage.data.cells {
        let c = &cell.calls;
        calls += c.calls;
        input += c.usage.input;
        output += c.usage.output;
        println!(
            "  {} {} {}: {} calls, {} in, {} out",
            c.model,
            cell.agent,
            cell.prompt.as_deref().unwrap_or("-"),
            c.calls,
            c.usage.input,
            c.usage.output
        );
    }
    println!("  total: {calls} calls, {input} in, {output} out");

    let mut pages = client.list_sessions_pages(&ListSessionsQuery::new());
    if let Some(page) = pages.next_page().await? {
        println!("sessions (first page, 24h):");
        for s in &page.data {
            println!(
                "  {} {} {} turns, last {}",
                s.session_id, s.user, s.turns, s.last
            );
        }
        if page.next.is_some() {
            println!("  (more pages)");
        }
    }
    Ok(())
}
