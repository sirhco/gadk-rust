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

//! Port of yaml.v3 v3.0.1 `decode_test.go`.
//!
//! Go's reflection targets become small serde types: Go `int`/`uint` are `i64`/`u64`,
//! `map[string]T` is `BTreeMap<String, T>`, `map[interface{}]T` is [`AnyMap`], `interface{}` is
//! [`GoValue`], `*T` is `Option<T>`, and a struct is a `#[serde(default)]` struct whose keys
//! follow [`Options`] (Go's default key = the lowercased field name; a `yaml:"key"` tag is a
//! `field_keys` entry). An inlined struct is written out field by field (spec R8) and an
//! inline `map[string]T` is an [`InlineMap`] (its values are `GoValue`s). Every table row's
//! input is checked against the probe's copy (`testdata/corpus/parse/decode-<table>-<NNN>.yaml`).

use std::collections::BTreeMap;
use std::fmt::{self, Debug};
use std::marker::PhantomData;

use chrono::{DateTime, FixedOffset, Timelike};
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer, MapAccess, Visitor};

use super::{RowCount, corpus, gmap, int, s};
use crate::{
    Decoder, Error, GoValue, InlineMap, Options, TypeError, from_node_partial, unmarshal,
    unmarshal_node,
};

// parity: skipped TestUnmarshalDurationInt: time.Duration reflection is a Non-Goal (spec Scope); gadk types choose their own serde form for durations
// parity: skipped TestUnmarshalerPointerField: the Unmarshaler and obsoleteUnmarshaler interfaces are Non-Goals (spec Scope)
// parity: skipped TestUnmarshalerValueField: the obsoleteUnmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestUnmarshalerInlinedField: the Unmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestUnmarshalerWholeDocument: the obsoleteUnmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestUnmarshalerTypeError: the Unmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestObsoleteUnmarshalerTypeError: the obsoleteUnmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestUnmarshalerTypeErrorProxying: the Unmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestObsoleteUnmarshalerTypeErrorProxying: the obsoleteUnmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestUnmarshalerError: the Unmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestObsoleteUnmarshalerError: the obsoleteUnmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestUnmarshalerRetry: the Unmarshaler interface is a Non-Goal (spec Scope)
// parity: skipped TestObsoleteUnmarshalerRetry: the obsoleteUnmarshaler interface is a Non-Goal (spec Scope)

/// Go's `float64` inside an `interface{}`.
fn float(v: f64) -> GoValue {
    GoValue::Float(v)
}

/// Go's `[]interface{}`.
fn seq<const N: usize>(items: [GoValue; N]) -> GoValue {
    GoValue::Seq(items.into())
}

/// Go's `map[string]T`.
fn m<V, const N: usize>(entries: [(&str, V); N]) -> BTreeMap<String, V> {
    entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
}

/// Go's `map[string]string`.
fn ss<const N: usize>(entries: [(&str, &str); N]) -> BTreeMap<String, String> {
    m(entries.map(|(k, v)| (k, v.to_string())))
}

/// A `time.Time` inside an `interface{}`, from RFC 3339 text.
fn ts(v: &str) -> GoValue {
    GoValue::Timestamp(DateTime::parse_from_rfc3339(v).unwrap())
}

/// Sorts every `map[string]interface{}` by key (Go's maps are unordered; `DeepEquals`
/// ignores order).
fn sorted(v: &GoValue) -> GoValue {
    match v {
        GoValue::Map(entries) => {
            let mut entries: Vec<(String, GoValue)> = entries
                .iter()
                .map(|(k, v)| (k.clone(), sorted(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            GoValue::Map(entries)
        }
        GoValue::Seq(items) => GoValue::Seq(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// Go's `map[interface{}]V`: keys decode as `interface{}` values, entries in document order.
#[derive(Debug, Default, PartialEq)]
struct AnyMap<V>(Vec<(GoValue, V)>);

struct AnyMapVisitor<V>(PhantomData<V>);

impl<'de, V: Deserialize<'de>> Visitor<'de> for AnyMapVisitor<V> {
    type Value = AnyMap<V>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<AnyMap<V>, A::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = access.next_entry::<GoValue, V>()? {
            entries.push(entry);
        }
        Ok(AnyMap(entries))
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for AnyMap<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(AnyMapVisitor(PhantomData))
    }
}

/// Go `struct{}`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct Empty {}

/// Go `struct{ Hello string }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SHello {
    hello: String,
}

/// Go `struct{ B string }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SB {
    b: String,
}

/// Go `struct{ A struct{ B string } }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SA {
    a: SB,
}

/// Go `struct{ A *struct{ B string } }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAPtr {
    a: Option<SB>,
}

/// Go `struct{ A map[string]string }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAMap {
    a: BTreeMap<String, String>,
}

/// Go `struct{ A *map[string]string }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAMapPtr {
    a: Option<BTreeMap<String, String>>,
}

/// Go `struct{ A int }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAInt {
    a: i64,
}

/// Go `struct{ A float64 }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAFloat {
    a: f64,
}

/// Go `struct{ A uint }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAUint {
    a: u64,
}

/// Go `struct{ A []int }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAInts {
    a: Vec<i64>,
}

/// Go `struct{ A [2]int }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAArray {
    a: [i64; 2],
}

/// Go `struct{ B int }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SBInt {
    b: i64,
}

/// Go `struct{ B int "a" }`: the tag is a `field_keys` entry ([`tag_a`]).
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct STagA {
    b: i64,
}

/// The [`Options`] of [`STagA`] (Go's tag `"a"` on field `B`).
fn tag_a() -> Options {
    let mut opts = Options::default();
    opts.field_keys.insert(("STagA", "b"), "a");
    opts
}

/// Go `struct{ A bool }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SABool {
    a: bool,
}

/// Go `struct{ A, B, C, D int }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAbcd {
    a: i64,
    b: i64,
    c: i64,
    d: i64,
}

/// Go `struct{ C int }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SC {
    c: i64,
}

/// Go `struct{ A, B struct{ C int } }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAndB {
    a: SC,
    b: SC,
}

/// Go `struct{ B []int }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SBInts {
    b: Vec<i64>,
}

/// Go `struct{ A int; B int "-" }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SIgnoredB {
    a: i64,
    #[serde(skip)]
    b: i64,
}

/// Go `struct{ A int; C inlineB ",inline" }` (and its `*inlineB` form) with `inlineB`'s fields
/// (`B`, then the inlined `inlineC`'s `C`) listed in the outer struct (R8).
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAbc {
    a: i64,
    b: i64,
    c: i64,
}

/// Go `struct{ A int; C *inlineD ",inline" }` with `inlineD`'s fields (the inlined
/// `*inlineC`'s `C`, then `D`) listed (R8).
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SAcd {
    a: i64,
    c: i64,
    d: i64,
}

/// Go `struct{ A int; C map[string]int ",inline" }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SInlineMap {
    a: i64,
    #[serde(rename = "$gadk_goyaml::inline")]
    c: InlineMap,
}

/// Go `struct{ B interface{} }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct SBAny {
    b: GoValue,
}

/// The bounds every `unmarshalTests` target meets.
trait Target: DeserializeOwned + Default + PartialEq + Debug {}

