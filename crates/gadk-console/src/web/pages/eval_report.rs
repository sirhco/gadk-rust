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

//! The page for one monitor report: its header and one expandable row per graded run.

use chrono::{DateTime, SecondsFormat, Utc};
use gadk_eval::{Outcome, ScoreResult, ToolCall};
use gadk_eval_monitor::{MonitorReport, MonitoredRun};
use maud::{Markup, html};

use super::layout::{Nav, page};
use crate::evals::{percent, spec_href};
use crate::traces::TraceId;

/// The report stored as `name`: its header, then one `<details>` per run with the run's score
/// chips in its summary and its scores, notes, error and transcript inside.
pub(crate) fn render(name: &str, r: &MonitorReport) -> Markup {
    let body = html! {
        h1 { "Report " span class="mono" { (name) } }
        dl class="eval-meta" {
            dt { "Monitor" } dd { a href=(spec_href(&r.spec)) { (r.spec) } }
            dt { "Window" } dd class="mono" { (secs(&r.window.from)) " – " (secs(&r.window.to)) }
            dt { "Source" } dd class="mono" { (r.source) }
            dt { "Generated" } dd class="mono" { (secs(&r.generated_at)) }
            dt { "Runs" } dd { (r.runs.len()) }
            dt { "Pass rate" } dd { (percent(r.pass_rate())) }
            dt { "Errors" } dd { (r.error_count()) }
        }
        @if r.runs.is_empty() {
            p class="empty" { "No runs in this window" }
        } @else {
            div class="eval-runs" {
                @for m in &r.runs { (run(m)) }
            }
        }
    };
    page(&format!("Report {name}"), Nav::Evals, body)
}

