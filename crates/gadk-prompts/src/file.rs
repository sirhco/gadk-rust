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

//! Prompts in YAML files: `<root>/<name>.yaml`, `/` in a name being a subdirectory.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::model::{Label, NewVersion, PromptVersion, VersionMeta, content_hash, validate_name};
use crate::rollout::{Rollout, RolloutSnapshot};
use crate::snapshot::{PromptEntry, PromptSnapshot};
use crate::store::{PromptStore, StoreError};

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// The on-disk document of one prompt.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PromptFile {
    name: String,
    #[serde(default)]
    labels: BTreeMap<Label, u32>,
    #[serde(default)]
    versions: Vec<PromptVersion>,
    /// Each label's rollout; omitted from the file when there are none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    rollouts: BTreeMap<Label, Rollout>,
}

impl PromptFile {
    fn entry(&self) -> PromptEntry {
        PromptEntry {
            versions: self
                .versions
                .iter()
                .map(|v| (v.version, Arc::new(v.clone())))
                .collect(),
            labels: self.labels.clone(),
        }
    }
}

/// A [`PromptStore`] in a directory of YAML files. Writes are atomic (temp file + rename) and
/// serialised per prompt by an exclusive `<file>.lock`; a lock left by a crashed writer must be
/// removed by hand (the timeout error names it). For many concurrent writers prefer a
/// transactional store.
///
/// Each label's rollout is kept in the prompt's file under `rollouts` (no key when there are
/// none).
///
/// `load_all` re-parses only files whose (mtime, length, inode) changed since the previous call.
/// Every write renames a new file into place, so on unix the inode changes even when a rewrite
/// keeps the length within one mtime tick; elsewhere the inode is taken as 0 and the check falls
/// back to (mtime, length).
#[derive(Debug)]
pub struct FileStore {
    root: PathBuf,
    /// (file key, parsed file) per path, from the previous load.
    cache: Mutex<HashMap<PathBuf, (FileKey, PromptFile)>>,
}

/// What identifies an unchanged file between two loads: (mtime, length, inode).
type FileKey = (SystemTime, u64, u64);

#[cfg(unix)]
fn inode(md: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(md)
}

#[cfg(not(unix))]
fn inode(_md: &std::fs::Metadata) -> u64 {
    0
}

/// `path` with `suffix` appended to its final component (lossless for non-UTF-8 paths).
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

impl FileStore {
    /// A store rooted at `root` (created on first write).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// `<root>/<seg>/…/<last>.yaml`. The `.yaml` is appended, never `set_extension`: names may
    /// contain dots (`x.v1` and `x.v2` are different prompts).
    fn path_of(&self, name: &str) -> PathBuf {
        let mut p = self.root.clone();
        let segs: Vec<&str> = name.split('/').collect();
        for (i, seg) in segs.iter().enumerate() {
            if i + 1 == segs.len() {
                p.push(format!("{seg}.yaml"));
            } else {
                p.push(seg);
            }
        }
        p
    }

    async fn locked<T>(
        &self,
        name: &str,
        f: impl FnOnce(&Path, Option<PromptFile>) -> Result<(Option<PromptFile>, T), StoreError>,
    ) -> Result<T, StoreError> {
        validate_name(name)?;
        let path = self.path_of(name);
        if let Some(dir) = path.parent() {
            // std, not tokio::fs: no blocking-pool hop, so paused-time tests stay deterministic.
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        }
        let lock = sidecar(&path, ".lock");
        let started = tokio::time::Instant::now();
        let mut backoff = Duration::from_millis(10);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
            {
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if started.elapsed() >= LOCK_TIMEOUT {
                        return Err(StoreError::Io(format!(
                            "timed out after {LOCK_TIMEOUT:?} waiting for lock {} (remove it if no writer is running)",
                            lock.display()
                        )));
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_millis(200));
                }
                Err(e) => return Err(io(&lock, e)),
            }
        }
        let result = (|| {
            let current = read_file(&path)?;
            let (write, out) = f(&path, current)?;
            if let Some(doc) = write {
                write_atomic(&path, &doc)?;
            }
            Ok(out)
        })();
        let _ = std::fs::remove_file(&lock);
        result
    }

    /// [`locked`](Self::locked) on an existing prompt: checks `rollout`'s bounds first (when it
    /// is `Some`), then runs `f` on the prompt's document; an unknown prompt is `NotFound` with
    /// an empty selector.
    async fn locked_doc<T>(
        &self,
        name: &str,
        rollout: Option<Rollout>,
        f: impl FnOnce(PromptFile) -> Result<(Option<PromptFile>, T), StoreError>,
    ) -> Result<T, StoreError> {
        if let Some(r) = rollout {
            Rollout::new(r.version, r.percent)?;
        }
        let name_s = name.to_owned();
        self.locked(name, move |_, current| {
            let doc = current.ok_or_else(|| StoreError::NotFound {
                name: name_s,
                selector: String::new(),
            })?;
            f(doc)
        })
        .await
    }
}

