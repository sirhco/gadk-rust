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

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! R16: `gadk::launcher` (feature `launcher`, not on by default) is `gadk_launcher`, with its
//! `full` and `prod` presets; a command line it cannot run fails as Go's examples report it.

use std::sync::Arc;

use gadk::agent::{AgentConfig, SingleLoader, new_agent};
use gadk::launcher::golog;
use gadk::launcher::{CancellationToken, Config, Error, Launcher, full, prod};
use pretty_assertions::assert_eq;

/// Compiles only if each façade path names the `gadk_launcher` item.
#[test]
fn facade_launcher_paths_are_gadk_launcher() {
    fn same<T: ?Sized>(_: Option<&T>, _: Option<&T>) {}
    same::<Config>(None, None::<&gadk_launcher::Config>);
    same::<Error>(None, None::<&gadk_launcher::Error>);
    same::<golog::Logger>(None, None::<&gadk_launcher::golog::Logger>);
    same::<gadk::launcher::universal::UniversalLauncher>(
        None,
        None::<&gadk_launcher::universal::UniversalLauncher>,
    );
    same::<dyn gadk::launcher::SubLauncher>(None, None::<&dyn gadk_launcher::SubLauncher>);
    same::<gadk::launcher::web::WebLauncher>(None, None::<&gadk_launcher::web::WebLauncher>);
}

/// The presets list Go's sub-launchers, in Go's order.
#[test]
fn facade_launcher_presets_are_gos() {
    let syntax = full::new().command_line_syntax();
    assert!(
        syntax.starts_with(
            "Arguments: Specify one of the following:\n  * console - runs an agent in console mode.\n  * web - starts web server with additional sub-servers specified by sublaunchers\n"
        ),
        "{syntax}"
    );
    assert!(
        prod::new()
            .command_line_syntax()
            .starts_with("Arguments: Specify one of the following:\n  * web - ")
    );
}

/// `web` without a sub-launcher fails as Go's does; an example reports it as Go's examples do
/// (`Run failed: <err>\n\n<syntax>` through the Go log).
#[tokio::test]
async fn facade_launcher_reports_a_command_line_it_cannot_run() {
    let agent = new_agent(AgentConfig {
        name: "facade_agent".into(),
        ..Default::default()
    })
    .unwrap();
    let (log, out) = golog::capture();
    let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
    config.log = log.clone();
    let mut l = full::new();
    let err = l
        .execute(CancellationToken::new(), config, vec!["web".to_owned()])
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "no active sublaunchers found - please specify them in the command line. Possible values: [webui a2a pubsub eventarc api]"
    );
    log.print(&format!("Run failed: {err}\n\n{}", l.command_line_syntax()));
    let logged = out.contents();
    assert!(
        logged.ends_with(&format!("Run failed: {err}\n\n{}", l.command_line_syntax())),
        "{logged}"
    );
}
