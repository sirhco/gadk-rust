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

//! In-memory word-matching memory service.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gadk_genai::Content;
use gadk_session::{Event, Session, zero_time};
use parking_lot::RwLock;
use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

use crate::service::{
    AddEventsRequest, EventAdder, MemoryEntry, MemoryError, MemoryService, SearchRequest,
    SearchResponse,
};

/// The most entries a search returns.
const MAX_SEARCH_RESULTS: usize = 10;

/// A session bucket within a user scope. The default bucket, for events
/// added with no session ID, never equals the key of a session, even one whose ID is `""`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SessionKey {
    Default,
    Id(String),
}

/// One stored event.
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

/// The memories of one (app, user) scope: session buckets in insertion order.
#[derive(Default)]
struct UserMemories {
    sessions: HashMap<SessionKey, Vec<StoredValue>>,
    session_order: Vec<SessionKey>,
}

impl UserMemories {
    /// Creates bucket `key` if absent. A new bucket
    /// takes the next position in the session order even when nothing is stored in it, as in
    /// Python's dict, and replacing a bucket keeps its position.
    fn ensure_bucket(&mut self, key: &SessionKey) -> &mut Vec<StoredValue> {
        if !self.sessions.contains_key(key) {
            self.session_order.push(key.clone());
        }
        self.sessions.entry(key.clone()).or_default()
    }
}

/// An in-memory implementation of [`MemoryService`]. Thread-safe.
///
/// Searches return at most ten entries, ranked by the number of distinct query words they
/// match. Ties follow session insertion order, then event order.
///
/// It also implements [`EventAdder`]: [`EventAdder::add_events`] appends to the bucket of
/// `req.session_id` and keeps the first stored event of each non-empty event ID in that
/// bucket; [`MemoryService::add_session_to_memory`] replaces the bucket of the session.
///
/// Both add methods skip events whose content is `None` or holds no part; such an event does
/// not reserve its ID. Thought parts are left out of both matching and the returned content.
/// An event with parts but no searchable text, such as a blob-only or thought-only event, is
/// stored and reserves its ID, but no search returns it.
///
/// A query and the stored text are split into words: runs of letters, numbers and
/// underscores, taken after Unicode NFC normalization and lowercased with full Unicode case
/// mapping. An event matches a query word when it has that word. A query word with non-ASCII
/// characters also matches anywhere inside the event's text, and a stored word that mixes Latin
/// and other scripts, such as the Python in 私はPythonを使う, also yields each single-script
/// run as a word. A query with no words returns no entries.
pub struct InMemoryMemoryService {
    /// Keyed by `(app_name, user_id)`.
    store: RwLock<HashMap<(String, String), UserMemories>>,
}

impl InMemoryMemoryService {
    /// A new, empty in-memory memory service.
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

/// The stored form of `event`, or `None` for an event that has no
/// content or no part, which is neither stored nor reserves its ID. An event with parts but no
/// searchable words, such as a blob-only event, is stored so that its ID blocks later
/// duplicates; search skips it.
fn build_value(event: Event) -> Option<StoredValue> {
    let content = event.response.content.filter(|c| !c.parts.is_empty())?;

    // Thought parts are the model's reasoning, not conversation, so they are neither searched
    // nor returned (adk-python v2.12.0 does both; this keeps internal reasoning out of memory
    // results).
    let mut texts = Vec::new();
    let mut visible_parts = Vec::new();
    for part in content.parts {
        if part.thought {
            continue;
        }
        if !part.text.is_empty() {
            texts.push(part.text.clone());
        }
        visible_parts.push(part);
    }
    let text = texts.join(" ");

    Some(StoredValue {
        id: event.id,
        content: Content {
            parts: visible_parts,
            role: content.role,
        },
        author: event.author,
        timestamp: (event.timestamp != zero_time()).then_some(event.timestamp),
        custom_metadata: event.response.custom_metadata,
        words: extract_searchable_words(&text),
        text_lower: text.nfc().collect::<String>().to_lowercase(),
    })
}

#[async_trait]
impl MemoryService for InMemoryMemoryService {
    /// Replaces the session's bucket with the session's events.
    async fn add_session_to_memory(&self, session: &dyn Session) -> Result<(), MemoryError> {
        let values: Vec<StoredValue> = session
            .events()
            .into_iter()
            .filter_map(build_value)
            .collect();

        let key = (session.app_name().to_owned(), session.user_id().to_owned());
        let bucket = SessionKey::Id(session.id().to_owned());
        let mut store = self.store.write();
        *store.entry(key).or_default().ensure_bucket(&bucket) = values;
        Ok(())
    }

