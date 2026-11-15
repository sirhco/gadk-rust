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

//! `gadk deploy agentengine` acceptance tests (R9–R13) and the fixtures the Go test ports in
//! `parity_tests/cmd_adkgo_internal_deploy_agentengine_agentengine.rs` share: a fixture Cargo
//! workspace, a runner answering `cargo metadata`, Go's `resetFlags`, and a fake Vertex AI
//! (gadk-model's `FakeServer` on a socket; `ScriptedClient` under paused time).

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{Local, SecondsFormat};
use flate2::read::GzDecoder;
use gadk_model::rest::test_util::{FakeResponse, FakeServer, ScriptedClient};
use gadk_platform::GoDuration;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::*;
use crate::testutil::{
    Canned, RecordingRunner, TestEnv, cargo_metadata_json, cargo_metadata_runner, default_now,
    write_cargo_workspace,
};
use crate::util::{GREEN, RED, RESET};

pub(crate) const PARENT: &str = "projects/my-project/locations/us-central1";
pub(crate) const ENGINE: &str = "projects/my-project/locations/us-central1/reasoningEngines/42";
const OP: &str = "projects/my-project/locations/us-central1/reasoningEngines/42/operations/7";
/// Go `url.Values.Encode` of `$alt=json;enum-encoding=int`.
const ALT_QUERY: &str = "%24alt=json%3Benum-encoding%3Dint";

/// Writes the one-package Cargo workspace `agent` (member `agent`) at `root`.
pub(crate) fn write_workspace(root: &Path) {
    write_cargo_workspace(root, "agent", "agent");
}

/// A runner answering every `cargo metadata` as [`cargo_metadata_json`] for the
/// `--manifest-path` it is asked about (so any `-e` resolves to `package` with `bins`).
pub(crate) fn metadata_runner(
    workspace_root: &Path,
    package: &str,
    bins: &[&str],
    deps: &[PathBuf],
) -> RecordingRunner {
    let root = workspace_root.to_path_buf();
    let package = package.to_string();
    let bins: Vec<String> = bins.iter().map(|b| b.to_string()).collect();
    let deps = deps.to_vec();
    cargo_metadata_runner(
        move |cmd| {
            let manifest = PathBuf::from(cmd.args.last().cloned().unwrap_or_default());
            let bins: Vec<&str> = bins.iter().map(String::as_str).collect();
            cargo_metadata_json(&root, &manifest, &package, &bins, &deps)
        },
        |_| Canned::success(Vec::new()),
    )
}

/// A [`TestEnv`] at `cwd` whose `cargo metadata` resolves to package `package` with `bins` in
/// the workspace at `workspace_root`.
pub(crate) fn test_env(cwd: &Path, workspace_root: &Path, package: &str, bins: &[&str]) -> TestEnv {
    TestEnv::builder(cwd)
        .runner(metadata_runner(workspace_root, package, bins, &[]))
        .build()
}

/// Go `resetFlags`: the zero value plus the entry point and a temp dir parent.
pub(crate) fn reset_flags(entry_point: &str, temp_dir: &Path) -> AgentEngineFlags {
    let mut f = AgentEngineFlags::default();
    f.source.entry_point_path = entry_point.to_string();
    f.build.temp_dir = temp_dir.to_string_lossy().into_owned();
    f
}

/// The names in `dir`.
pub(crate) fn dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The `(name, contents)` of every entry of the gzip tar `data`, in archive order.
fn tar_members(data: &[u8]) -> Vec<(String, String)> {
    let mut archive = tar::Archive::new(GzDecoder::new(data));
    archive
        .entries()
        .unwrap()
        .map(|e| {
            let mut e = e.unwrap();
            let name = e.path().unwrap().to_string_lossy().into_owned();
            let mut body = String::new();
            e.read_to_string(&mut body).unwrap();
            (name, body)
        })
        .collect()
}

/// Parses `args` (after `agentengine`) with the real command.
fn parse(args: &[&str]) -> AgentEngineFlags {
    let m = command()
        .try_get_matches_from(std::iter::once("agentengine").chain(args.iter().copied()))
        .unwrap();
    from_matches(&m)
}

/// One `log_start_stop` step as Go prints it: the banner, each printer line, an empty line,
/// the success banner.
fn step(name: &str, lines: &[String]) -> String {
    let mut s = format!("{name} : {GREEN}Starting{RESET}\n");
    for line in lines {
        s.push_str(&format!("    {GREEN}> {RESET} [{line}]\n"));
    }
    s.push('\n');
    s.push_str(&format!("{name} : {GREEN}Finished successfully{RESET}\n"));
    s
}

