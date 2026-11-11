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

//! Go `cmd/launcher/web/triggers/internal/triggerauth`: the `-oidc_audience` and
//! `-oidc_service_accounts` flags shared by the trigger sub-launchers, and [`Trigger`], the
//! flags, `Parse` and `SetupSubrouters` that Go's `pubsub` and `eventarc` packages each spell out
//! in full (they differ only in the keyword, the controller and the help and log wording).

use std::convert::Infallible;
use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard};

use gadk_platform::go_flag::{Flag, FlagSet};
use gadk_platform::{GoDuration, go_quote};
use gadk_server::authn::{self, Authenticator, AuthnError, GoogleOidcConfig};
use gadk_server::mux::{Handler, Request, handler};
use gadk_server::rest::ServerError;
use gadk_server::rest::triggers::{ControllerConfig, TriggerConfig};
use http::Method;
use tower::{Layer, Service};

use crate::config::Config;
use crate::error::Error;
use crate::flag_usage::format_flag_usage;
use crate::web::WebRouter;

/// Go `authn.NewGoogleOIDC`'s signature: the seam Go's tests replace (`newGoogleOIDC`) to see the
/// config [`Flags::authenticator`] builds, which the returned authenticator does not expose.
pub(crate) type NewGoogleOidc =
    dyn Fn(GoogleOidcConfig) -> Result<Arc<dyn Authenticator>, AuthnError>;

/// Go `triggerauth.Flags`: the `-oidc_audience` and `-oidc_service_accounts` flags.
pub(crate) struct Flags {
    audience: Flag<String>,
    service_accounts: Flag<String>,
}

impl Flags {
    /// Go `(*Flags).Register(fs)`: adds the flags to `fs`.
    pub(crate) fn register(fs: &mut FlagSet) -> Flags {
        let audience = fs.string(
            "oidc_audience",
            "",
            "Audience a Google-signed OIDC token must carry to call this trigger endpoint: the token audience configured on the push subscription or Eventarc trigger. Requires -oidc_service_accounts. When neither flag is given, the endpoint is unauthenticated, and an empty value for either is refused. Like every flag of this trigger, it must follow the trigger's own keyword: in \"pubsub eventarc -oidc_audience=...\" it configures eventarc only.",
        );
        let service_accounts = fs.string(
            "oidc_service_accounts",
            "",
            "Comma-separated service account emails allowed to call this trigger endpoint, matched against the token's verified email. Requires -oidc_audience.",
        );
        Flags {
            audience,
            service_accounts,
        }
    }

    /// Go `(*Flags).Authenticator`: the authenticator the flags describe; `None` (neither flag
    /// passed) keeps the endpoint unauthenticated. `fs` is the set the flags were registered on
    /// (Go keeps it in `Flags.fs` for `passed`).
    pub(crate) fn authenticator(
        &self,
        fs: &FlagSet,
    ) -> Result<Option<Arc<dyn Authenticator>>, Error> {
        authenticator_with(
            &self.audience.get(),
            &self.service_accounts.get(),
            &|name| fs.is_set(name),
            &authn::google_oidc,
        )
    }
}

