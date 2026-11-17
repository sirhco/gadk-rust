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

//! The Agent Engine deployment environment (`--env NAME=VALUE`, `--default_env`).
//!
//! `--env` values are kept as [`OsString`]s (the argument bytes) so a value that is not
//! valid UTF-8 reaches [`AgentEngineFlags::environment_variables`] and gets ADK's error text.
//! No error names an entry's name or value: a malformed entry may hold a secret.

use std::ffi::OsString;

use clap::{Arg, ArgAction, Command};
use serde_json::{Value, json};

use super::AgentEngineFlags;
use crate::flags::bool_flag;

/// The help text of `--env`.
pub const ENV_HELP: &str = "Environment variable NAME=VALUE (repeat --env for each variable). Overrides a default with the same name. On update, replaces the entire environment, including the defaults unless --default_env=false is set.";
/// The help text of `--default_env`.
pub const DEFAULT_ENV_HELP: &str = "Include GOOGLE_CLOUD_REGION, NUM_WORKERS=1, GOOGLE_CLOUD_AGENT_ENGINE_ENABLE_TELEMETRY=true and OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true. Use --default_env=false to omit them. Explicitly setting this flag replaces the environment on update; omit both environment flags to preserve it.";

/// One variable of the deployment environment. `Debug` shows the
/// name only: values may be secrets.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}

impl std::fmt::Debug for EnvVar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvVar")
            .field("name", &self.name)
            .field("value", &format_args!("<redacted>"))
            .finish()
    }
}

impl EnvVar {
    fn new(name: &str, value: &str) -> EnvVar {
        EnvVar {
            name: name.to_string(),
            value: value.to_string(),
        }
    }
}

/// Registers the environment flags: `--env` (repeatable,
/// one value per occurrence, kept verbatim — no comma splitting) and
/// `--default_env` (default true; a value only as `--default_env=<v>`).
pub fn register_environment_flags(cmd: Command) -> Command {
    // Repeated --env flags are used rather than Python's agent_config env_vars.
    cmd.arg(
        Arg::new("env")
            .long("env")
            .help(ENV_HELP)
            .value_name("stringArray")
            .action(ArgAction::Append)
            .num_args(1)
            .allow_hyphen_values(true)
            .value_parser(clap::value_parser!(OsString)),
    )
    .arg(bool_flag("default_env", true, DEFAULT_ENV_HELP))
}

/// The pattern of an environment variable name: `^[a-zA-Z_][a-zA-Z0-9_]*$`.
fn is_environment_name(name: &[u8]) -> bool {
    match name.split_first() {
        Some((first, rest)) => {
            (first.is_ascii_alphabetic() || *first == b'_')
                && rest.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
        }
        None => false,
    }
}