/// The temp dir `compute_flags` printed.
fn printed_temp_dir(out: &str) -> String {
    let start = out.find("[Using temp dir: ").unwrap() + "[Using temp dir: ".len();
    let end = start + out[start..].find(']').unwrap();
    out[start..end].to_string()
}

/// Go `time.Now().Format(time.RFC3339)` of the fixed clock, in local time.
pub(crate) fn local_rfc3339() -> String {
    default_now()
        .with_timezone(&Local)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A done operation whose response is the engine `ENGINE` named `my-agent`.
fn done_op() -> Value {
    json!({
        "name": OP,
        "done": true,
        "response": {
            "@type": "type.googleapis.com/google.cloud.aiplatform.v1beta1.ReasoningEngine",
            "name": ENGINE,
            "displayName": "my-agent",
        },
    })
}

/// A fake Vertex AI answering one create/update with a pending operation, then the done one.
pub(crate) fn fake_vertex() -> FakeServer {
    FakeServer::sequence(vec![
        FakeResponse::json(json!({"name": OP}).to_string()),
        FakeResponse::json(done_op().to_string()),
    ])
}

/// Points `t` at `server` without credentials.
pub(crate) fn use_fake_vertex(t: &mut TestEnv, server: &FakeServer) {
    t.env.vertex = ReasoningEngineOptions {
        endpoint: Some(server.url()),
        without_authentication: true,
        ..Default::default()
    };
}

/// The class methods as Go's `json.Marshal` prints them.
fn methods_json() -> String {
    String::from_utf8(
        gadk_model::to_go_json(&gadk_server::agentengine::list_class_methods()).unwrap(),
    )
    .unwrap()
}

/// A workspace `ws` (with a user `Dockerfile`, `.git`, `target`) in a fresh dir; returns the
/// dir and `ws`.
fn workspace_fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    write_workspace(&ws);
    fs::write(ws.join("Dockerfile"), "FROM user\n").unwrap();
    fs::create_dir_all(ws.join(".git")).unwrap();
    fs::write(ws.join(".git/HEAD"), "ref\n").unwrap();
    fs::create_dir_all(ws.join("target/release")).unwrap();
    fs::write(ws.join("target/release/agent"), "bin").unwrap();
    (dir, ws)
}

/// Every flag lands in its Go field (`--bin` in gadk's); without `--env`/`--default_env` the
/// environment is the defaults and an update keeps the deployed one.
#[test]
fn from_matches_reads_every_flag() {
    let f = parse(&[
        "-r",
        "us-central1",
        "-p",
        "my-project",
        "-s",
        "my-agent",
        "-t",
        "/tmp/t",
        "--server_port",
        "9090",
        "-e",
        "./agent",
        "-d",
        "..",
        "--agent_engine_id",
        "42",
        "--mem_deploy",
        "--mem_model",
        "m",
        "--mem_ttl",
        "1h",
        "--bin",
        "b",
    ]);
    let want = AgentEngineFlags {
        gcloud: GCloudFlags {
            region: "us-central1".into(),
            project_name: "my-project".into(),
        },
        agent_engine: AgentEngineServiceFlags {
            name: "my-agent".into(),
            display_name: String::new(),
            server_port: 9090,
            agent_engine_id: "42".into(),
            memory_bank: MemoryBankFlags {
                deploy: true,
                model: "m".into(),
                ttl: GoDuration(3_600_000_000_000),
            },
            env: Vec::new(),
            default_env: true,
            env_vars: Vec::new(),
            update_env: false,
        },
        build: BuildFlags {
            temp_dir: "/tmp/t".into(),
            ..BuildFlags::default()
        },
        source: SourceFlags {
            entry_point_path: "./agent".into(),
            source_dir: "..".into(),
            ..SourceFlags::default()
        },
        bin: "b".into(),
        target: None,
    };
    assert_eq!(f, want);
}

