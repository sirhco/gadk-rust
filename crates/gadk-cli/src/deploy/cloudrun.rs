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

//! `gadk deploy cloudrun`.
//!
//! The flag structs keep ADK's shape (the nested flag groups, field for field in ADK's order) so
//! [`CloudRunFlags::go_v`] can print the one-line flags dump ADK's tool prints; gadk's
//! additions (`bin`, `target`) sit after those fields and are never printed.
//!
//! gadk builds the image remotely from the staged Cargo workspace (the `"Staging source"` step
//! takes the slot of ADK's `"Compiling server"`) with the two-stage Dockerfile of
//! [`crate::stage::dockerfile`]; the checks, the `CMD` array, the gcloud invocations and the
//! console output follow ADK's.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use clap::{ArgMatches, Command};
use gadk_platform::{GoDuration, go_path_join, go_quote};

use crate::Env;
use crate::flags::{
    bool_flag, bool_value, duration_flag, duration_value, int_flag, int_value, passed,
    pflag_command, stray_args, string_flag, string_value,
};
use crate::stage::{BuilderStage, SourceTree, dockerfile, make_temp};
use crate::target::{
    COMPILED_RUSTC_VERSION, CargoTarget, builder_image, builder_rust_version,
    check_path_dependencies, resolve,
};
use crate::util::{
    Printer, center_string, log_command, log_start_stop, validate_dockerfile_safe,
    validate_shell_arg_safe,
};

/// The command's short description.
pub const SHORT: &str = "Deploys the application to cloudrun.";
/// The command's long description (tabs and trailing spaces preserved).
// The first sentence says the image is built from the staged Cargo workspace.
pub const LONG: &str = "Deployment prepares a Dockerfile which builds the server executable from the staged Cargo workspace.\n\tService on Cloudrun is created using this information. \n\tLocal proxy adding authentication is started. \n\t";

/// The four OIDC flags whose passed state is recorded.
pub const TRIGGER_OIDC_FLAG_NAMES: [&str; 4] = [
    "pubsub_oidc_audience",
    "pubsub_oidc_service_accounts",
    "eventarc_oidc_audience",
    "eventarc_oidc_service_accounts",
];

/// The gcloud flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GCloudFlags {
    pub region: String,
    pub project_name: String,
}

/// The trigger configuration flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriggerConfigFlags {
    pub max_retries: i64,
    pub base_delay: GoDuration,
    pub max_delay: GoDuration,
    pub max_runs: i64,
    pub oidc_audience: String,
    pub oidc_service_accounts: String,
    /// Whether each OIDC flag was on the command line, which an empty value
    /// alone cannot tell from it being omitted.
    pub oidc_audience_passed: bool,
    pub oidc_service_accounts_passed: bool,
}

/// The Cloud Run service flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudRunServiceFlags {
    pub service_name: String,
    pub server_port: i64,
    pub a2a_agent_card_url: String,
    pub a2a: bool,
    pub api: bool,
    pub debug_api: bool,
    pub webui: bool,
    pub pubsub: bool,
    pub pubsub_trigger: TriggerConfigFlags,
    pub eventarc: bool,
    pub eventarc_trigger: TriggerConfigFlags,
}

/// The local proxy flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalProxyFlags {
    pub port: i64,
}

/// The build flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildFlags {
    pub temp_dir: String,
    pub exec_path: String,
    pub exec_file: String,
    pub dockerfile_build_path: String,
}

/// The source flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFlags {
    pub src_base_path: String,
    pub entry_point_path: String,
}

/// The flags of `gadk deploy cloudrun`, plus gadk's `--bin` and the resolved Cargo target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudRunFlags {
    pub gcloud: GCloudFlags,
    pub cloud_run: CloudRunServiceFlags,
    pub proxy: LocalProxyFlags,
    pub build: BuildFlags,
    pub source: SourceFlags,
    /// gadk's `--bin` (not printed).
    pub bin: String,
    /// The Cargo target `compute_flags` resolved (not printed).
    pub target: Option<CargoTarget>,
}

/// The `cloudrun` command with its flags, in ADK's order, plus gadk's `--bin`.
pub fn command() -> Command {
    pflag_command("cloudrun")
        .about(SHORT)
        .long_about(LONG)
        .arg(string_flag("region", Some('r'), "", "GCP Region"))
        .arg(string_flag("project_name", Some('p'), "", "GCP Project Name"))
        .arg(string_flag("service_name", Some('s'), "", "Cloud Run Service name"))
        .arg(string_flag(
            "temp_dir",
            Some('t'),
            "",
            "Temp dir for build, defaults to os.TempDir() if not specified",
        ))
        .arg(int_flag("proxy_port", None, "8081", "Local proxy port"))
        .arg(int_flag("server_port", None, "8080", "Cloudrun server port"))
        // `-e` names a Cargo package.
        .arg(string_flag(
            "entry_point_path",
            Some('e'),
            "",
            "Path to the Cargo package of the entry point (its directory or Cargo.toml), defaults to the current directory",
        ))
        .arg(bool_flag("a2a", true, "Enable A2A"))
        .arg(string_flag(
            "a2a_agent_url",
            Some('a'),
            "http://127.0.0.1:8081",
            "A2A agent card URL as advertised in the public agent card",
        ))
        .arg(bool_flag("api", true, "Enable API"))
        .arg(bool_flag(
            "debug_api",
            false,
            "Enable the Debug API - requires '--api'. Exposes tool-call arguments, responses and tool names. The web UI's Traces and agent structure panels need it.",
        ))
        .arg(bool_flag("webui", true, "Enable Web UI"))
        .arg(bool_flag("pubsub", false, "Enable PubSub subrouter"))
        .arg(int_flag(
            "pubsub_max_retries",
            None,
            "3",
            "Maximum retries for HTTP 429 errors from PubSub triggers",
        ))
        .arg(duration_flag(
            "pubsub_base_delay",
            None,
            "1s",
            "Base delay for PubSub trigger retry exponential backoff",
        ))
        .arg(duration_flag(
            "pubsub_max_delay",
            None,
            "10s",
            "Maximum delay for PubSub trigger retry exponential backoff",
        ))
        .arg(int_flag(
            "pubsub_max_concurrent_runs",
            None,
            "100",
            "Maximum concurrent PubSub trigger runs",
        ))
        .arg(string_flag(
            "pubsub_oidc_audience",
            None,
            "",
            "Audience of the Google-signed OIDC token the PubSub push subscription attaches. Requires --pubsub and --pubsub_oidc_service_accounts. When neither OIDC flag is given, the PubSub endpoint is unauthenticated. An empty value is refused",
        ))
        .arg(string_flag(
            "pubsub_oidc_service_accounts",
            None,
            "",
            "Comma-separated service account emails allowed to call the PubSub endpoint. Requires --pubsub and --pubsub_oidc_audience. An empty value is refused",
        ))
        .arg(bool_flag("eventarc", false, "Enable Eventarc subrouter"))
        .arg(int_flag(
            "eventarc_max_retries",
            None,
            "3",
            "Maximum retries for HTTP 429 errors from Eventarc triggers",
        ))
        .arg(duration_flag(
            "eventarc_base_delay",
            None,
            "1s",
            "Base delay for Eventarc trigger retry exponential backoff",
        ))
        .arg(duration_flag(
            "eventarc_max_delay",
            None,
            "10s",
            "Maximum delay for Eventarc trigger retry exponential backoff",
        ))
        .arg(int_flag(
            "eventarc_max_concurrent_runs",
            None,
            "100",
            "Maximum concurrent Eventarc trigger runs",
        ))
        .arg(string_flag(
            "eventarc_oidc_audience",
            None,
            "",
            "Audience of the Google-signed OIDC token the Eventarc trigger attaches. Requires --eventarc and --eventarc_oidc_service_accounts. When neither OIDC flag is given, the Eventarc endpoint is unauthenticated. An empty value is refused",
        ))
        .arg(string_flag(
            "eventarc_oidc_service_accounts",
            None,
            "",
            "Comma-separated service account emails allowed to call the Eventarc endpoint. Requires --eventarc and --eventarc_oidc_audience. An empty value is refused",
        ))
        // A gadk-only flag selecting the Cargo binary target.
        .arg(string_flag(
            "bin",
            None,
            "",
            "Binary target of the package to deploy, required when the package has more than one",
        ))
        .arg(stray_args())
}

