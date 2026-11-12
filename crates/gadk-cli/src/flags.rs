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

//! pflag-compatible clap helpers: the flag kinds `cmd/adkgo` registers (pflag `StringVarP`,
//! `IntVar`, `DurationVar`, `BoolVar`) as clap builder [`Arg`]s that parse like pflag, the
//! value readers, Go's `Flags().Changed` ([`passed`]) and the mapping of clap's parse errors
//! to pflag's texts ([`pflag_error`]).
//!
//! pflag rules reproduced: `--f=v` and `--f v`, `-f v`, `-fv` and `-f=v`; a value-taking flag
//! takes the next argument even when it starts with `-`; a bool flag takes a value only as
//! `--b=v` (pflag `NoOptDefVal`), so `--b v` sets it true and leaves `v` as an argument; a
//! repeated flag keeps its last value; values parse with Go `strconv.ParseBool`,
//! `strconv.ParseInt(s, 0, 64)` and `time.ParseDuration`; stray positional arguments are
//! accepted and ignored (cobra without an `Args` validator).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use gadk_platform::{GoDuration, go_parse_duration, go_quote};

/// The id of the hidden trailing positional that swallows stray arguments.
pub const STRAY_ARGS: &str = "args";

/// A subcommand whose flags behave like a pflag `FlagSet`: a repeated flag overrides the
/// earlier value (pflag sets it again) instead of being a clap error, and there is no `help`
/// subcommand (cobra adds one to the root only).
pub fn pflag_command(name: &'static str) -> Command {
    todo!("Task 2")
}

/// A value-taking flag: exactly one value, which may start with `-` (pflag takes the next
/// argument whatever it is).
fn value_flag(
    long: &'static str,
    short: Option<char>,
    default: &'static str,
    help: &'static str,
    value_name: &'static str,
    zero: &'static str,
) -> Arg {
    todo!("Task 2")
}

/// pflag `StringVarP(&v, long, short, default, help)`.
pub fn string_flag(
    long: &'static str,
    short: Option<char>,
    default: &'static str,
    help: &'static str,
) -> Arg {
    todo!("Task 2")
}

/// pflag `IntVarP`: the value is Go `strconv.ParseInt(s, 0, 64)`.
pub fn int_flag(
    long: &'static str,
    short: Option<char>,
    default: &'static str,
    help: &'static str,
) -> Arg {
    todo!("Task 2")
}

/// pflag `DurationVarP`: the value is Go `time.ParseDuration`; `default` is Go's
/// `Duration.String()` of the default (e.g. `1s`, `8760h0m0s`).
pub fn duration_flag(
    long: &'static str,
    short: Option<char>,
    default: &'static str,
    help: &'static str,
) -> Arg {
    todo!("Task 2")
}

/// pflag `BoolVar`: `--b` is true (pflag `NoOptDefVal`), `--b=<v>` is Go `strconv.ParseBool`;
/// a value is never taken from the next argument.
pub fn bool_flag(long: &'static str, default: bool, help: &'static str) -> Arg {
    todo!("Task 2")
}

/// The hidden trailing positional: cobra commands without an `Args` validator accept and
/// ignore positional arguments.
pub fn stray_args() -> Arg {
    todo!("Task 2")
}

/// Go `cmd.Flags().Changed(id)`: the flag was on the command line.
pub fn passed(m: &ArgMatches, id: &str) -> bool {
    todo!("Task 2")
}

/// A [`string_flag`]'s value (its default when absent).
pub fn string_value(m: &ArgMatches, id: &str) -> String {
    todo!("Task 2")
}

/// An [`int_flag`]'s value.
pub fn int_value(m: &ArgMatches, id: &str) -> i64 {
    todo!("Task 2")
}

/// A [`duration_flag`]'s value.
pub fn duration_value(m: &ArgMatches, id: &str) -> GoDuration {
    todo!("Task 2")
}

/// A [`bool_flag`]'s value.
pub fn bool_value(m: &ArgMatches, id: &str) -> bool {
    todo!("Task 2")
}

fn context_str(e: &clap::Error, kind: ContextKind) -> Option<&str> {
    todo!("Task 2")
}

/// Go `%q` of a rune (`'x'`).
fn quote_rune(c: char) -> String {
    todo!("Task 2")
}

/// The long name in clap's rendering of an argument (`--region <string>`,
/// `--a2a[=<bool>]`).
fn long_name(rendered: &str) -> &str {
    todo!("Task 2")
}

/// pflag's error text for clap's parse error `e` on `cmd` (the subcommand being parsed);
/// `raw` are the command-line arguments, used where clap does not keep what pflag reports
/// (the token an unknown shorthand came from, whether a value-less flag was written short).
/// Errors pflag cannot produce keep clap's message.
pub fn pflag_error(e: &clap::Error, cmd: &Command, raw: &[String]) -> String {
    todo!("Task 2")
}

/// clap's own message for `e`: its first line without the `error: ` prefix.
fn clap_message(e: &clap::Error) -> String {
    todo!("Task 2")
}