/// `--env` is repeatable and kept verbatim (no comma splitting, `-`-leading values); either
/// environment flag on the command line, even at its default, sets `update_env` (cobra
/// `Changed`); `--default_env false` leaves `false` as a positional.
#[test]
fn environment_flags_parse_like_pflag() {
    let f = parse(&["--env", "A=1,2", "--env=B=-x", "--env", "-C=3"]);
    assert_eq!(
        f.agent_engine.env,
        [
            OsString::from("A=1,2"),
            OsString::from("B=-x"),
            OsString::from("-C=3")
        ]
    );
    assert!(f.agent_engine.default_env && f.agent_engine.update_env);
    for (args, default_env, update_env) in [
        (&[][..], true, false),
        (&["--default_env=true"][..], true, true),
        (&["--default_env"][..], true, true),
        (&["--default_env=false"][..], false, true),
        (&["--default_env=0"][..], false, true),
    ] {
        let f = parse(args);
        assert_eq!(
            (f.agent_engine.default_env, f.agent_engine.update_env),
            (default_env, update_env),
            "{args:?}"
        );
    }
    let m = command()
        .try_get_matches_from(["agentengine", "--default_env", "false"])
        .unwrap();
    assert!(bool_value(&m, "default_env"));
    assert_eq!(
        validate_args(&m),
        Err("accepts 0 arg(s), received 1".to_string())
    );
}

/// R8: an update with the environment and the memory bank masks the environment before the
/// memory bank's `spec.contextSpec` (Go appends `spec.deployment_spec.env` first).
#[tokio::test]
async fn update_masks_environment_before_memory_bank() {
    let (dir, ws) = workspace_fixture();
    let server = fake_vertex();
    let mut t = test_env(&ws, &ws, "agent", &["agent"]);
    use_fake_vertex(&mut t, &server);
    let mut f = parse(&[
        "-r",
        "us-central1",
        "-p",
        "my-project",
        "-t",
        &dir.path().display().to_string(),
        "--agent_engine_id",
        "42",
        "--mem_deploy",
        "--default_env=false",
        "--env=ONLY=custom",
    ]);

    f.deploy_on_agent_engine(&t.env).await.unwrap();

    let update = &server.requests()[0];
    assert_eq!(
        update.query.as_deref(),
        Some(
            "%24alt=json%3Benum-encoding%3Dint\
             &updateMask=spec.sourceCodeSpec%2Cspec.classMethods%2Cspec.deploymentSpec.env%2Cspec.contextSpec"
        )
    );
    let body = update.json();
    assert_eq!(
        body["spec"]["deploymentSpec"],
        json!({"env": [{"name": "ONLY", "value": "custom"}]})
    );
    assert_eq!(
        body["contextSpec"],
        json!({"memoryBankConfig": {"ttlConfig": {"defaultTtl": "31536000s"}}})
    );
    assert!(!t.out.text().contains("custom"));
}

/// R9: Go's derived fields, the `agentEngine_<local stamp>__*` temp dir, the display-name
/// default and the step output.
#[tokio::test]
async fn compute_flags_sets_the_derived_fields() {
    let (dir, ws) = workspace_fixture();
    let temp = dir.path().join("t");
    fs::create_dir(&temp).unwrap();
    let t = test_env(&ws, &ws, "agent", &["agent"]);
    let mut f = reset_flags("agent", &temp);

    f.compute_flags(&t.env).await.unwrap();

    let tmp = f.build.temp_dir.clone();
    let stamp = default_now()
        .with_timezone(&Local)
        .format("agentEngine_%Y%m%d_%H%M%S__")
        .to_string();
    assert!(
        tmp.starts_with(&format!("{}/{stamp}", temp.display())),
        "{tmp}"
    );
    assert!(Path::new(&tmp).is_dir());
    assert_eq!(f.build.exec_file, "agent");
    assert_eq!(f.build.exec_path, format!("{tmp}/agent"));
    assert_eq!(f.build.dockerfile_build_path, format!("{tmp}/Dockerfile"));
    assert_eq!(f.build.archive_path, format!("{tmp}/archive.tgz"));
    assert_eq!(f.source.orig_entry_point_path, "agent");
    assert_eq!(
        f.source.entry_point_path,
        ws.join("agent").display().to_string()
    );
    assert_eq!(
        f.source.src_base_path,
        ws.join("agent").display().to_string()
    );
    assert_eq!(f.source.source_dir, ws.display().to_string());
    assert_eq!(
        f.agent_engine.display_name,
        format!("ADK Agent: {}", local_rfc3339())
    );
    let target = f.target.clone().unwrap();
    assert_eq!(
        (target.package.as_str(), target.bin.as_str()),
        ("agent", "agent")
    );
    assert_eq!(
        t.out.text(),
        step(
            "Computing flags & preparing temp",
            &[format!("Using temp dir: {tmp}")]
        )
    );
    let calls = t.runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].args,
        vec![
            "cargo".to_string(),
            "metadata".into(),
            "--format-version".into(),
            "1".into(),
            "--no-deps".into(),
            "--manifest-path".into(),
            ws.join("agent/Cargo.toml").display().to_string(),
        ]
    );
}

