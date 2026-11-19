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

//! The root command and its dispatch: the command tree `gadk` → `deploy` →
//! {`agentengine`, `cloudrun`}, and the error reporting (`Error: <msg>` and the failing
//! command's usage on stderr, exit 1).

use std::ffi::OsString;

use clap::Command;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use gadk_platform::go_quote;

use crate::Env;
use crate::deploy::{self, agentengine, cloudrun};
use crate::flags::{STRAY_ARGS, pflag_error};

/// The root command's name.
pub const NAME: &str = "gadk";
// The root texts name gadk, not ADK's own tool.
/// The root command's short description.
pub const SHORT: &str = "CLI tool for use with gadk";
/// The root command's long description.
pub const LONG: &str =
    "gadk is a CLI tool which allows developer to quickly deploy and test an agentic application";

/// The root command with `deploy` added.
// There is no `completion` command; clap's `help` subcommand is the only extra one.
pub fn command() -> Command {
    Command::new(NAME)
        .about(SHORT)
        .long_about(LONG)
        .subcommand(deploy::command())
}

/// Parses `args` (program name first), runs the selected command and
/// returns the exit code (0, or 1 after printing the error).
pub async fn execute(args: Vec<OsString>, env: &Env) -> u8 {
    let raw: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let (path, rest) = match cobra_find(&raw) {
        Found::Command { path, rest } => (path, rest),
        Found::UnknownCommand(name) => return unknown_command(&name, env),
    };
    // clap parses the found command's own arguments: flags written before a subcommand name
    // move after it, so every remaining argument is parsed with the found command's flags.
    let argv: Vec<OsString> = args
        .iter()
        .take(1)
        .cloned()
        .chain(path.iter().map(OsString::from))
        .chain(rest.iter().map(|&i| args[i + 1].clone()))
        .collect();
    let parsed: Vec<String> = rest.iter().map(|&i| raw[i].clone()).collect();
    // A non-UTF-8 argument is a clap parse error.
    let matches = match command().try_get_matches_from(&argv) {
        Ok(m) => m,
        Err(e) => return report_parse_error(&e, &path, &parsed, env),
    };
    let Some(("deploy", deploy_m)) = matches.subcommand() else {
        // The root command is not runnable, so the help is printed.
        env.out.write_str(&help(&[]));
        return 0;
    };
    let result = match deploy_m.subcommand() {
        Some(("cloudrun", m)) => {
            let mut flags = cloudrun::from_matches(m);
            (flags.deploy_on_cloud_run(env).await, "cloudrun")
        }
        Some(("agentengine", m)) => {
            let result = match agentengine::validate_args(m) {
                Ok(()) => {
                    agentengine::from_matches(m)
                        .deploy_on_agent_engine(env)
                        .await
                }
                Err(e) => Err(e),
            };
            (result, "agentengine")
        }
        _ => {
            // `deploy` prints its help when there are no arguments, else nothing.
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

/// The error report: `Error: <msg>`, then the command's usage and an empty line.
fn report(env: &Env, msg: &str, path: &[&str]) -> u8 {
    env.err.write_str(&format!("Error: {msg}\n"));
    env.err.write_str(&format!("{}\n", usage(path)));
    1
}

fn report_parse_error(e: &clap::Error, path: &[&str], args: &[String], env: &Env) -> u8 {
    match e.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            env.out.write_str(&e.render().to_string());
            0
        }
        ErrorKind::InvalidSubcommand => {
            // clap's `help` subcommand naming an unknown command.
            let name = match e.get(ContextKind::InvalidSubcommand) {
                Some(ContextValue::String(s)) => s.clone(),
                _ => args.first().cloned().unwrap_or_default(),
            };
            unknown_command(&name, env)
        }
        _ => {
            let msg = pflag_error(e, &find(path), args);
            report(env, &msg, path)
        }
    }
}

/// An unknown command at the root: the error, suggestions, and a pointer to
/// `--help` instead of the usage.
fn unknown_command(name: &str, env: &Env) -> u8 {
    env.err.write_str(&format!(
        "Error: unknown command {} for {}{}\n",
        go_quote(name),
        go_quote(NAME),
        suggestions(name)
    ));
    env.err
        .write_str(&format!("Run '{NAME} --help' for usage.\n"));
    1
}

/// What the command search selects for the arguments after the program name.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// The command at `path`, to be parsed from the arguments at the indices `rest` (the
    /// arguments without the subcommand names, in their original order).
    Command {
        path: Vec<&'static str>,
        rest: Vec<usize>,
    },
    /// The first non-flag argument left at the root names no command.
    UnknownCommand(String),
}

