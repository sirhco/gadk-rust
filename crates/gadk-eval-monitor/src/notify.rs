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

//! The webhook summary `gadk-eval guards --notify` posts after a run in which a guard
//! advanced, aborted or errored.
//!
//! The webhook URL is a secret: nothing here formats it into a message, only its
//! [`origin`].

use std::fmt::Write as _;
use std::future::Future;
use std::time::Duration;

use serde_json::{Value, json};

use crate::cli::Backends;
use crate::error::MonitorError;
use crate::guard_report::GuardReport;

/// How long [`post`] waits before its one retry.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// The error of [`resolve`] for every invalid value other than an unset variable.
const BAD_TARGET: &str = "--notify: expected an http:// or https:// URL or env:NAME";

/// The webhook body: Slack-style text or structured JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotifyFormat {
    /// `{"text": …}`: the summary line, then one line per guard that advanced, aborted or
    /// errored.
    Slack,
    /// `{"summary": {…}, "guards": […]}`.
    Json,
}

/// Where and how a `guards` run posts its summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Notifier {
    /// The resolved webhook URL (a secret: print only its [`origin`]).
    pub(crate) url: String,
    /// The body format.
    pub(crate) format: NotifyFormat,
}

/// The webhook URL `--notify <target>` names: `target` itself, or the value of environment
/// variable `NAME` for `env:NAME` (read through `env`). The URL must parse as an `http::Uri`
/// with an `http` or `https` scheme (any case) and a non-empty host; it is not trimmed.
///
/// # Errors
///
/// [`MonitorError::Spec`] `--notify: environment variable NAME is not set` when `NAME` is unset
/// or empty, else `--notify: expected an http:// or https:// URL or env:NAME`. Neither names the
/// URL.
pub(crate) fn resolve(
    target: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<String, MonitorError> {
    let url = match target.strip_prefix("env:") {
        Some("") => return Err(MonitorError::Spec(BAD_TARGET.into())),
        Some(name) => match env(name) {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(MonitorError::Spec(format!(
                    "--notify: environment variable {name} is not set"
                )));
            }
        },
        None => target.to_owned(),
    };
    let valid = url.parse::<http::Uri>().is_ok_and(|uri| {
        uri.scheme_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https"))
            && uri.host().is_some_and(|h| !h.is_empty())
    });
    if valid {
        Ok(url)
    } else {
        Err(MonitorError::Spec(BAD_TARGET.into()))
    }
}

/// `scheme://host[:port]` of `url`, without userinfo, path, query or fragment: the only part
/// of a webhook URL a message may show. A URL that does not parse gives `<webhook>`.
pub(crate) fn origin(url: &str) -> String {
    let Ok(uri) = url.parse::<http::Uri>() else {
        return "<webhook>".to_owned();
    };
    let scheme = uri.scheme_str().unwrap_or_default();
    let host = uri.host().unwrap_or_default();
    match uri.port_u16() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    }
}

/// A guard that advanced, aborted or errored in a `guards` run (waits and "no rollout" are not
/// recorded).
#[derive(Debug, Clone)]
pub(crate) enum Outcome {
    /// The guard advanced or aborted.
    Report {
        /// Its report.
        report: GuardReport,
        /// The stored report's name, with `--out`.
        stored: Option<String>,
    },
    /// The guard failed.
    Error {
        /// The spec name.
        name: String,
        /// The text printed after `error: `.
        message: String,
    },
}

/// The outcome counts of a `guards` run, as its summary line prints them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Guards run.
    pub(crate) run: usize,
    /// Guards that advanced.
    pub(crate) advance: usize,
    /// Guards that aborted.
    pub(crate) abort: usize,
    /// Guards that waited.
    pub(crate) wait: usize,
    /// Guards whose label had no rollout.
    pub(crate) no_rollout: usize,
    /// Guards that failed.
    pub(crate) errors: usize,
}