/// Go `(*Flags).Authenticator` over the flag values: `passed` is Go's `f.passed(name)` (whether
/// the command line set the flag) and `new_google_oidc` Go's `newGoogleOIDC`.
pub(crate) fn authenticator_with(
    audience: &str,
    service_accounts: &str,
    passed: &dyn Fn(&str) -> bool,
    new_google_oidc: &NewGoogleOidc,
) -> Result<Option<Arc<dyn Authenticator>>, Error> {
    // A launch script reading an unset variable passes the flag empty, and leaving the endpoint
    // open for that would hide the mistake.
    for (name, value) in [
        ("oidc_audience", audience),
        ("oidc_service_accounts", service_accounts),
    ] {
        if value.is_empty() && passed(name) {
            return Err(Error::Config(format!(
                "-{name} is empty: give it a value or leave it out"
            )));
        }
    }
    if audience.is_empty() && service_accounts.is_empty() {
        return Ok(None);
    }
    // No real audience or service account starts with "-". A space-separated flag whose
    // unquoted variable is unset takes the next flag as its value.
    if audience.starts_with('-') {
        return Err(Error::Config(format!(
            "-oidc_audience {} starts with \"-\", as when an unset variable makes a flag take the next flag as its value",
            go_quote(audience)
        )));
    }
    // Go `strings.TrimSpace` and `str::trim` remove the same (Unicode White_Space) characters.
    if service_accounts.trim().starts_with('-') {
        return Err(Error::Config(format!(
            "-oidc_service_accounts {} starts with \"-\", as when an unset variable makes a flag take the next flag as its value",
            go_quote(service_accounts)
        )));
    }
    if service_accounts.is_empty() {
        return Err(Error::Config(
            "-oidc_audience requires -oidc_service_accounts: the audience alone does not identify the caller"
                .to_owned(),
        ));
    }
    if audience.is_empty() {
        return Err(Error::Config(
            "-oidc_service_accounts requires -oidc_audience".to_owned(),
        ));
    }
    // Tokens are matched on the exact audience, so a padded one would start cleanly and then
    // reject every delivery.
    if audience != audience.trim() {
        return Err(Error::Config(format!(
            "-oidc_audience {} has surrounding whitespace",
            go_quote(audience)
        )));
    }
    let mut accounts = Vec::new();
    for account in service_accounts.split(',') {
        let account = account.trim();
        if account.is_empty() {
            return Err(Error::Config(format!(
                "-oidc_service_accounts {} has an empty entry",
                go_quote(service_accounts)
            )));
        }
        if account.starts_with('-') {
            return Err(Error::Config(format!(
                "-oidc_service_accounts {} has an entry starting with \"-\"",
                go_quote(service_accounts)
            )));
        }
        accounts.push(account.to_owned());
    }
    new_google_oidc(GoogleOidcConfig {
        audience: audience.to_owned(),
        allowed_service_accounts: accounts,
    })
    .map(Some)
    .map_err(|e| Error::Config(e.to_string()))
}

/// Go `pubsubLauncher`/`eventarcLauncher` without what tells them apart: the flag set with Go's
/// `pubsubConfig`/`eventarcConfig` flags, `Parse`, `CommandLineSyntax` and `SetupSubrouters`.
pub(crate) struct Trigger {
    /// The sub-launcher keyword (`pubsub` or `eventarc`), also the flag set's name and the last
    /// segment of the endpoint path.
    keyword: &'static str,
    /// Behind a lock: a set with `Func` flags is not `Sync`, and sub-launchers are.
    flags: Mutex<FlagSet>,
    path_prefix_flag: Flag<String>,
    trigger_max_retries: Flag<i64>,
    trigger_base_delay: Flag<GoDuration>,
    trigger_max_delay: Flag<GoDuration>,
    trigger_max_runs: Flag<i64>,
    auth_flags: Flags,
    /// Go `config.pathPrefix` once `Parse` normalized it (Go writes it back to the flag's
    /// variable).
    normalized_path_prefix: Option<String>,
    /// Go `auth`: built from the flags by `parse`. `None` leaves the endpoint open.
    pub(crate) auth: Option<Arc<dyn Authenticator>>,
}

impl Trigger {
    /// Go `NewLauncher`'s flag set: `name` is the keyword, `endpoint` the endpoint's name in the
    /// `-path_prefix` help (`PubSub`, `Eventarc`).
    pub(crate) fn new(keyword: &'static str, endpoint: &str) -> Trigger {
        let mut fs = FlagSet::new(keyword);
        let path_prefix_flag = fs.string(
            "path_prefix",
            "/api",
            &format!("Path prefix for the {endpoint} trigger endpoint. Default is '/api'."),
        );
        let trigger_max_retries = fs.int(
            "trigger_max_retries",
            3,
            "Maximum retries for HTTP 429 errors from triggers",
        );
        let trigger_base_delay = fs.duration(
            "trigger_base_delay",
            GoDuration::SECOND,
            "Base delay for trigger retry exponential backoff",
        );
        let trigger_max_delay = fs.duration(
            "trigger_max_delay",
            GoDuration::from_secs(10),
            "Maximum delay for trigger retry exponential backoff",
        );
        let trigger_max_runs = fs.int(
            "trigger_max_concurrent_runs",
            100,
            "Maximum concurrent trigger runs",
        );
        let auth_flags = Flags::register(&mut fs);
        Trigger {
            keyword,
            flags: Mutex::new(fs),
            path_prefix_flag,
            trigger_max_retries,
            trigger_base_delay,
            trigger_max_delay,
            trigger_max_runs,
            auth_flags,
            normalized_path_prefix: None,
            auth: None,
        }
    }

