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
//! Tests for the prompt registry: versions, labels and caching.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use gadk_prompts::{
    DEFAULT_REFRESH_INTERVAL, FileStore, Label, NewVersion, PromptRegistry, PromptSnapshot,
    PromptStore, PromptVersion, Rollout, RolloutSnapshot, Selector, StoreError,
};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::reader::MetricReader;
use opentelemetry_sdk::metrics::{
    InstrumentKind, ManualReader, Pipeline, SdkMeterProvider, Temporality,
};

/// Wraps a FileStore, counting load_all calls and failing them on demand. The default
/// `load_all_with_rollouts` calls `load_all`, then the forwarded `load_rollouts`.
struct Flaky {
    inner: FileStore,
    fail: AtomicBool,
    loads: Arc<AtomicUsize>,
}

#[async_trait]
impl PromptStore for Flaky {
    async fn load_all(&self) -> Result<PromptSnapshot, StoreError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(StoreError::Io("boom".into()));
        }
        self.inner.load_all().await
    }
    async fn create_version(&self, n: NewVersion) -> Result<PromptVersion, StoreError> {
        self.inner.create_version(n).await
    }
    async fn set_label(
        &self,
        n: &str,
        l: &Label,
        v: u32,
        e: Option<Option<u32>>,
    ) -> Result<(), StoreError> {
        self.inner.set_label(n, l, v, e).await
    }
    async fn remove_label(&self, n: &str, l: &Label) -> Result<(), StoreError> {
        self.inner.remove_label(n, l).await
    }
    async fn load_rollouts(&self) -> Result<RolloutSnapshot, StoreError> {
        self.inner.load_rollouts().await
    }
    async fn set_rollout(&self, n: &str, l: &Label, r: Option<Rollout>) -> Result<(), StoreError> {
        self.inner.set_rollout(n, l, r).await
    }
}

fn nv(name: &str, t: &str) -> NewVersion {
    NewVersion {
        name: name.into(),
        template: t.into(),
        config: None,
        author: String::new(),
        message: String::new(),
    }
}

#[derive(Debug, Clone)]
struct Shared(Arc<ManualReader>);

impl MetricReader for Shared {
    fn register_pipeline(&self, p: Weak<Pipeline>) {
        self.0.register_pipeline(p);
    }
    fn collect(&self, rm: &mut ResourceMetrics) -> OTelSdkResult {
        self.0.collect(rm)
    }
    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }
    fn shutdown_with_timeout(&self, t: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(t)
    }
    fn temporality(&self, k: InstrumentKind) -> Temporality {
        self.0.temporality(k)
    }
}

/// The value of the sum/gauge data point of metric `name`, if collected.
fn metric_value(reader: &Shared, name: &str) -> Option<f64> {
    let mut rm = ResourceMetrics::default();
    reader.collect(&mut rm).unwrap();
    for sm in rm.scope_metrics() {
        for m in sm.metrics() {
            if m.name() != name {
                continue;
            }
            return match m.data() {
                AggregatedMetrics::U64(MetricData::Sum(s)) => {
                    s.data_points().next().map(|p| p.value() as f64)
                }
                AggregatedMetrics::F64(MetricData::Gauge(g)) => {
                    g.data_points().next().map(|p| p.value())
                }
                _ => None,
            };
        }
    }
    None
}

#[tokio::test]
async fn registry_load_fails_when_store_fails() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Flaky {
        inner: FileStore::new(dir.path()),
        fail: AtomicBool::new(true),
        loads: Arc::default(),
    });
    assert!(PromptRegistry::builder(store).load().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn registry_load_default_interval_and_zero_disables() {
    assert_eq!(DEFAULT_REFRESH_INTERVAL, Duration::from_secs(30));
    let dir = tempfile::tempdir().unwrap();
    let loads = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(Flaky {
        inner: FileStore::new(dir.path()),
        fail: AtomicBool::new(false),
        loads: loads.clone(),
    });
    let _r = PromptRegistry::builder(store)
        .refresh_interval(Duration::ZERO)
        .load()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn registry_refresh_swaps_keeps_last_good_counts_errors_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let loads = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(Flaky {
        inner: FileStore::new(dir.path()),
        fail: AtomicBool::new(false),
        loads: loads.clone(),
    });
    store.create_version(nv("p", "one")).await.unwrap();
    let reader = Shared(Arc::new(ManualReader::builder().build()));
    let mp = SdkMeterProvider::builder()
        .with_reader(reader.clone())
        .build();
    let r = PromptRegistry::builder(store.clone())
        .refresh_interval(Duration::from_secs(10))
        .meter_provider(&mp)
        .load()
        .await
        .unwrap();
    assert_eq!(r.get("p", &Selector::Latest).unwrap().template, "one");

    store.create_version(nv("p", "two")).await.unwrap();
    tokio::time::sleep(Duration::from_secs(11)).await;
    tokio::task::yield_now().await;
    assert_eq!(r.get("p", &Selector::Latest).unwrap().template, "two");

    store.fail.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        r.get("p", &Selector::Latest).unwrap().template,
        "two",
        "last good kept"
    );
    assert_eq!(
        metric_value(&reader, "gadk.prompts.refresh.errors"),
        Some(1.0)
    );
    let age = metric_value(&reader, "gadk.prompts.snapshot.age").unwrap();
    assert!(age >= 10.0, "age {age}");

    drop(r);
    tokio::task::yield_now().await;
    let after = loads.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(100)).await;
    assert_eq!(
        loads.load(Ordering::SeqCst),
        after,
        "refresh task stopped after last handle dropped"
    );
}

