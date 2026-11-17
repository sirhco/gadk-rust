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

//! The order used for map keys on encode (numbers before strings by kind, numeric runs compared
//! by value, letters before digits), over [`SortKey`]s (a key after looking through interfaces
//! and pointers), and the pattern-defeating quicksort yaml.v3 sorts them with. The key order
//! ([`key_list_less`]) is not a strict weak order (e.g. `"a1" < "a01"` and `"a01" < "a1"` can
//! both be false while their relations to a third key disagree), so the resulting order is
//! defined by the sorting algorithm, which is reproduced exactly here; Rust's `sort_by` (which
//! may panic on such orders) cannot be used.

use crate::value::GoValue;

/// Value kinds numbered as in yaml.v3's key order, which falls back to them. Kinds that no
/// serde value or `GoValue` has stay unbuilt.
#[allow(dead_code)] // the whole kind list is kept, though some kinds are never built
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ReflectKind {
    /// The Invalid kind.
    Invalid = 0,
    /// The Bool kind.
    Bool = 1,
    /// The Int kind.
    Int = 2,
    /// The Int8 kind.
    Int8 = 3,
    /// The Int16 kind.
    Int16 = 4,
    /// The Int32 kind.
    Int32 = 5,
    /// The Int64 kind.
    Int64 = 6,
    /// The Uint kind.
    Uint = 7,
    /// The Uint8 kind.
    Uint8 = 8,
    /// The Uint16 kind.
    Uint16 = 9,
    /// The Uint32 kind.
    Uint32 = 10,
    /// The Uint64 kind.
    Uint64 = 11,
    /// The Uintptr kind.
    Uintptr = 12,
    /// The Float32 kind.
    Float32 = 13,
    /// The Float64 kind.
    Float64 = 14,
    /// The Complex64 kind.
    Complex64 = 15,
    /// The Complex128 kind.
    Complex128 = 16,
    /// The Array kind.
    Array = 17,
    /// The Chan kind.
    Chan = 18,
    /// The Func kind.
    Func = 19,
    /// The Interface kind (a null interface key stays this kind).
    Interface = 20,
    /// The Map kind.
    Map = 21,
    /// The Pointer kind (a null pointer key stays this kind).
    Pointer = 22,
    /// The Slice kind.
    Slice = 23,
    /// The String kind.
    String = 24,
    /// The Struct kind (e.g. a timestamp).
    Struct = 25,
    /// The UnsafePointer kind.
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
    /// A string key, as its characters (invalid UTF-8 bytes become
    /// U+FFFD one byte at a time).
    String(String),
    /// Any other key: compared by kind only.
    Other(ReflectKind),
}

impl SortKey {
    /// The key's kind.
    pub(crate) fn kind(&self) -> ReflectKind {
        match self {
            SortKey::Bool(_) => ReflectKind::Bool,
            SortKey::Int(k, _) | SortKey::Uint(k, _) | SortKey::Float(k, _) | SortKey::Other(k) => {
                *k
            }
            SortKey::String(_) => ReflectKind::String,
        }
    }

    /// The `SortKey` of a dynamically typed map key holding `v` (`Null` → a null interface,
    /// `Int` → a signed integer, `Uint` → `uint64`, `Float` → `float64`, `String`/`Bytes` →
    /// string, `Timestamp` → a struct, `Seq` → slice, `Map`/`MapAny` → map).
    pub(crate) fn from_go_value(v: &GoValue) -> SortKey {
        match v {
            GoValue::Null => SortKey::Other(ReflectKind::Interface),
            GoValue::Bool(b) => SortKey::Bool(*b),
            GoValue::Int(i) => SortKey::Int(ReflectKind::Int, *i),
            GoValue::Uint(u) => SortKey::Uint(ReflectKind::Uint64, *u),
            GoValue::Float(f) => SortKey::Float(ReflectKind::Float64, *f),
            GoValue::String(s) => SortKey::String(s.clone()),
            GoValue::Bytes(b) => SortKey::String(go_runes_lossy(b)),
            GoValue::Timestamp(_) => SortKey::Other(ReflectKind::Struct),
            GoValue::Seq(_) => SortKey::Other(ReflectKind::Slice),
            GoValue::Map(_) | GoValue::MapAny(_) => SortKey::Other(ReflectKind::Map),
        }
    }
}

/// The characters of a string holding arbitrary bytes, as a `String`: every byte that does
/// not start a valid UTF-8 sequence becomes one U+FFFD (one byte is consumed per error;
/// `String::from_utf8_lossy` would replace a whole invalid sequence once).
fn go_runes_lossy(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        out.push_str(chunk.valid());
        for _ in chunk.invalid() {
            out.push(char::REPLACEMENT_CHARACTER);
        }
    }
    out
}

/// Whether key `a` sorts before key `b`.
pub(crate) fn key_list_less(a: &SortKey, b: &SortKey) -> bool {
    // (Looking through non-null interfaces and pointers happened when the `SortKey`s were
    // built.)
    let ak = a.kind();
    let bk = b.kind();
    if let (Some(af), Some(bf)) = (key_float(a), key_float(b)) {
        if af != bf {
            return af < bf;
        }
        if ak != bk {
            return ak < bk;
        }
        return num_less(a, b);
    }
    let (SortKey::String(a), SortKey::String(b)) = (a, b) else {
        return ak < bk;
    };
    let ar: Vec<char> = a.chars().collect();
    let br: Vec<char> = b.chars().collect();
    let mut digits = false;
    let mut i = 0;
    while i < ar.len() && i < br.len() {
        if ar[i] == br[i] {
            digits = is_digit(ar[i]);
            i += 1;
            continue;
        }
        let al = is_letter(ar[i]);
        let bl = is_letter(br[i]);
        if al && bl {
            return ar[i] < br[i];
        }
        if al || bl {
            if digits {
                return al;
            } else {
                return bl;
            }
        }
        let mut an: i64 = 0;
        let mut bn: i64 = 0;
        if ar[i] == '0' || br[i] == '0' {
            let mut j = i;
            while j > 0 && is_digit(ar[j - 1]) {
                j -= 1;
                if ar[j] != '0' {
                    an = 1;
                    bn = 1;
                    break;
                }
            }
        }
        // The digit value is taken with wrapping `i64` arithmetic (any Nd character counts).
        let mut ai = i;
        while ai < ar.len() && is_digit(ar[ai]) {
            an = an
                .wrapping_mul(10)
                .wrapping_add(i64::from(ar[ai] as u32) - i64::from(b'0'));
            ai += 1;
        }
        let mut bi = i;
        while bi < br.len() && is_digit(br[bi]) {
            bn = bn
                .wrapping_mul(10)
                .wrapping_add(i64::from(br[bi] as u32) - i64::from(b'0'));
            bi += 1;
        }
        if an != bn {
            return an < bn;
        }
        if ai != bi {
            return ai < bi;
        }
        return ar[i] < br[i];
    }
    ar.len() < br.len()
}

/// The numeric value of a number/bool key.
pub(crate) fn key_float(v: &SortKey) -> Option<f64> {
    match v {
        SortKey::Int(_, i) => Some(*i as f64),
        SortKey::Float(_, f) => Some(*f),
        SortKey::Uint(_, u) => Some(*u as f64),
        SortKey::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        SortKey::String(_) | SortKey::Other(_) => None,
    }
}

/// `a < b` for two numeric keys of the same kind.
pub(crate) fn num_less(a: &SortKey, b: &SortKey) -> bool {
    match (a, b) {
        (SortKey::Int(_, a), SortKey::Int(_, b)) => a < b,
        (SortKey::Float(_, a), SortKey::Float(_, b)) => a < b,
        (SortKey::Uint(_, a), SortKey::Uint(_, b)) => a < b,
        (SortKey::Bool(a), SortKey::Bool(b)) => !a && *b,
        _ => panic!("not a number"),
    }
}

/// Whether `r` is a letter (category L).
fn is_letter(r: char) -> bool {
    in_ranges(LETTER_RANGES, r)
}

/// Whether `r` is a decimal digit (category Nd).
fn is_digit(r: char) -> bool {
    in_ranges(DIGIT_RANGES, r)
}

