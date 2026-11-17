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

//! CRC-32C (Castagnoli) — the checksum the GCS offloader sends with every upload — and its wire
//! form: the standard base64 of the four big-endian bytes (the `crc32c` field of the object
//! metadata, `X-Goog-Hash: crc32c=…`). Table driven, one byte at a time.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// The reversed Castagnoli polynomial.
const CASTAGNOLI: u32 = 0x82f6_3b78;

/// The byte-at-a-time lookup table for [`CASTAGNOLI`].
const TABLE: [u32; 256] = make_table(CASTAGNOLI);

const fn make_table(poly: u32) -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        #[allow(clippy::cast_possible_truncation)]
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ poly
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// The checksum of the bytes so far (`crc`) and `data`.
pub(crate) fn update(crc: u32, data: &[u8]) -> u32 {
    let mut crc = !crc;
    for &b in data {
        crc = TABLE[usize::from((crc as u8) ^ b)] ^ (crc >> 8);
    }
    !crc
}

/// The checksum of `data`.
pub(crate) fn checksum(data: &[u8]) -> u32 {
    update(0, data)
}

/// The wire form of a CRC-32C: standard base64 of its big-endian bytes.
pub(crate) fn base64_be(crc: u32) -> String {
    STANDARD.encode(crc.to_be_bytes())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use pretty_assertions::assert_eq;
    use serde_json::Value;

    use super::*;

    fn data(case: &Value) -> Vec<u8> {
        let len = usize::try_from(case["len"].as_u64().unwrap()).unwrap();
        match case["data_pattern"].as_str() {
            Some("i % 251") => (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect(),
            Some(other) => panic!("unknown pattern {other}"),
            None => STANDARD
                .decode(case["data_base64"].as_str().unwrap())
                .unwrap(),
        }
    }

    #[test]
    fn checksums_match_go() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/helpers/crc32c.json");
        let go: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let cases = go["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 11);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let data = data(case);
            assert_eq!(
                data.len(),
                usize::try_from(case["len"].as_u64().unwrap()).unwrap()
            );
            let crc = checksum(&data);
            assert_eq!(u64::from(crc), case["crc32c"].as_u64().unwrap(), "{name}");
            assert_eq!(
                format!("{crc:08x}"),
                case["hex"].as_str().unwrap(),
                "{name}"
            );
            assert_eq!(
                base64_be(crc),
                case["base64_be"].as_str().unwrap(),
                "{name}"
            );
        }
    }

    #[test]
    fn update_continues_a_checksum() {
        let data = b"123456789";
        let (a, b) = data.split_at(4);
        assert_eq!(update(update(0, a), b), checksum(data));
        assert_eq!(checksum(data), 0xe306_9283);
        assert_eq!(update(checksum(data), &[]), checksum(data));
    }
}
