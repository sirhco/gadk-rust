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

//! A generic BM25 ranked search over named items, with exact selection, regex matching, a
//! relevance floor, result limits, and optional name-list advertisement. Port of ADK-Go
//! `internal/ranksearch/ranksearch.go`.
//!
//! Go strings may hold invalid UTF-8, and Go's classification treats such bytes specially, so the
//! core works on byte strings ([`rank_bytes`], [`compile_query`]); [`rank`] is the `&str` entry
//! point every caller uses.

pub(crate) mod bm25;
pub(crate) mod go_syntax;

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use regex::bytes::Regex as BytesRegex;

use bm25::{rank_bm25, tokenize};
use go_syntax::{Error as SyntaxError, ErrorCode, Op, PERL, Regexp};

/// Caps the query length in runes (Go `maxQueryLen`).
pub(crate) const MAX_QUERY_LEN: usize = 200;
/// Caps the estimated compiled size of a regex query (Go `maxPatternSize`).
const MAX_PATTERN_SIZE: i64 = 256;
/// Marks the exact-name query form: `select:name1,name2` (Go `selectPrefix`).
const SELECT_PREFIX: &[u8] = b"select:";
/// How many times an item's name tokens are repeated in its token bag (Go `nameRepeat`).
const NAME_REPEAT: usize = 3;
/// Bounds the search in [`any_balancing`] (Go `maxBalancingSteps`).
const MAX_BALANCING_STEPS: usize = 256;

/// A searchable named entity: a tool or a skill. `tokens` is the pre-built BM25 token bag (use
/// [`build_tokens`]). Go `Item`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Item {
    pub name: String,
    pub description: String,
    pub tokens: Vec<String>,
}

/// A ranked result: name and (possibly truncated) description. Go `Match`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct Match {
    pub name: String,
    pub description: String,
}

/// Tunes a [`rank`] call. Go `Config`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Config {
    /// The singular word used in notes ("tool", "skill"); plurals append "s".
    pub item_noun: String,
    /// Caps fuzzy/regex matches. Zero or negative means no cap. The `select:` form is never
    /// capped.
    pub max_results: i32,
    /// Drops BM25 matches scoring below this fraction of the top match's score.
    pub min_score_ratio: f64,
    /// Truncates returned descriptions to this many bytes (rune-safe). Zero or negative leaves
    /// descriptions untouched.
    pub max_desc_len: i32,
}

impl Config {
    /// `max_results` as a cap, if it is one.
    fn cap(&self) -> Option<usize> {
        usize::try_from(self.max_results).ok().filter(|&n| n > 0)
    }
}

/// Dispatches on the query form and returns ranked matches plus a UX note (empty for none).
/// Items whose name is in `already_available` are excluded. Port of Go `Rank`; see its doc
/// comment for the query forms.
pub(crate) fn rank(
    items: &[Item],
    query: &str,
    already_available: &HashSet<String>,
    cfg: &Config,
) -> (Vec<Match>, String) {
    rank_bytes(items, query.as_bytes(), already_available, cfg)
}

