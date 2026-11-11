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
//! Go `agentregistry/client.go` (List/Get/All for agents, MCP servers and endpoints) and the
//! list options of `types.go` (`ListOption`, `WithFilter`, `WithPageSize`, `WithPageToken`,
//! `listValues`).

use std::collections::BTreeMap;

use futures::StreamExt as _;
use futures::stream::{self, BoxStream};
use serde::de::DeserializeOwned;

use crate::config::Client;
use crate::error::Error;
use crate::types::{
    Agent, Endpoint, ListAgentsResponse, ListEndpointsResponse, ListMcpServersResponse, McpServer,
};

/// Go `ListOption`: customizes a list request (filter and pagination). Options apply in order;
/// a later one sets the same parameter over an earlier one, and an empty/zero option sets
/// nothing (it does not clear an earlier value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListOption {
    /// Go `WithFilter`: `filter=<expr>` unless empty.
    Filter(String),
    /// Go `WithPageSize`: `pageSize=<n>` when positive.
    PageSize(i64),
    /// Go `WithPageToken`: `pageToken=<token>` unless empty.
    PageToken(String),
}

/// Go `WithFilter`.
pub fn with_filter(filter: impl Into<String>) -> ListOption {
    ListOption::Filter(filter.into())
}

/// Go `WithPageSize`.
pub fn with_page_size(size: i64) -> ListOption {
    ListOption::PageSize(size)
}

/// Go `WithPageToken`.
pub fn with_page_token(token: impl Into<String>) -> ListOption {
    ListOption::PageToken(token.into())
}

/// Go `listValues`: the options collapsed into query parameters, sorted by key (Go
/// `url.Values.Encode` order), one value per key.
pub(crate) fn list_values(opts: &[ListOption]) -> Vec<(String, String)> {
    let mut v = BTreeMap::new();
    for opt in opts {
        match opt {
            ListOption::Filter(f) if !f.is_empty() => {
                v.insert("filter".to_owned(), f.clone());
            }
            ListOption::PageSize(n) if *n > 0 => {
                v.insert("pageSize".to_owned(), n.to_string());
            }
            ListOption::PageToken(t) if !t.is_empty() => {
                v.insert("pageToken".to_owned(), t.clone());
            }
            _ => {}
        }
    }
    v.into_iter().collect()
}

impl Client {
    /// Go `ListAgents`: one page of registered A2A agents (`GET …/agents`).
    pub async fn list_agents(&self, opts: &[ListOption]) -> Result<ListAgentsResponse, Error> {
        self.requester.get_json("agents", &list_values(opts)).await
    }

    /// Go `GetAgent`: one agent by full resource name
    /// (`projects/<p>/locations/<l>/agents/<id>`).
    pub async fn get_agent(&self, name: &str) -> Result<Agent, Error> {
        self.requester.get_json(name, &[]).await
    }

    /// Go `AllAgents`: every agent matching `opts` across pages (each later page adds
    /// `pageToken` after `opts`); a failed page yields one `Err` and ends the stream.
    pub fn all_agents<'a>(&'a self, opts: &[ListOption]) -> BoxStream<'a, Result<Agent, Error>> {
        pages(self, "agents", opts, |p: ListAgentsResponse| {
            (p.agents, p.next_page_token)
        })
    }

    /// Go `ListMCPServers`: one page of registered MCP servers (`GET …/mcpServers`).
    pub async fn list_mcp_servers(
        &self,
        opts: &[ListOption],
    ) -> Result<ListMcpServersResponse, Error> {
        self.requester
            .get_json("mcpServers", &list_values(opts))
            .await
    }

    /// Go `GetMCPServer`: one MCP server by full resource name.
    pub async fn get_mcp_server(&self, name: &str) -> Result<McpServer, Error> {
        self.requester.get_json(name, &[]).await
    }

    /// Go `AllMCPServers`.
    pub fn all_mcp_servers<'a>(
        &'a self,
        opts: &[ListOption],
    ) -> BoxStream<'a, Result<McpServer, Error>> {
        pages(self, "mcpServers", opts, |p: ListMcpServersResponse| {
            (p.mcp_servers, p.next_page_token)
        })
    }

    /// Go `ListEndpoints`: one page of registered model endpoints (`GET …/endpoints`).
    pub async fn list_endpoints(
        &self,
        opts: &[ListOption],
    ) -> Result<ListEndpointsResponse, Error> {
        self.requester
            .get_json("endpoints", &list_values(opts))
            .await
    }

    /// Go `GetEndpoint`: one endpoint by full resource name.
    pub async fn get_endpoint(&self, name: &str) -> Result<Endpoint, Error> {
        self.requester.get_json(name, &[]).await
    }

    /// Go `AllEndpoints`.
    pub fn all_endpoints<'a>(
        &'a self,
        opts: &[ListOption],
    ) -> BoxStream<'a, Result<Endpoint, Error>> {
        pages(self, "endpoints", opts, |p: ListEndpointsResponse| {
            (p.endpoints, p.next_page_token)
        })
    }
}

/// Go `pages`' iteration state: the current page's remaining items and the token of the page to
/// fetch next (`None` once the last page was fetched or a fetch failed).
struct Pages<'a, T> {
    client: &'a Client,
    path: &'static str,
    opts: Vec<ListOption>,
    items: std::vec::IntoIter<T>,
    next: Option<String>,
}

/// Go `pages`: every item across all pages of the list at `path`, fetching a page only when the
/// previous one is exhausted. The first page uses `opts`; each later page `opts` plus
/// `pageToken`. A failed fetch yields one `Err` and ends the stream.
fn pages<'a, R, T>(
    client: &'a Client,
    path: &'static str,
    opts: &[ListOption],
    split: fn(R) -> (Vec<T>, String),
) -> BoxStream<'a, Result<T, Error>>
where
    R: DeserializeOwned + Send + 'a,
    T: Send + 'a,
{
    let state = Pages {
        client,
        path,
        opts: opts.to_vec(),
        items: Vec::new().into_iter(),
        next: Some(String::new()),
    };
    stream::unfold(state, move |mut st| async move {
        loop {
            if let Some(item) = st.items.next() {
                return Some((Ok(item), st));
            }
            let token = st.next.take()?;
            let mut page_opts = st.opts.clone();
            if !token.is_empty() {
                page_opts.push(ListOption::PageToken(token));
            }
            match st
                .client
                .requester
                .get_json::<R>(st.path, &list_values(&page_opts))
                .await
            {
                Ok(page) => {
                    let (items, next) = split(page);
                    st.items = items.into_iter();
                    st.next = (!next.is_empty()).then_some(next);
                }
                Err(e) => return Some((Err(e), st)),
            }
        }
    })
    .boxed()
}
