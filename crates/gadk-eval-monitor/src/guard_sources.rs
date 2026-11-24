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

//! Where `gadk-eval guards` finds its guard specs and price file: local files and directories,
//! and `gs://` objects and prefixes read through [`Backends`].
//!
//! Every spec is read and parsed before any guard runs, so one bad spec stops the whole run
//! before anything is written.

use std::collections::HashMap;
use std::path::Path;

use crate::cli::Backends;
use crate::error::MonitorError;
use crate::guard_spec::GuardSpec;
use crate::prices::PriceTable;
use crate::store::is_valid_bucket;

/// The most guard specs one `guards` run takes.
pub(crate) const MAX_GUARD_SPECS: usize = 200;

/// A parsed guard spec and where it came from.
#[derive(Debug)]
pub(crate) struct LoadedSpec {
    /// The file path or `gs://<bucket>/<object>` URL the spec was read from.
    pub(crate) origin: String,
    /// The parsed spec.
    pub(crate) spec: GuardSpec,
}

/// Reads and parses every guard spec in `sources`, in order: a file is one spec; a directory
/// gives its `.yaml` and `.yml` files (not its subdirectories) sorted by name;
/// `gs://<bucket>/<object>` is one object; `gs://<bucket>/<prefix>/` or `gs://<bucket>` gives
/// the `.yaml` and `.yml` objects directly under the prefix, sorted by name.
///
/// # Errors
///
/// [`MonitorError::Spec`] when a source is missing, unreadable, an empty directory or prefix,
/// or a `gs://` URL with an invalid bucket; when a spec is invalid (naming its origin); when
/// there are more than [`MAX_GUARD_SPECS`] specs; or when two specs share a name. Errors from
/// [`Backends::list_objects`] and [`Backends::read_object`] are returned unchanged.
pub(crate) async fn load_specs(
    sources: &[String],
    backends: &dyn Backends,
) -> Result<Vec<LoadedSpec>, MonitorError> {
    let mut texts = Vec::new();
    for source in sources {
        resolve(source, backends, &mut texts).await?;
    }
    let mut specs = Vec::with_capacity(texts.len());
    for (origin, text) in texts {
        let spec = GuardSpec::from_yaml(&text).map_err(|e| match e {
            MonitorError::Spec(m) => {
                let detail = m.strip_prefix("guard spec: ").unwrap_or(&m);
                MonitorError::Spec(format!("guard spec {origin}: {detail}"))
            }
            other => other,
        })?;
        specs.push(LoadedSpec { origin, spec });
    }
    if specs.len() > MAX_GUARD_SPECS {
        return Err(MonitorError::Spec(format!(
            "guards: more than {MAX_GUARD_SPECS} specs"
        )));
    }
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for s in &specs {
        if let Some(first) = seen.get(s.spec.name.as_str()) {
            return Err(MonitorError::Spec(format!(
                "guards: duplicate guard name {} in {first} and {}",
                s.spec.name, s.origin
            )));
        }
        seen.insert(&s.spec.name, &s.origin);
    }
    Ok(specs)
}

/// The price table at `source`: a local file ([`PriceTable::load`]) or a `gs://<bucket>/<object>`
/// object.
///
/// # Errors
///
/// [`MonitorError::Spec`] `price file <source>: <detail>` when the file cannot be read or
/// parsed, or the object is missing (`not found`) or not a valid object URL. Errors from
/// [`Backends::read_object`] are returned unchanged.
pub(crate) async fn load_prices(
    source: &str,
    backends: &dyn Backends,
) -> Result<PriceTable, MonitorError> {
    let Some(rest) = source.strip_prefix("gs://") else {
        return PriceTable::load(source);
    };
    let err = |detail: &str| MonitorError::Spec(format!("price file {source}: {detail}"));
    let (bucket, object) = rest.split_once('/').unwrap_or((rest, ""));
    if !is_valid_bucket(bucket) {
        return Err(err("invalid bucket"));
    }
    if object.is_empty() || object.ends_with('/') {
        return Err(err("not an object"));
    }
    let text = backends
        .read_object(bucket, object)
        .await?
        .ok_or_else(|| err("not found"))?;
    PriceTable::from_yaml(&text).map_err(|detail| err(&detail))
}

