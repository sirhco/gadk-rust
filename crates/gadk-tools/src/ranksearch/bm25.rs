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

//! Okapi BM25 ranking over pre-tokenized token bags, and the tokenizer and plural stemmer that
//! build them. Port of ADK-Go `internal/ranksearch/bm25.go`.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use super::go_syntax;

/// Term-frequency saturation (Go `bm25K1`).
const BM25_K1: f64 = 1.5;
/// Document-length normalization (Go `bm25B`).
const BM25_B: f64 = 0.75;

/// A document index with its BM25 relevance score (Go `scoredDoc`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScoredDoc {
    pub(crate) index: usize,
    pub(crate) score: f64,
}

/// Scores each document against the query using Okapi BM25 and returns the indices sorted by
/// descending score (a stable sort, so equal scores keep document order). Documents with no
/// query-term overlap (score 0) are omitted. Port of Go `rankBM25`.
///
/// IDF and length statistics are rebuilt on every call, as in Go.
pub(crate) fn rank_bm25<D: AsRef<[String]>>(query: &str, docs: &[D]) -> Vec<ScoredDoc> {
    let query_terms = tokenize(query);
    if query_terms.is_empty() || docs.is_empty() {
        return Vec::new();
    }

    let mut df: HashMap<&str, usize> = HashMap::new();
    let mut total_len = 0usize;
    for doc in docs {
        let doc = doc.as_ref();
        total_len += doc.len();
        let mut seen: HashSet<&str> = HashSet::with_capacity(doc.len());
        for tok in doc {
            if seen.insert(tok.as_str()) {
                *df.entry(tok.as_str()).or_insert(0) += 1;
            }
        }
    }
    let avg_len = total_len as f64 / docs.len() as f64;
    let n = docs.len() as f64;

    let mut scored = Vec::new();
    for (i, doc) in docs.iter().enumerate() {
        let doc = doc.as_ref();
        let mut tf: HashMap<&str, usize> = HashMap::with_capacity(doc.len());
        for tok in doc {
            *tf.entry(tok.as_str()).or_insert(0) += 1;
        }
        let mut score = 0.0f64;
        for term in &query_terms {
            let count = tf.get(term.as_str()).copied().unwrap_or(0);
            if count == 0 {
                continue;
            }
            let f = count as f64;
            let d = df.get(term.as_str()).copied().unwrap_or(0) as f64;
            let idf = (1.0 + (n - d + 0.5) / (d + 0.5)).ln();
            let norm = f * (BM25_K1 + 1.0)
                / (f + BM25_K1 * (1.0 - BM25_B + BM25_B * doc.len() as f64 / avg_len));
            score += idf * norm;
        }
        if score > 0.0 {
            scored.push(ScoredDoc { index: i, score });
        }
    }
    // Go sort.SliceStable with `a.score > b.score`.
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored
}

/// Lowercases `s`, splits it on non-alphanumeric boundaries and camelCase humps (so "list_books"
/// and "getBook" both split into their words), and applies a light plural stemmer so "books" and
/// "book" collide. Port of Go `tokenize`; letters, digits and case use Go's `unicode` categories
/// (L, Nd, Lu, Ll), not Rust's `char` properties.
pub(crate) fn tokenize(s: &str) -> Vec<String> {
    fn flush(tokens: &mut Vec<String>, cur: &mut String) {
        if !cur.is_empty() {
            tokens.push(stem(&go_to_lower(cur)));
            cur.clear();
        }
    }
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for r in s.chars() {
        if is_letter(r) || is_digit(r) {
            if is_upper(r) && prev.is_some_and(is_lower) {
                flush(&mut tokens, &mut cur); // camelCase boundary
            }
            cur.push(r);
        } else {
            flush(&mut tokens, &mut cur);
        }
        prev = Some(r);
    }
    flush(&mut tokens, &mut cur);
    tokens
}

