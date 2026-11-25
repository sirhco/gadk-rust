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

//! CSV downloads: an RFC 4180 writer (CRLF line ends, UTF-8 without a BOM) with a guard
//! against spreadsheet formulas, and the response that serves a body as a file.

use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use http::StatusCode;
use http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};

use crate::prices::CostSum;

/// The response header set on a truncated download.
pub(crate) const TRUNCATED_HEADER: &str = "x-gadk-truncated";

/// One CSV value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Field {
    /// Text from the data (a model, agent, prompt, session id or user): quoted when needed,
    /// and prefixed with `'` when it starts with `=`, `+`, `-`, `@`, TAB or CR.
    Text(String),
    /// A count, in plain decimal.
    Int(u64),
    /// Two columns: the priced calls' cost in USD to 6 decimals (empty when no call had a
    /// price), then whether some calls had no price (`true` / `false`).
    Cost(CostSum),
    /// A time in UTC, RFC 3339 to the millisecond: `2026-11-24T01:00:00.000Z`.
    Time(DateTime<Utc>),
    /// `true` or `false`.
    Bool(bool),
}

/// A CSV body: a header row, then data rows, each line ending in CRLF.
#[derive(Debug, Default)]
pub(crate) struct Csv {
    out: String,
}

impl Csv {
    /// An empty body.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Adds the header row: the column names as they are.
    pub(crate) fn header(&mut self, names: &[&str]) {
        self.out.push_str(&names.join(","));
        self.out.push_str("\r\n");
    }

    /// Adds one data row.
    pub(crate) fn row(&mut self, fields: &[Field]) {
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            self.field(field);
        }
        self.out.push_str("\r\n");
    }

    /// The body.
    pub(crate) fn finish(self) -> String {
        self.out
    }

    fn field(&mut self, field: &Field) {
        match field {
            Field::Text(s) => self.text(s),
            Field::Int(n) => self.out.push_str(&n.to_string()),
            Field::Cost(c) => {
                if c.priced {
                    self.out.push_str(&format!("{:.6}", c.usd));
                }
                self.out.push(',');
                self.field(&Field::Bool(c.partial()));
            }
            Field::Time(t) => self
                .out
                .push_str(&t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()),
            Field::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
        }
    }

    /// `s` with the formula guard, then quoted (each `"` doubled) when it holds `,`, `"`, CR or
    /// LF.
    fn text(&mut self, s: &str) {
        let guarded = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
            format!("'{s}")
        } else {
            s.to_owned()
        };
        if guarded.contains([',', '"', '\r', '\n']) {
            self.out.push('"');
            self.out.push_str(&guarded.replace('"', "\"\""));
            self.out.push('"');
        } else {
            self.out.push_str(&guarded);
        }
    }
}

/// `gadk-<prefix>-<range>-<yyyymmddThhmmZ>.csv`, the stamp being the window end in UTC, ending
/// `-truncated.csv` instead when the rows were cut.
pub(crate) fn filename(prefix: &str, range: &str, end: DateTime<Utc>, truncated: bool) -> String {
    format!(
        "gadk-{prefix}-{range}-{}{}.csv",
        end.format("%Y%m%dT%H%MZ"),
        if truncated { "-truncated" } else { "" }
    )
}

