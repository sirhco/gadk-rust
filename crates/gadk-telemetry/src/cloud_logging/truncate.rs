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

//! Shrinks a Cloud Logging payload until its entry fits the entry size limit.

use std::collections::BinaryHeap;

use serde_json::{Map, Value, json};

/// Appended once to every shortened string.
pub(crate) const SUFFIX: &str = "…[truncated]";
/// Strings of at most this many bytes are never shortened.
pub(crate) const MIN_SHRINK: usize = 64;
/// Payload marker: the entry was shrunk.
pub(crate) const MARKER_TRUNCATED: &str = "gadk.truncated";
/// Payload marker: the entry's serialized size before shrinking.
pub(crate) const MARKER_ORIGINAL: &str = "gadk.original_bytes";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Seg {
    Key(String),
    Index(usize),
}

fn map_len(m: &Map<String, Value>) -> usize {
    serde_json::to_vec(m).map_or(0, |b| b.len())
}

fn value_len(v: &Value) -> usize {
    serde_json::to_vec(v).map_or(0, |b| b.len())
}

fn str_len(s: &str) -> usize {
    serde_json::to_vec(s).map_or(0, |b| b.len())
}

fn collect(v: &Value, path: &mut Vec<Seg>, out: &mut BinaryHeap<(usize, Vec<Seg>)>) {
    match v {
        Value::String(s) if s.len() > MIN_SHRINK => out.push((s.len(), path.clone())),
        Value::Array(a) => {
            for (i, item) in a.iter().enumerate() {
                path.push(Seg::Index(i));
                collect(item, path, out);
                path.pop();
            }
        }
        Value::Object(m) => {
            for (k, item) in m {
                path.push(Seg::Key(k.clone()));
                collect(item, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

fn leaf_mut<'a>(payload: &'a mut Map<String, Value>, path: &[Seg]) -> Option<&'a mut String> {
    let (Seg::Key(first), rest) = path.split_first()? else {
        return None;
    };
    let mut cur = payload.get_mut(first)?;
    for seg in rest {
        cur = match (seg, cur) {
            (Seg::Key(k), Value::Object(m)) => m.get_mut(k)?,
            (Seg::Index(i), Value::Array(a)) => a.get_mut(*i)?,
            _ => return None,
        };
    }
    match cur {
        Value::String(s) => Some(s),
        _ => None,
    }
}

/// Halves `s` (after removing an earlier suffix) on a char boundary and appends [`SUFFIX`].
fn cut(s: &mut String) {
    if s.ends_with(SUFFIX) {
        s.truncate(s.len() - SUFFIX.len());
    }
    let mut keep = s.len() / 2;
    while !s.is_char_boundary(keep) {
        keep -= 1;
    }
    s.truncate(keep);
    s.push_str(SUFFIX);
}

/// Shrinks `payload` so that `overhead` plus the payload (with markers) serializes to at most
/// `limit` bytes. Returns the original total size when it shrank; `None` when it already fit.
pub(crate) fn fit(
    payload: &mut Map<String, Value>,
    overhead: usize,
    limit: usize,
) -> Option<usize> {
    let original = overhead + map_len(payload);
    if original <= limit {
        return None;
    }
    let markers = value_len(&json!({MARKER_TRUNCATED: true, MARKER_ORIGINAL: original}));
    let target = limit.saturating_sub(overhead + markers);
    let mut heap = BinaryHeap::new();
    for (k, v) in payload.iter() {
        collect(v, &mut vec![Seg::Key(k.clone())], &mut heap);
    }
    let mut size = map_len(payload);
    while size > target {
        let Some((_, path)) = heap.pop() else {
            payload.clear();
            break;
        };
        let Some(s) = leaf_mut(payload, &path) else {
            continue;
        };
        let before = str_len(s);
        cut(s);
        let after = str_len(s);
        if s.len() > MIN_SHRINK {
            heap.push((s.len(), path));
        }
        size = size + after - before;
        if size <= target {
            size = map_len(payload);
        }
    }
    payload.insert(MARKER_TRUNCATED.to_owned(), Value::Bool(true));
    payload.insert(MARKER_ORIGINAL.to_owned(), json!(original));
    Some(original)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::{Map, Value, json};

    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }

    fn size(m: &Map<String, Value>) -> usize {
        serde_json::to_vec(m).unwrap().len()
    }

    #[test]
    fn under_the_limit_is_untouched() {
        let mut p = obj(json!({"body": "short"}));
        let before = p.clone();
        assert_eq!(fit(&mut p, 10, 1_000), None);
        assert_eq!(p, before);
    }

    #[test]
    fn one_huge_string_shrinks_with_markers() {
        let mut p = obj(json!({"attributes": {"big": "y".repeat(500_000)}, "body": "keep"}));
        let original = size(&p) + 100;
        assert_eq!(fit(&mut p, 100, 256_000), Some(original));
        assert!(size(&p) + 100 <= 256_000);
        assert_eq!(p["body"], json!("keep"));
        assert!(p["attributes"]["big"].as_str().unwrap().ends_with(SUFFIX));
        assert_eq!(p[MARKER_TRUNCATED], json!(true));
        assert_eq!(p[MARKER_ORIGINAL], json!(original));
    }

    #[test]
    fn many_medium_strings_fit_and_suffixes_never_stack() {
        let list: Vec<Value> = (0..5_000)
            .map(|i| json!(format!("{i}-{}", "z".repeat(200))))
            .collect();
        let mut p = obj(json!({"attributes": {"l": list}}));
        assert!(fit(&mut p, 0, 256_000).is_some());
        assert!(size(&p) <= 256_000);
        for v in p["attributes"]["l"].as_array().unwrap() {
            let s = v.as_str().unwrap();
            assert!(s.matches(SUFFIX).count() <= 1, "{s}");
        }
    }

    #[test]
    fn only_short_strings_fall_back_to_markers() {
        let list: Vec<Value> = (0..100_000).map(|_| json!("short")).collect();
        let mut p = obj(json!({"attributes": {"l": list}}));
        let original = size(&p);
        assert_eq!(fit(&mut p, 0, 256_000), Some(original));
        assert_eq!(
            Value::Object(p),
            json!({MARKER_TRUNCATED: true, MARKER_ORIGINAL: original})
        );
    }

    #[test]
    fn multibyte_text_stays_valid() {
        let mut p = obj(json!({"body": "é😀".repeat(100_000)}));
        assert!(fit(&mut p, 0, 256_000).is_some());
        assert!(size(&p) <= 256_000);
        // Values are `String`s, so they are valid UTF-8 by construction; check the cut point.
        assert!(p["body"].as_str().unwrap().ends_with(SUFFIX));
    }
}
