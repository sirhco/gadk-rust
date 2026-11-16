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

//! Contract tests every [`PromptStore`] must pass. Enabled by the `testsuite` feature; call
//! [`run`] from a store's tests with a factory that builds a fresh, empty store.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::sync::Arc;

use crate::model::{Label, NewVersion, Selector, content_hash};
use crate::store::{PromptStore, StoreError};

macro_rules! check {
    ($cond:expr, $case:expr, $($arg:tt)*) => {
        if !$cond {
            panic!("prompts testsuite/{}: {}", $case, format!($($arg)*));
        }
    };
}

fn new(name: &str, template: &str) -> NewVersion {
    NewVersion {
        name: name.into(),
        template: template.into(),
        config: None,
        author: "tester".into(),
        message: "msg".into(),
    }
}

/// Runs every scenario, each against a fresh store from `factory`; panics with
/// `prompts testsuite/<scenario>: <message>` on the first failure.
pub async fn run<F, Fut>(factory: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn PromptStore>>,
{
    versions_and_noop(factory().await).await;
    labels_cas(factory().await).await;
    not_found(factory().await).await;
    remove_label(factory().await).await;
    invalid_input(factory().await).await;
    nested_names(factory().await).await;
}

async fn versions_and_noop(s: Arc<dyn PromptStore>) {
    let c = "versions_and_noop";
    let v1 = s.create_version(new("p", "one")).await.unwrap();
    check!(v1.version == 1, c, "first version {}", v1.version);
    check!(
        v1.meta.hash == content_hash("one", None),
        c,
        "hash {}",
        v1.meta.hash
    );
    check!(
        v1.meta.author == "tester" && v1.meta.message == "msg",
        c,
        "meta {:?}",
        v1.meta
    );
    let again = s.create_version(new("p", "one")).await.unwrap();
    check!(again == v1, c, "identical push returned {again:?}");
    let mut with_cfg = new("p", "one");
    with_cfg.config = Some(serde_json::json!({"t": 1}));
    let v2 = s.create_version(with_cfg).await.unwrap();
    check!(v2.version == 2, c, "config change gave {}", v2.version);
    let v3 = s.create_version(new("p", "three")).await.unwrap();
    check!(v3.version == 3, c, "third {}", v3.version);
    let snap = s.load_all().await.unwrap();
    let e = &snap.prompts["p"];
    check!(
        e.versions.keys().copied().collect::<Vec<_>>() == vec![1, 2, 3],
        c,
        "versions {:?}",
        e.versions.keys()
    );
    check!(*e.versions[&2] == v2, c, "stored v2 {:?}", e.versions[&2]);
    check!(
        snap.resolve("p", &Selector::Latest).unwrap().template == "three",
        c,
        "latest"
    );
}

async fn labels_cas(s: Arc<dyn PromptStore>) {
    let c = "labels_cas";
    s.create_version(new("p", "a")).await.unwrap();
    s.create_version(new("p", "b")).await.unwrap();
    let prod = Label::production();
    s.set_label("p", &prod, 1, Some(None)).await.unwrap();
    let r = s.set_label("p", &prod, 2, Some(None)).await;
    check!(
        r == Err(StoreError::Conflict {
            expected: None,
            actual: Some(1)
        }),
        c,
        "unset-expect {r:?}"
    );
    let r = s.set_label("p", &prod, 2, Some(Some(2))).await;
    check!(
        r == Err(StoreError::Conflict {
            expected: Some(2),
            actual: Some(1)
        }),
        c,
        "wrong-expect {r:?}"
    );
    s.set_label("p", &prod, 2, Some(Some(1))).await.unwrap();
    s.set_label("p", &Label::staging(), 1, None).await.unwrap();
    let snap = s.load_all().await.unwrap();
    check!(
        snap.prompts["p"].labels.get(&prod) == Some(&2),
        c,
        "labels {:?}",
        snap.prompts["p"].labels
    );
    check!(
        snap.prompts["p"].labels.get(&Label::staging()) == Some(&1),
        c,
        "staging"
    );
}

async fn not_found(s: Arc<dyn PromptStore>) {
    let c = "not_found";
    let prod = Label::production();
    let r = s.set_label("nope", &prod, 1, None).await;
    check!(
        matches!(r, Err(StoreError::NotFound { .. })),
        c,
        "unknown name {r:?}"
    );
    s.create_version(new("p", "a")).await.unwrap();
    let r = s.set_label("p", &prod, 7, None).await;
    check!(
        r == Err(StoreError::NotFound {
            name: "p".into(),
            selector: "v7".into()
        }),
        c,
        "unknown version {r:?}"
    );
    let r = s.remove_label("p", &prod).await;
    check!(
        r == Err(StoreError::NotFound {
            name: "p".into(),
            selector: "production".into()
        }),
        c,
        "unknown label {r:?}"
    );
}

async fn remove_label(s: Arc<dyn PromptStore>) {
    let c = "remove_label";
    s.create_version(new("p", "a")).await.unwrap();
    s.set_label("p", &Label::production(), 1, None)
        .await
        .unwrap();
    s.remove_label("p", &Label::production()).await.unwrap();
    let snap = s.load_all().await.unwrap();
    check!(
        snap.prompts["p"].labels.is_empty(),
        c,
        "labels {:?}",
        snap.prompts["p"].labels
    );
}

async fn invalid_input(s: Arc<dyn PromptStore>) {
    let c = "invalid_input";
    let r = s.create_version(new("Bad Name", "a")).await;
    check!(
        matches!(r, Err(StoreError::Invalid(_))),
        c,
        "bad name {r:?}"
    );
    let snap = s.load_all().await.unwrap();
    check!(
        snap.prompts.is_empty(),
        c,
        "store not empty: {:?}",
        snap.prompts.keys()
    );
}

async fn nested_names(s: Arc<dyn PromptStore>) {
    let c = "nested_names";
    s.create_version(new("team/a", "x")).await.unwrap();
    s.create_version(new("team/b", "y")).await.unwrap();
    s.create_version(new("team", "z")).await.unwrap();
    let snap = s.load_all().await.unwrap();
    let names: Vec<_> = snap.prompts.keys().cloned().collect();
    check!(
        names == vec!["team", "team/a", "team/b"],
        c,
        "names {names:?}"
    );
}
