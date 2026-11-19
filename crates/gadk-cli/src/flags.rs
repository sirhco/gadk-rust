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

//! Command-line flag helpers for clap: the flag kinds the `gadk deploy` commands register
//! (string, int, duration and bool flags) as clap builder [`Arg`]s that parse like ADK's CLI
//! flags, the value readers, [`passed`] (the flag was on the command line) and the mapping of
//! clap's parse errors to ADK's error texts ([`pflag_error`]).
//!
//! Rules reproduced: `--f=v` and `--f v`, `-f v`, `-fv` and `-f=v`; a value-taking flag
//! takes the next argument even when it starts with `-`; a bool flag takes a value only as
//! `--b=v`, so `--b v` sets it true and leaves `v` as an argument; a
//! repeated flag keeps its last value; ints are parsed with the base chosen by the prefix
//! (`0x`, `0o`, `0b`, a leading `0` for octal, else decimal) into an `i64`, bools accept
//! `1 t T TRUE true True 0 f F FALSE false False`, and durations are signed sequences of
//! decimal numbers with unit suffixes (`ns`, `us`, `ms`, `s`, `m`, `h`) such as `2h45m`; stray
//! positional arguments are accepted and ignored.

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use gadk_platform::go_flag::{go_parse_bool, go_parse_int};
use gadk_platform::{GoDuration, go_parse_duration, go_quote};

/// The id of the hidden trailing positional that swallows stray arguments.
pub const STRAY_ARGS: &str = "args";

/// A subcommand whose flags are POSIX-style: a repeated flag overrides the
/// earlier value instead of being a clap error, and there is no `help`
/// subcommand (only the root has one).
pub fn pflag_command(name: &'static str) -> Command {
    Command::new(name)
        .args_override_self(true)
        .disable_help_subcommand(true)
}

/// A value-taking flag: exactly one value, which may start with `-` (the next
/// argument is taken whatever it is).
fn value_flag(
    long: &'static str,
    short: Option<char>,
    default: &'static str,
    help: &'static str,
    value_name: &'static str,
    zero: &'static str,
) -> Arg {
    let mut arg = Arg::new(long)
        .long(long)
        .help(help)
        .value_name(value_name)
        .action(ArgAction::Set)
        .num_args(1)
        .allow_hyphen_values(true)
        .default_value(default)
        // A `(default …)` is shown only for a non-zero default.
        .hide_default_value(default == zero);
    if let Some(c) = short {
        arg = arg.short(c);
    }
    arg
}

/// A string flag.
pub fn string_flag(
    long: &'static str,
    short: Option<char>,
    default: &'static str,
    help: &'static str,
) -> Arg {
    value_flag(long, short, default, help, "string", "")
}

/// An int flag: the value is an `i64` with the base chosen by its prefix (`0x`, `0o`, `0b`, a
/// leading `0` for octal, else decimal).
pub fn int_flag(
    long: &'static str,
    short: Option<char>,
    default: &'static str,
    help: &'static str,
) -> Arg {
    value_flag(long, short, default, help, "int", "0").value_parser(go_parse_int)
}

/// A duration flag: the value is a signed sequence of decimal numbers with unit suffixes
/// (`ns`, `us`, `ms`, `s`, `m`, `h`); `default` is the text form of the default
/// (e.g. `1s`, `8760h0m0s`).
pub fn duration_flag(
    long: &'static str,
    short: Option<char>,
    default: &'static str,
    help: &'static str,
) -> Arg {
    value_flag(long, short, default, help, "duration", "0s").value_parser(go_parse_duration)
}

/// A bool flag: `--b` is true, `--b=<v>` takes a bool value (`1 t T TRUE true True 0 f F FALSE
/// false False`); a value is never taken from the next argument.
pub fn bool_flag(long: &'static str, default: bool, help: &'static str) -> Arg {
    Arg::new(long)
        .long(long)
        .help(help)
        .value_name("bool")
        .action(ArgAction::Set)
        .num_args(0..=1)
        .require_equals(true)
        .default_missing_value("true")
        .default_value(if default { "true" } else { "false" })
        .hide_default_value(!default)
        .value_parser(go_parse_bool)
}

/// The hidden trailing positional: commands without a positional-argument validator accept and
/// ignore positional arguments.
pub fn stray_args() -> Arg {
    Arg::new(STRAY_ARGS)
        .num_args(0..)
        .action(ArgAction::Append)
        .hide(true)
}

/// Whether the flag was on the command line.
pub fn passed(m: &ArgMatches, id: &str) -> bool {
    matches!(m.value_source(id), Some(ValueSource::CommandLine))
}

