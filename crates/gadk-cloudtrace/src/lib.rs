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

//! A Google Cloud Trace v1 read client: list the traces of a project in a time window, and get
//! one trace with all of its spans.
//!
//! [`CloudTraceClient::list`] sends `GET /v1/projects/{project}/traces` for a [`ListRequest`]
//! (the window, a [`View`], [`Filter`] terms, newest-first order, a page size and a page token)
//! and returns a [`TracePage`]; [`CloudTraceClient::get`] sends
//! `GET /v1/projects/{project}/traces/{trace_id}` and returns a [`Trace`]. Span ids come back as
//! 16 lowercase hex characters. Failures are [`TraceError`]s. Every request carries the headers
//! of an [`AuthHeaders`] implementation ([`Adc`], Application Default Credentials, by default)
//! and `x-goog-user-project: <project>`.
//!
//! ```no_run
//! # async fn demo() -> Result<(), gadk_cloudtrace::TraceError> {
//! use chrono::{TimeDelta, Utc};
//! use gadk_cloudtrace::{CloudTraceClient, Filter, ListRequest, View};
//!
//! let client = CloudTraceClient::new("my-proj");
//! let end = Utc::now();
//! let page = client
//!     .list(&ListRequest {
//!         start: end - TimeDelta::hours(1),
//!         end,
//!         view: View::RootSpan,
//!         filters: vec![Filter::LabelExact("gen_ai.agent.name".into(), "support".into())],
//!         start_descending: true,
//!         page_size: 50,
//!         page_token: None,
//!     })
//!     .await?;
//! for summary in &page.traces {
//!     let trace = client.get(&summary.trace_id).await?;
//!     println!("{}: {} spans", trace.trace_id, trace.spans.len());
//! }
//! # Ok(()) }
//! ```

mod auth;
mod client;
mod error;
mod types;

pub use auth::{Adc, AuthHeaders, NoAuth};
pub use client::{CloudTraceClient, ENDPOINT};
pub use error::TraceError;
pub use types::{Filter, ListRequest, Span, Trace, TracePage, View};
