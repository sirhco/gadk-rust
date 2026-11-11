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

//! Ports of ADK-Go `agentregistry/client_test.go`.

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::agentregistry_requester::{Seen, new_test_client, serve};
use crate::client::{ListOption, with_filter, with_page_size};
use crate::config::Client;
use crate::error::Error;
use crate::types::{Annotations, McpServer, Tool};

/// One page as Go's table projects it: display names and the next-page token.
type Page = Result<(Vec<String>, String), Error>;

/// Go `TestClientList`.
#[tokio::test]
async fn test_client_list() {
    fn agents<'a>(c: &'a Client, o: &'a [ListOption]) -> BoxFuture<'a, Page> {
        Box::pin(async move {
            let p = c.list_agents(o).await?;
            Ok((
                p.agents.into_iter().map(|a| a.display_name).collect(),
                p.next_page_token,
            ))
        })
    }
    fn mcp_servers<'a>(c: &'a Client, o: &'a [ListOption]) -> BoxFuture<'a, Page> {
        Box::pin(async move {
            let p = c.list_mcp_servers(o).await?;
            Ok((
                p.mcp_servers.into_iter().map(|s| s.display_name).collect(),
                p.next_page_token,
            ))
        })
    }
    fn endpoints<'a>(c: &'a Client, o: &'a [ListOption]) -> BoxFuture<'a, Page> {
        Box::pin(async move {
            let p = c.list_endpoints(o).await?;
            Ok((
                p.endpoints.into_iter().map(|e| e.display_name).collect(),
                p.next_page_token,
            ))
        })
    }
    type List = for<'a> fn(&'a Client, &'a [ListOption]) -> BoxFuture<'a, Page>;

    struct Case {
        name: &'static str,
        resp_body: &'static str,
        status: u16,
        opts: Vec<ListOption>,
        want_path: &'static str,
        want_filter: &'static str,
        want_page_size: &'static str,
        list: List,
        want_names: Vec<&'static str>,
        want_next: &'static str,
        /// Non-zero: expect an `ApiError` with this status.
        want_err_code: u16,
    }
    let cases = vec![
        Case {
            name: "agents",
            resp_body: r#"{"agents":[{"displayName":"Foo"},{"displayName":"Bar"}],"nextPageToken":"n"}"#,
            status: 200,
            opts: vec![with_filter("type=A2A"), with_page_size(2)],
            want_path: "/projects/p/locations/l/agents",
            want_filter: "type=A2A",
            want_page_size: "2",
            list: agents,
            want_names: vec!["Foo", "Bar"],
            want_next: "n",
            want_err_code: 0,
        },
        Case {
            name: "mcpServers",
            resp_body: r#"{"mcpServers":[{"displayName":"Data","mcpServerId":"data-mcp"}]}"#,
            status: 200,
            opts: vec![],
            want_path: "/projects/p/locations/l/mcpServers",
            want_filter: "",
            want_page_size: "",
            list: mcp_servers,
            want_names: vec!["Data"],
            want_next: "",
            want_err_code: 0,
        },
        Case {
            name: "endpoints",
            resp_body: r#"{"endpoints":[{"displayName":"Gemini"}]}"#,
            status: 200,
            opts: vec![],
            want_path: "/projects/p/locations/l/endpoints",
            want_filter: "",
            want_page_size: "",
            list: endpoints,
            want_names: vec!["Gemini"],
            want_next: "",
            want_err_code: 0,
        },
        Case {
            // A non-2xx status is surfaced as a typed ApiError.
            name: "error status",
            resp_body: r#"{"error":"boom"}"#,
            status: 503,
            opts: vec![],
            want_path: "",
            want_filter: "",
            want_page_size: "",
            list: agents,
            want_names: vec![],
            want_next: "",
            want_err_code: 503,
        },
    ];
    for tc in cases {
        let (status, body) = (tc.status, tc.resp_body);
        let srv = serve(move |_| (status, body.to_owned())).await;
        let client = new_test_client(&srv);
        let result = (tc.list)(&client, &tc.opts).await;
        if tc.want_err_code != 0 {
            let err = result.unwrap_err();
            let Error::Api(api) = &err else {
                panic!("{}: error = {err:?}, want Error::Api", tc.name);
            };
            assert_eq!(api.status_code, tc.want_err_code, "{}", tc.name);
            continue;
        }
        let (names, next) = result.unwrap();
        let seen = srv.only_request();
        assert_eq!(seen.path, tc.want_path, "{}: request path", tc.name);
        assert_eq!(
            seen.query_get("filter"),
            tc.want_filter,
            "{}: filter",
            tc.name
        );
        assert_eq!(
            seen.query_get("pageSize"),
            tc.want_page_size,
            "{}: pageSize",
            tc.name
        );
        assert_eq!(names, tc.want_names, "{}: names", tc.name);
        assert_eq!(next, tc.want_next, "{}: nextPageToken", tc.name);
    }
}

