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
//! for field in Go's order); gadk's additions (`bin`, `target`) sit after Go's fields. As Go
//! since #1981, the flags are not printed: `--env` values may be secrets.
//!
//! `compute_flags` resolves the Cargo target (R4) where Go splits a `main.go` path, and fixes
//! the archive root (`-d`, else the Cargo workspace root, R9); the derived fields are set as
//! Go sets them (`execFile` = the binary, `srcBasePath` = the package directory,
//! `sourceDir` = the absolute archive root). Vertex AI is reached through
//! `gadk_vertexai::reasoning_engine` with [`Env::vertex`] (R11).

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{Local, SecondsFormat};
use clap::{ArgMatches, Command};
use gadk_platform::{GoDuration, go_path_clean, go_path_join, go_quote};
use gadk_vertexai::reasoning_engine::{ReasoningEngineClient, ReasoningEngineOptions};
use serde_json::{Value, json};

use crate::Env;
use crate::flags::{
    STRAY_ARGS, bool_flag, bool_value, duration_flag, duration_value, int_flag, int_value, passed,
    pflag_command, stray_args, string_flag, string_value,
};
use crate::stage::{BuilderStage, SourceTree, dockerfile, make_temp};
use crate::target::{
    COMPILED_RUSTC_VERSION, CargoTarget, builder_image, builder_rust_version,
    check_path_dependencies, resolve,
};
use crate::util::{Printer, log_start_stop, validate_shell_arg_safe};

pub mod environment;
#[cfg(test)]
pub(crate) mod tests;

use environment::{EnvVar, deployment_spec, register_environment_flags};

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
    /// `--env` entries, verbatim (Go `env []string`; unvalidated until
    /// [`AgentEngineFlags::environment_variables`]).
    pub env: Vec<OsString>,
    /// `--default_env`.
    pub default_env: bool,
    /// The environment [`AgentEngineFlags::compute_flags`] computed.
    pub env_vars: Vec<EnvVar>,
    /// `--env` or `--default_env` was on the command line (Go: cobra `Changed`): an update
    /// replaces the environment.
    pub update_env: bool,
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
/// Positional arguments are collected (not a clap error) so [`validate_args`] can reject them
/// with cobra's text.
pub fn command() -> Command {
    let cmd = pflag_command("agentengine")
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
        ));
    register_environment_flags(cmd)
        // Divergence (ledger 6b): gadk-only flag selecting the Cargo binary target.
        .arg(string_flag(
            "bin",
            None,
            "",
            "Binary target of the package to deploy, required when the package has more than one",
        ))
        .arg(stray_args())
}

/// Go `agentEngineCmd.Args` (`cobra.ExactArgs(0)`): any positional argument fails with
/// cobra's `accepts 0 arg(s), received N`, which does not echo the arguments; runs before
/// anything else.
pub fn validate_args(m: &ArgMatches) -> Result<(), String> {
    let n = m.get_raw(STRAY_ARGS).map_or(0, |v| v.len());
    if n != 0 {
        return Err(format!("accepts 0 arg(s), received {n}"));
    }
    Ok(())
}

/// The flags of a parsed `agentengine` command (Go: cobra binds them into the package
/// global); the derived fields are empty until [`AgentEngineFlags::compute_flags`].
pub fn from_matches(m: &ArgMatches) -> AgentEngineFlags {
    AgentEngineFlags {
        gcloud: GCloudFlags {
            region: string_value(m, "region"),
            project_name: string_value(m, "project_name"),
        },
        agent_engine: AgentEngineServiceFlags {
            name: string_value(m, "name"),
            display_name: String::new(),
            server_port: int_value(m, "server_port"),
            agent_engine_id: string_value(m, "agent_engine_id"),
            memory_bank: MemoryBankFlags {
                deploy: bool_value(m, "mem_deploy"),
                model: string_value(m, "mem_model"),
                ttl: duration_value(m, "mem_ttl"),
            },
            env: m
                .get_many::<OsString>("env")
                .map(|v| v.cloned().collect())
                .unwrap_or_default(),
            default_env: bool_value(m, "default_env"),
            env_vars: Vec::new(),
            update_env: passed(m, "env") || passed(m, "default_env"),
        },
        build: BuildFlags {
            temp_dir: string_value(m, "temp_dir"),
            ..BuildFlags::default()
        },
        source: SourceFlags {
            entry_point_path: string_value(m, "entry_point_path"),
            source_dir: string_value(m, "source_dir"),
            ..SourceFlags::default()
        },
        bin: string_value(m, "bin"),
        target: None,
    }
}