/// A [`string_flag`]'s value (its default when absent).
pub fn string_value(m: &ArgMatches, id: &str) -> String {
    m.try_get_one::<String>(id)
        .ok()
        .flatten()
        .cloned()
        .unwrap_or_default()
}

/// An [`int_flag`]'s value.
pub fn int_value(m: &ArgMatches, id: &str) -> i64 {
    m.try_get_one::<i64>(id)
        .ok()
        .flatten()
        .copied()
        .unwrap_or_default()
}

/// A [`duration_flag`]'s value.
pub fn duration_value(m: &ArgMatches, id: &str) -> GoDuration {
    m.try_get_one::<GoDuration>(id)
        .ok()
        .flatten()
        .copied()
        .unwrap_or_default()
}

/// A [`bool_flag`]'s value.
pub fn bool_value(m: &ArgMatches, id: &str) -> bool {
    m.try_get_one::<bool>(id)
        .ok()
        .flatten()
        .copied()
        .unwrap_or_default()
}

fn context_str(e: &clap::Error, kind: ContextKind) -> Option<&str> {
    match e.get(kind) {
        Some(ContextValue::String(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// A rune quoted with single quotes, as in `'x'`.
fn quote_rune(c: char) -> String {
    let q = go_quote(&c.to_string());
    let inner = &q[1..q.len() - 1];
    let inner = if inner == "\\\"" { "\"" } else { inner };
    if inner == "'" {
        return "'\\''".to_string();
    }
    format!("'{inner}'")
}

/// The long name in clap's rendering of an argument (`--region <string>`,
/// `--a2a[=<bool>]`).
fn long_name(rendered: &str) -> &str {
    let s = rendered.trim_start_matches('-');
    let end = s.find([' ', '[', '=']).unwrap_or(s.len());
    &s[..end]
}

/// ADK's error text for clap's parse error `e` on `cmd` (the subcommand being parsed);
/// `raw` are the command-line arguments, used where clap does not keep what ADK's text reports
/// (the token an unknown shorthand came from, whether a value-less flag was written short).
/// Errors with no ADK text keep clap's message.
pub fn pflag_error(e: &clap::Error, cmd: &Command, raw: &[String]) -> String {
    match e.kind() {
        ErrorKind::UnknownArgument => {
            let arg = context_str(e, ContextKind::InvalidArg).unwrap_or_default();
            if let Some(name) = arg.strip_prefix("--") {
                if name.is_empty() || name.starts_with(['-', '=']) {
                    return format!("bad flag syntax: {arg}");
                }
                let name = name.split('=').next().unwrap_or(name);
                return format!("unknown flag: --{name}");
            }
            if let Some(c) = arg.strip_prefix('-').and_then(|s| s.chars().next()) {
                // ADK names the shorthands left in the token from the unknown one on; an
                // unknown shorthand is always first in its token here (every gadk shorthand
                // takes a value, which swallows the rest of its token).
                let start = format!("-{c}");
                let shorthands = raw
                    .iter()
                    .find(|t| t.starts_with(&start) && !t.starts_with("--"))
                    .map(|t| &t[1..])
                    .unwrap_or(&arg[1..]);
                return format!("unknown shorthand flag: {} in -{shorthands}", quote_rune(c));
            }
            clap_message(e)
        }
        ErrorKind::InvalidValue
            if context_str(e, ContextKind::InvalidValue) == Some("")
                && std::error::Error::source(e).is_none() =>
        {
            // A value-taking flag was the last argument (any later one would be its value).
            match raw.last() {
                Some(last) if last.starts_with("--") => {
                    format!(
                        "flag needs an argument: {}",
                        last.split('=').next().unwrap_or(last)
                    )
                }
                Some(last) if last.starts_with('-') => {
                    let c = last.chars().last().unwrap_or('-');
                    format!("flag needs an argument: {} in -{c}", quote_rune(c))
                }
                _ => clap_message(e),
            }
        }
        ErrorKind::ValueValidation => {
            let rendered = context_str(e, ContextKind::InvalidArg).unwrap_or_default();
            let long = long_name(rendered);
            let short = cmd
                .get_arguments()
                .find(|a| a.get_long() == Some(long))
                .and_then(Arg::get_short);
            let flag = match short {
                Some(s) => format!("-{s}, --{long}"),
                None => format!("--{long}"),
            };
            let value = context_str(e, ContextKind::InvalidValue).unwrap_or_default();
            let cause = std::error::Error::source(e)
                .map(ToString::to_string)
                .unwrap_or_default();
            format!(
                "invalid argument {} for {} flag: {cause}",
                go_quote(value),
                go_quote(&flag)
            )
        }
        _ => clap_message(e),
    }
}

/// clap's own message for `e`: its first line without the `error: ` prefix.
fn clap_message(e: &clap::Error) -> String {
    let rendered = e.render().to_string();
    let first = rendered.lines().next().unwrap_or_default();
    first.strip_prefix("error: ").unwrap_or(first).to_string()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn cmd() -> Command {
        pflag_command("t")
            .arg(string_flag("region", Some('r'), "", "GCP Region"))
            .arg(string_flag("pubsub_oidc_audience", None, "", "aud"))
            .arg(string_flag("eventarc_oidc_audience", None, "", "aud"))
            .arg(string_flag(
                "a2a_agent_url",
                Some('a'),
                "http://127.0.0.1:8081",
                "url",
            ))
            .arg(int_flag("server_port", None, "8080", "port"))
            .arg(duration_flag("mem_ttl", None, "8760h0m0s", "ttl"))
            .arg(bool_flag("a2a", true, "Enable A2A"))
            .arg(bool_flag("pubsub", false, "Enable PubSub"))
            .arg(stray_args())
    }

    fn parse(args: &[&str]) -> Result<ArgMatches, String> {
        let raw: Vec<String> = std::iter::once("t")
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect();
        let c = cmd();
        c.clone()
            .try_get_matches_from(&raw)
            .map_err(|e| pflag_error(&e, &c, &raw[1..]))
    }

    fn stray(m: &ArgMatches) -> Vec<String> {
        m.get_many::<String>(STRAY_ARGS)
            .map(|v| v.cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn defaults_are_go_defaults_and_not_passed() {
        let m = parse(&[]).unwrap();
        assert_eq!(string_value(&m, "region"), "");
        assert_eq!(string_value(&m, "a2a_agent_url"), "http://127.0.0.1:8081");
        assert_eq!(int_value(&m, "server_port"), 8080);
        assert_eq!(
            duration_value(&m, "mem_ttl"),
            GoDuration(8760 * 3600 * 1_000_000_000)
        );
        assert!(bool_value(&m, "a2a"));
        assert!(!bool_value(&m, "pubsub"));
        for id in [
            "region",
            "a2a_agent_url",
            "server_port",
            "mem_ttl",
            "a2a",
            "pubsub",
        ] {
            assert!(!passed(&m, id), "{id}");
        }
        assert_eq!(stray(&m), Vec::<String>::new());
    }

    #[test]
    fn bool_flags_take_a_value_only_after_equals() {
        let m = parse(&["--a2a=false"]).unwrap();
        assert!(!bool_value(&m, "a2a"));
        assert!(passed(&m, "a2a"));
        let m = parse(&["--pubsub"]).unwrap();
        assert!(bool_value(&m, "pubsub"));
        assert!(passed(&m, "pubsub"));
        // pflag: `--a2a false` is `--a2a` (true) and a stray `false`.
        let m = parse(&["--a2a", "false"]).unwrap();
        assert!(bool_value(&m, "a2a"));
        assert_eq!(stray(&m), ["false"]);
        for (v, want) in [
            ("1", true),
            ("t", true),
            ("TRUE", true),
            ("0", false),
            ("F", false),
        ] {
            let m = parse(&[&format!("--pubsub={v}")]).unwrap();
            assert_eq!(bool_value(&m, "pubsub"), want, "{v}");
        }
        // The last occurrence wins.
        let m = parse(&["--a2a", "--a2a=false"]).unwrap();
        assert!(!bool_value(&m, "a2a"));
    }

    #[test]
    fn value_flags_accept_every_pflag_spelling() {
        for args in [
            &["-r", "x"][..],
            &["-rx"],
            &["-r=x"],
            &["--region", "x"],
            &["--region=x"],
            &["-r", "y", "--region", "x"],
        ] {
            let m = parse(args).unwrap();
            assert_eq!(string_value(&m, "region"), "x", "{args:?}");
            assert!(passed(&m, "region"), "{args:?}");
        }
        // The next argument is the value even when it starts with `-`.
        for args in [&["--region", "-y"][..], &["--region=-y"], &["-r", "-y"]] {
            assert_eq!(
                string_value(&parse(args).unwrap(), "region"),
                "-y",
                "{args:?}"
            );
        }
        let m = parse(&[
            "--pubsub_oidc_audience",
            "--eventarc_oidc_audience",
            "https://svc.run.app",
        ])
        .unwrap();
        assert_eq!(
            string_value(&m, "pubsub_oidc_audience"),
            "--eventarc_oidc_audience"
        );
        assert!(!passed(&m, "eventarc_oidc_audience"));
        assert_eq!(stray(&m), ["https://svc.run.app"]);
        // An explicitly empty value is passed.
        let m = parse(&["--region="]).unwrap();
        assert_eq!(string_value(&m, "region"), "");
        assert!(passed(&m, "region"));
    }

    #[test]
    fn ints_are_go_parse_int_base_0() {
        for (v, want) in [
            ("9090", 9090),
            ("0x10", 16),
            ("010", 8),
            ("0o10", 8),
            ("0b1", 1),
            ("1_000", 1000),
            ("-1", -1),
            ("+7", 7),
        ] {
            let m = parse(&["--server_port", v]).unwrap();
            assert_eq!(int_value(&m, "server_port"), want, "{v}");
        }
    }

    #[test]
    fn durations_are_go_parse_duration() {
        for (v, want) in [
            ("1s", 1_000_000_000),
            ("1h30m", 5_400_000_000_000),
            ("1.5s", 1_500_000_000),
            ("300ms", 300_000_000),
            ("0", 0),
            ("-2m", -120_000_000_000),
        ] {
            let m = parse(&[&format!("--mem_ttl={v}")]).unwrap();
            assert_eq!(duration_value(&m, "mem_ttl"), GoDuration(want), "{v}");
        }
    }

    #[test]
    fn stray_positionals_are_accepted_and_ignored() {
        let m = parse(&["a", "-r", "x", "b", "--", "--c", "-"]).unwrap();
        assert_eq!(string_value(&m, "region"), "x");
        assert_eq!(stray(&m), ["a", "b", "--c", "-"]);
    }

    /// pflag's error texts (`errors.go`; the cause is the value's Go parse error).
    #[test]
    fn parse_errors_are_pflag_texts() {
        let cases: &[(&[&str], &str)] = &[
            (&["--nope"], "unknown flag: --nope"),
            (&["--nope=3"], "unknown flag: --nope"),
            (&["-r", "x", "-x"], "unknown shorthand flag: 'x' in -x"),
            (&["-xyz"], "unknown shorthand flag: 'x' in -xyz"),
            (&["---x"], "bad flag syntax: ---x"),
            (&["--region"], "flag needs an argument: --region"),
            (&["-r"], "flag needs an argument: 'r' in -r"),
            (&["--a2a", "-a"], "flag needs an argument: 'a' in -a"),
            (
                &["--server_port=x"],
                r#"invalid argument "x" for "--server_port" flag: parsing "x" as an integer: invalid syntax"#,
            ),
            (
                &["--server_port", "99999999999999999999"],
                r#"invalid argument "99999999999999999999" for "--server_port" flag: parsing "99999999999999999999" as an integer: value out of range"#,
            ),
            (
                &["--a2a=x"],
                r#"invalid argument "x" for "--a2a" flag: parsing "x" as a boolean: invalid syntax"#,
            ),
            (
                &["--a2a="],
                r#"invalid argument "" for "--a2a" flag: parsing "" as a boolean: invalid syntax"#,
            ),
            (
                &["--mem_ttl", "1d"],
                r#"invalid argument "1d" for "--mem_ttl" flag: time: unknown unit "d" in duration "1d""#,
            ),
        ];
        for (args, want) in cases {
            assert_eq!(parse(args).unwrap_err(), *want, "{args:?}");
        }
    }

    #[test]
    fn invalid_value_of_a_flag_with_a_shorthand_names_both() {
        let c = pflag_command("t").arg(
            Arg::new("port")
                .long("port")
                .short('p')
                .num_args(1)
                .value_parser(go_parse_int),
        );
        let raw = vec!["t".to_string(), "-p".to_string(), "x".to_string()];
        let e = c.clone().try_get_matches_from(&raw).unwrap_err();
        assert_eq!(
            pflag_error(&e, &c, &raw[1..]),
            r#"invalid argument "x" for "-p, --port" flag: parsing "x" as an integer: invalid syntax"#
        );
    }

    #[test]
    fn quote_rune_is_go_percent_q_of_a_rune() {
        assert_eq!(quote_rune('x'), "'x'");
        assert_eq!(quote_rune('é'), "'é'");
        assert_eq!(quote_rune('\''), "'\\''");
        assert_eq!(quote_rune('"'), "'\"'");
    }
}
