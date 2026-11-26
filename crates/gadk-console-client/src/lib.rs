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

//! gadk-console-client: a typed async client for the read-only JSON API of
//! [gadk-console](https://crates.io/crates/gadk-console) (`/api/v1/…`).
//!
//! The types, query builders and methods are generated from the console's OpenAPI document by
//! `scripts/gen-console-client.py`. Each operation is a [`Client`] method that takes its query
//! by reference and returns the route's envelope (`data`, `truncated`, `next`):
//!
//! ```no_run
//! use gadk_console_client::{Client, GetUsageBreakdownQuery, UsageRange};
//!
//! # async fn run() -> Result<(), gadk_console_client::Error> {
//! let client = Client::new("http://127.0.0.1:8765")?;
//! let usage = client
//!     .get_usage_breakdown(&GetUsageBreakdownQuery::new().range(UsageRange::D7))
//!     .await?;
//! for cell in &usage.data.cells {
//!     println!("{} {} calls", cell.calls.model, cell.calls.calls);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The public API exposes reqwest 0.13 types: [`Error::Transport`] carries a `reqwest::Error`
//! and [`Client::with_http`] takes a `reqwest::Client`. A reqwest major-version upgrade is
//! therefore a breaking release of this crate.
//!
//! The response types have no constructors: build test values from JSON.
//!
//! ```
//! use gadk_console_client::PricedSessionRow;
//!
//! let row: PricedSessionRow = serde_json::from_value(serde_json::json!({
//!     "session_id": "s-1", "user": "u-1", "agent": "triage",
//!     "first": "2026-11-16T11:00:00Z", "last": "2026-11-16T11:05:00Z",
//!     "turns": 2, "errors": 0, "ups": 1, "downs": 0,
//!     "tokens": [["gemini-3.8-flash", {"input": 900, "cached": 0, "output": 120, "reasoning": 0}]],
//!     "cost_usd": null, "cost_partial": false
//! }))
//! .unwrap();
//! assert_eq!(row.tokens[0].0, "gemini-3.8-flash");
//! assert_eq!(row.tokens[0].1.input, 900);
//! ```

mod client;
mod error;
mod generated;

pub use client::{Client, SessionPages};
pub use error::Error;
pub use generated::*;

/// The README's examples, compiled as doc tests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