/// `path` relative to `root` when `path` is `root` or lies under it: compared lexically (both
/// cleaned), else once both resolve (a symlinked temp or source dir).
fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    let clean = |p: &Path| PathBuf::from(go_path_clean(&p.to_string_lossy()));
    if let Ok(rel) = clean(path).strip_prefix(clean(root)) {
        return Some(rel.to_path_buf());
    }
    let (path, root) = (fs::canonicalize(path).ok()?, fs::canonicalize(root).ok()?);
    path.strip_prefix(root).ok().map(Path::to_path_buf)
}

/// Go protojson's `google.protobuf.Duration`: seconds with 0, 3, 6 or 9 fractional digits and
/// an `s` suffix (`86400s`, `1.500s`, `-0.000000001s`).
fn protojson_duration(d: GoDuration) -> String {
    let (secs, nanos) = (d.0 / 1_000_000_000, d.0 % 1_000_000_000);
    let sign = if secs == 0 && nanos < 0 { "-" } else { "" };
    let x = format!("{sign}{secs}.{:09}", nanos.unsigned_abs());
    let x = x.strip_suffix("000").unwrap_or(&x);
    let x = x.strip_suffix("000").unwrap_or(x);
    let x = x.strip_suffix(".000").unwrap_or(x);
    format!("{x}s")
}

/// Go `op.Wait(waitCtx)` under `context.WithTimeout(ctx, agentEngineOperationTimeout)`,
/// wrapped as `operation failed: %w`.
async fn wait_for_operation(client: &ReasoningEngineClient, op: Value) -> Result<Value, String> {
    match tokio::time::timeout(AGENT_ENGINE_OPERATION_TIMEOUT, client.wait(op)).await {
        Ok(Ok(engine)) => Ok(engine),
        Ok(Err(e)) => Err(format!("operation failed: {e}")),
        Err(_) => Err("operation failed: context deadline exceeded".to_string()),
    }
}

/// A string field of a protojson message (`""` when absent, as Go's getters).
fn str_field<'a>(v: &'a Value, name: &str) -> &'a str {
    v.get(name).and_then(Value::as_str).unwrap_or_default()
}