/// The flags of a parsed `cloudrun` command, with the passed OIDC flags recorded
/// (`note_passed_oidc_flags` from [`crate::flags::passed`]).
pub fn from_matches(m: &ArgMatches) -> CloudRunFlags {
    let trigger = |name: &str| TriggerConfigFlags {
        max_retries: int_value(m, &format!("{name}_max_retries")),
        base_delay: duration_value(m, &format!("{name}_base_delay")),
        max_delay: duration_value(m, &format!("{name}_max_delay")),
        max_runs: int_value(m, &format!("{name}_max_concurrent_runs")),
        oidc_audience: string_value(m, &format!("{name}_oidc_audience")),
        oidc_service_accounts: string_value(m, &format!("{name}_oidc_service_accounts")),
        oidc_audience_passed: false,
        oidc_service_accounts_passed: false,
    };
    let mut f = CloudRunFlags {
        gcloud: GCloudFlags {
            region: string_value(m, "region"),
            project_name: string_value(m, "project_name"),
        },
        cloud_run: CloudRunServiceFlags {
            service_name: string_value(m, "service_name"),
            server_port: int_value(m, "server_port"),
            a2a_agent_card_url: string_value(m, "a2a_agent_url"),
            a2a: bool_value(m, "a2a"),
            api: bool_value(m, "api"),
            debug_api: bool_value(m, "debug_api"),
            webui: bool_value(m, "webui"),
            pubsub: bool_value(m, "pubsub"),
            pubsub_trigger: trigger("pubsub"),
            eventarc: bool_value(m, "eventarc"),
            eventarc_trigger: trigger("eventarc"),
        },
        proxy: LocalProxyFlags {
            port: int_value(m, "proxy_port"),
        },
        build: BuildFlags {
            temp_dir: string_value(m, "temp_dir"),
            ..BuildFlags::default()
        },
        source: SourceFlags {
            entry_point_path: string_value(m, "entry_point_path"),
            ..SourceFlags::default()
        },
        bin: string_value(m, "bin"),
        target: None,
    };
    // Record which OIDC flags were passed.
    f.note_passed_oidc_flags(&|id| passed(m, id));
    f
}

/// The trigger flags' one-line form: durations as their nanosecond count.
fn trigger_go_v(t: &TriggerConfigFlags) -> String {
    format!(
        "{{{} {} {} {} {} {} {} {}}}",
        t.max_retries,
        t.base_delay.0,
        t.max_delay.0,
        t.max_runs,
        t.oidc_audience,
        t.oidc_service_accounts,
        t.oidc_audience_passed,
        t.oidc_service_accounts_passed
    )
}

impl CloudRunFlags {
    /// The one-line form of the flags (without the newline), printed before deploying:
    /// every flag group in braces in field order, values space-separated (durations as
    /// nanoseconds, derived fields as they are now), byte-identical to the line ADK's tool
    /// prints; `bin` and `target` are not printed.
    pub fn go_v(&self) -> String {
        let (g, c, b, s) = (&self.gcloud, &self.cloud_run, &self.build, &self.source);
        format!(
            "{{{{{} {}}} {{{} {} {} {} {} {} {} {} {} {} {}}} {{{}}} {{{} {} {} {}}} {{{} {}}}}}",
            g.region,
            g.project_name,
            c.service_name,
            c.server_port,
            c.a2a_agent_card_url,
            c.a2a,
            c.api,
            c.debug_api,
            c.webui,
            c.pubsub,
            trigger_go_v(&c.pubsub_trigger),
            c.eventarc,
            trigger_go_v(&c.eventarc_trigger),
            self.proxy.port,
            b.temp_dir,
            b.exec_path,
            b.exec_file,
            b.dockerfile_build_path,
            s.src_base_path,
            s.entry_point_path
        )
    }

    /// Records which OIDC flags were passed on the command line.
    pub fn note_passed_oidc_flags(&mut self, passed: &dyn Fn(&str) -> bool) {
        self.cloud_run.pubsub_trigger.oidc_audience_passed = passed("pubsub_oidc_audience");
        self.cloud_run.pubsub_trigger.oidc_service_accounts_passed =
            passed("pubsub_oidc_service_accounts");
        self.cloud_run.eventarc_trigger.oidc_audience_passed = passed("eventarc_oidc_audience");
        self.cloud_run.eventarc_trigger.oidc_service_accounts_passed =
            passed("eventarc_oidc_service_accounts");
    }

    /// Computes the derived flags: the checks in order, target resolution, the shell-safe
    /// checks, then the `cloudrun_<local time>__*` temp dir and the derived paths.
    pub async fn compute_flags(&mut self, env: &Env) -> Result<(), String> {
        log_start_stop(
            &env.out,
            "Computing flags & preparing temp",
            async |p: Printer| {
                if self.cloud_run.debug_api && !self.cloud_run.api {
                    return Err("cannot enable Debug API without having enabled API".to_string());
                }

                validate_service_name(&self.cloud_run.service_name)?;

                // Checked before the temp dir is created so a rejected value does not leave
                // one behind. It is checked whether or not --a2a is set.
                validate_dockerfile_safe(&self.cloud_run.a2a_agent_card_url, "--a2a_agent_url")?;
                validate_trigger_oidc("pubsub", &self.cloud_run.pubsub_trigger)?;
                validate_trigger_oidc("eventarc", &self.cloud_run.eventarc_trigger)?;
                check_trigger_oidc_emitted(
                    "pubsub",
                    self.cloud_run.pubsub,
                    &self.cloud_run.pubsub_trigger,
                )?;
                check_trigger_oidc_emitted(
                    "eventarc",
                    self.cloud_run.eventarc,
                    &self.cloud_run.eventarc_trigger,
                )?;

                // The raw flag value, kept for the rejection messages below: what the checks
                // see is derived from it.
                let orig_entry_point_path = self.source.entry_point_path.clone();

                // Resolves the Cargo target (package, binary, workspace root; its errors are
                // gadk's) and rejects a `path` dependency outside the staged workspace root,
                // all before the temp dir is created.
                let target = resolve(env, &orig_entry_point_path, &self.bin).await?;
                check_path_dependencies(&target, &target.workspace_root)?;
                self.source.src_base_path = target.workspace_root.to_string_lossy().into_owned();
                self.source.entry_point_path = target.manifest_dir.to_string_lossy().into_owned();
                if self.build.exec_path.is_empty() {
                    self.build.exec_file = target.bin.clone();
                }

                // execFile becomes both COPY operands and the CMD argv[0], and COPY splits
                // its operands on whitespace: it takes the allowlist.
                validate_shell_arg_safe(
                    &self.build.exec_file,
                    &format!(
                        "executable name derived from --entry_point_path {}:",
                        go_quote(&orig_entry_point_path)
                    ),
                )?;
                // The package name reaches the builder's `RUN cargo build -p <package>` line,
                // so it takes the same allowlist.
                validate_shell_arg_safe(
                    &target.package,
                    &format!(
                        "package name derived from --entry_point_path {}:",
                        go_quote(&orig_entry_point_path)
                    ),
                )?;
                self.target = Some(target);

                if self.build.temp_dir.is_empty() {
                    self.build.temp_dir = env.temp_dir().to_string_lossy().into_owned();
                }
                let absp = env.abs(&self.build.temp_dir);
                // The wrapped error is Rust's `io::Error` text.
                let dir = make_temp(&absp, "cloudrun_", env.time.now()).map_err(|e| {
                    format!(
                        "cannot create a temporary sub directory in '{}': {e}",
                        absp.display()
                    )
                })?;
                self.build.temp_dir = dir.to_string_lossy().into_owned();
                p.print(&[&"Using temp dir:", &self.build.temp_dir]);

                if self.build.exec_path.is_empty() {
                    self.build.exec_path =
                        go_path_join(&[self.build.temp_dir.as_str(), &self.build.exec_file]);
                }
                self.build.dockerfile_build_path =
                    go_path_join(&[self.build.temp_dir.as_str(), "Dockerfile"]);

                Ok(())
            },
        )
        .await
    }

