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

//! The `adkcli` binary end to end: stdout lines and exit codes, and a REST-mode replay of a
//! Go-generated case (Task 12).
//!
//! The expected stdout/stderr texts were captured once from ADK-Go's own binary
//! (`go build ./cmd/internal/adkcli` at `docs/parity/ADK_GO_COMMIT`) run over the same trees
//! (built by the same steps, with `GOOGLE_API_KEY=fake-key`, stdin `/dev/null`, `$PWD` set to
//! the symlinked spelling of the working directory), with the tree root replaced by `{root}`
//! and Go's `log` timestamps by `<ts>`.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use gadk_launcher::Launcher;
use pretty_assertions::assert_eq;
use regex::Regex;
use serde_json::{Value, json};

/// A valid `LlmAgent` config: Go's probe `tools_agent` (`runner-tools-agent` fixture case) with
/// the seven conformance function tools, on a Gemini model.
const TOOLS_AGENT_YAML: &str = "\
agent_class: LlmAgent
name: tools_agent
model: gemini-2.5-flash
instruction: Use the tools.
tools:
  - name: tools_agent_002.tools.validate_email
  - name: tools_agent_002.tools.get_user_id
  - name: tools_agent_002.tools.create_booking
  - name: tools_agent_004.tools.search_flights
  - name: tools_agent_004.tools.calculate_trip_cost
  - name: tools_agent_009.tools.reimburse
  - name: tools_agent_009.tools.ask_for_approval
";

