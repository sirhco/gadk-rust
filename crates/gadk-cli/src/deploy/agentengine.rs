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

//! Port of ADK-Go `cmd/adkgo/internal/deploy/agentengine/agentengine.go`:
//! `gadk deploy agentengine`.
//!
//! The flag structs keep Go's shape (`deployAgentEngineFlags` and its nested structs, field
//! for field in Go's order) so [`AgentEngineFlags::go_v`] can print Go's `fmt.Println(flags)`
//! line; gadk's additions (`bin`, `target`) sit after Go's fields and are never printed.
//!
//! Task 2 (contracts) implements [`command`]; the bodies marked `todo!()` are Task 5's, which
//! also adds `Env::vertex` (plan decision D3).

#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use std::time::Duration;

use clap::{ArgMatches, Command};
use gadk_platform::GoDuration;
use serde_json::Value;

use crate::Env;
use crate::flags::{bool_flag, duration_flag, int_flag, pflag_command, stray_args, string_flag};
use crate::target::CargoTarget;

/// Go `agentEngineOperationTimeout`: the bound on waiting for a create/update operation.
pub const AGENT_ENGINE_OPERATION_TIMEOUT: Duration = Duration::from_secs(45 * 60);

/// Go `agentEngineCmd.Short`.
pub const SHORT: &str = "Deploys the application to Agent Engine.";
/// Go `agentEngineCmd.Long`.
pub const LONG: &str = "Deploys the application to Agent Engine. It creates a source archive, uploads it to create a Reasoning Engine, and cleans up temporary files.";

/// Go `gCloudFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GCloudFlags {
    pub region: String,
    pub project_name: String,
}

/// Go `memoryBankFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryBankFlags {
    pub deploy: bool,
    pub model: String,
    pub ttl: GoDuration,
}

/// Go `agentEngineServiceFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentEngineServiceFlags {
    pub name: String,
    pub display_name: String,
    pub server_port: i64,
    pub agent_engine_id: String,
    pub memory_bank: MemoryBankFlags,
}

/// Go `buildFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildFlags {
    pub temp_dir: String,
    pub exec_path: String,
    pub exec_file: String,
    pub dockerfile_build_path: String,
    pub archive_path: String,
}

/// Go `sourceFlags`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFlags {
    pub src_base_path: String,
    pub entry_point_path: String,
    pub orig_entry_point_path: String,
    pub source_dir: String,
}

/// Go `deployAgentEngineFlags` plus gadk's `--bin` and the resolved Cargo target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentEngineFlags {
    pub gcloud: GCloudFlags,
    pub agent_engine: AgentEngineServiceFlags,
    pub build: BuildFlags,
    pub source: SourceFlags,
    /// gadk's `--bin` (not in Go; not printed).
    pub bin: String,
    /// The Cargo target `compute_flags` resolved (not in Go; not printed).
    pub target: Option<CargoTarget>,
}

/// Go `agentEngineCmd` with the flags of its `init`, in Go's order, plus gadk's `--bin`.
pub fn command() -> Command {
    pflag_command("agentengine")
        .about(SHORT)
        .long_about(LONG)
        .arg(string_flag("region", Some('r'), "", "GCP Region"))
        .arg(string_flag("project_name", Some('p'), "", "GCP Project Name"))
        .arg(string_flag("name", Some('s'), "", "Agent Engine name"))
        .arg(string_flag(
            "temp_dir",
            Some('t'),
            "",
            "Temp dir for build, defaults to os.TempDir() if not specified",
        ))
        .arg(int_flag("server_port", None, "8080", "agentEngine server port"))
        // Divergence (ledger 6b): `-e` names a Cargo package (Go: "Path to an entry point (go 'main')").
        .arg(string_flag(
            "entry_point_path",
            Some('e'),
            "",
            "Path to the Cargo package of the entry point (its directory or Cargo.toml), defaults to the current directory",
        ))
        // Divergence (ledger 6b): the archive root defaults to the Cargo workspace root (Go: the
        // current working directory).
        .arg(string_flag(
            "source_dir",
            Some('d'),
            "",
            "Directory to archive (the Cargo workspace root or an ancestor of it), defaults to the Cargo workspace root",
        ))
        .arg(string_flag(
            "agent_engine_id",
            None,
            "",
            "ID of the Agent Engine instance to update if it exists (default: \"\", which means a new instance will be created).",
        ))
        .arg(bool_flag(
            "mem_deploy",
            false,
            "If set to true then memory bank will be deployed too",
        ))
        .arg(string_flag(
            "mem_model",
            None,
            "",
            "Name of the model to be used for memory generation, as publishers/google/models/<model_id>. If empty, Memory Bank uses its own default model. A non-empty value is prefixed with projects/<project_name>/locations/<region>, so the model must be available in that region. To list models, GET https://${LOCATION_ID}-aiplatform.googleapis.com/v1beta1/publishers/google/models",
        ))
        .arg(duration_flag(
            "mem_ttl",
            None,
            "8760h0m0s",
            "Time-To-Live for memories",
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

/// The flags of a parsed `agentengine` command (Go: cobra binds them into the package
/// global). Task 5.
pub fn from_matches(m: &ArgMatches) -> AgentEngineFlags {
    todo!("Task 5")
}

impl AgentEngineFlags {
    /// Go `fmt.Println(flags)`'s line without the newline: Go's `%v` of
    /// `deployAgentEngineFlags`, field for field (durations as nanoseconds, derived fields as
    /// they are now); `bin` and `target` are not printed. Task 5.
    pub fn go_v(&self) -> String {
        todo!("Task 5")
    }

    /// Go `computeFlags` (R9): target resolution, the shell-safe checks, the archive root,
    /// the `agentEngine_<local time>__*` temp dir, the derived paths, the display name and the
    /// `--mem_model` prefix. Task 5.
    pub async fn compute_flags(&mut self, env: &Env) -> Result<(), String> {
        todo!("Task 5")
    }

    /// Go `cleanTemp`. Task 5.
    pub async fn clean_temp(&self, env: &Env) -> Result<(), String> {
        todo!("Task 5")
    }

    /// Go `prepareDockerfile` (R10): reads only the receiver. Task 5.
    pub async fn prepare_dockerfile(&self, env: &Env) -> Result<(), String> {
        todo!("Task 5")
    }

    /// Go `createArchive` (R10): the sorted tar.gz of the archive root with the generated
    /// `Dockerfile` last. Task 5.
    pub async fn create_archive(&self, env: &Env) -> Result<(), String> {
        todo!("Task 5")
    }

    /// Go `memoryBankContextSpec` (R12) as protojson. Task 5.
    pub fn memory_bank_context_spec(&self) -> Value {
        todo!("Task 5")
    }

    /// Go `gcloudDeployToAgentEngine` (R11 create). Task 5.
    pub async fn deploy_to_agent_engine(&self, env: &Env) -> Result<(), String> {
        todo!("Task 5")
    }

    /// Go `gcloudUpdateAgentEngine` (R11 update). Task 5.
    pub async fn update_agent_engine(&self, env: &Env) -> Result<(), String> {
        todo!("Task 5")
    }

    /// Go `deployOnAgentEngine`: prints [`AgentEngineFlags::go_v`], then compute →
    /// Dockerfile → archive → update|deploy → clean, stopping at the first error. Task 5.
    pub async fn deploy_on_agent_engine(&mut self, env: &Env) -> Result<(), String> {
        todo!("Task 5")
    }
}