/// Go: an explicit `--name` is the display name.
#[tokio::test]
async fn compute_flags_uses_the_name_as_display_name() {
    let (dir, ws) = workspace_fixture();
    let t = test_env(&ws, &ws, "agent", &["agent"]);
    let mut f = reset_flags("agent", dir.path());
    f.agent_engine.name = "my-agent".into();

    f.compute_flags(&t.env).await.unwrap();

    assert_eq!(f.agent_engine.display_name, "my-agent");
}

/// The Agent Engine Dockerfile (R10) after `compute_flags` with `-d` given as `source_dir`.
async fn dockerfile_for(source_dir: &str, cwd: &Path, ws: &Path, temp: &Path) -> String {
    fs::write(
        ws.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.95.0\"\n",
    )
    .unwrap();
    let t = test_env(cwd, ws, "agent", &["agent-bin"]);
    let mut f = reset_flags(&ws.join("agent").display().to_string(), temp);
    f.source.source_dir = source_dir.into();
    f.agent_engine.server_port = 8080;
    f.compute_flags(&t.env).await.unwrap();
    f.prepare_dockerfile(&t.env).await.unwrap();
    assert!(t.out.text().contains(&format!(
        "    {GREEN}> {RESET} [Writing: {}]\n",
        f.build.dockerfile_build_path
    )));
    fs::read_to_string(&f.build.dockerfile_build_path).unwrap()
}

/// R9/R10: `-d` equal to the workspace root builds in `/app/`.
#[tokio::test]
async fn source_dir_equal_to_the_workspace_root_builds_in_app() {
    let (dir, ws) = workspace_fixture();
    let got = dockerfile_for(".", &ws, &ws, dir.path()).await;
    assert_eq!(
        got,
        r#"
FROM rust:1.95.0-bookworm AS builder
WORKDIR /app/
COPY . /app/
RUN cargo build --release --locked --target-dir /gadk-target -p agent --bin agent-bin

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /gadk-target/release/agent-bin  /app/agent-bin
EXPOSE 8080
# Command to run the executable when the container starts
CMD ["/app/agent-bin", "web", "-host", "0.0.0.0", "-port", "8080", "agentengine"]"#
    );
}

/// R9/R10: an ancestor `-d` builds in `/app/<workspace root below it>`.
#[tokio::test]
async fn source_dir_ancestor_builds_in_the_workspace_below_app() {
    let (dir, ws) = workspace_fixture();
    let got = dockerfile_for("..", &ws, &ws, dir.path()).await;
    assert_eq!(
        got,
        r#"
FROM rust:1.95.0-bookworm AS builder
WORKDIR /app/ws
COPY . /app/
RUN cargo build --release --locked --target-dir /gadk-target -p agent --bin agent-bin

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /gadk-target/release/agent-bin  /app/agent-bin
EXPOSE 8080
# Command to run the executable when the container starts
CMD ["/app/agent-bin", "web", "-host", "0.0.0.0", "-port", "8080", "agentengine"]"#
    );
}

/// R9: a `-d` that is neither the workspace root nor an ancestor is rejected before the temp
/// dir exists.
#[tokio::test]
async fn source_dir_unrelated_is_rejected_before_the_temp_dir() {
    let (dir, ws) = workspace_fixture();
    let other = dir.path().join("other");
    fs::create_dir(&other).unwrap();
    let temp = dir.path().join("t");
    fs::create_dir(&temp).unwrap();
    let t = test_env(&ws, &ws, "agent", &["agent"]);
    let mut f = reset_flags("agent", &temp);
    f.source.source_dir = "../other".into();

    let err = f.compute_flags(&t.env).await.unwrap_err();

    assert_eq!(
        err,
        format!(
            "--source_dir \"../other\" is neither the Cargo workspace root \"{}\" nor an ancestor of it",
            ws.display()
        )
    );
    assert!(dir_names(&temp).is_empty());
    assert!(t.out.text().ends_with(&format!(
        "Computing flags & preparing temp : {RED}Finished with error{RESET}\nError: {err}\n"
    )));
}

