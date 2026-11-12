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

//! Port of ADK-Go `cmd/adkgo/internal/root/root.go` and cobra's `Execute`: the command tree
//! `gadk` → `deploy` → {`agentengine`, `cloudrun`}, dispatch, and cobra's error reporting
//! (`Error: <msg>` and the failing command's usage on stderr, exit 1).

use std::ffi::OsString;

use clap::Command;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use gadk_platform::go_quote;

use crate::Env;
use crate::deploy::{self, agentengine, cloudrun};
use crate::flags::{STRAY_ARGS, pflag_error};

/// The root command's name (Go `RootCmd.Use` `adkgo`).
pub const NAME: &str = "gadk";
// Divergence (ledger 6b): the root texts name gadk (Go: "CLI tool for use with ADK-GO",
// "adkgo is a CLI tool …").
/// Go `RootCmd.Short`.
pub const SHORT: &str = "CLI tool for use with gadk";
/// Go `RootCmd.Long`.
pub const LONG: &str =
    "gadk is a CLI tool which allows developer to quickly deploy and test an agentic application";

/// Go `RootCmd` with `deploy` added.
// Divergence (ledger 6b): no `completion` command (cobra adds one to every root); clap's
// `help` subcommand stands in for cobra's.
pub fn command() -> Command {
    Command::new(NAME)
        .about(SHORT)
        .long_about(LONG)
        .subcommand(deploy::command())
}

/// Go `root.Execute()`: parses `args` (program name first), runs the selected command and
/// returns the exit code (0, or 1 after printing the error as cobra does).
pub async fn execute(args: Vec<OsString>, env: &Env) -> u8 {
    let raw: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // Divergence (ledger 6b): a non-UTF-8 argument is a clap parse error (Go keeps the bytes
    // and the shell-safe checks reject them later).
    let matches = match command().try_get_matches_from(&args) {
        Ok(m) => m,
        Err(e) => return report_parse_error(&e, &raw, env),
    };
    let Some(("deploy", deploy_m)) = matches.subcommand() else {
        // Go: the root command is not runnable, so cobra prints its help.
        env.out.write_str(&help(&[]));
        return 0;
    };
    let result = match deploy_m.subcommand() {
        Some(("cloudrun", m)) => {
            let mut flags = cloudrun::from_matches(m);
            (flags.deploy_on_cloud_run(env).await, "cloudrun")
        }
        Some(("agentengine", m)) => {
            let mut flags = agentengine::from_matches(m);
            (flags.deploy_on_agent_engine(env).await, "agentengine")
        }
        _ => {
            // Go `DeployCmd.RunE`: help when there are no arguments, else nothing.
            if deploy_m.get_many::<String>(STRAY_ARGS).is_none() {
                env.out.write_str(&help(&["deploy"]));
            }
            return 0;
        }
    };
    match result {
        (Ok(()), _) => 0,
        (Err(err), sub) => report(env, &err, &["deploy", sub]),
    }
}

/// cobra's error report: `Error: <msg>`, then the command's usage and an empty line.
fn report(env: &Env, msg: &str, path: &[&str]) -> u8 {
    env.err.write_str(&format!("Error: {msg}\n"));
    env.err.write_str(&format!("{}\n", usage(path)));
    1
}

fn report_parse_error(e: &clap::Error, raw: &[String], env: &Env) -> u8 {
    match e.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            env.out.write_str(&e.render().to_string());
            0
        }
        ErrorKind::InvalidSubcommand => {
            // Go `legacyArgs` on the root: the unknown command, cobra's suggestions, and a
            // pointer to `--help` instead of the usage.
            let name = match e.get(ContextKind::InvalidSubcommand) {
                Some(ContextValue::String(s)) => s.clone(),
                _ => raw.first().cloned().unwrap_or_default(),
            };
            env.err.write_str(&format!(
                "Error: unknown command {} for {}{}\n",
                go_quote(&name),
                go_quote(NAME),
                suggestions(&name)
            ));
            env.err
                .write_str(&format!("Run '{NAME} --help' for usage.\n"));
            1
        }
        _ => {
            let path = command_path(raw);
            let msg = pflag_error(e, &find(&path), raw);
            report(env, &msg, &path)
        }
    }
}

