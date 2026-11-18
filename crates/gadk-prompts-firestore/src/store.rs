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

//! The Firestore PromptStore.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gadk_model::gcp_auth::adc_credentials;
use gadk_prompts::{
    Label, NewVersion, PromptEntry, PromptSnapshot, PromptStore, PromptVersion, StoreError,
    VersionMeta, content_hash, validate_name,
};
use http::Method;
use serde_json::{Value, json};

use crate::client::{CallError, Client};
use crate::config::{DEFAULT_ENDPOINT, FirestoreConfig, SCOPES};
use crate::value::{prompt_fields, read_prompt, read_version, version_fields};

const ATTEMPTS: usize = 3;

/// Documents per `batchGet` request.
const BATCH_GET_LIMIT: usize = 100;

/// The document ID of prompt `name`: `%` → `%25`, `/` → `%2F`.
pub fn encode_doc_id(name: &str) -> String {
    name.replace('%', "%25").replace('/', "%2F")
}

/// `id` as a URL path segment (escape `%` once more).
fn url_seg(id: &str) -> String {
    id.replace('%', "%25")
}

/// Prompts in Firestore. See the crate docs for the layout.
pub struct FirestoreStore {
    client: Client,
    /// `projects/{p}/databases/{d}/documents`
    db: String,
    /// `db` or `db/{root}`
    root: String,
    /// Version documents already read, per prompt document name. Versions are immutable (they
    /// are written with `currentDocument.exists = false` and never updated), so a cached entry
    /// never goes stale; a prompt document's `createTime` tells a recreated prompt from the same one.
    /// Never held across an `.await`.
    cache: Mutex<HashMap<String, CachedVersions>>,
}

/// The versions read so far for one prompt document, valid for the incarnation of that
/// document identified by `create_time` (a deleted and recreated prompt has a new one).
#[derive(Default)]
struct CachedVersions {
    create_time: String,
    versions: BTreeMap<u32, Arc<PromptVersion>>,
}

impl std::fmt::Debug for FirestoreStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// State read inside a transaction.
struct Current {
    latest: u32,
    labels: BTreeMap<Label, u32>,
}

impl FirestoreStore {
    /// A store as `config` says.
    ///
    /// # Errors
    ///
    /// `Invalid` for an empty project, or a root with an odd number of segments or a segment
    /// outside `[A-Za-z0-9._-]+` (or `.` / `..`); `Io` when Application Default
    /// Credentials are needed and cannot be resolved.
    pub fn new(config: FirestoreConfig) -> Result<Self, StoreError> {
        if config.project.is_empty() {
            return Err(StoreError::Invalid("firestore: project is required".into()));
        }
        let root_path = config.root.as_deref().unwrap_or("").trim_matches('/');
        if !root_path.is_empty()
            && let Some(bad) = root_path.split('/').find(|seg| {
                seg.is_empty()
                    || *seg == "."
                    || *seg == ".."
                    || !seg
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            })
        {
            return Err(StoreError::Invalid(format!(
                "firestore root {root_path:?}: segment {bad:?} must match [A-Za-z0-9._-]+ and not be `.` or `..`"
            )));
        }
        if !root_path.is_empty() && !root_path.split('/').count().is_multiple_of(2) {
            return Err(StoreError::Invalid(format!(
                "firestore root {root_path:?} must be a document path (an even number of segments)"
            )));
        }
        let emulator = config
            .env
            .var("FIRESTORE_EMULATOR_HOST")
            .filter(|h| !h.is_empty());
        let endpoint = match (&config.endpoint, &emulator) {
            (Some(e), _) if !e.is_empty() => e.clone(),
            (_, Some(h)) if h.contains("://") => h.clone(),
            (_, Some(h)) => format!("http://{h}"),
            _ => DEFAULT_ENDPOINT.to_owned(),
        };
        let no_auth = emulator.is_some() || endpoint.starts_with("http://");
        let credentials = match (no_auth, config.credentials) {
            (true, _) => None,
            (false, Some(c)) => Some(c),
            (false, None) => {
                let file = config.env.var("GOOGLE_APPLICATION_CREDENTIALS");
                Some(
                    adc_credentials(file.as_deref(), &SCOPES)
                        .map_err(|e| StoreError::Io(format!("firestore credentials: {e}")))?,
                )
            }
        };
        let db = format!(
            "projects/{}/databases/{}/documents",
            config.project, config.database
        );
        let root = if root_path.is_empty() {
            db.clone()
        } else {
            format!("{db}/{root_path}")
        };
        Ok(Self {
            client: Client::new(&endpoint, credentials, config.http_client),
            db,
            root,
            cache: Mutex::default(),
        })
    }