impl<T: DeserializeOwned + Default + PartialEq + Debug> Target for T {}

/// A consumer of the `unmarshalTests` table, row by row in Go's order.
trait UnmarshalRows {
    /// Row `i`: `data` decoded into a new `T` must equal `want` (a `*TypeError` is allowed).
    fn row<T: Target>(&mut self, i: usize, data: &[u8], want: T, opts: &Options);
    /// Row `i` is skipped (the reason is the comment above the call).
    fn skip(&mut self, i: usize);
}

/// Go's `unmarshalTests` (172 rows), fed to `r` in order.
fn unmarshal_tests(r: &mut impl UnmarshalRows) {
    let d = &Options::default();
    r.row(0, b"", None::<Empty>, d);
    r.row(1, b"{}", Some(Empty {}), d);
    r.row(2, b"v: hi", ss([("v", "hi")]), d);
    r.row(3, b"v: hi", m([("v", s("hi"))]), d);
    r.row(4, b"v: true", ss([("v", "true")]), d);
    r.row(5, b"v: true", m([("v", GoValue::Bool(true))]), d);
    r.row(6, b"v: 10", m([("v", int(10))]), d);
    r.row(7, b"v: 0b10", m([("v", int(2))]), d);
    r.row(8, b"v: 0xA", m([("v", int(10))]), d);
    r.row(9, b"v: 4294967296", m([("v", 4294967296_i64)]), d);
    r.row(10, b"v: 0.1", m([("v", float(0.1))]), d);
    r.row(11, b"v: .1", m([("v", float(0.1))]), d);
    r.row(12, b"v: .Inf", m([("v", float(f64::INFINITY))]), d);
    r.row(13, b"v: -.Inf", m([("v", float(f64::NEG_INFINITY))]), d);
    r.row(14, b"v: -10", m([("v", int(-10))]), d);
    r.row(15, b"v: -.1", m([("v", float(-0.1))]), d);

    // Simple values.
    r.row(16, b"123", Some(123_i64), d);

    // Floats from spec
    r.row(
        17,
        b"canonical: 6.8523e+5",
        m([("canonical", float(6.8523e+5))]),
        d,
    );
    r.row(
        18,
        b"expo: 685.230_15e+03",
        m([("expo", float(685.230_15e+03))]),
        d,
    );
    r.row(
        19,
        b"fixed: 685_230.15",
        m([("fixed", float(685_230.15))]),
        d,
    );
    r.row(
        20,
        b"neginf: -.inf",
        m([("neginf", float(f64::NEG_INFINITY))]),
        d,
    );
    r.row(21, b"fixed: 685_230.15", m([("fixed", 685_230.15_f64)]), d);

    // Bools are per 1.2 spec.
    r.row(
        22,
        b"canonical: true",
        m([("canonical", GoValue::Bool(true))]),
        d,
    );
    r.row(
        23,
        b"canonical: false",
        m([("canonical", GoValue::Bool(false))]),
        d,
    );
    r.row(24, b"bool: True", m([("bool", GoValue::Bool(true))]), d);
    r.row(25, b"bool: False", m([("bool", GoValue::Bool(false))]), d);
    r.row(26, b"bool: TRUE", m([("bool", GoValue::Bool(true))]), d);
    r.row(27, b"bool: FALSE", m([("bool", GoValue::Bool(false))]), d);
    // For backwards compatibility with 1.1, decoding old strings into typed values still works.
    r.row(28, b"option: on", m([("option", true)]), d);
    r.row(29, b"option: y", m([("option", true)]), d);
    r.row(30, b"option: Off", m([("option", false)]), d);
    r.row(31, b"option: No", m([("option", false)]), d);
    r.row(32, b"option: other", BTreeMap::<String, bool>::new(), d);
    // Ints from spec
    r.row(33, b"canonical: 685230", m([("canonical", int(685230))]), d);
    r.row(34, b"decimal: +685_230", m([("decimal", int(685230))]), d);
    r.row(35, b"octal: 02472256", m([("octal", int(685230))]), d);
    r.row(36, b"octal: -02472256", m([("octal", int(-685230))]), d);
    r.row(37, b"octal: 0o2472256", m([("octal", int(685230))]), d);
    r.row(38, b"octal: -0o2472256", m([("octal", int(-685230))]), d);
    r.row(39, b"hexa: 0x_0A_74_AE", m([("hexa", int(685230))]), d);
    r.row(
        40,
        b"bin: 0b1010_0111_0100_1010_1110",
        m([("bin", int(685230))]),
        d,
    );
    r.row(41, b"bin: -0b101010", m([("bin", int(-42))]), d);
    r.row(
        42,
        b"bin: -0b1000000000000000000000000000000000000000000000000000000000000000",
        m([("bin", int(i64::MIN))]),
        d,
    );
    r.row(43, b"decimal: +685_230", m([("decimal", 685230_i64)]), d);

    // Nulls from spec
    r.row(44, b"empty:", m([("empty", GoValue::Null)]), d);
    r.row(45, b"canonical: ~", m([("canonical", GoValue::Null)]), d);
    r.row(46, b"english: null", m([("english", GoValue::Null)]), d);
    r.row(
        47,
        b"~: null key",
        AnyMap(vec![(GoValue::Null, "null key".to_string())]),
        d,
    );
    r.row(48, b"empty:", m([("empty", None::<bool>)]), d);

    // Flow sequence
    r.row(49, b"seq: [A,B]", m([("seq", seq([s("A"), s("B")]))]), d);
    r.row(
        50,
        b"seq: [A,B,C,]",
        m([("seq", vec!["A".to_string(), "B".into(), "C".into()])]),
        d,
    );
    r.row(
        51,
        b"seq: [A,1,C]",
        m([("seq", vec!["A".to_string(), "1".into(), "C".into()])]),
        d,
    );
    r.row(52, b"seq: [A,1,C]", m([("seq", vec![1_i64])]), d);
    r.row(
        53,
        b"seq: [A,1,C]",
        m([("seq", seq([s("A"), int(1), s("C")]))]),
        d,
    );
    // Block sequence
    r.row(
        54,
        b"seq:\n - A\n - B",
        m([("seq", seq([s("A"), s("B")]))]),
        d,
    );
    r.row(
        55,
        b"seq:\n - A\n - B\n - C",
        m([("seq", vec!["A".to_string(), "B".into(), "C".into()])]),
        d,
    );
    r.row(
        56,
        b"seq:\n - A\n - 1\n - C",
        m([("seq", vec!["A".to_string(), "1".into(), "C".into()])]),
        d,
    );
    r.row(57, b"seq:\n - A\n - 1\n - C", m([("seq", vec![1_i64])]), d);
    r.row(
        58,
        b"seq:\n - A\n - 1\n - C",
        m([("seq", seq([s("A"), int(1), s("C")]))]),
        d,
    );

    // Literal block scalar
    r.row(
        59,
        b"scalar: | # Comment\n\n literal\n\n \ttext\n\n",
        ss([("scalar", "\nliteral\n\n\ttext\n")]),
        d,
    );

    // Folded block scalar
    r.row(
        60,
        b"scalar: > # Comment\n\n folded\n line\n \n next\n line\n  * one\n  * two\n\n last\n line\n\n",
        ss([("scalar", "\nfolded line\nnext line\n * one\n * two\n\nlast line\n")]),
        d,
    );

    // Map inside interface with no type hints.
    r.row(
        61,
        b"a: {b: c}",
        AnyMap(vec![(s("a"), gmap([("b", s("c"))]))]),
        d,
    );
    // Non-string map inside interface with no type hints.
    r.row(
        62,
        b"a: {b: c, 1: d}",
        AnyMap(vec![(
            s("a"),
            GoValue::MapAny(vec![(s("b"), s("c")), (int(1), s("d"))]),
        )]),
        d,
    );

    // Structs and type conversions.
    r.row(
        63,
        b"hello: world",
        SHello {
            hello: "world".into(),
        },
        d,
    );
    r.row(
        64,
        b"a: {b: c}",
        SA {
            a: SB { b: "c".into() },
        },
        d,
    );
    r.row(
        65,
        b"a: {b: c}",
        SAPtr {
            a: Some(SB { b: "c".into() }),
        },
        d,
    );
    // Row 66 skipped: `*unmarshalerType` implements Unmarshaler (Non-Goal).
    r.skip(66);
    r.row(
        67,
        b"a: {b: c}",
        SAMap {
            a: ss([("b", "c")]),
        },
        d,
    );
    r.row(
        68,
        b"a: {b: c}",
        SAMapPtr {
            a: Some(ss([("b", "c")])),
        },
        d,
    );
    // Go leaves the map nil; a Rust map has no nil, so the zero value is the empty map.
    r.row(69, b"a:", SAMap::default(), d);
    r.row(70, b"a: 1", SAInt { a: 1 }, d);
    r.row(71, b"a: 1", SAFloat { a: 1.0 }, d);
    r.row(72, b"a: 1.0", SAInt { a: 1 }, d);
    r.row(73, b"a: 1.0", SAUint { a: 1 }, d);
    r.row(74, b"a: [1, 2]", SAInts { a: vec![1, 2] }, d);
    r.row(75, b"a: [1, 2]", SAArray { a: [1, 2] }, d);
    r.row(76, b"a: 1", SBInt { b: 0 }, d);
    r.row(77, b"a: 1", STagA { b: 1 }, &tag_a());
    // Some limited backwards compatibility with the 1.1 spec.
    r.row(78, b"a: YES", SABool { a: true }, d);

    // Some cross type conversions
    r.row(79, b"v: 42", m([("v", 42_u64)]), d);
    r.row(80, b"v: -42", BTreeMap::<String, u64>::new(), d);
    r.row(81, b"v: 4294967296", m([("v", 4294967296_u64)]), d);
    r.row(82, b"v: -4294967296", BTreeMap::<String, u64>::new(), d);

    // int
    r.row(
        83,
        b"int_max: 2147483647",
        m([("int_max", i64::from(i32::MAX))]),
        d,
    );
    r.row(
        84,
        b"int_min: -2147483648",
        m([("int_min", i64::from(i32::MIN))]),
        d,
    );
    r.row(
        85,
        b"int_overflow: 9223372036854775808",
        BTreeMap::<String, i64>::new(),
        d,
    );

    // int64
    r.row(
        86,
        b"int64_max: 9223372036854775807",
        m([("int64_max", i64::MAX)]),
        d,
    );
    r.row(
        87,
        b"int64_max_base2: 0b111111111111111111111111111111111111111111111111111111111111111",
        m([("int64_max_base2", i64::MAX)]),
        d,
    );
    r.row(
        88,
        b"int64_min: -9223372036854775808",
        m([("int64_min", i64::MIN)]),
        d,
    );
    r.row(
        89,
        b"int64_neg_base2: -0b111111111111111111111111111111111111111111111111111111111111111",
        m([("int64_neg_base2", -i64::MAX)]),
        d,
    );
    r.row(
        90,
        b"int64_overflow: 9223372036854775808",
        BTreeMap::<String, i64>::new(),
        d,
    );

    // uint
    r.row(91, b"uint_min: 0", m([("uint_min", 0_u64)]), d);
    r.row(
        92,
        b"uint_max: 4294967295",
        m([("uint_max", u64::from(u32::MAX))]),
        d,
    );
    r.row(93, b"uint_underflow: -1", BTreeMap::<String, u64>::new(), d);

    // uint64
    r.row(94, b"uint64_min: 0", m([("uint64_min", 0_u64)]), d);
    r.row(
        95,
        b"uint64_max: 18446744073709551615",
        m([("uint64_max", u64::MAX)]),
        d,
    );
    r.row(
        96,
        b"uint64_max_base2: 0b1111111111111111111111111111111111111111111111111111111111111111",
        m([("uint64_max_base2", u64::MAX)]),
        d,
    );
    r.row(
        97,
        b"uint64_maxint64: 9223372036854775807",
        m([("uint64_maxint64", i64::MAX as u64)]),
        d,
    );
    r.row(
        98,
        b"uint64_underflow: -1",
        BTreeMap::<String, u64>::new(),
        d,
    );

    // float32
    r.row(
        99,
        b"float32_max: 3.40282346638528859811704183484516925440e+38",
        m([("float32_max", f32::MAX)]),
        d,
    );
    r.row(
        100,
        b"float32_nonzero: 1.401298464324817070923729583289916131280e-45",
        m([("float32_nonzero", f32::from_bits(1))]),
        d,
    );
    r.row(
        101,
        b"float32_maxuint64: 18446744073709551615",
        m([("float32_maxuint64", u64::MAX as f32)]),
        d,
    );
    r.row(
        102,
        b"float32_maxuint64+1: 18446744073709551616",
        m([("float32_maxuint64+1", 2_f32.powi(64))]),
        d,
    );

    // float64
    r.row(
        103,
        b"float64_max: 1.797693134862315708145274237317043567981e+308",
        m([("float64_max", f64::MAX)]),
        d,
    );
    r.row(
        104,
        b"float64_nonzero: 4.940656458412465441765687928682213723651e-324",
        m([("float64_nonzero", f64::from_bits(1))]),
        d,
    );
    r.row(
        105,
        b"float64_maxuint64: 18446744073709551615",
        m([("float64_maxuint64", u64::MAX as f64)]),
        d,
    );
    r.row(
        106,
        b"float64_maxuint64+1: 18446744073709551616",
        m([("float64_maxuint64+1", 2_f64.powi(64))]),
        d,
    );

    // Overflow cases.
    r.row(107, b"v: 4294967297", BTreeMap::<String, i32>::new(), d);
    r.row(108, b"v: 128", BTreeMap::<String, i8>::new(), d);

    // Quoted values.
    r.row(109, b"'1': '\"2\"'", AnyMap(vec![(s("1"), s("\"2\""))]), d);
    r.row(
        110,
        b"v:\n- A\n- 'B\n\n  C'\n",
        m([("v", vec!["A".to_string(), "B\nC".into()])]),
        d,
    );

    // Explicit tags.
    r.row(111, b"v: !!float '1.1'", m([("v", float(1.1))]), d);
    r.row(112, b"v: !!float 0", m([("v", float(0.0))]), d);
    r.row(113, b"v: !!float -1", m([("v", float(-1.0))]), d);
    r.row(114, b"v: !!null ''", m([("v", GoValue::Null)]), d);
    r.row(
        115,
        b"%TAG !y! tag:yaml.org,2002:\n---\nv: !y!int '1'",
        m([("v", int(1))]),
        d,
    );

    // Non-specific tag (Issue #75)
    r.row(116, b"v: ! test", m([("v", s("test"))]), d);

    // Anchors and aliases.
    r.row(
        117,
        b"a: &x 1\nb: &y 2\nc: *x\nd: *y\n",
        SAbcd {
            a: 1,
            b: 2,
            c: 1,
            d: 2,
        },
        d,
    );
    r.row(
        118,
        b"a: &a {c: 1}\nb: *a",
        SAndB {
            a: SC { c: 1 },
            b: SC { c: 1 },
        },
        d,
    );
    r.row(119, b"a: &a [1, 2]\nb: *a", SBInts { b: vec![1, 2] }, d);

    // Bug #1133337
    r.row(120, b"foo: ''", m([("foo", Some(String::new()))]), d);
    r.row(121, b"foo: null", m([("foo", None::<String>)]), d);
    r.row(122, b"foo: null", ss([("foo", "")]), d);
    r.row(123, b"foo: null", m([("foo", GoValue::Null)]), d);

    // Support for ~
    r.row(124, b"foo: ~", m([("foo", None::<String>)]), d);
    r.row(125, b"foo: ~", ss([("foo", "")]), d);
    r.row(126, b"foo: ~", m([("foo", GoValue::Null)]), d);

    // Ignored field
    r.row(127, b"a: 1\nb: 2\n", SIgnoredB { a: 1, b: 0 }, d);

    // Bug #1191981
    r.row(
        128,
        concat!(
            "%YAML 1.1\n",
            "--- !!str\n",
            "\"Generic line break (no glyph)\\n\\\n",
            " Generic line break (glyphed)\\n\\\n",
            " Line separator\\u2028\\\n",
            " Paragraph separator\\u2029\"\n",
        )
        .as_bytes(),
        concat!(
            "Generic line break (no glyph)\n",
            "Generic line break (glyphed)\n",
            "Line separator\u{2028}Paragraph separator\u{2029}",
        )
        .to_string(),
        d,
    );

    // Struct inlining (R8: inlineB's fields listed in the outer struct).
    r.row(129, b"a: 1\nb: 2\nc: 3\n", SAbc { a: 1, b: 2, c: 3 }, d);

    // Struct inlining as a pointer. Adapted (R8): the pointer's fields are listed in the outer
    // struct, so Go's nil `*inlineB` (row 131) reads as zero fields.
    r.row(130, b"a: 1\nb: 2\nc: 3\n", SAbc { a: 1, b: 2, c: 3 }, d);
    r.row(131, b"a: 1\n", SAbc { a: 1, b: 0, c: 0 }, d);
    r.row(132, b"a: 1\nc: 3\nd: 4\n", SAcd { a: 1, c: 3, d: 4 }, d);

    // Map inlining. Adapted: the inline map is an `InlineMap`, whose values are `GoValue`s
    // (Go's `map[string]int` holds ints).
    r.row(
        133,
        b"a: 1\nb: 2\nc: 3\n",
        SInlineMap {
            a: 1,
            c: InlineMap(vec![("b".into(), int(2)), ("c".into(), int(3))]),
        },
        d,
    );

    // bug 1243827
    r.row(134, b"a: -b_c", m([("a", s("-b_c"))]), d);
    r.row(135, b"a: +b_c", m([("a", s("+b_c"))]), d);
    r.row(
        136,
        b"a: 50cent_of_dollar",
        m([("a", s("50cent_of_dollar"))]),
        d,
    );

    // issue #295 (allow scalars with colons in flow mappings and sequences)
    r.row(
        137,
        b"a: {b: https://github.com/go-yaml/yaml}",
        m([("a", gmap([("b", s("https://github.com/go-yaml/yaml"))]))]),
        d,
    );
    r.row(
        138,
        b"a: [https://github.com/go-yaml/yaml]",
        m([("a", seq([s("https://github.com/go-yaml/yaml")]))]),
        d,
    );

    // Duration
    // Row 139 skipped: `map[string]time.Duration` needs time.Duration reflection (Non-Goal).
    r.skip(139);

    // Issue #24.
    r.row(140, b"a: <foo>", ss([("a", "<foo>")]), d);

    // Base 60 floats are obsolete and unsupported.
    r.row(141, b"a: 1:1\n", ss([("a", "1:1")]), d);

    // Binary data. Adapted (rows 142, 143): Go's `string` keeps the decoded bytes
    // "\x80\x81\x82" / 54 x "\x90"; a Rust `String` must be UTF-8, so each invalid byte reads as
    // U+FFFD (Divergence (ledger 6c-1), de.rs `go_string_of`).
    r.row(
        142,
        b"a: !!binary gIGC\n",
        m([("a", "\u{FFFD}".repeat(3))]),
        d,
    );
    r.row(
        143,
        format!("a: !!binary |\n  {}kJ\n  CQ\n", "kJCQ".repeat(17)).as_bytes(),
        m([("a", "\u{FFFD}".repeat(54))]),
        d,
    );
    r.row(
        144,
        format!("a: !!binary |\n  {}\n  ==\n", "A".repeat(70)).as_bytes(),
        m([("a", "\0".repeat(52))]),
        d,
    );

    // Issue #39.
    r.row(
        145,
        b"a:\n b:\n  c: d\n",
        m([(
            "a",
            SBAny {
                b: gmap([("c", s("d"))]),
            },
        )]),
        d,
    );

    // Custom map type. Adapted: Go's named `M` (`map[string]interface{}`) is the same Rust
    // map; nested maps are `GoValue::Map` either way.
    r.row(146, b"a: {b: c}", m([("a", gmap([("b", s("c"))]))]), d);

    // Support encoding.TextUnmarshaler.
    // Rows 147-148 skipped: `textUnmarshaler` implements encoding.TextUnmarshaler (Non-Goal).
    r.skip(147);
    r.skip(148);

    // Timestamps
    // Rows 149-153 skipped: `map[string]time.Time` needs time.Time reflection (Non-Goal).
    r.skip(149);
    r.skip(150);
    r.skip(151);
    r.skip(152);
    r.skip(153);
    // explicit string tag
    r.row(154, b"a: !!str 2015-01-01", m([("a", s("2015-01-01"))]), d);
    // Rows 155-156 skipped: `map[string]time.Time` needs time.Time reflection (Non-Goal).
    r.skip(155);
    r.skip(156);
    // quoted string that's a valid timestamp
    r.row(157, b"a: \"2015-01-01\"", m([("a", s("2015-01-01"))]), d);
    // explicit timestamp tag into interface.
    r.row(
        158,
        b"a: !!timestamp \"2015-01-01\"",
        m([("a", ts("2015-01-01T00:00:00Z"))]),
        d,
    );
    // implicit timestamp tag into interface.
    r.row(
        159,
        b"a: 2015-01-01",
        m([("a", ts("2015-01-01T00:00:00Z"))]),
        d,
    );

    // Encode empty lists as zero-length slices.
    r.row(160, b"a: []", SAInts { a: vec![] }, d);

    // UTF-16-LE
    r.row(
        161,
        b"\xff\xfe\xf1\x00o\x00\xf1\x00o\x00:\x00 \x00v\x00e\x00r\x00y\x00 \x00y\x00e\x00s\x00\n\x00",
        m([("\u{f1}o\u{f1}o", s("very yes"))]),
        d,
    );
    // UTF-16-LE with surrogate.
    r.row(
        162,
        b"\xff\xfe\xf1\x00o\x00\xf1\x00o\x00:\x00 \x00v\x00e\x00r\x00y\x00 \x00y\x00e\x00s\x00 \x00=\xd8\xd4\xdf\n\x00",
        m([("\u{f1}o\u{f1}o", s("very yes \u{1F7D4}"))]),
        d,
    );

    // UTF-16-BE
    r.row(
        163,
        b"\xfe\xff\x00\xf1\x00o\x00\xf1\x00o\x00:\x00 \x00v\x00e\x00r\x00y\x00 \x00y\x00e\x00s\x00\n",
        m([("\u{f1}o\u{f1}o", s("very yes"))]),
        d,
    );
    // UTF-16-BE with surrogate.
    r.row(
        164,
        b"\xfe\xff\x00\xf1\x00o\x00\xf1\x00o\x00:\x00 \x00v\x00e\x00r\x00y\x00 \x00y\x00e\x00s\x00 \xd8=\xdf\xd4\x00\n",
        m([("\u{f1}o\u{f1}o", s("very yes \u{1F7D4}"))]),
        d,
    );

    // This *is* in fact a float number, per the spec. #171 was a mistake.
    r.row(165, b"a: 123456e1\n", m([("a", float(123456e1))]), d);
    r.row(166, b"a: 123456E1\n", m([("a", float(123456e1))]), d);
    // yaml-test-suite 3GZX: Spec Example 7.1. Alias Nodes
    r.row(
        167,
        b"First occurrence: &anchor Foo\nSecond occurrence: *anchor\nOverride anchor: &anchor Bar\nReuse anchor: *anchor\n",
        m([
            ("First occurrence", s("Foo")),
            ("Second occurrence", s("Foo")),
            ("Override anchor", s("Bar")),
            ("Reuse anchor", s("Bar")),
        ]),
        d,
    );
    // Single document with garbage following it.
    r.row(168, b"---\nhello\n...\n}not yaml", "hello".to_string(), d);

    // Comment scan exhausting the input buffer (issue #469).
    r.row(
        169,
        format!("true\n#{}", " ".repeat(512 * 3)).as_bytes(),
        "true".to_string(),
        d,
    );
    r.row(
        170,
        format!("true #{}", " ".repeat(512 * 3)).as_bytes(),
        "true".to_string(),
        d,
    );

    // CRLF
    r.row(
        171,
        b"a: b\r\nc:\r\n- d\r\n- e\r\n",
        m([("a", s("b")), ("c", seq([s("d"), s("e")]))]),
        d,
    );
}

