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
//! The cached, refreshing prompt registry.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use arc_swap::ArcSwap;
use opentelemetry::metrics::MeterProvider;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::metrics::Metrics;
use crate::model::{PromptVersion, Selector};
use crate::provider::PromptInstruction;
use crate::rollout::{RolloutSnapshot, bucket};
use crate::snapshot::PromptSnapshot;
use crate::store::{PromptStore, StoreError};

/// How often a registry reloads its store by default.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// The prompt and rollout snapshots of one store load.
type Loaded = (Arc<PromptSnapshot>, Arc<RolloutSnapshot>);

pub(crate) struct Inner {
    store: Arc<dyn PromptStore>,
    /// Swapped as one value, so readers never mix two loads.
    pub(crate) loaded: ArcSwap<Loaded>,
    last_ok: Mutex<Instant>,
    cancel: CancellationToken,
    metrics: Mutex<Option<Metrics>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Inner {
    fn age_secs(&self) -> f64 {
        self.last_ok
            .lock()
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    async fn refresh(&self) -> Result<(), StoreError> {
        match self.store.load_all_with_rollouts().await {
            Ok((s, r)) => {
                self.loaded.store(Arc::new((Arc::new(s), Arc::new(r))));
                if let Ok(mut t) = self.last_ok.lock() {
                    *t = Instant::now();
                }
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, "prompt registry refresh failed; keeping the last good snapshot");
                if let Ok(m) = self.metrics.lock()
                    && let Some(m) = m.as_ref()
                {
                    m.refresh_errors.add(1, &[]);
                }
                Err(e)
            }
        }
    }
}

/// Prompts served from an in-memory snapshot of a [`PromptStore`], refreshed in the
/// background. Cheap to clone; the refresh task stops on [`close`](Self::close) or when the
/// last clone is dropped.
#[derive(Clone)]
pub struct PromptRegistry {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for PromptRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptRegistry")
            .field("prompts", &self.inner.loaded.load().0.prompts.len())
            .finish_non_exhaustive()
    }
}

/// Configures and loads a [`PromptRegistry`].
pub struct PromptRegistryBuilder {
    store: Arc<dyn PromptStore>,
    interval: Duration,
    meter: Option<opentelemetry::metrics::Meter>,
}

impl PromptRegistryBuilder {
    /// How often to reload the store ([`DEFAULT_REFRESH_INTERVAL`]); zero loads once.
    pub fn refresh_interval(mut self, d: Duration) -> Self {
        self.interval = d;
        self
    }

    /// Records the registry's metrics through `provider` instead of the global meter provider.
    ///
    /// Without it the instruments bind to the global meter provider when [`load`](Self::load)
    /// runs, so a provider installed globally afterwards never sees them: build the registry
    /// after installing telemetry providers, or pass the provider here.
    pub fn meter_provider<P: MeterProvider + ?Sized>(mut self, provider: &P) -> Self {
        self.meter = Some(provider.meter("gadk.prompts"));
        self
    }

    /// Loads the store's prompts and rollouts once and starts the refresh task (when the
    /// interval is non-zero).
    ///
    /// The metric instruments are created here, from the [`meter_provider`](Self::meter_provider)
    /// or else the global meter provider as it is at this moment.
    ///
    /// # Errors
    ///
    /// The initial load's error.
    pub async fn load(self) -> Result<PromptRegistry, StoreError> {
        let (prompts, rollouts) = self.store.load_all_with_rollouts().await?;
        let inner = Arc::new(Inner {
            store: self.store,
            loaded: ArcSwap::from_pointee((Arc::new(prompts), Arc::new(rollouts))),
            last_ok: Mutex::new(Instant::now()),
            cancel: CancellationToken::new(),
            metrics: Mutex::new(None),
        });
        let meter = self
            .meter
            .unwrap_or_else(|| opentelemetry::global::meter("gadk.prompts"));
        let metrics = Metrics::new(&meter, Arc::downgrade(&inner), Inner::age_secs);
        if let Ok(mut m) = inner.metrics.lock() {
            *m = Some(metrics);
        }
        if !self.interval.is_zero() {
            spawn_refresh(Arc::downgrade(&inner), inner.cancel.clone(), self.interval);
        }
        Ok(PromptRegistry { inner })
    }
}