/// An `LlmAgent` config without `model`: Go's `'model' is required for LlmAgent`.
const BROKEN_YAML: &str = "agent_class: LlmAgent\nname: broken\n";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Runs the binary in `cwd` with `args`, `$PWD` = `pwd`, a fake Gemini key and stdin closed.
fn adkcli(cwd: &Path, pwd: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_adkcli"))
        .args(args)
        .current_dir(cwd)
        .env("PWD", pwd)
        .env("GOOGLE_API_KEY", "fake-key")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_GENAI_USE_VERTEXAI")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Go's `log` prefix (`YYYY/MM/DD HH:MM:SS `) at each line start → `<ts> `.
fn mask_log_prefix(stderr: &[u8]) -> String {
    let re = Regex::new(r"(?m)^\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} ").unwrap();
    re.replace_all(&String::from_utf8_lossy(stderr), "<ts> ")
        .into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

/// The `full` launcher's syntax, as Go's `l.CommandLineSyntax()` ends a `Run failed` record.
fn syntax() -> String {
    gadk_launcher::full::new().command_line_syntax()
}

/// Go's `log.Printf` of `msg`: a trailing newline is added when missing.
fn log_line(msg: &str) -> String {
    if msg.ends_with('\n') {
        format!("<ts> {msg}")
    } else {
        format!("<ts> {msg}\n")
    }
}

/// A temp dir with `real/` and a symlink `link -> real`; returns (guard, real tree, link tree)
/// for the tree `name`.
fn symlinked_tree(name: &str) -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(real.join(name)).unwrap();
    std::os::unix::fs::symlink(&real, tmp.path().join("link")).unwrap();
    let link = format!("{}/link/{name}", tmp.path().display());
    (tmp, real.join(name), link)
}

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Go capture (tree A, `adkcli console`): lexical walk order (`B_upper` before `a_broken`),
/// a walk error reported and skipped, a directory named `root_agent.yaml` ignored, a failing
/// config logged and skipped, a duplicate folder name logged and skipped, then the console's
/// EOF exit; the working directory printed as `$PWD` spells it (through the symlink).
#[test]
fn stdout_lines_walk_errors_failures_and_duplicates_match_go() {
    let (_tmp, t, root) = symlinked_tree("treeA");
    write(&t.join("tools_agent/root_agent.yaml"), TOOLS_AGENT_YAML);
    write(&t.join("a_broken/root_agent.yaml"), BROKEN_YAML);
    write(
        &t.join("x/dup/root_agent.yaml"),
        "agent_class: SequentialAgent\nname: dup_x\n",
    );
    write(
        &t.join("y/dup/root_agent.yaml"),
        "agent_class: SequentialAgent\nname: dup_y\n",
    );
    write(
        &t.join("B_upper/root_agent.yaml"),
        "agent_class: SequentialAgent\nname: upper\n",
    );
    write(
        &t.join("locked/inner/root_agent.yaml"),
        "agent_class: SequentialAgent\nname: hidden\n",
    );
    std::fs::create_dir_all(t.join("z/root_agent.yaml")).unwrap();
    chmod(&t.join("locked"), 0o000);
    let locked_readable = std::fs::read_dir(t.join("locked")).is_ok();
    let out = adkcli(Path::new(&root), &root, &["console"]);
    chmod(&t.join("locked"), 0o755);
    if locked_readable {
        eprintln!("skipped: a mode-000 directory is readable (running as root)");
        return;
    }

    let want_stdout = "\
🔍 Scanning for 'root_agent.yaml' in: {root}
Warning: skipping \"{root}/locked\" due to error: open {root}/locked: permission denied
🚀 Found 5 agent config(s)
➡️  Loading agent from: {root}/B_upper/root_agent.yaml
✅ Agent loaded successfully: upper
✅ Agent folder name: B_upper
➡️  Loading agent from: {root}/a_broken/root_agent.yaml
➡️  Loading agent from: {root}/tools_agent/root_agent.yaml
✅ Agent loaded successfully: tools_agent
✅ Agent folder name: tools_agent
➡️  Loading agent from: {root}/x/dup/root_agent.yaml
✅ Agent loaded successfully: dup_x
✅ Agent folder name: dup
➡️  Loading agent from: {root}/y/dup/root_agent.yaml
✅ Agent loaded successfully: dup_y
✅ Agent folder name: dup


User ->\x20
EOF detected, exiting...
"
    .replace("{root}", &root);
    let want_stderr = "\
<ts> ⚠️  Error loading agent at {root}/a_broken/root_agent.yaml: 'model' is required for LlmAgent
<ts> ⚠️  Agent dup already exists, skipping
"
    .replace("{root}", &root);
    assert_eq!(stdout(&out), want_stdout);
    assert_eq!(mask_log_prefix(&out.stderr), want_stderr);
    assert_eq!(out.status.code(), Some(0));
}

/// Go capture (tree B): no `root_agent.yaml` file (a directory of that name does not count) →
/// Go's `❌` line and exit 1, before any launcher output.
#[test]
fn no_configs_exits_1_with_go_message() {
    let (_tmp, t, root) = symlinked_tree("treeB");
    std::fs::create_dir_all(t.join("z/root_agent.yaml")).unwrap();
    write(&t.join("z/other.yaml"), "x: 1\n");
    let out = adkcli(Path::new(&root), &root, &[]);
    assert_eq!(
        stdout(&out),
        format!(
            "🔍 Scanning for 'root_agent.yaml' in: {root}\n\
             ❌ No 'root_agent.yaml' files found in {root} or subdirectories\n"
        )
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    assert_eq!(out.status.code(), Some(1));
}

/// Go `os.Getwd`: a `$PWD` that does not name the working directory is ignored for the OS
/// working directory (symlinks resolved).
#[test]
fn pwd_not_naming_the_working_directory_is_ignored() {
    let (_tmp, t, root) = symlinked_tree("treeP");
    let out = adkcli(Path::new(&root), "/", &[]);
    let resolved = std::fs::canonicalize(&t).unwrap();
    assert_eq!(
        stdout(&out).lines().next().unwrap(),
        format!(
            "🔍 Scanning for 'root_agent.yaml' in: {}",
            resolved.display()
        )
    );
    assert_eq!(out.status.code(), Some(1));
}

/// Go capture (tree C, no arguments = console): every config fails, so the agent map is empty;
/// Go's console fails with `failed to create runner: root agent is required` (its
/// `runner.New` of a nil root agent) and adkcli exits 1 with `Run failed: …` + the syntax.
#[test]
fn all_configs_failing_console_fails_like_go() {
    let (_tmp, t, root) = symlinked_tree("treeC");
    write(&t.join("a/root_agent.yaml"), BROKEN_YAML);
    let out = adkcli(Path::new(&root), &root, &[]);
    assert_eq!(
        stdout(&out),
        format!(
            "🔍 Scanning for 'root_agent.yaml' in: {root}\n\
             🚀 Found 1 agent config(s)\n\
             ➡️  Loading agent from: {root}/a/root_agent.yaml\n"
        )
    );
    let want = log_line(&format!(
        "⚠️  Error loading agent at {root}/a/root_agent.yaml: 'model' is required for LlmAgent"
    )) + &log_line(&format!(
        "Run failed: failed to create runner: root agent is required\n\n{}",
        syntax()
    ));
    assert_eq!(mask_log_prefix(&out.stderr), want);
    assert_eq!(out.status.code(), Some(1));
}

/// Go capture (tree D, `adkcli console -nope`): the launcher's parse error is `Run failed: …`
/// + the syntax on stderr (after the flag package's own usage output), exit 1.
#[test]
fn launcher_error_prints_run_failed_with_syntax_and_exits_1() {
    let (_tmp, t, root) = symlinked_tree("treeD");
    write(
        &t.join("seq/root_agent.yaml"),
        "agent_class: SequentialAgent\nname: seq\n",
    );
    let out = adkcli(Path::new(&root), &root, &["console", "-nope"]);
    assert_eq!(
        stdout(&out),
        format!(
            "🔍 Scanning for 'root_agent.yaml' in: {root}\n\
             🚀 Found 1 agent config(s)\n\
             ➡️  Loading agent from: {root}/seq/root_agent.yaml\n\
             ✅ Agent loaded successfully: seq\n\
             ✅ Agent folder name: seq\n"
        )
    );
    let stderr = mask_log_prefix(&out.stderr);
    assert!(
        stderr.starts_with("flag provided but not defined: -nope\nUsage of console:\n"),
        "{stderr}"
    );
    let want_tail = log_line(&format!(
        "Run failed: failed to parse flags: flag provided but not defined: -nope\n\n{}",
        syntax()
    ));
    assert!(stderr.ends_with(&want_tail), "{stderr}");
    assert_eq!(out.status.code(), Some(1));
}

/// A free loopback port.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Kills the server child on drop (also when an assertion fails).
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Waits until `127.0.0.1:<port>` accepts a connection; fails when the child exits first.
fn wait_listening(server: &mut Server, port: u16, logs: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if let Some(status) = server.0.try_wait().unwrap() {
            panic!(
                "adkcli exited ({status}) before listening; stderr:\n{}",
                std::fs::read_to_string(logs.join("stderr")).unwrap_or_default()
            );
        }
        assert!(Instant::now() < deadline, "nothing listens on port {port}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// REST (`adkcli web -port <p> api`) replay of the Go-generated `runner-tools-agent` case: the
/// session state's `_adk_replay_config` names the case directory (relative to the working
/// directory, under `$PWD`'s symlinked spelling), the agent loaded from `tools_agent/`
/// matches the recorded request, and `/run` returns the recorded model text without calling
/// Gemini (the key is fake and there is no network). A request the recording does not hold
/// fails with Go's mismatch prefix.
#[tokio::test(flavor = "multi_thread")]
async fn rest_api_replays_a_go_recorded_case() {
    let (tmp, t, root) = symlinked_tree("treeR");
    write(&t.join("tools_agent/root_agent.yaml"), TOOLS_AGENT_YAML);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/recordings/runner-tools-agent/generated-recordings.yaml");
    write(
        &t.join("cases/runner-tools-agent/generated-recordings.yaml"),
        &std::fs::read_to_string(fixture).unwrap(),
    );
    let logs = tmp.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();

    let port = free_port();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_adkcli"))
            .args(["web", "-port", &port.to_string(), "api"])
            .current_dir(&root)
            .env("PWD", &root)
            .env("GOOGLE_API_KEY", "fake-key")
            .env_remove("GEMINI_API_KEY")
            .env_remove("GOOGLE_GENAI_USE_VERTEXAI")
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(logs.join("stdout")).unwrap())
            .stderr(std::fs::File::create(logs.join("stderr")).unwrap())
            .spawn()
            .unwrap(),
    );
    wait_listening(&mut server, port, &logs);

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://127.0.0.1:{port}/api");
    let create = |session: &'static str| {
        client
            .post(format!(
                "{base}/apps/tools_agent/users/u/sessions/{session}"
            ))
            .json(&json!({"state": {"_adk_replay_config": {
                "dir": "cases/runner-tools-agent",
                "user_message_index": 0,
            }}}))
            .send()
    };
    let run = |session: &'static str, text: &'static str| {
        client
            .post(format!("{base}/run"))
            .json(&json!({
                "appName": "tools_agent",
                "userId": "u",
                "sessionId": session,
                "newMessage": {"role": "user", "parts": [{"text": text}]},
            }))
            .send()
    };

    let resp = create("s1").await.unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let resp = run("s1", "Hi").await.unwrap();
    assert_eq!(resp.status(), 200);
    let events: Vec<Value> = resp.json().await.unwrap();
    let texts: Vec<(&str, &str)> = events
        .iter()
        .flat_map(|e| {
            let author = e["author"].as_str().unwrap_or_default();
            e["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(move |p| p["text"].as_str().map(|t| (author, t)))
        })
        .collect();
    assert_eq!(texts, [("tools_agent", "done")], "{events:#?}");

    // A different user message: the replayed request no longer matches the recording.
    let resp = create("s2").await.unwrap();
    assert_eq!(resp.status(), 200);
    let resp = run("s2", "Bye").await.unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("LLM request mismatch for agent 'tools_agent' (index 0):"),
        "{body}"
    );

    drop(server);
    let stdout = std::fs::read_to_string(logs.join("stdout")).unwrap();
    assert!(
        stdout.starts_with(&format!(
            "🔍 Scanning for 'root_agent.yaml' in: {root}\n🚀 Found 1 agent config(s)\n"
        )),
        "{stdout}"
    );
}
