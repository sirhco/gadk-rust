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

//! Smoke tests of the generated `storagepb` (spec R3): prost wire encoding of the messages the
//! stream writer sends and receives, and the presence of the tonic BigQueryWrite client (always)
//! and server (feature `testutil`, on for this crate's tests).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gadk_agentanalytics::storagepb::google::protobuf::Int64Value;
use gadk_agentanalytics::storagepb::google::rpc::Status;
use gadk_agentanalytics::storagepb::{
    AppendRowsRequest, AppendRowsResponse, ArrowRecordBatch, ArrowSchema, append_rows_request,
    append_rows_response, big_query_write_client::BigQueryWriteClient, big_query_write_server,
};
use prost::Message;

#[test]
fn append_rows_request_wire_bytes_round_trip() {
    let req = AppendRowsRequest {
        write_stream: "s".to_string(),
        offset: Some(Int64Value { value: 7 }),
        trace_id: "t".to_string(),
        rows: Some(append_rows_request::Rows::ArrowRows(
            append_rows_request::ArrowData {
                writer_schema: Some(ArrowSchema {
                    serialized_schema: b"h".to_vec(),
                }),
                rows: Some(ArrowRecordBatch {
                    serialized_record_batch: b"x".to_vec(),
                    ..Default::default()
                }),
            },
        )),
        ..Default::default()
    };
    let bytes = req.encode_to_vec();
    // Fields in tag order: write_stream(1), offset(2), arrow_rows(5){writer_schema(1),
    // rows(2)}, trace_id(6).
    let want: &[u8] = &[
        0x0a, 0x01, b's', // write_stream
        0x12, 0x02, 0x08, 0x07, // offset { value: 7 }
        0x2a, 0x0a, // arrow_rows, 10 bytes
        0x0a, 0x03, 0x0a, 0x01, b'h', // writer_schema { serialized_schema }
        0x12, 0x03, 0x0a, 0x01, b'x', // rows { serialized_record_batch }
        0x32, 0x01, b't', // trace_id
    ];
    assert_eq!(bytes, want);
    assert_eq!(AppendRowsRequest::decode(bytes.as_slice()).unwrap(), req);
}

#[test]
fn append_rows_response_error_status_decodes() {
    let resp = AppendRowsResponse {
        write_stream: "projects/p/datasets/d/tables/t/streams/_default".to_string(),
        response: Some(append_rows_response::Response::Error(Status {
            code: 3,
            message: "bad row".to_string(),
            details: Vec::new(),
        })),
        ..Default::default()
    };
    let decoded = AppendRowsResponse::decode(resp.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded, resp);
}

#[tokio::test]
async fn write_client_and_server_are_generated() {
    let channel = tonic::transport::Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
    let _client = BigQueryWriteClient::new(channel);
    assert_eq!(
        big_query_write_server::SERVICE_NAME,
        "google.cloud.bigquery.storage.v1.BigQueryWrite"
    );
    assert!(
        std::any::type_name::<big_query_write_server::BigQueryWriteServer<()>>()
            .ends_with("BigQueryWriteServer<()>")
    );
}