/// Whether `r` lies in one of the inclusive `[lo, hi]` pairs of the sorted table `pairs`.
fn in_ranges(pairs: &[u32], r: char) -> bool {
    let r = r as u32;
    let n = pairs.len() / 2;
    // The first pair whose `hi` is >= r.
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if pairs[2 * mid + 1] < r {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo < n && pairs[2 * lo] <= r
}

/// Sorts `items` by [`key_list_less`] on `key(item)` with yaml.v3's exact sort algorithm
/// (pdqsort), so orders of keys that [`key_list_less`] does not totally order are the ones
/// yaml.v3 produces (and never trip Rust's total-order checks).
pub(crate) fn sort_keys<T>(items: &mut [T], key: impl Fn(&T) -> &SortKey) {
    let n = items.len();
    if n <= 1 {
        return;
    }
    let limit = bits_len(n);
    let mut data = KeyList { items, key };
    pdqsort(&mut data, 0, n, limit);
}

// ---------------------------------------------------------------------------------------------
// Pattern-defeating quicksort over Less/Swap, function by function as yaml.v3's sort.
// ---------------------------------------------------------------------------------------------

/// The sortable view of a key list: `Less` is [`key_list_less`], `Swap` swaps items.
struct KeyList<'a, T, F> {
    items: &'a mut [T],
    key: F,
}

impl<T, F: Fn(&T) -> &SortKey> KeyList<'_, T, F> {
    fn less(&self, i: usize, j: usize) -> bool {
        key_list_less((self.key)(&self.items[i]), (self.key)(&self.items[j]))
    }

    fn swap(&mut self, i: usize, j: usize) {
        self.items.swap(i, j);
    }
}

/// The number of bits needed to represent `n`.
fn bits_len(n: usize) -> usize {
    (usize::BITS - n.leading_zeros()) as usize
}

/// How sorted the input looks (the hint `choose_pivot` returns).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortedHint {
    Unknown,
    Increasing,
    Decreasing,
}

/// A xorshift pseudo-random generator
/// (<https://www.jstatsoft.org/article/view/v008i14/xorshift.pdf>).
struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// The smallest power of two greater than `length`.
fn next_power_of_two(length: usize) -> u64 {
    1u64 << bits_len(length)
}

/// Sorts `data[a:b]` using insertion sort.
fn insertion_sort<T, F: Fn(&T) -> &SortKey>(data: &mut KeyList<'_, T, F>, a: usize, b: usize) {
    for i in a + 1..b {
        let mut j = i;
        while j > a && data.less(j, j - 1) {
            data.swap(j, j - 1);
            j -= 1;
        }
    }
}

/// Implements the heap property on `data[lo:hi]`; `first` is an offset
/// into the array where the root of the heap lies.
fn sift_down<T, F: Fn(&T) -> &SortKey>(
    data: &mut KeyList<'_, T, F>,
    lo: usize,
    hi: usize,
    first: usize,
) {
    let mut root = lo;
    loop {
        let mut child = 2 * root + 1;
        if child >= hi {
            break;
        }
        if child + 1 < hi && data.less(first + child, first + child + 1) {
            child += 1;
        }
        if !data.less(first + root, first + child) {
            return;
        }
        data.swap(first + root, first + child);
        root = child;
    }
}

/// Heap sort of `data[a:b]`.
fn heap_sort<T, F: Fn(&T) -> &SortKey>(data: &mut KeyList<'_, T, F>, a: usize, b: usize) {
    let first = a;
    let lo = 0;
    let hi = b - a;

    // Build heap with greatest element at top.
    for i in (0..=(hi - 1) / 2).rev() {
        sift_down(data, i, hi, first);
    }

    // Pop elements, largest first, into end of data.
    for i in (0..hi).rev() {
        data.swap(first, first + i);
        sift_down(data, lo, i, first);
    }
}

/// Sorts `data[a:b]`; `limit` is the number of allowed bad (very
/// unbalanced) pivots before falling back to heapsort.
fn pdqsort<T, F: Fn(&T) -> &SortKey>(
    data: &mut KeyList<'_, T, F>,
    mut a: usize,
    mut b: usize,
    mut limit: usize,
) {
    const MAX_INSERTION: usize = 12;

    let mut was_balanced = true; // whether the last partitioning was reasonably balanced
    let mut was_partitioned = true; // whether the slice was already partitioned

    loop {
        let length = b - a;

        if length <= MAX_INSERTION {
            insertion_sort(data, a, b);
            return;
        }

        // Fall back to heapsort if too many bad choices were made.
        if limit == 0 {
            heap_sort(data, a, b);
            return;
        }

        // If the last partitioning was imbalanced, we need to breaking patterns.
        if !was_balanced {
            break_patterns(data, a, b);
            limit -= 1;
        }

        let (mut pivot, mut hint) = choose_pivot(data, a, b);
        if hint == SortedHint::Decreasing {
            reverse_range(data, a, b);
            // The chosen pivot was pivot-a elements after the start of the array.
            // After reversing it is pivot-a elements before the end of the array.
            // The idea came from Rust's implementation.
            pivot = (b - 1) - (pivot - a);
            hint = SortedHint::Increasing;
        }

        // The slice is likely already sorted.
        if was_balanced
            && was_partitioned
            && hint == SortedHint::Increasing
            && partial_insertion_sort(data, a, b)
        {
            return;
        }

        // Probably the slice contains many duplicate elements, partition the slice into
        // elements equal to and elements greater than the pivot.
        if a > 0 && !data.less(a - 1, pivot) {
            let mid = partition_equal(data, a, b, pivot);
            a = mid;
            continue;
        }

        let (mid, already_partitioned) = partition(data, a, b, pivot);
        was_partitioned = already_partitioned;

        let (left_len, right_len) = (mid - a, b - mid);
        let balance_threshold = length / 8;
        if left_len < right_len {
            was_balanced = left_len >= balance_threshold;
            pdqsort(data, a, mid, limit);
            a = mid + 1;
        } else {
            was_balanced = right_len >= balance_threshold;
            pdqsort(data, mid + 1, b, limit);
            b = mid;
        }
    }
}

/// Partitions `data[a:b]` into elements lower than `data[pivot]`,
/// followed by elements greater than or equal to `data[pivot]`.
fn partition<T, F: Fn(&T) -> &SortKey>(
    data: &mut KeyList<'_, T, F>,
    a: usize,
    b: usize,
    pivot: usize,
) -> (usize, bool) {
    data.swap(a, pivot);
    let (mut i, mut j) = (a + 1, b - 1); // i and j are inclusive of the elements remaining to be partitioned

    while i <= j && data.less(i, a) {
        i += 1;
    }
    while i <= j && !data.less(j, a) {
        j -= 1;
    }
    if i > j {
        data.swap(j, a);
        return (j, true);
    }
    data.swap(i, j);
    i += 1;
    j -= 1;

    loop {
        while i <= j && data.less(i, a) {
            i += 1;
        }
        while i <= j && !data.less(j, a) {
            j -= 1;
        }
        if i > j {
            break;
        }
        data.swap(i, j);
        i += 1;
        j -= 1;
    }
    data.swap(j, a);
    (j, false)
}

/// Partitions `data[a:b]` into elements equal to `data[pivot]`
/// followed by elements greater than `data[pivot]`; assumes no element is smaller.
fn partition_equal<T, F: Fn(&T) -> &SortKey>(
    data: &mut KeyList<'_, T, F>,
    a: usize,
    b: usize,
    pivot: usize,
) -> usize {
    data.swap(a, pivot);
    let (mut i, mut j) = (a + 1, b - 1); // i and j are inclusive of the elements remaining to be partitioned

    loop {
        while i <= j && !data.less(a, i) {
            i += 1;
        }
        while i <= j && data.less(a, j) {
            j -= 1;
        }
        if i > j {
            break;
        }
        data.swap(i, j);
        i += 1;
        j -= 1;
    }
    i
}

