// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
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

//! The settings check that the batch processor and the plugin constructors run after the
//! defaults are applied.

use std::collections::BTreeSet;

use gadk_platform::{GoDuration, go_format_float_v, go_quote};

use crate::config::{Config, RetryConfig};
use crate::error::Error;
use crate::redaction::{COLUMN_ATTRIBUTES, parse_payload_column_denylist};
use crate::views::is_valid_identifier;

/// Rejects settings that cannot take effect and returns the `PayloadColumnDenylist` as a set.
/// The first failing check wins.
///
/// A negative `BatchFlushIntv`, `ShutdownTimeout` or `RetryConfig.InitialDelay` cannot occur: a
/// Rust [`Duration`](std::time::Duration) is never negative, so only the zero cases are checked.
///
/// # Errors
///
/// An [`Error`] whose text starts `invalid config: ` (the texts, with ADK's config field names,
/// match those of an ADK plugin).
pub(crate) fn validate_config(c: &Config) -> Result<BTreeSet<String>, Error> {
    let r = &c.retry_config;
    if c.batch_size < 1 || c.queue_max_size < 1 {
        return Err(Error::msg(
            "invalid config: batch_size and queue_max_size must be >= 1",
        ));
    }
    if c.batch_flush_intv.is_zero() || c.shutdown_timeout.is_zero() {
        return Err(Error::msg(
            "invalid config: batch_flush_intv and shutdown_timeout must be > 0",
        ));
    }
    if c.max_content_len == 0 || c.max_content_len < -1 {
        return Err(Error::msg(
            "invalid config: max_content_len must be -1 or >= 1",
        ));
    }
    if r.max_retries < 0 || r.multiplier < 1.0 || r.max_delay < r.initial_delay {
        return Err(Error::msg(format!(
            "invalid config: invalid retry_config: needs max_retries >= 0, initial_delay >= 0, \
             multiplier >= 1 and max_delay >= initial_delay, got {}",
            retry_config_plus_v(r)
        )));
    }
    let denied = parse_payload_column_denylist(&c.payload_column_denylist)
        .map_err(|e| Error::wrap("invalid config", e))?;
    if denied.contains(COLUMN_ATTRIBUTES) && !c.custom_metadata_allowlist.is_empty() {
        return Err(Error::msg(
            "invalid config: custom_metadata_allowlist cannot be used when payload_column_denylist \
             contains \"attributes\"",
        ));
    }
    if !c.view_prefix.is_empty() && !is_valid_identifier(&c.view_prefix) {
        return Err(Error::msg(format!(
            "invalid config: invalid view_prefix {}",
            go_quote(&c.view_prefix)
        )));
    }
    Ok(denied)
}

/// The retry settings as `{max_retries:3 initial_delay:100ms max_delay:10s multiplier:2}`.
fn retry_config_plus_v(r: &RetryConfig) -> String {
    format!(
        "{{max_retries:{} initial_delay:{} max_delay:{} multiplier:{}}}",
        r.max_retries,
        GoDuration::from_std(r.initial_delay),
        GoDuration::from_std(r.max_delay),
        go_format_float_v(r.multiplier)
    )
}

// The golden (`config/validate_config.json`) builds configs with the shared projection helper,
// so it lives under `tests/support/` beside it.
#[cfg(test)]
#[path = "../tests/support/golden_validate_config.rs"]
mod golden_validate_config;

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn retry_config_text_is_gos_plus_v() {
        let r = RetryConfig {
            max_retries: 1,
            initial_delay: std::time::Duration::from_nanos(5_400_001_500_000),
            max_delay: std::time::Duration::ZERO,
            multiplier: 2.25,
        };
        assert_eq!(
            retry_config_plus_v(&r),
            "{max_retries:1 initial_delay:1h30m0.0015s max_delay:0s multiplier:2.25}"
        );
    }
}
