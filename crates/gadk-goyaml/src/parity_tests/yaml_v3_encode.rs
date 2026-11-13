// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

//! Port of yaml.v3 v3.0.1 `encode_test.go` (Task 11).
//!
//! Go values become serde values: Go `int`/`uint` are `i64`/`u64`, `map[string]T` is a
//! `BTreeMap` (Go sorts map keys on encode), `interface{}` is [`GoValue`], `*T` is `Option<T>`,
//! `yaml.Node` is [`Document`], and a struct is a serde struct whose keys follow [`Options`]
//! (Go's default key = the lowercased field name; a `yaml:"key"` tag is a `field_keys` entry;
//! `,omitempty` is `skip_serializing_if`; `-` and unexported fields are `#[serde(skip)]`). An
//! inlined struct is written out field by field (spec R8) and an inline `map[string]T` is an
//! [`InlineMap`]. Every `marshalTests` row's output is checked against the probe's copy
//! (`testdata/corpus/parse/encode-marshalTests-<NNN>.yaml`).

use std::collections::BTreeMap;

use serde::Serialize;

use super::{RowCount, corpus, gmap, int, s};
use crate::{
    Document, Encoder, Error, GoValue, InlineMap, Kind, NodeData, Options, Style, marshal,
};

// parity: skipped TestMarshaler: the Marshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestMarshalerWholeDocument: the Marshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestMarshalerError: the Marshaler interface is a Non-Goal (spec Scope)