/// Partially sorts a slice, returns true if the slice is
/// sorted at the end.
fn partial_insertion_sort<T, F: Fn(&T) -> &SortKey>(
    data: &mut KeyList<'_, T, F>,
    a: usize,
    b: usize,
) -> bool {
    const MAX_STEPS: usize = 5; // maximum number of adjacent out-of-order pairs that will get shifted
    const SHORTEST_SHIFTING: usize = 50; // don't shift any elements on short arrays
    let mut i = a + 1;
    for _ in 0..MAX_STEPS {
        while i < b && !data.less(i, i - 1) {
            i += 1;
        }

        if i == b {
            return true;
        }

        if b - a < SHORTEST_SHIFTING {
            return false;
        }

        data.swap(i, i - 1);

        // Shift the smaller one to the left.
        if i - a >= 2 {
            // The loop runs down to index 1, not a+1.
            let mut j = i - 1;
            while j >= 1 {
                if !data.less(j, j - 1) {
                    break;
                }
                data.swap(j, j - 1);
                j -= 1;
            }
        }
        // Shift the greater one to the right.
        if b - i >= 2 {
            let mut j = i + 1;
            while j < b {
                if !data.less(j, j - 1) {
                    break;
                }
                data.swap(j, j - 1);
                j += 1;
            }
        }
    }
    false
}

/// Scatters some elements around in an attempt to break some
/// patterns that might cause imbalanced partitions in quicksort.
fn break_patterns<T, F: Fn(&T) -> &SortKey>(data: &mut KeyList<'_, T, F>, a: usize, b: usize) {
    let length = b - a;
    if length >= 8 {
        let mut random = Xorshift(length as u64);
        let modulus = next_power_of_two(length);

        let start = a + (length / 4) * 2 - 1;
        for idx in start..=a + (length / 4) * 2 + 1 {
            let mut other = (random.next() & (modulus - 1)) as usize;
            if other >= length {
                other -= length;
            }
            data.swap(idx, a + other);
        }
    }
}

/// Chooses a pivot in `data[a:b]`: [0,8): chooses a static pivot;
/// [8,shortestNinther): uses the simple median-of-three method; [shortestNinther,∞): uses
/// the Tukey ninther method.
fn choose_pivot<T, F: Fn(&T) -> &SortKey>(
    data: &mut KeyList<'_, T, F>,
    a: usize,
    b: usize,
) -> (usize, SortedHint) {
    const SHORTEST_NINTHER: usize = 50;
    const MAX_SWAPS: usize = 4 * 3;

    let l = b - a;

    let mut swaps = 0;
    let mut i = a + l / 4;
    let mut j = a + l / 4 * 2;
    let mut k = a + l / 4 * 3;

    if l >= 8 {
        if l >= SHORTEST_NINTHER {
            // Tukey ninther method, the idea came from Rust's implementation.
            i = median_adjacent(data, i, &mut swaps);
            j = median_adjacent(data, j, &mut swaps);
            k = median_adjacent(data, k, &mut swaps);
        }
        // Find the median among i, j, k and stores it into j.
        j = median(data, i, j, k, &mut swaps);
    }

    match swaps {
        0 => (j, SortedHint::Increasing),
        MAX_SWAPS => (j, SortedHint::Decreasing),
        _ => (j, SortedHint::Unknown),
    }
}

/// Returns `(a, b)` such that `data[a] <= data[b]`.
fn order2<T, F: Fn(&T) -> &SortKey>(
    data: &KeyList<'_, T, F>,
    a: usize,
    b: usize,
    swaps: &mut usize,
) -> (usize, usize) {
    if data.less(b, a) {
        *swaps += 1;
        return (b, a);
    }
    (a, b)
}

/// Returns `x` where `data[x]` is the median of `a`, `b`, `c`.
fn median<T, F: Fn(&T) -> &SortKey>(
    data: &KeyList<'_, T, F>,
    a: usize,
    b: usize,
    c: usize,
    swaps: &mut usize,
) -> usize {
    let (a, b) = order2(data, a, b, swaps);
    let (b, _c) = order2(data, b, c, swaps);
    let (_a, b) = order2(data, a, b, swaps);
    b
}

/// Finds the median of `data[a - 1], data[a], data[a + 1]`.
fn median_adjacent<T, F: Fn(&T) -> &SortKey>(
    data: &KeyList<'_, T, F>,
    a: usize,
    swaps: &mut usize,
) -> usize {
    median(data, a - 1, a, a + 1, swaps)
}

/// Reverses `data[a:b]`.
fn reverse_range<T, F: Fn(&T) -> &SortKey>(data: &mut KeyList<'_, T, F>, a: usize, b: usize) {
    let mut i = a;
    let mut j = b - 1;
    while i < j {
        data.swap(i, j);
        i += 1;
        j -= 1;
    }
}

// ---------------------------------------------------------------------------------------------
// Unicode tables for `is_letter` / `is_digit` (the Unicode 17.0.0 tables yaml.v3's sort
// consults), as maximal runs of code points.
// ---------------------------------------------------------------------------------------------

