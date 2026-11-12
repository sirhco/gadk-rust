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

//! Port of ADK-Go `cmd/adkgo/internal/root/root.go` and cobra's `Execute`: the command tree
//! `gadk` → `deploy` → {`agentengine`, `cloudrun`}, dispatch, and cobra's error reporting
//! (`Error: <msg>` and the failing command's usage on stderr, exit 1).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::ffi::OsString;

use clap::Command;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use gadk_platform::go_quote;

use crate::Env;
use crate::deploy::{self, agentengine, cloudrun};
use crate::flags::{STRAY_ARGS, pflag_error};

/// The root command's name (Go `RootCmd.Use` `adkgo`).
pub const NAME: &str = "gadk";
// Divergence (ledger 6b): the root texts name gadk (Go: "CLI tool for use with ADK-GO",
// "adkgo is a CLI tool …").
/// Go `RootCmd.Short`.
pub const SHORT: &str = "CLI tool for use with gadk";
/// Go `RootCmd.Long`.
pub const LONG: &str =
    "gadk is a CLI tool which allows developer to quickly deploy and test an agentic application";

/// Go `RootCmd` with `deploy` added.
// Divergence (ledger 6b): no `completion` command (cobra adds one to every root); clap's
// `help` subcommand stands in for cobra's.
pub fn command() -> Command {
    todo!("Task 2")
}

/// Go `root.Execute()`: parses `args` (program name first), runs the selected command and
/// returns the exit code (0, or 1 after printing the error as cobra does).
pub async fn execute(args: Vec<OsString>, env: &Env) -> u8 {
    todo!("Task 2")
}

/// cobra's error report: `Error: <msg>`, then the command's usage and an empty line.
fn report(env: &Env, msg: &str, path: &[&str]) -> u8 {
    todo!("Task 2")
}

fn report_parse_error(e: &clap::Error, raw: &[String], env: &Env) -> u8 {
    todo!("Task 2")
}

/// The subcommand path cobra's `Find` would select for `raw`: the first non-flag argument of
/// each level, when it names a subcommand (flags before `--` are skipped; `gadk` and
/// `deploy` have no value-taking flags).
fn command_path(raw: &[String]) -> Vec<&'static str> {
    todo!("Task 2")
}

/// The built (`gadk deploy …`-named) subcommand at `path`.
fn find(path: &[&str]) -> Command {
    todo!("Task 2")
}

/// cobra's `UsageString()` in clap's layout: the usage line and the argument list.
// Divergence (ledger 6b): clap's help and usage layout, not cobra's template.
fn usage(path: &[&str]) -> String {
    todo!("Task 2")
}

/// cobra's help for the command at `path` in clap's layout.
fn help(path: &[&str]) -> String {
    todo!("Task 2")
}

/// cobra `findSuggestions` on the root: each available subcommand within Levenshtein
/// distance 2 (case-insensitive) of `typed`, or that `typed` prefixes.
fn suggestions(typed: &str) -> String {
    todo!("Task 2")
}

/// cobra `ld(s, t, true)`: byte-wise Levenshtein distance of the lowercased strings.
fn levenshtein_ignore_case(s: &str, t: &str) -> usize {
    todo!("Task 2")
}