fn io(path: &Path, e: std::io::Error) -> StoreError {
    StoreError::Io(format!("{}: {e}", path.display()))
}

fn read_file(path: &Path) -> Result<Option<PromptFile>, StoreError> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_saphyr::from_str(&s)
            .map(Some)
            .map_err(|e| StoreError::Decode(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(path, e)),
    }
}

fn write_atomic(path: &Path, doc: &PromptFile) -> Result<(), StoreError> {
    let yaml = serde_saphyr::to_string(doc).map_err(|e| StoreError::Decode(e.to_string()))?;
    let tmp = sidecar(path, ".tmp");
    std::fs::write(&tmp, yaml).map_err(|e| io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| io(path, e))
}

/// The prompt name of `path` relative to `root` (`a/b.yaml` → `a/b`).
fn name_of(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_str().map(str::to_owned))
        .collect::<Option<_>>()?;
    let last = parts.pop()?;
    parts.push(last.strip_suffix(".yaml")?.to_owned());
    Some(parts.join("/"))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for ent in rd {
        let p = ent?.path();
        if p.is_dir() {
            walk(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "yaml") {
            out.push(p);
        }
    }
    Ok(())
}

#[async_trait]
impl PromptStore for FileStore {
    async fn load_all(&self) -> Result<PromptSnapshot, StoreError> {
        self.load_all_with_rollouts().await.map(|(snap, _)| snap)
    }

    async fn load_rollouts(&self) -> Result<RolloutSnapshot, StoreError> {
        self.load_all_with_rollouts()
            .await
            .map(|(_, rollouts)| rollouts)
    }

