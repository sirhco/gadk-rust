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

//! Port of ADK-Go `agent/agent_on_error_test.go` (R9: `OnAgentErrorCallback`, #2229/#2260).

use std::sync::Arc;
use std::time::Duration;

use futures::{FutureExt, StreamExt};
use gadk_genai::{Content, ROLE_MODEL};
use gadk_session::Event;
use parking_lot::Mutex;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context as LayerContext, Layer, SubscriberExt};

use crate::agent::{Agent, AgentConfig, AgentRunFn, new, pin_tracing_interest};
use crate::callback::{AfterAgentCallback, BeforeAgentCallback, OnAgentErrorCallback};
use crate::context::{CancelCause, InvocationContext};
use crate::error::{AgentError, chain_any, wrap};
use crate::plugin::{Plugin, PluginConfig};
use crate::plugin_manager::PluginManager;

/// What the observer plugin saw (Go's `calls`, `callbackErr`, `callbackAgent`; the error is kept
/// as its text and the `errors.Is` answers, since a borrowed `AgentError` cannot outlive the call).
#[derive(Default)]
struct Seen {
    calls: usize,
    err_text: String,
    is_run_err: bool,
    is_cancelled: bool,
    is_deadline: bool,
    agent: String,
}

/// How a case's invocation context is cancelled (Go `invocationCtx`).
#[derive(Clone, Copy)]
enum Cancel {
    None,
    Canceled,
    Deadline,
    CanceledWithCause,
    DeadlineWithCause,
}

/// What a case's run function yields (Go `run`).
#[derive(Clone, Copy)]
enum Run {
    /// Go `run: nil`: the agent has no run function.
    Missing,
    Nothing,
    Err,
    Event,
    WrappedCancelled,
    WrappedDeadline,
    WrappedCause,
    WrappedRunErr,
}

const RUN_ERR: &str = "run failed";

fn is_run_err(e: &AgentError) -> bool {
    chain_any(
        e,
        &|x| matches!(x.downcast_ref::<AgentError>(), Some(AgentError::Other(b)) if b.to_string() == RUN_ERR),
    )
}

fn run_fn(kind: Run) -> Option<AgentRunFn> {
    let f: AgentRunFn = match kind {
        Run::Missing => return None,
        Run::Nothing => Arc::new(|_ctx: InvocationContext| futures::stream::empty().boxed()),
        Run::Err => {
            Arc::new(|_ctx| futures::stream::iter([Err(AgentError::other(RUN_ERR))]).boxed())
        }
        Run::Event => Arc::new(|ctx: InvocationContext| {
            let ev = Event::new(ctx.platform(), ctx.invocation_id());
            futures::stream::iter([Ok(ev)]).boxed()
        }),
        Run::WrappedCancelled => Arc::new(|_ctx| {
            futures::stream::iter([Err(wrap("wrapped", AgentError::Cancelled))]).boxed()
        }),
        Run::WrappedDeadline => Arc::new(|_ctx| {
            futures::stream::iter([Err(wrap("wrapped", AgentError::DeadlineExceeded))]).boxed()
        }),
        Run::WrappedCause => Arc::new(|ctx: InvocationContext| {
            let cause = ctx.cause().expect("cancelled context");
            futures::stream::iter([Err(wrap("wrapped", cause.to_agent_error()))]).boxed()
        }),
        Run::WrappedRunErr => Arc::new(|_ctx| {
            futures::stream::iter([Err(wrap("wrapped", AgentError::other(RUN_ERR)))]).boxed()
        }),
    };
    Some(f)
}

fn err_cb(text: &'static str) -> BeforeAgentCallback {
    Arc::new(move |_ctx| async move { Err(AgentError::other(text)) }.boxed())
}

fn content_cb(text: &'static str) -> AfterAgentCallback {
    Arc::new(move |_ctx| async move { Ok(Some(Content::from_text(text, ROLE_MODEL))) }.boxed())
}