    /// Go `fs.SetOutput`.
    pub(crate) fn set_flag_output(&mut self, out: Arc<Mutex<dyn Write + Send>>) {
        lock(&self.flags).set_output(out);
    }

    /// Go `config.pathPrefix`: the flag's value, normalized once `parse` got that far.
    pub(crate) fn path_prefix(&self) -> String {
        self.normalized_path_prefix
            .clone()
            .unwrap_or_else(|| self.path_prefix_flag.get())
    }

    /// Go `config.triggerMaxRetries`.
    #[cfg(test)]
    pub(crate) fn trigger_max_retries(&self) -> i64 {
        self.trigger_max_retries.get()
    }

    /// Go `Parse`.
    pub(crate) fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        // Go also fails on `!fs.Parsed()`, which cannot hold after `Parse`.
        let rest = {
            let mut fs = lock(&self.flags);
            fs.parse(&args).map(|()| fs.args())
        };
        let rest = match rest {
            Ok(rest) => rest,
            Err(e) => {
                return Err(Error::Parse(format!(
                    "failed to parse {} flags: {e}",
                    self.keyword
                )));
            }
        };
        if self.trigger_max_retries.get() <= 0 {
            return Err(Error::Config("trigger_max_retries must be > 0".to_owned()));
        }
        if self.trigger_base_delay.get() < GoDuration(0) {
            return Err(Error::Config("trigger_base_delay must be >= 0".to_owned()));
        }
        if self.trigger_max_delay.get() <= GoDuration(0) {
            return Err(Error::Config("trigger_max_delay must be > 0".to_owned()));
        }
        if self.trigger_max_runs.get() <= 0 {
            return Err(Error::Config(
                "trigger_max_concurrent_runs must be > 0".to_owned(),
            ));
        }

        let mut prefix = self.path_prefix_flag.get();
        if !prefix.starts_with('/') {
            prefix.insert(0, '/');
        }
        if prefix.ends_with('/') {
            prefix.pop();
        }
        self.normalized_path_prefix = Some(prefix);

        self.auth = None;
        self.auth = self.auth_flags.authenticator(&lock(&self.flags))?;