    fn prompt_doc(&self, id: &str) -> String {
        format!("{}/prompts/{id}", self.root)
    }

    fn map(&self, name: &str, e: CallError) -> StoreError {
        match e {
            CallError::NotFound => StoreError::NotFound {
                name: name.into(),
                selector: String::new(),
            },
            CallError::Aborted => StoreError::Conflict {
                expected: None,
                actual: None,
            },
            CallError::Store(s) => s,
        }
    }

    /// Reads `doc` inside transaction `tx` with `:batchGet` (the Firestore emulator never
    /// answers a transactional single-document GET).
    async fn get(
        &self,
        doc: &str,
        tx: &str,
    ) -> Result<Option<serde_json::Map<String, Value>>, CallError> {
        let v = self
            .client
            .call(
                Method::POST,
                &format!("{}:batchGet", self.db),
                Some(json!({"documents": [doc], "transaction": tx})),
            )
            .await?;
        let decode = |m: &str| CallError::Store(StoreError::Decode(format!("batchGet: {m}")));
        let items = v
            .as_array()
            .ok_or_else(|| decode("response is not an array"))?;
        for it in items {
            if let Some(found) = it.get("found") {
                return Ok(Some(
                    found
                        .get("fields")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default(),
                ));
            }
            if it.get("missing").is_some() {
                return Ok(None);
            }
        }
        Err(decode("no found or missing item"))
    }
}

fn url_doc(doc: &str) -> String {
    // doc is "projects/…/documents/…/prompts/{id}[/versions/{n}]"; only ids contain '%'.
    url_seg(doc)
}

