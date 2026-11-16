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
use crate::snapshot::PromptSnapshot;
use crate::store::{PromptStore, StoreError};

/// How often a registry reloads its store by default.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) struct Inner {
    store: Arc<dyn PromptStore>,
    snapshot: ArcSwap<PromptSnapshot>,
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
        match self.store.load_all().await {
            Ok(s) => {
                self.snapshot.store(Arc::new(s));
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
            .field("prompts", &self.inner.snapshot.load().prompts.len())
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

    /// Loads the store once and starts the refresh task (when the interval is non-zero).
    ///
    /// The metric instruments are created here, from the [`meter_provider`](Self::meter_provider)
    /// or else the global meter provider as it is at this moment.
    ///
    /// # Errors
    ///
    /// The initial load's error.
    pub async fn load(self) -> Result<PromptRegistry, StoreError> {
        let first = self.store.load_all().await?;
        let inner = Arc::new(Inner {
            store: self.store,
            snapshot: ArcSwap::from_pointee(first),
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

    /// The version `sel` selects in prompt `name`, from the current snapshot.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown name, label or version.
    pub fn get(&self, name: &str, sel: &Selector) -> Result<Arc<PromptVersion>, StoreError> {
        self.inner.snapshot.load().resolve(name, sel)
    }

    /// The current snapshot.
    pub fn snapshot(&self) -> Arc<PromptSnapshot> {
        self.inner.snapshot.load_full()
    }

    /// Reloads the store now.
    ///
    /// # Errors
    ///
    /// The load's error (the previous snapshot is kept).
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
