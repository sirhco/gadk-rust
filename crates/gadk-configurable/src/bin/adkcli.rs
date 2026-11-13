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

//! Go `cmd/internal/adkcli/main.go`: registers the conformance callbacks, functions and node
//! functions on a fresh `Registry`, loads every `root_agent.yaml` under the working directory
//! (folder name = agent key), and runs the full launcher with the replay and record plugins.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use gadk_agent::Agent;
use gadk_configurable::conformance::{
    self, new_conformance_agent_loader, register_callbacks, register_functions,
    register_node_functions,
};
use gadk_configurable::{ConfigError, Registry, go_getwd};
use gadk_launcher::console::ConsoleLauncher;
use gadk_launcher::golog::Logger;
use gadk_launcher::universal::UniversalLauncher;
use gadk_launcher::web::triggers::{eventarc, pubsub};
use gadk_launcher::web::{a2a, api, webui};
use gadk_launcher::{CancellationToken, Config, Error, Launcher, SubLauncher};
use gadk_platform::{go_path_base, go_path_dir, go_path_join, go_quote};

/// The file name the walk collects.
const ROOT_AGENT_YAML: &str = "root_agent.yaml";

/// Go `log.Fatalf`: the message on stderr through Go's default logger, then exit 1.
fn fatal(log: &Logger, msg: &str) -> ! {
    log.print(msg);
    std::process::exit(1)
}

/// Go's `main` returning ends the process with any goroutine still blocked (a replay waiter
/// whose turn never comes, see `conformance::replay`); a tokio runtime's drop would wait for
/// such a blocking task forever, so the runtime is built by hand and shut down without waiting.
fn main() {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => fatal(&Logger::stderr(), &format!("Error starting runtime: {e}")),
    };
    rt.block_on(run());
    rt.shutdown_background();
}

/// The body of Go's `main`.
async fn run() {
    // Go: cmd/internal/adkcli/main.go main
    let log = Logger::stderr();

    // 1. Get the Current Working Directory (where the user typed 'adk')
    let cwd = match go_getwd() {
        Ok(cwd) => cwd.to_string_lossy().into_owned(),
        Err(e) => fatal(&log, &format!("Error getting current directory: {e}")),
    };

    // Go registers on its package globals; gadk's registry is a value (one per process here).
    let registry = Registry::new();
    // Register callbacks for the conformance agents
    if let Err(e) = register_callbacks(&registry) {
        fatal(&log, &format!("Error registering callbacks: {e}"));
    }
    if let Err(e) = register_functions(&registry) {
        fatal(&log, &format!("Error registering functions: {e}"));
    }
    if let Err(e) = register_node_functions(&registry) {
        fatal(&log, &format!("Error registering node functions: {e}"));
    }

    println!("🔍 Scanning for '{ROOT_AGENT_YAML}' in: {cwd}");

    // 2. Crawl folder structure to find all configs
    let mut agent_configs: Vec<String> = Vec::new();
    walk_dir(&cwd, &mut |path, entry| {
        match entry {
            // Report error but continue walking other files
            Err(err) => println!("Warning: skipping {} due to error: {err}", go_quote(path)),
            // Check if it matches the filename we are looking for
            Ok(d) => {
                if !d.is_dir && d.name == ROOT_AGENT_YAML {
                    agent_configs.push(path.to_owned());
                }
            }
        }
    });
    // Go's `log.Fatalf("Error walking the path: %v", err)` is unreachable: the callback never
    // returns an error, and `filepath.WalkDir` reports every failure through it.

    // 3. Check if we found anything
    if agent_configs.is_empty() {
        println!("❌ No '{ROOT_AGENT_YAML}' files found in {cwd} or subdirectories");
        std::process::exit(1);
    }

    println!("🚀 Found {} agent config(s)", agent_configs.len());
    let mut agents_map: HashMap<String, Arc<dyn Agent>> =
        HashMap::with_capacity(agent_configs.len());
    // 4. Iterate and Load all agents found
    for config_path in &agent_configs {
        println!("➡️  Loading agent from: {config_path}");

        // This reads the YAML, finds the 'agent_class', and calls the registered factory.
        let my_agent = match registry.from_config(Path::new(config_path)).await {
            Ok(a) => a,
            Err(e) => {
                log.print(&format!("⚠️  Error loading agent at {config_path}: {e}"));
                continue; // Skip this one and try the next
            }
        };
        println!("✅ Agent loaded successfully: {}", my_agent.name());

        let folder_name = go_path_base(&go_path_dir(config_path));
        println!("✅ Agent folder name: {folder_name}");

        if agents_map.contains_key(&folder_name) {
            log.print(&format!("⚠️  Agent {folder_name} already exists, skipping"));
            continue;
        }
        agents_map.insert(folder_name, my_agent);
    }

    let no_agents = agents_map.is_empty();
    let loader = match new_conformance_agent_loader(agents_map) {
        Ok(l) => l,
        Err(e) => fatal(&log, &format!("Error loading agent: {e}")),
    };

    let mut config = Config::new(loader);
    config.plugin_config.plugins = vec![
        conformance::replay::new(&cwd),
        conformance::record::new(&cwd),
    ];

    let mut l = if no_agents {
        nil_root_full_launcher()
    } else {
        gadk_launcher::full::new()
    };
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Err(err) = l.execute(CancellationToken::new(), config, args).await {
        fatal(
            &log,
            &format!("Run failed: {err}\n\n{}", l.command_line_syntax()),
        );
    }
}

