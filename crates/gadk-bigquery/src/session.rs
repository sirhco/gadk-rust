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

//! The Protected write mode: one BigQuery session per agent session, whose anonymous dataset is
//! the only place writes may go.

use gadk_session::State;
use serde_json::Value;

use crate::client::BigQueryClient;
use crate::error::BigQueryError;
use crate::types::{QueryOptions, QueryResult, SessionInfo};

/// The agent-session state key holding the BigQuery session ([`SessionInfo`] as JSON).
pub(crate) const STATE_KEY: &str = "gadk_bigquery_session";

/// The refusal for a write outside the session's anonymous dataset.
pub(crate) const PROTECTED_MSG: &str = "Protected write mode only supports SELECT statements, or write operations in the anonymous dataset of a BigQuery session.";

/// The outcome of a Protected-mode statement.
#[derive(Debug)]
pub(crate) enum Protected {
    /// The statement ran.
    Ran(QueryResult),
    /// The statement writes outside the session's anonymous dataset and was not run.
    Refused,
}

/// Runs `sql` in the agent session's BigQuery session: starts one when none is stored (or the
/// stored one is for another compute project), and replaces it once, retrying the call, when
/// BigQuery reports it gone.
///
/// # Errors
///
/// Any [`BigQueryError`] of the calls, including a second session-gone error.
pub(crate) async fn run(
    state: &dyn State,
    client: &BigQueryClient,
    compute: &str,
    sql: &str,
    opts: &QueryOptions,
) -> Result<Protected, BigQueryError> {
    let session = match stored(state, compute) {
        Some(s) => s,
        None => start(state, client, compute, opts).await?,
    };
    match attempt(client, compute, sql, opts, &session).await {
        Err(e) if e.is_session_gone() => {
            let fresh = start(state, client, compute, opts).await?;
            attempt(client, compute, sql, opts, &fresh).await
        }
        other => other,
    }
}

/// The stored session when it belongs to `compute`.
fn stored(state: &dyn State, compute: &str) -> Option<SessionInfo> {
    let v: Value = state.get(STATE_KEY).ok()?;
    let s: SessionInfo = serde_json::from_value(v).ok()?;
    (s.project == compute).then_some(s)
}

/// Starts a session with `SELECT 1` and stores it.
async fn start(
    state: &dyn State,
    client: &BigQueryClient,
    compute: &str,
    opts: &QueryOptions,
) -> Result<SessionInfo, BigQueryError> {
    let o = opts.clone().with_session_id(None).with_create_session(true);
    let session = client
        .query(compute, "SELECT 1", &o)
        .await?
        .session
        .ok_or_else(|| BigQueryError::Decode("the reply started no session".into()))?;
    let value = serde_json::to_value(&session).map_err(|e| BigQueryError::State(e.to_string()))?;
    state
        .set(STATE_KEY, value)
        .map_err(|e| BigQueryError::State(e.to_string()))?;
    Ok(session)
}