    /// `"Staging source"`: copies the workspace into
    /// the temp dir and writes a `.gcloudignore` when the copy has none.
    // The Cargo workspace root is copied (without `.git`, `target` and a nested temp dir)
    // into the temp dir for a remote build, and a one-comment `.gcloudignore` is written when
    // the workspace has none, so `gcloud` does not fall back to `.gitignore` (which may exclude
    // `Cargo.lock`). Its error texts are gadk's.
    pub async fn stage_source(&mut self, env: &Env) -> Result<(), String> {
        log_start_stop(&env.out, "Staging source", async |p: Printer| {
            let Some(target) = &self.target else {
                return Err(NO_TARGET.to_string());
            };
            let root = &target.workspace_root;
            let temp = PathBuf::from(&self.build.temp_dir);
            p.print(&[&"Using", &self.source.entry_point_path, &"as entry point"]);
            p.print(&[&"Copying", &root.display(), &"to", &self.build.temp_dir]);
            let staged = SourceTree::walk(root, &nested_exclude(root, &temp))
                .and_then(|tree| tree.copy_into(&temp))
                .map_err(|e| {
                    format!(
                        "cannot copy '{}' to '{}': {e}",
                        root.display(),
                        temp.display()
                    )
                });
            staged?;
            let ignore = temp.join(".gcloudignore");
            if fs::symlink_metadata(&ignore).is_err() {
                p.print(&[&"Writing:", &ignore.display()]);
                fs::write(&ignore, GCLOUDIGNORE)
                    .map_err(|e| format!("cannot write '{}': {e}", ignore.display()))?;
            }
            Ok(())
        })
        .await
    }

    /// Prepares the Dockerfile: reads only the receiver; the generated `Dockerfile`
    /// replaces any copied root `Dockerfile` (removed first, so a symbolic link is never
    /// followed out of the temp dir).
    pub async fn prepare_dockerfile(&self, env: &Env) -> Result<(), String> {
        log_start_stop(&env.out, "Preparing Dockerfile", async |p: Printer| {
            p.print(&[&"Writing:", &self.build.dockerfile_build_path]);
            let Some(target) = &self.target else {
                return Err(NO_TARGET.to_string());
            };

            // Every value below is read off the receiver.
            let c = &self.cloud_run;
            let mut b = String::new();
            if c.api {
                let _ = write!(
                    b,
                    r#", "api", "-webui_address", "127.0.0.1:{}""#,
                    self.proxy.port
                );
                if c.debug_api {
                    b.push_str(r#", "-include_debug_api""#);
                }
            }
            if c.a2a {
                let _ = write!(
                    b,
                    r#", "a2a", "--a2a_agent_url", "{}""#,
                    c.a2a_agent_card_url
                );
            }
            if c.webui {
                let _ = write!(
                    b,
                    r#", "webui", "--api_server_address", "http://127.0.0.1:{}/api""#,
                    self.proxy.port
                );
            }
            if c.pubsub {
                b.push_str(r#", "pubsub""#);
                write_trigger_flags(&mut b, &c.pubsub_trigger);
                write_oidc_flags(&mut b, &c.pubsub_trigger);
            }
            if c.eventarc {
                b.push_str(r#", "eventarc""#);
                write_trigger_flags(&mut b, &c.eventarc_trigger);
                write_oidc_flags(&mut b, &c.eventarc_trigger);
            }
            let builder = BuilderStage {
                image: builder_image(&builder_rust_version(target, COMPILED_RUSTC_VERSION)),
                workdir: String::new(),
                package: target.package.clone(),
                bin: self.build.exec_file.clone(),
            };
            let content = dockerfile(&builder, c.server_port, &b);
            // The error is Rust's `io::Error` text after an `open <path>: ` prefix.
            write_file_0600(Path::new(&self.build.dockerfile_build_path), &content)
                .map_err(|e| format!("open {}: {e}", self.build.dockerfile_build_path))
        })
        .await
    }

    /// Deploys to Cloud Run with `gcloud run deploy`.
    pub async fn gcloud_deploy_to_cloud_run(&self, env: &Env) -> Result<(), String> {
        log_start_stop(&env.out, "Deploying to Cloud Run", async |p: Printer| {
            let params = [
                "run",
                "deploy",
                self.cloud_run.service_name.as_str(),
                "--source",
                ".",
                "--set-secrets=GOOGLE_API_KEY=GOOGLE_API_KEY:latest",
                "--region",
                self.gcloud.region.as_str(),
                "--project",
                self.gcloud.project_name.as_str(),
                "--ingress",
                "all",
                "--no-allow-unauthenticated",
            ];
            let cmd = env
                .command("gcloud", &params)
                .with_dir(self.build.temp_dir.clone());
            log_command(env.runner.as_ref(), &cmd, &p).await
        })
        .await
    }

    /// Removes the temp dir.
    pub async fn clean_temp(&self, env: &Env) -> Result<(), String> {
        log_start_stop(&env.out, "Cleaning temp", async |p: Printer| {
            p.print(&[&"Clean temp starting with", &self.build.temp_dir]);
            // A missing directory is not an error.
            // The wrapped error is Rust's `io::Error` text.
            match fs::remove_dir_all(&self.build.temp_dir) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(format!(
                    "failed to clean temp directory {}: {e}",
                    self.build.temp_dir
                )),
                _ => Ok(()),
            }
        })
        .await
    }

    /// Runs the local proxy (banner and `gcloud run services proxy`).
    pub async fn run_gcloud_proxy(&self, env: &Env) -> Result<(), String> {
        log_start_stop(
            &env.out,
            "Running local gcloud authenticating proxy",
            async |p: Printer| {
                let target_width = 80;
                let port = self.proxy.port.to_string();

                p.print(&[&"-".repeat(target_width)]);
                p.print(&[&center_string("", target_width)]);
                p.print(&[&center_string(
                    &format!("Running ADK Web UI on http://127.0.0.1:{port}/ui/    <-- open this"),
                    target_width,
                )]);
                p.print(&[&center_string(
                    &format!("ADK REST API on http://127.0.0.1:{port}/api/         "),
                    target_width,
                )]);
                p.print(&[&center_string("", target_width)]);
                p.print(&[&center_string("Press Ctrl-C to stop", target_width)]);
                p.print(&[&center_string("", target_width)]);
                p.print(&[&"-".repeat(target_width)]);

                let cmd = env.command(
                    "gcloud",
                    &[
                        "run",
                        "services",
                        "proxy",
                        &self.cloud_run.service_name,
                        "--project",
                        &self.gcloud.project_name,
                        "--port",
                        &port,
                        "--region",
                        &self.gcloud.region,
                    ],
                );
                log_command(env.runner.as_ref(), &cmd, &p).await
            },
        )
        .await
    }

    /// Deploys: prints [`CloudRunFlags::go_v`], then compute → stage →
    /// Dockerfile → deploy → clean → proxy, stopping at the first error (the temp dir is kept).
    pub async fn deploy_on_cloud_run(&mut self, env: &Env) -> Result<(), String> {
        // Prints the receiver's flags.
        env.out.write_str(&format!("{}\n", self.go_v()));

        self.compute_flags(env).await?;
        self.stage_source(env).await?;
        self.prepare_dockerfile(env).await?;
        self.gcloud_deploy_to_cloud_run(env).await?;
        self.clean_temp(env).await?;
        self.run_gcloud_proxy(env).await?;

        Ok(())
    }
}

/// The error of a step that needs the Cargo target before `compute_flags` resolved it (no
/// command-line path reaches it).
const NO_TARGET: &str = "no Cargo target: computeFlags has not run";

/// The `.gcloudignore` written when the staged workspace has none.
const GCLOUDIGNORE: &str =
    "# Written by gadk deploy cloudrun: without a .gcloudignore, gcloud would apply .gitignore.\n";

/// The paths the staging walk of `root` skips: the temp dir when it lies inside `root`, in
/// the same form as `root` (the walk compares `root`-joined paths as given), also when one of
/// the two is reached through a symbolic link.
fn nested_exclude(root: &Path, temp: &Path) -> Vec<PathBuf> {
    let mut exclude = vec![temp.to_path_buf()];
    if let (Ok(r), Ok(t)) = (fs::canonicalize(root), fs::canonicalize(temp))
        && let Ok(rel) = t.strip_prefix(&r)
    {
        exclude.push(root.join(rel));
    }
    exclude
}

/// Writes `data` to `path` with mode 0600 after removing any existing entry at `path`.
fn write_file_0600(path: &Path, data: &str) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => fs::remove_file(path)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data.as_bytes())
}