fn spawn_refresh(weak: Weak<Inner>, cancel: CancellationToken, every: Duration) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(every) => {}
            }
            let Some(inner) = weak.upgrade() else { return };
            let _ = inner.refresh().await;
        }
    });
}

impl PromptRegistry {
    /// A builder over `store`.
    pub fn builder(store: Arc<dyn PromptStore>) -> PromptRegistryBuilder {
        PromptRegistryBuilder {
            store,
            interval: DEFAULT_REFRESH_INTERVAL,
            meter: None,
        }
    }

    /// The version `sel` selects in prompt `name`, from the current snapshot. Never applies a
    /// rollout; see [`get_for`](Self::get_for).
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown name, label or version.
    pub fn get(&self, name: &str, sel: &Selector) -> Result<Arc<PromptVersion>, StoreError> {
        self.inner.loaded.load().0.resolve(name, sel)
    }

    /// The version served to bucket key `key` (a session id) for prompt `name` at `sel`.
    ///
    /// For [`Selector::Label`] with a rollout on that label whose version is in the snapshot,
    /// the rollout version is served when the key's bucket (0-99, from SHA-256 of the name,
    /// label and key) is below the rollout's percent, so a key always lands on the same side
    /// and raising the percent only moves keys onto the rollout version. Otherwise it equals
    /// [`get`](Self::get).
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown name, label or version.
    pub fn get_for(
        &self,
        name: &str,
        sel: &Selector,
        key: &str,
    ) -> Result<Arc<PromptVersion>, StoreError> {
        let loaded = self.inner.loaded.load();
        resolve_for(&loaded.0, &loaded.1, name, sel, key).map(|(v, _)| v)
    }

    /// The current snapshot.
    pub fn snapshot(&self) -> Arc<PromptSnapshot> {
        self.inner.loaded.load().0.clone()
    }

    /// The current rollouts, from the same load as [`snapshot`](Self::snapshot).
    pub fn rollouts(&self) -> Arc<RolloutSnapshot> {
        self.inner.loaded.load().1.clone()
    }

    /// Reloads the store now.
    ///
    /// # Errors
    ///
    /// The load's error (the previous snapshot and rollouts are kept).
    pub async fn refresh(&self) -> Result<(), StoreError> {
        self.inner.refresh().await
    }

    /// An instruction source for prompt `name` at `sel`; see [`PromptInstruction`].
    pub fn instruction(&self, name: &str, sel: Selector) -> PromptInstruction {
        PromptInstruction::new(self.clone(), name.to_owned(), sel)
    }

    /// Stops the refresh task. The registry keeps serving its last snapshot.
    pub fn close(&self) {
        self.inner.cancel.cancel();
    }
}