/// R9: the workspace root below `-d` reaches `WORKDIR`, so it gets the shell-safe check.
#[tokio::test]
async fn source_dir_with_an_unsafe_workdir_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("my ws");
    write_workspace(&ws);
    let temp = dir.path().join("t");
    fs::create_dir(&temp).unwrap();
    let t = test_env(&ws, &ws, "agent", &["agent"]);
    let mut f = reset_flags("agent", &temp);
    f.source.source_dir = "..".into();

    let err = f.compute_flags(&t.env).await.unwrap_err();

    assert!(
        err.starts_with("--source_dir \"my ws\" contains a character not allowed"),
        "{err}"
    );
    assert!(dir_names(&temp).is_empty());
}

/// R4: a `path` dependency outside the archive root is rejected; an ancestor `-d` that
/// contains it is accepted.
#[tokio::test]
async fn path_dependencies_must_lie_in_the_archive_root() {
    let (dir, ws) = workspace_fixture();
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let temp = dir.path().join("t");
    fs::create_dir(&temp).unwrap();
    let runner = || metadata_runner(&ws, "agent", &["agent"], std::slice::from_ref(&outside));

    let t = TestEnv::builder(&ws).runner(runner()).build();
    let mut f = reset_flags("agent", &temp);
    let err = f.compute_flags(&t.env).await.unwrap_err();
    assert_eq!(
        err,
        format!(
            "path dependency \"{}\" is outside \"{}\" and would not be staged",
            outside.display(),
            ws.display()
        )
    );
    assert!(dir_names(&temp).is_empty());

    let t = TestEnv::builder(&ws).runner(runner()).build();
    let mut f = reset_flags("agent", &temp);
    f.source.source_dir = "..".into();
    f.compute_flags(&t.env).await.unwrap();
}

/// R10: the archive is the sorted archive root without `.git`, `target`, the nested temp dir
/// and the root `Dockerfile`, then the generated `Dockerfile` last.
#[tokio::test]
async fn create_archive_appends_the_generated_dockerfile_last() {
    let (_dir, ws) = workspace_fixture();
    fs::write(ws.join("agent/Dockerfile"), "FROM nested\n").unwrap();
    fs::create_dir(ws.join("tmp")).unwrap();
    let t = test_env(&ws, &ws, "agent", &["agent"]);
    let mut f = reset_flags("agent", &ws.join("tmp"));
    f.agent_engine.server_port = 8080;
    f.compute_flags(&t.env).await.unwrap();
    f.prepare_dockerfile(&t.env).await.unwrap();

    f.create_archive(&t.env).await.unwrap();

    assert!(t.out.text().ends_with(&step(
        "Creating source archive",
        &[format!("Creating: {}", f.build.archive_path)]
    )));
    let members = tar_members(&fs::read(&f.build.archive_path).unwrap());
    let names: Vec<&str> = members.iter().map(|m| m.0.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Cargo.lock",
            "Cargo.toml",
            "agent",
            "agent/Cargo.toml",
            "agent/Dockerfile",
            "agent/src",
            "agent/src/main.rs",
            "tmp",
            "Dockerfile",
        ]
    );
    let generated = fs::read_to_string(&f.build.dockerfile_build_path).unwrap();
    assert_eq!(members.last().unwrap().1, generated);
    assert_eq!(members[4].1, "FROM nested\n");
}

/// R12: no `contextSpec` content beyond the TTL unless a model is set.
#[test]
fn memory_bank_context_spec_ttl_and_model() {
    let mut f = AgentEngineFlags::default();
    f.agent_engine.memory_bank.ttl = GoDuration(GoDuration::HOUR.0 * 24 * 365);
    assert_eq!(
        f.memory_bank_context_spec(),
        json!({"memoryBankConfig": {"ttlConfig": {"defaultTtl": "31536000s"}}})
    );
    f.agent_engine.memory_bank.model = "projects/p/locations/r/publishers/google/models/m".into();
    f.agent_engine.memory_bank.ttl = GoDuration(1_500_000_000);
    assert_eq!(
        f.memory_bank_context_spec(),
        json!({"memoryBankConfig": {
            "ttlConfig": {"defaultTtl": "1.500s"},
            "generationConfig": {"model": "projects/p/locations/r/publishers/google/models/m"},
        }})
    );
}

/// Go protojson `Duration`: 0, 3, 6 or 9 fractional digits.
#[test]
fn protojson_duration_matches_go() {
    for (d, want) in [
        (0, "0s"),
        (1_000_000_000, "1s"),
        (1_500_000_000, "1.500s"),
        (1_000_001_000, "1.000001s"),
        (1_000_000_001, "1.000000001s"),
        (-1, "-0.000000001s"),
        (-1_500_000_000, "-1.500s"),
        (86_400_000_000_000, "86400s"),
    ] {
        assert_eq!(protojson_duration(GoDuration(d)), want, "{d}");
    }
}