impl AgentEngineFlags {
    /// Go `computeFlags` (R9): the environment (validated first, before any other step),
    /// target resolution, the shell-safe checks, the archive root,
    /// the `agentEngine_<local time>__*` temp dir, the derived paths, the display name and the
    /// `--mem_model` prefix. Every check runs before the temp dir is created.
    pub async fn compute_flags(&mut self, env: &Env) -> Result<(), String> {
        log_start_stop(
            &env.out,
            "Computing flags & preparing temp",
            async |p: Printer| {
                self.agent_engine.env_vars = self.environment_variables()?;
                self.source.orig_entry_point_path = self.source.entry_point_path.clone();
                let orig = self.source.orig_entry_point_path.clone();
                self.source.entry_point_path = env.abs(&orig).to_string_lossy().into_owned();

                // Divergence (ledger 6b): Go splits the `main.go` path and strips `.go`; gadk
                // resolves the Cargo package and binary (R4), the binary standing in for Go's
                // execFile and the package directory for Go's srcBasePath.
                let target = resolve(env, &orig, &self.bin).await?;
                self.source.src_base_path = target.manifest_dir.to_string_lossy().into_owned();
                if self.build.exec_path.is_empty() {
                    self.build.exec_file = target.bin.clone();
                }

                validate_shell_arg_safe(
                    &self.build.exec_file,
                    &format!(
                        "executable name derived from --entry_point_path {}:",
                        go_quote(&orig)
                    ),
                )?;
                // Divergence (ledger 6b): the package name reaches the RUN line's `-p` (Go
                // has no package name).
                validate_shell_arg_safe(
                    &target.package,
                    &format!(
                        "package name derived from --entry_point_path {}:",
                        go_quote(&orig)
                    ),
                )?;
                validate_shell_arg_safe(&orig, "--entry_point_path")?;

                // Divergence (ledger 6b): the archive root is `-d` or else the Cargo workspace
                // root (Go: the working directory), and must contain the workspace root,
                // whose path below it becomes the builder's WORKDIR.
                let archive_root = if self.source.source_dir.is_empty() {
                    target.workspace_root.clone()
                } else {
                    env.abs(&self.source.source_dir)
                };
                let Some(workdir) = relative_to(&target.workspace_root, &archive_root) else {
                    return Err(format!(
                        "--source_dir {} is neither the Cargo workspace root {} nor an ancestor of it",
                        go_quote(&self.source.source_dir),
                        go_quote(&target.workspace_root.to_string_lossy())
                    ));
                };
                validate_shell_arg_safe(workdir.as_os_str().as_encoded_bytes(), "--source_dir")?;
                check_path_dependencies(&target, &archive_root)?;
                self.source.source_dir = archive_root.to_string_lossy().into_owned();
                self.target = Some(target);

                if self.build.temp_dir.is_empty() {
                    self.build.temp_dir = env.temp_dir().to_string_lossy().into_owned();
                }
                let absp = env.abs(&self.build.temp_dir);
                // Divergence (ledger 6b): the wrapped error is Rust's `io::Error` text (Go's
                // is the `*PathError`, e.g. `mkdir <path>: permission denied`).
                let dir = make_temp(&absp, "agentEngine_", env.time.now()).map_err(|e| {
                    format!(
                        "cannot create a temporary sub directory in '{}': {e}",
                        absp.display()
                    )
                })?;
                self.build.temp_dir = dir.to_string_lossy().into_owned();
                p.print(&[&"Using temp dir:", &self.build.temp_dir]);

                if self.build.exec_path.is_empty() {
                    self.build.exec_path = go_path_join(&[&self.build.temp_dir, &self.build.exec_file]);
                }
                self.build.dockerfile_build_path =
                    go_path_join(&[self.build.temp_dir.as_str(), "Dockerfile"]);
                self.build.archive_path = go_path_join(&[self.build.temp_dir.as_str(), "archive.tgz"]);

                let date_time = env
                    .time
                    .now()
                    .with_timezone(&Local)
                    .to_rfc3339_opts(SecondsFormat::Secs, true);
                self.agent_engine.display_name = self.agent_engine.name.clone();
                if self.agent_engine.display_name.is_empty() {
                    self.agent_engine.display_name = format!("ADK Agent: {date_time}");
                }

                // prefix an explicit model with the project and region
                if !self.agent_engine.memory_bank.model.is_empty() {
                    self.agent_engine.memory_bank.model = format!(
                        "projects/{}/locations/{}/{}",
                        self.gcloud.project_name,
                        self.gcloud.region,
                        self.agent_engine.memory_bank.model
                    );
                }
                Ok(())
            },
        )
        .await
    }

    /// Go `cleanTemp`.
    pub async fn clean_temp(&self, env: &Env) -> Result<(), String> {
        log_start_stop(&env.out, "Cleaning temp", async |p: Printer| {
            p.print(&[&"Clean temp starting with", &self.build.temp_dir]);
            // Divergence (ledger 6b): the wrapped error is Rust's `io::Error` text.
            match fs::remove_dir_all(&self.build.temp_dir) {
                // Go `os.RemoveAll`: a missing path is not an error.
                Err(e) if e.kind() != ErrorKind::NotFound => Err(format!(
                    "failed to clean temp directory {}: {e}",
                    self.build.temp_dir
                )),
                _ => Ok(()),
            }
        })
        .await
    }

    /// The directory archived (Go `createArchive`'s `workspaceRoot`): `source_dir` (absolute
    /// after [`compute_flags`](Self::compute_flags)), else the target's workspace root.
    fn archive_root(&self) -> PathBuf {
        if self.source.source_dir.is_empty() {
            self.target
                .as_ref()
                .map(|t| t.workspace_root.clone())
                .unwrap_or_default()
        } else {
            PathBuf::from(&self.source.source_dir)
        }
    }

    /// Go `prepareDockerfile` (R10): reads only the receiver (the builder version from the
    /// target's workspace, `WORKDIR /app/<workspace root below the archive root>`, `-p
    /// <package> --bin <execFile>`, `EXPOSE`/`CMD` from `server_port`), written with mode 0600.
    pub async fn prepare_dockerfile(&self, env: &Env) -> Result<(), String> {
        log_start_stop(&env.out, "Preparing Dockerfile", async |p: Printer| {
            p.print(&[&"Writing:", &self.build.dockerfile_build_path]);
            let target = self.target.clone().unwrap_or_default();
            let workdir = relative_to(&target.workspace_root, &self.archive_root())
                .map(|w| w.to_string_lossy().into_owned())
                .unwrap_or_default();
            let builder = BuilderStage {
                image: builder_image(&builder_rust_version(&target, COMPILED_RUSTC_VERSION)),
                workdir,
                package: target.package,
                bin: self.build.exec_file.clone(),
            };
            let text = dockerfile(
                &builder,
                self.agent_engine.server_port,
                r#", "agentengine""#,
            );
            // Divergence (ledger 6b): the error is Rust's `io::Error` text (Go: the
            // `*PathError`, `open <path>: …`).
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&self.build.dockerfile_build_path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
                .map_err(|e| e.to_string())
        })
        .await
    }

