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

//! [`TimeRange`], the trace-list time window, and [`TraceId`].

use serde::{Serialize, Serializer};

/// The trace-list time window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeRange {
    /// The last hour (the default).
    #[default]
    Hour1,
    /// The last 6 hours.
    Hours6,
    /// The last 24 hours.
    Hours24,
    /// The last 7 days.
    Days7,
}

impl TimeRange {
    /// Every range, shortest first.
    pub const ALL: [TimeRange; 4] = [
        TimeRange::Hour1,
        TimeRange::Hours6,
        TimeRange::Hours24,
        TimeRange::Days7,
    ];

    /// "1h" | "6h" | "24h" | "7d"; anything else is None.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// The query-string form: "1h", "6h", "24h" or "7d".
    pub fn as_str(self) -> &'static str {
        match self {
            TimeRange::Hour1 => "1h",
            TimeRange::Hours6 => "6h",
            TimeRange::Hours24 => "24h",
            TimeRange::Days7 => "7d",
        }
    }

    /// The window length.
    pub fn duration(self) -> chrono::TimeDelta {
        match self {
            TimeRange::Hour1 => chrono::TimeDelta::hours(1),
            TimeRange::Hours6 => chrono::TimeDelta::hours(6),
            TimeRange::Hours24 => chrono::TimeDelta::hours(24),
            TimeRange::Days7 => chrono::TimeDelta::days(7),
        }
    }
}

/// Serializes as the query-string form ([`TimeRange::as_str`]): `"1h"`, `"6h"`, `"24h"` or
/// `"7d"`.
impl Serialize for TimeRange {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A trace id: exactly 32 lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct TraceId(String);

impl TraceId {
    /// Accepts 32 ASCII hex digits (either case) and lowercases them; anything else is None.
    pub fn parse(s: &str) -> Option<Self> {
        (s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| TraceId(s.to_ascii_lowercase()))
    }

    /// The 32 lowercase hex characters.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_range_parses_its_own_strings() {
        for r in TimeRange::ALL {
            assert_eq!(TimeRange::parse(r.as_str()), Some(r));
        }
        let strs: Vec<&str> = TimeRange::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(strs, ["1h", "6h", "24h", "7d"]);
    }

    #[test]
    fn time_range_rejects_other_strings() {
        for s in ["", "1H", "2h", "1d", "7D", " 1h", "1h ", "hour"] {
            assert_eq!(TimeRange::parse(s), None, "{s:?}");
        }
    }

    #[test]
    fn trace_id_accepts_32_hex_and_lowercases() {
        let lower = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            TraceId::parse(lower).map(|t| t.as_str().to_owned()),
            Some(lower.to_owned())
        );
        let upper = "0123456789ABCDEF0123456789ABCDEF";
        assert_eq!(
            TraceId::parse(upper).map(|t| t.as_str().to_owned()),
            Some(lower.to_owned())
        );
        assert_eq!(TraceId::parse(upper), TraceId::parse(lower));
    }

    #[test]
    fn trace_id_rejects_bad_ids() {
        for s in [
            "",
            "0123456789abcdef0123456789abcde",   // 31
            "0123456789abcdef0123456789abcdef0", // 33
            "0123456789abcdef0123456789abcdeg",  // not hex
            "0123456789abcdef-123456789abcdef",  // dash
            "0123456789abcdef0123456789abcdé",   // 32 bytes, not ASCII
            " 123456789abcdef0123456789abcdef",  // space
        ] {
            assert_eq!(TraceId::parse(s), None, "{s:?}");
        }
    }
}