/// Adds `(origin, text)` for every spec `source` names to `out`.
async fn resolve(
    source: &str,
    backends: &dyn Backends,
    out: &mut Vec<(String, String)>,
) -> Result<(), MonitorError> {
    if let Some(rest) = source.strip_prefix("gs://") {
        let (bucket, object) = rest.split_once('/').unwrap_or((rest, ""));
        if !is_valid_bucket(bucket) {
            return Err(MonitorError::Spec(format!(
                "guard spec {source}: invalid bucket"
            )));
        }
        if !object.is_empty() && !object.ends_with('/') {
            let text = backends
                .read_object(bucket, object)
                .await?
                .ok_or_else(|| not_found(source))?;
            out.push((source.to_owned(), text));
            return Ok(());
        }
        let mut names: Vec<String> = backends
            .list_objects(bucket, object)
            .await?
            .into_iter()
            .filter(|n| is_yaml(n))
            .collect();
        if names.is_empty() {
            return Err(no_specs(source));
        }
        names.sort();
        for name in names {
            let origin = format!("gs://{bucket}/{name}");
            let text = backends
                .read_object(bucket, &name)
                .await?
                .ok_or_else(|| not_found(&origin))?;
            out.push((origin, text));
        }
        return Ok(());
    }
    let path = Path::new(source);
    let meta = std::fs::metadata(path).map_err(|e| local_error(source, e))?;
    if !meta.is_dir() {
        let text = std::fs::read_to_string(path).map_err(|e| local_error(source, e))?;
        out.push((source.to_owned(), text));
        return Ok(());
    }
    let dir = source.strip_suffix('/').unwrap_or(source);
    let mut files = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|e| local_error(source, e))? {
        let entry = entry.map_err(|e| local_error(source, e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_yaml(&name) {
            continue;
        }
        let origin = format!("{dir}/{name}");
        let meta = std::fs::metadata(entry.path()).map_err(|e| local_error(&origin, e))?;
        if meta.is_file() {
            files.push((name, origin, entry.path()));
        }
    }
    if files.is_empty() {
        return Err(no_specs(source));
    }
    files.sort();
    for (_, origin, file) in files {
        let text = std::fs::read_to_string(&file).map_err(|e| local_error(&origin, e))?;
        out.push((origin, text));
    }
    Ok(())
}

/// Whether `name` is a spec file name: it ends in `.yaml` or `.yml`.
fn is_yaml(name: &str) -> bool {
    name.ends_with(".yaml") || name.ends_with(".yml")
}

fn not_found(source: &str) -> MonitorError {
    MonitorError::Spec(format!("guard spec {source}: not found"))
}

fn no_specs(source: &str) -> MonitorError {
    MonitorError::Spec(format!("guard spec {source}: no .yaml or .yml specs"))
}

/// `not found` for a missing path, else `reading: <error>`.
fn local_error(source: &str, e: std::io::Error) -> MonitorError {
    if e.kind() == std::io::ErrorKind::NotFound {
        not_found(source)
    } else {
        MonitorError::Spec(format!("guard spec {source}: reading: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use gadk_model::Llm;

    use crate::source::TranscriptSource;
    use crate::spec::{JudgeSpec, SourceSpec};

    use super::*;

    /// Backends serving Cloud Storage objects from a list of `(bucket, name, text)`; listing
    /// the bucket `broken` fails.
    struct Objects(Vec<(&'static str, String, String)>);

    impl Objects {
        fn new(objects: &[(&'static str, &str, &str)]) -> Self {
            Self(
                objects
                    .iter()
                    .map(|(b, n, t)| (*b, (*n).to_owned(), (*t).to_owned()))
                    .collect(),
            )
        }
    }

    #[async_trait::async_trait]
    impl Backends for Objects {
        fn source(&self, _spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
            Err(MonitorError::Source("no source".into()))
        }

        async fn judge(&self, _spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
            Err(MonitorError::Judge("no judge".into()))
        }

        async fn list_objects(
            &self,
            bucket: &str,
            prefix: &str,
        ) -> Result<Vec<String>, MonitorError> {
            if bucket == "broken" {
                return Err(MonitorError::Store("HTTP 403: denied".into()));
            }
            Ok(self
                .0
                .iter()
                .filter(|(b, n, _)| {
                    *b == bucket
                        && n.strip_prefix(prefix)
                            .is_some_and(|rest| !rest.contains('/'))
                })
                .map(|(_, n, _)| n.clone())
                .collect())
        }

        async fn read_object(
            &self,
            bucket: &str,
            name: &str,
        ) -> Result<Option<String>, MonitorError> {
            Ok(self
                .0
                .iter()
                .find(|(b, n, _)| *b == bucket && n == name)
                .map(|(_, _, t)| t.clone()))
        }
    }

    /// Backends with the default (refusing) object methods.
    struct NoObjects;

    #[async_trait::async_trait]
    impl Backends for NoObjects {
        fn source(&self, _spec: &SourceSpec) -> Result<Arc<dyn TranscriptSource>, MonitorError> {
            Err(MonitorError::Source("no source".into()))
        }

        async fn judge(&self, _spec: &JudgeSpec) -> Result<Arc<dyn Llm>, MonitorError> {
            Err(MonitorError::Judge("no judge".into()))
        }
    }

    /// A valid guard spec named `name`.
    fn spec(name: &str) -> String {
        format!(
            "name: {name}\nsource: {{ bigquery: {{ project: p, dataset: d, table: t }} }}\n\
             store: file:///tmp/prompts\nprompt: support\nlabel: production\nwindow: 1h\n\
             ramp: [10, 25, 50]\nchecks:\n  - error_rate: {{ max_increase: 0.01 }}\n"
        )
    }

    fn write(dir: &Path, file: &str, text: &str) -> String {
        let p = dir.join(file);
        std::fs::write(&p, text).unwrap();
        p.display().to_string()
    }

    /// `(origin, name)` of each loaded spec.
    async fn load(sources: &[String], backends: &dyn Backends) -> Vec<(String, String)> {
        load_specs(sources, backends)
            .await
            .unwrap()
            .into_iter()
            .map(|s| (s.origin, s.spec.name))
            .collect()
    }

    /// The error text of loading `sources`.
    async fn load_err(sources: &[String], backends: &dyn Backends) -> String {
        load_specs(sources, backends).await.unwrap_err().to_string()
    }

    #[tokio::test]
    async fn a_file_is_one_spec() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(dir.path(), "support.txt", &spec("support"));
        assert_eq!(
            load(std::slice::from_ref(&file), &NoObjects).await,
            [(file, "support".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_directory_gives_its_yaml_files_sorted() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "b.yaml", &spec("b"));
        write(dir.path(), "a.yml", &spec("a"));
        write(dir.path(), "c.txt", &spec("c"));
        std::fs::create_dir(dir.path().join("sub.yaml")).unwrap();
        write(&dir.path().join("sub.yaml"), "d.yaml", &spec("d"));
        let d = dir.path().display().to_string();
        let expected = [
            (format!("{d}/a.yml"), "a".to_owned()),
            (format!("{d}/b.yaml"), "b".to_owned()),
        ];
        assert_eq!(load(std::slice::from_ref(&d), &NoObjects).await, expected);
        // A trailing `/` is not doubled in the origins.
        assert_eq!(load(&[format!("{d}/")], &NoObjects).await, expected);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = write(dir.path(), "target.txt", &spec("linked"));
        let specs = dir.path().join("specs");
        std::fs::create_dir(&specs).unwrap();
        std::os::unix::fs::symlink(&target, specs.join("linked.yaml")).unwrap();
        let link = specs.join("linked.yaml").display().to_string();
        assert_eq!(
            load(&[specs.display().to_string()], &NoObjects).await,
            [(link, "linked".to_owned())]
        );
    }

    #[tokio::test]
    async fn gs_objects_and_prefixes_give_their_specs() {
        let objects = Objects::new(&[
            ("b", "x.yaml", &spec("x")),
            ("b", "p/b.yaml", &spec("pb")),
            ("b", "p/a.yml", &spec("pa")),
            ("b", "p/c.txt", &spec("pc")),
            ("b", "p/sub/d.yaml", &spec("pd")),
            ("other", "p/e.yaml", &spec("pe")),
        ]);
        assert_eq!(
            load(&["gs://b/x.yaml".into(), "gs://b/p/".into()], &objects).await,
            pairs(&[
                ("gs://b/x.yaml", "x"),
                ("gs://b/p/a.yml", "pa"),
                ("gs://b/p/b.yaml", "pb"),
            ])
        );
        // `gs://b` is the empty prefix: the objects at the top level only.
        assert_eq!(
            load(&["gs://b".into()], &objects).await,
            pairs(&[("gs://b/x.yaml", "x")])
        );
    }

    #[tokio::test]
    async fn missing_and_unreadable_sources_are_errors_naming_them() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.yaml").display().to_string();
        assert_eq!(
            load_err(std::slice::from_ref(&missing), &NoObjects).await,
            format!("monitor spec: guard spec {missing}: not found")
        );
        let objects = Objects::new(&[]);
        assert_eq!(
            load_err(&["gs://b/missing.yaml".into()], &objects).await,
            "monitor spec: guard spec gs://b/missing.yaml: not found"
        );
        // A path through a regular file cannot be read.
        let file = write(dir.path(), "a-file", "x");
        let through = format!("{file}/g.yaml");
        let err = load_err(std::slice::from_ref(&through), &NoObjects).await;
        assert!(
            err.starts_with(&format!("monitor spec: guard spec {through}: reading: ")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn empty_directories_and_prefixes_are_errors_naming_them() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "notes.txt", "x");
        let d = dir.path().display().to_string();
        assert_eq!(
            load_err(std::slice::from_ref(&d), &NoObjects).await,
            format!("monitor spec: guard spec {d}: no .yaml or .yml specs")
        );
        let objects = Objects::new(&[("b", "p/notes.txt", "x")]);
        assert_eq!(
            load_err(&["gs://b/p/".into()], &objects).await,
            "monitor spec: guard spec gs://b/p/: no .yaml or .yml specs"
        );
    }

    #[tokio::test]
    async fn gs_errors_pass_through_and_buckets_are_checked() {
        let objects = Objects::new(&[]);
        assert_eq!(
            load_err(&["gs://broken/p/".into()], &objects).await,
            "report store: HTTP 403: denied"
        );
        assert_eq!(
            load_err(&["gs://b/x.yaml".into()], &NoObjects).await,
            "report store: gs:// sources are not supported by these backends"
        );
        for bad in ["gs://", "gs://Bad/x.yaml"] {
            assert_eq!(
                load_err(&[bad.into()], &objects).await,
                format!("monitor spec: guard spec {bad}: invalid bucket")
            );
        }
    }

    #[tokio::test]
    async fn an_invalid_spec_names_its_origin_after_every_source_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let good = write(dir.path(), "a.yaml", &spec("a"));
        let bad = write(dir.path(), "b.yaml", "name: b\n");
        let detail = GuardSpec::from_yaml("name: b\n")
            .unwrap_err()
            .to_string()
            .strip_prefix("monitor spec: guard spec: ")
            .unwrap()
            .to_owned();
        assert_eq!(
            load_err(&[good.clone(), bad.clone()], &NoObjects).await,
            format!("monitor spec: guard spec {bad}: {detail}")
        );
        // A missing source is reported before the invalid spec it follows.
        let missing = dir.path().join("missing.yaml").display().to_string();
        assert_eq!(
            load_err(&[bad, missing.clone()], &NoObjects).await,
            format!("monitor spec: guard spec {missing}: not found")
        );
    }

    #[tokio::test]
    async fn more_than_200_specs_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..MAX_GUARD_SPECS {
            write(
                dir.path(),
                &format!("g{i:03}.yaml"),
                &spec(&format!("g{i:03}")),
            );
        }
        let d = dir.path().display().to_string();
        assert_eq!(
            load(std::slice::from_ref(&d), &NoObjects).await.len(),
            MAX_GUARD_SPECS
        );
        write(dir.path(), "g200.yaml", &spec("g200"));
        assert_eq!(
            load_err(&[d], &NoObjects).await,
            "monitor spec: guards: more than 200 specs"
        );
    }

    #[tokio::test]
    async fn duplicate_names_are_errors_naming_both_origins() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.yaml", &spec("support"));
        let b = write(dir.path(), "b.yaml", &spec("support"));
        assert_eq!(
            load_err(&[dir.path().display().to_string()], &NoObjects).await,
            format!("monitor spec: guards: duplicate guard name support in {a} and {b}")
        );
    }

    #[tokio::test]
    async fn prices_load_from_a_file_or_an_object() {
        let dir = tempfile::tempdir().unwrap();
        let table = "- { model: m-flash, input: 0.3, output: 2.5 }\n";
        let file = write(dir.path(), "prices.yaml", table);
        // `PriceTable` has no `PartialEq`: compare the debug text.
        let expected = format!("{:?}", PriceTable::from_yaml(table).unwrap());
        assert_eq!(
            format!("{:?}", load_prices(&file, &NoObjects).await.unwrap()),
            expected
        );
        let missing = dir.path().join("missing.yaml").display().to_string();
        let err = load_prices(&missing, &NoObjects)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with(&format!("monitor spec: price file {missing}: ")),
            "{err}"
        );

        let objects = Objects::new(&[("b", "prices.yaml", table), ("b", "bad.yaml", "- 1\n")]);
        assert_eq!(
            format!(
                "{:?}",
                load_prices("gs://b/prices.yaml", &objects).await.unwrap()
            ),
            expected
        );
        let price_err = |source: &'static str| {
            let objects = &objects;
            async move { load_prices(source, objects).await.unwrap_err().to_string() }
        };
        assert_eq!(
            price_err("gs://b/missing.yaml").await,
            "monitor spec: price file gs://b/missing.yaml: not found"
        );
        let detail = PriceTable::from_yaml("- 1\n").unwrap_err();
        assert_eq!(
            price_err("gs://b/bad.yaml").await,
            format!("monitor spec: price file gs://b/bad.yaml: {detail}")
        );
        assert_eq!(
            price_err("gs://b/p/").await,
            "monitor spec: price file gs://b/p/: not an object"
        );
        assert_eq!(
            price_err("gs://Bad/prices.yaml").await,
            "monitor spec: price file gs://Bad/prices.yaml: invalid bucket"
        );
        assert_eq!(
            load_prices("gs://b/prices.yaml", &NoObjects)
                .await
                .unwrap_err()
                .to_string(),
            "report store: gs:// sources are not supported by these backends"
        );
    }

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter()
            .map(|(o, n)| ((*o).to_owned(), (*n).to_owned()))
            .collect()
    }
}
