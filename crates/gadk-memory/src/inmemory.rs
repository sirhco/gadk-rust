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

//! In-memory word-matching memory service (port of Go memory/inmemory.go).

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_genai::Content;
use gadk_session::{Session, zero_time};
use parking_lot::RwLock;
use serde_json::{Map, Value};
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

use crate::service::{MemoryEntry, MemoryError, MemoryService, SearchRequest, SearchResponse};

/// Port of Go maxSearchResults.
const MAX_SEARCH_RESULTS: usize = 10;

/// Port of Go value: one stored event.
struct StoredValue {
    id: String,
    content: Content,
    author: String,
    timestamp: Option<DateTime<Utc>>,
    custom_metadata: Map<String, Value>,
    /// Precomputed set of words in the content for simple keyword matching.
    words: HashSet<String>,
    /// Cached lowercase copy of the event text, to avoid joining and lowercasing content parts
    /// on each substring search.
    text_lower: String,
}

/// Port of Go userMemories.
#[derive(Default)]
struct UserMemories {
    sessions: HashMap<String, Vec<StoredValue>>,
    session_order: Vec<String>,
}

/// Port of Go inMemoryService: an in-memory implementation of [`MemoryService`]. Thread-safe.
///
/// Searches return at most ten entries, ranked by the number of distinct query words they
/// match. Ties follow session insertion order, then event order.
pub struct InMemoryMemoryService {
    /// Keyed by `(app_name, user_id)`.
    store: RwLock<HashMap<(String, String), UserMemories>>,
}

impl InMemoryMemoryService {
    /// Port of Go InMemoryService: a new, empty in-memory memory service.
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryMemoryService {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for InMemoryMemoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InMemoryMemoryService")
    }
}

#[async_trait]
impl MemoryService for InMemoryMemoryService {
    /// Port of Go inMemoryService.AddSessionToMemory.
    async fn add_session_to_memory(&self, session: &dyn Session) -> Result<(), MemoryError> {
        let mut values = Vec::new();

        for event in session.events() {
            let Some(content) = event.response.content else {
                continue;
            };

            // Thought parts are the model's reasoning, not conversation, so they are neither
            // searched nor returned, as in adk-python.
            let mut words = HashSet::new();
            let mut texts = Vec::new();
            let mut visible_parts = Vec::new();
            for part in content.parts {
                if part.thought {
                    continue;
                }
                if !part.text.is_empty() {
                    words.extend(extract_words(&part.text));
                    texts.push(part.text.clone());
                }
                visible_parts.push(part);
            }

            if words.is_empty() {
                continue;
            }

            values.push(StoredValue {
                id: event.id,
                content: Content {
                    parts: visible_parts,
                    ..content
                },
                author: event.author,
                timestamp: (event.timestamp != zero_time()).then_some(event.timestamp),
                custom_metadata: event.response.custom_metadata,
                words,
                text_lower: texts.join(" ").to_lowercase(),
            });
        }

        let key = (session.app_name().to_owned(), session.user_id().to_owned());
        let mut store = self.store.write();
        let memories = store.entry(key).or_default();
        let sid = session.id().to_owned();
        // Replacing a session must preserve its position, as in Python's dict.
        if !memories.sessions.contains_key(&sid) {
            memories.session_order.push(sid.clone());
        }
        memories.sessions.insert(sid, values);
        Ok(())
    }

    /// Port of Go inMemoryService.SearchMemory.
    async fn search_memory(&self, req: SearchRequest) -> Result<SearchResponse, MemoryError> {
        // A true value allows substring matching for that non-ASCII query word.
        let query_words: HashMap<String, bool> = extract_words(&req.query)
            .into_iter()
            .map(|w| {
                let non_ascii = !w.is_ascii();
                (w, non_ascii)
            })
            .collect();

        if query_words.is_empty() {
            return Ok(SearchResponse::default());
        }

        // Hold the read lock for the whole scan (Go does so to avoid racing a concurrent write).
        let store = self.store.read();
        let Some(memories) = store.get(&(req.app_name, req.user_id)) else {
            return Ok(SearchResponse::default());
        };

        let mut scored: Vec<(usize, MemoryEntry)> = Vec::new();
        for sid in &memories.session_order {
            let Some(values) = memories.sessions.get(sid) else {
                continue;
            };
            for v in values {
                let score = query_words
                    .iter()
                    .filter(|(word, allow_substring)| {
                        v.words.contains(*word)
                            || (**allow_substring && v.text_lower.contains(*word))
                    })
                    .count();
                if score > 0 {
                    scored.push((
                        score,
                        MemoryEntry {
                            id: v.id.clone(),
                            content: Some(v.content.clone()),
                            author: v.author.clone(),
                            timestamp: v.timestamp,
                            custom_metadata: v.custom_metadata.clone(),
                        },
                    ));
                }
            }
        }
        drop(store);

        // Common words can match most of the store. Rank all matches before limiting results so
        // that later, more relevant events can still reach the prompt. The sort is stable, so
        // ties keep insertion order.
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored.truncate(MAX_SEARCH_RESULTS);

        Ok(SearchResponse {
            memories: scored.into_iter().map(|(_, entry)| entry).collect(),
        })
    }
}

/// Port of Go extractWords: the set of lowercased words in `text`.
///
/// `to_lowercase` differs from Go's `strings.ToLower` only for special-casing chars such as
/// the final sigma, which Rust lowercases context-sensitively.
fn extract_words(text: &str) -> HashSet<String> {
    text.split(|c: char| !is_word_char(c))
        // A mark with no letter or number before it, such as the emoji variation selector
        // U+FE0F, does not belong to a word.
        .map(|piece| piece.trim_start_matches(is_mark))
        .filter(|piece| !piece.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Port of Go extractWords' split predicate: unicode.IsLetter || unicode.IsNumber ||
/// unicode.IsMark || '_'.
///
/// The Unicode version of `unicode-properties` may differ from Go's `unicode` tables for newly
/// assigned code points only.
fn is_word_char(c: char) -> bool {
    c == '_'
        || matches!(
            c.general_category_group(),
            GeneralCategoryGroup::Letter
                | GeneralCategoryGroup::Number
                | GeneralCategoryGroup::Mark
        )
}

/// Go unicode.IsMark (general category M = Mn | Mc | Me).
fn is_mark(c: char) -> bool {
    c.general_category_group() == GeneralCategoryGroup::Mark
}
