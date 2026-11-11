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

//! Go `cmd/launcher/full`: every sub-launcher.

use crate::universal::UniversalLauncher;
use crate::web::a2a;
use crate::web::api;
use crate::web::triggers::{eventarc, pubsub};
use crate::web::webui;

/// Go `full.NewLauncher()` = universal(console, web(webui, a2a, pubsub, eventarc, api)).
pub fn new() -> UniversalLauncher {
    UniversalLauncher::new(vec![
        Box::new(crate::console::new()),
        Box::new(crate::web::new(vec![
            Box::new(webui::new()),
            Box::new(a2a::new()),
            Box::new(pubsub::new()),
            Box::new(eventarc::new()),
            Box::new(api::new()),
        ])),
    ])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use pretty_assertions::assert_eq;

    use crate::launcher::Launcher;
    use crate::test_support as ts;

    /// The syntax a CLI fixture's `Run failed: <error>\n\n<syntax>` record ends with.
    fn fixture_syntax(name: &str) -> String {
        let f = ts::cli(name);
        let stderr = f["stderr"].as_str().unwrap();
        let record = format!("<ts> Run failed: {}\n\n", f["error"].as_str().unwrap());
        let at = stderr.find(&record).unwrap() + record.len();
        stderr[at..].to_owned()
    }

    /// `new()` composes Go's `full.NewLauncher()` in Go's order: the universal launcher's
    /// console then web, and web's webui, a2a, pubsub, eventarc then api (api last, so its mount
    /// shadows none of the others). Its syntax is the one every `full` CLI fixture ends with.
    #[test]
    fn preset_order_and_syntax_match_go() {
        let syntax = super::new().command_line_syntax();
        let keywords: Vec<&str> = syntax
            .lines()
            .filter_map(|l| l.strip_prefix("  * ").or_else(|| l.strip_prefix("    * ")))
            .filter_map(|l| l.split(' ').next())
            .collect();
        assert_eq!(
            keywords,
            [
                "console", "web", "webui", "a2a", "pubsub", "eventarc", "api"
            ]
        );
        let mut checked = 0;
        for name in ts::fixture_names("cli") {
            if ts::cli(&name)["launcher"] == "full" {
                assert_eq!(fixture_syntax(&name), syntax, "{name}");
                checked += 1;
            }
        }
        assert_eq!(checked, 32);
    }
}
