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

//! The JSON Path subset used to address streamed function-call arguments
//! (port of the path helpers in ADK-Go `internal/llminternal/stream_aggregator.go`).

use serde_json::{Map, Value};

/// Port of stream_aggregator.go pathSegment.
///
/// One step of a JSON Path: an object member name, or an array index when `is_index` is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathSegment {
    pub(crate) name: String,
    pub(crate) index: usize,
    pub(crate) is_index: bool,
}

/// Port of stream_aggregator.go maxPathIndex.
///
/// The largest array index a path may address. It bounds the array a single index can grow;
/// no function call argument list comes close to it.
const MAX_PATH_INDEX: usize = 1 << 16;

/// Port of stream_aggregator.go parseJSONPath.
///
/// Splits the RFC 9535 JSON Path that addresses a streamed argument into its segments. Only
/// the selectors a normalized path is built from are accepted — `$.name`, `$['name']` and
/// `$[0]` — because that is all the model produces; anything else, including wildcards,
/// slices and `\u` escapes in a quoted name, returns `None` so that the chunk is dropped
/// rather than written to an invented key.
pub(crate) fn parse_json_path(json_path: &str) -> Option<Vec<PathSegment>> {
    let mut rest = json_path.strip_prefix('$').unwrap_or(json_path);
    let mut segments = Vec::new();
    while let Some(first) = rest.as_bytes().first().copied() {
        if first == b'[' {
            let (segment, remainder) = parse_bracket_segment(rest)?;
            segments.push(segment);
            rest = remainder;
            continue;
        }
        if first == b'.' {
            rest = rest.get(1..)?;
        } else if !segments.is_empty() {
            return None;
        }
        let (name, remainder) = split_member_name(rest);
        if name.is_empty() {
            return None;
        }
        segments.push(PathSegment {
            name: name.to_owned(),
            index: 0,
            is_index: false,
        });
        rest = remainder;
    }
    if segments.is_empty() {
        return None;
    }
    Some(segments)
}

/// Port of stream_aggregator.go splitMemberName.
///
/// Takes the shorthand member name at the front of a path.
fn split_member_name(path: &str) -> (&str, &str) {
    match path.find(['.', '[']) {
        Some(i) => path.split_at(i),
        None => (path, ""),
    }
}

/// Port of stream_aggregator.go parseBracketSegment.
///
/// Takes the bracketed selector at the front of a path, which is either an array index or a
/// quoted member name.
fn parse_bracket_segment(path: &str) -> Option<(PathSegment, &str)> {
    let body = path.get(1..)?;
    if body.starts_with(['\'', '"']) {
        let (name, rest) = parse_quoted_name(body)?;
        let rest = rest.strip_prefix(']')?;
        return Some((
            PathSegment {
                name,
                index: 0,
                is_index: false,
            },
            rest,
        ));
    }
    let end = body.find(']')?;
    if end == 0 {
        return None;
    }
    let mut index: usize = 0;
    for digit in body.get(..end)?.bytes() {
        if !digit.is_ascii_digit() {
            return None;
        }
        index = index
            .checked_mul(10)?
            .checked_add(usize::from(digit - b'0'))?;
        if index > MAX_PATH_INDEX {
            return None;
        }
    }
    let rest = body.get(end.checked_add(1)?..)?;
    Some((
        PathSegment {
            name: String::new(),
            index,
            is_index: true,
        },
        rest,
    ))
}

/// Port of stream_aggregator.go parseQuotedName.
///
/// Reads a quoted member name, undoing the single-character escapes RFC 9535 defines for one.
/// Returns the name and the rest of the path after the closing quote.
fn parse_quoted_name(path: &str) -> Option<(String, &str)> {
    let mut chars = path.char_indices();
    let (_, quote) = chars.next()?;
    let mut parsed = String::new();
    while let Some((i, ch)) = chars.next() {
        if ch == quote {
            let rest = path.get(i.checked_add(ch.len_utf8())?..)?;
            return Some((parsed, rest));
        }
        if ch == '\\' {
            let (_, escaped) = chars.next()?;
            parsed.push(unescape(escaped)?);
        } else {
            parsed.push(ch);
        }
    }
    None
}

/// Port of stream_aggregator.go unescape.
fn unescape(ch: char) -> Option<char> {
    match ch {
        '\'' | '"' | '\\' | '/' => Some(ch),
        'b' => Some('\u{0008}'),
        'f' => Some('\u{000C}'),
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        _ => None,
    }
}

