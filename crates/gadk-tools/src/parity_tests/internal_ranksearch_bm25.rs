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

//! Ports of ADK-Go `internal/ranksearch/bm25_test.go` (white-box: `package ranksearch`).

use super::super::ranksearch::bm25::{rank_bm25, tokenize};

fn words(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|s| s.to_string()).collect()
}

/// The doc with the most matches of the query term ranks first and a doc with no overlap is
/// omitted entirely.
#[test]
fn test_rank_bm25_ranks_higher_overlap_first() {
    let docs = vec![
        tokenize("list books list books list books list all books"),
        tokenize("get author"),
        tokenize("create book"),
    ];

    let ranked = rank_bm25("books", &docs);
    assert!(!ranked.is_empty(), "rankBM25() returned no results");
    assert_eq!(
        ranked[0].index, 0,
        "top index = {}, want 0 (doc with the most 'book' matches)",
        ranked[0].index
    );
    for r in &ranked {
        assert_ne!(r.index, 1, "doc with no overlap must be omitted");
    }
}

/// IDF down-weights a term present in every doc ("list") so a rare term ("publisher") dominates
/// ranking.
#[test]
fn test_rank_bm25_downweights_common_terms() {
    let docs = vec![
        tokenize("list books"),
        tokenize("list authors"),
        tokenize("list publishers"),
    ];

    let ranked = rank_bm25("list publishers", &docs);
    assert!(!ranked.is_empty(), "rankBM25() returned no results");
    assert_eq!(
        ranked[0].index, 2,
        "top index = {}, want 2 (rare term 'publisher' must outweigh the common term 'list')",
        ranked[0].index
    );
}

/// The tokenizer splits both snake_case and camelCase into their constituent words.
#[test]
fn test_tokenize_splits_snake_and_camel_case() {
    for (input, want) in [
        ("list_books", words(&["list", "book"])),
        ("getBook", words(&["get", "book"])),
    ] {
        assert_eq!(tokenize(input), want, "tokenize({input:?})");
    }
}

/// Runs of separators produce no empty token.
#[test]
fn test_tokenize_no_empty_tokens() {
    for (input, want) in [
        ("weather, forecast", words(&["weather", "forecast"])),
        ("an author. Fast.", words(&["an", "author", "fast"])),
        ("..x..", words(&["x"])),
    ] {
        assert_eq!(tokenize(input), want, "tokenize({input:?})");
    }
}

/// Singular and plural forms tokenize equally, including sibilant "-es" plurals, "-ases" plurals
/// of words in "-as" and "-ase", and "-as" plurals of words in "-a".
#[test]
fn test_stem_plural_collapses() {
    for (singular, plural) in [
        ("book", "books"),
        ("activity", "activities"),
        ("name", "names"),
        ("process", "processes"),
        ("status", "statuses"),
        ("database", "databases"),
        ("case", "cases"),
        ("cache", "caches"),
        ("size", "sizes"),
        ("match", "matches"),
        ("box", "boxes"),
        ("use", "uses"),
        ("axe", "axes"),
        ("alias", "aliases"),
        ("canvas", "canvases"),
        ("release", "releases"),
        ("base", "bases"),
        ("gas", "gases"),
        ("schema", "schemas"),
        ("idea", "ideas"),
    ] {
        assert_eq!(tokenize(plural), tokenize(singular), "tokenize({plural:?})");
    }
}

/// "-us" and "-ss" words are not over-stripped.
#[test]
fn test_stem_leaves_non_plurals_intact() {
    for word in ["status", "nexus", "process", "address"] {
        assert_eq!(tokenize(word), words(&[word]), "tokenize({word:?})");
    }
}