/// The number of rows in Go's `unmarshalTests`.
const UNMARSHAL_TESTS: usize = 172;

/// The `unmarshalTests` rows skipped for a Non-Goal.
const UNMARSHAL_TESTS_SKIPPED: [usize; 11] = [66, 139, 147, 148, 149, 150, 151, 152, 153, 155, 156];

/// Go's partial value after a `*TypeError`: `from_node_partial` over the parsed document must
/// report the same error `unmarshal` returned.
fn partial<T: Target>(data: &[u8], opts: &Options, err: &TypeError) -> T {
    let doc = unmarshal_node(data).unwrap();
    let (value, terr) = from_node_partial::<T>(&doc, doc.root(), opts).unwrap();
    assert_eq!(terr.as_ref(), Some(err));
    value
}

/// `TestUnmarshal`: `yaml.Unmarshal` of every row.
#[derive(Default)]
struct UnmarshalCheck(RowCount);

impl UnmarshalRows for UnmarshalCheck {
    fn row<T: Target>(&mut self, i: usize, data: &[u8], want: T, opts: &Options) {
        assert_eq!(
            data,
            corpus(&format!("decode-unmarshalTests-{i:03}")),
            "row {i}: input differs from the probe corpus"
        );
        let got = match unmarshal::<T>(data, opts) {
            Ok(v) => v,
            Err(Error::Type(te)) => partial::<T>(data, opts, &te),
            Err(e) => panic!("row {i} {:?}: {e}", String::from_utf8_lossy(data)),
        };
        assert_eq!(got, want, "row {i} {:?}", String::from_utf8_lossy(data));
        self.0.ported(i);
    }

