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

//! Wire, console and CLI replay (spec R18): every fixture `scripts/gen-go-launcher` recorded from
//! ADK-Go's `full` and `prod` launchers (`testdata/{wire,console,cli}`) is replayed against
//! gadk-launcher's presets exactly as `scripts/gen-go-launcher/SCENARIO.md` ("Replay") describes,
//! with the probe's agent, and compared after the normalizations N1–N7 and the fixture's declared
//! deltas (`<name>.deltas.json`, each citing a `docs/parity/6.md` divergence; format in
//! `support::Deltas`).
//!
//! Process fixtures run `execute(token, config, args)` with stdin from the fixture, stdout
//! captured, and the `golog` sink and every flag set's output in one stderr buffer; an error is
//! logged as the probe's child does (`log.Fatalf("Run failed: %v\n\n%s", err, syntax)`, exit
//! status 1). Web fixtures start one server per fixture server on a fresh port, send each
//! exchange's raw request on a new connection, and cancel the token for the shutdown.
//!
//! The servers whose `api` sub-launcher adds a telemetry layer find the global `tracing`
//! subscriber this test installs first (a process can have only one, and which server would
//! install it first is up to the test order), so they log that they keep it: declared in their
//! startup deltas (ledger: telemetry keeps an existing global subscriber).

#![cfg(all(feature = "full", feature = "prod"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::io::Cursor;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use gadk_agent::SingleLoader;
use gadk_launcher::console::{self, input_lines};
use gadk_launcher::golog::{self, Captured};
use gadk_launcher::universal::UniversalLauncher;
use gadk_launcher::web::triggers::{eventarc, pubsub};
use gadk_launcher::web::{self, a2a, api, webui};
use gadk_launcher::{CancellationToken, Config, Launcher};
use serde_json::{Value, json};
use support::{
    Deltas, compare, exchange, fixture, fixture_names, free_port, n1, n4, normalize_log,
    parse_response, probe_agent, wait_listening,
};

/// The web sub-launchers of each preset in declaration order (N4).
const FULL_ORDER: &[&str] = &["webui", "a2a", "pubsub", "eventarc", "api"];
const PROD_ORDER: &[&str] = &["api", "a2a"];

/// How long stderr must stay quiet before an exchange's log is taken (the probe waits 150 ms).
const QUIET: Duration = Duration::from_millis(100);

/// One global subscriber for the whole test process, installed before any server runs.
fn install_global_subscriber() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = tracing::dispatcher::set_global_default(tracing::Dispatch::new(
            tracing_subscriber::registry(),
        ));
    });
}

/// The preset `name` as `full::new()`/`prod::new()` compose it, with every flag set writing to
/// `stderr` and the console reading `stdin` and writing `stdout` (stdout is not a terminal).
fn preset(name: &str, stdin: &str, stdout: Captured, stderr: &Captured) -> UniversalLauncher {
    let out = || Arc::new(Mutex::new(stderr.clone())) as Arc<Mutex<dyn std::io::Write + Send>>;
    let console = console::new()
        .with_io(
            input_lines(Cursor::new(stdin.as_bytes().to_vec())),
            Arc::new(Mutex::new(stdout)),
            false,
        )
        .with_flag_output(out());
    match name {
        "full" => UniversalLauncher::new(vec![
            Box::new(console),
            Box::new(
                web::new(vec![
                    Box::new(webui::new().with_flag_output(out())),
                    Box::new(a2a::new().with_flag_output(out())),
                    Box::new(pubsub::new().with_flag_output(out())),
                    Box::new(eventarc::new().with_flag_output(out())),
                    Box::new(api::new().with_flag_output(out())),
                ])
                .with_flag_output(out()),
            ),
        ]),
        "prod" => UniversalLauncher::new(vec![Box::new(
            web::new(vec![
                Box::new(api::new().with_flag_output(out())),
                Box::new(a2a::new().with_flag_output(out())),
            ])
            .with_flag_output(out()),
        )]),
        other => panic!("unknown preset {other}"),
    }
}