#[tokio::test(start_paused = true)]
async fn registry_close_stops_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let loads = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(Flaky {
        inner: FileStore::new(dir.path()),
        fail: AtomicBool::new(false),
        loads: loads.clone(),
    });
    let r = PromptRegistry::builder(store)
        .refresh_interval(Duration::from_secs(1))
        .load()
        .await
        .unwrap();
    r.close();
    tokio::task::yield_now().await;
    let after = loads.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(loads.load(Ordering::SeqCst), after);
}

#[tokio::test]
async fn registry_get_resolves_and_misses() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path()));
    store.create_version(nv("p", "a")).await.unwrap();
    store.create_version(nv("p", "b")).await.unwrap();
    store
        .set_label("p", &Label::production(), 1, None)
        .await
        .unwrap();
    let r = PromptRegistry::builder(store)
        .refresh_interval(Duration::ZERO)
        .load()
        .await
        .unwrap();
    assert_eq!(
        r.get("p", &Selector::Label(Label::production()))
            .unwrap()
            .version,
        1
    );
    assert_eq!(r.get("p", &Selector::Version(2)).unwrap().template, "b");
    assert_eq!(r.get("p", &Selector::Latest).unwrap().version, 2);
    for (name, sel) in [
        ("q", Selector::Latest),
        ("p", Selector::Label(Label::staging())),
        ("p", Selector::Version(5)),
    ] {
        assert!(
            matches!(r.get(name, &sel), Err(StoreError::NotFound { .. })),
            "{name} {sel}"
        );
    }
}

/// R8: `get_for` serves the rollout from the same load as the snapshot, a failed refresh keeps
/// the last pair, and `get` never applies a rollout.
#[tokio::test]
async fn registry_get_for_serves_rollouts_and_keeps_last_pair() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Flaky {
        inner: FileStore::new(dir.path()),
        fail: AtomicBool::new(false),
        loads: Arc::default(),
    });
    store.create_version(nv("p", "a")).await.unwrap();
    store.create_version(nv("p", "b")).await.unwrap();
    let prod = Label::production();
    store.set_label("p", &prod, 1, None).await.unwrap();
    let rollout = Rollout::new(2, 50).unwrap();
    store.set_rollout("p", &prod, Some(rollout)).await.unwrap();
    let r = PromptRegistry::builder(store.clone())
        .refresh_interval(Duration::ZERO)
        .load()
        .await
        .unwrap();
    assert_eq!(r.rollouts().get("p", &prod), Some(&rollout));

    let sel = Selector::Label(prod.clone());
    let keys: Vec<String> = (0..100).map(|i| format!("session-{i}")).collect();
    let served = |r: &PromptRegistry| -> Vec<u32> {
        keys.iter()
            .map(|k| r.get_for("p", &sel, k).unwrap().version)
            .collect()
    };
    let before = served(&r);
    assert!(before.contains(&1) && before.contains(&2), "{before:?}");
    assert_eq!(before, served(&r), "same key, same version");
    assert_eq!(r.get("p", &sel).unwrap().version, 1, "get ignores rollouts");
    assert_eq!(
        r.get_for("p", &Selector::Latest, &keys[0]).unwrap(),
        r.get("p", &Selector::Latest).unwrap()
    );

    store.inner.set_rollout("p", &prod, None).await.unwrap();
    store.fail.store(true, Ordering::SeqCst);
    assert!(r.refresh().await.is_err());
    assert_eq!(
        r.rollouts().get("p", &prod),
        Some(&rollout),
        "last pair kept"
    );
    assert_eq!(served(&r), before);

    store.fail.store(false, Ordering::SeqCst);
    r.refresh().await.unwrap();
    assert!(r.rollouts().is_empty());
    assert!(served(&r).iter().all(|&v| v == 1));
}