    /// Go `createArchive` (R10): the sorted tar.gz of the archive root (without `.git`,
    /// `target`, a nested temp dir and the root `Dockerfile`) with the generated `Dockerfile`
    /// last.
    // Divergence (ledger 6b): Go runs `tar -czf` through LogCommand (a `Running :` line, tar's
    // own errors); gadk writes the archive in-process (no `Running :` line, gadk's error texts)
    // and leaves out a root `Dockerfile` of the source, which Go archives before the generated
    // one (the later entry wins on extraction).
    pub async fn create_archive(&self, env: &Env) -> Result<(), String> {
        log_start_stop(&env.out, "Creating source archive", async |p: Printer| {
            let root = self.archive_root();
            p.print(&[&"Creating:", &self.build.archive_path]);
            let mut exclude = vec![root.join("Dockerfile")];
            if let Some(rel) = relative_to(Path::new(&self.build.temp_dir), &root)
                && !rel.as_os_str().is_empty()
            {
                exclude.push(root.join(rel));
            }
            let tree = SourceTree::walk(&root, &exclude).map_err(|e| {
                format!(
                    "cannot read the source directory {}: {e}",
                    go_quote(&root.to_string_lossy())
                )
            })?;
            let generated = fs::read(&self.build.dockerfile_build_path)
                .map_err(|e| format!("cannot read the generated Dockerfile: {e}"))?;
            tree.write_tar_gz(
                Path::new(&self.build.archive_path),
                &[("Dockerfile", generated.as_slice())],
            )
            .map_err(|e| {
                format!(
                    "cannot write the source archive {}: {e}",
                    go_quote(&self.build.archive_path)
                )
            })
        })
        .await
    }

    /// Go `memoryBankContextSpec` (R12) as a protojson `ReasoningEngineContextSpec`:
    /// `memoryBankConfig.ttlConfig.defaultTtl` = `--mem_ttl`, and
    /// `memoryBankConfig.generationConfig.model` only for a non-empty `--mem_model`.
    pub fn memory_bank_context_spec(&self) -> Value {
        let mb = &self.agent_engine.memory_bank;
        let mut cfg = json!({"ttlConfig": {"defaultTtl": protojson_duration(mb.ttl)}});
        if !mb.model.is_empty() {
            cfg["generationConfig"] = json!({"model": mb.model});
        }
        json!({"memoryBankConfig": cfg})
    }

    /// Go `aiplatform.NewReasoningEngineClient(ctx, option.WithEndpoint(<region>-aiplatform…))`
    /// with [`Env::vertex`]; ADC reads [`Env::env`] unless the options name an environment.
    fn client(&self, env: &Env) -> Result<ReasoningEngineClient, String> {
        let mut opts: ReasoningEngineOptions = env.vertex.clone();
        if opts.env.is_none() {
            opts.env = Some(env.env.clone());
        }
        ReasoningEngineClient::new(&self.gcloud.region, opts)
            .map_err(|e| format!("cannot create ReasoningEngineClient: {e}"))
    }

    /// The parts of Go's create and update requests read from disk and the server package:
    /// the `sourceCodeSpec` (inline archive, empty `imageSpec`) and the class methods,
    /// printing `Methods: <json>` as Go does.
    fn source_code_spec_and_methods(&self, p: &Printer) -> Result<(Value, Value), String> {
        // Divergence (ledger 6b): the wrapped error is Rust's `io::Error` text (Go: the
        // `*PathError`).
        let archive = fs::read(&self.build.archive_path)
            .map_err(|e| format!("cannot read archive file: {e}"))?;
        let methods = gadk_server::agentengine::list_class_methods();
        let methods_json =
            gadk_model::to_go_json(&methods).map_err(|e| format!("cannot marshal methods: {e}"))?;
        p.print(&[&"Methods:", &String::from_utf8_lossy(&methods_json)]);
        let source_code_spec = json!({
            "inlineSource": {"sourceArchive": STANDARD.encode(&archive)},
            "imageSpec": {},
        });
        Ok((source_code_spec, Value::from(methods)))
    }