fn observer(seen: &Arc<Mutex<Seen>>) -> Plugin {
    let seen = seen.clone();
    let cb: OnAgentErrorCallback = Arc::new(move |_ctx, a: Arc<dyn Agent>, err: &AgentError| {
        let mut s = seen.lock();
        s.calls += 1;
        s.err_text = err.to_string();
        s.is_run_err = is_run_err(err);
        s.is_cancelled = chain_any(err, &|x| {
            matches!(x.downcast_ref::<AgentError>(), Some(AgentError::Cancelled))
        });
        s.is_deadline = chain_any(err, &|x| {
            matches!(
                x.downcast_ref::<AgentError>(),
                Some(AgentError::DeadlineExceeded)
            )
        });
        s.agent = a.name().to_owned();
    });
    Plugin::new(PluginConfig {
        name: "observer".into(),
        on_agent_error_callback: Some(cb),
        ..Default::default()
    })
}

fn manager(plugins: Vec<Plugin>) -> Arc<PluginManager> {
    Arc::new(PluginManager::new(plugins, Duration::ZERO).expect("NewPluginManager()"))
}

fn invocation(a: &Arc<dyn Agent>, pm: Arc<PluginManager>) -> InvocationContext {
    InvocationContext::builder()
        .agent(a.clone())
        .invocation_id("inv-1")
        .plugin_manager(pm)
        .build_invocation()
}

fn failing_agent(run: Option<AgentRunFn>) -> Arc<dyn Agent> {
    new(AgentConfig {
        name: "failing".into(),
        run,
        ..Default::default()
    })
    .expect("agent.New()")
}

struct Case {
    name: &'static str,
    before: Vec<BeforeAgentCallback>,
    after: Vec<AfterAgentCallback>,
    cancel: Cancel,
    run: Run,
    want_calls: usize,
    want_run_err: bool,
    want_run_event: bool,
    /// Go `wantCallbackErr`: which sentinel `errors.Is` must find.
    want_is: Option<&'static str>,
    want_text: Option<&'static str>,
}

fn case(name: &'static str, run: Run) -> Case {
    Case {
        name,
        before: vec![],
        after: vec![],
        cancel: Cancel::None,
        run,
        want_calls: 0,
        want_run_err: false,
        want_run_event: false,
        want_is: None,
        want_text: None,
    }
}