/// Dry-runs `sql` in `session` and runs it when it is a `SELECT` or writes the session's
/// anonymous dataset.
async fn attempt(
    client: &BigQueryClient,
    compute: &str,
    sql: &str,
    opts: &QueryOptions,
    session: &SessionInfo,
) -> Result<Protected, BigQueryError> {
    let o = opts
        .clone()
        .with_create_session(false)
        .with_session_id(Some(session.session_id.clone()));
    let info = client.dry_run(compute, sql, &o).await?;
    let allowed = info.statement_type == "SELECT"
        || info
            .destination
            .as_ref()
            .is_some_and(|d| d.project == session.project && d.dataset == session.dataset);
    if !allowed {
        return Ok(Protected::Refused);
    }
    Ok(Protected::Ran(client.query(compute, sql, &o).await?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use gadk_session::SessionError;
    use serde_json::json;

    use super::*;
    use crate::client::tests::{FakeHttp, client};

    #[derive(Default)]
    struct MemState(Mutex<HashMap<String, Value>>);

    impl State for MemState {
        fn get(&self, key: &str) -> Result<Value, SessionError> {
            self.0
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or(SessionError::StateKeyNotExist)
        }
        fn set(&self, key: &str, value: Value) -> Result<(), SessionError> {
            self.0.lock().unwrap().insert(key.into(), value);
            Ok(())
        }
        fn all(&self) -> Vec<(String, Value)> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }
    }

    /// The two replies of a session start: the `SELECT 1` query and its `jobs.get`.
    fn start(session: &str, dataset: &str) -> Vec<(u16, Value)> {
        vec![
            (
                200,
                json!({ "jobComplete": true, "jobReference": { "projectId": "cp", "jobId": "s" },
                "sessionInfo": { "sessionId": session }, "totalRows": "1" }),
            ),
            (
                200,
                json!({ "configuration": { "query": { "destinationTable": { "projectId": "cp", "datasetId": dataset, "tableId": "x" } } } }),
            ),
        ]
    }

    fn dry(kind: &str, dest: Option<(&str, &str)>) -> (u16, Value) {
        let mut v = json!({ "statistics": { "query": { "statementType": kind } } });
        if let Some((p, d)) = dest {
            v["configuration"] = json!({ "query": { "destinationTable": { "projectId": p, "datasetId": d, "tableId": "t" } } });
        }
        (200, v)
    }

    fn done() -> (u16, Value) {
        (
            200,
            json!({ "jobComplete": true, "jobReference": { "projectId": "cp", "jobId": "q" } }),
        )
    }

    fn ran(p: Protected) -> bool {
        matches!(p, Protected::Ran(_))
    }

    #[tokio::test]
    async fn first_call_starts_a_session_and_stores_it() {
        let mut replies = start("S1", "_anon1");
        replies.extend([dry("SELECT", None), done()]);
        let fake = FakeHttp::new(replies);
        let state = MemState::default();
        assert!(ran(run(
            &state,
            &client(&fake),
            "cp",
            "SELECT 1",
            &QueryOptions::new()
        )
        .await
        .unwrap()));
        assert_eq!(
            state.get(STATE_KEY).unwrap(),
            json!({ "project": "cp", "session_id": "S1", "dataset": "_anon1" })
        );
        assert_eq!(fake.body(0)["query"], json!("SELECT 1"));
        assert_eq!(fake.body(0)["createSession"], json!(true));
        // The dry run and the query carry the session.
        assert_eq!(
            fake.body(2)["configuration"]["query"]["connectionProperties"],
            json!([{ "key": "session_id", "value": "S1" }])
        );
        assert_eq!(
            fake.body(3)["connectionProperties"],
            json!([{ "key": "session_id", "value": "S1" }])
        );
    }

    #[tokio::test]
    async fn a_stored_session_is_reused() {
        let fake = FakeHttp::new(vec![dry("SELECT", None), done()]);
        let state = MemState::default();
        state
            .set(
                STATE_KEY,
                json!({ "project": "cp", "session_id": "S1", "dataset": "_anon1" }),
            )
            .unwrap();
        assert!(ran(run(
            &state,
            &client(&fake),
            "cp",
            "SELECT 1",
            &QueryOptions::new()
        )
        .await
        .unwrap()));
        assert_eq!(fake.uris().len(), 2);
    }

    #[tokio::test]
    async fn temp_table_writes_in_the_session_dataset_run() {
        let fake = FakeHttp::new(vec![
            (
                200,
                json!({ "statistics": { "query": { "statementType": "CREATE_TABLE",
                "ddlTargetTable": { "projectId": "cp", "datasetId": "_anon1", "tableId": "tmp" } } } }),
            ),
            done(),
        ]);
        let state = MemState::default();
        state
            .set(
                STATE_KEY,
                json!({ "project": "cp", "session_id": "S1", "dataset": "_anon1" }),
            )
            .unwrap();
        assert!(ran(run(
            &state,
            &client(&fake),
            "cp",
            "CREATE TEMP TABLE tmp AS SELECT 1 AS n",
            &QueryOptions::new()
        )
        .await
        .unwrap()));
    }

    #[tokio::test]
    async fn writes_elsewhere_and_writes_without_destination_are_refused() {
        let fake = FakeHttp::new(vec![
            dry("INSERT", Some(("cp", "gadk_examples"))),
            dry("DROP_TABLE", None),
        ]);
        let state = MemState::default();
        state
            .set(
                STATE_KEY,
                json!({ "project": "cp", "session_id": "S1", "dataset": "_anon1" }),
            )
            .unwrap();
        let c = client(&fake);
        assert!(matches!(
            run(
                &state,
                &c,
                "cp",
                "INSERT gadk_examples.t VALUES (1)",
                &QueryOptions::new()
            )
            .await
            .unwrap(),
            Protected::Refused
        ));
        assert!(matches!(
            run(
                &state,
                &c,
                "cp",
                "DROP TABLE gadk_examples.t",
                &QueryOptions::new()
            )
            .await
            .unwrap(),
            Protected::Refused
        ));
        assert_eq!(fake.uris().len(), 2, "no query was sent");
    }

    #[tokio::test]
    async fn a_session_for_another_project_is_replaced_without_retry() {
        let mut replies = start("S2", "_anon2");
        replies.extend([dry("SELECT", None), done()]);
        let fake = FakeHttp::new(replies);
        let state = MemState::default();
        state
            .set(
                STATE_KEY,
                json!({ "project": "other", "session_id": "S1", "dataset": "_anon1" }),
            )
            .unwrap();
        assert!(ran(run(
            &state,
            &client(&fake),
            "cp",
            "SELECT 1",
            &QueryOptions::new()
        )
        .await
        .unwrap()));
        assert_eq!(state.get(STATE_KEY).unwrap()["session_id"], json!("S2"));
        assert_eq!(fake.uris().len(), 4);
    }

    #[tokio::test]
    async fn an_expired_session_is_replaced_and_the_call_retried_once() {
        let mut replies = vec![(
            400,
            json!({ "error": { "message": "Session S1 has expired" } }),
        )];
        replies.extend(start("S2", "_anon2"));
        replies.extend([dry("SELECT", None), done()]);
        let fake = FakeHttp::new(replies);
        let state = MemState::default();
        state
            .set(
                STATE_KEY,
                json!({ "project": "cp", "session_id": "S1", "dataset": "_anon1" }),
            )
            .unwrap();
        assert!(ran(run(
            &state,
            &client(&fake),
            "cp",
            "SELECT 1",
            &QueryOptions::new()
        )
        .await
        .unwrap()));
        assert_eq!(state.get(STATE_KEY).unwrap()["session_id"], json!("S2"));
    }

    #[tokio::test]
    async fn a_second_expiry_is_returned() {
        let gone = (400, json!({ "error": { "message": "Session not found" } }));
        let mut replies = vec![gone.clone()];
        replies.extend(start("S2", "_anon2"));
        replies.push(gone);
        let fake = FakeHttp::new(replies);
        let state = MemState::default();
        state
            .set(
                STATE_KEY,
                json!({ "project": "cp", "session_id": "S1", "dataset": "_anon1" }),
            )
            .unwrap();
        let err = run(
            &state,
            &client(&fake),
            "cp",
            "SELECT 1",
            &QueryOptions::new(),
        )
        .await
        .unwrap_err();
        assert!(err.is_session_gone(), "{err:?}");
    }
}
