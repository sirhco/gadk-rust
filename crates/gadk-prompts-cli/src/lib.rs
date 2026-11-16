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

//! The `gadk-prompts` command-line tool, as a library for testing.

mod commands;
mod store_url;

pub use store_url::open_store;

use std::io::{Read, Write};

use clap::{Arg, ArgAction, Command};
use gadk_prompts::StoreError;

fn cli() -> Command {
    let name = || Arg::new("name").required(true);
    let label = || Arg::new("label").long("label").required(true);
    Command::new("gadk-prompts")
        .about("Manage gadk prompt versions and labels")
        .arg(
            Arg::new("store")
                .long("store")
                .global(true)
                .help("file://<dir> or firestore://<project>/<database>[/<root>]"),
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("push")
                .about("Create a new version (no-op when unchanged)")
                .arg(name())
                .arg(
                    Arg::new("template")
                        .long("template")
                        .help("template file, or - for stdin (default)"),
                )
                .arg(Arg::new("config").long("config").help("JSON config file"))
                .arg(Arg::new("message").short('m').long("message"))
                .arg(Arg::new("author").long("author")),
        )
        .subcommand(
            Command::new("promote")
                .about("Point a label at a version")
                .arg(name())
                .arg(Arg::new("version").required(true))
                .arg(label())
                .arg(
                    Arg::new("expect")
                        .long("expect")
                        .help("required current version, or none"),
                ),
        )
        .subcommand(
            Command::new("unlabel")
                .about("Remove a label")
                .arg(name())
                .arg(label()),
        )
        .subcommand(
            Command::new("list")
                .about("List prompts")
                .arg(Arg::new("prefix").long("prefix")),
        )
        .subcommand(
            Command::new("show")
                .about("Show a version (latest by default)")
                .arg(name())
                .arg(Arg::new("label").long("label").conflicts_with("version"))
                .arg(Arg::new("version").long("version"))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("diff")
                .about("Unified diff of two versions (v<n>, a label, or latest)")
                .arg(name())
                .arg(Arg::new("from").required(true))
                .arg(Arg::new("to").required(true)),
        )
}

/// The exit code of a store error.
fn code(e: &StoreError) -> i32 {
    match e {
        StoreError::Invalid(_) => 1,
        StoreError::NotFound { .. } => 2,
        StoreError::Conflict { .. } => 3,
        _ => 4,
    }
}

/// Runs the tool with `args` (including the program name) and returns the exit code:
/// 0 ok, 1 usage or invalid input, 2 not found, 3 conflict, 4 store or IO error.
pub async fn run(
    args: Vec<String>,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let m = match cli().try_get_matches_from(args) {
        Ok(m) => m,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = write!(stdout, "{}", e.render());
                return 0;
            }
            let _ = write!(stderr, "gadk-prompts: {}", e.render());
            return 1;
        }
    };
    let result = async {
        // clap forbids required global args, so --store is checked here.
        let url = m
            .get_one::<String>("store")
            .cloned()
            .ok_or_else(|| StoreError::Invalid("--store is required".into()))?;
        let store = store_url::open_store(&url)?;
        commands::dispatch(&*store, &m, stdin, stdout).await
    }
    .await;
    match result {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(stderr, "gadk-prompts: {e}");
            code(&e)
        }
    }
}