/// The subcommand path cobra's `Find` would select for `raw`: the first non-flag argument of
/// each level, when it names a subcommand (flags before `--` are skipped; `gadk` and
/// `deploy` have no value-taking flags).
fn command_path(raw: &[String]) -> Vec<&'static str> {
    let mut path = Vec::new();
    let mut cmd = command();
    for arg in raw {
        if arg == "--" {
            break;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            continue;
        }
        let Some(sub) = cmd.get_subcommands().find(|s| s.get_name() == arg).cloned() else {
            break;
        };
        path.push(match sub.get_name() {
            "deploy" => "deploy",
            "cloudrun" => "cloudrun",
            "agentengine" => "agentengine",
            _ => break,
        });
        cmd = sub;
    }
    path
}

/// The built (`gadk deploy …`-named) subcommand at `path`.
fn find(path: &[&str]) -> Command {
    let mut cmd = command();
    cmd.build();
    for name in path {
        let Some(sub) = cmd.find_subcommand(name).cloned() else {
            break;
        };
        cmd = sub;
    }
    cmd
}

/// cobra's `UsageString()` in clap's layout: the usage line and the argument list.
// Divergence (ledger 6b): clap's help and usage layout, not cobra's template.
fn usage(path: &[&str]) -> String {
    find(path)
        .help_template("{usage-heading} {usage}\n\n{all-args}")
        .render_help()
        .to_string()
}

/// cobra's help for the command at `path` in clap's layout.
fn help(path: &[&str]) -> String {
    find(path).render_long_help().to_string()
}

/// cobra `findSuggestions` on the root: each available subcommand within Levenshtein
/// distance 2 (case-insensitive) of `typed`, or that `typed` prefixes.
fn suggestions(typed: &str) -> String {
    let names: Vec<String> = command()
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .filter(|name| {
            levenshtein_ignore_case(typed, name) <= 2
                || name.to_lowercase().starts_with(&typed.to_lowercase())
        })
        .collect();
    if names.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\nDid you mean this?\n");
    for name in names {
        s.push_str(&format!("\t{name}\n"));
    }
    s
}