#[async_trait]
impl PromptStore for FirestoreStore {
    async fn load_all(&self) -> Result<PromptSnapshot, StoreError> {
        let parent = if self.root == self.db {
            self.db.clone()
        } else {
            self.root.clone()
        };
        let docs = |v: Value| -> Vec<(String, String, serde_json::Map<String, Value>)> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|it| {
                    let d = it.get("document")?;
                    Some((
                        d.get("name")?.as_str()?.to_owned(),
                        d.get("createTime")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        d.get("fields")?.as_object()?.clone(),
                    ))
                })
                .collect()
        };
        let path = format!("{}:runQuery", url_doc(&parent));
        let query = json!({"structuredQuery": {"from": [{"collectionId": "prompts", "allDescendants": false}]}});
        let prompts = self
            .client
            .call_unclassified(Method::POST, &path, Some(query))
            .await?;
        // (prompt doc name, create time, prompt name, latest version, labels)
        let mut listed = Vec::new();
        for (doc, created, fields) in docs(prompts) {
            let name = crate::value::get_str(&fields, "name")?;
            let (latest, labels) = read_prompt(&fields)?;
            listed.push((doc, created, name, latest, labels));
        }
        // Which version documents the cache lacks. The lock is released before any request.
        let wanted: Vec<String> = {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.retain(|doc, _| listed.iter().any(|(d, ..)| d == doc));
            let mut wanted = Vec::new();
            for (doc, created, _, latest, _) in &listed {
                let have = cache.entry(doc.clone()).or_default();
                let max = have.versions.keys().next_back().copied().unwrap_or(0);
                // A document without a createTime cannot be told apart from a recreated one, so
                // its cache is never reused.
                if created.is_empty() || have.create_time != *created || max > *latest {
                    // Deleted and recreated: start over.
                    have.versions.clear();
                    have.create_time.clone_from(created);
                }
                // Every version not cached, so a gap left by a missing document or a racing
                // call heals on the next poll; a complete cache wants nothing.
                wanted.extend(
                    (1..=*latest)
                        .filter(|n| !have.versions.contains_key(n))
                        .map(|n| format!("{doc}/versions/{n}")),
                );
            }
            wanted
        };
        let mut fetched = Vec::new();
        for chunk in wanted.chunks(BATCH_GET_LIMIT) {
            let reply = self
                .client
                .call_unclassified(
                    Method::POST,
                    &format!("{}:batchGet", self.db),
                    Some(json!({"documents": chunk})),
                )
                .await?;
            let items = reply
                .as_array()
                .ok_or_else(|| StoreError::Decode("batchGet: response is not an array".into()))?;
            for it in items {
                // A `missing` item is a version document that is not there: skipped.
                let Some(found) = it.get("found") else {
                    continue;
                };
                let Some(doc) = found.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let Some((pdoc, _)) = doc.rsplit_once("/versions/") else {
                    continue;
                };
                let fields = found
                    .get("fields")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let v = read_version(&fields)?;
                fetched.push((pdoc.to_owned(), v));
            }
        }
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // Insert under the lock that also builds the snapshot, so a concurrent call that
        // pruned or reset the cache meanwhile cannot make this one return fewer versions than
        // it fetched.
        for (pdoc, v) in fetched {
            let Some((_, created, ..)) = listed.iter().find(|(d, ..)| *d == pdoc) else {
                continue;
            };
            let have = cache.entry(pdoc).or_default();
            if have.create_time != *created {
                have.versions.clear();
                have.create_time.clone_from(created);
            }
            have.versions.insert(v.version, Arc::new(v));
        }
        let mut snap = PromptSnapshot::default();
        for (doc, _, name, latest, labels) in listed {
            let versions = cache
                .get(&doc)
                .map(|have| {
                    have.versions
                        .range(..=latest)
                        .map(|(n, v)| (*n, v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            snap.prompts.insert(name, PromptEntry { versions, labels });
        }
        Ok(snap)
    }

    async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError> {
        self.create_version_reporting(new).await.map(|(v, _)| v)
    }

    async fn create_version_reporting(
        &self,
        new: NewVersion,
    ) -> Result<(PromptVersion, bool), StoreError> {
        validate_name(&new.name)?;
        let id = encode_doc_id(&new.name);
        let pdoc = self.prompt_doc(&id);
        let hash = content_hash(&new.template, new.config.as_ref());
        for attempt in 1..=ATTEMPTS {
            let tx = self.begin().await.map_err(|e| self.map(&new.name, e))?;
            let cur = match self.get(&pdoc, &tx).await {
                Ok(Some(f)) => {
                    let (latest, labels) = read_prompt(&f)?;
                    Current { latest, labels }
                }
                Ok(None) => Current {
                    latest: 0,
                    labels: BTreeMap::new(),
                },
                Err(e) => {
                    self.rollback(&tx).await;
                    return Err(self.map(&new.name, e));
                }
            };
            if cur.latest > 0 {
                let vdoc = format!("{pdoc}/versions/{}", cur.latest);
                match self.get(&vdoc, &tx).await {
                    Ok(Some(f)) => {
                        let last = read_version(&f)?;
                        if last.meta.hash == hash {
                            self.rollback(&tx).await;
                            return Ok((last, false));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.rollback(&tx).await;
                        return Err(self.map(&new.name, e));
                    }
                }
            }
            let v = PromptVersion {
                name: new.name.clone(),
                version: cur.latest + 1,
                template: new.template.clone(),
                config: new.config.clone(),
                meta: VersionMeta {
                    // Firestore keeps microseconds; truncate so the returned version equals the
                    // stored one on platforms with nanosecond clocks (Linux CI).
                    created_at: chrono::SubsecRound::trunc_subsecs(chrono::Utc::now(), 6),
                    author: new.author.clone(),
                    message: new.message.clone(),
                    hash: hash.clone(),
                },
            };
            let writes = vec![
                json!({"update": {"name": pdoc, "fields": prompt_fields(&new.name, v.version, &cur.labels)}}),
                json!({"update": {"name": format!("{pdoc}/versions/{}", v.version), "fields": version_fields(&v)},
                       "currentDocument": {"exists": false}}),
            ];
            match self.commit(&tx, writes).await {
                Ok(()) => return Ok((v, true)),
                Err(CallError::Aborted) if attempt < ATTEMPTS => continue,
                Err(e) => return Err(self.map(&new.name, e)),
            }
        }
        Err(StoreError::Conflict {
            expected: None,
            actual: None,
        })
    }

    async fn set_label(
        &self,
        name: &str,
        label: &Label,
        version: u32,
        expected: Option<Option<u32>>,
    ) -> Result<(), StoreError> {
        self.label_op(name, label, Some((version, expected))).await
    }

    async fn remove_label(&self, name: &str, label: &Label) -> Result<(), StoreError> {
        self.label_op(name, label, None).await
    }
}

impl FirestoreStore {
    async fn begin(&self) -> Result<String, CallError> {
        let v = self
            .client
            .call(
                Method::POST,
                &format!("{}:beginTransaction", self.db),
                Some(json!({"options": {"readWrite": {}}})),
            )
            .await?;
        v.get("transaction")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                CallError::Store(StoreError::Decode(
                    "beginTransaction: no transaction".into(),
                ))
            })
    }

    async fn commit(&self, tx: &str, writes: Vec<Value>) -> Result<(), CallError> {
        self.client
            .call(
                Method::POST,
                &format!("{}:commit", self.db),
                Some(json!({"transaction": tx, "writes": writes})),
            )
            .await
            .map(|_| ())
    }

    async fn rollback(&self, tx: &str) {
        let _ = self
            .client
            .call(
                Method::POST,
                &format!("{}:rollback", self.db),
                Some(json!({"transaction": tx})),
            )
            .await;
    }

    /// `Some((version, expected))` sets the label, `None` removes it.
    async fn label_op(
        &self,
        name: &str,
        label: &Label,
        set: Option<(u32, Option<Option<u32>>)>,
    ) -> Result<(), StoreError> {
        validate_name(name)?;
        let pdoc = self.prompt_doc(&encode_doc_id(name));
        for attempt in 1..=ATTEMPTS {
            let tx = self.begin().await.map_err(|e| self.map(name, e))?;
            let fields = match self.get(&pdoc, &tx).await {
                Ok(Some(f)) => f,
                Ok(None) => {
                    self.rollback(&tx).await;
                    return Err(StoreError::NotFound {
                        name: name.into(),
                        selector: String::new(),
                    });
                }
                Err(e) => {
                    self.rollback(&tx).await;
                    return Err(self.map(name, e));
                }
            };
            let (latest, mut labels) = read_prompt(&fields)?;
            let actual = labels.get(label).copied();
            let fail = match set {
                Some((version, _)) if version == 0 || version > latest => {
                    Some(StoreError::NotFound {
                        name: name.into(),
                        selector: format!("v{version}"),
                    })
                }
                Some((_, Some(exp))) if exp != actual => Some(StoreError::Conflict {
                    expected: exp,
                    actual,
                }),
                None if actual.is_none() => Some(StoreError::NotFound {
                    name: name.into(),
                    selector: label.to_string(),
                }),
                _ => None,
            };
            if let Some(e) = fail {
                self.rollback(&tx).await;
                return Err(e);
            }
            match set {
                Some((version, _)) => {
                    labels.insert(label.clone(), version);
                }
                None => {
                    labels.remove(label);
                }
            }
            let writes = vec![
                json!({"update": {"name": pdoc, "fields": prompt_fields(name, latest, &labels)}}),
            ];
            match self.commit(&tx, writes).await {
                Ok(()) => return Ok(()),
                Err(CallError::Aborted) if attempt < ATTEMPTS => continue,
                Err(e) => return Err(self.map(name, e)),
            }
        }
        Err(StoreError::Conflict {
            expected: None,
            actual: None,
        })
    }
}
