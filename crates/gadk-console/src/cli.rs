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

//! The `gadk-console` command line.

use clap::{Arg, Command, value_parser};

use crate::ConsoleConfig;

fn cli() -> Command {
    Command::new("gadk-console")
        .about("A local web console for gadk traces, prompts and eval reports")
        .arg(
            Arg::new("project")
                .long("project")
                .required(true)
                .help("GCP project whose Cloud Trace traces and Cloud Logging logs are read"),
        )
        .arg(
            Arg::new("store")
                .long("store")
                .help("Prompt store: file://<dir> or firestore://<project>/<database>[/<root>]"),
        )
        .arg(
            Arg::new("reports")
                .long("reports")
                .help("Monitor report store: file://<dir>, <dir> or gs://<bucket>[/<prefix>]"),
        )
        .arg(
            Arg::new("port")
                .long("port")
                .value_parser(value_parser!(u16))
                .default_value("8765")
                .help("Port on 127.0.0.1 (0 picks a free one)"),
        )
        .arg(
            Arg::new("log-id")
                .long("log-id")
                .default_value("gadk")
                .help("Cloud Logging log id the agents write"),
        )
}

/// Parses the command line (`args` includes the program name) into a [`ConsoleConfig`].
///
/// A usage error (or `--help`) is the clap error; `main` exits with it via
/// [`clap::Error::exit`] (status 2 for usage errors, 0 for help).
pub fn parse_args<I, T>(args: I) -> Result<ConsoleConfig, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let m = cli().try_get_matches_from(args)?;
    let string = |id: &str| m.get_one::<String>(id).cloned();
    Ok(ConsoleConfig {
        project: string("project").unwrap_or_default(),
        store: string("store"),
        reports: string("reports"),
        port: m.get_one::<u16>("port").copied().unwrap_or(8765),
        log_id: string("log-id").unwrap_or_else(|| "gadk".into()),
    })
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::parse_args;
    use crate::ConsoleConfig;

    #[test]
    fn defaults() {
        let cfg = parse_args(["gadk-console", "--project", "p"]).unwrap();
        assert_eq!(
            cfg,
            ConsoleConfig {
                project: "p".into(),
                store: None,
                reports: None,
                port: 8765,
                log_id: "gadk".into(),
            }
        );
    }

    #[test]
    fn all_flags() {
        let cfg = parse_args([
            "gadk-console",
            "--project",
            "proj",
            "--store",
            "file:///tmp/prompts",
            "--reports",
            "gs://evals/monitors",
            "--port",
            "0",
            "--log-id",
            "agents",
        ])
        .unwrap();
        assert_eq!(
            cfg,
            ConsoleConfig {
                project: "proj".into(),
                store: Some("file:///tmp/prompts".into()),
                reports: Some("gs://evals/monitors".into()),
                port: 0,
                log_id: "agents".into(),
            }
        );
    }

    #[test]
    fn missing_project_is_a_usage_error() {
        let err = parse_args(["gadk-console", "--port", "9000"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert_eq!(err.exit_code(), 2);
        assert!(err.render().to_string().contains("--project"), "{err}");
    }

    #[test]
    fn bad_port_is_a_usage_error() {
        for port in ["65536", "http", ""] {
            let err = parse_args(["gadk-console", "--project", "p", "--port", port]).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::ValueValidation, "{port}");
            assert_eq!(err.exit_code(), 2, "{port}");
        }
    }
}
