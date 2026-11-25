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

use std::path::PathBuf;

use clap::{Arg, ArgMatches, Command, value_parser};

use crate::traces::BigQueryTable;
use crate::{Cli, ConsoleConfig, DEFAULT_MAX_BYTES, TracesFrom};

fn base_cli() -> Command {
    Command::new("gadk-console")
        .about("A local web console for gadk traces, prompts and eval reports")
        .arg(
            Arg::new("project")
                .long("project")
                .required(true)
                .help("GCP project: Cloud Trace + Cloud Logging source, or the billing project for BigQuery queries (--traces bigquery://…)"),
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
    let m = base_cli().try_get_matches_from(args)?;
    Ok(config_from(&m))
}

/// The [`ConsoleConfig`] from matches of `base_cli` (or a command extending it).
fn config_from(m: &ArgMatches) -> ConsoleConfig {
    let string = |id: &str| m.get_one::<String>(id).cloned();
    ConsoleConfig {
        project: string("project").unwrap_or_default(),
        store: string("store"),
        reports: string("reports"),
        port: m.get_one::<u16>("port").copied().unwrap_or(8765),
        log_id: string("log-id").unwrap_or_else(|| "gadk".into()),
    }
}

/// `base_cli` plus the trace-source flags (`parse_cli` only).
fn full_cli() -> Command {
    base_cli()
        .arg(
            Arg::new("traces")
                .long("traces")
                .value_parser(|s: &str| BigQueryTable::parse_url(s))
                .help("Trace source: bigquery://<project>.<dataset>.<table> (default: Cloud Trace + Cloud Logging)"),
        )
        .arg(
            Arg::new("bigquery-max-bytes")
                .long("bigquery-max-bytes")
                .value_parser(value_parser!(i64).range(1..))
                .default_value("1073741824")
                .help("Most bytes one BigQuery query may bill (with --traces bigquery://…)"),
        )
        .arg(
            Arg::new("prices")
                .long("prices")
                .value_parser(value_parser!(PathBuf))
                .help("Price file (YAML): USD per 1,000,000 tokens per model; enables cost columns"),
        )
        .arg(
            Arg::new("author")
                .long("author")
                .help("Author of the scores added on /traces/{id} (default: $USER, else $USERNAME, else console)"),
        )
        .arg(
            Arg::new("datasets")
                .long("datasets")
                .value_parser(value_parser!(PathBuf))
                .help("Directory of eval-set YAML files that /traces/{id} adds runs to (with --traces bigquery://…)"),
        )
        .arg(
            Arg::new("rubrics")
                .long("rubrics")
                .value_parser(value_parser!(PathBuf))
                .help("YAML file of review rubrics (criteria scored 1-5 by default)"),
        )
}

/// Parses the full command line: [`ConsoleConfig`] plus the trace source.
///
/// # Errors
///
/// A clap usage error (exit code 2), or the help/version display.
pub fn parse_cli<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let m = full_cli().try_get_matches_from(args)?;
    let config = config_from(&m);
    let max_bytes = m
        .get_one::<i64>("bigquery-max-bytes")
        .copied()
        .unwrap_or(DEFAULT_MAX_BYTES);
    let mut cli = Cli::new(config);
    if let Some(table) = m.get_one::<BigQueryTable>("traces") {
        cli = cli.with_traces(TracesFrom::bigquery(table.clone(), max_bytes));
    }
    if let Some(path) = m.get_one::<PathBuf>("prices") {
        cli = cli.with_prices(path.clone());
    }
    if let Some(author) = m.get_one::<String>("author") {
        cli = cli.with_author(author.clone());
    }
    if let Some(dir) = m.get_one::<PathBuf>("datasets") {
        cli = cli.with_datasets(dir.clone());
    }
    if let Some(path) = m.get_one::<PathBuf>("rubrics") {
        cli = cli.with_rubrics(path.clone());
    }
    Ok(cli)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::error::ErrorKind;

    use super::{parse_args, parse_cli};
    use crate::traces::BigQueryTable;
    use crate::{Cli, ConsoleConfig, DEFAULT_MAX_BYTES, TracesFrom};

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

    #[test]
    fn parse_cli_defaults_to_cloud_trace() {
        let cli = parse_cli(["gadk-console", "--project", "p"]).unwrap();
        assert_eq!(
            cli.config,
            parse_args(["gadk-console", "--project", "p"]).unwrap()
        );
        assert_eq!(cli.traces, None);
    }

    #[test]
    fn parse_cli_reads_bigquery_traces_and_byte_cap() {
        let cli = parse_cli([
            "gadk-console",
            "--project",
            "billing",
            "--traces",
            "bigquery://example.com:proj-1.agent_analytics.agent_events",
        ])
        .unwrap();
        assert_eq!(
            cli.traces,
            Some(TracesFrom::bigquery(
                BigQueryTable::new("example.com:proj-1", "agent_analytics", "agent_events"),
                DEFAULT_MAX_BYTES
            ))
        );
        let cli = parse_cli([
            "gadk-console",
            "--project",
            "p",
            "--traces",
            "bigquery://sirhco-apps.ds.tbl",
            "--bigquery-max-bytes",
            "5000",
        ])
        .unwrap();
        assert_eq!(
            cli,
            Cli::new(parse_args(["gadk-console", "--project", "p"]).unwrap()).with_traces(
                TracesFrom::bigquery(BigQueryTable::new("sirhco-apps", "ds", "tbl"), 5000)
            )
        );
    }

    #[test]
    fn parse_cli_rejects_bad_traces_and_byte_caps() {
        for url in [
            "sirhco-apps.ds.tbl",
            "bigquery://sirhco-apps.ds",
            "bigquery://x.ds.tbl",
        ] {
            let err = parse_cli(["gadk-console", "--project", "p", "--traces", url]).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::ValueValidation, "{url}");
            assert_eq!(err.exit_code(), 2);
            assert!(err.render().to_string().contains("--traces"), "{err}");
        }
        // `=` form: clap would read a bare `-1` as a flag rather than a value.
        for bytes in ["0", "-1", "9223372036854775808", "x"] {
            let arg = format!("--bigquery-max-bytes={bytes}");
            let err = parse_cli(["gadk-console", "--project", "p", arg.as_str()]).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::ValueValidation, "{bytes}");
            assert!(
                err.render().to_string().contains("--bigquery-max-bytes"),
                "{err}"
            );
        }
    }

    #[test]
    fn parse_args_does_not_know_the_new_flags() {
        for flag in [
            ["--traces", "bigquery://sirhco-apps.ds.tbl"],
            ["--bigquery-max-bytes", "5"],
            ["--prices", "prices.yaml"],
            ["--author", "chris"],
            ["--datasets", "evals"],
            ["--rubrics", "rubrics.yaml"],
        ] {
            let err = parse_args(["gadk-console", "--project", "p", flag[0], flag[1]]).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::UnknownArgument, "{flag:?}");
        }
    }

    #[test]
    fn help_lists_the_new_flags() {
        let err = parse_cli(["gadk-console", "--help"]).unwrap_err();
        let help = err.render().to_string();
        assert!(
            help.contains("--traces")
                && help.contains("--bigquery-max-bytes")
                && help.contains("--prices")
                && help.contains("--author")
                && help.contains("--datasets")
                && help.contains("--rubrics")
                && help.contains("YAML file of review rubrics (criteria scored 1-5 by default)"),
            "{help}"
        );
    }

    #[test]
    fn parse_cli_reads_the_price_file_path() {
        let cli = parse_cli(["gadk-console", "--project", "p", "--prices", "prices.yaml"]).unwrap();
        assert_eq!(
            cli,
            Cli::new(parse_args(["gadk-console", "--project", "p"]).unwrap())
                .with_prices(PathBuf::from("prices.yaml"))
        );
        assert_eq!(
            parse_cli(["gadk-console", "--project", "p"])
                .unwrap()
                .prices,
            None
        );
    }

    #[test]
    fn parse_cli_reads_the_author() {
        let cli = parse_cli(["gadk-console", "--project", "p", "--author", "chris"]).unwrap();
        assert_eq!(
            cli,
            Cli::new(parse_args(["gadk-console", "--project", "p"]).unwrap()).with_author("chris")
        );
        assert_eq!(cli.author.as_deref(), Some("chris"));
        assert_eq!(
            parse_cli(["gadk-console", "--project", "p"])
                .unwrap()
                .author,
            None
        );
    }

    #[test]
    fn parse_cli_reads_the_datasets_directory() {
        let cli = parse_cli(["gadk-console", "--project", "p", "--datasets", "evals"]).unwrap();
        assert_eq!(
            cli,
            Cli::new(parse_args(["gadk-console", "--project", "p"]).unwrap())
                .with_datasets(PathBuf::from("evals"))
        );
        assert_eq!(
            parse_cli(["gadk-console", "--project", "p"])
                .unwrap()
                .datasets,
            None
        );
    }

    #[test]
    fn parse_cli_reads_the_rubric_file_path() {
        let cli = parse_cli([
            "gadk-console",
            "--project",
            "p",
            "--rubrics",
            "rubrics.yaml",
        ])
        .unwrap();
        assert_eq!(
            cli,
            Cli::new(parse_args(["gadk-console", "--project", "p"]).unwrap())
                .with_rubrics(PathBuf::from("rubrics.yaml"))
        );
        assert_eq!(
            parse_cli(["gadk-console", "--project", "p"])
                .unwrap()
                .rubrics,
            None
        );
    }
}