// Finds the command for the arguments. At each
// level the first argument that is not a flag or a flag's value names the next subcommand,
// and that one occurrence is removed. Neither `gadk` nor `deploy` has a flag at that point
// (`-h/--help` is added to the found command only, later), so every `--x` and `-x`
// written without `=` takes the next argument as its value: `gadk deploy -r x cloudrun` runs
// cloudrun with region x, and `gadk -h deploy agentengine` is an unknown command
// "agentengine".
fn cobra_find(raw: &[String]) -> Found {
    let mut path: Vec<&'static str> = Vec::new();
    let mut rest: Vec<usize> = (0..raw.len()).collect();
    let mut cmd = command();
    loop {
        let args: Vec<&str> = rest.iter().map(|&i| raw[i].as_str()).collect();
        let Some(next) = strip_flags(&args).first().map(|s| s.to_string()) else {
            break;
        };
        let Some(sub) = cmd
            .get_subcommands()
            .find(|s| s.get_name() == next)
            .cloned()
        else {
            if !path.is_empty() {
                break;
            }
            if next == "help" {
                // clap's `help` subcommand parses the arguments as
                // given.
                return Found::Command {
                    path,
                    rest: (0..raw.len()).collect(),
                };
            }
            return Found::UnknownCommand(next);
        };
        let name = match sub.get_name() {
            "deploy" => "deploy",
            "cloudrun" => "cloudrun",
            "agentengine" => "agentengine",
            _ => break,
        };
        if let Some(pos) = position_of_first(&args, name) {
            rest.remove(pos);
        }
        path.push(name);
        cmd = sub;
    }
    if path.is_empty() {
        // The root is not runnable; the arguments after `--` are left alone and the
        // help is printed (the root has no positional arguments in clap).
        if let Some(end) = rest.iter().position(|&i| raw[i] == "--") {
            rest.truncate(end);
        }
    }
    Found::Command { path, rest }
}

/// Strips the flags from the arguments of a command without flags: `--x`/`-x` (no `=`) take the
/// next argument, `--` ends the flags, and the other non-flag arguments are returned.
fn strip_flags<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut commands = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let s = args[i];
        i += 1;
        if s == "--" {
            break;
        }
        if takes_next(s) {
            i += 1;
        } else if !s.is_empty() && !s.starts_with('-') {
            commands.push(s);
        }
    }
    commands
}

/// The position of the first argument equal to `x` that is not a flag or
/// a flag's value, before any `--`.
fn position_of_first(args: &[&str], x: &str) -> Option<usize> {
    let mut pos = 0;
    while pos < args.len() {
        let s = args[pos];
        if s == "--" {
            return None;
        }
        if takes_next(s) {
            pos += 2;
            continue;
        }
        if !s.starts_with('-') && s == x {
            return Some(pos);
        }
        pos += 1;
    }
    None
}