/// The four trigger lines of the generated Dockerfile `CMD`.
fn write_trigger_flags(b: &mut String, t: &TriggerConfigFlags) {
    let _ = write!(b, r#", "--trigger_max_retries", "{}""#, t.max_retries);
    let _ = write!(b, r#", "--trigger_base_delay", "{}""#, t.base_delay);
    let _ = write!(b, r#", "--trigger_max_delay", "{}""#, t.max_delay);
    let _ = write!(b, r#", "--trigger_max_concurrent_runs", "{}""#, t.max_runs);
}

/// Rejects a `--service_name` gcloud would parse as a flag.
pub fn validate_service_name(name: &str) -> Result<(), String> {
    if name.starts_with('-') {
        return Err(format!(
            "invalid --service_name {}: a Cloud Run service name must not begin with '-'",
            go_quote(name)
        ));
    }
    Ok(())
}

/// Rejects the trigger's OIDC values the Dockerfile cannot
/// carry or the trigger's sublauncher would refuse at startup.
pub fn validate_trigger_oidc(name: &str, t: &TriggerConfigFlags) -> Result<(), String> {
    let aud_flag = format!("--{name}_oidc_audience");
    let sa_flag = format!("--{name}_oidc_service_accounts");
    validate_dockerfile_safe(&t.oidc_audience, &aud_flag)?;
    validate_dockerfile_safe(&t.oidc_service_accounts, &sa_flag)?;
    // Before the pair check, so a deploy script with one variable unset is told which value
    // is empty rather than to pass both flags.
    if t.oidc_audience_passed && t.oidc_audience.is_empty() {
        return Err(format!(
            "{aud_flag} is empty: give it a value or leave it out"
        ));
    }
    if t.oidc_service_accounts_passed && t.oidc_service_accounts.is_empty() {
        return Err(format!(
            "{sa_flag} is empty: give it a value or leave it out"
        ));
    }
    check_oidc_values_are_not_flags(
        &aud_flag,
        &sa_flag,
        &t.oidc_audience,
        &t.oidc_service_accounts,
    )?;
    if t.oidc_audience.is_empty() != t.oidc_service_accounts.is_empty() {
        return Err(format!("{aud_flag} and {sa_flag} must be set together"));
    }
    if t.oidc_audience != t.oidc_audience.trim() {
        return Err(format!(
            "{aud_flag} {} has surrounding whitespace",
            go_quote(&t.oidc_audience)
        ));
    }
    if !t.oidc_service_accounts.is_empty()
        && t.oidc_service_accounts
            .split(',')
            .any(|account| account.trim().is_empty())
    {
        return Err(format!(
            "{sa_flag} {} has an empty entry",
            go_quote(&t.oidc_service_accounts)
        ));
    }
    Ok(())
}

/// Rejects an audience or service account starting with
/// `-`, as when an unset variable makes a flag take the next flag as its value.
pub fn check_oidc_values_are_not_flags(
    aud_flag: &str,
    sa_flag: &str,
    audience: &str,
    accounts: &str,
) -> Result<(), String> {
    if audience.starts_with('-') {
        return Err(format!(
            "{aud_flag} {} starts with \"-\", as when an unset variable makes a flag take the next flag as its value",
            go_quote(audience)
        ));
    }
    if accounts.trim().starts_with('-') {
        return Err(format!(
            "{sa_flag} {} starts with \"-\", as when an unset variable makes a flag take the next flag as its value",
            go_quote(accounts)
        ));
    }
    if accounts
        .split(',')
        .any(|account| account.trim().starts_with('-'))
    {
        return Err(format!(
            "{sa_flag} {} has an entry starting with \"-\"",
            go_quote(accounts)
        ));
    }
    Ok(())
}

/// Rejects OIDC values for a trigger that is not enabled (they
/// are written only after that trigger's keyword).
pub fn check_trigger_oidc_emitted(
    name: &str,
    enabled: bool,
    t: &TriggerConfigFlags,
) -> Result<(), String> {
    if !enabled && (!t.oidc_audience.is_empty() || !t.oidc_service_accounts.is_empty()) {
        return Err(format!(
            "--{name}_oidc_audience and --{name}_oidc_service_accounts require --{name}"
        ));
    }
    Ok(())
}

/// Writes the trigger's OIDC flags when set (`compute_flags` has checked that
/// both or neither are).
pub fn write_oidc_flags(b: &mut String, t: &TriggerConfigFlags) {
    if !t.oidc_audience.is_empty() {
        let _ = write!(b, r#", "-oidc_audience", "{}""#, t.oidc_audience);
    }
    if !t.oidc_service_accounts.is_empty() {
        let _ = write!(
            b,
            r#", "-oidc_service_accounts", "{}""#,
            t.oidc_service_accounts
        );
    }
}

/// Test fixtures shared by this module's tests: a Cargo
/// workspace on disk with a [`TestEnv`](crate::testutil::TestEnv) whose runner answers
/// `cargo metadata` for it.
#[cfg(test)]
pub(crate) mod fixture {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{CloudRunFlags, CloudRunServiceFlags};
    use crate::runner::Cmd;
    use crate::target::CargoTarget;
    use crate::testutil::{
        Canned, TestEnv, cargo_metadata_json, cargo_metadata_runner, write_cargo_workspace,
    };

    /// The `rust-toolchain.toml` channel every fixture workspace pins.
    pub(crate) const CHANNEL: &str = "1.90.0";

    /// `<dir>/ws`: a Cargo workspace ([`write_cargo_workspace`] plus `rust-toolchain.toml`)
    /// whose package `package` lives at `<dir>/ws/<member>` with the binary targets `bins`;
    /// `<dir>/tmp` is the temp parent (Go `t.TempDir()`).
    pub(crate) struct Workspace {
        pub dir: tempfile::TempDir,
        pub member: String,
        pub package: String,
        pub bins: Vec<String>,
        /// `path` dependencies `cargo metadata` reports for the package.
        pub path_dependencies: Vec<PathBuf>,
    }

    impl Workspace {
        pub(crate) fn new(member: &str, package: &str, bins: &[&str]) -> Workspace {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("ws");
            fs::create_dir_all(dir.path().join("tmp")).unwrap();
            write_cargo_workspace(&root, member, package);
            fs::write(
                root.join("rust-toolchain.toml"),
                format!("[toolchain]\nchannel = \"{CHANNEL}\"\n"),
            )
            .unwrap();
            Workspace {
                dir,
                member: member.to_string(),
                package: package.to_string(),
                bins: bins.iter().map(|b| b.to_string()).collect(),
                path_dependencies: Vec::new(),
            }
        }

        /// The workspace root `<dir>/ws`.
        pub(crate) fn root(&self) -> PathBuf {
            self.dir.path().join("ws")
        }

        /// The temp parent `<dir>/tmp`.
        pub(crate) fn temp_parent(&self) -> PathBuf {
            self.dir.path().join("tmp")
        }

        /// The package's manifest directory.
        pub(crate) fn manifest_dir(&self) -> PathBuf {
            if self.member.is_empty() {
                self.root()
            } else {
                self.root().join(&self.member)
            }
        }

        /// What `cargo metadata --format-version 1 --no-deps` prints for the workspace.
        pub(crate) fn metadata(&self) -> String {
            let bins: Vec<&str> = self.bins.iter().map(String::as_str).collect();
            cargo_metadata_json(
                &self.root(),
                &self.manifest_dir().join("Cargo.toml"),
                &self.package,
                &bins,
                &self.path_dependencies,
            )
        }

        /// A [`TestEnv`] in the workspace root whose runner answers `cargo metadata` with
        /// [`Workspace::metadata`] and every other command with exit 0.
        pub(crate) fn env(&self) -> TestEnv {
            self.env_with(|_| Canned::success(Vec::new()))
        }

        /// As [`Workspace::env`], with every command other than `cargo metadata` answered by
        /// `other`.
        pub(crate) fn env_with(
            &self,
            other: impl Fn(&Cmd) -> Canned + Send + Sync + 'static,
        ) -> TestEnv {
            let metadata = self.metadata();
            let runner = cargo_metadata_runner(move |_| metadata.clone(), other);
            TestEnv::builder(self.root()).runner(runner).build()
        }

        /// Go `resetFlags(t, entryPointPath, a2aAgentCardURL)`: the zero value plus the
        /// entry point, the temp parent as `--temp_dir` and the A2A agent card URL.
        pub(crate) fn flags(&self, entry_point: &str, a2a_agent_card_url: &str) -> CloudRunFlags {
            let mut f = CloudRunFlags {
                cloud_run: CloudRunServiceFlags {
                    a2a_agent_card_url: a2a_agent_card_url.to_string(),
                    ..CloudRunServiceFlags::default()
                },
                ..CloudRunFlags::default()
            };
            f.source.entry_point_path = entry_point.to_string();
            f.build.temp_dir = self.temp_parent().to_string_lossy().into_owned();
            f
        }
    }

    /// A receiver `prepare_dockerfile` can run on directly (Go builds `&deployCloudRunFlags{}`
    /// by hand): `exec_file`, the Dockerfile at `<dir>/Dockerfile` and a resolved target
    /// (package `agent`) whose workspace root is `dir` (no `rust-toolchain.toml` unless the
    /// test writes one).
    pub(crate) fn receiver(dir: &Path, exec_file: &str) -> CloudRunFlags {
        let mut f = CloudRunFlags::default();
        f.build.exec_file = exec_file.to_string();
        f.build.dockerfile_build_path = dir.join("Dockerfile").to_string_lossy().into_owned();
        f.target = Some(CargoTarget {
            package: "agent".to_string(),
            bin: exec_file.to_string(),
            workspace_root: dir.to_path_buf(),
            manifest_dir: dir.to_path_buf(),
            rust_version: None,
            path_dependencies: Vec::new(),
        });
        f
    }

    /// The `CMD` line's JSON array of a generated Dockerfile (Go: the line after `CMD `,
    /// `json.Unmarshal` into `[]string`).
    pub(crate) fn cmd_args(content: &str) -> Vec<String> {
        let line = content
            .lines()
            .filter_map(|l| l.strip_prefix("CMD "))
            .next_back()
            .unwrap_or_else(|| {
                panic!("no CMD instruction in the generated Dockerfile:\n{content}")
            });
        serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("CMD {line} does not parse as a JSON array: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use chrono::Local;
    use pretty_assertions::assert_eq;

    use super::fixture::{CHANNEL, Workspace, receiver};
    use super::*;
    use crate::testutil::{Canned, TestEnv, default_now};

    fn parse(args: &[&str]) -> CloudRunFlags {
        let m = command()
            .try_get_matches_from(std::iter::once("cloudrun").chain(args.iter().copied()))
            .unwrap();
        from_matches(&m)
    }

    #[test]
    fn go_v_golden_line_with_defaults() {
        assert_eq!(
            parse(&["-e", "x"]).go_v(),
            "{{ } { 8080 http://127.0.0.1:8081 true true false true false {3 1000000000 10000000000 100   false false} false {3 1000000000 10000000000 100   false false}} {8081} {   } { x}}"
        );
    }

    #[test]
    fn from_matches_reads_every_flag_and_go_v_prints_it() {
        let f = parse(&[
            "-r",
            "us-east1",
            "-p",
            "proj",
            "-s",
            "svc",
            "-t",
            "/t",
            "--proxy_port",
            "9001",
            "--server_port=0x10",
            "-e",
            "agent",
            "--a2a=false",
            "-a",
            "https://a.example",
            "--api=f",
            "--debug_api",
            "--webui=0",
            "--pubsub",
            "--pubsub_max_retries",
            "5",
            "--pubsub_base_delay",
            "1.5s",
            "--pubsub_max_delay=1m",
            "--pubsub_max_concurrent_runs",
            "7",
            "--pubsub_oidc_audience",
            "aud",
            "--pubsub_oidc_service_accounts=sa@x",
            "--eventarc=true",
            "--eventarc_max_retries=6",
            "--eventarc_base_delay=2ms",
            "--eventarc_max_delay",
            "3h",
            "--eventarc_max_concurrent_runs=8",
            "--eventarc_oidc_audience=",
            "--bin",
            "tool",
            "stray",
        ]);
        let want = CloudRunFlags {
            gcloud: GCloudFlags {
                region: "us-east1".into(),
                project_name: "proj".into(),
            },
            cloud_run: CloudRunServiceFlags {
                service_name: "svc".into(),
                server_port: 16,
                a2a_agent_card_url: "https://a.example".into(),
                a2a: false,
                api: false,
                debug_api: true,
                webui: false,
                pubsub: true,
                pubsub_trigger: TriggerConfigFlags {
                    max_retries: 5,
                    base_delay: GoDuration(1_500_000_000),
                    max_delay: GoDuration::MINUTE,
                    max_runs: 7,
                    oidc_audience: "aud".into(),
                    oidc_service_accounts: "sa@x".into(),
                    oidc_audience_passed: true,
                    oidc_service_accounts_passed: true,
                },
                eventarc: true,
                eventarc_trigger: TriggerConfigFlags {
                    max_retries: 6,
                    base_delay: GoDuration(2 * GoDuration::MILLISECOND.0),
                    max_delay: GoDuration(3 * GoDuration::HOUR.0),
                    max_runs: 8,
                    oidc_audience: String::new(),
                    oidc_service_accounts: String::new(),
                    oidc_audience_passed: true,
                    oidc_service_accounts_passed: false,
                },
            },
            proxy: LocalProxyFlags { port: 9001 },
            build: BuildFlags {
                temp_dir: "/t".into(),
                ..BuildFlags::default()
            },
            source: SourceFlags {
                src_base_path: String::new(),
                entry_point_path: "agent".into(),
            },
            bin: "tool".into(),
            target: None,
        };
        assert_eq!(f, want);
        assert_eq!(
            f.go_v(),
            "{{us-east1 proj} {svc 16 https://a.example false false true false true {5 1500000000 60000000000 7 aud sa@x true true} true {6 2000000 10800000000000 8   true false}} {9001} {/t   } { agent}}"
        );
    }

    #[test]
    fn note_passed_oidc_flags_names_each_flag() {
        for (i, name) in TRIGGER_OIDC_FLAG_NAMES.iter().enumerate() {
            let mut f = CloudRunFlags::default();
            f.note_passed_oidc_flags(&|id| id == *name);
            let got = [
                f.cloud_run.pubsub_trigger.oidc_audience_passed,
                f.cloud_run.pubsub_trigger.oidc_service_accounts_passed,
                f.cloud_run.eventarc_trigger.oidc_audience_passed,
                f.cloud_run.eventarc_trigger.oidc_service_accounts_passed,
            ];
            let mut want = [false; 4];
            want[i] = true;
            assert_eq!(got, want, "{name}");
        }
    }

    #[tokio::test]
    async fn prepare_dockerfile_golden_with_every_optional_segment() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.92\"\n",
        )
        .unwrap();
        let mut f = receiver(dir.path(), "agent-bin");
        f.target.as_mut().unwrap().package = "my-agent".into();
        f.cloud_run.server_port = 8080;
        f.proxy.port = 8081;
        f.cloud_run.api = true;
        f.cloud_run.debug_api = true;
        f.cloud_run.a2a = true;
        f.cloud_run.a2a_agent_card_url = "https://agent.example/a2a".into();
        f.cloud_run.webui = true;
        f.cloud_run.pubsub = true;
        f.cloud_run.pubsub_trigger = TriggerConfigFlags {
            max_retries: 3,
            base_delay: GoDuration::SECOND,
            max_delay: GoDuration(10 * GoDuration::SECOND.0),
            max_runs: 100,
            oidc_audience: "https://svc.run.app/pubsub".into(),
            oidc_service_accounts: "push@p.iam.gserviceaccount.com".into(),
            ..TriggerConfigFlags::default()
        };
        f.cloud_run.eventarc = true;
        f.cloud_run.eventarc_trigger = TriggerConfigFlags {
            max_retries: 4,
            base_delay: GoDuration(1_500 * GoDuration::MILLISECOND.0),
            max_delay: GoDuration(90 * GoDuration::SECOND.0),
            max_runs: 9,
            oidc_audience: "https://svc.run.app".into(),
            oidc_service_accounts: "a@p.iam.gserviceaccount.com,b@p.iam.gserviceaccount.com".into(),
            ..TriggerConfigFlags::default()
        };
        let t = TestEnv::builder(dir.path()).build();

        f.prepare_dockerfile(&t.env).await.unwrap();

        let content = fs::read_to_string(&f.build.dockerfile_build_path).unwrap();
        assert_eq!(
            content,
            r#"
FROM rust:1.92-bookworm AS builder
WORKDIR /app/
COPY . /app/
RUN cargo build --release --locked --target-dir /gadk-target -p my-agent --bin agent-bin

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /gadk-target/release/agent-bin  /app/agent-bin
EXPOSE 8080
# Command to run the executable when the container starts
CMD ["/app/agent-bin", "web", "-host", "0.0.0.0", "-port", "8080", "api", "-webui_address", "127.0.0.1:8081", "-include_debug_api", "a2a", "--a2a_agent_url", "https://agent.example/a2a", "webui", "--api_server_address", "http://127.0.0.1:8081/api", "pubsub", "--trigger_max_retries", "3", "--trigger_base_delay", "1s", "--trigger_max_delay", "10s", "--trigger_max_concurrent_runs", "100", "-oidc_audience", "https://svc.run.app/pubsub", "-oidc_service_accounts", "push@p.iam.gserviceaccount.com", "eventarc", "--trigger_max_retries", "4", "--trigger_base_delay", "1.5s", "--trigger_max_delay", "1m30s", "--trigger_max_concurrent_runs", "9", "-oidc_audience", "https://svc.run.app", "-oidc_service_accounts", "a@p.iam.gserviceaccount.com,b@p.iam.gserviceaccount.com"]"#
        );
        let mode = fs::metadata(&f.build.dockerfile_build_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            t.out.text(),
            format!(
                "Preparing Dockerfile : \x1b[32mStarting\x1b[0m\n    \x1b[32m> \x1b[0m [Writing: {}]\n\nPreparing Dockerfile : \x1b[32mFinished successfully\x1b[0m\n",
                f.build.dockerfile_build_path
            )
        );
    }

    #[tokio::test]
    async fn prepare_dockerfile_replaces_a_copied_dockerfile_link_without_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::write(&outside, "FROM user\n").unwrap();
        let temp = dir.path().join("temp");
        fs::create_dir(&temp).unwrap();
        std::os::unix::fs::symlink(&outside, temp.join("Dockerfile")).unwrap();
        let f = receiver(&temp, "main");
        let t = TestEnv::builder(dir.path()).build();

        f.prepare_dockerfile(&t.env).await.unwrap();

        assert_eq!(fs::read_to_string(&outside).unwrap(), "FROM user\n");
        let written = temp.join("Dockerfile");
        assert!(!fs::symlink_metadata(&written).unwrap().is_symlink());
        assert!(
            fs::read_to_string(&written)
                .unwrap()
                .contains("COPY . /app/\n")
        );
    }

    #[tokio::test]
    async fn compute_flags_resolves_the_target_and_creates_the_temp_dir() {
        let ws = Workspace::new("agent", "my-agent", &["agent", "tool"]);
        let mut f = ws.flags("agent", "http://127.0.0.1:8081");
        f.bin = "tool".into();
        // An empty --temp_dir is Go's os.TempDir(): TMPDIR.
        f.build.temp_dir = String::new();
        let metadata = ws.metadata();
        let runner = crate::testutil::RecordingRunner::with_responder(move |_| {
            Canned::success(metadata.clone())
        });
        let t = TestEnv::builder(ws.root())
            .var("TMPDIR", &ws.temp_parent().to_string_lossy())
            .runner(runner)
            .build();

        f.compute_flags(&t.env).await.unwrap();

        let temp = Path::new(&f.build.temp_dir);
        assert_eq!(temp.parent(), Some(ws.temp_parent().as_path()));
        let name = temp.file_name().unwrap().to_string_lossy().into_owned();
        let stamp = default_now()
            .with_timezone(&Local)
            .format("cloudrun_%Y%m%d_%H%M%S__")
            .to_string();
        let suffix = name
            .strip_prefix(&stamp)
            .unwrap_or_else(|| panic!("{name}"));
        assert!(suffix.parse::<u32>().is_ok(), "{name}");
        assert!(temp.is_dir());
        assert_eq!(f.build.exec_file, "tool");
        assert_eq!(f.build.exec_path, format!("{}/tool", f.build.temp_dir));
        assert_eq!(
            f.build.dockerfile_build_path,
            format!("{}/Dockerfile", f.build.temp_dir)
        );
        assert_eq!(f.source.src_base_path, ws.root().to_string_lossy());
        assert_eq!(
            f.source.entry_point_path,
            ws.manifest_dir().to_string_lossy()
        );
        let target = f.target.as_ref().unwrap();
        assert_eq!(
            (target.package.as_str(), target.bin.as_str()),
            ("my-agent", "tool")
        );
        let manifest = ws.manifest_dir().join("Cargo.toml");
        let calls = t.runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].args,
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--manifest-path",
                &manifest.to_string_lossy(),
            ]
        );
        assert_eq!(
            t.out.text(),
            format!(
                "Computing flags & preparing temp : \x1b[32mStarting\x1b[0m\n    \x1b[32m> \x1b[0m [Using temp dir: {}]\n\nComputing flags & preparing temp : \x1b[32mFinished successfully\x1b[0m\n",
                f.build.temp_dir
            )
        );
    }

    #[tokio::test]
    async fn compute_flags_rejects_target_problems_before_the_temp_dir() {
        type Setup = fn(&mut Workspace, &mut CloudRunFlags);
        let cases: [(&str, &[&str], Setup, &str); 5] = [
            (
                "unsafe package name",
                &["agent"],
                |ws, _| ws.package = "my agent".into(),
                r#"package name derived from --entry_point_path "agent": "my agent" contains a character not allowed"#,
            ),
            (
                "path dependency outside the workspace",
                &["agent"],
                |ws, _| {
                    let dep = ws.dir.path().join("elsewhere");
                    ws.path_dependencies.push(dep);
                },
                "is outside",
            ),
            (
                "no binary",
                &[],
                |_, _| {},
                r#"package "my-agent" has no binary target"#,
            ),
            (
                "several binaries without --bin",
                &["a", "b"],
                |_, _| {},
                r#"package "my-agent" has several binary targets (a, b); select one with --bin"#,
            ),
            (
                "unknown --bin",
                &["a", "b"],
                |_, f| f.bin = "c".into(),
                r#"package "my-agent" has no binary target "c" (binaries: a, b)"#,
            ),
        ];
        for (name, bins, setup, want) in cases {
            let mut ws = Workspace::new("agent", "my-agent", bins);
            let mut f = ws.flags("agent", "http://127.0.0.1:8081");
            setup(&mut ws, &mut f);
            let t = ws.env();

            let err = f.compute_flags(&t.env).await.unwrap_err();

            assert!(err.contains(want), "{name}: {err}");
            assert_eq!(
                fs::read_dir(ws.temp_parent()).unwrap().count(),
                0,
                "{name}: a temp dir was left behind"
            );
        }
    }

    #[tokio::test]
    async fn clean_temp_removes_the_dir_and_ignores_a_missing_one() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("cloudrun_x");
        fs::create_dir_all(temp.join("a/b")).unwrap();
        fs::write(temp.join("a/b/c"), "x").unwrap();
        let mut f = CloudRunFlags::default();
        f.build.temp_dir = temp.to_string_lossy().into_owned();
        let t = TestEnv::builder(dir.path()).build();

        f.clean_temp(&t.env).await.unwrap();
        assert!(!temp.exists());
        f.clean_temp(&t.env).await.unwrap();
        assert_eq!(
            t.out.text().lines().nth(1).unwrap(),
            format!(
                "    \x1b[32m> \x1b[0m [Clean temp starting with {}]",
                f.build.temp_dir
            )
        );
    }

    /// What the gcloud responder saw in the temp dir at `gcloud run deploy` time.
    #[derive(Default, Debug)]
    struct Staged {
        dockerfile: String,
        dockerfile_mode: u32,
        gcloudignore: String,
        files: Vec<String>,
    }

    fn list(root: &Path, rel: &Path, out: &mut Vec<String>) {
        let mut names: Vec<_> = fs::read_dir(root.join(rel))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        for name in names {
            let path = rel.join(&name);
            out.push(path.to_string_lossy().into_owned());
            if root.join(&path).is_dir() {
                list(root, &path, out);
            }
        }
    }

    /// A workspace with source, a root `Dockerfile`, `.git` and `target` dirs; the gcloud
    /// responder records the staged temp dir and answers `run deploy` with `deploy_exit`.
    fn run_fixture(deploy_exit: i32) -> (Workspace, TestEnv, Arc<Mutex<Staged>>) {
        let ws = Workspace::new("", "agent", &["agent"]);
        let root = ws.root();
        fs::write(root.join("Dockerfile"), "FROM user-image\n").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref\n").unwrap();
        fs::create_dir_all(root.join("target/release")).unwrap();
        fs::write(root.join("target/release/agent"), "bin").unwrap();
        let staged = Arc::new(Mutex::new(Staged::default()));
        let seen = staged.clone();
        let t = ws.env_with(move |c| {
            if c.args.get(2).map(String::as_str) == Some("deploy") {
                let temp = Path::new(&c.dir);
                let mut s = seen.lock().unwrap();
                s.dockerfile = fs::read_to_string(temp.join("Dockerfile")).unwrap();
                s.dockerfile_mode = fs::metadata(temp.join("Dockerfile"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777;
                s.gcloudignore = fs::read_to_string(temp.join(".gcloudignore")).unwrap_or_default();
                list(temp, Path::new(""), &mut s.files);
                return Canned::exit(deploy_exit);
            }
            Canned::success(Vec::new())
        });
        (ws, t, staged)
    }

    fn cloud_run_args(ws: &Workspace, temp_dir: &Path) -> Vec<String> {
        [
            "-r",
            "us-central1",
            "-p",
            "my-proj",
            "-s",
            "my-svc",
            "-t",
            &temp_dir.to_string_lossy(),
        ]
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once(format!(
            "--entry_point_path={}",
            ws.root().display()
        )))
        .collect()
    }

    #[tokio::test]
    async fn deploy_on_cloud_run_runs_go_steps_in_order() {
        let (ws, t, staged) = run_fixture(0);
        let root = ws.root();
        // A temp parent inside the workspace: the staging walk must skip the temp dir.
        let parent = root.join("build-tmp");
        fs::create_dir(&parent).unwrap();
        let args = cloud_run_args(&ws, &parent);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut f = parse(&args);
        let flags_line = f.go_v();

        f.deploy_on_cloud_run(&t.env).await.unwrap();

        let temp = f.build.temp_dir.clone();
        assert!(!Path::new(&temp).exists(), "temp dir kept after success");
        let calls = t.runner.calls();
        let got: Vec<(Vec<String>, String)> = calls
            .iter()
            .map(|c| (c.args.clone(), c.dir.clone()))
            .collect();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let manifest = root.join("Cargo.toml");
        assert_eq!(
            got,
            vec![
                (
                    s(&[
                        "cargo",
                        "metadata",
                        "--format-version",
                        "1",
                        "--no-deps",
                        "--manifest-path",
                        &manifest.to_string_lossy(),
                    ]),
                    root.to_string_lossy().into_owned(),
                ),
                (
                    s(&[
                        "gcloud",
                        "run",
                        "deploy",
                        "my-svc",
                        "--source",
                        ".",
                        "--set-secrets=GOOGLE_API_KEY=GOOGLE_API_KEY:latest",
                        "--region",
                        "us-central1",
                        "--project",
                        "my-proj",
                        "--ingress",
                        "all",
                        "--no-allow-unauthenticated",
                    ]),
                    temp.clone(),
                ),
                (
                    s(&[
                        "gcloud",
                        "run",
                        "services",
                        "proxy",
                        "my-svc",
                        "--project",
                        "my-proj",
                        "--port",
                        "8081",
                        "--region",
                        "us-central1",
                    ]),
                    String::new(),
                ),
            ]
        );

        let staged = staged.lock().unwrap();
        assert!(staged.dockerfile.starts_with(&format!(
            "\nFROM rust:{CHANNEL}-bookworm AS builder\nWORKDIR /app/\nCOPY . /app/\nRUN cargo build --release --locked --target-dir /gadk-target -p agent --bin agent\n"
        )));
        assert!(staged.dockerfile.ends_with(
            r#"CMD ["/app/agent", "web", "-host", "0.0.0.0", "-port", "8080", "api", "-webui_address", "127.0.0.1:8081", "a2a", "--a2a_agent_url", "http://127.0.0.1:8081", "webui", "--api_server_address", "http://127.0.0.1:8081/api"]"#
        ));
        assert_eq!(staged.dockerfile_mode, 0o600);
        assert_eq!(staged.gcloudignore, GCLOUDIGNORE);
        assert_eq!(
            staged.files,
            [
                ".gcloudignore",
                "Cargo.lock",
                "Cargo.toml",
                "Dockerfile",
                "build-tmp",
                "rust-toolchain.toml",
                "src",
                "src/main.rs",
            ]
        );

        let out = t.out.text();
        let banners: Vec<&str> = out
            .lines()
            .filter(|l| l.ends_with("Starting\x1b[0m"))
            .collect();
        assert_eq!(
            banners,
            [
                "Computing flags & preparing temp : \x1b[32mStarting\x1b[0m",
                "Staging source : \x1b[32mStarting\x1b[0m",
                "Preparing Dockerfile : \x1b[32mStarting\x1b[0m",
                "Deploying to Cloud Run : \x1b[32mStarting\x1b[0m",
                "Cleaning temp : \x1b[32mStarting\x1b[0m",
                "Running local gcloud authenticating proxy : \x1b[32mStarting\x1b[0m",
            ]
        );
        let p = |s: &str| format!("    \x1b[32m> \x1b[0m [{s}]\n");
        let banner_line = |s: &str| p(&center_string(s, 80));
        let proxy_step = [
            "Running local gcloud authenticating proxy : \x1b[32mStarting\x1b[0m\n".to_string(),
            p(&"-".repeat(80)),
            banner_line(""),
            banner_line("Running ADK Web UI on http://127.0.0.1:8081/ui/    <-- open this"),
            banner_line("ADK REST API on http://127.0.0.1:8081/api/         "),
            banner_line(""),
            banner_line("Press Ctrl-C to stop"),
            banner_line(""),
            p(&"-".repeat(80)),
            p("Running :  \x1b[33m  \x1b[0m   gcloud run services proxy my-svc --project my-proj --port 8081 --region us-central1"),
            "\n".to_string(),
            "Running local gcloud authenticating proxy : \x1b[32mFinished successfully\x1b[0m\n".to_string(),
        ]
        .concat();
        assert!(out.starts_with(&format!("{flags_line}\n")), "{out}");
        assert!(out.ends_with(&proxy_step), "{out}");
        assert!(out.contains(&p(&format!("Copying {} to {temp}", root.display()))));
        assert!(out.contains(&p(&format!(
            "Running :  \x1b[33m {temp} \x1b[0m   gcloud run deploy my-svc --source . --set-secrets=GOOGLE_API_KEY=GOOGLE_API_KEY:latest --region us-central1 --project my-proj --ingress all --no-allow-unauthenticated"
        ))));
    }

    #[tokio::test]
    async fn deploy_on_cloud_run_keeps_a_user_gcloudignore() {
        let (ws, t, staged) = run_fixture(0);
        fs::write(ws.root().join(".gcloudignore"), "custom\n").unwrap();
        let args = cloud_run_args(&ws, &ws.temp_parent());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut f = parse(&args);

        f.deploy_on_cloud_run(&t.env).await.unwrap();

        assert_eq!(staged.lock().unwrap().gcloudignore, "custom\n");
        assert!(!t.out.text().contains(".gcloudignore]"));
    }

    #[tokio::test]
    async fn deploy_on_cloud_run_keeps_the_temp_dir_when_gcloud_fails() {
        let (ws, t, _) = run_fixture(1);
        let args = cloud_run_args(&ws, &ws.temp_parent());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut f = parse(&args);

        let err = f.deploy_on_cloud_run(&t.env).await.unwrap_err();

        assert_eq!(err, "exit status 1");
        assert!(
            Path::new(&f.build.temp_dir).is_dir(),
            "temp dir removed after a failure"
        );
        let calls = t.runner.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        let out = t.out.text();
        assert!(out.ends_with(
            "Deploying to Cloud Run : \x1b[31mFinished with error\x1b[0m\nError: exit status 1\n"
        ));
        assert!(!out.contains("Cleaning temp"));
    }

    #[tokio::test]
    async fn steps_need_a_resolved_target() {
        let dir = tempfile::tempdir().unwrap();
        let t = TestEnv::builder(dir.path()).build();
        let mut f = CloudRunFlags::default();
        assert_eq!(f.stage_source(&t.env).await, Err(NO_TARGET.to_string()));
        assert_eq!(
            f.prepare_dockerfile(&t.env).await,
            Err(NO_TARGET.to_string())
        );
    }
}