/// A 200 serving `body` as the attachment `filename` (`text/csv; charset=utf-8`), with
/// `X-Gadk-Truncated: true` when the rows were cut.
pub(crate) fn response(body: String, filename: &str, truncated: bool) -> Response {
    let mut resp = (
        StatusCode::OK,
        [
            (CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (
                CONTENT_DISPOSITION,
                format!(r#"attachment; filename="{filename}""#),
            ),
        ],
        body,
    )
        .into_response();
    if truncated {
        resp.headers_mut().insert(
            http::HeaderName::from_static(TRUNCATED_HEADER),
            http::HeaderValue::from_static("true"),
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use http::StatusCode;
    use http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};

    use super::*;

    fn one(fields: &[Field]) -> String {
        let mut csv = Csv::new();
        csv.row(fields);
        csv.finish()
    }

    fn text(s: &str) -> Field {
        Field::Text(s.to_owned())
    }

    #[test]
    fn a_header_and_rows_end_in_crlf() {
        let mut csv = Csv::new();
        csv.header(&["model", "llm_calls"]);
        csv.row(&[text("m"), Field::Int(3)]);
        csv.row(&[text(""), Field::Int(0)]);
        assert_eq!(csv.finish(), "model,llm_calls\r\nm,3\r\n,0\r\n");
        assert_eq!(Csv::new().finish(), "");
    }

    #[test]
    fn fields_with_commas_quotes_or_line_breaks_are_quoted() {
        assert_eq!(one(&[text("a,b")]), "\"a,b\"\r\n");
        assert_eq!(one(&[text(r#"say "hi""#)]), "\"say \"\"hi\"\"\"\r\n");
        assert_eq!(one(&[text("a\nb")]), "\"a\nb\"\r\n");
        assert_eq!(one(&[text("a\rb")]), "\"a\rb\"\r\n");
        assert_eq!(one(&[text("plain text é")]), "plain text é\r\n");
        assert_eq!(one(&[text("a'b")]), "a'b\r\n");
    }

    #[test]
    fn text_that_could_start_a_formula_gets_a_quote_prefix() {
        assert_eq!(one(&[text("=1+2")]), "'=1+2\r\n");
        assert_eq!(one(&[text("+1")]), "'+1\r\n");
        assert_eq!(one(&[text("-1")]), "'-1\r\n");
        assert_eq!(one(&[text("@sum")]), "'@sum\r\n");
        assert_eq!(one(&[text("\tx")]), "'\tx\r\n");
        assert_eq!(one(&[text("\rx")]), "\"'\rx\"\r\n");
        assert_eq!(one(&[text("=a,b")]), "\"'=a,b\"\r\n");
        assert_eq!(one(&[text("a=b")]), "a=b\r\n");
        assert_eq!(one(&[text("(none)")]), "(none)\r\n");
    }

    #[test]
    fn numbers_times_and_bools_are_plain() {
        let t = Utc.with_ymd_and_hms(2026, 11, 24, 1, 0, 0).unwrap()
            + chrono::TimeDelta::microseconds(123_999);
        assert_eq!(
            one(&[
                Field::Int(0),
                Field::Int(u64::MAX),
                Field::Time(t),
                Field::Bool(true),
                Field::Bool(false),
            ]),
            "0,18446744073709551615,2026-11-24T01:00:00.123Z,true,false\r\n"
        );
    }

    #[test]
    fn a_cost_is_two_columns_usd_and_partial() {
        let mut none = CostSum::default();
        none.add(None);
        assert_eq!(one(&[Field::Cost(CostSum::default())]), ",false\r\n");
        assert_eq!(one(&[Field::Cost(none)]), ",false\r\n");
        let mut all = CostSum::default();
        all.add(Some(0.001));
        all.add(Some(0.0000004));
        assert_eq!(one(&[Field::Cost(all)]), "0.001000,false\r\n");
        let mut partial = all;
        partial.add(None);
        assert_eq!(
            one(&[text("m"), Field::Cost(partial), Field::Int(1)]),
            "m,0.001000,true,1\r\n"
        );
    }

    #[test]
    fn filenames_carry_the_range_and_the_window_end() {
        let end = Utc.with_ymd_and_hms(2026, 11, 24, 1, 5, 59).unwrap();
        assert_eq!(
            filename("usage-model", "24h", end, false),
            "gadk-usage-model-24h-20261124T0105Z.csv"
        );
        assert_eq!(
            filename("sessions", "7d", end, true),
            "gadk-sessions-7d-20261124T0105Z-truncated.csv"
        );
    }

    async fn body(resp: Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn responses_are_csv_attachments_flagged_when_truncated() {
        let resp = response(
            "a\r\n".to_owned(),
            "gadk-users-7d-20261124T0105Z.csv",
            false,
        );
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[CONTENT_TYPE], "text/csv; charset=utf-8");
        assert_eq!(
            resp.headers()[CONTENT_DISPOSITION],
            r#"attachment; filename="gadk-users-7d-20261124T0105Z.csv""#
        );
        assert!(resp.headers().get(TRUNCATED_HEADER).is_none());
        assert_eq!(body(resp).await, "a\r\n");

        let resp = response(
            String::new(),
            "gadk-users-7d-20261124T0105Z-truncated.csv",
            true,
        );
        assert_eq!(resp.headers()[TRUNCATED_HEADER], "true");
        assert_eq!(
            resp.headers()[CONTENT_DISPOSITION],
            r#"attachment; filename="gadk-users-7d-20261124T0105Z-truncated.csv""#
        );
    }
}