/// [`rank`] on a Go byte string, which may hold invalid UTF-8.
pub(crate) fn rank_bytes(
    items: &[Item],
    query: &[u8],
    already_available: &HashSet<String>,
    cfg: &Config,
) -> (Vec<Match>, String) {
    let query = go_syntax::trim_space(query);
    if query.is_empty() {
        return (
            Vec::new(),
            format!("provide a keyword to search for {}s", cfg.item_noun),
        );
    }
    if go_syntax::rune_count(query) > MAX_QUERY_LEN {
        return (
            Vec::new(),
            format!(
                "query too long (max {MAX_QUERY_LEN} characters) — use a shorter keyword or pattern"
            ),
        );
    }

    if let Some(rest) = query.strip_prefix(SELECT_PREFIX) {
        return select_by_name(items, rest, already_available, cfg);
    }

    let q = compile_query(query);
    if !q.not_run.is_empty() {
        return (Vec::new(), q.not_run);
    }

    // BM25 sees the query as Go's range loop does: each invalid byte is a RuneError, which is
    // a separator.
    let words = go_syntax::to_valid_utf8(query, "\u{FFFD}");
    let (available, eligible): (Vec<&Item>, Vec<&Item>) = items
        .iter()
        .partition(|it| already_available.contains(&it.name));

    let mut all: Vec<Match>;
    let mut note = String::new();
    if q.has_meta {
        all = q.name_matches(&eligible, cfg);
        if all.is_empty() && !q.keyword_fallback {
            let had = q.name_matches(&available, cfg);
            if !had.is_empty() {
                return (Vec::new(), already_available_note(&had, cfg));
            }
            return (
                Vec::new(),
                format!(
                    "no {} names match this pattern — patterns match names only, \
                     so search by keywords to match descriptions",
                    cfg.item_noun
                ),
            );
        }
        if all.is_empty() {
            all = bm25_matches(&eligible, &words, cfg);
            note = format!(
                "no new {} names contain the query, showing keyword matches",
                cfg.item_noun
            );
        }
    } else {
        all = bm25_matches(&eligible, &words, cfg);
    }
    if all.is_empty() {
        let mut had = q.name_matches(&available, cfg);
        if had.is_empty() {
            had = bm25_matches(&available, &words, cfg);
        }
        if !had.is_empty() {
            return (Vec::new(), already_available_note(&had, cfg));
        }
        if q.regex_hint {
            return (
                Vec::new(),
                format!(
                    "no {}s matched as keywords — to match {} names as a pattern, \
                     add .* or a character class",
                    cfg.item_noun, cfg.item_noun
                ),
            );
        }
        return (Vec::new(), no_match_note(cfg));
    }

    if let Some(max) = cfg.cap()
        && all.len() > max
    {
        let mut capped = format!(
            "showing {max} of {} matches — use a more specific keyword to narrow results",
            all.len()
        );
        if !note.is_empty() {
            capped = format!("{note}. {capped}");
        }
        all.truncate(max);
        return (all, capped);
    }
    (all, note)
}

/// A query prepared once per [`rank`] call (Go `compiledQuery`).
pub(crate) struct CompiledQuery {
    /// Set for a query with regex metacharacters, which is matched against names first.
    pub(crate) has_meta: bool,
    /// Matches an item name: as a regex for a pattern, and as a literal substring for
    /// punctuated keywords or prose. `None` for a query without metacharacters and for a
    /// pattern that is not run.
    pub(crate) match_name: Option<Regex>,
    /// Ranks the query by BM25 when no new name contains it.
    pub(crate) keyword_fallback: bool,
    /// The note explaining why a pattern was not run, if it was not (empty otherwise).
    pub(crate) not_run: String,
    /// Set for punctuated keywords that would also parse as a regex, such as `fore.ast`.
    pub(crate) regex_hint: bool,
}

impl CompiledQuery {
    /// Go `nameMatches`: the items whose name the query matches, sorted by name.
    fn name_matches(&self, items: &[&Item], cfg: &Config) -> Vec<Match> {
        let Some(re) = &self.match_name else {
            return Vec::new();
        };
        let mut matches: Vec<Match> = items
            .iter()
            .filter(|it| re.is_match(&it.name))
            .map(|it| to_match(it, cfg))
            .collect();
        matches.sort_by(|a, b| a.name.cmp(&b.name));
        matches
    }
}

/// Go `compileQuery`: prepares a query with regex metacharacters. See the Go doc comment for
/// the classification.
pub(crate) fn compile_query(query: &[u8]) -> CompiledQuery {
    let mut q = CompiledQuery {
        has_meta: looks_like_regex(query),
        match_name: None,
        keyword_fallback: false,
        not_run: String::new(),
        regex_hint: false,
    };
    if !q.has_meta {
        return q;
    }
    let prose = go_syntax::contains_space(query);
    // An invalid byte would hide a marker, as in .\xff+.
    let markers = has_pattern_marker(go_syntax::to_valid_utf8(query, "").as_bytes());
    // Size a parse without the (?i) the compile adds (see Go's comment).
    let parsed = go_syntax::parse(query, PERL);
    let is_pattern = markers || repeats_once_parsed(query, &parsed);
    match &parsed {
        Err(perr) => {
            if is_pattern && !prose {
                q.not_run = not_run_note(perr);
            }
        }
        Ok(re) => {
            if !prose && pattern_size(re) > MAX_PATTERN_SIZE {
                q.not_run = "pattern not run: counted repeats such as {1,128} make it too large — \
                             use .*, a smaller count, or keywords"
                    .to_string();
            }
        }
    }
    q.keyword_fallback = prose || !is_pattern;
    q.regex_hint = !is_pattern && !prose && parsed.is_ok();
    if q.keyword_fallback {
        // Go QuoteMeta keeps invalid UTF-8, which regexp rejects even in a literal.
        let literal = regex::escape(&go_syntax::to_valid_utf8(query, "\u{FFFD}"));
        q.match_name = Regex::new(&format!("(?i){literal}")).ok();
    } else if q.not_run.is_empty() {
        // A query that parses also parses with (?i) in front. Matching follows Go's parse of
        // it, so Go-only syntax such as \Q...\E and {,n} means what it means in Go.
        let folded = [b"(?i)".as_slice(), query].concat();
        match go_syntax::parse(&folded, PERL) {
            Ok(re) => match Regex::new(&go_syntax::to_hir(&re).to_string()) {
                Ok(re) => q.match_name = Some(re),
                // The regex crate's size limit is the only way here; Go would compile it.
                Err(_) => {
                    q.not_run = not_run_note(&SyntaxError {
                        code: ErrorCode::Large,
                        expr: Vec::new(),
                    });
                }
            },
            Err(perr) => q.not_run = not_run_note(&perr),
        }
    }
    q
}