    async fn load_all_with_rollouts(
        &self,
    ) -> Result<(PromptSnapshot, RolloutSnapshot), StoreError> {
        let mut files = Vec::new();
        walk(&self.root, &mut files).map_err(|e| io(&self.root, e))?;
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| StoreError::Io("cache poisoned".into()))?;
        let mut next = HashMap::new();
        let mut snap = PromptSnapshot::default();
        let mut rollouts = RolloutSnapshot::default();
        for path in files {
            let Some(name) = name_of(&self.root, &path) else {
                continue;
            };
            let md = std::fs::metadata(&path).map_err(|e| io(&path, e))?;
            let key: FileKey = (
                md.modified().map_err(|e| io(&path, e))?,
                md.len(),
                inode(&md),
            );
            let doc = match cache.get(&path) {
                Some((k, doc)) if *k == key => doc.clone(),
                _ => match read_file(&path)? {
                    Some(doc) => doc,
                    None => continue,
                },
            };
            for (label, rollout) in &doc.rollouts {
                rollouts.insert(name.clone(), label.clone(), *rollout);
            }
            snap.prompts.insert(name, doc.entry());
            next.insert(path, (key, doc));
        }
        *cache = next;
        Ok((snap, rollouts))
    }

    async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError> {
        self.create_version_reporting(new).await.map(|(v, _)| v)
    }

    async fn create_version_reporting(
        &self,
        new: NewVersion,
    ) -> Result<(PromptVersion, bool), StoreError> {
        let name = new.name.clone();
        self.locked(&name, move |_, current| {
            let mut doc = current.unwrap_or_else(|| PromptFile {
                name: new.name.clone(),
                ..Default::default()
            });
            let hash = content_hash(&new.template, new.config.as_ref());
            if let Some(last) = doc.versions.last()
                && last.meta.hash == hash
            {
                return Ok((None, (last.clone(), false)));
            }
            let version = doc.versions.last().map_or(1, |v| v.version + 1);
            let v = PromptVersion {
                name: new.name,
                version,
                template: new.template,
                config: new.config,
                meta: VersionMeta {
                    created_at: chrono::Utc::now(),
                    author: new.author,
                    message: new.message,
                    hash,
                },
            };
            doc.versions.push(v.clone());
            Ok((Some(doc), (v, true)))
        })
        .await
    }

    async fn set_label(
        &self,
        name: &str,
        label: &Label,
        version: u32,
        expected: Option<Option<u32>>,
    ) -> Result<(), StoreError> {
        let (name_s, label) = (name.to_owned(), label.clone());
        self.locked_doc(name, None, move |doc| {
            move_label(doc, name_s, label, version, expected)
        })
        .await
    }

    async fn remove_label(&self, name: &str, label: &Label) -> Result<(), StoreError> {
        let (name_s, label) = (name.to_owned(), label.clone());
        self.locked_doc(name, None, move |mut doc| {
            if doc.labels.remove(&label).is_none() {
                return Err(StoreError::NotFound {
                    name: name_s,
                    selector: label.to_string(),
                });
            }
            doc.rollouts.remove(&label);
            Ok((Some(doc), ()))
        })
        .await
    }

    async fn set_rollout(
        &self,
        name: &str,
        label: &Label,
        rollout: Option<Rollout>,
    ) -> Result<(), StoreError> {
        let (name_s, label) = (name.to_owned(), label.clone());
        self.locked_doc(name, rollout, move |doc| {
            apply_rollout(doc, name_s, label, rollout)
        })
        .await
    }

    async fn set_rollout_if(
        &self,
        name: &str,
        label: &Label,
        rollout: Option<Rollout>,
        expected_version: u32,
        expected_rollout: Option<Rollout>,
    ) -> Result<(), StoreError> {
        let (name_s, label) = (name.to_owned(), label.clone());
        self.locked_doc(name, rollout, move |doc| {
            check_read(&doc, &label, expected_version, expected_rollout)?;
            apply_rollout(doc, name_s, label, rollout)
        })
        .await
    }

    async fn promote_rollout_if(
        &self,
        name: &str,
        label: &Label,
        expected_version: u32,
        expected_rollout: Rollout,
    ) -> Result<(), StoreError> {
        let (name_s, label) = (name.to_owned(), label.clone());
        self.locked_doc(name, None, move |doc| {
            check_read(&doc, &label, expected_version, Some(expected_rollout))?;
            move_label(doc, name_s, label, expected_rollout.version, None)
        })
        .await
    }
}

/// `set_label`'s checks and move on the prompt document `doc` (read under its lock): `version`
/// must be a version of the prompt, then the label must be as `expected` says (`None` skips
/// the check); the move ends the label's rollout.
fn move_label(
    mut doc: PromptFile,
    name: String,
    label: Label,
    version: u32,
    expected: Option<Option<u32>>,
) -> Result<(Option<PromptFile>, ()), StoreError> {
    if !doc.versions.iter().any(|v| v.version == version) {
        return Err(StoreError::NotFound {
            name,
            selector: format!("v{version}"),
        });
    }
    let actual = doc.labels.get(&label).copied();
    if let Some(exp) = expected
        && exp != actual
    {
        return Err(StoreError::Conflict {
            expected: exp,
            actual,
        });
    }
    doc.rollouts.remove(&label);
    doc.labels.insert(label, version);
    Ok((Some(doc), ()))
}