/// The webhook body for a run that printed `summary_line` (without its newline) and counted
/// `counts`, with `outcomes` in run order.
pub(crate) fn body(
    format: NotifyFormat,
    summary_line: &str,
    counts: &Counts,
    outcomes: &[Outcome],
) -> Value {
    match format {
        NotifyFormat::Slack => {
            let mut text = summary_line.to_owned();
            for outcome in outcomes {
                match outcome {
                    Outcome::Report { report: r, .. } => {
                        let _ = write!(
                            text,
                            "\n• {}: {}/{} v{}→v{} at {}% — {}: {}",
                            r.spec,
                            r.prompt,
                            r.label,
                            r.baseline,
                            r.rollout,
                            r.percent,
                            r.verdict.as_str(),
                            r.action_text()
                        );
                    }
                    Outcome::Error { name, message } => {
                        let _ = write!(text, "\n• {name}: error: {message}");
                    }
                }
            }
            json!({ "text": text })
        }
        NotifyFormat::Json => {
            let guards: Vec<Value> = outcomes
                .iter()
                .map(|outcome| match outcome {
                    Outcome::Report { report: r, stored } => json!({
                        "name": r.spec,
                        "prompt": r.prompt,
                        "label": r.label,
                        "baseline": r.baseline,
                        "rollout": r.rollout,
                        "percent": r.percent,
                        "verdict": r.verdict.as_str(),
                        "action": r.action_text(),
                        "applied": r.applied,
                        "report": stored,
                    }),
                    Outcome::Error { name, message } => json!({
                        "name": name,
                        "error": message,
                    }),
                })
                .collect();
            json!({
                "summary": {
                    "run": counts.run,
                    "advance": counts.advance,
                    "abort": counts.abort,
                    "wait": counts.wait,
                    "no_rollout": counts.no_rollout,
                    "errors": counts.errors,
                },
                "guards": guards,
            })
        }
    }
}

