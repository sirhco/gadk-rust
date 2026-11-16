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

use std::collections::BTreeMap;
use std::sync::Arc;

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

/// The document ID of prompt `name`: `%` → `%25`, `/` → `%2F`.
pub fn encode_doc_id(name: &str) -> String {
    name.replace('%', "%25").replace('/', "%2F")
}

/// `id` as a URL path segment (escape `%` once more).
fn url_seg(id: &str) -> String {
    id.replace('%', "%25")
}

fn url_query(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Prompts in Firestore. See the crate docs for the layout.
pub struct FirestoreStore {
    client: Client,
    /// `projects/{p}/databases/{d}/documents`
    db: String,
    /// `db` or `db/{root}`
    root: String,
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
    /// `Invalid` for an empty project or an odd-segment root; `Io` when Application Default
    /// Credentials are needed and cannot be resolved.
    pub fn new(config: FirestoreConfig) -> Result<Self, StoreError> {
        if config.project.is_empty() {
            return Err(StoreError::Invalid("firestore: project is required".into()));
        }
        let root_path = config.root.as_deref().unwrap_or("").trim_matches('/');
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

    async fn get(
        &self,
        doc: &str,
        tx: &str,
    ) -> Result<Option<serde_json::Map<String, Value>>, CallError> {
        let path = format!("{}?transaction={}", url_doc(doc), url_query(tx));
        match self.client.call(Method::GET, &path, None).await {
            Ok(v) => Ok(Some(
                v.get("fields")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default(),
            )),
            Err(CallError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
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
        let q = |coll: &str, all: bool| json!({"structuredQuery": {"from": [{"collectionId": coll, "allDescendants": all}]}});
        let docs = |v: Value| -> Vec<(String, serde_json::Map<String, Value>)> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|it| {
                    let d = it.get("document")?;
                    Some((
                        d.get("name")?.as_str()?.to_owned(),
                        d.get("fields")?.as_object()?.clone(),
                    ))
                })
                .collect()
        };
        let path = format!("{}:runQuery", url_doc(&parent));
        let prompts = self
            .client
            .call(Method::POST, &path, Some(q("prompts", false)))
            .await
            .map_err(|e| self.map("", e))?;
        let versions = self
            .client
            .call(Method::POST, &path, Some(q("versions", true)))
            .await
            .map_err(|e| self.map("", e))?;
        let mut snap = PromptSnapshot::default();
        let mut by_doc: BTreeMap<String, String> = BTreeMap::new(); // prompt doc name -> prompt name
        for (doc, fields) in docs(prompts) {
            let name = crate::value::get_str(&fields, "name")?;
            let (_, labels) = read_prompt(&fields)?;
            snap.prompts.insert(
                name.clone(),
                PromptEntry {
                    versions: BTreeMap::new(),
                    labels,
                },
            );
            by_doc.insert(doc, name);
        }
        let prefix = format!("{}/prompts/", self.root);
        for (doc, fields) in docs(versions) {
            if !doc.starts_with(&prefix) {
                continue;
            }
            let Some((pdoc, _)) = doc.rsplit_once("/versions/") else {
                continue;
            };
            let Some(name) = by_doc.get(pdoc) else {
                continue;
            };
            let v = read_version(&fields)?;
            if let Some(e) = snap.prompts.get_mut(name) {
                e.versions.insert(v.version, Arc::new(v));
            }
        }
        Ok(snap)
    }

    async fn create_version(&self, new: NewVersion) -> Result<PromptVersion, StoreError> {
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
                            return Ok(last);
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
                Ok(()) => return Ok(v),
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
