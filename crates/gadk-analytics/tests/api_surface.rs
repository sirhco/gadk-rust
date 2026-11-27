// Copyright 2026 Chris Olson
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

//! The public API (R2–R9): every listed function is coerced to a `fn` pointer of its signature
//! or called, and every listed constant and type is named, so a change to any of them fails to
//! compile here.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use gadk_analytics::cost::{
    CostSum, LABEL_CACHED, LABEL_INPUT, LABEL_MODEL, LABEL_OPERATION, LABEL_OUTPUT,
    LABEL_REASONING, OP_GENERATE_CONTENT, UNKNOWN_MODEL, Usage, cost_of,
};
use gadk_analytics::prompts::{
    FEEDBACK_CAP, FeedbackRow, InvocationVersions, VersionFeedback, VersionMetrics, VersionModel,
    VersionStats,
};
use gadk_analytics::review::{Cursor, JudgeScore, PendingTrace, REVIEW_CAP, ReviewQueue, at_text};
use gadk_analytics::row::{
    BQ_BYTES_HINT, BQ_ROLES_HINT, from_bigquery, param, text, timestamp, ts_param,
};
use gadk_analytics::scores::{
    BQ_EDITOR_HINT, DML_MIN_BYTES, NewScore, NewValue, ScoreRow, TRACE_SCORES_CAP, TraceRoot,
    TraceScores,
};
use gadk_analytics::sessions::{
    CSV_CAP, CSV_ROW_CAP, INVALID_CURSOR, MAX_ID_LEN, ROW_CAP, SESSION_CAP, SessionCursor,
    SessionPage, SessionQuery, SessionRow, TURN_CAP, TurnRow, TurnScore, USER_CAP, UserPage,
    UserQuery, UserRow, is_session_id,
};
use gadk_analytics::testutil::FakeHttp;
use gadk_analytics::usage::{
    BREAKDOWN_CAP, Breakdown, Cell, ModelCalls, TOP_TRACES, TREND_CAP, TopTrace, Trend, TrendCell,
    UsageRange,
};
use gadk_analytics::{AnalyticsSource, BigQueryTable, Service, SourceError, TimeRange, TraceId};
use gadk_bigquery::{BigQueryClient, BigQueryError, NoAuth};
use serde_json::{Map, Value};

type Row = Map<String, Value>;

#[test]
fn the_shared_types_constructors_and_row_helpers() {
    let _: fn(BigQueryClient, String, BigQueryTable, i64) -> AnalyticsSource = AnalyticsSource::new;
    let _: fn(&AnalyticsSource) -> &BigQueryTable = AnalyticsSource::table;
    let _: fn(String, String, String) -> BigQueryTable = BigQueryTable::new;
    let _: fn(&BigQueryTable) -> Result<String, SourceError> = BigQueryTable::sql_path;
    let _: fn(Option<u16>, Option<String>, String) -> SourceError = SourceError::bigquery;
    let _: fn(&SourceError) -> Option<&'static str> = SourceError::hint;
    let _: [Service; 2] = [Service::Trace, Service::Logging];
    let _: fn(&str) -> Option<TimeRange> = TimeRange::parse;
    let _: fn(&str) -> Option<TraceId> = TraceId::parse;
    let _: fn(BigQueryError) -> SourceError = from_bigquery;
    let _: fn(&str, &str, Option<String>) -> Value = param;
    let _: fn(DateTime<Utc>) -> String = ts_param;
    let _: fn(&Row, &str) -> Option<String> = text;
    let _: fn(&Row, &str) -> Result<DateTime<Utc>, SourceError> = timestamp;
    let _: [&str; 2] = [BQ_ROLES_HINT, BQ_BYTES_HINT];
}

#[test]
fn the_cost_module() {
    let _: fn(u64, u64, u64, u64) -> Usage = Usage::new;
    let _: fn(&BTreeMap<String, String>) -> Usage = Usage::from_labels;
    let _: fn(Option<&gadk_eval_monitor::PriceTable>, &str, &Usage) -> Option<f64> = cost_of;
    // The price types are re-exported, so `cost_of`'s argument can be named from this crate.
    let _: fn(Option<&gadk_analytics::cost::PriceTable>, &str, &Usage) -> Option<f64> = cost_of;
    let _: Option<gadk_analytics::cost::ModelPrice> = None;
    let _: fn(&mut CostSum, Option<f64>) = CostSum::add;
    let _: fn(&CostSum) -> bool = CostSum::partial;
    let _: fn(&CostSum) -> String = CostSum::text;
    let _: [&str; 8] = [
        LABEL_INPUT,
        LABEL_OUTPUT,
        LABEL_CACHED,
        LABEL_REASONING,
        LABEL_MODEL,
        LABEL_OPERATION,
        OP_GENERATE_CONTENT,
        UNKNOWN_MODEL,
    ];
}

