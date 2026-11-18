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

//! A stateful in-memory Firestore REST v1 fake: documents, optimistic read-write
//! transactions (reads via batchGet), runQuery, and injectable commit aborts.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gadk_model::rest::test_util::{FakeResponse, FakeServer, RecordedRequest};
use serde_json::{Value, json};

#[derive(Default)]
pub struct State {
    /// full document name → (fields, update counter)
    pub docs: BTreeMap<String, (Value, u64)>,
    /// transaction id → (document name → counter seen at read; 0 = absent)
    pub txs: HashMap<String, HashMap<String, u64>>,
    /// document name → create time, set when the document is first written (a deleted and
    /// rewritten document gets a new one, like Firestore's `createTime`)
    pub created: HashMap<String, String>,
}

pub struct Fake {
    pub server: FakeServer,
    pub abort_next: Arc<AtomicUsize>,
    pub requests: Arc<Mutex<Vec<RecordedRequest>>>,
    pub state: Arc<Mutex<State>>,
}

/// Undoes one level of URL percent-encoding (URL paths arrive with '%' escaped twice).
fn decode_path(p: &str) -> String {
    let b = p.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 3 <= b.len()
            && let Ok(x) = u8::from_str_radix(&p[i + 1..i + 3], 16)
        {
            out.push(x);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

fn err(status: u16, code: &str) -> FakeResponse {
    FakeResponse::json_status(
        status,
        json!({"error": {"code": status, "status": code, "message": code}}).to_string(),
    )
}

pub fn start() -> Fake {
    let state: Arc<Mutex<State>> = Arc::default();
    let abort_next = Arc::new(AtomicUsize::new(0));
    let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::default();
    let counter = Arc::new(AtomicU64::new(1));
    let (st, ab, rq) = (state.clone(), abort_next.clone(), requests.clone());
    let server = FakeServer::start(move |req: &RecordedRequest| {
        rq.lock().unwrap().push(req.clone());
        let mut s = st.lock().unwrap();
        let path = req.path.trim_start_matches("/v1/");
        if let Some(_db) = path.strip_suffix(":beginTransaction") {
            let id = format!("tx{}+/=", counter.fetch_add(1, Ordering::SeqCst));
            s.txs.insert(id.clone(), HashMap::new());
            return FakeResponse::json(json!({"transaction": id}).to_string());
        }
        if path.ends_with(":rollback") {
            let tx = req.json()["transaction"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            s.txs.remove(&tx);
            return FakeResponse::json("{}");
        }
        if path.ends_with(":commit") {
            let body = req.json();
            let tx = body["transaction"].as_str().unwrap_or_default().to_owned();
            let Some(reads) = s.txs.remove(&tx) else {
                return err(400, "INVALID_ARGUMENT");
            };
            if ab.load(Ordering::SeqCst) > 0 {
                ab.fetch_sub(1, Ordering::SeqCst);
                return err(409, "ABORTED");
            }
            for (name, seen) in &reads {
                let now = s.docs.get(name).map_or(0, |d| d.1);
                if now != *seen {
                    return err(409, "ABORTED");
                }
            }
            for w in body["writes"].as_array().cloned().unwrap_or_default() {
                let doc = &w["update"];
                let name = doc["name"].as_str().unwrap_or_default().to_owned();
                if w["currentDocument"]["exists"] == json!(false) && s.docs.contains_key(&name) {
                    return err(400, "FAILED_PRECONDITION");
                }
                let n = counter.fetch_add(1, Ordering::SeqCst);
                if !s.docs.contains_key(&name) {
                    s.created
                        .insert(name.clone(), format!("2026-11-15T00:00:00.{n:09}Z"));
                }
                s.docs.insert(name, (doc["fields"].clone(), n));
            }
            return FakeResponse::json(json!({"writeResults": []}).to_string());
        }
        if path.ends_with(":batchGet") {
            let body = req.json();
            let tx = body["transaction"].as_str().map(str::to_owned);
            let mut out = Vec::new();
            for name in body["documents"].as_array().cloned().unwrap_or_default() {
                let name = name.as_str().unwrap_or_default().to_owned();
                let current = s.docs.get(&name).cloned();
                if let Some(tx) = &tx
                    && let Some(reads) = s.txs.get_mut(tx)
                {
                    reads.insert(name.clone(), current.as_ref().map_or(0, |d| d.1));
                }
                out.push(match current {
                    Some((fields, _)) => json!({
                        "found": {
                            "name": name,
                            "fields": fields,
                            "createTime": "2026-11-15T00:00:00Z",
                            "updateTime": "2026-11-15T00:00:00Z",
                        },
                        "readTime": "2026-11-15T00:00:00Z",
                    }),
                    None => json!({"missing": name, "readTime": "2026-11-15T00:00:00Z"}),
                });
            }
            return FakeResponse::json(Value::Array(out).to_string());
        }
        if let Some(parent) = path.strip_suffix(":runQuery") {
            let parent = decode_path(parent);
            let from = &req.json()["structuredQuery"]["from"][0];
            let coll = from["collectionId"].as_str().unwrap_or_default();
            let all = from["allDescendants"] == json!(true);
            let prefix = format!("{parent}/");
            let mut out = Vec::new();
            for (name, (fields, _)) in &s.docs {
                let Some(rest) = name.strip_prefix(&prefix) else {
                    continue;
                };
                let segs: Vec<&str> = rest.split('/').collect();
                let hit = if all {
                    segs.len() >= 2 && segs[segs.len() - 2] == coll
                } else {
                    segs.len() == 2 && segs[0] == coll
                };
                if hit {
                    let created = s.created.get(name).cloned().unwrap_or_default();
                    out.push(json!({"document": {"name": name, "fields": fields, "createTime": created, "updateTime": created}, "readTime": "2026-11-15T00:00:00Z"}));
                }
            }
            if out.is_empty() {
                out.push(json!({"readTime": "2026-11-15T00:00:00Z"}));
            }
            return FakeResponse::json(Value::Array(out).to_string());
        }
        if req.method == "GET" {
            // The Firestore emulator never answers a transactional GET; fail loudly so a
            // regression back to it is caught here.
            if req
                .query
                .as_deref()
                .is_some_and(|q| q.contains("transaction="))
            {
                return err(400, "UNSUPPORTED");
            }
            let name = decode_path(path);
            return match s.docs.get(&name) {
                Some((fields, _)) => {
                    FakeResponse::json(json!({"name": name, "fields": fields}).to_string())
                }
                None => err(404, "NOT_FOUND"),
            };
        }
        err(400, "UNSUPPORTED")
    });
    Fake {
        server,
        abort_next,
        requests,
        state,
    }
}