/// The create request body the fake server received, with its `sourceArchive` decoded.
fn split_archive(mut body: Value) -> (Value, Vec<u8>) {
    let archive = body["spec"]["sourceCodeSpec"]["inlineSource"]["sourceArchive"]
        .take()
        .as_str()
        .map(|s| STANDARD.decode(s).unwrap())
        .unwrap();
    (body, archive)
}

/// R11/R13 through `gadk deploy agentengine`: the create request (path, raw query, routing
/// header, body), the poll, Go's output byte for byte, and the temp dir removed.
#[tokio::test]
async fn deploy_creates_the_reasoning_engine() {
    let (dir, ws) = workspace_fixture();
    let temp = dir.path().join("t");
    fs::create_dir(&temp).unwrap();
    let server = fake_vertex();
    let mut t = test_env(&ws, &ws, "agent", &["agent"]);
    use_fake_vertex(&mut t, &server);
    let temp_arg = temp.display().to_string();
    let args = [
        "gadk",
        "deploy",
        "agentengine",
        "-r",
        "us-central1",
        "-p",
        "my-project",
        "-s",
        "my-agent",
        "-t",
        &temp_arg,
    ];

    let code = crate::root::execute(args.iter().map(Into::into).collect(), &t.env).await;

    assert_eq!(code, 0, "{}", t.out.text());
    assert_eq!(t.err.text(), "");
    let out = t.out.text();
    let tmp = printed_temp_dir(&out);
    let expected = [
        step(
            "Computing flags & preparing temp",
            &[format!("Using temp dir: {tmp}")],
        ),
        step(
            "Preparing Dockerfile",
            &[format!("Writing: {tmp}/Dockerfile")],
        ),
        step(
            "Creating source archive",
            &[format!("Creating: {tmp}/archive.tgz")],
        ),
        step(
            "Deploying to Agent Engine",
            &[
                format!("Methods: {}", methods_json()),
                "Sending CreateReasoningEngine request...".into(),
                "Waiting for operation to complete...".into(),
                format!("Deployed Reasoning Engine: {ENGINE}"),
                "Display Name: my-agent".into(),
            ],
        ),
        step(
            "Cleaning temp",
            &[format!("Clean temp starting with {tmp}")],
        ),
    ]
    .concat();
    assert_eq!(out, expected);
    assert!(!Path::new(&tmp).exists(), "temp dir removed after success");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let create = &requests[0];
    assert_eq!(create.method, "POST");
    assert_eq!(create.path, format!("/v1beta1/{PARENT}/reasoningEngines"));
    assert_eq!(create.query.as_deref(), Some(ALT_QUERY));
    assert_eq!(
        create.header("x-goog-request-params"),
        "parent=projects%2Fmy-project%2Flocations%2Fus-central1"
    );
    let (body, archive) = split_archive(create.json());
    assert_eq!(
        body,
        json!({
            "displayName": "my-agent",
            "spec": {
                "sourceCodeSpec": {"inlineSource": {"sourceArchive": null}, "imageSpec": {}},
                "agentFramework": "google-adk",
                "deploymentSpec": {"env": [
                    {"name": "GOOGLE_CLOUD_REGION", "value": "us-central1"},
                    {"name": "NUM_WORKERS", "value": "1"},
                    {"name": "GOOGLE_CLOUD_AGENT_ENGINE_ENABLE_TELEMETRY", "value": "true"},
                    {"name": "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "value": "true"},
                ]},
                "classMethods": gadk_server::agentengine::list_class_methods(),
            },
        })
    );
    let names: Vec<String> = tar_members(&archive).into_iter().map(|m| m.0).collect();
    assert_eq!(
        names,
        vec![
            "Cargo.lock",
            "Cargo.toml",
            "agent",
            "agent/Cargo.toml",
            "agent/src",
            "agent/src/main.rs",
            "Dockerfile",
        ]
    );
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, format!("/ui/{OP}"));
}