    /// Go `gcloudDeployToAgentEngine` (R11 create): `CreateReasoningEngine` under
    /// `projects/<p>/locations/<r>`, then the operation awaited for at most 45 minutes.
    pub async fn deploy_to_agent_engine(&self, env: &Env) -> Result<(), String> {
        log_start_stop(&env.out, "Deploying to Agent Engine", async |p: Printer| {
            let parent = format!(
                "projects/{}/locations/{}",
                self.gcloud.project_name, self.gcloud.region
            );
            let client = self.client(env)?;
            let (source_code_spec, methods) = self.source_code_spec_and_methods(&p)?;
            let mut engine = json!({
                "displayName": self.agent_engine.display_name,
                "spec": {
                    "sourceCodeSpec": source_code_spec,
                    "agentFramework": "google-adk",
                    "deploymentSpec": deployment_spec(&self.agent_engine.env_vars),
                    "classMethods": methods,
                },
            });
            if self.agent_engine.memory_bank.deploy {
                engine["contextSpec"] = self.memory_bank_context_spec();
            }

            p.print(&[&"Sending CreateReasoningEngine request..."]);
            let op = client
                .create(&parent, engine)
                .await
                .map_err(|e| format!("CreateReasoningEngine failed: {e}"))?;

            p.print(&[&"Waiting for operation to complete..."]);
            let re = wait_for_operation(&client, op).await?;

            p.print(&[&"Deployed Reasoning Engine:", &str_field(&re, "name")]);
            p.print(&[&"Display Name:", &str_field(&re, "displayName")]);
            Ok(())
        })
        .await
    }

    /// Go `gcloudUpdateAgentEngine` (R11 update): `UpdateReasoningEngine` of
    /// `projects/<p>/locations/<r>/reasoningEngines/<id>` with Go's update mask in protojson
    /// form (the environment only when [`AgentEngineServiceFlags::update_env`], before the
    /// memory bank's `spec.contextSpec`), then the operation awaited for at most 45 minutes.
    pub async fn update_agent_engine(&self, env: &Env) -> Result<(), String> {
        log_start_stop(&env.out, "Updating Agent Engine", async |p: Printer| {
            let name = format!(
                "projects/{}/locations/{}/reasoningEngines/{}",
                self.gcloud.project_name, self.gcloud.region, self.agent_engine.agent_engine_id
            );
            let client = self.client(env)?;
            let (source_code_spec, methods) = self.source_code_spec_and_methods(&p)?;
            let mut engine = json!({
                "name": name,
                "spec": {
                    "sourceCodeSpec": source_code_spec,
                    "classMethods": methods,
                },
            });
            let mut mask = vec!["spec.sourceCodeSpec", "spec.classMethods"];
            if self.agent_engine.update_env {
                engine["spec"]["deploymentSpec"] = deployment_spec(&self.agent_engine.env_vars);
                mask.push("spec.deploymentSpec.env");
            }
            if self.agent_engine.memory_bank.deploy {
                engine["contextSpec"] = self.memory_bank_context_spec();
                // Divergence (ledger 6b): Go's mask path `spec.context_spec` is kept (in
                // protojson form) although `contextSpec` is a top-level ReasoningEngine field.
                mask.push("spec.contextSpec");
            }

            p.print(&[&"Sending UpdateReasoningEngine request..."]);
            let op = client
                .update(engine, &mask)
                .await
                .map_err(|e| format!("UpdateReasoningEngine failed: {e}"))?;

            p.print(&[&"Waiting for operation to complete..."]);
            let re = wait_for_operation(&client, op).await?;

            p.print(&[&"Updated Reasoning Engine:", &str_field(&re, "name")]);
            p.print(&[&"Display Name:", &str_field(&re, "displayName")]);
            Ok(())
        })
        .await
    }

    /// Go `deployOnAgentEngine`: compute → Dockerfile → archive → update|deploy → clean,
    /// stopping at the first error (the temp dir is kept).
    pub async fn deploy_on_agent_engine(&mut self, env: &Env) -> Result<(), String> {
        self.compute_flags(env).await?;
        self.prepare_dockerfile(env).await?;
        self.create_archive(env).await?;
        if !self.agent_engine.agent_engine_id.is_empty() {
            self.update_agent_engine(env).await?;
        } else {
            self.deploy_to_agent_engine(env).await?;
        }
        self.clean_temp(env).await
    }
}
