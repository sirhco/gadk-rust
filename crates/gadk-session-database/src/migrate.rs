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

//! `auto_migrate`: create missing tables, add missing columns (spec R4).
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use gadk_session::SessionError;

use crate::backend::Backend;

/// One table: its name, the exact `CREATE TABLE` statement ADK-Go's gorm emits for the dialect,
/// and every column with the type `ADD COLUMN` uses.
pub(crate) struct TableDef {
    pub(crate) name: &'static str,
    pub(crate) create: &'static str,
    pub(crate) columns: &'static [(&'static str, &'static str)],
}

/// For each table of `backend.tables()` in order: create it when it has no columns, else add
/// each missing column. One write transaction for the whole migration.
pub(crate) async fn auto_migrate(backend: &Backend) -> Result<(), SessionError> {
    todo!("Task 4")
}
