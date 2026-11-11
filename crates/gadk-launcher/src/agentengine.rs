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

//! Go `cmd/launcher/agentengine`: the launcher an Agent Engine deployment runs.

use crate::universal::UniversalLauncher;
use crate::web::agentengine;

/// Go `NewLauncher(agentEngineId)` = universal(web(agentengine(agentEngineId))).
pub fn new_launcher(agent_engine_id: &str) -> UniversalLauncher {
    UniversalLauncher::new(vec![Box::new(crate::web::new(vec![Box::new(
        agentengine::new(agent_engine_id),
    )]))])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::launcher::Launcher;
    use crate::test_support as ts;

    /// `new_launcher` composes Go's `agentengine.NewLauncher`: the universal launcher's web
    /// alone, with agentengine. Its syntax is the one every `cli_*` fixture ends with.
    #[test]
    fn preset_order_and_syntax_match_go() {
        let syntax = super::new_launcher("test-engine").command_line_syntax();
        let keywords: Vec<&str> = syntax
            .lines()
            .filter_map(|l| l.strip_prefix("  * ").or_else(|| l.strip_prefix("    * ")))
            .filter_map(|l| l.split(' ').next())
            .collect();
        assert_eq!(keywords, ["web", "agentengine"]);
        let mut checked = 0;
        for name in ts::fixture_names("agentengine") {
            if !name.starts_with("cli_") {
                continue;
            }
            let f = ts::fixture("agentengine", &name);
            let stderr = f["stderr"].as_str().unwrap();
            let record = format!("<ts> Run failed: {}\n\n", f["error"].as_str().unwrap());
            let at = stderr.find(&record).unwrap() + record.len();
            assert_eq!(&stderr[at..], syntax, "{name}");
            checked += 1;
        }
        assert_eq!(checked, 7);
    }
}
