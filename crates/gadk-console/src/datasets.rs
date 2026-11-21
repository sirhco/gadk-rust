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

//! The eval-set files of `--datasets <dir>`: adding a trace's runs to one as cases.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gadk_eval::EvalSet;
use gadk_eval_monitor::{ExportOptions, ExportSource, cases_text, export_runs, write_set_file};

/// The note shown when a trace cannot be added to a dataset.
pub(crate) const NEEDS_DATASETS: &str = "Adding traces to a dataset needs --datasets <dir> and the BigQuery trace source (--traces bigquery://<project>.<dataset>.<table>).";

/// The eval-set directory and the export source that reads traces for it.
pub(crate) struct Datasets {
    dir: PathBuf,
    source: Arc<dyn ExportSource>,
    /// Held while a POST reads and writes a file in `dir`, so writes do not interleave.
    lock: tokio::sync::Mutex<()>,
}

/// Why a trace was not added.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AddError {
    /// The dataset name is not `[A-Za-z0-9_-]{1,64}`.
    BadName,
    /// The trace has no invocation in the last 30 days.
    NoTrace,
    /// No case was added; one reason per run.
    NothingAdded(Vec<String>),
    /// The existing file is not an eval set the cases can be appended to.
    File(String),
    /// A BigQuery query failed.
    Source(String),
    /// The file could not be written.
    Write(String),
}

/// Whether `name` is a usable dataset name: 1-64 of `[A-Za-z0-9_-]`.
pub(crate) fn is_dataset_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Datasets {
    /// The eval sets in `dir`, read through `source`.
    pub(crate) fn new(dir: PathBuf, source: Arc<dyn ExportSource>) -> Self {
        Self {
            dir,
            source,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The names of the eval sets in the directory (the stems of its `*.yaml` files that are
    /// valid dataset names), sorted; none when it cannot be read.
    pub(crate) fn names(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter_map(|n| n.strip_suffix(".yaml").map(str::to_owned))
            .filter(|n| is_dataset_name(n))
            .collect();
        names.sort();
        names
    }

    /// Adds the runs of trace `trace` as cases to `<dir>/<dataset>.yaml` and returns how many
    /// were added. The file is created (set name `dataset`, default rubric `rubric`) when
    /// absent, else appended to; judge cases use `rubric`, else the file's `judge_rubric`.
    /// Runs already in the file are skipped. Requests are serialized from the file read to the
    /// write.
    pub(crate) async fn add_trace(
        &self,
        trace: &str,
        dataset: &str,
        rubric: Option<&str>,
    ) -> Result<usize, AddError> {
        if !is_dataset_name(dataset) {
            return Err(AddError::BadName);
        }
        let source_err = |e: gadk_eval_monitor::MonitorError| AddError::Source(e.to_string());
        let invocations = self
            .source
            .trace_invocations(&[trace.to_owned()])
            .await
            .map_err(source_err)?;
        if invocations.is_empty() {
            return Err(AddError::NoTrace);
        }
        let _write = self.lock.lock().await;
        let file = format!("{dataset}.yaml");
        let path = self.dir.join(&file);
        let file_err = |e: &dyn std::fmt::Display| AddError::File(format!("{file}: {e}"));
        let existing = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let set = EvalSet::from_yaml(&text).map_err(|e| file_err(&e))?;
                Some((text, set))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(file_err(&e)),
        };
        let mut opts = ExportOptions::default();
        let judge_rubric = rubric.map(str::to_owned).or_else(|| {
            existing
                .as_ref()
                .and_then(|(_, set)| set.judge_rubric.clone())
        });
        if let Some(r) = judge_rubric {
            opts = opts.with_rubric(r);
        }
        let known: HashSet<&str> = existing
            .as_ref()
            .map(|(_, set)| set.cases.iter().map(|c| c.id.as_str()).collect())
            .unwrap_or_default();
        let selected: Vec<_> = invocations
            .into_iter()
            .map(|t| (t.invocation_id, t.started_at))
            .collect();
        let mut reasons = Vec::new();
        let mut added = Vec::new();
        for (run, result) in export_runs(self.source.as_ref(), &selected, &opts)
            .await
            .map_err(source_err)?
        {
            let id = format!("inv-{}", run.invocation_id);
            match result {
                _ if known.contains(id.as_str()) => {
                    reasons.push(format!("{id}: already in {file}"))
                }
                Ok(exported) => added.push(exported),
                Err(reason) => reasons.push(format!("{id}: {reason}")),
            }
        }
        if added.is_empty() {
            if reasons.is_empty() {
                reasons.push("no run of the trace could be read".to_owned());
            }
            return Err(AddError::NothingAdded(reasons));
        }
        let current = existing.as_ref().map(|(text, _)| text.as_str());
        let text = cases_text(current, dataset, rubric, &added).map_err(|e| file_err(&e))?;
        write_set_file(&path, &text, current.is_none()).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                AddError::File(format!("{file}: was created while adding; try again"))
            } else {
                AddError::Write(format!("{}: {e}", path.display()))
            }
        })?;
        Ok(added.len())
    }
}