/// Go `notRunNote`: explains why a pattern that does not parse was not run.
fn not_run_note(perr: &SyntaxError) -> String {
    if perr.expr.is_empty() {
        return format!(
            "pattern not run: {} — use a simpler pattern or keywords",
            perr.code
        );
    }
    format!(
        "pattern not run: {}: `{}` — use a simpler pattern or keywords",
        perr.code,
        go_syntax::to_valid_utf8(&perr.expr, "\u{FFFD}")
    )
}

/// Go `patternMarkers`: syntax that keywords rarely contain but patterns do (see the Go doc
/// comment). Go's `\d` and `\w` are ASCII, so they are spelled as ASCII classes here.
static PATTERN_MARKERS: LazyLock<Option<BytesRegex>> = LazyLock::new(|| {
    BytesRegex::new(r"[\[|^$]|\(\?|\(\*|\\.|\.\+|[*?]\+|\{,?[0-9]|_.*[?+]|\)[0-9A-Za-z_]").ok()
});

fn has_pattern_marker(text: &[u8]) -> bool {
    PATTERN_MARKERS.as_ref().is_some_and(|re| re.is_match(text))
}

/// Go `repeatsOnceParsed`: whether a query with none of the pattern markers is a pattern by
/// its parse tree, fixing a query that does not parse first (see the Go doc comment).
fn repeats_once_parsed(query: &[u8], parsed: &Result<Regexp, SyntaxError>) -> bool {
    if let Ok(re) = parsed {
        return uses_repeat(re);
    }
    let valid = go_syntax::to_valid_utf8(query, "");
    let b = valid.as_bytes();
    let b = b.strip_suffix(b"\\").unwrap_or(b);
    let (opens, closes) = unbalanced(b);
    let closed = [
        "(".repeat(closes).as_bytes(),
        b,
        ")".repeat(opens).as_bytes(),
    ]
    .concat();
    balanced_is_pattern(&closed) || any_balancing(b, opens, closes, balanced_is_pattern)
}

/// Go `balancedIsPattern`: whether `b`, with balanced parentheses, is a pattern once its stray
/// repeats are deleted, or still does not parse.
fn balanced_is_pattern(b: &[u8]) -> bool {
    let text = delete_stray_repeats(b);
    if has_pattern_marker(&text) {
        return true;
    }
    match go_syntax::parse(&text, PERL) {
        Ok(re) => uses_repeat(&re),
        Err(_) => true,
    }
}

/// Go `unbalanced`: how many of the `(` and how many of the `)` in `b` have no partner.
fn unbalanced(b: &[u8]) -> (usize, usize) {
    let (mut opens, mut closes) = (0, 0);
    for &c in b {
        match c {
            b'(' => opens += 1,
            b')' if opens == 0 => closes += 1,
            b')' => opens -= 1,
            _ => {}
        }
    }
    (opens, closes)
}

/// Go `anyBalancing`: whether `f` returns true for `b` with any fewest set of parentheses
/// deleted that balances it. Reports true once the search takes more than
/// [`MAX_BALANCING_STEPS`] steps.
fn any_balancing(b: &[u8], opens: usize, closes: usize, f: fn(&[u8]) -> bool) -> bool {
    struct Walk<'a> {
        b: &'a [u8],
        kept: Vec<u8>,
        steps: usize,
        f: fn(&[u8]) -> bool,
    }
    fn walk(w: &mut Walk<'_>, i: usize, depth: usize, opens: usize, closes: usize) -> bool {
        w.steps += 1;
        if w.steps > MAX_BALANCING_STEPS {
            return true;
        }
        let n = w.kept.len();
        let found = walk_from(w, i, depth, opens, closes);
        w.kept.truncate(n);
        found
    }
    fn walk_from(w: &mut Walk<'_>, i: usize, depth: usize, opens: usize, closes: usize) -> bool {
        let mut depth = depth;
        for i in i..w.b.len() {
            let c = w.b[i];
            if c == b'(' && opens > 0 && walk(w, i + 1, depth, opens - 1, closes)
                || c == b')' && closes > 0 && walk(w, i + 1, depth, opens, closes - 1)
            {
                return true;
            }
            match c {
                b'(' => depth += 1,
                b')' if depth == 0 => return false,
                b')' => depth -= 1,
                _ => {}
            }
            w.kept.push(c);
        }
        depth == 0 && (w.f)(&w.kept)
    }
    let mut w = Walk {
        b,
        kept: Vec::with_capacity(b.len()),
        steps: 0,
        f,
    };
    walk(&mut w, 0, 0, opens, closes)
}

