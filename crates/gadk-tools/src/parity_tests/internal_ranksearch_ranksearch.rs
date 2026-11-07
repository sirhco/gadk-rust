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

//! Ports of ADK-Go `internal/ranksearch/ranksearch_test.go` (white-box: `package ranksearch`).
//!
//! Go strings may hold invalid UTF-8 and Rust `&str` cannot, so tests whose Go queries contain
//! invalid bytes (`"\xdc"`, `"\xff"`) call `rank_bytes`, the byte-string core that `rank` wraps,
//! with Go's exact bytes.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::super::ranksearch::go_syntax::{self, PERL};
use super::super::ranksearch::{
    Config, Item, MAX_QUERY_LEN, Match, build_tokens, compile_query, describe_search,
    looks_like_regex, no_match_note, rank, rank_bytes,
};

const TEST_MAX_DESC_LEN: i32 = 200;

fn default_cfg() -> Config {
    Config {
        item_noun: "tool".to_string(),
        max_results: 8,
        min_score_ratio: 0.1,
        max_desc_len: TEST_MAX_DESC_LEN,
    }
}

/// Builds an Item with a name+description token bag, the same way callers do.
fn item(name: &str, desc: &str, extra: &[&str]) -> Item {
    Item {
        name: name.to_string(),
        description: desc.to_string(),
        tokens: build_tokens(name, desc, extra),
    }
}

fn make_items(pairs: &[[&str; 2]]) -> Vec<Item> {
    pairs.iter().map(|[n, d]| item(n, d, &[])).collect()
}