/// The probe child's `main` after `Execute`: `log.Fatalf("Run failed: %v\n\n%s", …)` on error.
fn finish(
    result: Result<(), gadk_launcher::Error>,
    syntax: &str,
    log: &golog::Logger,
) -> (i64, Option<String>) {
    match result {
        Ok(()) => (0, None),
        Err(e) => {
            log.print(&format!("Run failed: {e}\n\n{syntax}"));
            (1, Some(e.to_string()))
        }
    }
}

/// A config serving the probe's agent with `log` as its Go log.
fn config(log: golog::Logger) -> Config {
    let mut config = Config::new(Arc::new(SingleLoader::new(probe_agent())));
    config.log = log;
    config
}

/// Replays one process fixture (`console/` or `cli/`).
async fn replay_process(dir: &str, name: &str) -> Result<(), String> {
    let f = fixture(dir, name);
    let stdout = Captured::default();
    let (log, stderr) = golog::capture();
    let mut l = preset(
        f["launcher"].as_str().unwrap(),
        f["stdin"].as_str().unwrap(),
        stdout.clone(),
        &stderr,
    );
    let args = support::args(&f, 0);
    let result = l
        .execute(CancellationToken::new(), config(log.clone()), args)
        .await;
    let (exit_status, error) = finish(result, &l.command_line_syntax(), &log);
    let got = json!({
        "stdout": stdout.contents(),
        "stderr": n1(&stderr.contents()),
        "exit_status": exit_status,
        "error": error,
    });
    let want = json!({
        "stdout": f["stdout"],
        "stderr": f["stderr"],
        "exit_status": f["exit_status"],
        "error": f["error"],
    });
    compare(
        &format!("{dir}/{name}"),
        &want,
        &got,
        &Deltas::load(dir, name),
    )
}

/// Waits until `captured` has been quiet for [`QUIET`].
async fn quiet(captured: &Captured) {
    let mut last = captured.contents().len();
    loop {
        tokio::time::sleep(QUIET).await;
        let now = captured.contents().len();
        if now == last {
            return;
        }
        last = now;
    }
}

/// Replays every fixture of the web server `server` (`<server>_NN_*`, sorted by name).
async fn replay_server(server: &str) -> Vec<String> {
    let names: Vec<String> = fixture_names("wire")
        .into_iter()
        .filter(|n| n.starts_with(&format!("{server}_")))
        .collect();
    let startup = fixture("wire", &names[0]);
    assert_eq!(startup["kind"], "startup", "{}", names[0]);
    let preset_name = startup["launcher"].as_str().unwrap().to_owned();
    let order = if preset_name == "full" {
        FULL_ORDER
    } else {
        PROD_ORDER
    };
    let port = free_port();
    let stdout = Captured::default();
    let (log, stderr) = golog::capture();
    let mut l = preset(&preset_name, "", stdout.clone(), &stderr);
    let cancel = CancellationToken::new();
    let running = {
        let (cancel, config, args) = (
            cancel.clone(),
            config(log.clone()),
            support::args(&startup, port),
        );
        let log = log.clone();
        tokio::spawn(async move {
            let result = l.execute(cancel, config, args).await;
            finish(result, &l.command_line_syntax(), &log)
        })
    };
    wait_listening(port).await;
    quiet(&stderr).await;

    let mut problems = Vec::new();
    let mut taken = 0;
    let mut take_log = |all: String| {
        let chunk = all[taken..].to_owned();
        taken = all.len();
        normalize_log(&chunk, port)
    };
    let got = json!({ "log": n4(take_log(stderr.contents()), order) });
    let want = json!({ "log": startup["log"] });
    if let Err(e) = compare(&names[0], &want, &got, &Deltas::load("wire", &names[0])) {
        problems.push(e);
    }

    for name in &names[1..] {
        let f = fixture("wire", name);
        match f["kind"].as_str().unwrap() {
            "exchange" => {
                let raw = exchange(port, &f["request"]).await;
                quiet(&stderr).await;
                let head = f["request"]["method"] == "HEAD";
                let got = json!({
                    "response": parse_response(&raw, head, port),
                    "log": take_log(stderr.contents()),
                });
                let want = json!({ "response": f["response"], "log": f["log"] });
                if let Err(e) = compare(name, &want, &got, &Deltas::load("wire", name)) {
                    problems.push(e);
                }
            }
            "shutdown" => {
                cancel.cancel();
                let (exit_status, error) = tokio::time::timeout(Duration::from_secs(30), running)
                    .await
                    .expect("the server did not shut down")
                    .unwrap();
                let got = json!({
                    "log": take_log(stderr.contents()),
                    "exit_status": exit_status,
                    "stdout": stdout.contents(),
                    "error": error,
                });
                let want = json!({
                    "log": f["log"],
                    "exit_status": f["exit_status"],
                    "stdout": f["stdout"],
                    "error": f["error"],
                });
                if let Err(e) = compare(name, &want, &got, &Deltas::load("wire", name)) {
                    problems.push(e);
                }
                return problems;
            }
            kind => panic!("{name}: unexpected kind {kind}"),
        }
    }
    panic!("{server}: no shutdown fixture");
}