    fn skip(&mut self, i: usize) {
        self.0.skipped(i);
    }
}

// parity: adapted: Go returns the partial value next to a *TypeError; unmarshal returns only Err(Error::Type) (ledgered, R7), so TypeError rows read it via from_node_partial; struct targets are serde structs (inlined structs listed field by field, R8); 11 rows skipped for Non-Goals, rows 69/130-133/142/143/146 adapted (see their comments)
#[test]
fn test_unmarshal() {
    let mut check = UnmarshalCheck::default();
    unmarshal_tests(&mut check);
    check.0.finish(UNMARSHAL_TESTS, &UNMARSHAL_TESTS_SKIPPED);
}

#[test]
fn test_unmarshal_full_timestamp() {
    // Full timestamp in same format as encoded. This is confirmed to be
    // properly decoded by Python as a timestamp as well.
    let str = "2015-02-24T18:19:39.123456789-03:00";
    let t: GoValue = unmarshal(str.as_bytes(), &Options::default()).unwrap();
    let GoValue::Timestamp(t) = t else {
        panic!("want a time.Time, got {t:?}");
    };
    let minus3 = FixedOffset::west_opt(3 * 3600).unwrap();
    assert_eq!(*t.offset(), minus3);
    assert_eq!(t.naive_local().to_string(), "2015-02-24 18:19:39.123456789");
    assert_eq!(t.nanosecond(), 123_456_789);
    assert_eq!(t.naive_utc().to_string(), "2015-02-24 21:19:39.123456789");
}

