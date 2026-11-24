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

//! The `gadk-prompts` command-line tool, as a library for testing.

mod commands;
mod store_url;

pub use store_url::open_store;

use std::io::{Read, Write};

use clap::{Arg, ArgAction, ArgGroup, Command};
use gadk_prompts::StoreError;

fn cli() -> Command {
    let name = || Arg::new("name").required(true);
    let label = || Arg::new("label").long("label").required(true);
    Command::new("gadk-prompts")
        .about("Manage gadk prompt versions and labels")
        .arg(
            Arg::new("store")
                .long("store")
                .global(true)
                .help("file://<dir> or firestore://<project>/<database>[/<root>]"),
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("push")
                .about("Create a new version (no-op when unchanged)")
                .arg(name())
                .arg(
                    Arg::new("template")
                        .long("template")
                        .help("template file, or - for stdin (default)"),
                )
                .arg(Arg::new("config").long("config").help("JSON config file"))
                .arg(Arg::new("message").short('m').long("message"))
                .arg(Arg::new("author").long("author")),
        )
        .subcommand(
            Command::new("promote")
                .about("Point a label at a version")
                .arg(name())
                .arg(Arg::new("version").required(true))
                .arg(label())
                .arg(
                    Arg::new("expect")
                        .long("expect")
                        .help("required current version, or none"),
                ),
        )
        .subcommand(
            Command::new("unlabel")
                .about("Remove a label")
                .arg(name())
                .arg(label()),
        )
        .subcommand(
            Command::new("rollout")
                .about("Start, change, promote or abort a label's rollout")
                .arg(name())
                .arg(
                    Arg::new("version")
                        .requires("percent")
                        .conflicts_with_all(["promote", "abort"])
                        .help("rollout version (with --percent)"),
                )
                .arg(label())
                .arg(
                    Arg::new("percent")
                        .long("percent")
                        .requires("version")
                        .help("percent of sessions served the rollout version, 1-99"),
                )
                .arg(
                    Arg::new("promote")
                        .long("promote")
                        .action(ArgAction::SetTrue)
                        .help("move the label to its rollout version"),
                )
                .arg(
                    Arg::new("abort")
                        .long("abort")
                        .action(ArgAction::SetTrue)
                        .help("remove the label's rollout"),
                )
                .group(
                    ArgGroup::new("mode")
                        .args(["percent", "promote", "abort"])
                        .required(true),
                ),
        )
        .subcommand(
            Command::new("list")
                .about("List prompts")
                .arg(Arg::new("prefix").long("prefix")),
        )
        .subcommand(
            Command::new("show")
                .about("Show a version (latest by default)")
                .arg(name())
                .arg(Arg::new("label").long("label").conflicts_with("version"))
                .arg(Arg::new("version").long("version"))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("diff")
                .about("Unified diff of two versions (v<n>, a label, or latest)")
                .arg(name())
                .arg(Arg::new("from").required(true))
                .arg(Arg::new("to").required(true)),
        )
}

/// The exit code of a store error.
fn code(e: &StoreError) -> i32 {
    match e {
        StoreError::Invalid(_) => 1,
        StoreError::NotFound { .. } => 2,
        StoreError::Conflict { .. } => 3,
        _ => 4,
    }
}