/// cobra `ld(s, t, true)`: byte-wise Levenshtein distance of the lowercased strings.
fn levenshtein_ignore_case(s: &str, t: &str) -> usize {
    let (s, t) = (s.to_lowercase().into_bytes(), t.to_lowercase().into_bytes());
    let mut prev: Vec<usize> = (0..=t.len()).collect();
    for (i, &sc) in s.iter().enumerate() {
        let mut cur = vec![i + 1; t.len() + 1];
        for (j, &tc) in t.iter().enumerate() {
            cur[j + 1] = if sc == tc {
                prev[j]
            } else {
                1 + prev[j].min(prev[j + 1]).min(cur[j])
            };
        }
        prev = cur;
    }
    prev[t.len()]
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::testutil::TestEnv;

    async fn run(args: &[&str]) -> (u8, String, String) {
        let t = TestEnv::builder("/w").build();
        let args: Vec<OsString> = std::iter::once("gadk")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let code = execute(args, &t.env).await;
        assert!(
            t.runner.calls().is_empty(),
            "a parse-only path ran a command"
        );
        (code, t.out.text(), t.err.text())
    }

    #[test]
    fn the_tree_is_gadk_deploy_agentengine_cloudrun_with_go_texts() {
        let root = command();
        assert_eq!(root.get_name(), "gadk");
        assert_eq!(
            root.get_about().map(ToString::to_string).as_deref(),
            Some(SHORT)
        );
        let deploy = root.find_subcommand("deploy").unwrap();
        assert_eq!(
            deploy.get_about().map(ToString::to_string).as_deref(),
            Some("Makes deployment to various platforms easy")
        );
        assert_eq!(
            deploy.get_long_about().map(ToString::to_string).as_deref(),
            Some("Please see subcommands for details")
        );
        let subs: Vec<&str> = deploy.get_subcommands().map(Command::get_name).collect();
        assert_eq!(subs, ["agentengine", "cloudrun"]);
        assert_eq!(
            deploy
                .find_subcommand("cloudrun")
                .unwrap()
                .get_about()
                .map(ToString::to_string)
                .as_deref(),
            Some("Deploys the application to cloudrun.")
        );
        assert_eq!(
            deploy
                .find_subcommand("agentengine")
                .unwrap()
                .get_about()
                .map(ToString::to_string)
                .as_deref(),
            Some("Deploys the application to Agent Engine.")
        );
        root.debug_assert();
    }

    /// Every Go flag with Go's name, shorthand and default, plus gadk's `--bin`.
    #[test]
    fn deploy_subcommands_register_go_flags() {
        let flags = |c: Command| -> Vec<(String, Option<char>, String)> {
            c.get_arguments()
                .filter(|a| a.get_long().is_some() && a.get_id() != "help")
                .map(|a| {
                    (
                        a.get_long().unwrap_or_default().to_string(),
                        a.get_short(),
                        a.get_default_values()
                            .iter()
                            .map(|v| v.to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join(","),
                    )
                })
                .collect()
        };
        let own = |v: &[(&str, Option<char>, &str)]| -> Vec<(String, Option<char>, String)> {
            v.iter()
                .map(|(l, s, d)| (l.to_string(), *s, d.to_string()))
                .collect()
        };
        assert_eq!(
            flags(cloudrun::command()),
            own(&[
                ("region", Some('r'), ""),
                ("project_name", Some('p'), ""),
                ("service_name", Some('s'), ""),
                ("temp_dir", Some('t'), ""),
                ("proxy_port", None, "8081"),
                ("server_port", None, "8080"),
                ("entry_point_path", Some('e'), ""),
                ("a2a", None, "true"),
                ("a2a_agent_url", Some('a'), "http://127.0.0.1:8081"),
                ("api", None, "true"),
                ("debug_api", None, "false"),
                ("webui", None, "true"),
                ("pubsub", None, "false"),
                ("pubsub_max_retries", None, "3"),
                ("pubsub_base_delay", None, "1s"),
                ("pubsub_max_delay", None, "10s"),
                ("pubsub_max_concurrent_runs", None, "100"),
                ("pubsub_oidc_audience", None, ""),
                ("pubsub_oidc_service_accounts", None, ""),
                ("eventarc", None, "false"),
                ("eventarc_max_retries", None, "3"),
                ("eventarc_base_delay", None, "1s"),
                ("eventarc_max_delay", None, "10s"),
                ("eventarc_max_concurrent_runs", None, "100"),
                ("eventarc_oidc_audience", None, ""),
                ("eventarc_oidc_service_accounts", None, ""),
                ("bin", None, ""),
            ])
        );
        assert_eq!(
            flags(agentengine::command()),
            own(&[
                ("region", Some('r'), ""),
                ("project_name", Some('p'), ""),
                ("name", Some('s'), ""),
                ("temp_dir", Some('t'), ""),
                ("server_port", None, "8080"),
                ("entry_point_path", Some('e'), ""),
                ("source_dir", Some('d'), ""),
                ("agent_engine_id", None, ""),
                ("mem_deploy", None, "false"),
                ("mem_model", None, ""),
                ("mem_ttl", None, "8760h0m0s"),
                ("bin", None, ""),
            ])
        );
    }

    #[tokio::test]
    async fn bare_gadk_and_bare_deploy_print_help_and_succeed() {
        let (code, out, err) = run(&[]).await;
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains(LONG), "{out}");
        assert!(out.contains("deploy"), "{out}");

        let (code, out, err) = run(&["deploy"]).await;
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("Please see subcommands for details"), "{out}");
        assert!(
            out.contains("agentengine") && out.contains("cloudrun"),
            "{out}"
        );
    }

    /// Go `DeployCmd.RunE` returns nil without printing when it has arguments.
    #[tokio::test]
    async fn deploy_with_a_stray_argument_does_nothing() {
        assert_eq!(
            run(&["deploy", "foo"]).await,
            (0, String::new(), String::new())
        );
        assert_eq!(
            run(&["deploy", "foo", "cloudrun"]).await,
            (0, String::new(), String::new())
        );
    }

    #[tokio::test]
    async fn help_flags_print_help_to_stdout() {
        let (code, out, err) = run(&["deploy", "cloudrun", "--help"]).await;
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("--entry_point_path"), "{out}");
        assert!(out.contains("Usage: gadk deploy cloudrun"), "{out}");
        let (code, out, _) = run(&["deploy", "agentengine", "-h"]).await;
        assert_eq!(code, 0);
        assert!(out.contains("--mem_ttl"), "{out}");
    }

    /// cobra: `Error: <pflag text>`, then the failing command's usage and an empty line, all
    /// on stderr; exit 1.
    #[tokio::test]
    async fn parse_errors_print_error_and_usage_to_stderr() {
        let (code, out, err) = run(&["deploy", "cloudrun", "--nope"]).await;
        assert_eq!((code, out.as_str()), (1, ""));
        let cloudrun_usage = usage(&["deploy", "cloudrun"]);
        assert!(
            cloudrun_usage.starts_with("Usage: gadk deploy cloudrun [OPTIONS]"),
            "{cloudrun_usage}"
        );
        assert!(
            cloudrun_usage.contains("--service_name"),
            "{cloudrun_usage}"
        );
        assert_eq!(
            err,
            format!("Error: unknown flag: --nope\n{cloudrun_usage}\n")
        );

        let (code, _, err) = run(&["deploy", "agentengine", "--server_port=x"]).await;
        assert_eq!(code, 1);
        assert_eq!(
            err,
            format!(
                "Error: invalid argument \"x\" for \"--server_port\" flag: strconv.ParseInt: parsing \"x\": invalid syntax\n{}\n",
                usage(&["deploy", "agentengine"])
            )
        );

        let (code, _, err) = run(&["deploy", "cloudrun", "-r"]).await;
        assert_eq!(code, 1);
        assert!(
            err.starts_with(
                "Error: flag needs an argument: 'r' in -r\nUsage: gadk deploy cloudrun"
            ),
            "{err}"
        );

        let (code, _, err) = run(&["deploy", "--region", "x", "cloudrun"]).await;
        assert_eq!(code, 1);
        assert_eq!(
            err,
            format!("Error: unknown flag: --region\n{}\n", usage(&["deploy"]))
        );
    }

    /// cobra `legacyArgs` on the root: no usage, a pointer to `--help`, suggestions when close.
    #[tokio::test]
    async fn an_unknown_root_command_is_cobra_unknown_command() {
        let (code, out, err) = run(&["foo"]).await;
        assert_eq!((code, out.as_str()), (1, ""));
        assert_eq!(
            err,
            "Error: unknown command \"foo\" for \"gadk\"\nRun 'gadk --help' for usage.\n"
        );
        let (_, _, err) = run(&["deplo"]).await;
        assert_eq!(
            err,
            "Error: unknown command \"deplo\" for \"gadk\"\n\nDid you mean this?\n\tdeploy\n\nRun 'gadk --help' for usage.\n"
        );
        let (_, _, err) = run(&["DEP"]).await;
        assert!(err.contains("Did you mean this?\n\tdeploy\n"), "{err}");
    }

    #[test]
    fn levenshtein_matches_cobra_ld() {
        assert_eq!(levenshtein_ignore_case("deplo", "deploy"), 1);
        assert_eq!(levenshtein_ignore_case("DEPLOY", "deploy"), 0);
        assert_eq!(levenshtein_ignore_case("dpeloy", "deploy"), 2);
        assert_eq!(levenshtein_ignore_case("", "abc"), 3);
        assert_eq!(levenshtein_ignore_case("kitten", "sitting"), 3);
    }

    #[test]
    fn command_path_follows_cobra_find() {
        let p = |a: &[&str]| command_path(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(p(&["deploy", "cloudrun", "--x"]), ["deploy", "cloudrun"]);
        assert_eq!(
            p(&["-h", "deploy", "agentengine"]),
            ["deploy", "agentengine"]
        );
        assert_eq!(p(&["deploy", "x", "cloudrun"]), ["deploy"]);
        assert_eq!(p(&["deploy", "--", "cloudrun"]), ["deploy"]);
        assert_eq!(p(&["nope"]), Vec::<&str>::new());
    }
}
