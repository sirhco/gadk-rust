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

//! Test fixtures (feature `testutil`): a scripted runner for the executor, a fake A2A client and
//! client provider for the remote agent, and an in-process a2a-rs server.

#[cfg(feature = "client")]
pub(crate) mod client;
pub(crate) mod runner;
#[cfg(feature = "server")]
pub(crate) mod server;

#[cfg(feature = "client")]
pub use client::{FakeA2AClient, FakeCall, FakeClientProvider};
pub use runner::ScriptedRunner;
#[cfg(feature = "server")]
pub use server::{SpawnedServer, spawn_a2a_server};