/// R12: `--mem_deploy` adds the top-level `contextSpec`, the model prefixed with the project
/// and region.
#[tokio::test]
async fn deploy_with_memory_bank_sends_the_context_spec() {
    let (dir, ws) = workspace_fixture();
    let server = fake_vertex();
    let mut t = test_env(&ws, &ws, "agent", &["agent"]);
    use_fake_vertex(&mut t, &server);
    let mut f = parse(&[
        "-r",
        "us-central1",
        "-p",
        "my-project",
        "-t",
        &dir.path().display().to_string(),
        "--mem_deploy",
        "--mem_model",
        "publishers/google/models/some-model",
        "--mem_ttl",
        "24h",
    ]);

    f.deploy_on_agent_engine(&t.env).await.unwrap();

    let body = server.requests()[0].json();
    assert_eq!(
        body["contextSpec"],
        json!({"memoryBankConfig": {
            "ttlConfig": {"defaultTtl": "86400s"},
            "generationConfig": {"model": "projects/my-project/locations/us-central1/publishers/google/models/some-model"},
        }})
    );
    assert_eq!(
        body["displayName"],
        format!("ADK Agent: {}", local_rfc3339())
    );
}

/// R11 update with `--agent_engine_id` and `--mem_deploy`: PATCH of the engine name, Go's
/// mask (protojson form, `spec.contextSpec` appended) in the raw query, the routing header,
/// the body; Go's update output.
#[tokio::test]
async fn update_patches_the_reasoning_engine() {
    let (dir, ws) = workspace_fixture();
    let server = fake_vertex();
    let mut t = test_env(&ws, &ws, "agent", &["agent"]);
    use_fake_vertex(&mut t, &server);
    let mut f = parse(&[
        "-r",
        "us-central1",
        "-p",
        "my-project",
        "-t",
        &dir.path().display().to_string(),
        "--agent_engine_id",
        "42",
        "--mem_deploy",
    ]);

    f.deploy_on_agent_engine(&t.env).await.unwrap();

    let update = &server.requests()[0];
    assert_eq!(update.method, "PATCH");
    assert_eq!(update.path, format!("/v1beta1/{ENGINE}"));
    assert_eq!(
        update.query.as_deref(),
        Some(
            "%24alt=json%3Benum-encoding%3Dint\
             &updateMask=spec.sourceCodeSpec%2Cspec.classMethods%2Cspec.contextSpec"
        )
    );
    assert_eq!(
        update.header("x-goog-request-params"),
        "reasoning_engine.name=projects%2Fmy-project%2Flocations%2Fus-central1%2FreasoningEngines%2F42"
    );
    let (body, archive) = split_archive(update.json());
    assert_eq!(
        body,
        json!({
            "name": ENGINE,
            "spec": {
                "sourceCodeSpec": {"inlineSource": {"sourceArchive": null}, "imageSpec": {}},
                "classMethods": gadk_server::agentengine::list_class_methods(),
            },
            "contextSpec": {"memoryBankConfig": {"ttlConfig": {"defaultTtl": "31536000s"}}},
        })
    );
    assert_eq!(tar_members(&archive).last().unwrap().0, "Dockerfile");
    let out = t.out.text();
    assert!(out.contains(&step(
        "Updating Agent Engine",
        &[
            format!("Methods: {}", methods_json()),
            "Sending UpdateReasoningEngine request...".into(),
            "Waiting for operation to complete...".into(),
            format!("Updated Reasoning Engine: {ENGINE}"),
            "Display Name: my-agent".into(),
        ],
    )));
    assert!(!out.contains("Deploying to Agent Engine"));
}

/// Without `--mem_deploy` the update masks only the source and the class methods.
#[tokio::test]
async fn update_without_memory_bank_masks_source_and_methods() {
    let (dir, ws) = workspace_fixture();
    let server = fake_vertex();
    let mut t = test_env(&ws, &ws, "agent", &["agent"]);
    use_fake_vertex(&mut t, &server);
    let mut f = parse(&[
        "-r",
        "us-central1",
        "-p",
        "my-project",
        "-t",
        &dir.path().display().to_string(),
        "--agent_engine_id",
        "42",
    ]);

    f.deploy_on_agent_engine(&t.env).await.unwrap();

    let update = &server.requests()[0];
    assert_eq!(
        update.query.as_deref(),
        Some(
            "%24alt=json%3Benum-encoding%3Dint&updateMask=spec.sourceCodeSpec%2Cspec.classMethods"
        )
    );
    assert_eq!(update.json().get("contextSpec"), None);
}