impl AgentEngineFlags {
    /// Validates every `--env` entry (form `NAME=VALUE`, a name
    /// matching `[a-zA-Z_][a-zA-Z0-9_]*`, no repeated name, a valid UTF-8 value), then returns
    /// the defaults not overridden by an entry (when `--default_env`) followed by the entries
    /// in flag order. Errors name the 1-based entry, never its contents.
    pub fn environment_variables(&self) -> Result<Vec<EnvVar>, String> {
        let mut custom: Vec<EnvVar> = Vec::with_capacity(self.agent_engine.env.len());
        for (i, raw) in self.agent_engine.env.iter().enumerate() {
            let n = i + 1;
            let bytes = raw.as_encoded_bytes();
            let split = bytes
                .iter()
                .position(|&b| b == b'=')
                .map(|at| (&bytes[..at], &bytes[at + 1..]));
            let Some((name, value)) = split.filter(|(name, _)| is_environment_name(name)) else {
                // Do not echo malformed input: it may contain a secret, including when the
                // user accidentally omitted NAME=.
                return Err(format!(
                    "--env entry {n} must have the form NAME=VALUE with a name matching [a-zA-Z_][a-zA-Z0-9_]*"
                ));
            };
            // The name is ASCII (checked above).
            let name = String::from_utf8_lossy(name);
            if custom.iter().any(|e| e.name == name) {
                return Err(format!(
                    "--env entry {n} repeats an environment variable name"
                ));
            }
            let Ok(value) = std::str::from_utf8(value) else {
                return Err(format!("--env entry {n} must have a valid UTF-8 value"));
            };
            custom.push(EnvVar::new(&name, value));
        }

        let mut vars = Vec::new();
        if self.agent_engine.default_env {
            let defaults = [
                EnvVar::new("GOOGLE_CLOUD_REGION", &self.gcloud.region),
                EnvVar::new("NUM_WORKERS", "1"),
                EnvVar::new("GOOGLE_CLOUD_AGENT_ENGINE_ENABLE_TELEMETRY", "true"),
                EnvVar::new("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "true"),
            ];
            vars.extend(
                defaults
                    .into_iter()
                    .filter(|d| !custom.iter().any(|e| e.name == d.name)),
            );
        }
        // EnvVar expands references to previously defined variables. Keep custom flags in
        // their original order, including overrides of a default.
        vars.extend(custom);
        Ok(vars)
    }
}

/// The JSON form of the `ReasoningEngineSpec.DeploymentSpec` holding `vars` as `env`:
/// an empty list is left out (`{}`) and so is an empty
/// `value`.
pub fn deployment_spec(vars: &[EnvVar]) -> Value {
    if vars.is_empty() {
        return json!({});
    }
    let env: Vec<Value> = vars
        .iter()
        .map(|v| {
            if v.value.is_empty() {
                json!({"name": v.name})
            } else {
                json!({"name": v.name, "value": v.value})
            }
        })
        .collect();
    json!({ "env": env })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn flags(env: &[&str], default_env: bool) -> AgentEngineFlags {
        let mut f = AgentEngineFlags::default();
        f.gcloud.region = "r".into();
        f.agent_engine.env = env.iter().map(OsString::from).collect();
        f.agent_engine.default_env = default_env;
        f
    }

    #[test]
    fn names_follow_go_pattern() {
        for ok in ["A", "_", "a_1", "_9", "Z_BASE"] {
            assert!(is_environment_name(ok.as_bytes()), "{ok}");
        }
        for bad in ["", "9A", "A-B", "A B", "é", "A.B"] {
            assert!(!is_environment_name(bad.as_bytes()), "{bad}");
        }
    }

    #[test]
    fn defaults_first_unless_overridden_then_customs_in_flag_order() {
        let got = flags(&["Z=5", "NUM_WORKERS=$(Z)", "E="], true)
            .environment_variables()
            .unwrap();
        assert_eq!(
            got,
            vec![
                EnvVar::new("GOOGLE_CLOUD_REGION", "r"),
                EnvVar::new("GOOGLE_CLOUD_AGENT_ENGINE_ENABLE_TELEMETRY", "true"),
                EnvVar::new("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "true"),
                EnvVar::new("Z", "5"),
                EnvVar::new("NUM_WORKERS", "$(Z)"),
                EnvVar::new("E", ""),
            ]
        );
        assert_eq!(flags(&[], false).environment_variables(), Ok(vec![]));
        // The value is everything after the first `=`.
        assert_eq!(
            flags(&["A=b=c"], false).environment_variables(),
            Ok(vec![EnvVar::new("A", "b=c")])
        );
    }

    #[test]
    fn errors_are_go_texts_and_never_echo_the_entry() {
        let form = "must have the form NAME=VALUE with a name matching [a-zA-Z_][a-zA-Z0-9_]*";
        for (env, want) in [
            (vec!["SECRET"], format!("--env entry 1 {form}")),
            (vec!["A=1", "=SECRET"], format!("--env entry 2 {form}")),
            (
                vec!["A=SECRET", "B=1", "A=SECRET"],
                "--env entry 3 repeats an environment variable name".to_string(),
            ),
        ] {
            let got = flags(&env, true).environment_variables().unwrap_err();
            assert_eq!(got, want);
            assert!(!got.contains("SECRET"));
        }
        use std::os::unix::ffi::OsStringExt;
        let mut f = flags(&["A=1"], true);
        f.agent_engine
            .env
            .push(OsString::from_vec(b"B=SECRET\xff".to_vec()));
        assert_eq!(
            f.environment_variables(),
            Err("--env entry 2 must have a valid UTF-8 value".to_string())
        );
    }

    #[test]
    fn deployment_spec_is_protojson() {
        assert_eq!(deployment_spec(&[]), json!({}));
        assert_eq!(
            deployment_spec(&[EnvVar::new("A", "1"), EnvVar::new("_empty", "")]),
            json!({"env": [{"name": "A", "value": "1"}, {"name": "_empty"}]})
        );
    }

    /// `--env` values may be secrets: `Debug` shows the names only.
    #[test]
    fn debug_redacts_env_values() {
        let mut f = flags(&["API_KEY=s3cret-value", "bare-s3cret-value"], true);
        f.agent_engine.env_vars = vec![EnvVar::new("API_KEY", "s3cret-value")];
        let got = format!("{f:?}");
        assert!(!got.contains("s3cret-value"), "{got}");
        assert!(got.contains("API_KEY"), "{got}");
        assert!(
            got.contains(r#"env: ["API_KEY=<redacted>", "<redacted>"]"#),
            "{got}"
        );
        let got = format!("{:?}", EnvVar::new("TOKEN", "s3cret-value"));
        assert_eq!(got, r#"EnvVar { name: "TOKEN", value: <redacted> }"#);
    }
}