/// `TestDecoderSingleDocument`: `Decoder.Decode` of every row with input.
#[derive(Default)]
struct DecoderCheck(RowCount);

impl UnmarshalRows for DecoderCheck {
    fn row<T: Target>(&mut self, i: usize, data: &[u8], want: T, opts: &Options) {
        self.0.ported(i);
        if data.is_empty() {
            // Behaviour differs when there's no YAML.
            return;
        }
        let got = match Decoder::new(data).decode::<T>(opts) {
            Ok(Some(v)) => v,
            Ok(None) => panic!("row {i}: no document"),
            Err(Error::Type(te)) => {
                let doc = Decoder::new(data).decode_node().unwrap().unwrap();
                let (value, terr) = from_node_partial::<T>(&doc, doc.root(), opts).unwrap();
                assert_eq!(terr, Some(te), "row {i}");
                value
            }
            Err(e) => panic!("row {i} {:?}: {e}", String::from_utf8_lossy(data)),
        };
        assert_eq!(got, want, "row {i} {:?}", String::from_utf8_lossy(data));
    }

    fn skip(&mut self, i: usize) {
        self.0.skipped(i);
    }
}

// parity: adapted: as test_unmarshal, through Decoder::decode; a *TypeError row's partial value is read via decode_node + from_node_partial (Decoder::decode returns only Err(Error::Type), ledgered R7)
#[test]
fn test_decoder_single_document() {
    let mut check = DecoderCheck::default();
    unmarshal_tests(&mut check);
    check.0.finish(UNMARSHAL_TESTS, &UNMARSHAL_TESTS_SKIPPED);
}

