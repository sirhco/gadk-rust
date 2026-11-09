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

//! `gadk-mcp-stdio-test-server`: the `testutil` weather server over stdio, spawned by the
//! `child_process` test through `McpTransport::Command`.
//!
//! It serves [`gadk_mcp::testutil::weather_server`] on stdin/stdout until the client closes
//! stdin, then exits 0; a failed handshake exits 1.

use std::process::ExitCode;

use gadk_mcp::rmcp::ServiceExt;
use gadk_mcp::testutil::weather_server;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("gadk-mcp-stdio-test-server: runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let running = match weather_server()
            .serve(gadk_mcp::rmcp::transport::stdio())
            .await
        {
            Ok(running) => running,
            Err(e) => {
                eprintln!("gadk-mcp-stdio-test-server: initialize: {e}");
                return ExitCode::FAILURE;
            }
        };
        match running.waiting().await {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("gadk-mcp-stdio-test-server: {e}");
                ExitCode::FAILURE
            }
        }
    })
}
