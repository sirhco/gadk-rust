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

//! `gadk-console`: a local web console for gadk traces and prompts.

use std::process::ExitCode;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    // A usage error prints clap's message and exits 2 (`--help` prints and exits 0).
    let cfg = gadk_console::parse_args(std::env::args_os()).unwrap_or_else(|e| e.exit());
    match gadk_console::run(cfg, &mut std::io::stdout()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gadk-console: {e}");
            ExitCode::FAILURE
        }
    }
}
