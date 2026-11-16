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

//! Port of ADK-Go `cmd/adkgo/internal/deploy/agentengine/environment_test.go`: the Agent
//! Engine deployment environment (#1981).
//!
//! Go's `environmentCommand` runs the real `agentEngineCmd.Args`/`RunE` on a cobra command
//! with the environment flags; here the commands run through [`crate::root::execute`] (the
//! real `gadk deploy agentengine`), with the CLI's stdout and stderr captured by [`TestEnv`].
//! Go's "reached entry-point processing" (`origEntryPointPath` set) is gadk's first
//! `cargo metadata` call on the recording runner (target resolution is compute's first step
//! after the environment). Go's gRPC fake Reasoning Engine service is gadk-model's REST
//! `FakeServer`, and the requests are compared with Go goldens
//! (`testdata/environment/<scenario>.json`, recorded from ADK-Go).

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::Value;

use crate::deploy::agentengine::environment::{DEFAULT_ENV_HELP, ENV_HELP};
use crate::deploy::agentengine::tests::{
    dir_names, fake_vertex, local_rfc3339, test_env, use_fake_vertex, write_workspace,
};
use crate::deploy::agentengine::{command, from_matches};
use crate::testutil::TestEnv;

/// Runs `gadk deploy agentengine -t <temp> -e not-a-go-file <args>` with a recording runner
/// that answers nothing; returns the exit code, stdout, stderr and the test env.
async fn execute(temp: &Path, args: Vec<OsString>) -> (u8, String, String, TestEnv) {
    let t = TestEnv::builder(temp).build();
    let argv: Vec<OsString> = ["gadk", "deploy", "agentengine", "-t"]
        .into_iter()
        .map(OsString::from)
        .chain([
            temp.as_os_str().to_owned(),
            "-e".into(),
            "not-a-go-file".into(),
        ])
        .chain(args)
        .collect();
    let code = crate::root::execute(argv, &t.env).await;
    let (out, err) = (t.out.text(), t.err.text());
    (code, out, err, t)
}

fn os_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[test]
fn test_environment_flags_registered() {
    let cmd = command();
    let arg = |name: &str| {
        cmd.get_arguments()
            .find(|a| a.get_long() == Some(name))
            .unwrap_or_else(|| panic!("agentengine --{name} is not registered"))
    };
    // pflag `StringArrayVar`: repeatable, one value per occurrence, help type `stringArray`.
    let env = arg("env");
    assert!(matches!(env.get_action(), clap::ArgAction::Append));
    assert_eq!(env.get_num_args(), Some(1.into()));
    assert_eq!(env.get_value_names().unwrap_or_default(), ["stringArray"]);
    assert_eq!(
        env.get_help().map(ToString::to_string),
        Some(ENV_HELP.into())
    );
    // pflag `BoolVar` defaulting to true.
    let default_env = arg("default_env");
    assert_eq!(default_env.get_value_names().unwrap_or_default(), ["bool"]);
    assert_eq!(default_env.get_default_values(), ["true"]);
    assert_eq!(
        default_env.get_help().map(ToString::to_string),
        Some(DEFAULT_ENV_HELP.into())
    );
}

// parity: adapted: run through `gadk deploy agentengine`; "reached entry-point processing" is the first `cargo metadata` call on the recording runner
#[tokio::test]
async fn test_environment_flags_reject_invalid_before_preparing_deployment() {
    let cases: [(&str, Vec<OsString>); 7] = [
        ("missing equals", os_args(&["--env=DO_NOT_LOG_THIS_VALUE"])),
        ("empty name", os_args(&["--env==DO_NOT_LOG_THIS_VALUE"])),
        (
            "leading digit",
            os_args(&["--env=9INVALID=DO_NOT_LOG_THIS_VALUE"]),
        ),
        (
            "hyphen",
            os_args(&["--env=INVALID-NAME=DO_NOT_LOG_THIS_VALUE"]),
        ),
        (
            "space",
            os_args(&["--env=INVALID NAME=DO_NOT_LOG_THIS_VALUE"]),
        ),
        (
            "duplicate",
            os_args(&[
                "--env=VALID=DO_NOT_LOG_THIS_VALUE",
                "--env=VALID=OTHER_PRIVATE_VALUE",
            ]),
        ),
        (
            "invalid UTF-8",
            vec![OsString::from_vec(
                b"--env=VALID=DO_NOT_LOG_THIS_VALUE\xff".to_vec(),
            )],
        ),
    ];
    for (name, args) in cases {
        let dir = tempfile::tempdir().unwrap();
        let (code, out, err, t) = execute(dir.path(), args).await;
        assert_eq!(code, 1, "{name}");
        assert!(
            err.starts_with("Error: --env entry "),
            "{name}: want an --env validation error, got {err}"
        );
        let output = format!("{out}{err}");
        assert!(
            !output.contains("DO_NOT_LOG_THIS_VALUE") && !output.contains("OTHER_PRIVATE_VALUE"),
            "{name}: CLI printed an environment value"
        );
        assert_eq!(
            dir_names(dir.path()),
            Vec::<String>::new(),
            "{name}: invalid --env created temporary entries"
        );
        assert!(
            t.runner.calls().is_empty(),
            "{name}: invalid --env reached entry-point processing"
        );
    }
}