// parity: adapted: the callback gets `&AgentError`, so Go's `callbackErr != runErr` identity check compares the error's text with the yielded one
#[tokio::test]
async fn test_agent_run_on_agent_error_callback() {
    let cases = vec![
        Case {
            want_calls: 1,
            want_run_err: true,
            want_is: Some("run"),
            ..case("error from run is reported", Run::Err)
        },
        Case {
            before: vec![err_cb("before failed")],
            want_calls: 1,
            want_run_err: true,
            want_text: Some("failed to run before agent callback: before failed"),
            ..case("error from before agent callback is reported", Run::Nothing)
        },
        Case {
            after: vec![err_cb("after failed")],
            want_calls: 1,
            want_run_err: true,
            want_text: Some("failed to run after agent callback: after failed"),
            ..case("error from after agent callback is reported", Run::Nothing)
        },
        case("successful run is not reported", Run::Nothing),
        Case {
            before: vec![content_cb("short circuit")],
            want_run_event: true,
            ..case(
                "successful before agent callback event is not reported",
                Run::Nothing,
            )
        },
        Case {
            want_run_event: true,
            ..case("successful run event is not reported", Run::Event)
        },
        Case {
            after: vec![content_cb("after callback")],
            want_run_event: true,
            ..case(
                "successful after agent callback event is not reported",
                Run::Nothing,
            )
        },
        Case {
            cancel: Cancel::Canceled,
            want_run_err: true,
            ..case("caller cancellation is not reported", Run::WrappedCancelled)
        },
        Case {
            cancel: Cancel::Deadline,
            want_run_err: true,
            ..case("caller deadline is not reported", Run::WrappedDeadline)
        },
        Case {
            cancel: Cancel::CanceledWithCause,
            want_run_err: true,
            ..case(
                "caller cancellation with cause is not reported",
                Run::WrappedCause,
            )
        },
        Case {
            cancel: Cancel::DeadlineWithCause,
            want_run_err: true,
            ..case(
                "caller deadline with cause is not reported",
                Run::WrappedCause,
            )
        },
        Case {
            cancel: Cancel::CanceledWithCause,
            want_calls: 1,
            want_run_err: true,
            want_is: Some("run"),
            ..case(
                "unrelated error while cancelled with cause is reported",
                Run::WrappedRunErr,
            )
        },
        Case {
            want_calls: 1,
            want_run_err: true,
            want_is: Some("deadline"),
            ..case("live invocation deadline is reported", Run::WrappedDeadline)
        },
        Case {
            want_calls: 1,
            want_run_err: true,
            want_is: Some("cancelled"),
            ..case(
                "live invocation wrapped cancellation is reported",
                Run::WrappedCancelled,
            )
        },
        Case {
            want_calls: 1,
            want_run_err: true,
            want_text: Some(r#"agent "failing" has no Run function"#),
            ..case("missing run function is reported", Run::Missing)
        },
    ];

    for tc in cases {
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let pm = manager(vec![observer(&seen)]);
        let a = new(AgentConfig {
            name: "failing".into(),
            before_agent_callbacks: tc.before,
            after_agent_callbacks: tc.after,
            run: run_fn(tc.run),
            ..Default::default()
        })
        .expect("agent.New()");
        let ic = invocation(&a, pm);
        let caller_gave_up: Arc<dyn std::error::Error + Send + Sync> =
            Arc::from(Box::<dyn std::error::Error + Send + Sync>::from(
                "caller gave up",
            ));
        match tc.cancel {
            Cancel::None => {}
            Cancel::Canceled => ic.cancel_with_cause(CancelCause::Canceled),
            Cancel::Deadline => ic.cancel_with_cause(CancelCause::DeadlineExceeded),
            // Go WithCancelCause / WithTimeoutCause: the context's error is Canceled /
            // DeadlineExceeded and its cause the caller's error; gadk records only the cause.
            Cancel::CanceledWithCause | Cancel::DeadlineWithCause => {
                ic.cancel_with_cause(CancelCause::Error(caller_gave_up.clone()))
            }
        }

        let mut run_err: Option<String> = None;
        let mut run_event = false;
        let mut s = a.run(ic);
        while let Some(item) = s.next().await {
            match item {
                Ok(_) => run_event = true,
                Err(e) => run_err = Some(e.to_string()),
            }
        }
        assert_eq!(
            run_err.is_some(),
            tc.want_run_err,
            "{}: Run() error = {run_err:?}, want error = {}",
            tc.name,
            tc.want_run_err
        );
        assert_eq!(run_event, tc.want_run_event, "{}: Run() event", tc.name);
        let seen = seen.lock();
        assert_eq!(
            seen.calls, tc.want_calls,
            "{}: OnAgentErrorCallback calls",
            tc.name
        );
        if seen.calls == 0 {
            continue;
        }
        assert_eq!(
            Some(&seen.err_text),
            run_err.as_ref(),
            "{}: callback error vs Run() error; want the same error",
            tc.name
        );
        match tc.want_is {
            Some("run") => assert!(
                seen.is_run_err,
                "{}: callback error = {}",
                tc.name, seen.err_text
            ),
            Some("cancelled") => assert!(
                seen.is_cancelled,
                "{}: callback error = {}",
                tc.name, seen.err_text
            ),
            Some("deadline") => assert!(
                seen.is_deadline,
                "{}: callback error = {}",
                tc.name, seen.err_text
            ),
            _ => {}
        }
        if let Some(text) = tc.want_text {
            assert_eq!(seen.err_text, text, "{}: callback error text", tc.name);
        }
        assert_eq!(seen.agent, "failing", "{}: callback agent name", tc.name);
    }
}

/// Collects the `exception.message` of every event (Go's in-memory span exporter's `exception`
/// events).
#[derive(Clone, Default)]
struct Exceptions(Arc<Mutex<Vec<String>>>);

struct Message<'a>(&'a mut Option<String>);
impl Visit for Message<'_> {
    fn record_str(&mut self, f: &Field, v: &str) {
        if f.name() == "exception.message" {
            *self.0 = Some(v.to_owned());
        }
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        if f.name() == "exception.message" {
            *self.0 = Some(format!("{v:?}"));
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for Exceptions {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: LayerContext<'_, S>) {
        let mut msg = None;
        event.record(&mut Message(&mut msg));
        if let Some(m) = msg {
            self.0.lock().push(m);
        }
    }
}

fn named_plugin(name: &str, cb: OnAgentErrorCallback) -> Plugin {
    Plugin::new(PluginConfig {
        name: name.into(),
        on_agent_error_callback: Some(cb),
        ..Default::default()
    })
}

fn run_err_agent() -> Arc<dyn Agent> {
    failing_agent(run_fn(Run::Err))
}

#[tokio::test]
async fn test_agent_run_on_agent_error_callback_recovers_panic() {
    pin_tracing_interest();
    let exceptions = Exceptions::default();
    let _g =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(exceptions.clone()));

    let first_called = Arc::new(Mutex::new(false));
    let notified: Arc<Mutex<Vec<String>>> = Arc::default();
    let (fc, n) = (first_called.clone(), notified.clone());
    let pm = manager(vec![
        named_plugin(
            "panicking",
            Arc::new(move |_, _, _| {
                *fc.lock() = true;
                panic!("plugin blew up");
            }),
        ),
        named_plugin(
            "observer",
            Arc::new(move |_, _, _| n.lock().push("observer".into())),
        ),
    ]);
    let a = run_err_agent();
    let mut got_err = None;
    let mut s = a.run(invocation(&a, pm));
    while let Some(item) = s.next().await {
        if let Err(e) = item {
            got_err = Some(e);
        }
    }

    assert!(*first_called.lock(), "panicking callback was not called");
    assert_eq!(*notified.lock(), ["observer"], "notified plugins");
    let got_err = got_err.expect("Run() error");
    assert!(
        is_run_err(&got_err),
        "Run() error = {got_err}, want original error"
    );
    let recorded = exceptions
        .0
        .lock()
        .iter()
        .any(|m| m.contains("OnAgentErrorCallback panicked"));
    assert!(recorded, "panic was not recorded on the span");
}