/// Go `TestClientGet`.
#[tokio::test]
async fn test_client_get() {
    /// The display name plus one field unique to the resource type, which confirms the get
    /// decoded the correct concrete type.
    type Got = Result<(String, String), Error>;
    fn agent<'a>(c: &'a Client, name: &'a str) -> BoxFuture<'a, Got> {
        Box::pin(async move {
            let a = c.get_agent(name).await?;
            Ok((a.display_name, a.description))
        })
    }
    fn mcp_server<'a>(c: &'a Client, name: &'a str) -> BoxFuture<'a, Got> {
        Box::pin(async move {
            let s = c.get_mcp_server(name).await?;
            Ok((s.display_name, s.mcp_server_id))
        })
    }
    fn endpoint<'a>(c: &'a Client, name: &'a str) -> BoxFuture<'a, Got> {
        Box::pin(async move {
            let e = c.get_endpoint(name).await?;
            Ok((e.display_name, e.endpoint_id))
        })
    }
    type Get = for<'a> fn(&'a Client, &'a str) -> BoxFuture<'a, Got>;

    let cases: [(&str, &str, &str, &str, Get, &str, &str); 3] = [
        (
            "agent",
            r#"{"displayName":"Summarizer","description":"sums things up"}"#,
            "projects/p/locations/l/agents/summarizer",
            "/projects/p/locations/l/agents/summarizer",
            agent,
            "Summarizer",
            "sums things up",
        ),
        (
            "mcpServer",
            r#"{"displayName":"Data","mcpServerId":"data-mcp"}"#,
            "projects/p/locations/l/mcpServers/data-mcp",
            "/projects/p/locations/l/mcpServers/data-mcp",
            mcp_server,
            "Data",
            "data-mcp",
        ),
        (
            "endpoint",
            r#"{"displayName":"Gemini","endpointId":"gemini"}"#,
            "projects/p/locations/l/endpoints/gemini",
            "/projects/p/locations/l/endpoints/gemini",
            endpoint,
            "Gemini",
            "gemini",
        ),
    ];
    for (name, resp_body, resource_name, want_path, get, want_display, want_type_field) in cases {
        let srv = serve(move |_| (200, resp_body.to_owned())).await;
        let client = new_test_client(&srv);
        let (display, type_field) = get(&client, resource_name).await.unwrap();
        // The full resource name is used verbatim, not re-prefixed with the parent.
        assert_eq!(srv.only_request().path, want_path, "{name}: request path");
        assert_eq!(display, want_display, "{name}: displayName");
        assert_eq!(type_field, want_type_field, "{name}: type-specific field");
    }
}

/// Go `TestGetMCPServerDecodesFullResource`: the output-only `MCPServer` metadata (tools,
/// annotations, timestamps, attributes) the leaner list/get tests don't exercise.
#[tokio::test]
async fn test_get_mcp_server_decodes_full_resource() {
    let full = r#"{
        "name": "projects/p/locations/l/mcpServers/data",
        "mcpServerId": "data-mcp",
        "displayName": "Data",
        "tools": [
            {
                "name": "query",
                "description": "run a query",
                "annotations": {
                    "title": "Query",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": true
                }
            },
            {"name": "wipe", "description": "delete everything"}
        ],
        "createTime": "2026-01-02T15:04:05Z",
        "updateTime": "2026-01-03T15:04:05Z",
        "attributes": {"agentregistry.googleapis.com/system/RuntimeReference": {"uri": "//gke/dep"}}
    }"#;
    let want_full = McpServer {
        name: "projects/p/locations/l/mcpServers/data".to_owned(),
        mcp_server_id: "data-mcp".to_owned(),
        display_name: "Data".to_owned(),
        tools: vec![
            Tool {
                name: "query".to_owned(),
                description: "run a query".to_owned(),
                annotations: Some(Annotations {
                    title: "Query".to_owned(),
                    read_only_hint: true,
                    idempotent_hint: true,
                    open_world_hint: true,
                    ..Annotations::default()
                }),
            },
            Tool {
                name: "wipe".to_owned(),
                description: "delete everything".to_owned(),
                annotations: None,
            },
        ],
        create_time: "2026-01-02T15:04:05Z".to_owned(),
        update_time: "2026-01-03T15:04:05Z".to_owned(),
        attributes: json!({
            "agentregistry.googleapis.com/system/RuntimeReference": {"uri": "//gke/dep"}
        })
        .as_object()
        .unwrap()
        .clone(),
        ..McpServer::default()
    };
    let cases = [
        (
            "tools annotations timestamps and attributes",
            full,
            want_full,
        ),
        (
            "no tools reported leaves Tools nil",
            r#"{"displayName":"Data","mcpServerId":"data-mcp"}"#,
            McpServer {
                display_name: "Data".to_owned(),
                mcp_server_id: "data-mcp".to_owned(),
                ..McpServer::default()
            },
        ),
    ];
    for (name, resp_body, want) in cases {
        let srv = serve(move |_| (200, resp_body.to_owned())).await;
        let got = new_test_client(&srv)
            .get_mcp_server("projects/p/locations/l/mcpServers/data")
            .await
            .unwrap();
        assert_eq!(got, want, "{name}");
    }
}

