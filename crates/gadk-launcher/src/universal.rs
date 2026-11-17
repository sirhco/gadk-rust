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

//! The universal launcher: an umbrella over sub-launchers (console and web). The first
//! command-line argument picks a sub-launcher by keyword; it parses the rest of the arguments and
//! then runs.

use std::collections::HashSet;

use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::error::Error;
use crate::launcher::{Launcher, SubLauncher};

/// A hook that checks the arguments left after parsing (for example
/// [`error_on_unparsed_args`]).
pub type ParseRemaining = fn(&[String]) -> Result<(), Error>;

/// A launcher that routes to one of its sub-launchers by keyword.
pub struct UniversalLauncher {
    /// Index of the chosen sub-launcher, set by a successful keyword routing.
    chosen: Option<usize>,
    sublaunchers: Vec<Box<dyn SubLauncher>>,
}

impl UniversalLauncher {
    /// A launcher over `sublaunchers`: the first sub-launcher is the default when
    /// there are no arguments or the first one is not a keyword.
    pub fn new(sublaunchers: Vec<Box<dyn SubLauncher>>) -> UniversalLauncher {
        UniversalLauncher {
            chosen: None,
            sublaunchers,
        }
    }

    /// Parses, then runs `parse_remaining` on the leftovers, then runs the chosen sub-launcher.
    pub async fn parse_and_run(
        &mut self,
        cancel: CancellationToken,
        config: Config,
        args: Vec<String>,
        parse_remaining: Option<ParseRemaining>,
    ) -> Result<(), Error> {
        let remaining = self.parse(args)?;
        if let Some(parse_remaining) = parse_remaining {
            parse_remaining(&remaining)?;
        }
        self.run(cancel, config).await
    }

    /// Runs the chosen sub-launcher.
    async fn run(&self, cancel: CancellationToken, config: Config) -> Result<(), Error> {
        match self.chosen.and_then(|i| self.sublaunchers.get(i)) {
            Some(chosen) => chosen.run(cancel, config).await,
            // `parse` always chooses one first.
            None => Err(Error::Parse(
                "there are no sub launchers to parse the arguments".to_owned(),
            )),
        }
    }

    /// Checks the keywords are unique, picks the sub-launcher and lets it parse.
    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        let mut keywords = HashSet::new();
        for sublauncher in &self.sublaunchers {
            let keyword = sublauncher.keyword();
            if !keywords.insert(keyword.clone()) {
                return Err(Error::Parse(format!(
                    "cannot create universal launcher. Keywords for sublaunchers should be unique and they are not: '{keyword}'"
                )));
            }
        }
        if self.sublaunchers.is_empty() {
            return Err(Error::Parse(
                "there are no sub launchers to parse the arguments".to_owned(),
            ));
        }
        // Default to the first one in the list.
        self.chosen = Some(0);
        let Some(key) = args.first() else {
            return self.sublaunchers[0].parse(args);
        };
        if let Some(i) = self.sublaunchers.iter().position(|l| l.keyword() == *key) {
            // Match found: use it and continue parsing without the keyword.
            self.chosen = Some(i);
            return self.sublaunchers[i].parse(args[1..].to_vec());
        }
        self.sublaunchers[0].parse(args)
    }

    /// The description text, with its trailing spaces and tab indents.
    fn simple_description(&self) -> String {
        "Universal launcher acts as a router, routing command line arguments to one of its sublaunchers. \n\tThe sublauncher is chosen by the first argument - a keyword. \n\tIf there are no arguments at all or the first one is not recognized by any of the sublaunchers, the first sublauncher is used.".to_owned()
    }
}

#[async_trait::async_trait]
impl Launcher for UniversalLauncher {
    /// Validates the config, then parses and runs with [`error_on_unparsed_args`].
    async fn execute(
        &mut self,
        cancel: CancellationToken,
        config: Config,
        args: Vec<String>,
    ) -> Result<(), Error> {
        config.validate()?;
        self.parse_and_run(cancel, config, args, Some(error_on_unparsed_args))
            .await
    }

    /// The syntax of every sub-launcher: a keyword list, then each sub-launcher's details.
    fn command_line_syntax(&self) -> String {
        if self.sublaunchers.is_empty() {
            return self.simple_description()
                + "\n\nThere are no sublaunchers to format syntax for.";
        }
        let mut b = String::from("Arguments: Specify one of the following:\n");
        for l in &self.sublaunchers {
            b.push_str(&format!(
                "  * {} - {}\n",
                l.keyword(),
                l.simple_description()
            ));
        }
        b.push_str("Details:\n");
        for l in &self.sublaunchers {
            b.push_str(&format!("  {}\n{}\n", l.keyword(), l.command_line_syntax()));
        }
        b
    }
}

