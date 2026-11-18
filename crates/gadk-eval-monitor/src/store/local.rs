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

//! Reports in a local directory.

use std::io::Write as _;
use std::path::PathBuf;

use crate::error::MonitorError;

pub(super) struct LocalStore {
    dir: PathBuf,
}

impl LocalStore {
    pub(super) fn new(dir: &str) -> Self {
        Self { dir: dir.into() }
    }

    fn store_err(&self, e: std::io::Error) -> MonitorError {
        MonitorError::Store(format!("{}: {e}", self.dir.display()))
    }

    /// Writes `body` as `name` unless that file exists; `false` when it does.
    pub(super) fn create(&self, name: &str, body: &str) -> Result<bool, MonitorError> {
        self.create_with(name, |f| f.write_all(body.as_bytes()))
    }

    /// Creates `name` unless it exists (`false` when it does) and fills it with `write`. When
    /// `write` fails the file is removed, so a partial report never keeps a report name.
    fn create_with(
        &self,
        name: &str,
        write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
    ) -> Result<bool, MonitorError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| self.store_err(e))?;
        let path = self.dir.join(name);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match file {
            Ok(mut f) => match write(&mut f) {
                Ok(()) => Ok(true),
                Err(e) => {
                    drop(f);
                    // Best effort: the write error is the one worth reporting.
                    let _ = std::fs::remove_file(&path);
                    Err(self.store_err(e))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(self.store_err(e)),
        }
    }

    /// The file names in the directory (none when it does not exist).
    pub(super) fn names(&self) -> Result<Vec<String>, MonitorError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(self.store_err(e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| self.store_err(e))?;
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    /// The contents of `name`, or `None` when it does not exist.
    pub(super) fn read(&self, name: &str) -> Result<Option<String>, MonitorError> {
        match std::fs::read_to_string(self.dir.join(name)) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(self.store_err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_write_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(&dir.path().display().to_string());
        let err = store
            .create_with("r.json", |f| {
                f.write_all(b"{\"partial")?;
                Err(std::io::ErrorKind::StorageFull.into())
            })
            .unwrap_err();
        assert!(matches!(err, MonitorError::Store(_)), "{err}");
        assert_eq!(store.names().unwrap(), Vec::<String>::new());
        assert!(store.create("r.json", "{}").unwrap());
        assert_eq!(store.read("r.json").unwrap().as_deref(), Some("{}"));
    }
}
