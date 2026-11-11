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

//! gadk-launcher: run a gadk agent the way an ADK-Go binary runs (port of ADK-Go `cmd/launcher`).
//!
//! A [`Launcher`] parses ADK-Go's command lines and runs one [`SubLauncher`]: the interactive
//! `console`, or the `web` server with its sub-launchers (`api`, `a2a`, `webui`, `pubsub`,
//! `eventarc`). [`full::new`] and [`prod::new`] are ADK-Go's presets; [`universal`] routes the
//! first argument to a sub-launcher. Flags, help, error, log and console text are ADK-Go's.
//!
//! Features: `web` (the web server and the REST API sub-launcher), `a2a`, `webui`, `triggers`,
//! `prod` (`web` + `a2a`) and `full` (all of them, the default). The console is always built.

#[cfg(feature = "agentengine")]
pub mod agentengine;
pub mod config;
pub mod console;
pub mod error;
mod flag_usage;
#[cfg(feature = "full")]
pub mod full;
pub mod golog;
pub mod launcher;
#[cfg(test)]
mod parity_tests;
#[cfg(feature = "prod")]
pub mod prod;
pub mod telemetry;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
pub mod universal;
#[cfg(feature = "web")]
pub mod web;

#[cfg(feature = "a2a")]
pub use config::{A2AHandlerOption, A2AHandlerWrapper};
pub use config::{BoxedLayer, Config};
pub use error::Error;
pub use launcher::{Launcher, SubLauncher};
pub use tokio_util::sync::CancellationToken;