        Ok(rest)
    }

    /// Go `CommandLineSyntax`.
    pub(crate) fn command_line_syntax(&self) -> String {
        format_flag_usage(&lock(&self.flags))
    }

    /// Go `SetupSubrouters`: the controller `new_handler` builds from the launcher config and
    /// the trigger flags, mounted for POST at `<prefix>/apps/{app_name}/trigger/<keyword>` behind
    /// the authenticator (or, without one, Go's warning that the endpoint is open).
    ///
    /// Go mounts the route on `router.PathPrefix(prefix).Subrouter()` (the whole router for an
    /// empty or `/` prefix). A subrouter's `Path` template is the prefix with its trailing slashes
    /// trimmed followed by the route's own, it inherits the parent's `StrictSlash`, and a method
    /// mismatch inside it lets the parent try its later routes; that is exactly a route for the
    /// joined template on the parent, which is what this registers.
    pub(crate) fn setup_subrouters(
        &self,
        router: &mut WebRouter,
        config: &Config,
        new_handler: impl FnOnce(ControllerConfig) -> Result<Handler, ServerError>,
    ) -> Result<(), Error> {
        let trigger_config = TriggerConfig {
            // Go `int`s: saturated into the controller's `i32`s (validated > 0).
            max_retries: saturate(self.trigger_max_retries.get()),
            base_delay: self.trigger_base_delay.get().to_std().unwrap_or_default(),
            max_delay: self.trigger_max_delay.get().to_std().unwrap_or_default(),
            max_concurrent_runs: saturate(self.trigger_max_runs.get()),
        };
        let handler = new_handler(ControllerConfig {
            // The web launcher fills every service before setup; Go's controller would keep a
            // nil one, this falls back to an in-memory service.
            session_service: config
                .session_service
                .clone()
                .unwrap_or_else(|| Arc::new(gadk_session::InMemorySessionService::new())),
            agent_loader: Arc::clone(&config.agent_loader),
            memory_service: config.memory_service.clone(),
            artifact_service: config.artifact_service.clone(),
            plugin_config: config.plugin_config.clone(),
            trigger_config,
            compaction: config.compaction.clone(),
            platform: gadk_platform::Platform::default(),
        })
        .map_err(|e| Error::Config(e.to_string()))?;

        let prefix = self.path_prefix();
        let route = format!("/apps/{{app_name}}/trigger/{}", self.keyword);
        let path = if prefix.is_empty() || prefix == "/" {
            route
        } else {
            format!("{}{route}", prefix.trim_end_matches('/'))
        };

        if self.auth.is_none() {
            config.log.print(&format!(
                "adk: the {kw} trigger endpoint is unauthenticated, so any caller that reaches it chooses the user ID the agent runs as. Set -oidc_audience and -oidc_service_accounts after the {kw} keyword to require a Google-signed OIDC token.",
                kw = self.keyword
            ));
        }
        // The handler takes the run's user ID from the delivery metadata, so with auth on only an
        // allow-listed service account can choose it.
        router.handle(
            &[Method::POST],
            &path,
            authn_middleware(self.auth.clone(), handler),
        );
        Ok(())
    }
}