/// Maps a word and its plural to the same token. Port of Go `stem` (see its doc comment for the
/// rules); the first matching rule wins, and lengths are in bytes, as in Go.
fn stem(tok: &str) -> String {
    let n = tok.len();
    let stripped = if n > 4 && tok.ends_with("ies") {
        // policies -> policy, activities -> activity
        return tok
            .strip_suffix("ies")
            .map_or_else(|| tok.to_string(), |s| format!("{s}y"));
    } else if n > 5 && tok.ends_with("ases") {
        tok.strip_suffix("ses") // aliases -> alia, databases -> databa
    } else if n > 4 && tok.ends_with("ase") {
        tok.strip_suffix("se") // database -> databa, release -> relea
    } else if n > 4 && ends_with_any(tok, &["ses", "xes", "zes", "ches", "shes"]) {
        tok.strip_suffix("es") // processes -> process, statuses -> status, boxes -> box
    } else if n > 3 && ends_with_any(tok, &["se", "xe", "ze", "che", "she"]) {
        tok.strip_suffix('e') // case -> cas, cache -> cach, size -> siz
    } else if tok.ends_with("ss") || tok.ends_with("us") {
        None // process, address, status, nexus — not plurals
    } else if n > 3 && tok.ends_with('s') {
        tok.strip_suffix('s') // books -> book, names -> name
    } else {
        None
    };
    stripped.unwrap_or(tok).to_string()
}

/// Reports whether `s` ends with any of the given suffixes (Go `endsWithAny`).
fn ends_with_any(s: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suf| s.ends_with(suf))
}

/// Go `strings.ToLower`: the simple per-rune lowercase mapping, with no final-sigma context. Rust's
/// `str::to_lowercase` differs (context-sensitive Σ, and U+0130 lowercases to two runes).
fn go_to_lower(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(l), None) => l,
                // U+0130 is the only rune whose full lowercase mapping has two runes; Go's
                // unicode.ToLower maps it to 'i'.
                _ if c == '\u{130}' => 'i',
                _ => c,
            }
        })
        .collect()
}

/// Go `unicode.L`, `unicode.Nd`, `unicode.Lu` and `unicode.Ll`, as Go-layout range pairs.
static LETTER: LazyLock<Vec<i32>> = LazyLock::new(|| general_category("L"));
static DIGIT: LazyLock<Vec<i32>> = LazyLock::new(|| general_category("Nd"));
static UPPER: LazyLock<Vec<i32>> = LazyLock::new(|| general_category("Lu"));
static LOWER: LazyLock<Vec<i32>> = LazyLock::new(|| general_category("Ll"));

fn general_category(name: &str) -> Vec<i32> {
    go_syntax::property_ranges(&format!("gc={name}")).unwrap_or_default()
}

/// Go `unicode.IsLetter`.
fn is_letter(c: char) -> bool {
    go_syntax::in_char_class(go_syntax::rune(c), &LETTER)
}

/// Go `unicode.IsDigit`.
fn is_digit(c: char) -> bool {
    go_syntax::in_char_class(go_syntax::rune(c), &DIGIT)
}

/// Go `unicode.IsUpper`.
fn is_upper(c: char) -> bool {
    go_syntax::in_char_class(go_syntax::rune(c), &UPPER)
}

/// Go `unicode.IsLower`.
fn is_lower(c: char) -> bool {
    go_syntax::in_char_class(go_syntax::rune(c), &LOWER)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn unicode_categories_follow_go() {
        assert!(is_letter('é') && is_letter('Δ') && !is_letter('_') && !is_letter('1'));
        assert!(is_digit('7') && is_digit('٣') && !is_digit('½') && !is_digit('Ⅳ'));
        assert!(is_upper('Δ') && is_lower('δ') && !is_upper('ǅ') && !is_lower('ǅ'));
        assert_eq!(go_to_lower("ΣΑΣ İ"), "σασ i");
    }

    #[test]
    fn stem_rules_follow_go() {
        for (tok, want) in [
            ("policies", "policy"),
            ("ties", "tie"),
            ("aliases", "alia"),
            ("bases", "bas"),
            ("database", "databa"),
            ("processes", "process"),
            ("caches", "cach"),
            ("case", "cas"),
            ("use", "use"),
            ("address", "address"),
            ("nexus", "nexus"),
            ("books", "book"),
            ("bus", "bus"),
            ("gas", "gas"),
        ] {
            assert_eq!(stem(tok), want, "stem({tok:?})");
        }
    }
}
