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

//! Port of ADK-Go `cmd/adkgo/internal/deploy/cloudrun/cloudrun.go`: `gadk deploy cloudrun`.
//!
//! The flag structs keep Go's shape (`deployCloudRunFlags` and its nested structs, field for
//! field in Go's order) so [`CloudRunFlags::go_v`] can print Go's `fmt.Println(flags)` line;
//! gadk's additions (`bin`, `target`) sit after Go's fields and are never printed.
//!
//! Task 2 (contracts) implements [`command`]; the bodies marked `todo!()` are Task 4's.

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use clap::{ArgMatches, Command};
use gadk_platform::GoDuration;

use crate::Env;
use crate::flags::{bool_flag, duration_flag, int_flag, pflag_command, stray_args, string_flag};
use crate::target::CargoTarget;

/// Go `cloudrunCmd.Short`.
pub const SHORT: &str = "Deploys the application to cloudrun.";
/// Go `cloudrunCmd.Long` (tabs and trailing spaces as Go's raw string).
// Divergence (ledger 6b): the first sentence says the image is built from the staged Cargo
// workspace (Go: "fed with locally compiled server executable containing Web UI static files").
pub const LONG: &str = "Deployment prepares a Dockerfile which builds the server executable from the staged Cargo workspace.\n\tService on Cloudrun is created using this information. \n\tLocal proxy adding authentication is started. \n\t";

/// Go `triggerOIDCFlagNames`: the four OIDC flags whose `Changed` state is recorded.
pub const TRIGGER_OIDC_FLAG_NAMES: [&str; 4] = [
    "pubsub_oidc_audience",
    "pubsub_oidc_service_accounts",
    "eventarc_oidc_audience",
    "eventarc_oidc_service_accounts",
];

/// Go `gCloudFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GCloudFlags {
    pub region: String,
    pub project_name: String,
}

/// Go `triggerConfigFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriggerConfigFlags {
    pub max_retries: i64,
    pub base_delay: GoDuration,
    pub max_delay: GoDuration,
    pub max_runs: i64,
    pub oidc_audience: String,
    pub oidc_service_accounts: String,
    /// Whether each OIDC flag was on the command line (Go `Changed`), which an empty value
    /// alone cannot tell from it being omitted.
    pub oidc_audience_passed: bool,
    pub oidc_service_accounts_passed: bool,
}

/// Go `cloudRunServiceFlags`.
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

/// Go `localProxyFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalProxyFlags {
    pub port: i64,
}

/// Go `buildFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildFlags {
    pub temp_dir: String,
    pub exec_path: String,
    pub exec_file: String,
    pub dockerfile_build_path: String,
}

/// Go `sourceFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFlags {
    pub src_base_path: String,
    pub entry_point_path: String,
}

/// Go `deployCloudRunFlags` plus gadk's `--bin` and the resolved Cargo target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudRunFlags {
    pub gcloud: GCloudFlags,
    pub cloud_run: CloudRunServiceFlags,
    pub proxy: LocalProxyFlags,
    pub build: BuildFlags,
    pub source: SourceFlags,
    /// gadk's `--bin` (not in Go; not printed).
    pub bin: String,
    /// The Cargo target `compute_flags` resolved (not in Go; not printed).
    pub target: Option<CargoTarget>,
}

/// Go `cloudrunCmd` with the flags of its `init`, in Go's order, plus gadk's `--bin`.
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
        // Divergence (ledger 6b): `-e` names a Cargo package (Go: "Path to an entry point (go 'main')").
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
        // Divergence (ledger 6b): gadk-only flag selecting the Cargo binary target.
        .arg(string_flag(
            "bin",
            None,
            "",
            "Binary target of the package to deploy, required when the package has more than one",
        ))
        .arg(stray_args())
}

/// The flags of a parsed `cloudrun` command (Go: cobra binds them into the package global),
/// with Go's `PreRunE` applied (`note_passed_oidc_flags` from [`crate::flags::passed`]).
/// Task 4.
pub fn from_matches(m: &ArgMatches) -> CloudRunFlags {
    todo!("Task 4")
}

impl CloudRunFlags {
    /// Go `fmt.Println(flags)`'s line without the newline: Go's `%v` of
    /// `deployCloudRunFlags`, field for field (durations as nanoseconds, derived fields as
    /// they are now); `bin` and `target` are not printed. Task 4.
    pub fn go_v(&self) -> String {
        todo!("Task 4")
    }

    /// Go `notePassedOIDCFlags(cmd.Flags().Changed)`. Task 4.
    pub fn note_passed_oidc_flags(&mut self, passed: &dyn Fn(&str) -> bool) {
        todo!("Task 4")
    }

    /// Go `computeFlags` (R6): Go's checks in Go's order, target resolution, the shell-safe
    /// checks, then the `cloudrun_<local time>__*` temp dir and the derived paths. Task 4.
    pub async fn compute_flags(&mut self, env: &Env) -> Result<(), String> {
        todo!("Task 4")
    }

    /// `"Staging source"` (in Go's `compileEntryPoint` slot, R8): copies the workspace into
    /// the temp dir and writes a `.gcloudignore` when the copy has none. Task 4.
    pub async fn stage_source(&mut self, env: &Env) -> Result<(), String> {
        todo!("Task 4")
    }

    /// Go `prepareDockerfile` (R7): reads only the receiver. Task 4.
    pub async fn prepare_dockerfile(&self, env: &Env) -> Result<(), String> {
        todo!("Task 4")
    }

    /// Go `gcloudDeployToCloudRun` (R8). Task 4.
    pub async fn gcloud_deploy_to_cloud_run(&self, env: &Env) -> Result<(), String> {
        todo!("Task 4")
    }

    /// Go `cleanTemp`. Task 4.
    pub async fn clean_temp(&self, env: &Env) -> Result<(), String> {
        todo!("Task 4")
    }

    /// Go `runGcloudProxy` (banner and `gcloud run services proxy`). Task 4.
    pub async fn run_gcloud_proxy(&self, env: &Env) -> Result<(), String> {
        todo!("Task 4")
    }

    /// Go `deployOnCloudRun`: prints [`CloudRunFlags::go_v`], then compute → stage →
    /// Dockerfile → deploy → clean → proxy, stopping at the first error. Task 4.
    pub async fn deploy_on_cloud_run(&mut self, env: &Env) -> Result<(), String> {
        todo!("Task 4")
    }
}

/// Go `validateServiceName`. Task 4.
pub fn validate_service_name(name: &str) -> Result<(), String> {
    todo!("Task 4")
}

/// Go `validateTriggerOIDC(name, t)`. Task 4.
pub fn validate_trigger_oidc(name: &str, t: &TriggerConfigFlags) -> Result<(), String> {
    todo!("Task 4")
}

/// Go `checkOIDCValuesAreNotFlags`. Task 4.
pub fn check_oidc_values_are_not_flags(
    aud_flag: &str,
    sa_flag: &str,
    audience: &str,
    accounts: &str,
) -> Result<(), String> {
    todo!("Task 4")
}

/// Go `checkTriggerOIDCEmitted`. Task 4.
pub fn check_trigger_oidc_emitted(
    name: &str,
    enabled: bool,
    t: &TriggerConfigFlags,
) -> Result<(), String> {
    todo!("Task 4")
}

/// Go `writeOIDCFlags`. Task 4.
// The stub never grows `b`; Task 4's body does, which removes this allow.
#[allow(clippy::ptr_arg)]
pub fn write_oidc_flags(b: &mut String, t: &TriggerConfigFlags) {
    todo!("Task 4")
}