/// What Go's `fs.WalkDirFunc` is handed for an entry: its name and whether it is a directory
/// (`fs.DirEntry.Name` / `IsDir`; symlinks are not followed).
struct DirEntry {
    name: String,
    is_dir: bool,
}

/// Go `filepath.WalkDir(root, fn)` for a callback that never returns an error (adkcli's): the
/// root, then each directory's entries in lexical (byte) order, depth first. `fn(path, Err)`
/// reports an `lstat` failure of the root, and a directory's `ReadDir` failure after the
/// directory itself was visited (Go's second call); the walk then goes on with the entries
/// read before the error.
fn walk_dir(root: &str, f: &mut dyn FnMut(&str, Result<&DirEntry, ConfigError>)) {
    // Go: path/filepath/path.go WalkDir
    match std::fs::symlink_metadata(root) {
        Err(e) => f(root, Err(ConfigError::path("lstat", Path::new(root), &e))),
        Ok(info) => {
            let d = DirEntry {
                name: go_path_base(root),
                is_dir: info.is_dir(),
            };
            walk(root, &d, f);
        }
    }
}

/// Go `filepath.walkDir`.
fn walk(path: &str, d: &DirEntry, f: &mut dyn FnMut(&str, Result<&DirEntry, ConfigError>)) {
    // Go: path/filepath/path.go walkDir
    f(path, Ok(d));
    if !d.is_dir {
        return;
    }
    let (dirs, err) = read_dir(path);
    if let Some(err) = err {
        // Second call, to report ReadDir error.
        f(path, Err(err));
    }
    for d1 in &dirs {
        let path1 = go_path_join(&[path, d1.name.as_str()]);
        walk(&path1, d1, f);
    }
}

/// Go `os.ReadDir`: the entries sorted by name, plus the error that stopped the read (`open`
/// when the directory cannot be opened; the entries read before a later failure are kept).
fn read_dir(path: &str) -> (Vec<DirEntry>, Option<ConfigError>) {
    let rd = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => {
            return (
                Vec::new(),
                Some(ConfigError::path("open", Path::new(path), &e)),
            );
        }
    };
    let mut dirs = Vec::new();
    let mut err = None;
    for entry in rd {
        match entry {
            Ok(entry) => {
                // Go's `ReadDir` skips an entry that vanished between the read and its lstat.
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                dirs.push(DirEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_dir: file_type.is_dir(),
                });
            }
            Err(e) => {
                err = Some(read_dir_error(path, &e));
                break;
            }
        }
    }
    dirs.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    (dirs, err)
}

/// A failure while reading directory entries: Go's `readdirent` `*PathError`.
/// Divergence (ledger 6c-2): Go's op name for a failed directory read after a successful open
/// is platform-specific (only the `open` failure is verified byte-identical).
fn read_dir_error(path: &str, e: &io::Error) -> ConfigError {
    ConfigError::path("readdirent", Path::new(path), e)
}

/// Go's `full.NewLauncher()` with the console replaced by [`NilRootConsole`]: the launcher
/// adkcli runs when no agent loaded, so the loader has no root agent.
fn nil_root_full_launcher() -> UniversalLauncher {
    UniversalLauncher::new(vec![
        Box::new(NilRootConsole(gadk_launcher::console::new())),
        Box::new(gadk_launcher::web::new(vec![
            Box::new(webui::new()),
            Box::new(a2a::new()),
            Box::new(pubsub::new()),
            Box::new(eventarc::new()),
            Box::new(api::new()),
        ])),
    ])
}