/// Port of stream_aggregator.go setValueByPath.
///
/// Writes `value` at the location `segments` address within `args`, creating the objects and
/// arrays on the way to it. Function call arguments are a JSON object, so a path that starts
/// at an array index addresses nothing and leaves `args` unchanged.
pub(crate) fn set_value_by_path(
    args: &mut Map<String, Value>,
    segments: &[PathSegment],
    value: Value,
) {
    if segments.first().is_none_or(|s| s.is_index) {
        return;
    }
    let container = Value::Object(std::mem::take(args));
    if let Value::Object(updated) = set_into(container, segments, value) {
        *args = updated;
    }
}

/// Port of stream_aggregator.go setInto.
///
/// Returns `container` with `value` written at the location `segments` address within it,
/// which is a new container when the one that is there cannot hold that segment. An array
/// grows to fit an index, so an argument whose elements arrive out of order keeps the
/// positions the model chose.
fn set_into(container: Value, segments: &[PathSegment], value: Value) -> Value {
    let Some(segment) = segments.first() else {
        return container;
    };
    if segment.is_index {
        let mut elements = match container {
            Value::Array(elements) => elements,
            _ => Vec::new(),
        };
        if elements.len() <= segment.index {
            elements.resize(segment.index.saturating_add(1), Value::Null);
        }
        if let Some(slot) = elements.get_mut(segment.index) {
            let existing = std::mem::take(slot);
            *slot = descend(existing, segments, value);
        }
        return Value::Array(elements);
    }
    let mut members = match container {
        Value::Object(members) => members,
        _ => Map::new(),
    };
    let slot = members.entry(segment.name.clone()).or_insert(Value::Null);
    let existing = std::mem::take(slot);
    *slot = descend(existing, segments, value);
    Value::Object(members)
}

/// Port of stream_aggregator.go descend.
fn descend(existing: Value, segments: &[PathSegment], value: Value) -> Value {
    match segments.get(1..) {
        Some(rest) if !rest.is_empty() => set_into(existing, rest, value),
        _ => value,
    }
}

/// Port of stream_aggregator.go valueByPath.
///
/// Reads the value `segments` address, or `None` when the whole path does not exist.
pub(crate) fn value_by_path<'a>(
    args: &'a Map<String, Value>,
    segments: &[PathSegment],
) -> Option<&'a Value> {
    let mut iter = segments.iter();
    let first = iter.next()?;
    if first.is_index {
        return None;
    }
    let mut current = args.get(&first.name)?;
    for segment in iter {
        current = if segment.is_index {
            current.as_array()?.get(segment.index)?
        } else {
            current.as_object()?.get(&segment.name)?
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str) -> PathSegment {
        PathSegment {
            name: name.into(),
            index: 0,
            is_index: false,
        }
    }

    fn index(index: usize) -> PathSegment {
        PathSegment {
            name: String::new(),
            index,
            is_index: true,
        }
    }

    #[test]
    fn parses_supported_selectors() {
        assert_eq!(
            parse_json_path(r#"$.a['b.c'][3]["d\"e"]"#),
            Some(vec![member("a"), member("b.c"), index(3), member("d\"e")])
        );
        assert_eq!(parse_json_path("$[65536]"), Some(vec![index(65536)]));
        assert_eq!(
            parse_json_path("$.é.ü"),
            Some(vec![member("é"), member("ü")])
        );
    }

    #[test]
    fn rejects_unsupported_selectors() {
        for path in [
            "",
            "$",
            "$.",
            "$..a",
            "$.a[]",
            "$.a[*]",
            "$.a[0:2]",
            "$.a[-1]",
            "$[65537]",
            "$['a'",
            "$['a\\u0041']",
            "$['a']x",
            "$.a[1",
        ] {
            assert_eq!(parse_json_path(path), None, "{path}");
        }
    }

    #[test]
    fn root_index_is_a_no_op() {
        let mut args = Map::new();
        args.insert("k".into(), Value::from(1));
        set_value_by_path(&mut args, &[index(0)], Value::from(2));
        assert_eq!(Value::Object(args), serde_json::json!({"k": 1}));
    }

    #[test]
    fn value_by_path_reads_nested() {
        let mut args = Map::new();
        set_value_by_path(&mut args, &[member("a"), index(1), member("b")], "x".into());
        assert_eq!(
            value_by_path(&args, &[member("a"), index(1), member("b")]),
            Some(&Value::from("x"))
        );
        assert_eq!(value_by_path(&args, &[member("a"), index(2)]), None);
        assert_eq!(value_by_path(&args, &[member("a"), member("b")]), None);
    }
}