/// [`PromptSnapshot::resolve`] with `sel`'s label rollout applied for bucket key `key`; the flag
/// is true when the rollout version was served.
pub(crate) fn resolve_for(
    snap: &PromptSnapshot,
    rollouts: &RolloutSnapshot,
    name: &str,
    sel: &Selector,
    key: &str,
) -> Result<(Arc<PromptVersion>, bool), StoreError> {
    let resolved = snap.resolve(name, sel)?;
    if let Selector::Label(label) = sel
        && let Some(r) = rollouts.get(name, label)
        && r.version != resolved.version
        && let Some(v) = snap
            .prompts
            .get(name)
            .and_then(|e| e.versions.get(&r.version))
        && bucket(name, label, key) < r.percent
    {
        return Ok((v.clone(), true));
    }
    Ok((resolved, false))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::model::{Label, VersionMeta, content_hash};
    use crate::rollout::{Rollout, bucket};
    use crate::snapshot::PromptEntry;

    fn v(n: u32) -> Arc<PromptVersion> {
        Arc::new(PromptVersion {
            name: "p".into(),
            version: n,
            template: format!("t{n}"),
            config: None,
            meta: VersionMeta {
                created_at: chrono::Utc::now(),
                author: String::new(),
                message: String::new(),
                hash: content_hash(&format!("t{n}"), None),
            },
        })
    }

    /// Prompt `p` with versions 1-4, `production` at v3 and `staging` at v2.
    fn snap() -> PromptSnapshot {
        let mut e = PromptEntry::default();
        for n in 1..=4 {
            e.versions.insert(n, v(n));
        }
        e.labels.insert(Label::production(), 3);
        e.labels.insert(Label::staging(), 2);
        let mut s = PromptSnapshot::default();
        s.prompts.insert("p".into(), e);
        s
    }

    fn rollouts(version: u32, percent: u8) -> RolloutSnapshot {
        let mut r = RolloutSnapshot::default();
        r.insert(
            "p",
            Label::production(),
            Rollout::new(version, percent).unwrap(),
        );
        r
    }

    fn keys() -> impl Iterator<Item = String> {
        (0..1000).map(|i| format!("session-{i}"))
    }

    #[test]
    fn resolve_for_serves_the_rollout_below_percent_and_is_deterministic() {
        let (s, r) = (snap(), rollouts(4, 10));
        let prod = Selector::Label(Label::production());
        let mut hits = 0;
        for key in keys() {
            let rolled = bucket("p", &Label::production(), &key) < 10;
            let (got, flag) = resolve_for(&s, &r, "p", &prod, &key).unwrap();
            assert_eq!(got.version, if rolled { 4 } else { 3 }, "{key}");
            assert_eq!(flag, rolled, "{key}");
            let (again, flag_again) = resolve_for(&s, &r, "p", &prod, &key).unwrap();
            assert_eq!((again.version, flag_again), (got.version, flag), "{key}");
            hits += usize::from(rolled);
        }
        assert!((1..1000).contains(&hits), "both sides served: {hits}");
    }

    #[test]
    fn resolve_for_raising_percent_keeps_rollout_keys() {
        let (s, ten, fifty) = (snap(), rollouts(4, 10), rollouts(4, 50));
        let prod = Selector::Label(Label::production());
        for key in keys() {
            let (at_ten, _) = resolve_for(&s, &ten, "p", &prod, &key).unwrap();
            let (at_fifty, _) = resolve_for(&s, &fifty, "p", &prod, &key).unwrap();
            if at_ten.version == 4 {
                assert_eq!(at_fifty.version, 4, "{key} moved back");
            }
        }
    }

    #[test]
    fn resolve_for_falls_back_to_resolve() {
        let s = snap();
        let prod = Label::production();
        // A key in the rollout share at 99%, so only the fallback rules can keep v3.
        let key = keys().find(|k| bucket("p", &prod, k) < 99).unwrap();
        let cases = [
            // no rollouts at all
            (RolloutSnapshot::default(), Selector::Label(prod.clone())),
            // a rollout version missing from the snapshot
            (rollouts(9, 99), Selector::Label(prod.clone())),
            // other selectors ignore the label's rollout
            (rollouts(4, 99), Selector::Latest),
            (rollouts(4, 99), Selector::Version(1)),
            // a label without a rollout
            (rollouts(4, 99), Selector::Label(Label::staging())),
        ];
        for (r, sel) in cases {
            let (got, flag) = resolve_for(&s, &r, "p", &sel, &key).unwrap();
            assert_eq!(got, s.resolve("p", &sel).unwrap(), "{sel}");
            assert!(!flag, "{sel}");
        }
        let r = rollouts(4, 99);
        for (name, sel) in [
            ("q", Selector::Label(prod.clone())),
            ("p", Selector::Label(Label::new("canary").unwrap())),
        ] {
            assert_eq!(
                resolve_for(&s, &r, name, &sel, &key).unwrap_err(),
                s.resolve(name, &sel).unwrap_err(),
                "{name} {sel}"
            );
        }
    }
}