fn secs(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn run(m: &MonitoredRun) -> Markup {
    let run = &m.run;
    // Report JSON is untrusted: link only a well-formed trace id.
    let trace = run.trace_id.as_deref().and_then(TraceId::parse);
    html! {
        details class="eval-run" {
            summary {
                span class="mono" { (secs(&run.started_at)) }
                span { (run.agent) }
                span class="mono" { (run.session_id) }
                span class="chips" { @for s in &m.scores { (chip(s)) } }
            }
            div class="eval-run-body" {
                p class="ids" {
                    "Invocation " span class="mono" { (run.invocation_id) }
                    @if let Some(t) = &trace {
                        " · " a href={ "/traces/" (t.as_str()) } { "trace" }
                    }
                }
                @if !run.content_complete || !run.tools_complete {
                    div class="banner warn" {
                        p { "Incomplete transcript" }
                        @if !run.notes.is_empty() {
                            ul { @for n in &run.notes { li { (n) } } }
                        }
                    }
                }
                @if let Some(err) = &run.transcript.error {
                    div class="banner error" { p { "Run error: " (err) } }
                }
                h3 { "Scores" }
                table {
                    thead { tr { th { "Scorer" } th { "Outcome" } th { "Score" } th { "Rationale" } } }
                    tbody {
                        @for s in &m.scores {
                            tr {
                                td class="mono" { (s.kind) }
                                td { (chip(s)) }
                                td class="num" { @if let Some(v) = s.score { (v) } }
                                td { (s.rationale) }
                            }
                        }
                    }
                }
                h3 { "Transcript" }
                @for (i, t) in run.transcript.turns.iter().enumerate() {
                    div class="turn" {
                        p class="turn-head" { "Turn " (i + 1) }
                        p class="who" { "User" }
                        pre { (t.user) }
                        @for c in &t.calls { (call(c)) }
                        p class="who" { "Agent" }
                        pre { (t.reply) }
                    }
                }
            }
        }
    }
}

fn chip(s: &ScoreResult) -> Markup {
    let (class, label) = match s.outcome {
        Outcome::Pass => ("chip chip-pass", "pass"),
        Outcome::Fail => ("chip chip-fail", "fail"),
        Outcome::Error => ("chip chip-error", "error"),
    };
    html! { span class=(class) title=(s.kind) { (s.kind) ": " (label) } }
}

fn call(c: &ToolCall) -> Markup {
    let args = serde_json::to_string_pretty(&c.args).unwrap_or_default();
    html! {
        p class="call" { "Tool call " span class="mono" { (c.name) } }
        pre class="mono" { (args) }
    }
}

#[cfg(test)]
mod tests {
    use gadk_eval::{Outcome, ToolCall};
    use serde_json::json;

    use super::render;
    use crate::web::testutil::report;

    const NAME: &str = "support-20270115T080000Z.json";

    #[test]
    fn header_shows_the_report_numbers() {
        let r = report(
            "support",
            1_800_000_000,
            &[&[Outcome::Pass], &[Outcome::Error]],
        );
        let out = render(NAME, &r).into_string();
        assert!(
            out.contains("<title>Report support-20270115T080000Z.json · gadk console</title>"),
            "{out}"
        );
        assert!(
            out.contains(r#"<a class="active" href="/evals">Evals</a>"#),
            "{out}"
        );
        for want in [
            r#"<dt>Monitor</dt><dd><a href="/evals?spec=support">support</a></dd>"#,
            r#"<dt>Window</dt><dd class="mono">2027-01-15T07:00:00Z – 2027-01-15T08:00:00Z</dd>"#,
            r#"<dt>Source</dt><dd class="mono">bigquery:p.d.t</dd>"#,
            r#"<dt>Generated</dt><dd class="mono">2027-01-15T08:00:00Z</dd>"#,
            "<dt>Runs</dt><dd>2</dd>",
            "<dt>Pass rate</dt><dd>50.0%</dd>",
            "<dt>Errors</dt><dd>1</dd>",
        ] {
            assert!(out.contains(want), "{want}\n{out}");
        }
        assert_eq!(
            out.matches(r#"<details class="eval-run">"#).count(),
            2,
            "{out}"
        );
    }

    #[test]
    fn no_runs_says_so() {
        let out = render(NAME, &report("support", 1_800_000_000, &[])).into_string();
        assert!(out.contains("No runs in this window"), "{out}");
    }

    #[test]
    fn chips_follow_the_score_order() {
        let mut r = report(
            "support",
            1_800_000_000,
            &[&[Outcome::Pass, Outcome::Fail, Outcome::Error]],
        );
        for (s, kind) in r.runs[0]
            .scores
            .iter_mut()
            .zip(["contains", "tools", "llm_judge"])
        {
            s.kind = kind.to_owned();
        }
        let out = render(NAME, &r).into_string();
        let summary = &out[out.find("<summary>").unwrap()..out.find("</summary>").unwrap()];
        let pass = summary
            .find(r#"<span class="chip chip-pass" title="contains">contains: pass</span>"#)
            .expect(summary);
        let fail = summary
            .find(r#"<span class="chip chip-fail" title="tools">tools: fail</span>"#)
            .expect(summary);
        let error = summary
            .find(r#"<span class="chip chip-error" title="llm_judge">llm_judge: error</span>"#)
            .expect(summary);
        assert!(pass < fail && fail < error, "{summary}");
        assert!(
            summary.contains("2027-01-15T07:59:00Z")
                && summary.contains("agent")
                && summary.contains("session-0"),
            "{summary}"
        );
    }

    #[test]
    fn details_show_scores_turns_and_tool_calls() {
        let mut r = report("support", 1_800_000_000, &[&[Outcome::Fail]]);
        let s = &mut r.runs[0].scores[0];
        s.kind = "llm_judge".to_owned();
        s.score = Some(2);
        s.rationale = "too terse".to_owned();
        let args = json!({"city": "Paris"}).as_object().unwrap().clone();
        r.runs[0].run.transcript.turns[0].calls.push(ToolCall {
            name: "weather".to_owned(),
            args,
        });
        let out = render(NAME, &r).into_string();
        assert!(out.contains(r#"<td class="mono">llm_judge</td>"#), "{out}");
        assert!(
            out.contains(r#"<td class="num">2</td><td>too terse</td>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<p class="who">User</p><pre>hi</pre>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<p class="who">Agent</p><pre>hello</pre>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<p class="call">Tool call <span class="mono">weather</span></p>"#),
            "{out}"
        );
        assert!(out.contains("&quot;city&quot;: &quot;Paris&quot;"), "{out}");
        assert!(
            out.contains(r#"Invocation <span class="mono">inv-0</span>"#),
            "{out}"
        );
    }

    #[test]
    fn trace_link_only_for_a_valid_trace_id() {
        let mut r = report(
            "support",
            1_800_000_000,
            &[&[Outcome::Pass], &[Outcome::Pass], &[Outcome::Pass]],
        );
        r.runs[0].run.trace_id = Some("0123456789ABCDEF0123456789ABCDEF".to_owned());
        r.runs[1].run.trace_id = Some("../prompts/x".to_owned());
        let out = render(NAME, &r).into_string();
        assert!(
            out.contains(r#"<a href="/traces/0123456789abcdef0123456789abcdef">trace</a>"#),
            "{out}"
        );
        // The nav links to "/traces" (no slash); only the valid id gets a "/traces/" link.
        assert_eq!(out.matches("/traces/").count(), 1, "{out}");
        assert!(!out.contains("prompts/x"), "{out}");
    }

    #[test]
    fn notes_and_run_errors_show_only_when_present() {
        let mut r = report(
            "support",
            1_800_000_000,
            &[&[Outcome::Error], &[Outcome::Pass]],
        );
        r.runs[0].run.tools_complete = false;
        r.runs[0].run.notes = vec!["TOOL_STARTING row 3: truncated".to_owned()];
        r.runs[0].run.transcript.error = Some("agent timed out".to_owned());
        r.runs[1].run.notes = vec!["ignored when complete".to_owned()];
        let out = render(NAME, &r).into_string();
        assert!(
            out.contains("<li>TOOL_STARTING row 3: truncated</li>"),
            "{out}"
        );
        assert!(out.contains("Run error: agent timed out"), "{out}");
        assert!(!out.contains("ignored when complete"), "{out}");
        assert_eq!(out.matches("Incomplete transcript").count(), 1, "{out}");
    }

    #[test]
    fn incomplete_run_without_notes_renders_no_empty_list() {
        let mut r = report("support", 1_800_000_000, &[&[Outcome::Pass]]);
        r.runs[0].run.tools_complete = false;
        r.runs[0].run.notes = vec![];
        let out = render(NAME, &r).into_string();
        assert!(out.contains("Incomplete transcript"), "{out}");
        assert!(!out.contains("<ul></ul>"), "{out}");
    }

    #[test]
    fn report_text_is_escaped() {
        let mut r = report("support", 1_800_000_000, &[&[Outcome::Fail]]);
        r.runs[0].run.transcript.turns[0].user = "<script>alert(1)</script>".to_owned();
        r.runs[0].run.transcript.turns[0].reply = "<img src=x>".to_owned();
        r.runs[0].scores[0].rationale = "<b>bold</b>".to_owned();
        r.runs[0].run.agent = "<i>a</i>".to_owned();
        let out = render(NAME, &r).into_string();
        for raw in ["<script>alert", "<img src=x>", "<b>bold</b>", "<i>a</i>"] {
            assert!(!out.contains(raw), "{raw}\n{out}");
        }
        assert!(
            out.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{out}"
        );
    }
}
