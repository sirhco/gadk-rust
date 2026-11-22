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

//! Rollouts: a label serving a second version to a share of sessions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::Label;
use crate::store::StoreError;

/// A label's rollout: `percent` of sessions get `version` instead of the label's version.
///
/// Serializes as `{"version": N, "percent": P}`; deserializing checks the same bounds as
/// [`Rollout::new`], so stored data outside them fails to load.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawRollout")]
pub struct Rollout {
    /// The version served to the rollout share of sessions.
    pub version: u32,
    /// The share of sessions, in percent (1 to 99).
    pub percent: u8,
}

/// The unchecked serialized form of a [`Rollout`].
#[derive(Deserialize)]
struct RawRollout {
    version: u32,
    percent: u8,
}

impl Rollout {
    /// A rollout of `version` to `percent` of sessions.
    ///
    /// # Errors
    ///
    /// `Invalid` unless `version >= 1` and `1 <= percent <= 99`.
    pub fn new(version: u32, percent: u8) -> Result<Self, StoreError> {
        if version == 0 {
            return Err(StoreError::Invalid(
                "rollout version must be at least 1".into(),
            ));
        }
        if !(1..=99).contains(&percent) {
            return Err(StoreError::Invalid(format!(
                "rollout percent must be between 1 and 99, got {percent}"
            )));
        }
        Ok(Self { version, percent })
    }
}

impl TryFrom<RawRollout> for Rollout {
    type Error = StoreError;
    fn try_from(raw: RawRollout) -> Result<Self, StoreError> {
        Rollout::new(raw.version, raw.percent)
    }
}

/// Every rollout, by prompt name and label.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RolloutSnapshot {
    rollouts: BTreeMap<String, BTreeMap<Label, Rollout>>,
}

impl RolloutSnapshot {
    /// The rollout on label `label` of prompt `name`, if any.
    pub fn get(&self, name: &str, label: &Label) -> Option<&Rollout> {
        self.rollouts.get(name).and_then(|m| m.get(label))
    }

    /// Sets the rollout on label `label` of prompt `name`, replacing any previous one.
    pub fn insert(&mut self, name: impl Into<String>, label: Label, rollout: Rollout) {
        self.rollouts
            .entry(name.into())
            .or_default()
            .insert(label, rollout);
    }

    /// Whether there are no rollouts.
    pub fn is_empty(&self) -> bool {
        self.rollouts.values().all(BTreeMap::is_empty)
    }

    /// Every `(name, label, rollout)`, sorted by name, then label.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Label, &Rollout)> + '_ {
        self.rollouts
            .iter()
            .flat_map(|(name, m)| m.iter().map(move |(l, r)| (name.as_str(), l, r)))
    }
}

/// The bucket (0 to 99) of session `key` for label `label` of prompt `name`: the first 8 bytes
/// of SHA-256(`name` 0x00 `label` 0x00 `key`) as a big-endian `u64`, modulo 100. The rollout
/// version is served when the bucket is below the rollout's percent.
pub(crate) fn bucket(name: &str, label: &Label, key: &str) -> u8 {
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    h.update([0u8]);
    h.update(label.as_str().as_bytes());
    h.update([0u8]);
    h.update(key.as_bytes());
    let digest = h.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest[..8]);
    // `% 100` is below 100, so the cast is lossless.
    (u64::from_be_bytes(first) % 100) as u8
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn new_checks_bounds() {
        for (v, p) in [(4, 0), (4, 100), (0, 10), (4, 255)] {
            assert!(
                matches!(Rollout::new(v, p), Err(StoreError::Invalid(_))),
                "({v}, {p})"
            );
        }
        let r = Rollout::new(4, 1).unwrap();
        assert_eq!((r.version, r.percent), (4, 1));
        let r = Rollout::new(4, 99).unwrap();
        assert_eq!((r.version, r.percent), (4, 99));
    }

    #[test]
    fn serde_round_trip_and_rejects_out_of_bounds() {
        let r = Rollout::new(4, 10).unwrap();
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"version":4,"percent":10}"#);
        assert_eq!(serde_json::from_str::<Rollout>(&json).unwrap(), r);
        let err = serde_json::from_str::<Rollout>(r#"{"version":4,"percent":100}"#).unwrap_err();
        assert!(err.to_string().contains("percent"), "{err}");
        let err = serde_json::from_str::<Rollout>(r#"{"version":0,"percent":10}"#).unwrap_err();
        assert!(err.to_string().contains("version"), "{err}");
    }

    #[test]
    fn snapshot_insert_get_iter() {
        let mut s = RolloutSnapshot::default();
        assert!(s.is_empty());
        let (prod, staging) = (Label::production(), Label::staging());
        s.insert("q", prod.clone(), Rollout::new(2, 50).unwrap());
        s.insert("p", staging.clone(), Rollout::new(3, 20).unwrap());
        s.insert("p", prod.clone(), Rollout::new(4, 10).unwrap());
        assert!(!s.is_empty());
        assert_eq!(s.get("p", &prod), Some(&Rollout::new(4, 10).unwrap()));
        assert_eq!(s.get("p", &Label::new("canary").unwrap()), None);
        assert_eq!(s.get("r", &prod), None);
        s.insert("p", prod.clone(), Rollout::new(5, 30).unwrap());
        let all: Vec<_> = s
            .iter()
            .map(|(n, l, r)| (n.to_owned(), l.as_str().to_owned(), r.version, r.percent))
            .collect();
        assert_eq!(
            all,
            vec![
                ("p".to_owned(), "production".to_owned(), 5, 30),
                ("p".to_owned(), "staging".to_owned(), 3, 20),
                ("q".to_owned(), "production".to_owned(), 2, 50),
            ]
        );
    }

    #[test]
    fn bucket_follows_the_formula_and_is_deterministic() {
        let prod = Label::production();
        let digest = Sha256::digest(b"p\x00production\x00session-1");
        let want = u64::from_be_bytes(digest[..8].try_into().unwrap()) % 100;
        assert_eq!(u64::from(bucket("p", &prod, "session-1")), want);
        for i in 0..100 {
            let key = format!("session-{i}");
            let b = bucket("p", &prod, &key);
            assert!(b < 100);
            assert_eq!(b, bucket("p", &prod, &key));
        }
        // The empty session id is a valid key.
        assert!(bucket("p", &prod, "") < 100);
    }

    #[test]
    fn ten_percent_of_ten_thousand_keys_is_near_one_thousand() {
        let prod = Label::production();
        let hits = (0..10_000)
            .filter(|i| bucket("p", &prod, &format!("session-{i}")) < 10)
            .count();
        assert!((900..=1100).contains(&hits), "{hits}");
    }

    #[test]
    fn raising_percent_keeps_every_rollout_key() {
        let prod = Label::production();
        for i in 0..10_000 {
            let b = bucket("p", &prod, &format!("session-{i}"));
            if b < 10 {
                assert!(b < 50, "session-{i}");
            }
        }
    }
}
