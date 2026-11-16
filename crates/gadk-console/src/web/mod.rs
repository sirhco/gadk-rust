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

//! The HTTP layer: router, request guards and shared state.

use std::sync::Arc;

use crate::traces::TraceSource;

pub(crate) mod pages;
mod prompts;
mod traces;

#[derive(Clone)]
pub(crate) struct AppState {
    pub source: Arc<dyn TraceSource>,
    pub store: Option<Arc<dyn gadk_prompts::PromptStore>>,
    pub csrf: Arc<str>,                             // 64 hex chars
    pub port: u16,                                  // the bound port
    pub now: fn() -> chrono::DateTime<chrono::Utc>, // Utc::now in production; fixed in tests
}