#[test]
fn the_output_constructors() {
    let _: fn(String, u64, Usage) -> ModelCalls = ModelCalls::new;
    let _: fn(String, Option<String>, ModelCalls) -> Cell = Cell::new;
    let _: fn(Vec<Cell>, bool) -> Breakdown = Breakdown::new;
    let _: fn(usize, Cell) -> TrendCell = TrendCell::new;
    let _: fn(Vec<TrendCell>, bool) -> Trend = Trend::new;
    let _: fn(String, u64, Vec<ModelCalls>) -> TopTrace = TopTrace::new;
    let _: fn(&str) -> Option<UsageRange> = UsageRange::parse;
    let _: fn(UsageRange) -> &'static str = UsageRange::as_str;
    let _: fn(UsageRange) -> TimeDelta = UsageRange::duration;
    let _: fn(UsageRange) -> (i64, usize, &'static str) = UsageRange::bucket;
    let _: &[UsageRange] = UsageRange::ALL;
    let _: [usize; 3] = [BREAKDOWN_CAP, TOP_TRACES, TREND_CAP];

    let _: fn(DateTime<Utc>, String) -> SessionCursor = SessionCursor::new;
    let _: fn(&str, &str) -> Result<SessionCursor, &'static str> = SessionCursor::parse;
    let _: fn(TimeRange, DateTime<Utc>) -> SessionQuery = SessionQuery::new;
    let _: fn(SessionQuery, String) -> SessionQuery = SessionQuery::with_user;
    let _: fn(SessionQuery, String) -> SessionQuery = SessionQuery::with_agent;
    let _: fn(SessionQuery, String) -> SessionQuery = SessionQuery::with_session;
    let _: fn(SessionQuery, SessionCursor) -> SessionQuery = SessionQuery::with_after;
    let _: fn(TimeRange, DateTime<Utc>) -> UserQuery = UserQuery::new;
    let _: fn(UserQuery, String) -> UserQuery = UserQuery::with_agent;
    #[allow(clippy::type_complexity)]
    let _: fn(
        String,
        String,
        String,
        DateTime<Utc>,
        DateTime<Utc>,
        u64,
        u64,
        u64,
        u64,
        Vec<(String, Usage)>,
    ) -> SessionRow = SessionRow::new;
    let _: fn(Vec<SessionRow>, bool, bool) -> SessionPage = SessionPage::new;
    #[allow(clippy::type_complexity)]
    let _: fn(String, u64, u64, u64, u64, u64, DateTime<Utc>, Vec<(String, Usage)>) -> UserRow =
        UserRow::new;
    let _: fn(Vec<UserRow>, bool, bool) -> UserPage = UserPage::new;
    let _: fn(String, String, String) -> TurnScore = TurnScore::new;
    #[allow(clippy::type_complexity)]
    let _: fn(
        String,
        Option<String>,
        DateTime<Utc>,
        Option<DateTime<Utc>>,
        u64,
        Vec<String>,
        Vec<(String, Usage)>,
        Vec<TurnScore>,
    ) -> TurnRow = TurnRow::new;
    let _: fn(&str) -> bool = is_session_id;
    let _: [usize; 5] = [SESSION_CAP, USER_CAP, TURN_CAP, CSV_CAP, MAX_ID_LEN];
    let _: [u32; 2] = [ROW_CAP, CSV_ROW_CAP];
    let _: &str = INVALID_CURSOR;

    let _: fn(u32, u64, f64, f64, Option<f64>, Option<f64>) -> VersionStats = VersionStats::new;
    let _: fn(u32, ModelCalls) -> VersionModel = VersionModel::new;
    let _: fn(Vec<VersionStats>, Vec<VersionModel>, bool) -> VersionMetrics = VersionMetrics::new;
    let _: fn(Vec<(String, u32)>, bool) -> InvocationVersions = InvocationVersions::new;
    #[allow(clippy::type_complexity)]
    let _: fn(
        u32,
        String,
        String,
        String,
        u64,
        u64,
        Option<f64>,
        Option<String>,
    ) -> FeedbackRow = FeedbackRow::new;
    let _: fn(Vec<FeedbackRow>, bool) -> VersionFeedback = VersionFeedback::new;
    let _: usize = FEEDBACK_CAP;

    let _: fn(DateTime<Utc>, String, String, String, String, String, String) -> ScoreRow =
        ScoreRow::new;
    let _: fn(Vec<ScoreRow>, bool) -> TraceScores = TraceScores::new;
    let _: fn(Option<String>, Option<String>, String, Option<String>) -> TraceRoot = TraceRoot::new;
    let _: fn(String, NewValue, Option<String>) -> Result<NewScore, String> = NewScore::new;
    let _: fn(&str, &str, &str, &str) -> Result<NewScore, String> = NewScore::from_form;
    let _: fn(&NewScore) -> Value = NewScore::content;
    let _: [NewValue; 3] = [
        NewValue::Bool(true),
        NewValue::Number(1.0),
        NewValue::Category("c".into()),
    ];
    let _: usize = TRACE_SCORES_CAP;
    let _: &str = BQ_EDITOR_HINT;
    let _: i64 = DML_MIN_BYTES;

    let _: fn(TimeRange) -> ReviewQueue = ReviewQueue::new;
    let _: fn(ReviewQueue, String) -> ReviewQueue = ReviewQueue::with_prompt;
    let _: fn(ReviewQueue, u32) -> ReviewQueue = ReviewQueue::with_version;
    let _: fn(ReviewQueue, bool) -> ReviewQueue = ReviewQueue::with_flagged;
    let _: fn(DateTime<Utc>, String) -> Cursor = Cursor::new;
    let _: fn(&str, &str) -> Result<Cursor, String> = Cursor::parse;
    let _: fn(DateTime<Utc>) -> String = at_text;
    let _: fn(String, DateTime<Utc>, Vec<String>, u64) -> PendingTrace = PendingTrace::new;
    let _: fn(PendingTrace, Vec<JudgeScore>) -> PendingTrace = PendingTrace::with_judge;
    let _: fn(String, f64, bool) -> JudgeScore = JudgeScore::new;
    let _: usize = REVIEW_CAP;
}

/// The async queries: each is called with arguments of its parameter types; the futures are
/// only built (no request is sent), so the result types are checked by the annotations.
#[test]
#[allow(clippy::let_underscore_future)]
fn the_queries() {
    let src = AnalyticsSource::new(
        BigQueryClient::with_client(FakeHttp::new(vec![]), Arc::new(NoAuth)),
        "billing-proj",
        BigQueryTable::new("data-proj", "agent_analytics", "agent_events"),
        1_000,
    );
    let t = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
    let ids = vec!["inv-1".to_owned()];
    let names = vec!["quality".to_owned()];
    let root = TraceRoot::new(None, None, "inv-1".into(), None);
    let score = NewScore::new("quality", NewValue::Bool(true), None).unwrap();
    let sq = SessionQuery::new(TimeRange::Hours24, t);
    let uq = UserQuery::new(TimeRange::Days7, t);
    let rq = ReviewQueue::new(TimeRange::Days7);
    let after = Cursor::new(t, "0123456789abcdef0123456789abcdef".into());

    fn is<T>(_: impl std::future::Future<Output = Result<T, SourceError>>) {}
    is::<Breakdown>(src.breakdown(t, t));
    is::<Vec<TopTrace>>(src.top_traces(t, t, TOP_TRACES));
    is::<Trend>(src.trend(t, t, 3600));
    is::<SessionPage>(src.sessions(&sq, SESSION_CAP, ROW_CAP));
    is::<UserPage>(src.users(&uq, USER_CAP, ROW_CAP));
    is::<Vec<TurnRow>>(src.session_turns("s1", t, t));
    is::<VersionMetrics>(src.version_metrics("triage", t, t));
    is::<VersionFeedback>(src.version_feedback("triage", t, t));
    is::<InvocationVersions>(src.versions_for_invocations("triage", &ids, t, t));
    is::<TraceScores>(src.trace_scores("0123456789abcdef0123456789abcdef", &ids, t));
    is::<Option<TraceRoot>>(src.trace_root("0123456789abcdef0123456789abcdef"));
    is::<()>(src.insert_score("0123456789abcdef0123456789abcdef", &root, &score, "me"));
    is::<()>(src.insert_scores(
        "0123456789abcdef0123456789abcdef",
        &root,
        std::slice::from_ref(&score),
        "me",
    ));
    is::<Vec<PendingTrace>>(src.pending(&rq, &names, t));
    is::<Option<PendingTrace>>(src.next_pending(&rq, &names, t, &after));
}
