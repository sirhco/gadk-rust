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

//! Port of ADK-Go `session/database/compaction_test.go`.

use chrono::{Duration, TimeZone, Utc};
use gadk_genai::{Content, ROLE_MODEL, ROLE_USER};
use gadk_platform::Platform;
use gadk_session::{CreateRequest, Event, EventCompaction, GetRequest, SessionService};
use gadk_session_database::DatabaseSessionService;

use crate::compaction::apply::{known_event_ids, repair_after_append};

#[tokio::test]
async fn test_a_compaction_correction_sorts_after_the_record_it_corrects() {
    let platform = Platform::default();
    let get = |id: String| GetRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        session_id: id,
        ..Default::default()
    };
    for trial in 0..20 {
        let svc = DatabaseSessionService::connect("sqlite::memory:")
            .await
            .unwrap();
        svc.auto_migrate().await.unwrap();
        let sess = svc
            .create(CreateRequest {
                app_name: "app".into(),
                user_id: "user".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let base = Utc.with_ymd_and_hms(2026, 5, 6, 7, 8, 9).unwrap();
        let mut record = Event::new(&platform, "inv-sum");
        record.author = "user".into();
        record.timestamp = base;
        record.actions.compaction = Some(EventCompaction {
            start_timestamp: base - Duration::hours(1),
            end_timestamp: base,
            compacted_content: Some(Content::from_text("summary", ROLE_MODEL)),
            excluded_events: Vec::new(),
        });
        svc.append_event(sess.as_ref(), &mut record).await.unwrap();

        // The straggler: inside the record's range, and not part of what the summary was
        // built from.
        let known = known_event_ids(sess.as_ref());
        let mut late = Event::new(&platform, "inv-late");
        late.author = "user".into();
        late.timestamp = base - Duration::minutes(1);
        late.response.content = Some(Content::from_text("late", ROLE_USER));
        svc.append_event(sess.as_ref(), &mut late).await.unwrap();
        let reread = svc.get(get(sess.id().to_string())).await.unwrap();
        let mut correction = repair_after_append(&record, &known, reread.as_ref()).unwrap_or_else(|| {
            panic!("trial {trial}: repair_after_append() = None, want a correction naming the straggler")
        });
        svc.append_event(sess.as_ref(), &mut correction)
            .await
            .unwrap();

        let got = svc.get(get(sess.id().to_string())).await.unwrap();
        let recs: Vec<Event> = got
            .events()
            .into_iter()
            .filter(|e| e.actions.compaction.is_some())
            .collect();
        assert_eq!(
            recs.len(),
            2,
            "trial {trial}: read back {} records, want 2",
            recs.len()
        );
        let holes = recs[1]
            .actions
            .compaction
            .as_ref()
            .map_or(0, |c| c.excluded_events.len());
        assert_eq!(
            holes, 1,
            "trial {trial}: the record read back last names {holes} holes, want 1: the correction must sort after the record it corrects"
        );
    }
}