/// Go `deleteStrayRepeats`: deletes each `++`, then each run of `*`, `+` and `?` with nothing to
/// repeat, at the start or after a `(` (keeping a `?` of a run after a `(`).
fn delete_stray_repeats(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut run = Vec::new();
    for &c in b {
        if is_repeat_op(c) {
            run.push(c);
            continue;
        }
        kept_of_run(&mut out, &run);
        out.push(c);
        run.clear();
    }
    kept_of_run(&mut out, &run);
    out
}

/// Go `keptOfRun`: appends to `out` what [`delete_stray_repeats`] keeps of `run`.
fn kept_of_run(out: &mut Vec<u8>, run: &[u8]) {
    // bytes.ReplaceAll(run, "++", nil): non-overlapping, left to right.
    let mut kept = Vec::with_capacity(run.len());
    let mut i = 0;
    while i < run.len() {
        if run[i..].starts_with(b"++") {
            i += 2;
        } else {
            kept.push(run[i]);
            i += 1;
        }
    }
    match out.last() {
        None => {}
        Some(b'(') => {
            if kept.contains(&b'?') {
                out.push(b'?');
            }
        }
        Some(_) => out.extend_from_slice(&kept),
    }
}

fn is_repeat_op(c: u8) -> bool {
    c == b'*' || c == b'+' || c == b'?'
}

/// Go `looksLikeRegex`: whether the query contains regex metacharacters.
pub(crate) fn looks_like_regex(query: &[u8]) -> bool {
    query.iter().any(|c| b".*+?[]()|^$\\{}".contains(c))
}

/// Go `usesRepeat`: whether `re` uses a repeat other than a trailing `?` or `+` on a literal.
fn uses_repeat(re: &Regexp) -> bool {
    if is_trailing_punctuation(re) {
        return false;
    }
    if re.op == Op::Concat
        && let Some((last, rest)) = re.sub.split_last()
        && is_trailing_punctuation(last)
    {
        return rest.iter().any(has_repeat);
    }
    has_repeat(re)
}

/// Go `isTrailingPunctuation`: whether `re` is a `?` or `+` on a literal.
fn is_trailing_punctuation(re: &Regexp) -> bool {
    matches!(re.op, Op::Quest | Op::Plus) && re.sub.first().is_some_and(|s| s.op == Op::Literal)
}

/// Go `hasRepeat`: whether `re` or any part of it is a `*`, `+` or `?` repeat.
fn has_repeat(re: &Regexp) -> bool {
    matches!(re.op, Op::Star | Op::Plus | Op::Quest) || re.sub.iter().any(has_repeat)
}

/// Go `patternSize`: estimates how many instructions `re` compiles to, stopping once the
/// estimate passes [`MAX_PATTERN_SIZE`]. A counted repeat multiplies the size of its operand.
fn pattern_size(re: &Regexp) -> i64 {
    let mut n: i64 = 1;
    if re.op == Op::Literal {
        n = i64::try_from(re.rune.len()).unwrap_or(i64::MAX);
    }
    for sub in &re.sub {
        n += pattern_size(sub);
        if n > MAX_PATTERN_SIZE {
            return n;
        }
    }
    if re.op == Op::Repeat {
        n *= i64::from(re.min.max(re.max).max(1));
    }
    n
}

/// Go `noMatchNote`.
pub(crate) fn no_match_note(cfg: &Config) -> String {
    format!(
        "no {}s matched; try broader keywords or a different term",
        cfg.item_noun
    )
}

/// Go `alreadyAvailableNote`: the query matched, but only items the caller can already use.
fn already_available_note(had: &[Match], cfg: &Config) -> String {
    let mut names: Vec<&str> = had.iter().map(|m| m.name.as_str()).collect();
    if let Some(max) = cfg.cap() {
        names.truncate(max);
    }
    format!(
        "no new {}s matched; these matching {}s are already available: {}",
        cfg.item_noun,
        cfg.item_noun,
        names.join(", ")
    )
}