/// Go's `decoderTests`: (input, the `interface{}` values `Decode` yields until `io.EOF`).
fn decoder_tests() -> Vec<(&'static str, Vec<GoValue>)> {
    vec![
        ("", vec![]),
        ("a: b", vec![gmap([("a", s("b"))])]),
        ("---\na: b\n...\n", vec![gmap([("a", s("b"))])]),
        (
            "---\n'hello'\n...\n---\ngoodbye\n...\n",
            vec![s("hello"), s("goodbye")],
        ),
    ]
}

#[test]
fn test_decoder() {
    for (i, (data, want)) in decoder_tests().into_iter().enumerate() {
        assert_eq!(
            data.as_bytes(),
            corpus(&format!("decode-decoderTests-{i:03}")),
            "test {i}"
        );
        let mut values = Vec::new();
        let mut dec = Decoder::new(data.as_bytes());
        while let Some(value) = dec.decode::<GoValue>(&Options::default()).unwrap() {
            values.push(value);
        }
        assert_eq!(values, want, "test {i}: {data:?}");
    }
}

/// Go's `errReader`.
struct ErrReader;

impl std::io::Read for ErrReader {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("some read error"))
    }
}

#[test]
fn test_decoder_read_error() {
    let err = Decoder::new(ErrReader)
        .decode::<Empty>(&Options::default())
        .unwrap_err();
    assert_eq!(err.to_string(), "yaml: input error: some read error");
}

#[test]
fn test_unmarshal_na_n() {
    let value: BTreeMap<String, GoValue> =
        unmarshal(b"notanum: .NaN", &Options::default()).unwrap();
    assert!(matches!(value["notanum"], GoValue::Float(f) if f.is_nan()));
}

/// Go's `unmarshalErrorTests`: (input, error). Go's `ErrorMatches` patterns are written as the
/// exact texts they match (rows 8, 9, 12 and 13 are regular expressions in Go:
/// `yaml: invalid map key: \[\]interface \{\}\{"\."\}`, `…map\[string]interface \{\}…`,
/// `.*could not find expected ':'`, `^yaml: line 3: could not find expected ':'$`).
fn unmarshal_error_tests() -> Vec<(Vec<u8>, &'static str)> {
    vec![
        (
            b"v: !!float 'error'".to_vec(),
            "yaml: cannot decode !!str `error` as a !!float",
        ),
        (
            b"v: [A,".to_vec(),
            "yaml: line 1: did not find expected node content",
        ),
        (
            b"v:\n- [A,".to_vec(),
            "yaml: line 2: did not find expected node content",
        ),
        (
            b"a:\n- b: *,".to_vec(),
            "yaml: line 2: did not find expected alphabetic or numeric character",
        ),
        (b"a: *b\n".to_vec(), "yaml: unknown anchor 'b' referenced"),
        (
            b"a: &a\n  b: *a\n".to_vec(),
            "yaml: anchor 'a' value contains itself",
        ),
        (
            b"value: -".to_vec(),
            "yaml: block sequence entries are not allowed in this context",
        ),
        (
            b"a: !!binary ==".to_vec(),
            "yaml: !!binary value contains invalid base64 data",
        ),
        (
            b"{[.]}".to_vec(),
            "yaml: invalid map key: []interface {}{\".\"}",
        ),
        (
            b"{{.}}".to_vec(),
            "yaml: invalid map key: map[string]interface {}{\".\":interface {}(nil)}",
        ),
        (
            b"b: *a\na: &a {c: 1}".to_vec(),
            "yaml: unknown anchor 'a' referenced",
        ),
        (
            b"%TAG !%79! tag:yaml.org,2002:\n---\nv: !%79!int '1'".to_vec(),
            "yaml: did not find expected whitespace",
        ),
        (
            b"a:\n  1:\nb\n  2:".to_vec(),
            "yaml: line 3: could not find expected ':'",
        ),
        (
            b"a: 1\nb: 2\nc 2\nd: 3\n".to_vec(),
            "yaml: line 3: could not find expected ':'",
        ),
        // Issue #665
        (
            b"#\n-\n{".to_vec(),
            "yaml: line 3: could not find expected ':'",
        ),
        // Issue #666
        (
            b"0: [:!00 \xef".to_vec(),
            "yaml: incomplete UTF-8 octet sequence",
        ),
        (
            concat!(
                "a: &a [00,00,00,00,00,00,00,00,00]\n",
                "b: &b [*a,*a,*a,*a,*a,*a,*a,*a,*a]\n",
                "c: &c [*b,*b,*b,*b,*b,*b,*b,*b,*b]\n",
                "d: &d [*c,*c,*c,*c,*c,*c,*c,*c,*c]\n",
                "e: &e [*d,*d,*d,*d,*d,*d,*d,*d,*d]\n",
                "f: &f [*e,*e,*e,*e,*e,*e,*e,*e,*e]\n",
                "g: &g [*f,*f,*f,*f,*f,*f,*f,*f,*f]\n",
                "h: &h [*g,*g,*g,*g,*g,*g,*g,*g,*g]\n",
                "i: &i [*h,*h,*h,*h,*h,*h,*h,*h,*h]\n",
            )
            .as_bytes()
            .to_vec(),
            "yaml: document contains excessive aliasing",
        ),
    ]
}

// parity: adapted: Go's ErrorMatches regular expressions are checked as the exact error texts they match (no regex dependency)
#[test]
fn test_unmarshal_errors() {
    let tests = unmarshal_error_tests();
    assert_eq!(tests.len(), 17);
    for (i, (data, want)) in tests.iter().enumerate() {
        assert_eq!(
            *data,
            corpus(&format!("decode-unmarshalErrorTests-{i:03}")),
            "test {i}"
        );
        let err = unmarshal::<GoValue>(data, &Options::default()).unwrap_err();
        assert_eq!(err.to_string(), *want, "test {i}");
    }
}

// parity: adapted: Go's ErrorMatches regular expressions are checked as the exact error texts they match (no regex dependency)
#[test]
fn test_decoder_errors() {
    for (i, (data, want)) in unmarshal_error_tests().iter().enumerate() {
        let err = Decoder::new(data.as_slice())
            .decode::<GoValue>(&Options::default())
            .unwrap_err();
        assert_eq!(err.to_string(), *want, "test {i}");
    }
}