/// Letters (category L, Unicode 17.0.0): inclusive `[lo, hi]` pairs.
const LETTER_RANGES: &[u32] = &[
    0x41, 0x5a, 0x61, 0x7a, 0xaa, 0xaa, 0xb5, 0xb5, 0xba, 0xba, 0xc0, 0xd6, 0xd8, 0xf6, 0xf8,
    0x2c1, 0x2c6, 0x2d1, 0x2e0, 0x2e4, 0x2ec, 0x2ec, 0x2ee, 0x2ee, 0x370, 0x374, 0x376, 0x377,
    0x37a, 0x37d, 0x37f, 0x37f, 0x386, 0x386, 0x388, 0x38a, 0x38c, 0x38c, 0x38e, 0x3a1, 0x3a3,
    0x3f5, 0x3f7, 0x481, 0x48a, 0x52f, 0x531, 0x556, 0x559, 0x559, 0x560, 0x588, 0x5d0, 0x5ea,
    0x5ef, 0x5f2, 0x620, 0x64a, 0x66e, 0x66f, 0x671, 0x6d3, 0x6d5, 0x6d5, 0x6e5, 0x6e6, 0x6ee,
    0x6ef, 0x6fa, 0x6fc, 0x6ff, 0x6ff, 0x710, 0x710, 0x712, 0x72f, 0x74d, 0x7a5, 0x7b1, 0x7b1,
    0x7ca, 0x7ea, 0x7f4, 0x7f5, 0x7fa, 0x7fa, 0x800, 0x815, 0x81a, 0x81a, 0x824, 0x824, 0x828,
    0x828, 0x840, 0x858, 0x860, 0x86a, 0x870, 0x887, 0x889, 0x88f, 0x8a0, 0x8c9, 0x904, 0x939,
    0x93d, 0x93d, 0x950, 0x950, 0x958, 0x961, 0x971, 0x980, 0x985, 0x98c, 0x98f, 0x990, 0x993,
    0x9a8, 0x9aa, 0x9b0, 0x9b2, 0x9b2, 0x9b6, 0x9b9, 0x9bd, 0x9bd, 0x9ce, 0x9ce, 0x9dc, 0x9dd,
    0x9df, 0x9e1, 0x9f0, 0x9f1, 0x9fc, 0x9fc, 0xa05, 0xa0a, 0xa0f, 0xa10, 0xa13, 0xa28, 0xa2a,
    0xa30, 0xa32, 0xa33, 0xa35, 0xa36, 0xa38, 0xa39, 0xa59, 0xa5c, 0xa5e, 0xa5e, 0xa72, 0xa74,
    0xa85, 0xa8d, 0xa8f, 0xa91, 0xa93, 0xaa8, 0xaaa, 0xab0, 0xab2, 0xab3, 0xab5, 0xab9, 0xabd,
    0xabd, 0xad0, 0xad0, 0xae0, 0xae1, 0xaf9, 0xaf9, 0xb05, 0xb0c, 0xb0f, 0xb10, 0xb13, 0xb28,
    0xb2a, 0xb30, 0xb32, 0xb33, 0xb35, 0xb39, 0xb3d, 0xb3d, 0xb5c, 0xb5d, 0xb5f, 0xb61, 0xb71,
    0xb71, 0xb83, 0xb83, 0xb85, 0xb8a, 0xb8e, 0xb90, 0xb92, 0xb95, 0xb99, 0xb9a, 0xb9c, 0xb9c,
    0xb9e, 0xb9f, 0xba3, 0xba4, 0xba8, 0xbaa, 0xbae, 0xbb9, 0xbd0, 0xbd0, 0xc05, 0xc0c, 0xc0e,
    0xc10, 0xc12, 0xc28, 0xc2a, 0xc39, 0xc3d, 0xc3d, 0xc58, 0xc5a, 0xc5c, 0xc5d, 0xc60, 0xc61,
    0xc80, 0xc80, 0xc85, 0xc8c, 0xc8e, 0xc90, 0xc92, 0xca8, 0xcaa, 0xcb3, 0xcb5, 0xcb9, 0xcbd,
    0xcbd, 0xcdc, 0xcde, 0xce0, 0xce1, 0xcf1, 0xcf2, 0xd04, 0xd0c, 0xd0e, 0xd10, 0xd12, 0xd3a,
    0xd3d, 0xd3d, 0xd4e, 0xd4e, 0xd54, 0xd56, 0xd5f, 0xd61, 0xd7a, 0xd7f, 0xd85, 0xd96, 0xd9a,
    0xdb1, 0xdb3, 0xdbb, 0xdbd, 0xdbd, 0xdc0, 0xdc6, 0xe01, 0xe30, 0xe32, 0xe33, 0xe40, 0xe46,
    0xe81, 0xe82, 0xe84, 0xe84, 0xe86, 0xe8a, 0xe8c, 0xea3, 0xea5, 0xea5, 0xea7, 0xeb0, 0xeb2,
    0xeb3, 0xebd, 0xebd, 0xec0, 0xec4, 0xec6, 0xec6, 0xedc, 0xedf, 0xf00, 0xf00, 0xf40, 0xf47,
    0xf49, 0xf6c, 0xf88, 0xf8c, 0x1000, 0x102a, 0x103f, 0x103f, 0x1050, 0x1055, 0x105a, 0x105d,
    0x1061, 0x1061, 0x1065, 0x1066, 0x106e, 0x1070, 0x1075, 0x1081, 0x108e, 0x108e, 0x10a0, 0x10c5,
    0x10c7, 0x10c7, 0x10cd, 0x10cd, 0x10d0, 0x10fa, 0x10fc, 0x1248, 0x124a, 0x124d, 0x1250, 0x1256,
    0x1258, 0x1258, 0x125a, 0x125d, 0x1260, 0x1288, 0x128a, 0x128d, 0x1290, 0x12b0, 0x12b2, 0x12b5,
    0x12b8, 0x12be, 0x12c0, 0x12c0, 0x12c2, 0x12c5, 0x12c8, 0x12d6, 0x12d8, 0x1310, 0x1312, 0x1315,
    0x1318, 0x135a, 0x1380, 0x138f, 0x13a0, 0x13f5, 0x13f8, 0x13fd, 0x1401, 0x166c, 0x166f, 0x167f,
    0x1681, 0x169a, 0x16a0, 0x16ea, 0x16f1, 0x16f8, 0x1700, 0x1711, 0x171f, 0x1731, 0x1740, 0x1751,
    0x1760, 0x176c, 0x176e, 0x1770, 0x1780, 0x17b3, 0x17d7, 0x17d7, 0x17dc, 0x17dc, 0x1820, 0x1878,
    0x1880, 0x1884, 0x1887, 0x18a8, 0x18aa, 0x18aa, 0x18b0, 0x18f5, 0x1900, 0x191e, 0x1950, 0x196d,
    0x1970, 0x1974, 0x1980, 0x19ab, 0x19b0, 0x19c9, 0x1a00, 0x1a16, 0x1a20, 0x1a54, 0x1aa7, 0x1aa7,
    0x1b05, 0x1b33, 0x1b45, 0x1b4c, 0x1b83, 0x1ba0, 0x1bae, 0x1baf, 0x1bba, 0x1be5, 0x1c00, 0x1c23,
    0x1c4d, 0x1c4f, 0x1c5a, 0x1c7d, 0x1c80, 0x1c8a, 0x1c90, 0x1cba, 0x1cbd, 0x1cbf, 0x1ce9, 0x1cec,
    0x1cee, 0x1cf3, 0x1cf5, 0x1cf6, 0x1cfa, 0x1cfa, 0x1d00, 0x1dbf, 0x1e00, 0x1f15, 0x1f18, 0x1f1d,
    0x1f20, 0x1f45, 0x1f48, 0x1f4d, 0x1f50, 0x1f57, 0x1f59, 0x1f59, 0x1f5b, 0x1f5b, 0x1f5d, 0x1f5d,
    0x1f5f, 0x1f7d, 0x1f80, 0x1fb4, 0x1fb6, 0x1fbc, 0x1fbe, 0x1fbe, 0x1fc2, 0x1fc4, 0x1fc6, 0x1fcc,
    0x1fd0, 0x1fd3, 0x1fd6, 0x1fdb, 0x1fe0, 0x1fec, 0x1ff2, 0x1ff4, 0x1ff6, 0x1ffc, 0x2071, 0x2071,
    0x207f, 0x207f, 0x2090, 0x209c, 0x2102, 0x2102, 0x2107, 0x2107, 0x210a, 0x2113, 0x2115, 0x2115,
    0x2119, 0x211d, 0x2124, 0x2124, 0x2126, 0x2126, 0x2128, 0x2128, 0x212a, 0x212d, 0x212f, 0x2139,
    0x213c, 0x213f, 0x2145, 0x2149, 0x214e, 0x214e, 0x2183, 0x2184, 0x2c00, 0x2ce4, 0x2ceb, 0x2cee,
    0x2cf2, 0x2cf3, 0x2d00, 0x2d25, 0x2d27, 0x2d27, 0x2d2d, 0x2d2d, 0x2d30, 0x2d67, 0x2d6f, 0x2d6f,
    0x2d80, 0x2d96, 0x2da0, 0x2da6, 0x2da8, 0x2dae, 0x2db0, 0x2db6, 0x2db8, 0x2dbe, 0x2dc0, 0x2dc6,
    0x2dc8, 0x2dce, 0x2dd0, 0x2dd6, 0x2dd8, 0x2dde, 0x2e2f, 0x2e2f, 0x3005, 0x3006, 0x3031, 0x3035,
    0x303b, 0x303c, 0x3041, 0x3096, 0x309d, 0x309f, 0x30a1, 0x30fa, 0x30fc, 0x30ff, 0x3105, 0x312f,
    0x3131, 0x318e, 0x31a0, 0x31bf, 0x31f0, 0x31ff, 0x3400, 0x4dbf, 0x4e00, 0xa48c, 0xa4d0, 0xa4fd,
    0xa500, 0xa60c, 0xa610, 0xa61f, 0xa62a, 0xa62b, 0xa640, 0xa66e, 0xa67f, 0xa69d, 0xa6a0, 0xa6e5,
    0xa717, 0xa71f, 0xa722, 0xa788, 0xa78b, 0xa7dc, 0xa7f1, 0xa801, 0xa803, 0xa805, 0xa807, 0xa80a,
    0xa80c, 0xa822, 0xa840, 0xa873, 0xa882, 0xa8b3, 0xa8f2, 0xa8f7, 0xa8fb, 0xa8fb, 0xa8fd, 0xa8fe,
    0xa90a, 0xa925, 0xa930, 0xa946, 0xa960, 0xa97c, 0xa984, 0xa9b2, 0xa9cf, 0xa9cf, 0xa9e0, 0xa9e4,
    0xa9e6, 0xa9ef, 0xa9fa, 0xa9fe, 0xaa00, 0xaa28, 0xaa40, 0xaa42, 0xaa44, 0xaa4b, 0xaa60, 0xaa76,
    0xaa7a, 0xaa7a, 0xaa7e, 0xaaaf, 0xaab1, 0xaab1, 0xaab5, 0xaab6, 0xaab9, 0xaabd, 0xaac0, 0xaac0,
    0xaac2, 0xaac2, 0xaadb, 0xaadd, 0xaae0, 0xaaea, 0xaaf2, 0xaaf4, 0xab01, 0xab06, 0xab09, 0xab0e,
    0xab11, 0xab16, 0xab20, 0xab26, 0xab28, 0xab2e, 0xab30, 0xab5a, 0xab5c, 0xab69, 0xab70, 0xabe2,
    0xac00, 0xd7a3, 0xd7b0, 0xd7c6, 0xd7cb, 0xd7fb, 0xf900, 0xfa6d, 0xfa70, 0xfad9, 0xfb00, 0xfb06,
    0xfb13, 0xfb17, 0xfb1d, 0xfb1d, 0xfb1f, 0xfb28, 0xfb2a, 0xfb36, 0xfb38, 0xfb3c, 0xfb3e, 0xfb3e,
    0xfb40, 0xfb41, 0xfb43, 0xfb44, 0xfb46, 0xfbb1, 0xfbd3, 0xfd3d, 0xfd50, 0xfd8f, 0xfd92, 0xfdc7,
    0xfdf0, 0xfdfb, 0xfe70, 0xfe74, 0xfe76, 0xfefc, 0xff21, 0xff3a, 0xff41, 0xff5a, 0xff66, 0xffbe,
    0xffc2, 0xffc7, 0xffca, 0xffcf, 0xffd2, 0xffd7, 0xffda, 0xffdc, 0x10000, 0x1000b, 0x1000d,
    0x10026, 0x10028, 0x1003a, 0x1003c, 0x1003d, 0x1003f, 0x1004d, 0x10050, 0x1005d, 0x10080,
    0x100fa, 0x10280, 0x1029c, 0x102a0, 0x102d0, 0x10300, 0x1031f, 0x1032d, 0x10340, 0x10342,
    0x10349, 0x10350, 0x10375, 0x10380, 0x1039d, 0x103a0, 0x103c3, 0x103c8, 0x103cf, 0x10400,
    0x1049d, 0x104b0, 0x104d3, 0x104d8, 0x104fb, 0x10500, 0x10527, 0x10530, 0x10563, 0x10570,
    0x1057a, 0x1057c, 0x1058a, 0x1058c, 0x10592, 0x10594, 0x10595, 0x10597, 0x105a1, 0x105a3,
    0x105b1, 0x105b3, 0x105b9, 0x105bb, 0x105bc, 0x105c0, 0x105f3, 0x10600, 0x10736, 0x10740,
    0x10755, 0x10760, 0x10767, 0x10780, 0x10785, 0x10787, 0x107b0, 0x107b2, 0x107ba, 0x10800,
    0x10805, 0x10808, 0x10808, 0x1080a, 0x10835, 0x10837, 0x10838, 0x1083c, 0x1083c, 0x1083f,
    0x10855, 0x10860, 0x10876, 0x10880, 0x1089e, 0x108e0, 0x108f2, 0x108f4, 0x108f5, 0x10900,
    0x10915, 0x10920, 0x10939, 0x10940, 0x10959, 0x10980, 0x109b7, 0x109be, 0x109bf, 0x10a00,
    0x10a00, 0x10a10, 0x10a13, 0x10a15, 0x10a17, 0x10a19, 0x10a35, 0x10a60, 0x10a7c, 0x10a80,
    0x10a9c, 0x10ac0, 0x10ac7, 0x10ac9, 0x10ae4, 0x10b00, 0x10b35, 0x10b40, 0x10b55, 0x10b60,
    0x10b72, 0x10b80, 0x10b91, 0x10c00, 0x10c48, 0x10c80, 0x10cb2, 0x10cc0, 0x10cf2, 0x10d00,
    0x10d23, 0x10d4a, 0x10d65, 0x10d6f, 0x10d85, 0x10e80, 0x10ea9, 0x10eb0, 0x10eb1, 0x10ec2,
    0x10ec7, 0x10f00, 0x10f1c, 0x10f27, 0x10f27, 0x10f30, 0x10f45, 0x10f70, 0x10f81, 0x10fb0,
    0x10fc4, 0x10fe0, 0x10ff6, 0x11003, 0x11037, 0x11071, 0x11072, 0x11075, 0x11075, 0x11083,
    0x110af, 0x110d0, 0x110e8, 0x11103, 0x11126, 0x11144, 0x11144, 0x11147, 0x11147, 0x11150,
    0x11172, 0x11176, 0x11176, 0x11183, 0x111b2, 0x111c1, 0x111c4, 0x111da, 0x111da, 0x111dc,
    0x111dc, 0x11200, 0x11211, 0x11213, 0x1122b, 0x1123f, 0x11240, 0x11280, 0x11286, 0x11288,
    0x11288, 0x1128a, 0x1128d, 0x1128f, 0x1129d, 0x1129f, 0x112a8, 0x112b0, 0x112de, 0x11305,
    0x1130c, 0x1130f, 0x11310, 0x11313, 0x11328, 0x1132a, 0x11330, 0x11332, 0x11333, 0x11335,
    0x11339, 0x1133d, 0x1133d, 0x11350, 0x11350, 0x1135d, 0x11361, 0x11380, 0x11389, 0x1138b,
    0x1138b, 0x1138e, 0x1138e, 0x11390, 0x113b5, 0x113b7, 0x113b7, 0x113d1, 0x113d1, 0x113d3,
    0x113d3, 0x11400, 0x11434, 0x11447, 0x1144a, 0x1145f, 0x11461, 0x11480, 0x114af, 0x114c4,
    0x114c5, 0x114c7, 0x114c7, 0x11580, 0x115ae, 0x115d8, 0x115db, 0x11600, 0x1162f, 0x11644,
    0x11644, 0x11680, 0x116aa, 0x116b8, 0x116b8, 0x11700, 0x1171a, 0x11740, 0x11746, 0x11800,
    0x1182b, 0x118a0, 0x118df, 0x118ff, 0x11906, 0x11909, 0x11909, 0x1190c, 0x11913, 0x11915,
    0x11916, 0x11918, 0x1192f, 0x1193f, 0x1193f, 0x11941, 0x11941, 0x119a0, 0x119a7, 0x119aa,
    0x119d0, 0x119e1, 0x119e1, 0x119e3, 0x119e3, 0x11a00, 0x11a00, 0x11a0b, 0x11a32, 0x11a3a,
    0x11a3a, 0x11a50, 0x11a50, 0x11a5c, 0x11a89, 0x11a9d, 0x11a9d, 0x11ab0, 0x11af8, 0x11bc0,
    0x11be0, 0x11c00, 0x11c08, 0x11c0a, 0x11c2e, 0x11c40, 0x11c40, 0x11c72, 0x11c8f, 0x11d00,
    0x11d06, 0x11d08, 0x11d09, 0x11d0b, 0x11d30, 0x11d46, 0x11d46, 0x11d60, 0x11d65, 0x11d67,
    0x11d68, 0x11d6a, 0x11d89, 0x11d98, 0x11d98, 0x11db0, 0x11ddb, 0x11ee0, 0x11ef2, 0x11f02,
    0x11f02, 0x11f04, 0x11f10, 0x11f12, 0x11f33, 0x11fb0, 0x11fb0, 0x12000, 0x12399, 0x12480,
    0x12543, 0x12f90, 0x12ff0, 0x13000, 0x1342f, 0x13441, 0x13446, 0x13460, 0x143fa, 0x14400,
    0x14646, 0x16100, 0x1611d, 0x16800, 0x16a38, 0x16a40, 0x16a5e, 0x16a70, 0x16abe, 0x16ad0,
    0x16aed, 0x16b00, 0x16b2f, 0x16b40, 0x16b43, 0x16b63, 0x16b77, 0x16b7d, 0x16b8f, 0x16d40,
    0x16d6c, 0x16e40, 0x16e7f, 0x16ea0, 0x16eb8, 0x16ebb, 0x16ed3, 0x16f00, 0x16f4a, 0x16f50,
    0x16f50, 0x16f93, 0x16f9f, 0x16fe0, 0x16fe1, 0x16fe3, 0x16fe3, 0x16ff2, 0x16ff3, 0x17000,
    0x18cd5, 0x18cff, 0x18d1e, 0x18d80, 0x18df2, 0x1aff0, 0x1aff3, 0x1aff5, 0x1affb, 0x1affd,
    0x1affe, 0x1b000, 0x1b122, 0x1b132, 0x1b132, 0x1b150, 0x1b152, 0x1b155, 0x1b155, 0x1b164,
    0x1b167, 0x1b170, 0x1b2fb, 0x1bc00, 0x1bc6a, 0x1bc70, 0x1bc7c, 0x1bc80, 0x1bc88, 0x1bc90,
    0x1bc99, 0x1d400, 0x1d454, 0x1d456, 0x1d49c, 0x1d49e, 0x1d49f, 0x1d4a2, 0x1d4a2, 0x1d4a5,
    0x1d4a6, 0x1d4a9, 0x1d4ac, 0x1d4ae, 0x1d4b9, 0x1d4bb, 0x1d4bb, 0x1d4bd, 0x1d4c3, 0x1d4c5,
    0x1d505, 0x1d507, 0x1d50a, 0x1d50d, 0x1d514, 0x1d516, 0x1d51c, 0x1d51e, 0x1d539, 0x1d53b,
    0x1d53e, 0x1d540, 0x1d544, 0x1d546, 0x1d546, 0x1d54a, 0x1d550, 0x1d552, 0x1d6a5, 0x1d6a8,
    0x1d6c0, 0x1d6c2, 0x1d6da, 0x1d6dc, 0x1d6fa, 0x1d6fc, 0x1d714, 0x1d716, 0x1d734, 0x1d736,
    0x1d74e, 0x1d750, 0x1d76e, 0x1d770, 0x1d788, 0x1d78a, 0x1d7a8, 0x1d7aa, 0x1d7c2, 0x1d7c4,
    0x1d7cb, 0x1df00, 0x1df1e, 0x1df25, 0x1df2a, 0x1e030, 0x1e06d, 0x1e100, 0x1e12c, 0x1e137,
    0x1e13d, 0x1e14e, 0x1e14e, 0x1e290, 0x1e2ad, 0x1e2c0, 0x1e2eb, 0x1e4d0, 0x1e4eb, 0x1e5d0,
    0x1e5ed, 0x1e5f0, 0x1e5f0, 0x1e6c0, 0x1e6de, 0x1e6e0, 0x1e6e2, 0x1e6e4, 0x1e6e5, 0x1e6e7,
    0x1e6ed, 0x1e6f0, 0x1e6f4, 0x1e6fe, 0x1e6ff, 0x1e7e0, 0x1e7e6, 0x1e7e8, 0x1e7eb, 0x1e7ed,
    0x1e7ee, 0x1e7f0, 0x1e7fe, 0x1e800, 0x1e8c4, 0x1e900, 0x1e943, 0x1e94b, 0x1e94b, 0x1ee00,
    0x1ee03, 0x1ee05, 0x1ee1f, 0x1ee21, 0x1ee22, 0x1ee24, 0x1ee24, 0x1ee27, 0x1ee27, 0x1ee29,
    0x1ee32, 0x1ee34, 0x1ee37, 0x1ee39, 0x1ee39, 0x1ee3b, 0x1ee3b, 0x1ee42, 0x1ee42, 0x1ee47,
    0x1ee47, 0x1ee49, 0x1ee49, 0x1ee4b, 0x1ee4b, 0x1ee4d, 0x1ee4f, 0x1ee51, 0x1ee52, 0x1ee54,
    0x1ee54, 0x1ee57, 0x1ee57, 0x1ee59, 0x1ee59, 0x1ee5b, 0x1ee5b, 0x1ee5d, 0x1ee5d, 0x1ee5f,
    0x1ee5f, 0x1ee61, 0x1ee62, 0x1ee64, 0x1ee64, 0x1ee67, 0x1ee6a, 0x1ee6c, 0x1ee72, 0x1ee74,
    0x1ee77, 0x1ee79, 0x1ee7c, 0x1ee7e, 0x1ee7e, 0x1ee80, 0x1ee89, 0x1ee8b, 0x1ee9b, 0x1eea1,
    0x1eea3, 0x1eea5, 0x1eea9, 0x1eeab, 0x1eebb, 0x20000, 0x2a6df, 0x2a700, 0x2b81d, 0x2b820,
    0x2cead, 0x2ceb0, 0x2ebe0, 0x2ebf0, 0x2ee5d, 0x2f800, 0x2fa1d, 0x30000, 0x3134a, 0x31350,
    0x33479,
];