/// Go `selectByName`: loads items by exact name, skipping duplicates, and reporting unknown and
/// already-available names.
fn select_by_name(
    items: &[Item],
    names: &[u8],
    already_available: &HashSet<String>,
    cfg: &Config,
) -> (Vec<Match>, String) {
    let mut matches = Vec::new();
    let mut not_found: Vec<String> = Vec::new();
    let mut had: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for raw in names.split(|&c| c == b',') {
        let name = go_syntax::to_valid_utf8(go_syntax::trim_space(raw), "\u{FFFD}");
        if name.is_empty() || seen.contains(&name) {
            continue;
        }
        seen.insert(name.clone());
        if already_available.contains(&name) {
            had.push(name);
            continue;
        }
        // Go's map keeps the last item of a name.
        match items.iter().rev().find(|it| it.name == name) {
            Some(it) => matches.push(to_match(it, cfg)),
            None => not_found.push(name),
        }
    }

    let mut notes = Vec::new();
    if !not_found.is_empty() {
        notes.push(format!(
            "{}s not found: {}",
            cfg.item_noun,
            not_found.join(", ")
        ));
    }
    if !had.is_empty() {
        notes.push(format!("already available: {}", had.join(", ")));
    }
    if matches.is_empty() && notes.is_empty() {
        notes.push(no_match_note(cfg));
    }
    (matches, notes.join(". "))
}

/// Go `bm25Matches`: ranks eligible items by BM25 and trims the tail below `min_score_ratio`
/// of the top score.
fn bm25_matches(eligible: &[&Item], query: &str, cfg: &Config) -> Vec<Match> {
    let docs: Vec<&[String]> = eligible.iter().map(|it| it.tokens.as_slice()).collect();
    let ranked = rank_bm25(query, &docs);
    let Some(top) = ranked.first() else {
        return Vec::new();
    };
    let floor = top.score * cfg.min_score_ratio;
    ranked
        .iter()
        .take_while(|r| r.score >= floor)
        .filter_map(|r| eligible.get(r.index))
        .map(|it| to_match(it, cfg))
        .collect()
}

fn to_match(it: &Item, cfg: &Config) -> Match {
    Match {
        name: it.name.clone(),
        description: truncate_runes(&it.description, cfg.max_desc_len).to_string(),
    }
}

/// Builds an item's BM25 token bag: its name (repeated to weight name matches highest), its
/// description, and any extra strings (e.g. argument names and descriptions). Go `BuildTokens`.
pub(crate) fn build_tokens(name: &str, description: &str, extra: &[&str]) -> Vec<String> {
    let name_toks = tokenize(name);
    let mut toks = Vec::with_capacity(name_toks.len() * NAME_REPEAT + 8);
    for _ in 0..NAME_REPEAT {
        toks.extend(name_toks.iter().cloned());
    }
    toks.extend(tokenize(description));
    for e in extra {
        toks.extend(tokenize(e));
    }
    toks
}

/// Appends the sorted, deduplicated item names to a search tool's base description. Returns
/// `base` unchanged when there are no names. Go `DescribeSearch`.
pub(crate) fn describe_search(base: &str, item_names: &[String]) -> String {
    if item_names.is_empty() {
        return base.to_string();
    }
    let mut names: Vec<&str> = item_names.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    format!(
        "{base}\n\nAvailable to load (not yet active — search or select: by name): {}",
        names.join(", ")
    )
}

/// Go `truncateRunes`: caps `s` at `max_bytes` without splitting a multi-byte rune. A
/// non-positive `max_bytes` leaves `s` untouched.
fn truncate_runes(s: &str, max_bytes: i32) -> &str {
    let Ok(max) = usize::try_from(max_bytes) else {
        return s;
    };
    if max == 0 || s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn pattern_markers_compile() {
        assert!(PATTERN_MARKERS.is_some());
        assert!(has_pattern_marker(b"get_.*_file?"));
        assert!(!has_pattern_marker(b"node.js"));
    }

    #[test]
    fn truncate_runes_backs_off_to_a_rune_start() {
        assert_eq!(truncate_runes("ab€", 3), "ab");
        assert_eq!(truncate_runes("ab€", 5), "ab€");
        assert_eq!(truncate_runes("ab€", 0), "ab€");
        assert_eq!(truncate_runes("ab€", -1), "ab€");
    }
}
