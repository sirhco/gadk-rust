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

//! The three context types (port of ADK-Go `agent/context.go` and `internal/context`). Task 4 owns
//! this file.

pub mod builder;
#[allow(clippy::module_inception)]
pub mod context;
pub mod delta;
pub mod invocation;
pub mod readonly;

// Glob re-exports of every child (Task 4 imports through `crate::context::*`). The allows keep the
// empty Task 1 stubs warning-free.
#[allow(unused_imports)]
pub use builder::*;
#[allow(unused_imports)]
pub use context::*;
#[allow(unused_imports)]
pub use delta::*;
#[allow(unused_imports)]
pub use invocation::*;
#[allow(unused_imports)]
pub use readonly::*;