#[tokio::test]
async fn test_agent_run_on_agent_error_callback_runs_before_yield() {
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let o = order.clone();
    let pm = manager(vec![named_plugin(
        "observer",
        Arc::new(move |_, _, _| o.lock().push("callback")),
    )]);
    let a = run_err_agent();
    let mut s = a.run(invocation(&a, pm));
    while let Some(item) = s.next().await {
        if item.is_err() {
            order.lock().push("yield");
        }
    }
    assert_eq!(*order.lock(), ["callback", "yield"], "order");
}

#[tokio::test]
async fn test_agent_run_on_agent_error_callback_notifies_every_plugin() {
    let notified: Arc<Mutex<Vec<String>>> = Arc::default();
    let plugin = |name: &'static str| {
        let n = notified.clone();
        named_plugin(name, Arc::new(move |_, _, _| n.lock().push(name.into())))
    };
    let no_callback = Plugin::new(PluginConfig {
        name: "no-callback".into(),
        ..Default::default()
    });
    let pm = manager(vec![plugin("first"), no_callback, plugin("second")]);
    let a = run_err_agent();
    let mut got_err = false;
    let mut s = a.run(invocation(&a, pm));
    while let Some(item) = s.next().await {
        got_err |= item.is_err();
    }
    assert!(got_err, "Run() error = nil, want error");
    assert_eq!(*notified.lock(), ["first", "second"], "notified plugins");
}