/// R11/R13: a failed operation is `operation failed: <status>`, the deploy stops and the temp
/// dir is kept.
#[tokio::test]
async fn failed_operation_keeps_the_temp_dir() {
    let (dir, ws) = workspace_fixture();
    let server = FakeServer::sequence(vec![
        FakeResponse::json(json!({"name": OP}).to_string()),
        FakeResponse::json(
            json!({"name": OP, "done": true, "error": {"code": 9, "message": "build failed"}})
                .to_string(),
        ),
    ]);
    let mut t = test_env(&ws, &ws, "agent", &["agent"]);
    use_fake_vertex(&mut t, &server);
    let mut f = parse(&[
        "-r",
        "us-central1",
        "-p",
        "my-project",
        "-t",
        &dir.path().display().to_string(),
    ]);

    let err = f.deploy_on_agent_engine(&t.env).await.unwrap_err();

    assert_eq!(
        err,
        "operation failed: rpc error: code = FailedPrecondition desc = build failed"
    );
    let out = t.out.text();
    assert!(out.ends_with(&format!(
        "Deploying to Agent Engine : {RED}Finished with error{RESET}\nError: {err}\n"
    )));
    assert!(!out.contains("Cleaning temp"));
    assert!(Path::new(&f.build.temp_dir).is_dir(), "temp dir kept");
    assert!(Path::new(&f.build.archive_path).is_file());
}

/// Go `CreateReasoningEngine failed: %w`.
#[tokio::test]
async fn create_error_is_wrapped() {
    let (dir, ws) = workspace_fixture();
    let server = FakeServer::always(FakeResponse::json_status(
        403,
        r#"{"error":{"code":403,"message":"denied","status":"PERMISSION_DENIED"}}"#,
    ));
    let mut t = test_env(&ws, &ws, "agent", &["agent"]);
    use_fake_vertex(&mut t, &server);
    let mut f = parse(&[
        "-r",
        "us-central1",
        "-p",
        "my-project",
        "-t",
        &dir.path().display().to_string(),
    ]);

    let err = f.deploy_on_agent_engine(&t.env).await.unwrap_err();

    assert_eq!(
        err,
        "CreateReasoningEngine failed: rpc error: code = PermissionDenied desc = denied"
    );
    assert_eq!(server.requests().len(), 1);
}

/// Go `cannot create ReasoningEngineClient: %w`; ADC reads `Env::env` (not the process
/// environment) when the Vertex options name none.
#[tokio::test]
async fn client_error_is_wrapped_and_adc_reads_the_injected_environment() {
    let dir = tempfile::tempdir().unwrap();
    let creds = dir.path().join("creds.json");
    fs::write(&creds, r#"{"type":"bogus"}"#).unwrap();
    fs::write(dir.path().join("archive.tgz"), "x").unwrap();
    let t = TestEnv::builder(dir.path())
        .vars(HashMap::from([(
            "GOOGLE_APPLICATION_CREDENTIALS".to_string(),
            creds.display().to_string(),
        )]))
        .build();
    let mut f = AgentEngineFlags::default();
    f.gcloud.region = "us-central1".into();
    f.build.archive_path = dir.path().join("archive.tgz").display().to_string();

    let err = f.deploy_to_agent_engine(&t.env).await.unwrap_err();

    assert_eq!(
        err,
        "cannot create ReasoningEngineClient: unknown credential type: bogus"
    );
}

/// R11: the wait is bounded by 45 minutes; expiry is Go's
/// `operation failed: context deadline exceeded`.
#[tokio::test(start_paused = true)]
async fn operation_wait_is_bounded_by_45_minutes() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("archive.tgz"), "x").unwrap();
    let scripted = ScriptedClient::new(|req| {
        if req.method == "POST" {
            FakeResponse::json(json!({"name": OP}).to_string())
        } else {
            FakeResponse::json(json!({"name": OP, "done": false}).to_string())
        }
    });
    let mut t = TestEnv::builder(dir.path()).build();
    t.env.vertex = ReasoningEngineOptions {
        without_authentication: true,
        http_client: Some(scripted.clone()),
        ..Default::default()
    };
    let mut f = AgentEngineFlags::default();
    f.gcloud.region = "us-central1".into();
    f.gcloud.project_name = "my-project".into();
    f.build.archive_path = dir.path().join("archive.tgz").display().to_string();
    let start = tokio::time::Instant::now();

    let err = f.deploy_to_agent_engine(&t.env).await.unwrap_err();

    assert_eq!(err, "operation failed: context deadline exceeded");
    assert!(start.elapsed() >= Duration::from_secs(45 * 60));
    assert!(start.elapsed() < Duration::from_secs(46 * 60));
    assert!(scripted.requests().len() > 2, "the operation was polled");
}