// parity: adapted: run through `gadk deploy agentengine`; Go's `updateEnv` check is that nothing ran (no output, no runner call)
#[tokio::test]
async fn test_environment_flags_reject_stray_arguments() {
    for (name, args) in [
        ("space-separated false", &["--default_env", "false"][..]),
        ("space-separated true", &["--default_env", "true"][..]),
        (
            "extra environment",
            &["--env", "VALID=accepted", "OTHER=DO_NOT_LOG_THIS_VALUE"][..],
        ),
        (
            "interspersed argument",
            &["DO_NOT_LOG_THIS_VALUE", "--env=VALID=accepted"][..],
        ),
        (
            "after separator",
            &["--env=VALID=accepted", "--", "DO_NOT_LOG_THIS_VALUE"][..],
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (code, out, err, t) = execute(dir.path(), os_args(args)).await;
        assert_eq!(code, 1, "{name}: accepted a positional argument");
        let msg = err.lines().next().unwrap_or_default();
        assert_eq!(msg, "Error: accepts 0 arg(s), received 1", "{name}");
        assert!(
            !msg.contains("DO_NOT_LOG_THIS_VALUE")
                && !msg.contains("false")
                && !msg.contains("true"),
            "{name}: argument error echoed a positional value"
        );
        assert!(
            !format!("{out}{err}").contains("DO_NOT_LOG_THIS_VALUE"),
            "{name}: CLI printed a positional value"
        );
        assert_eq!(
            out, "",
            "{name}: positional argument reached deployment preparation"
        );
        assert!(t.runner.calls().is_empty(), "{name}");
        assert_eq!(
            dir_names(dir.path()),
            Vec::<String>::new(),
            "{name}: positional argument created temporary deployment files"
        );
    }
}

/// The scenarios of Go's table, in its order (golden file stems).
const SCENARIOS: [&str; 14] = [
    "create_defaults",
    "create_overrides_and_preserves_values",
    "create_without_defaults",
    "create_empty_environment",
    "create_private_and_unicode_values",
    "create_preserves_reference_order",
    "create_default_override_follows_its_reference",
    "update_preserves_environment_by_default",
    "update_overrides_defaults",
    "update_custom_environment",
    "update_private_value",
    "update_preserves_reference_order",
    "update_explicitly_enables_defaults",
    "update_clears_environment",
];

/// The Go golden of scenario `stem`.
fn golden(stem: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/environment")
        .join(format!("{stem}.json"));
    serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap()
}

/// The decoded `updateMask` of a raw query.
fn update_mask(query: &str) -> String {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("updateMask="))
        .unwrap_or_default()
        .replace("%2C", ",")
}

// parity: adapted: the flags parse through the real clap command (not Execute to a bounded entry point); REST FakeServer instead of gRPC; create/update bodies and the update mask equal the Go goldens
#[tokio::test]
async fn test_environment_deployment_requests() {
    for stem in SCENARIOS {
        let g = golden(stem);
        let args: Vec<String> = g["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap().to_string())
            .collect();
        let update = g["update"].as_bool().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        write_workspace(&ws);
        let temp = dir.path().join("t");
        fs::create_dir(&temp).unwrap();
        let server = fake_vertex();
        let mut t = test_env(&ws, &ws, "agent", &["agent"]);
        use_fake_vertex(&mut t, &server);

        let m = command()
            .try_get_matches_from(
                std::iter::once("agentengine").chain(args.iter().map(String::as_str)),
            )
            .unwrap();
        let mut f = from_matches(&m);
        f.gcloud.project_name = g["projectName"].as_str().unwrap().into();
        f.gcloud.region = g["region"].as_str().unwrap().into();
        f.source.entry_point_path = "agent".into();
        f.build.temp_dir = temp.to_string_lossy().into_owned();
        f.compute_flags(&t.env).await.unwrap();
        fs::write(&f.build.archive_path, "test archive").unwrap();

        let mut want = g["request"].clone();
        if update {
            f.agent_engine.agent_engine_id = g["agentEngineId"].as_str().unwrap().into();
            f.update_agent_engine(&t.env).await.unwrap();
            let req = &server.requests()[0];
            assert_eq!(req.method, "PATCH", "{stem}");
            assert_eq!(
                update_mask(req.query.as_deref().unwrap_or_default()),
                want["updateMask"].as_str().unwrap(),
                "{stem}: update mask"
            );
            assert_eq!(req.json(), want["reasoningEngine"], "{stem}: update body");
        } else {
            f.deploy_to_agent_engine(&t.env).await.unwrap();
            let req = &server.requests()[0];
            assert_eq!(req.method, "POST", "{stem}");
            assert_eq!(
                req.path,
                format!(
                    "/v1beta1/{}/reasoningEngines",
                    want["parent"].as_str().unwrap()
                ),
                "{stem}: create parent"
            );
            want["reasoningEngine"]["displayName"] =
                format!("ADK Agent: {}", local_rfc3339()).into();
            assert_eq!(req.json(), want["reasoningEngine"], "{stem}: create body");
        }

        let output = format!("{}{}", t.out.text(), t.err.text());
        assert!(
            !output.contains("only-custom")
                && !output.contains("https://example.test/")
                && !output.contains("DO_NOT_LOG_THIS_VALUE"),
            "{stem}: CLI printed an environment value"
        );
    }
}