/// Go `TestClientAll`.
#[tokio::test]
async fn test_client_all() {
    // Serves a1,a2 (nextPageToken tok2) then a3.
    fn two_page_agents(r: &Seen) -> (u16, String) {
        match r.query_get("pageToken").as_str() {
            "" => (
                200,
                r#"{"agents":[{"displayName":"a1"},{"displayName":"a2"}],"nextPageToken":"tok2"}"#
                    .to_owned(),
            ),
            "tok2" => (200, r#"{"agents":[{"displayName":"a3"}]}"#.to_owned()),
            _ => (500, String::new()),
        }
    }
    // The paginator must forward opts (filter) on every page, not just the first: the handler
    // 500s any request missing the filter, so a token page that dropped it would error.
    fn filtered_agents(r: &Seen) -> (u16, String) {
        if r.query_get("filter") != "type=A2A" {
            return (500, r#"{"error":"missing filter"}"#.to_owned());
        }
        match r.query_get("pageToken").as_str() {
            "" => (
                200,
                r#"{"agents":[{"displayName":"a1"}],"nextPageToken":"tok2"}"#.to_owned(),
            ),
            "tok2" => (200, r#"{"agents":[{"displayName":"a2"}]}"#.to_owned()),
            _ => (500, String::new()),
        }
    }
    fn boom(_: &Seen) -> (u16, String) {
        (500, r#"{"error":"boom"}"#.to_owned())
    }
    fn two_mcp_servers(_: &Seen) -> (u16, String) {
        (
            200,
            r#"{"mcpServers":[{"displayName":"one"},{"displayName":"two"}]}"#.to_owned(),
        )
    }
    fn one_endpoint(_: &Seen) -> (u16, String) {
        (
            200,
            r#"{"endpoints":[{"displayName":"gemini"}]}"#.to_owned(),
        )
    }

    /// Go `nameSeq`: one name per item.
    type Names<'a> = BoxStream<'a, Result<String, Error>>;
    fn agents_all(c: &Client) -> Names<'_> {
        c.all_agents(&[]).map(|r| r.map(|a| a.display_name)).boxed()
    }
    fn agents_all_filtered(c: &Client) -> Names<'_> {
        c.all_agents(&[with_filter("type=A2A")])
            .map(|r| r.map(|a| a.display_name))
            .boxed()
    }
    fn mcp_servers_all(c: &Client) -> Names<'_> {
        c.all_mcp_servers(&[])
            .map(|r| r.map(|s| s.display_name))
            .boxed()
    }
    fn endpoints_all(c: &Client) -> Names<'_> {
        c.all_endpoints(&[])
            .map(|r| r.map(|e| e.display_name))
            .boxed()
    }
    type Handler = fn(&Seen) -> (u16, String);
    type All = for<'a> fn(&'a Client) -> Names<'a>;

    /// (name, handler, all, stop_after, want_names, want_err, want_requests (0: unchecked))
    type Case = (
        &'static str,
        Handler,
        All,
        usize,
        Option<Vec<&'static str>>,
        bool,
        usize,
    );
    let cases: [Case; 6] = [
        (
            "agents paginate across pages",
            two_page_agents,
            agents_all,
            0,
            Some(vec!["a1", "a2", "a3"]),
            false,
            2,
        ),
        (
            "agents early stop skips the next page",
            two_page_agents,
            agents_all,
            1,
            Some(vec!["a1"]),
            false,
            1,
        ),
        (
            "agents error stops iteration",
            boom,
            agents_all,
            0,
            None,
            true,
            1,
        ),
        (
            "filter is forwarded on every page",
            filtered_agents,
            agents_all_filtered,
            0,
            Some(vec!["a1", "a2"]),
            false,
            2,
        ),
        (
            "mcpServers single page",
            two_mcp_servers,
            mcp_servers_all,
            0,
            Some(vec!["one", "two"]),
            false,
            0,
        ),
        (
            "endpoints single page",
            one_endpoint,
            endpoints_all,
            0,
            Some(vec!["gemini"]),
            false,
            0,
        ),
    ];
    for (name, handler, all, stop_after, want_names, want_err, want_requests) in cases {
        let srv = serve(handler).await;
        let client = new_test_client(&srv);
        let mut got = Vec::new();
        let mut got_err = None;
        let mut stream = all(&client);
        while let Some(item) = stream.next().await {
            match item {
                // Let the stream terminate on its own.
                Err(e) => got_err = Some(e),
                Ok(n) => {
                    got.push(n);
                    if stop_after > 0 && got.len() >= stop_after {
                        break;
                    }
                }
            }
        }
        drop(stream);

        assert_eq!(
            got_err.is_some(),
            want_err,
            "{name}: stream error = {got_err:?}"
        );
        if let Some(want_names) = want_names {
            assert_eq!(got, want_names, "{name}: collected");
        }
        if want_requests > 0 {
            assert_eq!(
                srv.requests().len(),
                want_requests,
                "{name}: server requests"
            );
        }
    }
}
