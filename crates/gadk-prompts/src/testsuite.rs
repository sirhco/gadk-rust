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
//! [`run`] from a store's tests with a factory that builds a fresh, empty store, and also
//! [`run_rollouts`] when the store supports rollouts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::future::Future;
use std::sync::Arc;

use crate::model::{Label, NewVersion, Selector, content_hash};
use crate::rollout::{Rollout, RolloutSnapshot};
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
    reporting(factory().await).await;
    labels_cas(factory().await).await;
    not_found(factory().await).await;
    remove_label(factory().await).await;
    invalid_input(factory().await).await;
    nested_names(factory().await).await;
}

/// Runs the rollout scenario against a fresh store from `factory` (for stores that support
/// rollouts); panics with `prompts testsuite/rollouts: <message>` on the first failure.
pub async fn run_rollouts<F, Fut>(factory: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn PromptStore>>,
{
    rollouts(factory().await).await;
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

async fn reporting(s: Arc<dyn PromptStore>) {
    let c = "reporting";
    let (v1, created) = s.create_version_reporting(new("p", "one")).await.unwrap();
    check!(
        created && v1.version == 1,
        c,
        "first: {created} v{}",
        v1.version
    );
    let (again, created) = s.create_version_reporting(new("p", "one")).await.unwrap();
    check!(
        !created && again == v1,
        c,
        "same content: {created} {again:?}"
    );
    let (v2, created) = s.create_version_reporting(new("p", "two")).await.unwrap();
    check!(
        created && v2.version == 2,
        c,
        "changed: {created} v{}",
        v2.version
    );
    let (v3, created) = s.create_version_reporting(new("q", "one")).await.unwrap();
    check!(
        created && v3.version == 1,
        c,
        "other prompt: {created} v{}",
        v3.version
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

/// A rollout built without [`Rollout::new`]'s checks, to hand invalid values to a store.
fn raw(version: u32, percent: u8) -> Rollout {
    Rollout { version, percent }
}

async fn rollouts(s: Arc<dyn PromptStore>) {
    let c = "rollouts";
    for t in ["a", "b", "c"] {
        s.create_version(new("p", t)).await.unwrap();
    }
    let (prod, staging) = (Label::production(), Label::staging());
    s.set_label("p", &prod, 1, None).await.unwrap();
    let not_found = |name: &str, selector: &str| {
        Err(StoreError::NotFound {
            name: name.into(),
            selector: selector.into(),
        })
    };

    // Errors (R4).
    for bad in [raw(2, 0), raw(2, 100), raw(0, 10)] {
        let r = s.set_rollout("p", &prod, Some(bad)).await;
        check!(
            matches!(r, Err(StoreError::Invalid(_))),
            c,
            "out of bounds {bad:?}: {r:?}"
        );
    }
    let r = s.set_rollout("nope", &prod, Some(raw(2, 10))).await;
    check!(r == not_found("nope", ""), c, "unknown prompt {r:?}");
    let r = s.set_rollout("p", &staging, Some(raw(2, 10))).await;
    check!(r == not_found("p", "staging"), c, "unknown label {r:?}");
    let r = s.set_rollout("p", &prod, Some(raw(9, 10))).await;
    check!(r == not_found("p", "v9"), c, "unknown version {r:?}");
    let r = s.set_rollout("p", &prod, Some(raw(1, 10))).await;
    check!(
        matches!(r, Err(StoreError::Invalid(_))),
        c,
        "label's own version {r:?}"
    );
    let r = s.set_rollout("nope", &prod, None).await;
    check!(
        r == not_found("nope", ""),
        c,
        "remove, unknown prompt {r:?}"
    );
    let r = s.set_rollout("p", &staging, None).await;
    check!(
        r == not_found("p", "staging"),
        c,
        "remove, unknown label {r:?}"
    );
    let r = s.set_rollout("p", &prod, None).await;
    check!(r == Ok(()), c, "remove without a rollout {r:?}");
    let got = s.load_rollouts().await.unwrap();
    check!(got.is_empty(), c, "after errors: {got:?}");

    // Start and replace.
    s.set_rollout("p", &prod, Some(raw(2, 10))).await.unwrap();
    let got = s.load_rollouts().await.unwrap();
    check!(
        got.get("p", &prod) == Some(&raw(2, 10)),
        c,
        "started: {got:?}"
    );
    s.set_rollout("p", &prod, Some(raw(3, 50))).await.unwrap();

    // Round trip through both loads, with a second prompt and a second label.
    s.create_version(new("q", "x")).await.unwrap();
    s.create_version(new("q", "y")).await.unwrap();
    s.set_label("q", &staging, 2, None).await.unwrap();
    s.set_rollout("q", &staging, Some(raw(1, 99)))
        .await
        .unwrap();
    let mut want = RolloutSnapshot::default();
    want.insert("p", prod.clone(), raw(3, 50));
    want.insert("q", staging.clone(), raw(1, 99));
    let (snap, got) = s.load_all_with_rollouts().await.unwrap();
    check!(got == want, c, "load_all_with_rollouts: {got:?}");
    check!(
        snap == s.load_all().await.unwrap(),
        c,
        "load_all_with_rollouts prompts differ from load_all"
    );
    check!(
        snap.prompts["p"].labels.get(&prod) == Some(&1),
        c,
        "a rollout moved the label: {:?}",
        snap.prompts["p"].labels
    );
    let got = s.load_rollouts().await.unwrap();
    check!(got == want, c, "load_rollouts: {got:?}");

    // create_version keeps every rollout (R5).
    s.create_version(new("p", "d")).await.unwrap();
    let got = s.load_rollouts().await.unwrap();
    check!(got == want, c, "after create_version: {got:?}");

    // None removes it.
    s.set_rollout("p", &prod, None).await.unwrap();
    let got = s.load_rollouts().await.unwrap();
    check!(
        got.get("p", &prod).is_none() && got.get("q", &staging) == Some(&raw(1, 99)),
        c,
        "after remove: {got:?}"
    );

    // set_label clears that label's rollout when it keeps the version (R5) ...
    s.set_rollout("p", &prod, Some(raw(2, 10))).await.unwrap();
    s.set_label("p", &prod, 1, None).await.unwrap();
    let got = s.load_rollouts().await.unwrap();
    check!(
        got.get("p", &prod).is_none(),
        c,
        "after set_label to the same version: {got:?}"
    );
    // ... and when it moves the label, leaving other labels' rollouts alone.
    s.set_label("p", &staging, 1, None).await.unwrap();
    s.set_rollout("p", &staging, Some(raw(4, 20)))
        .await
        .unwrap();
    s.set_rollout("p", &prod, Some(raw(2, 10))).await.unwrap();
    s.set_label("p", &prod, 3, Some(Some(1))).await.unwrap();
    let got = s.load_rollouts().await.unwrap();
    check!(
        got.get("p", &prod).is_none() && got.get("p", &staging) == Some(&raw(4, 20)),
        c,
        "after set_label to another version: {got:?}"
    );

    // remove_label clears it, and setting the label again does not bring it back (R5).
    s.remove_label("p", &staging).await.unwrap();
    let got = s.load_rollouts().await.unwrap();
    check!(
        got.get("p", &staging).is_none(),
        c,
        "after remove_label: {got:?}"
    );
    s.set_label("p", &staging, 1, None).await.unwrap();
    let got = s.load_rollouts().await.unwrap();
    check!(
        got.get("p", &staging).is_none(),
        c,
        "after setting the removed label again: {got:?}"
    );
    let (snap, got) = s.load_all_with_rollouts().await.unwrap();
    want = RolloutSnapshot::default();
    want.insert("q", staging.clone(), raw(1, 99));
    check!(got == want, c, "final rollouts: {got:?}");
    check!(
        snap.prompts["p"].labels.get(&prod) == Some(&3)
            && snap.prompts["p"].labels.get(&staging) == Some(&1),
        c,
        "final labels {:?}",
        snap.prompts["p"].labels
    );
}