/// Posts `body` to `url` through [`Backends::post_webhook`]. A 2xx reply succeeds. A transport
/// error or a 5xx reply is retried once, after `sleep(1 s)`. Any other reply, or a second
/// failure, is the error `POST <origin> failed: http <status>` or
/// `POST <origin> failed: <transport error>`, with every occurrence of `url` in the transport
/// error replaced by its [`origin`].
pub(crate) async fn post<F, Fut>(
    backends: &dyn Backends,
    url: &str,
    body: &Value,
    sleep: F,
) -> Result<(), String>
where
    F: Fn(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    let origin = origin(url);
    let mut retried = false;
    loop {
        let (reason, retry) = match backends.post_webhook(url, body).await {
            Ok(status) if (200..300).contains(&status) => return Ok(()),
            Ok(status) => (format!("http {status}"), (500..600).contains(&status)),
            Err(e) => (e.replace(url, &origin), true),
        };
        if retry && !retried {
            retried = true;
            sleep(RETRY_DELAY).await;
            continue;
        }
        return Err(format!("POST {origin} failed: {reason}"));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, TimeDelta};
    use gadk_model::Llm;

    use crate::guard_report::{Action, Verdict};
    use crate::report::Window;
    use crate::source::TranscriptSource;
    use crate::spec::{JudgeSpec, SourceSpec};

    use super::*;

    /// `NAME` → value for [`resolve`]: `HOOK` is a URL, `EMPTY` is empty, `BAD` is not a
    /// webhook URL, anything else is unset.
    fn env(name: &str) -> Option<String> {
        match name {
            "HOOK" => Some("https://hooks.example/T0/secret".into()),
            "EMPTY" => Some(String::new()),
            "BAD" => Some("ftp://hooks.example/T0/secret".into()),
            _ => None,
        }
    }

    /// [`resolve`]'s error as the CLI prints it after `gadk-eval: `.
    fn resolve_err(target: &str) -> String {
        resolve(target, &env).unwrap_err().to_string()
    }

    #[test]
    fn resolve_takes_an_http_or_https_url_as_given() {
        for url in [
            "https://hooks.example/T0/secret",
            "http://127.0.0.1:8080/hook?token=t",
            "HTTPS://Hooks.Example/x",
            "https://user:tok@hooks.example/x",
        ] {
            assert_eq!(resolve(url, &env).unwrap(), url);
        }
    }

    #[test]
    fn resolve_reads_env_name() {
        assert_eq!(
            resolve("env:HOOK", &env).unwrap(),
            "https://hooks.example/T0/secret"
        );
    }

    #[test]
    fn an_unset_or_empty_variable_is_named() {
        assert_eq!(
            resolve_err("env:MISSING"),
            "monitor spec: --notify: environment variable MISSING is not set"
        );
        assert_eq!(
            resolve_err("env:EMPTY"),
            "monitor spec: --notify: environment variable EMPTY is not set"
        );
    }

    #[test]
    fn anything_else_is_not_a_webhook_url() {
        for target in [
            "",
            "env:",
            "env:BAD",
            "ftp://hooks.example/x",
            "hooks.example/x",
            "https://",
            "https:///x",
            " https://hooks.example/x",
            "https://hooks.example/x ",
            "mailto:ops@example.com",
        ] {
            let err = resolve_err(target);
            assert_eq!(
                err, "monitor spec: --notify: expected an http:// or https:// URL or env:NAME",
                "{target:?}"
            );
            assert!(!err.contains("secret"), "{target:?}: {err}");
        }
    }

    #[test]
    fn origin_keeps_scheme_host_and_port_only() {
        assert_eq!(
            origin("https://hooks.slack.com/services/T0/B0/secret"),
            "https://hooks.slack.com"
        );
        assert_eq!(
            origin("http://127.0.0.1:8080/x?token=t#frag"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(origin("https://user:tok@h/x"), "https://h");
        assert_eq!(origin("not a url"), "<webhook>");
    }

    fn report(
        spec: &str,
        percent: u8,
        verdict: Verdict,
        action: Action,
        applied: bool,
    ) -> GuardReport {
        let to = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        GuardReport {
            spec: spec.into(),
            generated_at: to,
            window: Window {
                from: to - TimeDelta::hours(1),
                to,
            },
            source: "bigquery:p.d.t".into(),
            prompt: "support".into(),
            label: "production".into(),
            baseline: 1,
            rollout: 2,
            percent,
            checks: Vec::new(),
            verdict,
            action,
            applied,
            note: None,
        }
    }

    const SUMMARY: &str = "guards: 5 run — 2 advance, 1 abort, 1 wait, 0 no rollout, 1 errors";

    /// A dry-run ramp, an applied promote (stored), an applied abort and an error.
    fn mix() -> (Counts, Vec<Outcome>) {
        let counts = Counts {
            run: 5,
            advance: 2,
            abort: 1,
            wait: 1,
            no_rollout: 0,
            errors: 1,
        };
        let outcomes = vec![
            Outcome::Report {
                report: report("ramp", 10, Verdict::Advance, Action::Ramp(25), false),
                stored: None,
            },
            Outcome::Report {
                report: report("promote", 50, Verdict::Advance, Action::Promote, true),
                stored: Some("promote-20270115T080000Z.json".into()),
            },
            Outcome::Report {
                report: report("abort", 10, Verdict::Abort, Action::Abort, true),
                stored: None,
            },
            Outcome::Error {
                name: "broken".into(),
                message: "report store: prompt store: support/production: no such prompt".into(),
            },
        ];
        (counts, outcomes)
    }

    #[test]
    fn the_slack_body_is_the_summary_then_a_line_per_guard() {
        let (counts, outcomes) = mix();
        assert_eq!(
            body(NotifyFormat::Slack, SUMMARY, &counts, &outcomes),
            json!({
                "text": "guards: 5 run — 2 advance, 1 abort, 1 wait, 0 no rollout, 1 errors\n\
                         • ramp: support/production v1→v2 at 10% — advance: would ramp to 25%\n\
                         • promote: support/production v1→v2 at 50% — advance: promoted v2\n\
                         • abort: support/production v1→v2 at 10% — abort: aborted\n\
                         • broken: error: report store: prompt store: support/production: \
                         no such prompt"
            })
        );
        assert_eq!(
            body(NotifyFormat::Slack, SUMMARY, &counts, &[]),
            json!({ "text": SUMMARY })
        );
    }

    #[test]
    fn the_json_body_has_the_counts_and_each_guard() {
        let (counts, outcomes) = mix();
        assert_eq!(
            body(NotifyFormat::Json, SUMMARY, &counts, &outcomes),
            json!({
                "summary": {
                    "run": 5, "advance": 2, "abort": 1, "wait": 1, "no_rollout": 0, "errors": 1
                },
                "guards": [
                    {
                        "name": "ramp", "prompt": "support", "label": "production",
                        "baseline": 1, "rollout": 2, "percent": 10, "verdict": "advance",
                        "action": "would ramp to 25%", "applied": false, "report": null
                    },
                    {
                        "name": "promote", "prompt": "support", "label": "production",
                        "baseline": 1, "rollout": 2, "percent": 50, "verdict": "advance",
                        "action": "promoted v2", "applied": true,
                        "report": "promote-20270115T080000Z.json"
                    },
                    {
                        "name": "abort", "prompt": "support", "label": "production",
                        "baseline": 1, "rollout": 2, "percent": 10, "verdict": "abort",
                        "action": "aborted", "applied": true, "report": null
                    },
                    {
                        "name": "broken",
                        "error": "report store: prompt store: support/production: no such prompt"
                    }
                ]
            })
        );
    }

    /// Backends whose `post_webhook` returns `replies` in order and records each call.
    struct Scripted {
        replies: Mutex<VecDeque<Result<u16, String>>>,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl Scripted {
        fn new(replies: Vec<Result<u16, String>>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Backends for Scripted {
        fn source(&self, _spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
            Err(MonitorError::Source("no source".into()))
        }

        async fn judge(&self, _spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
            Err(MonitorError::Judge("no judge".into()))
        }

        async fn post_webhook(&self, url: &str, body: &Value) -> Result<u16, String> {
            self.calls
                .lock()
                .unwrap()
                .push((url.to_owned(), body.clone()));
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("an unscripted post")
        }
    }

    const URL: &str = "https://hooks.example/T0/secret";

    /// Runs [`post`] to `url` against `replies`: its result, the URLs posted to and the
    /// sleeps taken.
    async fn post_to(
        url: &str,
        replies: Vec<Result<u16, String>>,
    ) -> (Result<(), String>, Vec<String>, Vec<Duration>) {
        let backends = Scripted::new(replies);
        let slept = Mutex::new(Vec::new());
        let body = json!({ "text": "hi" });
        let result = post(&backends, url, &body, |d| {
            slept.lock().unwrap().push(d);
            async {}
        })
        .await;
        let calls = backends.calls.lock().unwrap().clone();
        assert!(calls.iter().all(|(_, b)| *b == body), "{calls:?}");
        let urls = calls.into_iter().map(|(u, _)| u).collect();
        (result, urls, slept.into_inner().unwrap())
    }

    #[tokio::test]
    async fn a_2xx_reply_posts_once() {
        for status in [200, 204, 299] {
            let (result, urls, slept) = post_to(URL, vec![Ok(status)]).await;
            assert_eq!(result, Ok(()), "{status}");
            assert_eq!(urls, [URL]);
            assert!(slept.is_empty());
        }
    }

    #[tokio::test]
    async fn a_5xx_then_2xx_is_retried_once_after_a_second() {
        let (result, urls, slept) = post_to(URL, vec![Ok(503), Ok(200)]).await;
        assert_eq!(result, Ok(()));
        assert_eq!(urls, [URL, URL]);
        assert_eq!(slept, [Duration::from_secs(1)]);
    }

    #[tokio::test]
    async fn two_5xx_replies_fail_with_the_second_status() {
        let (result, urls, slept) = post_to(URL, vec![Ok(500), Ok(502)]).await;
        assert_eq!(
            result,
            Err("POST https://hooks.example failed: http 502".into())
        );
        assert_eq!(urls.len(), 2);
        assert_eq!(slept, [Duration::from_secs(1)]);
    }

    #[tokio::test]
    async fn a_transport_error_then_2xx_is_retried_once() {
        let (result, urls, slept) =
            post_to(URL, vec![Err("connection failed".into()), Ok(200)]).await;
        assert_eq!(result, Ok(()));
        assert_eq!(urls.len(), 2);
        assert_eq!(slept, [Duration::from_secs(1)]);
    }

    #[tokio::test]
    async fn a_4xx_3xx_or_1xx_reply_fails_without_a_retry() {
        for status in [404, 400, 302, 101] {
            let (result, urls, slept) = post_to(URL, vec![Ok(status)]).await;
            assert_eq!(
                result,
                Err(format!("POST https://hooks.example failed: http {status}"))
            );
            assert_eq!(urls.len(), 1, "{status}");
            assert!(slept.is_empty(), "{status}");
        }
    }

    #[tokio::test]
    async fn a_transport_error_naming_the_url_is_redacted_to_its_origin() {
        let (result, urls, _) = post_to(
            URL,
            vec![
                Err(format!("error sending request for url ({URL})")),
                Err(format!("timed out: {URL} and {URL}")),
            ],
        )
        .await;
        assert_eq!(
            result,
            Err(
                "POST https://hooks.example failed: timed out: https://hooks.example and \
                 https://hooks.example"
                    .into()
            )
        );
        assert_eq!(urls.len(), 2);
    }

    #[tokio::test]
    async fn userinfo_never_reaches_the_reason() {
        let url = "https://user:tok@h/x";
        let (result, _, _) = post_to(url, vec![Ok(401)]).await;
        assert_eq!(result, Err("POST https://h failed: http 401".into()));
        let (result, _, _) = post_to(url, vec![Err(url.into()), Err(url.into())]).await;
        let reason = result.unwrap_err();
        assert_eq!(reason, "POST https://h failed: https://h");
        assert!(!reason.contains("tok"), "{reason}");
    }
}