/// `cannot parse following arguments: [a b]` when any are left.
pub fn error_on_unparsed_args(args: &[String]) -> Result<(), Error> {
    if args.is_empty() {
        return Ok(());
    }
    Err(Error::Parse(format!(
        "cannot parse following arguments: [{}]",
        args.join(" ")
    )))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::test_support::NoAgents;

    /// What a fake sub-launcher saw.
    #[derive(Default)]
    struct Seen {
        parsed: Vec<(String, Vec<String>)>,
        ran: Vec<String>,
    }

    struct Fake {
        keyword: &'static str,
        /// Arguments `parse` leaves unparsed: everything from the first one equal to this.
        stop_at: Option<&'static str>,
        parse_error: Option<&'static str>,
        seen: Arc<Mutex<Seen>>,
    }

    fn fake(keyword: &'static str, seen: &Arc<Mutex<Seen>>) -> Fake {
        Fake {
            keyword,
            stop_at: None,
            parse_error: None,
            seen: seen.clone(),
        }
    }

    #[async_trait::async_trait]
    impl SubLauncher for Fake {
        fn keyword(&self) -> String {
            self.keyword.to_owned()
        }
        fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
            self.seen
                .lock()
                .unwrap()
                .parsed
                .push((self.keyword.to_owned(), args.clone()));
            if let Some(e) = self.parse_error {
                return Err(Error::Parse(e.to_owned()));
            }
            Ok(match self.stop_at {
                Some(stop) => args
                    .iter()
                    .position(|a| a == stop)
                    .map(|i| args[i..].to_vec())
                    .unwrap_or_default(),
                None => Vec::new(),
            })
        }
        fn command_line_syntax(&self) -> String {
            format!(
                "  -{}_flag string\n    \tusage of {}\n",
                self.keyword, self.keyword
            )
        }
        fn simple_description(&self) -> String {
            format!("runs {}.", self.keyword)
        }
        async fn run(&self, _cancel: CancellationToken, _config: Config) -> Result<(), Error> {
            self.seen.lock().unwrap().ran.push(self.keyword.to_owned());
            Ok(())
        }
    }

    fn config() -> Config {
        Config::new(Arc::new(NoAgents))
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn two(seen: &Arc<Mutex<Seen>>) -> UniversalLauncher {
        UniversalLauncher::new(vec![
            Box::new(fake("console", seen)),
            Box::new(fake("web", seen)),
        ])
    }

    async fn execute(l: &mut UniversalLauncher, a: &[&str]) -> Result<(), Error> {
        l.execute(CancellationToken::new(), config(), args(a)).await
    }

    #[tokio::test]
    async fn no_arguments_run_the_first_sublauncher() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        execute(&mut two(&seen), &[]).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.parsed, vec![("console".to_owned(), vec![])]);
        assert_eq!(seen.ran, vec!["console"]);
    }

    #[tokio::test]
    async fn a_keyword_picks_its_sublauncher_and_is_not_passed_on() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        execute(&mut two(&seen), &["web", "-port", "9"])
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.parsed, vec![("web".to_owned(), args(&["-port", "9"]))]);
        assert_eq!(seen.ran, vec!["web"]);
    }

    #[tokio::test]
    async fn an_unknown_first_argument_goes_to_the_first_sublauncher_whole() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        execute(&mut two(&seen), &["-streaming_mode", "sse"])
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.parsed,
            vec![("console".to_owned(), args(&["-streaming_mode", "sse"]))]
        );
        assert_eq!(seen.ran, vec!["console"]);
    }

    #[tokio::test]
    async fn a_keyword_later_in_the_arguments_does_not_route() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        execute(&mut two(&seen), &["console", "web"]).await.unwrap();
        assert_eq!(
            seen.lock().unwrap().parsed,
            vec![("console".to_owned(), args(&["web"]))]
        );
    }

    #[tokio::test]
    async fn unparsed_arguments_are_an_error_and_nothing_runs() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut l = UniversalLauncher::new(vec![Box::new(Fake {
            stop_at: Some("extra"),
            ..fake("console", &seen)
        })]);
        let err = execute(&mut l, &["console", "extra", "more"])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot parse following arguments: [extra more]"
        );
        assert!(seen.lock().unwrap().ran.is_empty());
    }

    #[tokio::test]
    async fn parse_and_run_without_a_hook_runs_with_leftovers() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut l = UniversalLauncher::new(vec![Box::new(Fake {
            stop_at: Some("extra"),
            ..fake("console", &seen)
        })]);
        l.parse_and_run(CancellationToken::new(), config(), args(&["extra"]), None)
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().ran, vec!["console"]);
    }

    #[tokio::test]
    async fn a_sublauncher_parse_error_is_returned_as_is() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut l = UniversalLauncher::new(vec![Box::new(Fake {
            parse_error: Some("failed to parse flags: flag: help requested"),
            ..fake("console", &seen)
        })]);
        let err = execute(&mut l, &["-h"]).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to parse flags: flag: help requested"
        );
        assert!(seen.lock().unwrap().ran.is_empty());
    }

    #[tokio::test]
    async fn duplicate_keywords_are_an_error() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut l = UniversalLauncher::new(vec![
            Box::new(fake("console", &seen)),
            Box::new(fake("web", &seen)),
            Box::new(fake("web", &seen)),
        ]);
        let err = execute(&mut l, &[]).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "cannot create universal launcher. Keywords for sublaunchers should be unique and they are not: 'web'"
        );
        assert!(seen.lock().unwrap().parsed.is_empty());
    }

    #[tokio::test]
    async fn no_sublaunchers_is_an_error() {
        let err = execute(&mut UniversalLauncher::new(Vec::new()), &["web"])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "there are no sub launchers to parse the arguments"
        );
    }

    #[tokio::test]
    async fn execute_validates_the_config_before_parsing() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut cfg = config();
        cfg.compaction = Some(gadk_session::compaction::Config {
            overlap_size: 2,
            ..Default::default()
        });
        let err = two(&seen)
            .execute(CancellationToken::new(), cfg, args(&["web"]))
            .await
            .unwrap_err();
        assert!(err.to_string().starts_with("invalid Compaction: "), "{err}");
        assert!(seen.lock().unwrap().parsed.is_empty());
    }

    #[test]
    fn error_on_unparsed_args_formats_like_go_percent_v() {
        assert!(error_on_unparsed_args(&[]).is_ok());
        assert_eq!(
            error_on_unparsed_args(&args(&["a"]))
                .unwrap_err()
                .to_string(),
            "cannot parse following arguments: [a]"
        );
        assert_eq!(
            error_on_unparsed_args(&args(&["", "b c"]))
                .unwrap_err()
                .to_string(),
            "cannot parse following arguments: [ b c]"
        );
    }

    #[test]
    fn command_line_syntax_is_go_layout() {
        let seen = Arc::new(Mutex::new(Seen::default()));
        assert_eq!(
            two(&seen).command_line_syntax(),
            "Arguments: Specify one of the following:\n\
             \x20 * console - runs console.\n\
             \x20 * web - runs web.\n\
             Details:\n\
             \x20 console\n\
             \x20 -console_flag string\n\
             \x20   \tusage of console\n\
             \n\
             \x20 web\n\
             \x20 -web_flag string\n\
             \x20   \tusage of web\n\
             \n"
        );
    }

    #[test]
    fn command_line_syntax_without_sublaunchers_is_the_simple_description() {
        assert_eq!(
            UniversalLauncher::new(Vec::new()).command_line_syntax(),
            "Universal launcher acts as a router, routing command line arguments to one of its \
             sublaunchers. \n\tThe sublauncher is chosen by the first argument - a keyword. \n\tIf \
             there are no arguments at all or the first one is not recognized by any of the \
             sublaunchers, the first sublauncher is used.\n\nThere are no sublaunchers to format \
             syntax for."
        );
    }

    /// The `full` launcher's syntax as ADK-Go's probe recorded it after the error record (CLI
    /// fixture `unparsed_args_console`).
    #[cfg(feature = "full")]
    #[test]
    fn command_line_syntax_matches_the_cli_fixture() {
        use crate::launcher::Launcher as _;
        let f = crate::test_support::cli("unparsed_args_console");
        let stderr = f["stderr"].as_str().unwrap();
        let record = format!("<ts> Run failed: {}\n\n", f["error"].as_str().unwrap());
        let at = stderr.find(&record).unwrap() + record.len();
        assert_eq!(
            crate::full::new().command_line_syntax(),
            &stderr[at..],
            "{stderr}"
        );
    }
}