    /// Returns up to ten entries, ranked by the number of distinct query words matched.
    async fn search_memory(&self, req: SearchRequest) -> Result<SearchResponse, MemoryError> {
        // A true value allows substring matching for that non-ASCII query word.
        let query_words: HashMap<String, bool> = extract_words_lower(&req.query)
            .into_iter()
            .map(|w| {
                let non_ascii = !w.is_ascii();
                (w, non_ascii)
            })
            .collect();

        if query_words.is_empty() {
            return Ok(SearchResponse::default());
        }

        // Hold the read lock for the whole scan (so the scan does not race a concurrent write).
        let store = self.store.read();
        let Some(memories) = store.get(&(req.app_name, req.user_id)) else {
            return Ok(SearchResponse::default());
        };

        let mut scored: Vec<(usize, MemoryEntry)> = Vec::new();
        for key in &memories.session_order {
            let Some(values) = memories.sessions.get(key) else {
                continue;
            };
            for v in values {
                if v.words.is_empty() {
                    continue;
                }
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

    fn as_event_adder(&self) -> Option<&dyn EventAdder> {
        Some(self)
    }
}

#[async_trait]
impl EventAdder for InMemoryMemoryService {
    /// Appends to the bucket of `req.session_id`
    /// (the default bucket when it is empty), skipping an event whose non-empty ID the bucket
    /// already holds. `req.custom_metadata` is ignored.
    async fn add_events(&self, req: AddEventsRequest) -> Result<(), MemoryError> {
        let values: Vec<StoredValue> = req.events.into_iter().filter_map(build_value).collect();
        let bucket = if req.session_id.is_empty() {
            SessionKey::Default
        } else {
            SessionKey::Id(req.session_id)
        };

        let mut store = self.store.write();
        let existing = store
            .entry((req.app_name, req.user_id))
            .or_default()
            .ensure_bucket(&bucket);
        let mut existing_ids: HashSet<String> = existing
            .iter()
            .filter(|e| !e.id.is_empty())
            .map(|e| e.id.clone())
            .collect();
        for value in values {
            if !value.id.is_empty() && !existing_ids.insert(value.id.clone()) {
                continue;
            }
            existing.push(value);
        }
        Ok(())
    }
}

/// The lowercased `\w+` tokens of the NFC form of `text`, as
/// adk-python's `_extract_words_lower` does. Combining marks are not word characters, so after
/// NFC a mark that composes nowhere separates words.
///
/// `to_lowercase` is full Unicode case mapping.
fn extract_words_lower(text: &str) -> HashSet<String> {
    let nfc: String = text.nfc().collect();
    nfc.split(|c: char| !is_word_char(c))
        .filter(|piece| !piece.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The words an event can be matched on, the tokens of
/// [`extract_words_lower`] plus, for each non-ASCII token, its runs of Latin and of non-Latin
/// characters. Japanese and Chinese are written without spaces, so in 私はPythonを使う the run
/// `python` becomes a word of its own.
fn extract_searchable_words(text: &str) -> HashSet<String> {
    let mut words = extract_words_lower(text);
    let mut runs = Vec::new();
    for word in words.iter().filter(|w| !w.is_ascii()) {
        let (mut start, mut run_latin) = (0, false);
        for (i, c) in word.char_indices() {
            let latin = is_latin(c);
            if i > 0 && latin != run_latin {
                runs.push(word[start..i].to_owned());
                start = i;
            }
            run_latin = latin;
        }
        runs.push(word[start..].to_owned());
    }
    words.extend(runs);
    words
}

/// Whether `c` is a word character: a letter, a number or `_`.
///
/// The Unicode version of `unicode-properties` may differ from other Unicode releases for newly
/// assigned code points only.
fn is_word_char(c: char) -> bool {
    c == '_'
        || matches!(
            c.general_category_group(),
            GeneralCategoryGroup::Letter | GeneralCategoryGroup::Number
        )
}

/// Whether `c` belongs to a Latin run, as adk-python's `_is_latin` does:
/// an ASCII word character, or a character whose Unicode name starts with `LATIN`. The name rule
/// leaves out ª and º, which Unicode's Latin script includes.
fn is_latin(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_';
    }
    unicode_names2::name(c).is_some_and(|name| name.to_string().starts_with("LATIN"))
}