/// Go `int` → the controller's `i32`.
fn saturate(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

/// Go `authn.Middleware(a)(next)`: `next` itself when `a` is nil.
fn authn_middleware(auth: Option<Arc<dyn Authenticator>>, next: Handler) -> Handler {
    let Some(auth) = auth else {
        return next;
    };
    let service = authn::middleware(auth).layer(tower::service_fn(move |req: Request| {
        let next = Arc::clone(&next);
        async move { Ok::<_, Infallible>(next(req).await) }
    }));
    handler(move |req: Request| {
        // The inner `service_fn` is always ready, so `call` needs no `poll_ready` first; the
        // returned future is `'static`.
        let response = service.clone().call(req);
        async move {
            match response.await {
                Ok(resp) => resp,
                Err(never) => match never {},
            }
        }
    })
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::{Duration, Instant};

    use axum::body::Body;
    use futures::StreamExt;
    use gadk_agent::{AgentConfig, SingleLoader, new_agent};
    use gadk_server::mux::Response;
    use http::StatusCode;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::golog::Captured;
    use crate::launcher::SubLauncher;
    use crate::web::triggers::{eventarc, pubsub};
    use crate::web::{self as webl, WebSublauncher};

    /// The fixed `golog` clock of these tests.
    const TS: &str = "2009/11/10 23:00:00 ";

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    /// A config serving a do-nothing agent named `name`, its log captured with the fixed clock.
    fn config_serving(name: &str) -> (Config, Captured) {
        let agent = new_agent(AgentConfig {
            name: name.into(),
            run: Some(Arc::new(|_ctx| futures::stream::empty().boxed())),
            ..Default::default()
        })
        .unwrap();
        let (log, out) = crate::golog::capture();
        let mut config = Config::new(Arc::new(SingleLoader::new(agent)));
        config.log = log.with_clock(|| TS.to_owned());
        (config, out)
    }

    fn post(path: &str) -> Request {
        http::Request::builder()
            .method(Method::POST)
            .uri(path)
            .body(Body::from("{"))
            .unwrap()
    }

    async fn status_and_location(router: &WebRouter, req: Request) -> (StatusCode, String) {
        let resp: Response = router.serve(req).await;
        let location = resp
            .headers()
            .get(http::header::LOCATION)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        (resp.status(), location)
    }

    #[test]
    fn help_text_is_go() {
        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut l = pubsub::new().with_flag_output(out.clone());
        let err = l.parse(strings(&["-h"])).unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to parse pubsub flags: flag: help requested"
        );
        let written = String::from_utf8(out.lock().unwrap().clone()).unwrap();
        assert_eq!(
            written,
            format!("Usage of pubsub:\n{}", l.command_line_syntax())
        );

        // Task 2's CLI fixtures: the usage `web <trigger> -h` writes before the launcher's own
        // `Run failed` record.
        for (keyword, l) in [
            ("pubsub", Box::new(pubsub::new()) as Box<dyn WebSublauncher>),
            ("eventarc", Box::new(eventarc::new())),
        ] {
            let f = cli_fixture(&format!("help_web_{keyword}"));
            let stderr = f["stderr"].as_str().unwrap();
            let usage = stderr.split("<ts> ").next().unwrap();
            assert_eq!(
                format!("Usage of {keyword}:\n{}", l.command_line_syntax()),
                usage,
                "{keyword}: usage"
            );
        }
    }

    fn cli_fixture(name: &str) -> Value {
        let path = format!("{}/testdata/cli/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    /// Task 2's CLI fixtures for the trigger flags: the error `Execute` returns, through the web
    /// launcher's parse (which adds `the "<kw>" launcher cannot parse arguments: `).
    #[test]
    fn parse_errors_match_go_cli_fixtures() {
        for name in [
            "help_web_pubsub",
            "help_web_eventarc",
            "pubsub_max_retries_zero",
            "pubsub_negative_base_delay",
            "eventarc_max_delay_zero",
            "eventarc_max_concurrent_runs_zero",
            "oidc_accounts_empty_entry",
            "oidc_audience_dash",
            "oidc_audience_empty",
            "oidc_accounts_without_audience",
            "oidc_audience_without_accounts",
        ] {
            let f = cli_fixture(name);
            let args: Vec<String> = f["args"].as_array().unwrap()[1..]
                .iter()
                .map(|a| a.as_str().unwrap().to_owned())
                .collect();
            let sink = Arc::new(Mutex::new(Vec::<u8>::new()));
            let mut l = webl::new(vec![
                Box::new(pubsub::new().with_flag_output(sink.clone())),
                Box::new(eventarc::new().with_flag_output(sink)),
            ]);
            let err = SubLauncher::parse(&mut l, args).unwrap_err();
            assert_eq!(err.to_string(), f["error"].as_str().unwrap(), "{name}");
        }
    }

    #[test]
    fn parse_validates_every_trigger_flag_with_go_text() {
        for (args, want) in [
            (
                vec!["-nope"],
                "failed to parse eventarc flags: flag provided but not defined: -nope",
            ),
            (
                vec!["-trigger_max_retries=0"],
                "trigger_max_retries must be > 0",
            ),
            (
                vec!["-trigger_base_delay=-1ns"],
                "trigger_base_delay must be >= 0",
            ),
            (
                vec!["-trigger_max_delay=0"],
                "trigger_max_delay must be > 0",
            ),
            (
                vec!["-trigger_max_concurrent_runs=-3"],
                "trigger_max_concurrent_runs must be > 0",
            ),
            (
                vec!["-oidc_audience=a", "-oidc_service_accounts=b, ,c"],
                r#"-oidc_service_accounts "b, ,c" has an empty entry"#,
            ),
        ] {
            let sink = Arc::new(Mutex::new(Vec::<u8>::new()));
            let mut l = eventarc::new().with_flag_output(sink);
            let err = l.parse(strings(&args)).unwrap_err();
            assert_eq!(err.to_string(), want, "{args:?}");
        }
        // A zero base delay is allowed; the remaining arguments are returned.
        let mut l = eventarc::new();
        assert_eq!(
            l.parse(strings(&["-trigger_base_delay=0", "api", "-x"]))
                .unwrap(),
            strings(&["api", "-x"])
        );
    }

    #[test]
    fn path_prefix_is_normalized_and_named_in_the_user_message() {
        for (flag, want) in [
            (None, "/api"),
            (Some("custom"), "/custom"),
            (Some("/custom/"), "/custom"),
            (Some("/a//"), "/a/"),
            (Some(""), ""),
            (Some("/"), ""),
        ] {
            let args = flag
                .map(|p| vec![format!("-path_prefix={p}")])
                .unwrap_or_default();
            let mut p = pubsub::new();
            p.parse(args.clone()).unwrap();
            let mut e = eventarc::new();
            e.parse(args).unwrap();
            assert_eq!(p.trigger.path_prefix(), want, "{flag:?}");
            let mut lines = Vec::new();
            p.user_message("http://localhost:8080", &mut |l| lines.push(l.to_owned()));
            e.user_message("http://localhost:8080", &mut |l| lines.push(l.to_owned()));
            assert_eq!(
                lines,
                vec![
                    format!(
                        "       pubsub:  PubSub trigger endpoint is available at http://localhost:8080{want}/apps/{{app_name}}/trigger/pubsub"
                    ),
                    format!(
                        "       eventarc: Eventarc trigger endpoint is available at http://localhost:8080{want}/apps/{{app_name}}/trigger/eventarc"
                    ),
                ],
                "{flag:?}"
            );
        }
        assert_eq!(pubsub::new().keyword(), "pubsub");
        assert_eq!(eventarc::new().keyword(), "eventarc");
        assert_eq!(
            pubsub::new().simple_description(),
            "starts ADK PubSub trigger endpoint server"
        );
        assert_eq!(
            eventarc::new().simple_description(),
            "starts ADK Eventarc trigger endpoint server"
        );
    }

    /// Go's subrouter mounting, through the web launcher's strict-slash router: the route is
    /// POST-only under the prefix (or at the root), a trailing slash is StrictSlash-redirected,
    /// and another method falls through to the routes registered after it.
    #[tokio::test]
    async fn routes_mount_like_gorilla_subrouters() {
        for (prefix, path) in [
            (None, "/api/apps/a/trigger/pubsub"),
            (Some("/x/"), "/x/apps/a/trigger/pubsub"),
            (Some("/a//"), "/a/apps/a/trigger/pubsub"),
            (Some(""), "/apps/a/trigger/pubsub"),
            (Some("/"), "/apps/a/trigger/pubsub"),
        ] {
            let mut l = pubsub::new();
            let args = prefix
                .map(|p| vec![format!("-path_prefix={p}")])
                .unwrap_or_default();
            l.parse(args).unwrap();
            let (mut config, _) = config_serving("a");
            let mut router = WebRouter::new(true);
            l.setup_subrouters(&mut router, &mut config).unwrap();
            // Bad JSON reaches the controller: Go's 400.
            assert_eq!(
                status_and_location(&router, post(path)).await,
                (StatusCode::BAD_REQUEST, String::new()),
                "{prefix:?}: POST {path}"
            );
            assert_eq!(
                status_and_location(&router, post(&format!("{path}/"))).await,
                (StatusCode::MOVED_PERMANENTLY, path.to_owned()),
                "{prefix:?}: POST {path}/"
            );
            let get = http::Request::builder()
                .uri(path)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                status_and_location(&router, get).await.0,
                StatusCode::METHOD_NOT_ALLOWED,
                "{prefix:?}: GET {path}"
            );
            // A path the prefix matches only as a string (gorilla's `PathPrefix` is not
            // segment-aware) is not the route.
            if !path.starts_with("/apps") {
                let other = path.replacen("/apps", "x/apps", 1);
                assert_eq!(
                    status_and_location(&router, post(&other)).await.0,
                    StatusCode::NOT_FOUND,
                    "{prefix:?}: POST {other}"
                );
            }
            // A later catch-all gets the other methods (Go's API mount behind the triggers).
            router.handle_prefix(
                &[],
                "/",
                gadk_server::mux::handler(|_req: Request| async {
                    let mut resp = Response::new(Body::empty());
                    *resp.status_mut() = StatusCode::IM_A_TEAPOT;
                    resp
                }),
            );
            let get = http::Request::builder()
                .uri(path)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                status_and_location(&router, get).await.0,
                StatusCode::IM_A_TEAPOT,
                "{prefix:?}: GET {path} after a catch-all"
            );
        }
    }

    // ---- Task 2's wire fixtures (SCENARIO.md) for the trigger exchanges ----

    fn fixture(name: &str) -> Value {
        let path = format!("{}/testdata/wire/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    /// SCENARIO.md N1 (fixed clock), N2 and N3 on log text.
    fn normalize_log(text: &str, port: u16) -> Vec<String> {
        text.lines()
            .map(|line| {
                let line = line.replacen(TS, "<ts> ", 1);
                let line = normalize_port(&line, port);
                // N3: `<ts> METHOD URI elapsed`.
                let fields: Vec<&str> = line.split(' ').collect();
                if fields.len() == 4
                    && fields[0] == "<ts>"
                    && !fields[1].is_empty()
                    && fields[1].bytes().all(|b| b.is_ascii_uppercase())
                    && fields[3].ends_with('s')
                    && fields[3].starts_with(|c: char| c.is_ascii_digit())
                {
                    format!("<ts> {} {} <elapsed>", fields[1], fields[2])
                } else {
                    line
                }
            })
            .collect()
    }

    /// SCENARIO.md N2.
    fn normalize_port(s: &str, port: u16) -> String {
        let mut s = s.to_owned();
        for host in ["127.0.0.1", "localhost", "port"] {
            s = s.replace(&format!("{host}:{port}"), &format!("{host}:<port>"));
        }
        s
    }

    /// The raw request of a fixture (SCENARIO.md "Request").
    fn raw_request(request: &Value, port: u16) -> Vec<u8> {
        let mut raw = format!(
            "{} {} HTTP/1.1\r\n",
            request["method"].as_str().unwrap(),
            request["target"].as_str().unwrap()
        );
        for h in request["headers"].as_array().unwrap() {
            let value = h[1].as_str().unwrap().replace("<port>", &port.to_string());
            raw.push_str(&format!("{}: {value}\r\n", h[0].as_str().unwrap()));
        }
        raw.push_str("\r\n");
        raw.push_str(request["body"].as_str().unwrap());
        raw.into_bytes()
    }

    /// A response read to EOF in the fixture's `response` shape (N2, N7; header names lowercased
    /// and sorted, as the fixtures' are compared case-insensitively). The trigger responses are
    /// never chunked.
    fn parse_response(raw: &[u8], port: u16) -> Value {
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8(raw[..split].to_vec()).unwrap();
        let body = String::from_utf8(raw[split + 4..].to_vec()).unwrap();
        let mut lines = head.split("\r\n");
        let (_, rest) = lines.next().unwrap().split_once(' ').unwrap();
        let (status, status_text) = rest.split_once(' ').unwrap();
        let headers = lines
            .map(|l| {
                let (k, v) = l.split_once(':').unwrap();
                let v = if k.eq_ignore_ascii_case("date") {
                    "<date>".to_owned()
                } else {
                    normalize_port(v.trim(), port)
                };
                (k.to_owned(), v)
            })
            .collect();
        response_value(
            status.parse().unwrap(),
            status_text,
            headers,
            &normalize_port(&body, port),
            body.len(),
        )
    }

    fn response_value(
        status: u64,
        status_text: &str,
        headers: Vec<(String, String)>,
        body: &str,
        body_length: usize,
    ) -> Value {
        let mut headers: Vec<(String, String)> = headers
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
        headers.sort();
        json!({
            "status": status,
            "status_text": status_text,
            "headers": headers,
            "body": body,
            "body_length": body_length,
        })
    }

    /// The fixture's response in the shape [`parse_response`] produces.
    fn expected_response(fixture: &Value) -> Value {
        let r = &fixture["response"];
        let headers = r["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h[0].as_str().unwrap().to_owned(),
                    h[1].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        response_value(
            r["status"].as_u64().unwrap(),
            r["status_text"].as_str().unwrap(),
            headers,
            r["body"].as_str().unwrap(),
            usize::try_from(r["body_length"].as_u64().unwrap()).unwrap(),
        )
    }

    fn log_lines(fixture: &Value) -> Vec<String> {
        fixture["log"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_owned())
            .collect()
    }

    /// Is `line` (a fixture log line) one of the Google OIDC provider's own records? gadk-server
    /// writes those through `tracing`, not the launcher's Go log (Task 12 replay).
    fn is_oidc_provider_line(line: &str) -> bool {
        line.starts_with("<ts> adk: authn: ")
    }

    /// Is `line` (a startup log line) the user message of a sub-launcher these tests leave out?
    fn is_absent_user_message(line: &str) -> bool {
        ["webui:", "a2a:", "api:"]
            .iter()
            .any(|k| line.starts_with(&format!("<ts>        {k}")))
    }

    /// Starts the web launcher with only the two trigger sub-launchers on the fixture server's
    /// arguments (the other sub-launchers' keywords dropped), replays `exchanges` byte for byte
    /// after the SCENARIO.md normalizations, and checks the startup and shutdown logs.
    async fn replay(server: &str, exchanges: &[&str]) {
        let startup = fixture(&format!("{server}_00_startup"));
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let args: Vec<String> = startup["args"].as_array().unwrap()[1..]
            .iter()
            .map(|a| a.as_str().unwrap().replace("<port>", &port.to_string()))
            .filter(|a| !["api", "webui", "a2a"].contains(&a.as_str()))
            .collect();
        let mut l = webl::new(vec![Box::new(pubsub::new()), Box::new(eventarc::new())]);
        assert_eq!(
            SubLauncher::parse(&mut l, args).unwrap(),
            Vec::<String>::new()
        );
        let l = Arc::new(l);
        // The probe's agent is `assistant`; what it answers never reaches a trigger response.
        let (config, out) = config_serving("assistant");
        let cancel = CancellationToken::new();
        let running = {
            let (l, cancel) = (Arc::clone(&l), cancel.clone());
            tokio::spawn(async move { l.run_with(cancel, config, |_| true).await })
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            assert!(Instant::now() < deadline, "the server did not start");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let want: Vec<String> = log_lines(&startup)
            .into_iter()
            .filter(|l| !is_absent_user_message(l))
            .collect();
        assert_eq!(
            normalize_log(&out.contents(), port),
            want,
            "{server}: startup log"
        );

        for name in exchanges {
            let f = fixture(name);
            let logged = out.contents().len();
            let mut conn = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            conn.write_all(&raw_request(&f["request"], port))
                .await
                .unwrap();
            let mut raw = Vec::new();
            conn.read_to_end(&mut raw).await.unwrap();
            assert_eq!(
                parse_response(&raw, port),
                expected_response(&f),
                "{name}: response"
            );
            let want: Vec<String> = log_lines(&f)
                .into_iter()
                .filter(|l| !is_oidc_provider_line(l))
                .collect();
            assert_eq!(
                normalize_log(&out.contents()[logged..], port),
                want,
                "{name}: log"
            );
        }

        let logged = out.contents().len();
        cancel.cancel();
        running.await.unwrap().unwrap();
        assert_eq!(
            normalize_log(&out.contents()[logged..], port),
            log_lines(&fixture(&format!("{server}_99_shutdown"))),
            "{server}: shutdown log"
        );
    }

    /// The `oidc` server whole: pubsub behind OIDC (401 without a token and with a malformed
    /// one), eventarc open, and the startup log naming only the open endpoint.
    #[tokio::test]
    async fn the_oidc_server_matches_go_wire_fixtures() {
        replay(
            "oidc",
            &[
                "oidc_01_pubsub_no_token",
                "oidc_02_pubsub_malformed_token",
                "oidc_03_eventarc_unauthenticated",
            ],
        )
        .await;
    }

    /// The `full` server's trigger exchanges that need no other sub-launcher. Left to Task 12:
    /// the GET fall-through to the API mount (`full_49`, needs `api`), the bad-JSON body
    /// (`full_47`: serde's decode text after Go's prefix, the 5a ledger's declared divergence)
    /// and the body over `-max_request_body_size` (`full_48`: gadk-server's trigger `read_body`
    /// recognizes only axum's length-limit error, not the launcher middleware's
    /// `MaxBytesError`).
    #[tokio::test]
    async fn the_full_server_trigger_exchanges_match_go_wire_fixtures() {
        replay(
            "full",
            &[
                "full_46_pubsub_trigger",
                "full_50_eventarc_trigger_structured",
                "full_51_eventarc_trigger_binary",
            ],
        )
        .await;
    }
}