/// Runs the tool with `args` (including the program name) and returns the exit code:
/// 0 ok, 1 usage or invalid input, 2 not found, 3 conflict, 4 store or IO error.
pub async fn run(
    args: Vec<String>,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let m = match cli().try_get_matches_from(args) {
        Ok(m) => m,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = write!(stdout, "{}", e.render());
                return 0;
            }
            let _ = write!(stderr, "gadk-prompts: {}", e.render());
            return 1;
        }
    };
    let result = async {
        // clap forbids required global args, so --store is checked here.
        let url = m
            .get_one::<String>("store")
            .cloned()
            .ok_or_else(|| StoreError::Invalid("--store is required".into()))?;
        let store = store_url::open_store(&url)?;
        commands::dispatch(&*store, &m, stdin, stdout).await
    }
    .await;
    match result {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(stderr, "gadk-prompts: {e}");
            code(&e)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use gadk_prompts::{
        FileStore, Label, NewVersion, PromptSnapshot, PromptStore, PromptVersion, Rollout,
        RolloutSnapshot, Selector, StoreError,
    };

    /// A store that counts `load_all` calls.
    struct Counting {
        inner: FileStore,
        loads: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl PromptStore for Counting {
        async fn load_all(&self) -> Result<PromptSnapshot, StoreError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.inner.load_all().await
        }
        async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError> {
            self.inner.create_version(new).await
        }
        async fn create_version_reporting(
            &self,
            new: NewVersion,
        ) -> Result<(PromptVersion, bool), StoreError> {
            self.inner.create_version_reporting(new).await
        }
        async fn set_label(
            &self,
            name: &str,
            label: &Label,
            version: u32,
            expected: Option<Option<u32>>,
        ) -> Result<(), StoreError> {
            self.inner.set_label(name, label, version, expected).await
        }
        async fn remove_label(&self, name: &str, label: &Label) -> Result<(), StoreError> {
            self.inner.remove_label(name, label).await
        }
    }

    async fn push(store: &Counting, template: &str) -> String {
        let m = super::cli()
            .try_get_matches_from(["gadk-prompts", "push", "p", "--author", "me"])
            .unwrap();
        let mut out = Vec::new();
        super::commands::dispatch(store, &m, &mut template.as_bytes(), &mut out)
            .await
            .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[tokio::test]
    async fn push_does_not_read_the_whole_store() {
        let dir = tempfile::tempdir().unwrap();
        let loads = Arc::new(AtomicUsize::new(0));
        let store = Counting {
            inner: FileStore::new(dir.path()),
            loads: loads.clone(),
        };
        assert_eq!(push(&store, "hello").await, "p v1\n");
        assert_eq!(push(&store, "hello").await, "p v1 (unchanged)\n");
        assert_eq!(push(&store, "bye").await, "p v2\n");
        assert_eq!(loads.load(Ordering::SeqCst), 0);
    }

    /// A store that counts [`PromptStore::load_prompt`] calls apart from the whole-store reads
    /// (`load_all` and `load_all_with_rollouts`).
    struct PromptReads {
        inner: FileStore,
        prompt: AtomicUsize,
        whole: AtomicUsize,
    }

    #[async_trait]
    impl PromptStore for PromptReads {
        async fn load_all(&self) -> Result<PromptSnapshot, StoreError> {
            self.whole.fetch_add(1, Ordering::SeqCst);
            self.inner.load_all().await
        }
        async fn load_all_with_rollouts(
            &self,
        ) -> Result<(PromptSnapshot, RolloutSnapshot), StoreError> {
            self.whole.fetch_add(1, Ordering::SeqCst);
            self.inner.load_all_with_rollouts().await
        }
        async fn load_prompt(
            &self,
            name: &str,
        ) -> Result<(PromptSnapshot, RolloutSnapshot), StoreError> {
            self.prompt.fetch_add(1, Ordering::SeqCst);
            self.inner.load_prompt(name).await
        }
        async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError> {
            self.inner.create_version(new).await
        }
        async fn set_label(
            &self,
            name: &str,
            label: &Label,
            version: u32,
            expected: Option<Option<u32>>,
        ) -> Result<(), StoreError> {
            self.inner.set_label(name, label, version, expected).await
        }
        async fn remove_label(&self, name: &str, label: &Label) -> Result<(), StoreError> {
            self.inner.remove_label(name, label).await
        }
        async fn set_rollout(
            &self,
            name: &str,
            label: &Label,
            rollout: Option<Rollout>,
        ) -> Result<(), StoreError> {
            self.inner.set_rollout(name, label, rollout).await
        }
    }

    #[tokio::test]
    async fn single_prompt_commands_read_only_that_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileStore::new(dir.path());
        for t in ["one\n", "two\n", "three\n"] {
            fs.create_version(NewVersion {
                name: "p".into(),
                template: t.into(),
                config: None,
                author: "me".into(),
                message: String::new(),
            })
            .await
            .unwrap();
        }
        let prod = Label::production();
        fs.set_label("p", &prod, 1, None).await.unwrap();
        fs.set_rollout("p", &prod, Some(Rollout::new(2, 10).unwrap()))
            .await
            .unwrap();
        let store = PromptReads {
            inner: fs,
            prompt: AtomicUsize::new(0),
            whole: AtomicUsize::new(0),
        };
        let cases: [(&[&str], &str); 4] = [
            (
                &["show", "p", "--label", "production"],
                "one\nrollout: v2 at 10%\n",
            ),
            (&["diff", "p", "v1", "v2"], "-one\n+two\n"),
            (
                &[
                    "rollout",
                    "p",
                    "3",
                    "--label",
                    "production",
                    "--percent",
                    "20",
                ],
                "p production -> v1 (v3 at 20%)\n",
            ),
            (
                &["rollout", "p", "--label", "production", "--promote"],
                "p production -> v3\n",
            ),
        ];
        for (i, (args, tail)) in cases.into_iter().enumerate() {
            let m = super::cli()
                .try_get_matches_from(std::iter::once("gadk-prompts").chain(args.iter().copied()))
                .unwrap();
            let mut out = Vec::new();
            super::commands::dispatch(&store, &m, &mut "".as_bytes(), &mut out)
                .await
                .unwrap();
            let out = String::from_utf8(out).unwrap();
            assert!(out.ends_with(tail), "{args:?}: {out}");
            assert_eq!(store.prompt.load(Ordering::SeqCst), i + 1, "{args:?}");
            assert_eq!(store.whole.load(Ordering::SeqCst), 0, "{args:?}");
        }
    }

    /// A store where another writer moves `production` to v3 right after every
    /// `load_all_with_rollouts` read, which the default `load_prompt` makes, i.e. between
    /// `rollout --promote`'s `load_prompt` and its write.
    struct MovedAfterRead(FileStore);

    #[async_trait]
    impl PromptStore for MovedAfterRead {
        async fn load_all(&self) -> Result<PromptSnapshot, StoreError> {
            self.0.load_all().await
        }
        async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError> {
            self.0.create_version(new).await
        }
        async fn set_label(
            &self,
            name: &str,
            label: &Label,
            version: u32,
            expected: Option<Option<u32>>,
        ) -> Result<(), StoreError> {
            self.0.set_label(name, label, version, expected).await
        }
        async fn remove_label(&self, name: &str, label: &Label) -> Result<(), StoreError> {
            self.0.remove_label(name, label).await
        }
        async fn load_all_with_rollouts(
            &self,
        ) -> Result<(PromptSnapshot, RolloutSnapshot), StoreError> {
            let read = self.0.load_all_with_rollouts().await?;
            self.0.set_label("p", &Label::production(), 3, None).await?;
            Ok(read)
        }
    }

    #[tokio::test]
    async fn promote_is_compare_and_set_on_the_label_version_read() {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileStore::new(dir.path());
        for t in ["one", "two", "three"] {
            fs.create_version(NewVersion {
                name: "p".into(),
                template: t.into(),
                config: None,
                author: "me".into(),
                message: String::new(),
            })
            .await
            .unwrap();
        }
        let prod = Label::production();
        fs.set_label("p", &prod, 1, None).await.unwrap();
        fs.set_rollout("p", &prod, Some(Rollout::new(2, 10).unwrap()))
            .await
            .unwrap();
        let store = MovedAfterRead(fs);
        let m = super::cli()
            .try_get_matches_from([
                "gadk-prompts",
                "rollout",
                "p",
                "--label",
                "production",
                "--promote",
            ])
            .unwrap();
        let mut out = Vec::new();
        let err = super::commands::dispatch(&store, &m, &mut "".as_bytes(), &mut out)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::Conflict {
                    expected: Some(1),
                    actual: Some(3)
                }
            ),
            "{err:?}"
        );
        assert_eq!(super::code(&err), 3);
        assert!(out.is_empty());
        let snap = store.0.load_all().await.unwrap();
        assert_eq!(
            snap.resolve("p", &Selector::Label(prod)).unwrap().version,
            3
        );
    }
}