/// Decimal digits (category Nd, Unicode 17.0.0): inclusive `[lo, hi]` pairs.
const DIGIT_RANGES: &[u32] = &[
    0x30, 0x39, 0x660, 0x669, 0x6f0, 0x6f9, 0x7c0, 0x7c9, 0x966, 0x96f, 0x9e6, 0x9ef, 0xa66, 0xa6f,
    0xae6, 0xaef, 0xb66, 0xb6f, 0xbe6, 0xbef, 0xc66, 0xc6f, 0xce6, 0xcef, 0xd66, 0xd6f, 0xde6,
    0xdef, 0xe50, 0xe59, 0xed0, 0xed9, 0xf20, 0xf29, 0x1040, 0x1049, 0x1090, 0x1099, 0x17e0,
    0x17e9, 0x1810, 0x1819, 0x1946, 0x194f, 0x19d0, 0x19d9, 0x1a80, 0x1a89, 0x1a90, 0x1a99, 0x1b50,
    0x1b59, 0x1bb0, 0x1bb9, 0x1c40, 0x1c49, 0x1c50, 0x1c59, 0xa620, 0xa629, 0xa8d0, 0xa8d9, 0xa900,
    0xa909, 0xa9d0, 0xa9d9, 0xa9f0, 0xa9f9, 0xaa50, 0xaa59, 0xabf0, 0xabf9, 0xff10, 0xff19,
    0x104a0, 0x104a9, 0x10d30, 0x10d39, 0x10d40, 0x10d49, 0x11066, 0x1106f, 0x110f0, 0x110f9,
    0x11136, 0x1113f, 0x111d0, 0x111d9, 0x112f0, 0x112f9, 0x11450, 0x11459, 0x114d0, 0x114d9,
    0x11650, 0x11659, 0x116c0, 0x116c9, 0x116d0, 0x116e3, 0x11730, 0x11739, 0x118e0, 0x118e9,
    0x11950, 0x11959, 0x11bf0, 0x11bf9, 0x11c50, 0x11c59, 0x11d50, 0x11d59, 0x11da0, 0x11da9,
    0x11de0, 0x11de9, 0x11f50, 0x11f59, 0x16130, 0x16139, 0x16a60, 0x16a69, 0x16ac0, 0x16ac9,
    0x16b50, 0x16b59, 0x16d70, 0x16d79, 0x1ccf0, 0x1ccf9, 0x1d7ce, 0x1d7ff, 0x1e140, 0x1e149,
    0x1e2f0, 0x1e2f9, 0x1e4f0, 0x1e4f9, 0x1e5f1, 0x1e5fa, 0x1e950, 0x1e959, 0x1fbf0, 0x1fbf9,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn int(i: i64) -> SortKey {
        SortKey::Int(ReflectKind::Int, i)
    }

    fn uint(u: u64) -> SortKey {
        SortKey::Uint(ReflectKind::Uint, u)
    }

    fn float(f: f64) -> SortKey {
        SortKey::Float(ReflectKind::Float64, f)
    }

    fn string(s: &str) -> SortKey {
        SortKey::String(s.to_string())
    }

    /// Go's `sort.Sort` of `keys`, returned as the original indices in sorted order.
    fn sorted_indices(keys: &[SortKey]) -> Vec<usize> {
        let mut items: Vec<(usize, &SortKey)> = keys.iter().enumerate().collect();
        sort_keys(&mut items, |item| item.1);
        items.into_iter().map(|(i, _)| i).collect()
    }

    /// FNV-1a-style 64-bit hash of a permutation (as the Go generator computed it).
    fn fnv(indices: &[usize]) -> u64 {
        let mut h: u64 = 14_695_981_039_346_656_037;
        for &i in indices {
            h ^= i as u64;
            h = h.wrapping_mul(1_099_511_628_211);
        }
        h
    }

    /// The Go generator's key list for `seed`: a 64-bit LCG choosing ints, uints, floats,
    /// bools, NaNs, nil interfaces and short strings over `a0b1Z9_.é٣02` (letters, digits incl.
    /// zero runs and a non-ASCII Nd digit), so `Less` is often not a strict weak order.
    fn generated(seed: u64, n: usize) -> Vec<SortKey> {
        let mut x = seed;
        let mut next = || {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            x >> 33
        };
        let alphabet = ['a', '0', 'b', '1', 'Z', '9', '_', '.', 'é', '٣', '0', '2'];
        (0..n)
            .map(|_| match next() % 12 {
                0 => int((next() % 5) as i64 - 2),
                1 => uint(next() % 5),
                2 => float((next() % 5) as f64 / 2.0),
                3 => SortKey::Bool(next() % 2 == 0),
                4 => float(f64::NAN),
                5 => SortKey::Other(ReflectKind::Interface),
                _ => {
                    let l = next() % 7;
                    let s: String = (0..l)
                        .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                        .collect();
                    SortKey::String(s)
                }
            })
            .collect()
    }

    /// encode_test.go `TestSortedOutput`: a `map[interface{}]int` with these keys marshals
    /// them in exactly this order, whatever Go's map iteration order was (here: several
    /// input permutations).
    #[test]
    fn test_sorted_output() {
        let mut order = vec![
            SortKey::Bool(false),
            SortKey::Bool(true),
            int(1),
            uint(1),
            float(1.0),
            float(1.1),
            float(1.2),
            int(2),
            uint(2),
            float(2.0),
            float(2.1),
        ];
        for s in [
            "", ".1", ".2", ".a", "1", "2", "a!10", "a/0001", "a/002", "a/3", "a/10", "a/11",
            "a/0012", "a/100", "a~10", "ab/1", "b/1", "b/01", "b/2", "b/02", "b/3", "b/03", "b1",
            "b01", "b3", "c2.10", "c10.2", "d1", "d7", "d7abc", "d12", "d12a", "e2b", "e4b",
            "e21a",
        ] {
            order.push(string(s));
        }
        let n = order.len();
        let mut perms: Vec<Vec<usize>> = vec![(0..n).collect(), (0..n).rev().collect()];
        for seed in 1..=20u64 {
            // A Fisher-Yates shuffle driven by a small LCG.
            let mut p: Vec<usize> = (0..n).collect();
            let mut x = seed;
            for i in (1..n).rev() {
                x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                p.swap(i, (x >> 33) as usize % (i + 1));
            }
            perms.push(p);
        }
        for p in perms {
            let mut keys: Vec<SortKey> = p.iter().map(|&i| order[i].clone()).collect();
            sort_keys(&mut keys, |k| k);
            assert_eq!(keys, order, "input permutation {p:?}");
        }
    }

    /// Go's `sort.Sort(keyList)` on generated key lists (interface keys, as from a
    /// `map[interface{}]interface{}`), recorded by sorter.go + Go 1.27.2's `sort` as the
    /// permutation of the input indices: lengths around the insertion-sort (12) and ninther
    /// (50) thresholds.
    #[test]
    fn sort_keys_matches_go_permutations() {
        let cases: &[(u64, usize, &[usize])] = &[
            (1, 7, &[1, 6, 4, 0, 3, 5, 2]),
            (2, 12, &[0, 3, 8, 10, 11, 7, 4, 9, 5, 2, 6, 1]),
            (3, 13, &[3, 11, 10, 9, 1, 5, 6, 2, 0, 4, 12, 8, 7]),
            (
                4,
                49,
                &[
                    29, 16, 36, 39, 4, 43, 13, 38, 45, 33, 6, 31, 23, 35, 14, 0, 26, 44, 18, 24,
                    37, 10, 22, 15, 42, 27, 30, 8, 3, 11, 21, 46, 7, 40, 1, 2, 34, 5, 32, 28, 17,
                    41, 20, 12, 48, 47, 19, 9, 25,
                ],
            ),
            (
                5,
                50,
                &[
                    7, 5, 19, 33, 9, 48, 49, 41, 34, 43, 25, 0, 12, 44, 31, 15, 32, 29, 8, 46, 4,
                    40, 37, 39, 24, 45, 26, 20, 1, 14, 22, 42, 11, 2, 21, 28, 13, 36, 10, 3, 17,
                    35, 23, 18, 38, 30, 47, 16, 27, 6,
                ],
            ),
            (
                6,
                51,
                &[
                    30, 45, 32, 26, 49, 10, 29, 5, 17, 9, 31, 28, 6, 23, 21, 43, 16, 35, 18, 34,
                    47, 50, 7, 22, 37, 14, 46, 2, 3, 38, 12, 13, 19, 4, 41, 1, 39, 8, 11, 20, 0,
                    27, 36, 33, 48, 44, 24, 25, 15, 42, 40,
                ],
            ),
            (
                7,
                120,
                &[
                    16, 97, 78, 40, 108, 68, 49, 36, 89, 81, 41, 33, 0, 30, 50, 5, 43, 92, 29, 109,
                    117, 18, 56, 90, 34, 24, 101, 20, 3, 57, 42, 86, 105, 106, 15, 21, 53, 58, 83,
                    55, 82, 111, 65, 112, 93, 99, 72, 17, 71, 9, 13, 39, 84, 66, 76, 31, 107, 54,
                    48, 96, 60, 45, 80, 69, 73, 2, 27, 113, 119, 47, 14, 10, 12, 110, 44, 88, 1,
                    26, 70, 22, 35, 104, 6, 100, 38, 103, 118, 4, 79, 46, 94, 102, 75, 23, 115, 37,
                    51, 8, 19, 52, 91, 98, 62, 25, 114, 59, 7, 85, 61, 64, 74, 87, 116, 11, 63, 67,
                    77, 32, 95, 28,
                ],
            ),
        ];
        for (seed, n, want) in cases {
            assert_eq!(sorted_indices(&generated(*seed, *n)), *want, "seed {seed}");
        }
    }

    /// As above for longer lists (partitioning, pattern breaking), compared by a hash of the
    /// permutation.
    #[test]
    fn sort_keys_matches_go_on_long_lists() {
        let cases: &[(u64, usize, u64)] = &[
            (8, 300, 0xac456426a98708df),
            (9, 1000, 0x2d22067ae6c99ebd),
            (10, 5000, 0x027d547a3dc80e2d),
        ];
        for (seed, n, want) in cases {
            assert_eq!(
                fnv(&sorted_indices(&generated(*seed, *n))),
                *want,
                "seed {seed}"
            );
        }
    }

    /// Go's `sort.Sort(keyList)` on patterned inputs (already sorted, reversed, all equal,
    /// zero-padded numbers, organ pipe, NaNs, almost sorted), as permutation hashes.
    #[test]
    fn sort_keys_matches_go_on_patterns() {
        let mk = |n: usize, f: &dyn Fn(usize) -> SortKey| (0..n).map(f).collect::<Vec<_>>();
        let inputs: Vec<(&str, Vec<SortKey>)> = vec![
            ("ascending", mk(500, &|i| string(&format!("k{i}")))),
            ("descending", mk(500, &|i| string(&format!("k{}", 500 - i)))),
            ("equal", mk(500, &|_| string("same"))),
            (
                "zeros",
                mk(500, &|i| string(&format!("a{:0w$}", i % 3, w = i % 7))),
            ),
            (
                "organ",
                mk(500, &|i| {
                    int(if i < 250 { i as i64 } else { 500 - i as i64 })
                }),
            ),
            (
                "nan",
                mk(300, &|i| {
                    float(if i % 3 == 0 {
                        f64::NAN
                    } else {
                        (i % 17) as f64
                    })
                }),
            ),
            (
                "almost",
                mk(400, &|i| {
                    int(if i % 97 == 0 {
                        1000 - i as i64
                    } else {
                        i as i64
                    })
                }),
            ),
        ];
        let want: &[(&str, u64)] = &[
            ("ascending", 0x8e07d716feaf3681),
            ("descending", 0x7aa3661028122c79),
            ("equal", 0x8e07d716feaf3681),
            ("zeros", 0x7c4f671192f5f45b),
            ("organ", 0x4f0ce7a2c612e3eb),
            ("nan", 0xcfeba1e14ea5e6a7),
            ("almost", 0x44afa45af6537d75),
        ];
        for ((name, keys), (want_name, want)) in inputs.iter().zip(want) {
            assert_eq!(name, want_name);
            assert_eq!(fnv(&sorted_indices(keys)), *want, "{name}");
        }
    }

    #[test]
    fn sort_keys_short_inputs() {
        let mut empty: Vec<SortKey> = Vec::new();
        sort_keys(&mut empty, |k| k);
        let mut one = vec![string("x")];
        sort_keys(&mut one, |k| k);
        assert_eq!(one, vec![string("x")]);
        let mut keys = vec![
            string("b"),
            int(3),
            string("a10"),
            string("a2"),
            SortKey::Bool(true),
        ];
        sort_keys(&mut keys, |k| k);
        assert_eq!(
            keys,
            vec![
                SortKey::Bool(true),
                int(3),
                string("a2"),
                string("a10"),
                string("b")
            ]
        );
    }

    #[test]
    fn less_numbers_then_kinds() {
        // Numbers and bools compare by value, then by kind, then within the kind.
        assert!(key_list_less(&SortKey::Bool(true), &int(2)));
        assert!(key_list_less(&int(1), &uint(1)));
        assert!(key_list_less(&uint(1), &float(1.0)));
        assert!(!key_list_less(&float(1.0), &int(1)));
        assert!(key_list_less(&SortKey::Bool(false), &SortKey::Bool(true)));
        assert!(key_list_less(
            &SortKey::Float(ReflectKind::Float32, 1.0),
            &float(1.0)
        ));
        // NaN is neither less nor greater than anything numeric; kinds still order.
        assert!(!key_list_less(&float(f64::NAN), &float(1.0)));
        assert!(!key_list_less(&float(1.0), &float(f64::NAN)));
        assert!(key_list_less(&int(5), &string("1")));
        assert!(key_list_less(
            &string("z"),
            &SortKey::Other(ReflectKind::Struct)
        ));
        assert!(key_list_less(
            &SortKey::Other(ReflectKind::Interface),
            &string("")
        ));
    }

    /// `keyList.Less` on string pairs (printed by sorter.go): letters compare as runes, a
    /// non-letter sorts before a letter unless the previous equal rune was a digit, digit
    /// runs compare numerically (any Nd rune counts, by `r - '0'`), zero-padded runs by
    /// value then length, prefixes first.
    #[test]
    fn less_strings_matches_go() {
        let rows: &[(&str, &str, bool)] = &[
            ("a", "b", true),
            ("B", "a", true),
            ("a2", "a10", true),
            ("a10", "a2", false),
            ("a", "1", false),
            ("1", "a", true),
            ("ab", "a1", false),
            ("a1", "ab", true),
            ("1a", "1.", true),
            ("1.", "1a", false),
            ("a/", "a1", true),
            ("a1", "a/", false),
            ("a/002", "a/3", true),
            ("a/0001", "a/002", true),
            ("a/002", "a/0001", false),
            ("b1", "b01", true),
            ("b01", "b1", false),
            ("b01", "b3", true),
            ("b01", "b2", true),
            ("d7", "d7abc", true),
            ("", "a", true),
            ("a", "", false),
            ("é", "1", false),
            ("1", "é", true),
            ("x٣", "x٤", true),
            ("x٣", "x3", false),
            ("x3", "x٣", true),
            ("a0", "a00", true),
            ("a00", "a0", false),
            ("a10", "a010", true),
            ("a010", "a10", false),
            ("1.5", "1.10", true),
            ("x", "x", false),
            ("_", "a", true),
            ("a", "_", false),
            ("9", "10", true),
            ("09", "10", true),
        ];
        for (a, b, want) in rows {
            assert_eq!(
                key_list_less(&string(a), &string(b)),
                *want,
                "{a:?} < {b:?}"
            );
        }
    }

    #[test]
    fn key_float_and_num_less() {
        assert_eq!(key_float(&SortKey::Bool(true)), Some(1.0));
        assert_eq!(key_float(&SortKey::Bool(false)), Some(0.0));
        assert_eq!(key_float(&uint(u64::MAX)), Some(u64::MAX as f64));
        assert_eq!(key_float(&int(-3)), Some(-3.0));
        assert_eq!(key_float(&string("1")), None);
        assert_eq!(key_float(&SortKey::Other(ReflectKind::Interface)), None);
        assert!(num_less(&int(-1), &int(0)));
        assert!(num_less(&uint(1), &uint(2)));
        assert!(num_less(&float(0.5), &float(1.5)));
        assert!(num_less(&SortKey::Bool(false), &SortKey::Bool(true)));
        assert!(!num_less(&SortKey::Bool(true), &SortKey::Bool(true)));
    }

    #[test]
    #[should_panic(expected = "not a number")]
    fn num_less_panics_on_strings() {
        num_less(&string("a"), &string("b"));
    }

    #[test]
    fn sort_key_kinds_and_go_values() {
        use chrono::DateTime;
        assert_eq!(int(1).kind(), ReflectKind::Int);
        assert_eq!(SortKey::Bool(true).kind(), ReflectKind::Bool);
        assert_eq!(string("x").kind(), ReflectKind::String);
        assert_eq!(SortKey::Other(ReflectKind::Map).kind(), ReflectKind::Map);
        let ts = DateTime::parse_from_rfc3339("2001-12-14T21:59:43Z").unwrap();
        let cases = [
            (GoValue::Null, SortKey::Other(ReflectKind::Interface)),
            (GoValue::Bool(true), SortKey::Bool(true)),
            (GoValue::Int(-1), int(-1)),
            (GoValue::Uint(7), SortKey::Uint(ReflectKind::Uint64, 7)),
            (GoValue::Float(0.5), float(0.5)),
            (GoValue::String("s".into()), string("s")),
            (
                GoValue::Bytes(b"a\xe2\x82b\xff".to_vec()),
                string("a\u{fffd}\u{fffd}b\u{fffd}"),
            ),
            (GoValue::Timestamp(ts), SortKey::Other(ReflectKind::Struct)),
            (GoValue::Seq(vec![]), SortKey::Other(ReflectKind::Slice)),
            (GoValue::Map(vec![]), SortKey::Other(ReflectKind::Map)),
            (GoValue::MapAny(vec![]), SortKey::Other(ReflectKind::Map)),
        ];
        for (v, want) in cases {
            assert_eq!(SortKey::from_go_value(&v), want, "{v:?}");
        }
    }

    #[test]
    fn go_unicode_tables() {
        for (r, letter, digit) in [
            ('a', true, false),
            ('é', true, false),
            ('ª', true, false),
            ('µ', true, false),
            ('²', false, false),
            ('٣', false, true),
            ('Ⅻ', false, false),
            ('中', true, false),
            ('_', false, false),
            ('0', false, true),
            ('\u{10ffff}', false, false),
        ] {
            assert_eq!(is_letter(r), letter, "{r:?}");
            assert_eq!(is_digit(r), digit, "{r:?}");
        }
        for table in [LETTER_RANGES, DIGIT_RANGES] {
            assert_eq!(table.len() % 2, 0);
            for pair in table.chunks(2) {
                assert!(pair[0] <= pair[1]);
            }
            for w in table.windows(2).skip(1).step_by(2) {
                // hi of one pair < lo of the next (maximal runs never touch).
                assert!(w[0] + 1 < w[1]);
            }
        }
        assert_eq!(LETTER_RANGES.len() / 2, 684);
        assert_eq!(DIGIT_RANGES.len() / 2, 72);
    }

    #[test]
    fn pdqsort_helpers() {
        assert_eq!(bits_len(0), 0);
        assert_eq!(bits_len(1), 1);
        assert_eq!(bits_len(12), 4);
        assert_eq!(next_power_of_two(12), 16);
        assert_eq!(next_power_of_two(16), 32);
        let mut x = Xorshift(8);
        assert_eq!(
            x.next(),
            8 ^ (8 << 13) ^ ((8 ^ (8 << 13)) >> 7) ^ {
                let y = 8u64 ^ (8 << 13);
                let y = y ^ (y >> 7);
                y << 17
            }
        );
    }
}