/// The compare-and-set of `set_rollout_if` and `promote_rollout_if` on `doc`: `label` must
/// point at `expected_version`, then its rollout must equal `expected_rollout` (`None` = no
/// rollout); each mismatch is its `Conflict`.
fn check_read(
    doc: &PromptFile,
    label: &Label,
    expected_version: u32,
    expected_rollout: Option<Rollout>,
) -> Result<(), StoreError> {
    let at = doc.labels.get(label).copied();
    if at != Some(expected_version) {
        return Err(StoreError::Conflict {
            expected: Some(expected_version),
            actual: at,
        });
    }
    let current = doc.rollouts.get(label).copied();
    if current != expected_rollout {
        return Err(StoreError::Conflict {
            expected: expected_rollout.map(|r| r.version),
            actual: current.map(|r| r.version),
        });
    }
    Ok(())
}

/// `set_rollout`'s checks and change on the prompt document `doc` (read under its lock):
/// returns the document to write, or `None` when there is nothing to write.
fn apply_rollout(
    mut doc: PromptFile,
    name: String,
    label: Label,
    rollout: Option<Rollout>,
) -> Result<(Option<PromptFile>, ()), StoreError> {
    // `set_rollout_if` and `promote_rollout_if` check the label first (`check_read`), and
    // `promote_rollout_if` moves it with `move_label`, so only `set_rollout` reaches this
    // branch.
    let Some(&at) = doc.labels.get(&label) else {
        return Err(StoreError::NotFound {
            name,
            selector: label.to_string(),
        });
    };
    match rollout {
        None => {
            if doc.rollouts.remove(&label).is_none() {
                return Ok((None, ()));
            }
        }
        Some(r) => {
            if !doc.versions.iter().any(|v| v.version == r.version) {
                return Err(StoreError::NotFound {
                    name,
                    selector: format!("v{}", r.version),
                });
            }
            if r.version == at {
                return Err(StoreError::Invalid(format!(
                    "rollout version v{} is already the version of label {label}",
                    r.version
                )));
            }
            doc.rollouts.insert(label, r);
        }
    }
    Ok((Some(doc), ()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::rollout::Rollout;

    fn nv(t: &str) -> NewVersion {
        NewVersion {
            name: "p".into(),
            template: t.into(),
            config: None,
            author: String::new(),
            message: String::new(),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn file_store_concurrent_create_version() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(FileStore::new(dir.path()));
        let mut tasks = Vec::new();
        for i in 0..8 {
            let s = s.clone();
            tasks.push(tokio::spawn(async move {
                s.create_version(nv(&format!("t{i}")))
                    .await
                    .unwrap()
                    .version
            }));
        }
        let mut got: Vec<u32> = Vec::new();
        for t in tasks {
            got.push(t.await.unwrap());
        }
        got.sort_unstable();
        assert_eq!(got, (1..=8).collect::<Vec<_>>());
        let snap = s.load_all().await.unwrap();
        assert_eq!(snap.prompts["p"].versions.len(), 8);
    }

    #[tokio::test(start_paused = true)]
    async fn file_store_stale_lock_times_out_naming_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path());
        std::fs::write(dir.path().join("p.yaml.lock"), "").unwrap();
        let err = s.create_version(nv("a")).await.unwrap_err();
        match err {
            StoreError::Io(msg) => assert!(msg.contains("p.yaml.lock"), "{msg}"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn file_store_deleted_file_disappears_and_changes_are_seen() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path());
        s.create_version(nv("a")).await.unwrap();
        assert_eq!(s.load_all().await.unwrap().prompts["p"].versions.len(), 1);
        s.create_version(nv("b")).await.unwrap();
        assert_eq!(s.load_all().await.unwrap().prompts["p"].versions.len(), 2);
        std::fs::remove_file(dir.path().join("p.yaml")).unwrap();
        assert!(s.load_all().await.unwrap().prompts.is_empty());
    }

    /// A same-length rewrite within one mtime tick (here: mtime restored by hand) is still seen,
    /// because the atomic rename gives the file a new inode.
    #[cfg(unix)]
    #[tokio::test]
    async fn file_store_same_length_label_move_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path());
        s.create_version(nv("a")).await.unwrap();
        s.create_version(nv("b")).await.unwrap();
        let prod = Label::production();
        s.set_label("p", &prod, 1, None).await.unwrap();
        let path = dir.path().join("p.yaml");
        let before = std::fs::metadata(&path).unwrap();
        assert_eq!(
            s.load_all().await.unwrap().prompts["p"].labels.get(&prod),
            Some(&1)
        );
        s.set_label("p", &prod, 2, Some(Some(1))).await.unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(before.modified().unwrap()).unwrap();
        drop(file);
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(
            (after.modified().unwrap(), after.len()),
            (before.modified().unwrap(), before.len())
        );
        assert_eq!(
            s.load_all().await.unwrap().prompts["p"].labels.get(&prod),
            Some(&2)
        );
    }

    #[tokio::test]
    async fn file_store_dotted_names_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path());
        for n in ["x.v1", "x.v2"] {
            let mut v = nv("t");
            v.name = n.into();
            s.create_version(v).await.unwrap();
        }
        let names: Vec<_> = s
            .load_all()
            .await
            .unwrap()
            .prompts
            .keys()
            .cloned()
            .collect();
        assert_eq!(names, vec!["x.v1", "x.v2"]);
        assert!(dir.path().join("x.v1.yaml").is_file());
    }

    #[tokio::test]
    async fn file_store_label_move_without_rollouts_writes_no_rollouts_key() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path());
        s.create_version(nv("a")).await.unwrap();
        s.create_version(nv("b")).await.unwrap();
        let prod = Label::production();
        s.set_label("p", &prod, 1, None).await.unwrap();
        s.set_label("p", &prod, 2, Some(Some(1))).await.unwrap();
        let text = std::fs::read_to_string(dir.path().join("p.yaml")).unwrap();
        assert!(!text.contains("rollouts"), "{text}");
    }

    #[tokio::test]
    async fn file_store_rollout_is_written_and_cleared_with_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path());
        s.create_version(nv("a")).await.unwrap();
        s.create_version(nv("b")).await.unwrap();
        let prod = Label::production();
        s.set_label("p", &prod, 1, None).await.unwrap();
        s.set_rollout("p", &prod, Some(Rollout::new(2, 10).unwrap()))
            .await
            .unwrap();
        let path = dir.path().join("p.yaml");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("rollouts:\n  production:\n    version: 2\n    percent: 10\n"),
            "{text}"
        );
        s.set_label("p", &prod, 1, None).await.unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("rollouts"), "{text}");
    }

    #[tokio::test]
    async fn file_store_reads_rollouts_and_rejects_percent_100() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path());
        s.create_version(nv("a")).await.unwrap();
        s.create_version(nv("b")).await.unwrap();
        let prod = Label::production();
        s.set_label("p", &prod, 1, None).await.unwrap();
        let path = dir.path().join("p.yaml");
        let text = std::fs::read_to_string(&path).unwrap();
        let with = |percent: u32| {
            format!("{text}rollouts:\n  production:\n    version: 2\n    percent: {percent}\n")
        };
        std::fs::write(&path, with(10)).unwrap();
        let (snap, rollouts) = s.load_all_with_rollouts().await.unwrap();
        assert_eq!(snap.prompts["p"].labels.get(&prod), Some(&1));
        assert_eq!(
            rollouts.get("p", &prod),
            Some(&Rollout::new(2, 10).unwrap())
        );
        std::fs::write(&path, with(100)).unwrap();
        match s.load_all().await {
            Err(StoreError::Decode(msg)) => assert!(msg.contains("p.yaml"), "{msg}"),
            other => panic!("{other:?}"),
        }
        match FileStore::new(dir.path()).load_rollouts().await {
            Err(StoreError::Decode(msg)) => assert!(msg.contains("percent"), "{msg}"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn file_store_nested_name_maps_to_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileStore::new(dir.path());
        let mut n = nv("x");
        n.name = "team/a".into();
        s.create_version(n).await.unwrap();
        assert!(dir.path().join("team").join("a.yaml").is_file());
    }
}