/// Go's `map[string]T`.
fn m<V, const N: usize>(entries: [(&'static str, V); N]) -> BTreeMap<&'static str, V> {
    entries.into_iter().collect()
}

/// A `yaml.Node` holding one node of `kind` with `style`, `tag` and `value`.
fn node(kind: Kind, style: Style, tag: &str, value: &str) -> Document {
    Document::new(NodeData {
        kind,
        style,
        tag: tag.to_string(),
        value: value.to_string(),
        ..NodeData::default()
    })
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

/// Go `struct{}`.
#[derive(Serialize)]
struct Empty {}

/// Go `struct{ Hello string }`.
#[derive(Serialize)]
struct MHello {
    hello: &'static str,
}

/// Go `struct{ B string }`.
#[derive(Serialize)]
struct MB {
    b: &'static str,
}

/// Go `struct{ A struct{ B string } }`.
#[derive(Serialize)]
struct MA {
    a: MB,
}

/// Go `struct{ A *struct{ B string } }`.
#[derive(Serialize)]
struct MAPtr {
    a: Option<MB>,
}

/// Go `struct{ A int }`.
#[derive(Serialize)]
struct MAInt {
    a: i64,
}

/// Go `struct{ A []int }`.
#[derive(Serialize)]
struct MAInts {
    a: Vec<i64>,
}

/// Go `struct{ A [2]int }`.
#[derive(Serialize)]
struct MAArray {
    a: [i64; 2],
}

/// Go `struct{ B int "a" }`: the tag is a `field_keys` entry ([`tag_a`]).
#[derive(Serialize)]
struct MTagA {
    b: i64,
}

/// The [`Options`] of [`MTagA`] (Go's tag `"a"` on field `B`).
fn tag_a() -> Options {
    let mut opts = Options::default();
    opts.field_keys.insert(("MTagA", "b"), "a");
    opts
}

/// Go `struct{ A bool }`.
#[derive(Serialize)]
struct MABool {
    a: bool,
}

/// Go `struct{ A string }`.
#[derive(Serialize)]
struct MAStr {
    a: &'static str,
}

/// Go `struct{ A int "a,omitempty"; B int "b,omitempty" }`.
#[derive(Serialize)]
struct MOmitInts {
    #[serde(skip_serializing_if = "is_zero_i64")]
    a: i64,
    #[serde(skip_serializing_if = "is_zero_i64")]
    b: i64,
}

/// Go `struct{ X, y int }` (`y` is unexported: never encoded, ignored by `omitempty`).
#[derive(Serialize)]
struct MXy {
    x: i64,
    #[allow(dead_code)] // Go's unexported field: set, never encoded.
    #[serde(skip)]
    y: i64,
}

/// Go's `isZero` for [`MXy`]: unexported fields are ignored.
fn xy_is_zero(v: &MXy) -> bool {
    v.x == 0
}

/// Go `struct{ A *struct{ X, y int } "a,omitempty,flow" }` without `,flow` (only used where
/// the field is omitted, so the flow style never applies).
#[derive(Serialize)]
struct MOmitXyPtr {
    #[serde(skip_serializing_if = "Option::is_none")]
    a: Option<MXy>,
}

/// Go `struct{ A struct{ X, y int } "a,omitempty,flow" }` without `,flow` (only used where
/// the field is omitted).
#[derive(Serialize)]
struct MOmitXy {
    #[serde(skip_serializing_if = "xy_is_zero")]
    a: MXy,
}

/// Go `struct{ A float64 "a,omitempty"; B float64 "b,omitempty" }`.
#[derive(Serialize)]
struct MOmitFloats {
    #[serde(skip_serializing_if = "is_zero_f64")]
    a: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    b: f64,
}

/// Go `struct{ u int; A int }`.
#[derive(Serialize)]
struct MUnexported {
    #[allow(dead_code)] // Go's unexported field: set, never encoded.
    #[serde(skip)]
    u: i64,
    a: i64,
}

/// Go `struct{ A int; B int "-" }`.
#[derive(Serialize)]
struct MIgnoredB {
    a: i64,
    #[allow(dead_code)] // Go's `yaml:"-"` field: set, never encoded.
    #[serde(skip)]
    b: i64,
}

/// Go `struct{ A int; C inlineB ",inline" }` (and `*inlineB`) with `inlineB`'s fields (`B`,
/// then the inlined `inlineC`'s `C`) listed in the outer struct (R8).
#[derive(Serialize)]
struct MAbc {
    a: i64,
    b: i64,
    c: i64,
}

/// Go `struct{ A int; C *inlineB ",inline" }` holding a nil pointer: the pointer's fields are
/// listed (R8) as omitted `Option`s, as Go writes nothing for a nil inline pointer.
#[derive(Serialize)]
struct MAbcNil {
    a: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    b: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    c: Option<i64>,
}

/// Go `struct{ A int; D *inlineD ",inline" }` with `inlineD`'s fields (the inlined
/// `*inlineC`'s `C`, then `D`) listed (R8).
#[derive(Serialize)]
struct MAcd {
    a: i64,
    c: i64,
    d: i64,
}

/// Go `struct{ A int; C map[string]int ",inline" }`.
#[derive(Serialize)]
struct MInlineMap {
    a: i64,
    #[serde(rename = "$gadk_goyaml::inline")]
    c: InlineMap,
}

/// Go `struct{ Value yaml.Node }`.
#[derive(Serialize)]
struct MValue {
    value: Document,
}

/// A consumer of the `marshalTests` table, row by row in Go's order.
trait MarshalRows {
    /// Row `i`: `value` must encode to `data`.
    fn row<T: Serialize + ?Sized>(&mut self, i: usize, value: &T, data: &str, opts: &Options);
    /// Row `i` is skipped (the reason is the comment above the call).
    fn skip(&mut self, i: usize);
}

/// Go's `marshalTests` (88 rows), fed to `r` in order.
fn marshal_tests(r: &mut impl MarshalRows) {
    let d = &Options::default();
    r.row(0, &GoValue::Null, "null\n", d);
    // Go's nil `*marshalerType`: a nil pointer is encoded as null before any Marshaler check.
    r.row(1, &None::<Empty>, "null\n", d);
    r.row(2, &Empty {}, "{}\n", d);
    r.row(3, &m([("v", "hi")]), "v: hi\n", d);
    r.row(4, &m([("v", s("hi"))]), "v: hi\n", d);
    r.row(5, &m([("v", "true")]), "v: \"true\"\n", d);
    r.row(6, &m([("v", "false")]), "v: \"false\"\n", d);
    r.row(7, &m([("v", GoValue::Bool(true))]), "v: true\n", d);
    r.row(8, &m([("v", GoValue::Bool(false))]), "v: false\n", d);
    r.row(9, &m([("v", int(10))]), "v: 10\n", d);
    r.row(10, &m([("v", int(-10))]), "v: -10\n", d);
    r.row(11, &m([("v", 42_u64)]), "v: 42\n", d);
    r.row(12, &m([("v", int(4294967296))]), "v: 4294967296\n", d);
    r.row(13, &m([("v", 4294967296_i64)]), "v: 4294967296\n", d);
    r.row(14, &m([("v", 4294967296_u64)]), "v: 4294967296\n", d);
    r.row(15, &m([("v", s("10"))]), "v: \"10\"\n", d);
    r.row(16, &m([("v", GoValue::Float(0.1))]), "v: 0.1\n", d);
    r.row(17, &m([("v", GoValue::Float(0.1))]), "v: 0.1\n", d);
    // Adapted: `GoValue` has no float32, so Go's `float32(0.99)` inside an `interface{}` is a
    // map of `f32` (encoded with float32 precision either way).
    r.row(18, &m([("v", 0.99_f32)]), "v: 0.99\n", d);
    r.row(19, &m([("v", GoValue::Float(-0.1))]), "v: -0.1\n", d);
    r.row(
        20,
        &m([("v", GoValue::Float(f64::INFINITY))]),
        "v: .inf\n",
        d,
    );
    r.row(
        21,
        &m([("v", GoValue::Float(f64::NEG_INFINITY))]),
        "v: -.inf\n",
        d,
    );
    r.row(22, &m([("v", GoValue::Float(f64::NAN))]), "v: .nan\n", d);
    r.row(23, &m([("v", GoValue::Null)]), "v: null\n", d);
    r.row(24, &m([("v", s(""))]), "v: \"\"\n", d);
    r.row(25, &m([("v", vec!["A", "B"])]), "v:\n    - A\n    - B\n", d);
    r.row(
        26,
        &m([("v", vec!["A", "B\nC"])]),
        "v:\n    - A\n    - |-\n      B\n      C\n",
        d,
    );
    r.row(
        27,
        &m([(
            "v",
            vec![
                s("A"),
                int(1),
                gmap([("B", GoValue::Seq(vec![int(2), int(3)]))]),
            ],
        )]),
        "v:\n    - A\n    - 1\n    - B:\n        - 2\n        - 3\n",
        d,
    );
    r.row(
        28,
        &m([("a", GoValue::MapAny(vec![(s("b"), s("c"))]))]),
        "a:\n    b: c\n",
        d,
    );
    r.row(29, &m([("a", s("-"))]), "a: '-'\n", d);

    // Simple values.
    r.row(30, &123_i64, "123\n", d);

    // Structures
    r.row(31, &MHello { hello: "world" }, "hello: world\n", d);
    r.row(32, &MA { a: MB { b: "c" } }, "a:\n    b: c\n", d);
    r.row(
        33,
        &MAPtr {
            a: Some(MB { b: "c" }),
        },
        "a:\n    b: c\n",
        d,
    );
    r.row(34, &MAPtr { a: None }, "a: null\n", d);
    r.row(35, &MAInt { a: 1 }, "a: 1\n", d);
    r.row(36, &MAInts { a: vec![1, 2] }, "a:\n    - 1\n    - 2\n", d);
    r.row(37, &MAArray { a: [1, 2] }, "a:\n    - 1\n    - 2\n", d);
    r.row(38, &MTagA { b: 1 }, "a: 1\n", &tag_a());
    r.row(39, &MABool { a: true }, "a: true\n", d);
    r.row(40, &MAStr { a: "true" }, "a: \"true\"\n", d);
    r.row(41, &MAStr { a: "off" }, "a: \"off\"\n", d);

    // Conditional flag
    r.row(42, &MOmitInts { a: 1, b: 0 }, "a: 1\n", d);
    r.row(43, &MOmitInts { a: 0, b: 0 }, "{}\n", d);
    // Row 44 skipped: the field is written with the `,flow` tag option (Non-Goal).
    r.skip(44);
    // Adapted: `,flow` is a Non-Goal; the nil field is omitted, so its style never applies.
    r.row(45, &MOmitXyPtr { a: None }, "{}\n", d);
    // Rows 46-47 skipped: the field is written with the `,flow` tag option (Non-Goal).
    r.skip(46);
    r.skip(47);
    // Adapted: `,flow` is a Non-Goal; the zero field (`y` is unexported) is omitted, so its
    // style never applies.
    r.row(
        48,
        &MOmitXy {
            a: MXy { x: 0, y: 1 },
        },
        "{}\n",
        d,
    );
    r.row(49, &MOmitFloats { a: 1.0, b: 0.0 }, "a: 1\n", d);
    // Row 50 skipped: `time.Time` fields need time.Time reflection (Non-Goal).
    r.skip(50);
    // Nil interface that implements Marshaler.
    // Row 51 skipped: a `map[string]yaml.Marshaler` (the Marshaler interface is a Non-Goal).
    r.skip(51);

    // Flow flag
    // Rows 52-55 skipped: the `,flow` tag option is a Non-Goal.
    r.skip(52);
    r.skip(53);
    r.skip(54);
    r.skip(55);

    // Unexported field
    r.row(56, &MUnexported { u: 0, a: 1 }, "a: 1\n", d);

    // Ignored field
    r.row(57, &MIgnoredB { a: 1, b: 2 }, "a: 1\n", d);

    // Struct inlining (R8: inlineB's fields listed in the outer struct).
    r.row(58, &MAbc { a: 1, b: 2, c: 3 }, "a: 1\nb: 2\nc: 3\n", d);
    // Struct inlining as a pointer. Adapted (R8): the pointer's fields are listed in the outer
    // struct; Go's nil `*inlineB` (row 60) is the listed fields left out.
    r.row(59, &MAbc { a: 1, b: 2, c: 3 }, "a: 1\nb: 2\nc: 3\n", d);
    r.row(
        60,
        &MAbcNil {
            a: 1,
            b: None,
            c: None,
        },
        "a: 1\n",
        d,
    );
    r.row(61, &MAcd { a: 1, c: 3, d: 4 }, "a: 1\nc: 3\nd: 4\n", d);

    // Map inlining. Adapted: the inline map is an `InlineMap` of `GoValue`s (Go's
    // `map[string]int`).
    r.row(
        62,
        &MInlineMap {
            a: 1,
            c: InlineMap(vec![("b".into(), int(2)), ("c".into(), int(3))]),
        },
        "a: 1\nb: 2\nc: 3\n",
        d,
    );

    // Duration
    // Row 63 skipped: `map[string]time.Duration` needs time.Duration reflection (Non-Goal).
    r.skip(63);

    // Issue #24: bug in map merging logic.
    r.row(64, &m([("a", "<foo>")]), "a: <foo>\n", d);

    // Issue #34: marshal unsupported base 60 floats quoted for compatibility
    // with old YAML 1.1 parsers.
    r.row(65, &m([("a", "1:1")]), "a: \"1:1\"\n", d);

    // Binary data.
    r.row(66, &m([("a", "\0")]), "a: \"\\0\"\n", d);
    // Adapted (rows 67, 68): a Go string that is not UTF-8 is `GoValue::Bytes` (a Rust
    // `String` must be UTF-8).
    r.row(
        67,
        &m([("a", GoValue::Bytes(b"\x80\x81\x82".to_vec()))]),
        "a: !!binary gIGC\n",
        d,
    );
    r.row(
        68,
        &m([("a", GoValue::Bytes(vec![0x90; 54]))]),
        &format!("a: !!binary |\n    {}kJ\n    CQ\n", "kJCQ".repeat(17)),
        d,
    );

    // Encode unicode as utf-8 rather than in escaped form.
    r.row(
        69,
        &m([("a", "\u{4f60}\u{597d}")]),
        "a: \u{4f60}\u{597d}\n",
        d,
    );

    // Support encoding.TextMarshaler.
    // Row 70 skipped: `net.IP` implements encoding.TextMarshaler (Non-Goal).
    r.skip(70);
    // time.Time gets a timestamp tag.
    // Rows 71-73 skipped: `map[string]time.Time` / `*time.Time` need time.Time reflection
    // (Non-Goal).
    r.skip(71);
    r.skip(72);
    r.skip(73);
    // Ensure timestamp-like strings are quoted.
    r.row(
        74,
        &m([("a", "2015-02-24T18:19:39Z")]),
        "a: \"2015-02-24T18:19:39Z\"\n",
        d,
    );

    // Ensure strings containing ": " are quoted (reported as PR #43, but not reproducible).
    r.row(75, &m([("a", "b: c")]), "a: 'b: c'\n", d);

    // Containing hash mark ('#') in string should be quoted
    r.row(
        76,
        &m([("a", "Hello #comment")]),
        "a: 'Hello #comment'\n",
        d,
    );
    r.row(
        77,
        &m([("a", "\u{4f60}\u{597d} #comment")]),
        "a: '\u{4f60}\u{597d} #comment'\n",
        d,
    );

    // Ensure MarshalYAML also gets called on the result of MarshalYAML itself.
    // Rows 78-79 skipped: `marshalerType` implements Marshaler (Non-Goal).
    r.skip(78);
    r.skip(79);

    // Check indentation of maps inside sequences inside maps.
    r.row(
        80,
        &m([(
            "a",
            gmap([(
                "b",
                GoValue::Seq(vec![gmap([("c", int(1)), ("d", int(2))])]),
            )]),
        )]),
        "a:\n    b:\n        - c: 1\n          d: 2\n",
        d,
    );

    // Strings with tabs were disallowed as literals (issue #471).
    r.row(81, &m([("a", "\tB\n\tC\n")]), "a: |\n    \tB\n    \tC\n", d);

    // Ensure that strings do not wrap
    r.row(
        82,
        &m([(
            "a",
            "abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 1234567890 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 1234567890 ",
        )]),
        "a: 'abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 1234567890 abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 1234567890 '\n",
        d,
    );

    // yaml.Node
    r.row(
        83,
        &MValue {
            value: node(Kind::Scalar, Style::SINGLE_QUOTED, "!!str", "foo"),
        },
        "value: 'foo'\n",
        d,
    );
    r.row(
        84,
        &node(Kind::Scalar, Style::SINGLE_QUOTED, "!!str", "foo"),
        "'foo'\n",
        d,
    );

    // Enforced tagging with shorthand notation (issue #616).
    r.row(
        85,
        &MValue {
            value: node(Kind::Scalar, Style::TAGGED, "!!str", "foo"),
        },
        "value: !!str foo\n",
        d,
    );
    r.row(
        86,
        &MValue {
            value: node(Kind::Mapping, Style::TAGGED, "!!map", ""),
        },
        "value: !!map {}\n",
        d,
    );
    r.row(
        87,
        &MValue {
            value: node(Kind::Sequence, Style::TAGGED, "!!seq", ""),
        },
        "value: !!seq []\n",
        d,
    );
}

/// The number of rows in Go's `marshalTests`.
const MARSHAL_TESTS: usize = 88;

/// The `marshalTests` rows skipped for a Non-Goal.
const MARSHAL_TESTS_SKIPPED: [usize; 16] = [
    44, 46, 47, 50, 51, 52, 53, 54, 55, 63, 70, 71, 72, 73, 78, 79,
];

/// Checks a ported `marshalTests` row's expected output against the probe's copy.
fn check_marshal_corpus(i: usize, data: &str) {
    assert_eq!(
        data.as_bytes(),
        corpus(&format!("encode-marshalTests-{i:03}")),
        "row {i}: output differs from the probe corpus"
    );
}

/// `TestMarshal`: `yaml.Marshal` of every row.
#[derive(Default)]
struct MarshalCheck(RowCount);

impl MarshalRows for MarshalCheck {
    fn row<T: Serialize + ?Sized>(&mut self, i: usize, value: &T, data: &str, opts: &Options) {
        self.0.ported(i);
        check_marshal_corpus(i, data);
        let got = marshal(value, opts).unwrap_or_else(|e| panic!("row {i}: {e}"));
        assert_eq!(String::from_utf8_lossy(&got), data, "row {i}");
    }

    fn skip(&mut self, i: usize) {
        self.0.skipped(i);
    }
}

// parity: adapted: Go values are serde values (inlined structs listed field by field, R8; a non-UTF-8 string is GoValue::Bytes; float32 in interface{} is a map of f32); 16 rows skipped for Non-Goals (,flow, Marshaler, TextMarshaler, time.Time/Duration), rows 18/45/48/59-60/62/67/68 adapted (see their comments)
#[test]
fn test_marshal() {
    let mut check = MarshalCheck::default();
    marshal_tests(&mut check);
    check.0.finish(MARSHAL_TESTS, &MARSHAL_TESTS_SKIPPED);
}

/// `TestEncoderSingleDocument`: `Encoder.Encode` + `Close` of every row.
#[derive(Default)]
struct EncoderCheck(RowCount);

impl MarshalRows for EncoderCheck {
    fn row<T: Serialize + ?Sized>(&mut self, i: usize, value: &T, data: &str, opts: &Options) {
        self.0.ported(i);
        check_marshal_corpus(i, data);
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.encode(value, opts)
            .unwrap_or_else(|e| panic!("row {i}: {e}"));
        enc.close().unwrap_or_else(|e| panic!("row {i}: {e}"));
        assert_eq!(String::from_utf8_lossy(&buf), data, "row {i}");
    }

    fn skip(&mut self, i: usize) {
        self.0.skipped(i);
    }
}

// parity: adapted: as test_marshal (same rows, skips and adaptations), through Encoder::encode + close
#[test]
fn test_encoder_single_document() {
    let mut check = EncoderCheck::default();
    marshal_tests(&mut check);
    check.0.finish(MARSHAL_TESTS, &MARSHAL_TESTS_SKIPPED);
}

#[test]
fn test_encoder_multiple_documents() {
    let opts = &Options::default();
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.encode(&m([("a", "b")]), opts).unwrap();
    enc.encode(&m([("c", "d")]), opts).unwrap();
    enc.close().unwrap();
    assert_eq!(String::from_utf8_lossy(&buf), "a: b\n---\nc: d\n");
}

/// Go's `errorWriter`.
struct ErrorWriter;

impl std::io::Write for ErrorWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("some write error"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_encoder_write_error() {
    let mut enc = Encoder::new(ErrorWriter);
    let err = enc
        .encode(&m([("a", "b")]), &Options::default())
        .unwrap_err();
    // Data not flushed yet
    assert_eq!(err.to_string(), "yaml: write error: some write error");
}

/// Go `struct{ B int; inlineB ",inline" }` with `inlineB`'s fields (`B`, `C`) listed (R8):
/// the inlined `B` is the serde field `inline_b`, keyed `b` by [`dup_options`].
#[derive(Serialize)]
struct MDuplicated {
    b: i64,
    inline_b: i64,
    c: i64,
}

/// The [`Options`] of [`MDuplicated`]: the inlined field's Go key is `b`.
fn dup_options() -> Options {
    let mut opts = Options::default();
    opts.field_keys.insert(("MDuplicated", "inline_b"), "b");
    opts
}

/// Go `struct{ A int; B map[string]int ",inline" }`.
#[derive(Serialize)]
struct MConflict {
    a: i64,
    #[serde(rename = "$gadk_goyaml::inline")]
    b: InlineMap,
}

// parity: adapted: Go panics (handleErr re-panics) where marshal returns Err(Error::Unsupported) with Go's text (ledgered); the duplicated-key text names the serde struct (MDuplicated) where Go prints `struct { B int; … }`
#[test]
fn test_marshal_errors() {
    let err = marshal(
        &MDuplicated {
            b: 1,
            inline_b: 2,
            c: 3,
        },
        &dup_options(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
    assert_eq!(err.to_string(), "duplicated key 'b' in struct MDuplicated");

    let err = marshal(
        &MConflict {
            a: 1,
            b: InlineMap(vec![("a".into(), int(2))]),
        },
        &Options::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
    assert_eq!(
        err.to_string(),
        "cannot have key \"a\" in inlined map: conflicts with struct field"
    );
}

#[test]
fn test_marshal_type_cache() {
    let opts = &Options::default();
    {
        #[derive(Serialize, Default)]
        struct T {
            a: i64,
        }
        marshal(&T::default(), opts).unwrap();
    }
    let data = {
        #[derive(Serialize, Default)]
        struct T {
            b: i64,
        }
        marshal(&T::default(), opts).unwrap()
    };
    assert_eq!(String::from_utf8_lossy(&data), "b: 0\n");
}

#[test]
fn test_set_indent() {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.set_indent(8);
    enc.encode(
        &m([("a", m([("b", m([("c", "d")]))]))]),
        &Options::default(),
    )
    .unwrap();
    enc.close().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&buf),
        "a:\n        b:\n                c: d\n"
    );
}

#[test]
fn test_sorted_output() {
    // (key, Go's `fmt.Sprint(k)`, quoted when it is "" or parses as a float32).
    let order: Vec<(GoValue, &str)> = vec![
        (GoValue::Bool(false), "false"),
        (GoValue::Bool(true), "true"),
        (int(1), "1"),
        (GoValue::Uint(1), "1"),
        (GoValue::Float(1.0), "1"),
        (GoValue::Float(1.1), "1.1"),
        (GoValue::Float(1.2), "1.2"),
        (int(2), "2"),
        (GoValue::Uint(2), "2"),
        (GoValue::Float(2.0), "2"),
        (GoValue::Float(2.1), "2.1"),
        (s(""), "\"\""),
        (s(".1"), "\".1\""),
        (s(".2"), "\".2\""),
        (s(".a"), ".a"),
        (s("1"), "\"1\""),
        (s("2"), "\"2\""),
        (s("a!10"), "a!10"),
        (s("a/0001"), "a/0001"),
        (s("a/002"), "a/002"),
        (s("a/3"), "a/3"),
        (s("a/10"), "a/10"),
        (s("a/11"), "a/11"),
        (s("a/0012"), "a/0012"),
        (s("a/100"), "a/100"),
        (s("a~10"), "a~10"),
        (s("ab/1"), "ab/1"),
        (s("b/1"), "b/1"),
        (s("b/01"), "b/01"),
        (s("b/2"), "b/2"),
        (s("b/02"), "b/02"),
        (s("b/3"), "b/3"),
        (s("b/03"), "b/03"),
        (s("b1"), "b1"),
        (s("b01"), "b01"),
        (s("b3"), "b3"),
        (s("c2.10"), "c2.10"),
        (s("c10.2"), "c10.2"),
        (s("d1"), "d1"),
        (s("d7"), "d7"),
        (s("d7abc"), "d7abc"),
        (s("d12"), "d12"),
        (s("d12a"), "d12a"),
        (s("e2b"), "e2b"),
        (s("e4b"), "e4b"),
        (s("e21a"), "e21a"),
    ];
    // Go's map iteration order is random; feed the keys in reverse.
    let m = GoValue::MapAny(
        order
            .iter()
            .rev()
            .map(|(k, _)| (k.clone(), int(1)))
            .collect(),
    );
    let data = marshal(&m, &Options::default()).unwrap();
    let out = format!("\n{}", String::from_utf8_lossy(&data));
    let mut last = 0;
    for (i, (k, repr)) in order.iter().enumerate() {
        let Some(index) = out.find(&format!("\n{repr}:")) else {
            panic!("{k:?} is not in the output: {out:?}");
        };
        assert!(
            index >= last,
            "{k:?} was generated before {:?}: {out:?}",
            order[i - 1].0
        );
        last = index;
    }
}