/// A flag that takes the next argument during the command search: `--x` or a two-byte `-x`,
/// without `=` (no flag of `gadk` or `deploy` has a no-option default at `Find` time).
fn takes_next(s: &str) -> bool {
    !s.contains('=') && (s.starts_with("--") || (s.starts_with('-') && s.len() == 2))
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

/// The usage string in clap's layout: the usage line and the argument list.
// The layout is clap's.
fn usage(path: &[&str]) -> String {
    find(path)
        .help_template("{usage-heading} {usage}\n\n{all-args}")
        .render_help()
        .to_string()
}

/// The help for the command at `path` in clap's layout.
fn help(path: &[&str]) -> String {
    find(path).render_long_help().to_string()
}

/// Suggestions for an unknown command at the root: each available subcommand within Levenshtein
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

/// Byte-wise Levenshtein distance of the lowercased strings.
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
                ("env", None, ""),
                ("default_env", None, "true"),
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
                "Error: invalid argument \"x\" for \"--server_port\" flag: parsing \"x\" as an integer: invalid syntax\n{}\n",
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

        // Go: cobra Find/stripFlags. A flag before the subcommand name swallows the name as its
        // value, so the parent command is found and fails on the flag.
        for flag in ["--nope", "--a2a", "--region"] {
            let (code, out, err) = run(&["deploy", flag, "cloudrun"]).await;
            assert_eq!((code, out.as_str()), (1, ""));
            assert_eq!(
                err,
                format!("Error: unknown flag: {flag}\n{}\n", usage(&["deploy"]))
            );
        }
        let (code, _, err) = run(&["--region", "x"]).await;
        assert_eq!(code, 1);
        assert_eq!(
            err,
            format!("Error: unknown flag: --region\n{}\n", usage(&[]))
        );
        let (code, _, err) = run(&["-x", "deploy"]).await;
        assert_eq!(code, 1);
        assert_eq!(
            err,
            format!("Error: unknown shorthand flag: 'x' in -x\n{}\n", usage(&[]))
        );
    }

    /// The arguments clap parses for `args` after cobra's `Find`: the command path, then the
    /// remaining arguments in their original order.
    fn found_argv(args: &[&str]) -> Vec<String> {
        let raw: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match cobra_find(&raw) {
            Found::Command { path, rest } => path
                .iter()
                .map(|p| p.to_string())
                .chain(rest.iter().map(|&i| raw[i].clone()))
                .collect(),
            Found::UnknownCommand(name) => panic!("unknown command {name:?}"),
        }
    }

    /// Go: cobra Find/stripFlags. Flags written before a subcommand name are parsed by the
    /// found subcommand (cobra v1.10.2: `adkgo deploy --region x cloudrun`, `adkgo --region x
    /// deploy cloudrun` and `adkgo deploy -r x cloudrun` all run cloudrun with region "x").
    #[test]
    fn flags_before_the_subcommand_name_reach_the_subcommand() {
        for args in [
            &["deploy", "--region", "x", "cloudrun"][..],
            &["--region", "x", "deploy", "cloudrun"],
            &["deploy", "-r", "x", "cloudrun"],
            &["-r", "x", "deploy", "cloudrun"],
        ] {
            let argv = found_argv(args);
            assert_eq!(argv[..2], ["deploy", "cloudrun"], "{args:?}");
            let m = command()
                .try_get_matches_from(std::iter::once("gadk".to_string()).chain(argv))
                .unwrap_or_else(|e| panic!("{args:?}: {e}"));
            let (_, deploy_m) = m.subcommand().unwrap();
            let (name, leaf) = deploy_m.subcommand().unwrap();
            assert_eq!(name, "cloudrun");
            assert_eq!(crate::flags::string_value(leaf, "region"), "x", "{args:?}");
        }
        // The flag's value may itself be a subcommand name; the next occurrence is the command.
        assert_eq!(
            found_argv(&[
                "--region",
                "cloudrun",
                "deploy",
                "cloudrun",
                "--server_port",
                "9"
            ]),
            [
                "deploy",
                "cloudrun",
                "--region",
                "cloudrun",
                "--server_port",
                "9"
            ]
        );
        assert_eq!(
            found_argv(&["deploy", "--server_port=9", "agentengine", "x"]),
            ["deploy", "agentengine", "--server_port=9", "x"]
        );
        assert_eq!(
            found_argv(&["deploy", "-rx", "cloudrun"]),
            ["deploy", "cloudrun", "-rx"]
        );
    }

    /// cobra registers `-h/--help` only on the found command, so during `Find` it swallows the
    /// next argument like any other flag (cobra v1.10.2 output).
    #[tokio::test]
    async fn a_help_flag_before_the_subcommand_name_is_a_flag_of_the_parent() {
        let (code, out, err) = run(&["-h", "deploy", "agentengine"]).await;
        assert_eq!((code, out.as_str()), (1, ""));
        assert_eq!(
            err,
            "Error: unknown command \"agentengine\" for \"gadk\"\nRun 'gadk --help' for usage.\n"
        );
        let (code, out, err) = run(&["deploy", "-h", "cloudrun"]).await;
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("Usage: gadk deploy [COMMAND]"), "{out}");
        let (code, out, err) = run(&["-h", "foo"]).await;
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("Usage: gadk [COMMAND]"), "{out}");
        let (code, out, err) = run(&["--", "foo"]).await;
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains(LONG), "{out}");
        let (code, _, err) = run(&["--region", "x", "foo"]).await;
        assert_eq!(code, 1);
        assert!(
            err.starts_with("Error: unknown command \"foo\" for \"gadk\"\n"),
            "{err}"
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
    fn cobra_find_follows_cobra() {
        let f = |a: &[&str]| cobra_find(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let cmd = |path: &[&'static str], rest: &[usize]| Found::Command {
            path: path.to_vec(),
            rest: rest.to_vec(),
        };
        assert_eq!(
            f(&["deploy", "cloudrun", "--x"]),
            cmd(&["deploy", "cloudrun"], &[2])
        );
        assert_eq!(
            f(&["-h", "deploy", "agentengine"]),
            Found::UnknownCommand("agentengine".into())
        );
        assert_eq!(f(&["deploy", "x", "cloudrun"]), cmd(&["deploy"], &[1, 2]));
        assert_eq!(f(&["deploy", "--", "cloudrun"]), cmd(&["deploy"], &[1, 2]));
        assert_eq!(f(&["deploy", "-h", "cloudrun"]), cmd(&["deploy"], &[1, 2]));
        assert_eq!(f(&["--", "deploy"]), cmd(&[], &[]));
        assert_eq!(f(&["help", "deploy"]), cmd(&[], &[0, 1]));
        assert_eq!(f(&["nope"]), Found::UnknownCommand("nope".into()));
        assert_eq!(f(&[]), cmd(&[], &[]));
    }
}