fn report(problems: Vec<String>) {
    assert!(
        problems.is_empty(),
        "{} fixture(s) differ:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_fixtures_replay() {
    install_global_subscriber();
    let names = fixture_names("cli");
    assert_eq!(names.len(), 34);
    let mut problems = Vec::new();
    for name in &names {
        if let Err(e) = replay_process("cli", name).await {
            problems.push(e);
        }
    }
    report(problems);
}

#[tokio::test(flavor = "multi_thread")]
async fn console_fixtures_replay() {
    install_global_subscriber();
    let names = fixture_names("console");
    assert_eq!(names.len(), 15);
    let mut problems = Vec::new();
    for name in &names {
        if let Err(e) = replay_process("console", name).await {
            problems.push(e);
        }
    }
    report(problems);
}

/// The web servers, one after another (each on its own port).
#[tokio::test(flavor = "multi_thread")]
async fn wire_fixtures_replay() {
    install_global_subscriber();
    let names = fixture_names("wire");
    assert_eq!(names.len(), 70);
    let mut servers: Vec<String> = names
        .iter()
        .map(|n| n.split('_').next().unwrap().to_owned())
        .collect();
    servers.dedup();
    assert_eq!(servers, ["full", "noprefix", "oidc", "prod"]);
    let mut problems = Vec::new();
    for server in &servers {
        problems.extend(replay_server(server).await);
    }
    report(problems);
}

/// Every deltas file belongs to a fixture, and every divergence it cites is a line of the
/// ledger's accepted divergences (`docs/parity/6.md`).
#[test]
fn every_delta_cites_a_ledger_divergence() {
    let ledger = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/parity/6.md"),
    )
    .unwrap();
    let divergences: Vec<&str> = ledger
        .lines()
        .filter_map(|l| l.strip_prefix("- "))
        .collect();
    let mut files = 0;
    for dir in ["wire", "console", "cli"] {
        let names = fixture_names(dir);
        for entry in std::fs::read_dir(support::testdata(dir)).unwrap() {
            let file = entry.unwrap().file_name().into_string().unwrap();
            let Some(name) = file.strip_suffix(".deltas.json") else {
                continue;
            };
            files += 1;
            assert!(names.iter().any(|n| n == name), "{dir}/{file}: no fixture");
            let doc: Value = serde_json::from_str(
                &std::fs::read_to_string(support::testdata(dir).join(&file)).unwrap(),
            )
            .unwrap();
            let cited = doc["deltas"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| &d["divergence"])
                .chain(doc.get("json_body"));
            for divergence in cited {
                let text = divergence.as_str().unwrap();
                assert!(
                    divergences.contains(&text),
                    "{dir}/{file}: {text:?} is not a divergence line of docs/parity/6.md"
                );
            }
        }
    }
    assert_eq!(files, 8, "declared deltas files");
}
