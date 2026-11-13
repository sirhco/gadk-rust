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

//! Port of yaml.v3 v3.0.1 `sorter.go`: the `keyList` order used for map keys on encode
//! (numbers before strings by `reflect.Kind`, numeric runs compared by value, letters before
//! digits), over [`SortKey`]s — what Go's `reflect.Value` keys look like after dereferencing
//! interfaces and pointers.
// Bodies are Task 5's; Task 5 removes this line.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use crate::value::GoValue;

/// Go's `reflect.Kind` numbers (the order `keyList.Less` falls back to).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ReflectKind {
    /// `reflect.Invalid`.
    Invalid = 0,
    /// `reflect.Bool`.
    Bool = 1,
    /// `reflect.Int`.
    Int = 2,
    /// `reflect.Int8`.
    Int8 = 3,
    /// `reflect.Int16`.
    Int16 = 4,
    /// `reflect.Int32`.
    Int32 = 5,
    /// `reflect.Int64`.
    Int64 = 6,
    /// `reflect.Uint`.
    Uint = 7,
    /// `reflect.Uint8`.
    Uint8 = 8,
    /// `reflect.Uint16`.
    Uint16 = 9,
    /// `reflect.Uint32`.
    Uint32 = 10,
    /// `reflect.Uint64`.
    Uint64 = 11,
    /// `reflect.Uintptr`.
    Uintptr = 12,
    /// `reflect.Float32`.
    Float32 = 13,
    /// `reflect.Float64`.
    Float64 = 14,
    /// `reflect.Complex64`.
    Complex64 = 15,
    /// `reflect.Complex128`.
    Complex128 = 16,
    /// `reflect.Array`.
    Array = 17,
    /// `reflect.Chan`.
    Chan = 18,
    /// `reflect.Func`.
    Func = 19,
    /// `reflect.Interface` (a nil interface key stays this kind).
    Interface = 20,
    /// `reflect.Map`.
    Map = 21,
    /// `reflect.Pointer` (a nil pointer key stays this kind).
    Pointer = 22,
    /// `reflect.Slice`.
    Slice = 23,
    /// `reflect.String`.
    String = 24,
    /// `reflect.Struct` (e.g. `time.Time`).
    Struct = 25,
    /// `reflect.UnsafePointer`.
    UnsafePointer = 26,
}

/// One map key as `keyList.Less` sees it (after `Elem()` through interfaces and pointers).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SortKey {
    /// A `bool` key.
    Bool(bool),
    /// A signed integer key of the given kind (`Int` … `Int64`).
    Int(ReflectKind, i64),
    /// An unsigned integer key of the given kind (`Uint` … `Uintptr`).
    Uint(ReflectKind, u64),
    /// A float key of the given kind (`Float32`/`Float64`; f32 values widened).
    Float(ReflectKind, f64),
    /// A string key, as the runes Go's `[]rune(s)` yields (invalid UTF-8 bytes become
    /// U+FFFD one byte at a time).
    String(String),
    /// Any other key: compared by kind only.
    Other(ReflectKind),
}

impl SortKey {
    /// The key's `reflect.Kind`.
    pub(crate) fn kind(&self) -> ReflectKind {
        todo!("Task 5")
    }

    /// The `SortKey` of a `map[interface{}]interface{}` key holding `v` (`Null` → a nil
    /// interface, `Int` → `int`, `Uint` → `uint64`, `Float` → `float64`, `String`/`Bytes` →
    /// `string`, `Timestamp` → `time.Time`, `Seq` → slice, `Map`/`MapAny` → map).
    pub(crate) fn from_go_value(v: &GoValue) -> SortKey {
        todo!("Task 5")
    }
}

/// Go: sorter.go `keyList.Less`.
pub(crate) fn key_list_less(a: &SortKey, b: &SortKey) -> bool {
    // Go: sorter.go Less
    todo!("Task 5")
}

/// Go: sorter.go `keyFloat` — the numeric value of a number/bool key.
pub(crate) fn key_float(v: &SortKey) -> Option<f64> {
    // Go: sorter.go keyFloat
    todo!("Task 5")
}

/// Go: sorter.go `numLess` — `a < b` for two numeric keys of the same kind.
pub(crate) fn num_less(a: &SortKey, b: &SortKey) -> bool {
    // Go: sorter.go numLess
    todo!("Task 5")
}

/// Go's `sort.Sort(keyList(keys))`: sorts `items` by [`key_list_less`] on `key(item)` with the
/// exact algorithm of Go's `sort.Sort` (pdqsort), so orders of keys `Less` does not totally
/// order match Go (and never trip Rust's total-order checks).
pub(crate) fn sort_keys<T>(items: &mut [T], key: impl Fn(&T) -> &SortKey) {
    // Go: sorter.go sort.Sort(keyList)
    todo!("Task 5")
}
