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
//! The registry's OpenTelemetry instruments (meter `gadk.prompts`).

use std::sync::Weak;

use opentelemetry::metrics::{Counter, Meter, ObservableGauge};

/// Instruments bound to one registry.
pub(crate) struct Metrics {
    pub(crate) refresh_errors: Counter<u64>,
    _age: ObservableGauge<f64>,
}

impl Metrics {
    /// `age` is called on each collection with a weak handle; `None` (registry gone) reports
    /// nothing.
    pub(crate) fn new<T: Send + Sync + 'static>(
        meter: &Meter,
        state: Weak<T>,
        age: fn(&T) -> f64,
    ) -> Self {
        let refresh_errors = meter
            .u64_counter("gadk.prompts.refresh.errors")
            .with_description("Failed prompt registry refreshes")
            .build();
        let gauge = meter
            .f64_observable_gauge("gadk.prompts.snapshot.age")
            .with_unit("s")
            .with_description("Seconds since the prompt snapshot was last loaded successfully")
            .with_callback(move |obs| {
                if let Some(s) = state.upgrade() {
                    obs.observe(age(&s), &[]);
                }
            })
            .build();
        Self {
            refresh_errors,
            _age: gauge,
        }
    }
}