/// Go's console sub-launcher over a loader whose `RootAgent()` is nil (an empty agent map).
///
/// Go's console `Run` hands the nil root agent to `runner.New`, which fails with
/// `root agent is required`, so `Run` returns `failed to create runner: root agent is
/// required`. gadk's `Loader::root_agent` cannot return nil (the conformance loader panics on
/// an empty map), so this wrapper returns Go's error instead of running the console; keyword,
/// flags and syntax are the console's own.
///
/// Divergence (ledger 6c-2): Go initializes telemetry (`-otel_to_cloud`) and creates the
/// console session before `runner.New` fails; the wrapper fails first, so a telemetry
/// initialization failure that Go would report instead is not reported.
struct NilRootConsole(ConsoleLauncher);

impl SubLauncher for NilRootConsole {
    fn keyword(&self) -> String {
        self.0.keyword()
    }

    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        self.0.parse(args)
    }

    fn command_line_syntax(&self) -> String {
        SubLauncher::command_line_syntax(&self.0)
    }

    fn simple_description(&self) -> String {
        self.0.simple_description()
    }

    fn run<'life0, 'async_trait>(
        &'life0 self,
        _cancel: CancellationToken,
        _config: Config,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        // Go: cmd/launcher/console/console.go Run → runner.New (runner/runner.go New)
        Box::pin(async {
            Err(Error::Wrapped {
                context: "failed to create runner".to_owned(),
                source: Box::new(ConfigError::msg("root agent is required")),
            })
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn nil_root_launcher_has_the_full_launchers_syntax() {
        assert_eq!(
            nil_root_full_launcher().command_line_syntax(),
            gadk_launcher::full::new().command_line_syntax()
        );
    }

    #[tokio::test]
    async fn nil_root_console_fails_like_gos_runner_new() {
        let loader = new_conformance_agent_loader(HashMap::new()).unwrap();
        let mut l = nil_root_full_launcher();
        let err = l
            .execute(CancellationToken::new(), Config::new(loader), Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to create runner: root agent is required"
        );
        // Console flags still parse as the console's: an unknown one fails the same way.
        let loader = new_conformance_agent_loader(HashMap::new()).unwrap();
        let err = nil_root_full_launcher()
            .execute(
                CancellationToken::new(),
                Config::new(loader),
                vec![
                    "console".to_owned(),
                    "-streaming_mode".to_owned(),
                    "x".to_owned(),
                ],
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid streaming_mode: x. Should be (none|sse)"
        );
    }

    /// Records every callback of a walk: `path` or `path!error`.
    fn visits(root: &str) -> Vec<String> {
        let mut out = Vec::new();
        walk_dir(root, &mut |path, entry| match entry {
            Ok(d) => out.push(format!("{path}{}", if d.is_dir { "/" } else { "" })),
            Err(e) => out.push(format!("{path}!{e}")),
        });
        out
    }

    #[cfg(unix)]
    #[test]
    fn walk_dir_visits_in_gos_lexical_preorder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_string_lossy().into_owned();
        for f in ["b/x", "a/z", "a/B/y", "_", "Z"] {
            let p = tmp.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        std::os::unix::fs::symlink(tmp.path().join("a"), tmp.path().join("ln")).unwrap();
        // Directories carry a `/` suffix here; `ln` (a symlink to a directory) is not followed.
        let want: Vec<String> = [
            "/", "/Z", "/_", "/a/", "/a/B/", "/a/B/y", "/a/z", "/b/", "/b/x", "/ln",
        ]
        .iter()
        .map(|s| format!("{root}{s}"))
        .collect();
        assert_eq!(visits(&root), want);
    }

    #[cfg(unix)]
    #[test]
    fn walk_dir_reports_root_lstat_and_read_dir_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing").to_string_lossy().into_owned();
        assert_eq!(
            visits(&missing),
            [format!(
                "{missing}!lstat {missing}: no such file or directory"
            )]
        );
        // A symlinked root is not followed (Go's Lstat): visited once, as a non-directory.
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        std::fs::write(tmp.path().join("d/f"), "").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("d"), tmp.path().join("l")).unwrap();
        let l = tmp.path().join("l").to_string_lossy().into_owned();
        assert_eq!(visits(&l), std::slice::from_ref(&l));
    }
}
