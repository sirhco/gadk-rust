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

//! `gadk-eval`: run production eval monitors and read their reports.

#[tokio::main]
async fn main() {
    let code = gadk_eval_monitor::cli::run(
        std::env::args().collect(),
        &gadk_eval_monitor::cli::CloudBackends,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
    .await;
    std::process::exit(code);
}