/// From http://yaml.org/type/merge.html (Go's `mergeTests`; the `# Inlined map ` comment line
/// ends in a space).
const MERGE_TESTS: &str = concat!(
    r#"
anchors:
  list:
    - &CENTER { "x": 1, "y": 2 }
    - &LEFT   { "x": 0, "y": 2 }
    - &BIG    { "r": 10 }
    - &SMALL  { "r": 1 }

# All the following maps are equal:

plain:
  # Explicit keys
  "x": 1
  "y": 2
  "r": 10
  label: center/big

mergeOne:
  # Merge one map
  << : *CENTER
  "r": 10
  label: center/big

mergeMultiple:
  # Merge multiple maps
  << : [ *CENTER, *BIG ]
  label: center/big

override:
  # Override
  << : [ *BIG, *LEFT, *SMALL ]
  "x": 1
  label: center/big

shortTag:
  # Explicit short merge tag
  !!merge "<<" : [ *CENTER, *BIG ]
  label: center/big

longTag:
  # Explicit merge long tag
  !<tag:yaml.org,2002:merge> "<<" : [ *CENTER, *BIG ]
  label: center/big

inlineMap:
  # Inlined map"#,
    " ",
    r#"
  << : {"x": 1, "y": 2, "r": 10}
  label: center/big

inlineSequenceMap:
  # Inlined map in sequence
  << : [ *CENTER, {"r": 10} ]
  label: center/big
"#
);

#[test]
fn test_merge() {
    assert_eq!(MERGE_TESTS.as_bytes(), corpus("decode-mergeTests"));
    let want = sorted(&gmap([
        ("x", int(1)),
        ("y", int(2)),
        ("r", int(10)),
        ("label", s("center/big")),
    ]));

    let m: AnyMap<GoValue> = unmarshal(MERGE_TESTS.as_bytes(), &Options::default()).unwrap();
    assert_eq!(m.0.len(), 9);
    for (name, test) in &m.0 {
        if *name == s("anchors") {
            continue;
        }
        // Go compares "plain" against the same map built with string keys.
        assert_eq!(sorted(test), want, "test {name:?} failed");
    }
}

/// `TestMergeStruct`'s `Data`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct MergeData {
    x: i64,
    y: i64,
    r: i64,
    label: String,
}

#[test]
fn test_merge_struct() {
    let want = MergeData {
        x: 1,
        y: 2,
        r: 10,
        label: "center/big".into(),
    };
    let m: BTreeMap<String, MergeData> =
        unmarshal(MERGE_TESTS.as_bytes(), &Options::default()).unwrap();
    assert_eq!(m.len(), 9);
    for (name, test) in &m {
        if name == "anchors" {
            continue;
        }
        assert_eq!(*test, want, "test {name:?} failed");
    }
}

/// Go's `mergeTestsNested`.
const MERGE_TESTS_NESTED: &str = "
mergeouter1: &mergeouter1
    d: 40
    e: 50

mergeouter2: &mergeouter2
    e: 5
    f: 6
    g: 70

mergeinner1: &mergeinner1
    <<: *mergeouter1
    inner:
        a: 1
        b: 2

mergeinner2: &mergeinner2
    <<: *mergeouter2
    inner:
        a: -1
        b: -2

outer:
    <<: [*mergeinner1, *mergeinner2]
    f: 60
    inner:
        a: 10
";

/// `TestMergeNestedStruct`'s `Inner`; its `Default` is Go's pre-set `Inner{C: 30}`.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(default)]
struct NestedInner {
    a: i64,
    b: i64,
    c: i64,
}

impl Default for NestedInner {
    fn default() -> Self {
        NestedInner { a: 0, b: 0, c: 30 }
    }
}

/// `TestMergeNestedStruct`'s `Outer` (`Inline map[string]int ",inline"` is an `InlineMap`).
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct NestedOuter {
    d: i64,
    e: i64,
    inner: NestedInner,
    #[serde(rename = "$gadk_goyaml::inline")]
    inline: InlineMap,
}

/// `TestMergeNestedStruct`'s `Data`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct NestedData {
    outer: NestedOuter,
}

// parity: adapted: serde builds a fresh value, so Go's pre-set Inner{C: 30} is NestedInner's Default (container #[serde(default)]); the inline map[string]int is an InlineMap of GoValue ints
#[test]
fn test_merge_nested_struct() {
    // Issue #818: Merging used to just unmarshal twice on the target
    // value, which worked for maps as these were replaced by the new map,
    // but not on struct values as these are preserved. This resulted in
    // the nested data from the merged map to be mixed up with the data
    // from the map being merged into.
    //
    // This test also prevents two potential bugs from showing up:
    //
    // 1) A simple implementation might just zero out the nested value
    //    before unmarshaling the second time, but this would clobber previous
    //    data that is usually respected ({C: 30} below).
    //
    // 2) A simple implementation might attempt to handle the key skipping
    //    directly by iterating over the merging map without recursion, but
    //    there are more complex cases that require recursion.
    //
    // Quick summary of the fields:
    //
    // - A must come from outer and not overriden
    // - B must not be set as its in the ignored merge
    // - C should still be set as it's preset in the value
    // - D should be set from the recursive merge
    // - E should be set from the first recursive merge, ignored on the second
    // - F should be set in the inlined map from outer, ignored later
    // - G should be set in the inlined map from the second recursive merge
    assert_eq!(
        MERGE_TESTS_NESTED.as_bytes(),
        corpus("decode-mergeTestsNested")
    );
    let want = NestedData {
        outer: NestedOuter {
            d: 40,
            e: 50,
            inner: NestedInner { a: 10, b: 0, c: 30 },
            inline: InlineMap(vec![("f".into(), int(60)), ("g".into(), int(70))]),
        },
    };
    let mut test: NestedData =
        unmarshal(MERGE_TESTS_NESTED.as_bytes(), &Options::default()).unwrap();
    test.outer.inline.0.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(test, want);

    // Repeat test with a map.

    let testm: BTreeMap<String, GoValue> =
        unmarshal(MERGE_TESTS_NESTED.as_bytes(), &Options::default()).unwrap();
    let wantm = gmap([
        ("f", int(60)),
        ("inner", gmap([("a", int(10))])),
        ("d", int(40)),
        ("e", int(50)),
        ("g", int(70)),
    ]);
    assert_eq!(sorted(&testm["outer"]), sorted(&wantm));
}

/// Go's `unmarshalNullTests` inputs, by row.
const UNMARSHAL_NULL_INPUTS: [&str; 9] = [
    "null",
    "null",
    "null",
    "null",
    "null",
    "null",
    "null",
    "s2: null\ns3: null",
    "s2: null\ns3: null",
];