fn names(matches: &[Match]) -> Vec<String> {
    matches.iter().map(|m| m.name.clone()).collect()
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn set(names: &[&str]) -> HashSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// Go's nil `alreadyAvailable` map.
fn none() -> HashSet<String> {
    HashSet::new()
}

/// Reports names in `want` that are missing from `got` and names in `not_want` that are
/// present in `got`.
fn check_names(got: &[String], want: &[&str], not_want: &[&str]) {
    for n in want {
        assert!(
            got.iter().any(|g| g == n),
            "matches = {got:?}, want {n:?} included"
        );
    }
    for n in not_want {
        assert!(
            !got.iter().any(|g| g == n),
            "matches = {got:?}, want {n:?} excluded"
        );
    }
}

/// A query for the error messages: Go `%q` shows invalid bytes, lossy decoding marks them.
fn show(query: &[u8]) -> String {
    String::from_utf8_lossy(query).into_owned()
}

#[test]
fn test_rank_case_insensitive() {
    let items = make_items(&[
        ["ListBooks", "Lists all Books"],
        ["get_author", "returns an Author"],
    ]);
    let (matches, _) = rank(&items, "books", &none(), &default_cfg());
    assert_eq!(
        names(&matches),
        strings(&["ListBooks"]),
        "Rank() names mismatch"
    );
}

#[test]
fn test_rank_matches_on_description_only() {
    let items = make_items(&[
        ["fetch_records", "List all books for a library"],
        ["get_thing", "returns an unrelated thing"],
    ]);
    let (matches, _) = rank(&items, "books", &none(), &default_cfg());
    assert_eq!(
        names(&matches),
        strings(&["fetch_records"]),
        "Rank() names mismatch"
    );
}

#[test]
fn test_rank_keyword_match() {
    let items = make_items(&[
        ["list_books", "list books"],
        ["get_book", "get a book"],
        ["list_authors", "list authors"],
    ]);
    let (matches, _) = rank(&items, "book", &none(), &default_cfg());
    check_names(
        &names(&matches),
        &["list_books", "get_book"],
        &["list_authors"],
    );
}

#[test]
fn test_rank_invalid_regex_falls_back_to_literal() {
    let items = make_items(&[
        ["list_books", "list all books"],
        ["get_author", "get author"],
    ]);
    // "(unclosed" is an invalid regex without pattern syntax, so it is matched as a literal,
    // which matches nothing, and then by keywords.
    let (matches, note) = rank(&items, "(unclosed", &none(), &default_cfg());
    assert!(
        matches.is_empty(),
        "Rank() = {:?}, want no matches",
        names(&matches)
    );
    assert_eq!(note, no_match_note(&default_cfg()), "Rank() note");
}

#[test]
fn test_rank_regex_pattern() {
    let items = make_items(&[
        ["get_library_book", "retrieve a book for a library"],
        ["get_author", "retrieve an author"],
        ["list_books", "list all books"],
    ]);
    let (matches, _) = rank(&items, "get_.*_book", &none(), &default_cfg());
    check_names(
        &names(&matches),
        &["get_library_book"],
        &["get_author", "list_books"],
    );
}

#[test]
fn test_rank_ranks_name_matches_above_description_only() {
    let items = make_items(&[
        ["get_publisher", "retrieve a publisher record"],
        ["list_reviews", "list publisher reviews"],
        ["publisher_search", "search across all publishers"],
    ]);
    let (matches, _) = rank(&items, "publisher", &none(), &default_cfg());
    assert_eq!(
        matches.len(),
        3,
        "Rank() returned {} matches, want 3",
        matches.len()
    );
    assert_eq!(
        matches[2].name, "list_reviews",
        "last match (description-only match ranks last)"
    );
    check_names(
        &names(&matches[..2]),
        &["get_publisher", "publisher_search"],
        &[],
    );
}

#[test]
fn test_rank_indexes_extra_tokens() {
    let items = vec![
        item(
            "update_note",
            "modify a note",
            &["body", "the comment text to attach"],
        ),
        item("list_images", "list all images", &[]),
    ];
    let (matches, _) = rank(&items, "comment", &none(), &default_cfg());
    check_names(&names(&matches), &["update_note"], &["list_images"]);
}

#[test]
fn test_rank_excludes_already_available() {
    let items = make_items(&[["list_books", "list books"], ["get_book", "get a book"]]);
    let (matches, _) = rank(&items, "book", &set(&["list_books"]), &default_cfg());
    check_names(&names(&matches), &["get_book"], &["list_books"]);
}

#[test]
fn test_rank_capped_results_include_note() {
    let mut cfg = default_cfg();
    cfg.max_results = 2;
    let items = make_items(&[
        ["tool_a", "some tool"],
        ["tool_b", "some tool"],
        ["tool_c", "some tool"],
    ]);
    let (matches, note) = rank(&items, "tool", &none(), &cfg);
    assert_eq!(
        matches.len(),
        2,
        "Rank() returned {} matches, want 2",
        matches.len()
    );
    assert!(
        !note.is_empty(),
        "Rank() note is empty, want a truncation note"
    );
}

#[test]
fn test_rank_empty_query_returns_note() {
    let items = make_items(&[["list_books", "list books"]]);
    let (matches, note) = rank(&items, "   ", &none(), &default_cfg());
    assert!(
        matches.is_empty(),
        "Rank() = {:?}, want no matches",
        names(&matches)
    );
    assert!(
        !note.is_empty(),
        "Rank() note is empty, want a guidance note"
    );
}

#[test]
fn test_rank_query_too_long() {
    let items = make_items(&[["list_books", "list books"]]);
    let (matches, note) = rank(
        &items,
        &"a".repeat(MAX_QUERY_LEN + 1),
        &none(),
        &default_cfg(),
    );
    assert!(
        matches.is_empty(),
        "Rank() = {:?}, want no matches",
        names(&matches)
    );
    assert!(
        note.contains("query too long"),
        "Rank() note = {note:?}, want it to contain \"query too long\""
    );
}

#[test]
fn test_rank_multi_word_query() {
    let items = make_items(&[
        ["list_publisher_reviews", "list reviews for publishers"],
        ["get_author", "get author"],
    ]);
    let (matches, _) = rank(&items, "publisher reviews", &none(), &default_cfg());
    check_names(
        &names(&matches),
        &["list_publisher_reviews"],
        &["get_author"],
    );
}

#[test]
fn test_rank_select_by_name() {
    let items = make_items(&[
        ["patch_book", "patch a book"],
        ["update_book", "update a book"],
        ["list_books", "list books"],
    ]);
    let (matches, note) = rank(
        &items,
        "select:patch_book,update_book,unknown_tool",
        &none(),
        &default_cfg(),
    );
    check_names(
        &names(&matches),
        &["patch_book", "update_book"],
        &["list_books"],
    );
    assert!(
        note.contains("unknown_tool"),
        "Rank() note = {note:?}, want the missing name \"unknown_tool\" in it"
    );
}

#[test]
fn test_rank_select_skips_already_available() {
    let items = make_items(&[["patch_book", "patch a book"], ["list_books", "list books"]]);
    let (matches, note) = rank(
        &items,
        "select:patch_book,list_books",
        &set(&["list_books"]),
        &default_cfg(),
    );
    assert_eq!(
        names(&matches),
        strings(&["patch_book"]),
        "Rank() names mismatch"
    );
    assert_eq!(note, "already available: list_books", "Rank() note");
}

#[test]
fn test_rank_truncates_at_rune_boundary() {
    let desc = format!(
        "{}€{}",
        "x".repeat(usize::try_from(TEST_MAX_DESC_LEN - 1).unwrap()),
        "x".repeat(10)
    );
    // The first query is ranked by BM25 and the second matches the name.
    for query in ["t", "^t$"] {
        let (matches, _) = rank(&[item("t", &desc, &[])], query, &none(), &default_cfg());
        assert_eq!(
            matches.len(),
            1,
            "Rank({query:?}) returned {} matches, want 1",
            matches.len()
        );
        let got = &matches[0].description;
        assert!(
            std::str::from_utf8(got.as_bytes()).is_ok(),
            "Rank({query:?}): truncated description is not valid UTF-8"
        );
        assert!(
            got.len() <= usize::try_from(TEST_MAX_DESC_LEN).unwrap(),
            "Rank({query:?}): len(description) = {}, want <= {TEST_MAX_DESC_LEN}",
            got.len()
        );
    }
}

#[test]
fn test_describe_search_appends_sorted_names() {
    let out = describe_search("base.", &strings(&["zebra", "alpha"]));
    for want in ["base.", "alpha, zebra"] {
        assert!(
            out.contains(want),
            "DescribeSearch() = {out:?}, want it to contain {want:?}"
        );
    }
}

#[test]
fn test_describe_search_lists_each_name_once() {
    let out = describe_search("base.", &strings(&["zebra", "alpha", "zebra"]));
    let got = out.matches("zebra").count();
    assert_eq!(
        got, 1,
        "DescribeSearch() = {out:?}, lists zebra {got} times, want once"
    );
}

#[test]
fn test_describe_search_no_names_is_base_description() {
    assert_eq!(describe_search("base.", &[]), "base.", "DescribeSearch()");
}

#[test]
fn test_rank_natural_language_with_regex_characters() {
    let items = make_items(&[
        ["list_books", "list all books"],
        ["get_author", "get author"],
    ]);
    for query in [
        "how do I list books?",
        "list the books.",
        "books (all of them)",
        "list 2*3 books",
    ] {
        let (matches, _) = rank(&items, query, &none(), &default_cfg());
        check_names(&names(&matches), &["list_books"], &["get_author"]);
    }
}

#[test]
fn test_rank_pattern_that_matches_nothing_stays_empty() {
    let items = make_items(&[
        ["get_user_file", "fetch a user's file"],
        ["delete_user_record", "delete a record"],
    ]);
    let (matches, note) = rank(&items, "^get_.*_record$", &none(), &default_cfg());
    assert!(
        matches.is_empty(),
        "Rank() = {:?}, want no matches",
        names(&matches)
    );
    assert!(
        !note.is_empty(),
        "Rank() note is empty, want a no-match note"
    );
}

#[test]
fn test_rank_fallback_is_announced() {
    let items = make_items(&[["list_books", "list all books"]]);
    let (_, note) = rank(&items, "how do I list books?", &none(), &default_cfg());
    const FALLBACK_NOTE: &str = "showing keyword matches";
    assert!(
        note.contains(FALLBACK_NOTE),
        "Rank() note = {note:?}, want it to contain {FALLBACK_NOTE:?}"
    );

    let mut cfg = default_cfg();
    cfg.max_results = 2;
    let items = make_items(&[
        ["tool_a", "some tool"],
        ["tool_b", "some tool"],
        ["tool_c", "some tool"],
    ]);
    let (_, note) = rank(&items, "which tool?", &none(), &cfg);
    for want in [FALLBACK_NOTE, "showing 2 of 3"] {
        assert!(
            note.contains(want),
            "capped fallback note = {note:?}, want it to contain {want:?}"
        );
    }
}

#[test]
fn test_rank_only_already_available_matches_say_so() {
    let items = make_items(&[
        ["list_books", "list books"],
        ["get_author", "get an author"],
    ]);
    let available = set(&["list_books"]);
    for query in ["books", "select:list_books"] {
        let (matches, note) = rank(&items, query, &available, &default_cfg());
        assert!(
            matches.is_empty(),
            "Rank({query:?}) matches = {:?}, want none",
            names(&matches)
        );
        assert!(
            note.contains("already available") && note.contains("list_books"),
            "Rank({query:?}) note = {note:?}, want it to name list_books as already available"
        );
    }
    // Control: with nothing available, the same query does match.
    let (matches, _) = rank(&items, "books", &none(), &default_cfg());
    assert!(
        !matches.is_empty(),
        "Rank(\"books\") with nothing available returned no matches"
    );
    // A query that matches nothing at all still says so.
    let (_, note) = rank(&items, "weather", &available, &default_cfg());
    assert_eq!(
        note,
        no_match_note(&default_cfg()),
        "Rank(\"weather\") note"
    );
    // MaxResults 0 means no cap, so the note still names the item.
    let mut unlimited = default_cfg();
    unlimited.max_results = 0;
    let (_, note) = rank(&items, "books", &available, &unlimited);
    assert!(
        note.contains("list_books"),
        "Rank(\"books\") with MaxResults 0 note = {note:?}, want it to name list_books"
    );
}

#[test]
fn test_rank_select_reports_unknown_name_once() {
    let (_, note) = rank(
        &make_items(&[["list_books", "list books"]]),
        "select:ghost,ghost",
        &none(),
        &default_cfg(),
    );
    assert_eq!(note, "tools not found: ghost", "Rank() note");
}

#[test]
fn test_rank_already_available_note_is_capped() {
    let items = make_items(&[
        ["list_books", "list books"],
        ["list_authors", "list authors"],
        ["list_notes", "list notes"],
    ]);
    let available = set(&["list_books", "list_authors", "list_notes"]);
    let mut cfg = default_cfg();
    cfg.max_results = 2;
    let (_, note) = rank(&items, "list", &available, &cfg);
    let Some((_, listed)) = note.split_once("already available: ") else {
        panic!("Rank() note = {note:?}, want it to name the already-available tools");
    };
    let got: Vec<&str> = listed.split(", ").collect();
    assert_eq!(
        got.len(),
        2,
        "Rank() note names {got:?}, want 2 of the 3 matching tools"
    );
}

#[test]
fn test_rank_relevance_floor_drops_weak_matches() {
    let items = make_items(&[
        ["list_books", "list books"],
        [
            "get_author",
            "returns the author of one of the books in the library catalog",
        ],
    ]);
    for (ratio, want) in [
        (0.5, strings(&["list_books"])),
        (0.1, strings(&["list_books", "get_author"])),
    ] {
        let mut cfg = default_cfg();
        cfg.min_score_ratio = ratio;
        let (matches, _) = rank(&items, "books", &none(), &cfg);
        assert_eq!(
            names(&matches),
            want,
            "MinScoreRatio {ratio}: Rank() names mismatch"
        );
    }
}

#[test]
fn test_rank_regex_matches_names_only() {
    let items = make_items(&[
        ["a_tool", "fetches books"],
        ["z_books", "unrelated"],
        ["m_books", "unrelated"],
    ]);
    let (matches, _) = rank(&items, "book.*", &none(), &default_cfg());
    assert_eq!(
        names(&matches),
        strings(&["m_books", "z_books"]),
        "Rank() names mismatch"
    );
}

#[test]
fn test_rank_regex_is_case_insensitive() {
    let (matches, _) = rank(
        &make_items(&[["getUserFile", "returns a file"]]),
        "getuser.*",
        &none(),
        &default_cfg(),
    );
    assert_eq!(
        names(&matches),
        strings(&["getUserFile"]),
        "Rank() names mismatch"
    );
}

#[test]
fn test_rank_invalid_pattern_matches_literally() {
    let items = make_items(&[
        ["beta(v2)_tool", "runs beta features"],
        ["other_tool", "runs (v2 features"],
    ]);
    let (matches, _) = rank(&items, "(V2", &none(), &default_cfg());
    assert_eq!(
        names(&matches),
        strings(&["beta(v2)_tool"]),
        "Rank() names mismatch"
    );
}

#[test]
fn test_rank_large_pattern_is_not_run() {
    let items = make_items(&[["fix_tool", "fixes things"]]);
    let fix_tool: &[&str] = &["fix_tool"];
    for (query, want) in [
        (".{0,100}x", fix_tool),
        (".{0,999}x", &[][..]),
        // Whitespace marks prose, which is ranked by keywords instead.
        ("fixes things {0,999}", fix_tool),
        // Each pattern below matches the empty string as a regex, so whether fix_tool comes
        // back shows whether it ran. A literal counts each rune, and an unbounded repeat counts
        // its minimum.
        ("(?:fix_tool){0,20}", fix_tool),
        ("(?:fix_tool){0,30}", &[][..]),
        ("(?:x?){150,}", &[][..]),
    ] {
        let (matches, note) = rank(&items, query, &none(), &default_cfg());
        assert_eq!(
            names(&matches),
            strings(want),
            "Rank({query:?}) names mismatch"
        );
        let too_large = note.contains("too large");
        assert_eq!(
            too_large,
            want.is_empty(),
            "Rank({query:?}) note = {note:?}, want it to say the pattern is too large: {}",
            want.is_empty()
        );
    }
}

// parity: adapted: the table holds byte strings because one Go query ("node+\xdc") is invalid UTF-8, which a Rust &str cannot hold; every query runs through rank_bytes
#[test]
fn test_rank_punctuated_keywords_fall_back_to_keywords() {
    let items = make_items(&[
        ["run_script", "Runs Node.js scripts"],
        ["compile_code", "Compiles C++ sources"],
        ["list_invoices", "Lists unpaid invoices. Supports filters."],
        ["price_lookup", "look up a price for v1.2 products"],
        ["update_record", "update a record"],
        ["convert_document", "convert PDF files to text"],
        ["build_app", "Build an iOS or Android app"],
        ["get_location", "returns the device location"],
    ]);
    let none_matched: &[&str] = &[];
    let table: &[(&[u8], &[&str])] = &[
        (b"node.js", &["run_script"]),
        (b"C++", &["compile_code"]),
        (b"invoices.", &["list_invoices"]),
        (b"v1.2", &["price_lookup"]),
        (b"(scripts)", &["run_script"]),
        // A group on its own or after a word is still keywords.
        (b"node(js)", &["run_script"]),
        (b"invoice(s)", &["list_invoices"]),
        (b"convert()", &["convert_document"]),
        // As a regex this would match the "pd" in update_record.
        (b"PDF?", &["convert_document"]),
        // Each of these fails to parse the way punctuated keywords do.
        (b"*node", &["run_script"]),
        (b"(node", &["run_script"]),
        (b"node)", &["run_script"]),
        (b"node\\", &["run_script"]),
        (b"Objective-C++", &["compile_code"]),
        (b"C++17", &["compile_code"]),
        (b"(C++)", &["compile_code"]),
        (b"(+js)", &["run_script"]),
        (b"node)(js", &["run_script"]),
        (b"node+\xdc", &["run_script"]),
        (b"convert())", &["convert_document"]),
        // A ] or a brace without a digit is text to Go's regexp.
        (b"node]", &["run_script"]),
        (b"{node}", &["run_script"]),
        // A trailing ? or + on one character is still keywords.
        (b"sources?", &["compile_code"]),
        (b"node+", &["run_script"]),
        (b"c+", &["compile_code"]),
        // So is one after a hyphen or a camelCase hump, which keywords have. As a regex, iOS?
        // would match the "io" in get_location.
        (b"iOS?", &["build_app"]),
        (b"nodeScripts?", &["run_script"]),
        (b"node-scripts?", &["run_script"]),
        // Each of these is a pattern by its text alone (see patternMarkers).
        (b"node.+js", none_matched),
        (b"node.+", none_matched),
        (b"node.{2}js", none_matched),
        (b"node.{,2}js", none_matched),
        (b"node\\x2ejs", none_matched),
        (b"node[a-z]js", none_matched),
        (b"node[.]js", none_matched),
        (b"node\\d", none_matched),
        (b"node\\Djs", none_matched),
        (b"node\\sjs", none_matched),
        (b"node\\Sjs", none_matched),
        (b"node\\Wjs", none_matched),
        (b"node\\pLjs", none_matched),
        (b"node\\PLjs", none_matched),
        (b"node|deno", none_matched),
        (b"node|node", none_matched),
        (b"^node", none_matched),
        (b"\\Anode", none_matched),
        (b"js$", none_matched),
        (b"js\\z", none_matched),
        (b"no$de", none_matched),
        (b"js\\b", none_matched),
        (b"js\\B", none_matched),
        (b"(?m)^node", none_matched),
        (b"(?:run)?js", none_matched),
        (b"node(?:s)?", none_matched),
        (b"(?i)node", none_matched),
        (b"(?:node)", none_matched),
        (b"node\\.js", none_matched),
        (b"\\Qnode\\E", none_matched),
        (b"node_scripts?", none_matched),
        (b"node_script+", none_matched),
        (b"(node)_js", none_matched),
        // Each of these is a pattern only by a repeat in its parse tree.
        (b"node.*js", none_matched),
        (b"node.?js", none_matched),
        (b"nodes*js", none_matched),
        (b"nodes*", none_matched),
        (b"nodes+js", none_matched),
        (b"node?js", none_matched),
        (b"(run)?js", none_matched),
        (b"node.(js)?", none_matched),
        (b"node.(js?)", none_matched),
        (b"nodes*j?", none_matched),
    ];
    for &(query, want) in table {
        let (matches, note) = rank_bytes(&items, query, &none(), &default_cfg());
        assert_eq!(
            names(&matches),
            strings(want),
            "Rank({:?}) names mismatch",
            show(query)
        );
        let want_note = if want.is_empty() {
            "patterns match names only"
        } else {
            "showing keyword matches"
        };
        assert!(
            note.contains(want_note),
            "Rank({:?}) note = {note:?}, want it to contain {want_note:?}",
            show(query)
        );
    }
}

// parity: adapted: the table holds byte strings because seven Go queries ("$\xdc", "getx.\xff++record", ...) are invalid UTF-8, which a Rust &str cannot hold; every query runs through rank_bytes, and the prose query through rank
#[test]
fn test_rank_unsupported_pattern_is_not_run() {
    let items = make_items(&[
        ["get_user_file", "fetch a user's file"],
        ["delete_user_record", "delete a record"],
    ]);
    let table: &[(&[u8], &str)] = &[
        (
            b"^(?!delete).*_record$",
            "invalid or unsupported Perl syntax",
        ),
        (b"_record\\Z", "invalid escape sequence"),
        (b"^get_[a-z]{1,2000}$", "invalid repeat count"),
        (b"[z-a]_record", "invalid character class range"),
        (b"(?<!delete)_record$", "invalid named capture"),
        (b"^get_[a-z_+_record$", "missing closing ]"),
        // Each of these fails with an error that punctuated keywords also produce, but its
        // text shows a pattern.
        (b"^get\\", "trailing backslash at end of expression"),
        (b"$\xdc", "invalid UTF-8"),
        (b"^get_.*+_record$", "invalid nested repetition operator"),
        (b"get_users*+_record", "invalid nested repetition operator"),
        (b"^(get|list_.*_record$", "missing closing )"),
        (b"^(get|delete)_.*)$", "unexpected )"),
        // Each of these shows one marker only.
        (b"^getuser)", "unexpected )"),
        (b"getuser)$", "unexpected )"),
        (b"(*FAIL)", "missing argument to repetition operator"),
        (b"getx.+*record", "invalid nested repetition operator"),
        (b"getusers*+record", "invalid nested repetition operator"),
        (b"getuser?+record", "invalid nested repetition operator"),
        (b"getx\\w++record", "invalid nested repetition operator"),
        (b"getx{2}+", "invalid nested repetition operator"),
        // Each of these has no marker, and is a pattern by repeatsOnceParsed.
        (b"get_users**_record", "invalid nested repetition operator"),
        (b"getusers**record", "invalid nested repetition operator"),
        (b"(get_users*_record", "missing closing )"),
        (b"get_users*_record)", "unexpected )"),
        (
            b"*getusers*record",
            "missing argument to repetition operator",
        ),
        (
            b"getusers*record\\",
            "trailing backslash at end of expression",
        ),
        (b"getusers*record\xdc", "invalid UTF-8"),
        (b"getx.**record", "invalid nested repetition operator"),
        (b"getx.?*record", "invalid nested repetition operator"),
        // Each of these lost a parenthesis. Adding one at the start or the end makes it a
        // pattern, and deleting one does not.
        (b"node.(js?", "missing closing )"),
        (b"node.js)?", "unexpected )"),
        (b"getx+)+record", "unexpected )"),
        // Each of these hides a marker behind an invalid byte.
        (b"getx.\xff++record", "invalid UTF-8"),
        (b"(\xff*FAIL)", "invalid UTF-8"),
        // Each of these is a pattern by its marker alone, since deleting what breaks it leaves
        // keywords.
        (b"getx.++record", "invalid nested repetition operator"),
        (b"*+getusers", "missing argument to repetition operator"),
        (b"?+getusers", "missing argument to repetition operator"),
        // Deleting the first ( leaves keywords, and deleting the second leaves a pattern, so
        // this is a pattern.
        (
            b"(getusers(+record)",
            "missing argument to repetition operator",
        ),
        // Deleting both parentheses leaves a pattern, and adding their partners does not.
        (b"getusers)(+record", "unexpected )"),
        // Each of these looks like keywords, but one way of fixing it leaves a pattern:
        // deleting the first *, deleting the (, or closing the (.
        (b"*node*", "missing argument to repetition operator"),
        (b"tel(+48", "missing argument to repetition operator"),
        (b"(beta?", "missing closing )"),
        // This still does not parse once fixed.
        (b"why???", "invalid nested repetition operator"),
    ];
    for &(query, want) in table {
        let (matches, note) = rank_bytes(&items, query, &none(), &default_cfg());
        assert!(
            matches.is_empty(),
            "Rank({:?}) = {:?}, want no matches",
            show(query),
            names(&matches)
        );
        assert!(
            note.starts_with("pattern not run: ") && note.contains(want),
            "Rank({:?}) note = {note:?}, want it to say the pattern was not run because of {want:?}",
            show(query)
        );
        assert!(
            !note.contains("``") && std::str::from_utf8(note.as_bytes()).is_ok(),
            "Rank({:?}) note = {note:?}, want no empty quote and valid UTF-8",
            show(query)
        );
    }
    let (matches, note) = rank(&items, "delete (?!x) record", &none(), &default_cfg());
    assert!(
        names(&matches) == strings(&["delete_user_record"])
            && note.contains("showing keyword matches"),
        "Rank(prose) = {:?}, note {note:?}, want delete_user_record ranked by keywords",
        names(&matches)
    );
}

#[test]
fn test_rank_balancing_search_is_bounded() {
    let items = make_items(&[["run_script", "Runs Node.js scripts"]]);
    // Nine ( and five ) take exactly maxBalancingSteps steps.
    let (matches, note) = rank(&items, "(((((((((node)))))", &none(), &default_cfg());
    assert!(
        names(&matches) == strings(&["run_script"]) && note.contains("showing keyword matches"),
        "Rank(nine) = {:?}, note {note:?}, want run_script by keywords",
        names(&matches)
    );
    // Ten take more.
    let (matches, note) = rank(&items, "((((((((((node)))))", &none(), &default_cfg());
    assert!(
        matches.is_empty() && note.starts_with("pattern not run: missing closing )"),
        "Rank(ten) = {:?}, note {note:?}, want no matches and a note that the pattern was not run",
        names(&matches)
    );
}

#[test]
fn test_compile_query_broken_pattern_stays_pattern() {
    let mut queries: Vec<Vec<u8>> = vec![Vec::new()];
    let mut i = 0;
    while i < queries.len() {
        if queries[i].len() < 4 {
            for &c in b"a_.*+?()" {
                let mut q = queries[i].clone();
                q.push(c);
                queries.push(q);
            }
        }
        i += 1;
    }
    for p in &queries {
        if !looks_like_regex(p) || compile_query(p).keyword_fallback {
            continue;
        }
        if go_syntax::parse(p, PERL).is_err() {
            continue;
        }
        let count = |s: &[u8], c: u8| s.iter().filter(|&&b| b == c).count();
        let group_only = !p.iter().any(|c| b"*+?".contains(c)) || p.windows(3).any(|w| w == b"()+");
        for typo in typos(p) {
            if group_only && count(&typo, b')') < count(p, b')') {
                continue;
            }
            assert!(
                !compile_query(&typo).keyword_fallback,
                "compileQuery({:?}) is keywords, want a pattern like {:?}",
                show(&typo),
                show(p)
            );
        }
    }
}

/// Go `typos`: `p` with each typo punctuated keywords also have: a `(`, a `)` or an invalid
/// byte added anywhere, one of its `(` or `)` deleted, a `*`, `+` or `?` at the start or after
/// a `(`, a doubled `*` or `?`, and a trailing backslash.
fn typos(p: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let insert = |out: &mut Vec<Vec<u8>>, i: usize, s: &[u8]| {
        out.push([&p[..i], s, &p[i..]].concat());
    };
    for i in 0..=p.len() {
        for s in [&b"("[..], b")", b"\xff"] {
            insert(&mut out, i, s);
        }
        if i == 0 || p[i - 1] == b'(' {
            for s in [&b"*"[..], b"+", b"?"] {
                insert(&mut out, i, s);
            }
        }
        if i > 0 && (p[i - 1] == b'*' || p[i - 1] == b'?') {
            insert(&mut out, i, &p[i - 1..i]);
        }
        if i < p.len() && (p[i] == b'(' || p[i] == b')') {
            out.push([&p[..i], &p[i + 1..]].concat());
        }
    }
    out.push([p, b"\\"].concat());
    out
}

#[test]
fn test_rank_already_available_name_matches() {
    let available = set(&["get_forecast", "go1.26_tool"]);
    let items = make_items(&[
        ["get_forecast", "returns the forecast"],
        ["weather_alerts", "weather alerts for a forecast region"],
    ]);
    let cases: Vec<(Vec<Item>, &str, Vec<String>, &str)> = vec![
        (
            items.clone(),
            "^get_forecast$",
            vec![],
            "already available: get_forecast",
        ),
        // As a regex, forecast? would match get_forecast and report it as already available,
        // so this row guards the classification.
        (
            items.clone(),
            "forecast?",
            strings(&["weather_alerts"]),
            "showing keyword matches",
        ),
        // Only the literal name match finds this one: BM25 sees "o1" and "2", and the name's
        // tokens are "go1", "26" and "tool".
        (
            make_items(&[["go1.26_tool", "unrelated"]]),
            "o1.2",
            vec![],
            "already available: go1.26_tool",
        ),
        // The same keyword ranks an eligible tool by keywords first.
        (
            make_items(&[
                ["go1.26_tool", "unrelated"],
                ["release_notes", "notes for o1 2"],
            ]),
            "o1.2",
            strings(&["release_notes"]),
            "showing keyword matches",
        ),
    ];
    for (items, query, want, want_in_note) in cases {
        let (matches, note) = rank(&items, query, &available, &default_cfg());
        assert_eq!(names(&matches), want, "Rank({query:?}) names mismatch");
        assert!(
            note.contains(want_in_note),
            "Rank({query:?}) note = {note:?}, want it to contain {want_in_note:?}"
        );
    }
}

#[test]
fn test_rank_keywords_that_parse_as_regex_say_how_to_write_one() {
    let items = make_items(&[["get_forecast", "returns the weather"]]);
    let no_match = no_match_note(&default_cfg());
    for (query, want) in [
        ("fore.ast", "add .* or a character class"),
        ("(forest", no_match.as_str()),
        ("how is fore.ast?", no_match.as_str()),
    ] {
        let (matches, note) = rank(&items, query, &none(), &default_cfg());
        assert!(
            matches.is_empty(),
            "Rank({query:?}) = {:?}, want no matches",
            names(&matches)
        );
        assert!(
            note.contains(want),
            "Rank({query:?}) note = {note:?}, want it to contain {want:?}"
        );
    }
}

// parity: adapted: Go bounds compileQuery at 1000 allocations (testing.AllocsPerRun); Rust cannot count allocations without unsafe code, so the port checks that no matcher was built (the regex was never compiled), that the note says why, and that the call is fast
#[test]
fn test_compile_query_sizes_pattern_before_compiling() {
    let query = format!("{}QQQQQQQ", ".{0,999}".repeat(24));
    let start = Instant::now();
    let q = compile_query(query.as_bytes());
    let elapsed = start.elapsed();
    assert!(
        q.match_name.is_none(),
        "compileQuery() built a matcher for a pattern it does not run"
    );
    assert!(
        q.not_run.contains("too large"),
        "compileQuery() note = {:?}, want it to say too large",
        q.not_run
    );
    assert!(
        elapsed < Duration::from_millis(50),
        "compileQuery() took {elapsed:?}, want it to size the pattern before compiling"
    );
}

/// The fastest of `runs` timed calls of `f`.
fn fastest(runs: usize, mut f: impl FnMut()) -> Duration {
    (0..runs)
        .map(|_| {
            let start = Instant::now();
            f();
            start.elapsed()
        })
        .min()
        .unwrap_or_default()
}

// parity: adapted: Go bounds Rank at 1.5 times the allocations of one regexp.Compile("(?i)" + query); Rust cannot count allocations without unsafe code, so the same 1.5x bound applies to wall time against one regex::Regex::new("(?i)" + query), each side the fastest of three runs
#[test]
fn test_rank_folds_case_at_most_once() {
    let query = "[B-\u{10ffff}]".repeat(10);
    // The names are shorter than the pattern, so nothing matches and Rank goes on to the
    // already-available y_tool.
    let items = make_items(&[["x_tool", "unrelated"], ["y_tool", "unrelated"]]);
    let available = set(&["y_tool"]);
    let compile = fastest(3, || {
        let _ = regex::Regex::new(&format!("(?i){query}"));
    });
    let got = fastest(3, || {
        let _ = rank(&items, &query, &available, &default_cfg());
    });
    assert!(
        got.as_secs_f64() <= 1.5 * compile.as_secs_f64(),
        "Rank() took {got:?}, want at most 1.5 times the {compile:?} of one compile"
    );
}

// parity: adapted: Go's query "(\xdc" is invalid UTF-8, which a Rust &str cannot hold, so it runs through rank_bytes
#[test]
fn test_rank_invalid_utf8_pattern_does_not_panic() {
    let items = make_items(&[["price_(_tool", "returns a price"]]);
    let (matches, note) = rank_bytes(&items, b"(\xdc", &none(), &default_cfg());
    assert!(
        matches.is_empty(),
        "Rank() = {:?}, want no matches",
        names(&matches)
    );
    assert_eq!(note, no_match_note(&default_cfg()), "Rank() note");
}

#[test]
fn test_rank_select_with_no_names_says_nothing_matched() {
    let items = make_items(&[["list_books", "list books"]]);
    for query in ["select:", "select: , "] {
        let (matches, note) = rank(&items, query, &none(), &default_cfg());
        assert!(
            matches.is_empty(),
            "Rank({query:?}) = {:?}, want no matches",
            names(&matches)
        );
        assert_eq!(
            note, "no tools matched; try broader keywords or a different term",
            "Rank({query:?}) note"
        );
    }
}