// parity: adapted: serde cannot decode into a pre-filled value; rows decode null into Option<T> (None = Go leaves or nils the caller's value), and rows 7-8 (Go merges into a pre-filled map) decode into a fresh map, whose result was checked against Go (map[s2:0 s3:0], map[s2:<nil> s3:<nil>])
#[test]
fn test_unmarshal_null() {
    let opts = &Options::default();
    for (i, input) in UNMARSHAL_NULL_INPUTS.iter().enumerate() {
        assert_eq!(
            input.as_bytes(),
            corpus(&format!("decode-unmarshalNullTests-{i:03}")),
            "row {i}"
        );
    }
    let null = UNMARSHAL_NULL_INPUTS[0].as_bytes();

    // var v interface{} = "v" → nil.
    let v: GoValue = unmarshal(null, opts).unwrap();
    assert_eq!(v, GoValue::Null);

    // var s = "s" → unchanged.
    let mut s = "s".to_string();
    if let Some(got) = unmarshal::<Option<String>>(null, opts).unwrap() {
        s = got;
    }
    assert_eq!(s, "s");

    // sptr := &s → nil.
    let sptr: Option<String> = unmarshal(null, opts).unwrap();
    assert_eq!(sptr, None);

    // var i = 1 → unchanged.
    let mut i = 1_i64;
    if let Some(got) = unmarshal::<Option<i64>>(null, opts).unwrap() {
        i = got;
    }
    assert_eq!(i, 1);

    // iptr := &i → nil.
    let iptr: Option<i64> = unmarshal(null, opts).unwrap();
    assert_eq!(iptr, None);

    // &map[string]int{"s": 1} → nil map.
    let mptr: Option<BTreeMap<String, i64>> = unmarshal(null, opts).unwrap();
    assert_eq!(mptr, None);

    // map[string]int{"s": 1} (not a pointer) → unchanged.
    let mut mv = m([("s", 1_i64)]);
    if let Some(got) = unmarshal::<Option<BTreeMap<String, i64>>>(null, opts).unwrap() {
        mv = got;
    }
    assert_eq!(mv, m([("s", 1_i64)]));

    // A fresh map[string]int: null values are kept as zero.
    let m7: BTreeMap<String, i64> = unmarshal(UNMARSHAL_NULL_INPUTS[7].as_bytes(), opts).unwrap();
    assert_eq!(m7, m([("s2", 0), ("s3", 0)]));

    // A fresh map[string]interface{}: null values are kept as nil.
    let m8: BTreeMap<String, GoValue> =
        unmarshal(UNMARSHAL_NULL_INPUTS[8].as_bytes(), opts).unwrap();
    assert_eq!(m8, m([("s2", GoValue::Null), ("s3", GoValue::Null)]));
}

/// `TestUnmarshalPreservesData`'s struct; its `Default` is Go's pre-set `A = 42`, `C = 88`.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(default)]
struct PreservesData {
    a: i64,
    b: i64,
    #[serde(skip)]
    c: i64,
}

impl Default for PreservesData {
    fn default() -> Self {
        PreservesData { a: 42, b: 0, c: 88 }
    }
}

// parity: adapted: serde builds a fresh value, so Go's pre-set fields (A = 42, C = 88) are the struct's Default (container #[serde(default)])
#[test]
fn test_unmarshal_preserves_data() {
    let v: PreservesData = unmarshal(b"---", &Options::default()).unwrap();
    assert_eq!(v.a, 42);
    assert_eq!(v.b, 0);
    assert_eq!(v.c, 88);

    let v: PreservesData = unmarshal(b"b: 21\nc: 99", &Options::default()).unwrap();
    assert_eq!(v.a, 42);
    assert_eq!(v.b, 21);
    assert_eq!(v.c, 88);
}

/// `TestUnmarshalSliceOnPreset`'s struct; its `Default` is Go's pre-set `A = []int{1}`.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(default)]
struct SliceOnPreset {
    a: Vec<i64>,
}

impl Default for SliceOnPreset {
    fn default() -> Self {
        SliceOnPreset { a: vec![1] }
    }
}

// parity: adapted: serde builds a fresh value, so Go's pre-set A = []int{1} is the struct's Default (container #[serde(default)])
#[test]
fn test_unmarshal_slice_on_preset() {
    // Issue #48.
    let v: SliceOnPreset = unmarshal(b"a: [2]", &Options::default()).unwrap();
    assert_eq!(v.a, vec![2]);
}

/// `unmarshalStrictTests` row 0's `struct{ A, B int }`.
#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
struct StrictAB {
    a: i64,
    b: i64,
}

/// A consumer of the unique-key rows of `unmarshalStrictTests` (Go's `unique: true`).
fn check_unique<T: Target>(i: usize, data: &str, error: &str) {
    assert_eq!(
        data.as_bytes(),
        corpus(&format!("decode-unmarshalStrictTests-{i:03}")),
        "row {i}"
    );
    let err = Decoder::new(data.as_bytes())
        .decode::<T>(&Options::default())
        .unwrap_err();
    assert_eq!(err.to_string(), error, "row {i}");
}

// parity: adapted: Decoder.KnownFields (strict mode) is a Non-Goal, so row 0's KnownFields half is skipped (its plain Unmarshal half is ported); rows 1-5 (unique keys, KnownFields off) are ported with inlineB's fields listed (R8) and map[interface{}]interface{} as AnyMap
#[test]
fn test_unmarshal_known_fields() {
    // Row 0: plain Unmarshal; the KnownFields decode (`field c not found in type struct { A int;
    // B int }`) is skipped.
    let data = "a: 1\nc: 2\n";
    assert_eq!(data.as_bytes(), corpus("decode-unmarshalStrictTests-000"));
    let v: StrictAB = unmarshal(data.as_bytes(), &Options::default()).unwrap();
    assert_eq!(v, StrictAB { a: 1, b: 0 });

    check_unique::<StrictAB>(
        1,
        "a: 1\nb: 2\na: 3\n",
        "yaml: unmarshal errors:\n  line 3: mapping key \"a\" already defined at line 1",
    );
    check_unique::<SAbc>(
        2,
        "c: 3\na: 1\nb: 2\nc: 4\n",
        "yaml: unmarshal errors:\n  line 4: mapping key \"c\" already defined at line 1",
    );
    check_unique::<SAbc>(
        3,
        "c: 0\na: 1\nb: 2\nc: 1\n",
        "yaml: unmarshal errors:\n  line 4: mapping key \"c\" already defined at line 1",
    );
    check_unique::<SInlineMap>(
        4,
        "c: 1\na: 1\nb: 2\nc: 3\n",
        "yaml: unmarshal errors:\n  line 4: mapping key \"c\" already defined at line 1",
    );
    check_unique::<AnyMap<GoValue>>(
        5,
        "a: 1\n9: 2\nnull: 3\n9: 4",
        "yaml: unmarshal errors:\n  line 4: mapping key \"9\" already defined at line 2",
    );
}

#[test]
fn test_fuzz_crashers() {
    let cases = [
        // runtime error: index out of range
        "\"\\0\\\r\n",
        // should not happen
        "  0: [\n] 0",
        "? ? \"\n\" 0",
        "    - {\n000}0",
        "0:\n  0: [0\n] 0",
        "    - \"\n000\"0",
        "    - \"\n000\"\"",
        "0:\n    - {\n000}0",
        "0:\n    - \"\n000\"0",
        "0:\n    - \"\n000\"\"",
        // runtime error: index out of range
        " \u{feff}\n",
        "? \u{feff}\n",
        "? \u{feff}:\n",
        "0: \u{feff}\n",
        "? \u{feff}: \u{feff}\n",
    ];
    for (i, data) in cases.iter().enumerate() {
        assert_eq!(
            data.as_bytes(),
            corpus(&format!("decode-TestFuzzCrashers-cases-{i:03}")),
            "case {i}"
        );
        let _ = unmarshal::<GoValue>(data.as_bytes(), &Options::default());
    }
}
