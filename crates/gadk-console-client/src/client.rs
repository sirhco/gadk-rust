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

//! The HTTP client: base URL checks, requests, reply mapping and session paging.

use reqwest::header::ACCEPT;
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::{Error, http_body};
use crate::generated::{ApiError, ListSessionsQuery, PricedSessionRow, SessionsResponse};

/// One path segment of an API route: literal text, or a parameter sent percent-encoded.
pub(crate) enum Seg<'a> {
    /// A literal segment (`api`, `v1`, `sessions`).
    Lit(&'static str),
    /// A path parameter, percent-encoded as one segment (`team/greet` → `team%2Fgreet`).
    Param(&'a str),
}

/// A client of one console's JSON API.
///
/// Every operation of the API is a method (see the crate docs). The client is cheap to clone:
/// clones share the HTTP connection pool.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Client {
    base: Url,
    http: reqwest::Client,
}

impl Client {
    /// The client of the console at `base_url`, such as `http://127.0.0.1:8765`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidUrl`] unless `base_url` is an `http` or `https` URL without credentials
    /// whose path is empty or `/` (no query or fragment); [`Error::Transport`] when the HTTP
    /// client cannot be built.
    ///
    /// The client does not follow redirects: a 3xx reply is an [`Error::Http`] or
    /// [`Error::Api`] with its status.
    pub fn new(base_url: &str) -> Result<Client, Error> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(Error::Transport)?;
        Client::with_http(base_url, http)
    }

    /// The client of the console at `base_url` over `http`, for your own timeouts, proxy or
    /// headers. `http` keeps its own redirect policy (reqwest follows up to 10 redirects by
    /// default); [`Client::new`] follows none.
    ///
    /// Credentials do not belong in the base URL: set an `Authorization` header as a default
    /// header of `http` instead.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidUrl`] unless `base_url` is an `http` or `https` URL without a user name
    /// or password whose path is empty or `/` (no query or fragment). The error's message does
    /// not repeat `base_url`.
    pub fn with_http(base_url: &str, http: reqwest::Client) -> Result<Client, Error> {
        let base = Url::parse(base_url)
            .map_err(|e| Error::InvalidUrl(format!("the base URL does not parse: {e}")))?;
        if !base.username().is_empty() || base.password().is_some() {
            return Err(Error::InvalidUrl(
                "the base URL must not carry credentials; set an Authorization header on a \
                 reqwest::Client and use Client::with_http"
                    .to_owned(),
            ));
        }
        if !matches!(base.scheme(), "http" | "https") {
            return Err(Error::InvalidUrl(
                "the base URL's scheme must be http or https".to_owned(),
            ));
        }
        if base.path() != "/" || base.query().is_some() || base.fragment().is_some() {
            return Err(Error::InvalidUrl(format!(
                "the base URL must have no path, query or fragment (use {})",
                base.origin().ascii_serialization()
            )));
        }
        Ok(Client { base, http })
    }

    /// GET the route `segments` with the query `query` and decode the 2xx body as `T`.
    ///
    /// A path parameter `.`, `..` or empty is an [`Error::InvalidUrl`]: as a path segment it
    /// would name another route.
    pub(crate) async fn get_json<T: DeserializeOwned>(
        &self,
        segments: &[Seg<'_>],
        query: Vec<(&'static str, String)>,
    ) -> Result<T, Error> {
        for seg in segments {
            if let Seg::Param(p @ ("" | "." | "..")) = seg {
                return Err(Error::InvalidUrl(format!(
                    "the path parameter {p:?} is not a valid path segment"
                )));
            }
        }
        let mut url = self.base.clone();
        {
            let mut path = url.path_segments_mut().map_err(|()| {
                Error::InvalidUrl("the base URL cannot have path segments".to_owned())
            })?;
            path.clear();
            for seg in segments {
                match seg {
                    Seg::Lit(s) => path.push(s),
                    Seg::Param(s) => path.push(s),
                };
            }
        }
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        self.send(url).await
    }

    /// GET `relative` (a link the console sent, such as a sessions page's `next`), resolved
    /// against the base URL, and decode the 2xx body as `T`.
    pub(crate) async fn get_url<T: DeserializeOwned>(&self, relative: &str) -> Result<T, Error> {
        let url = self
            .base
            .join(relative)
            .map_err(|e| Error::InvalidUrl(format!("{relative}: {e}")))?;
        if url.origin() != self.base.origin() {
            return Err(Error::InvalidUrl(format!(
                "{relative}: the link leaves {}",
                self.base.origin().ascii_serialization()
            )));
        }
        self.send(url).await
    }

    /// GET `url` and map the reply: a 2xx body decoded as `T`, else [`Error::Api`] or
    /// [`Error::Http`].
    async fn send<T: DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let resp = self
            .http
            .get(url)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(Error::Transport)?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(Error::Transport)?;
        let code = status.as_u16();
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(|e| Error::Decode {
                status: code,
                message: e.to_string(),
            });
        }
        Err(match serde_json::from_slice::<ApiError>(&bytes) {
            Ok(api) => Error::Api {
                status: code,
                kind: api.error.kind,
                message: api.error.message,
                hint: api.error.hint,
            },
            Err(_) => Error::Http {
                status: code,
                body: http_body(&bytes),
            },
        })
    }

    /// The pages of `GET /api/v1/sessions` for `query`, newest last activity first: the first
    /// page, then each page's `next` link.
    pub fn list_sessions_pages(&self, query: &ListSessionsQuery) -> SessionPages {
        SessionPages {
            client: self.clone(),
            state: PageState::First(query.clone()),
        }
    }

    /// The sessions of at most `max_pages` pages of `GET /api/v1/sessions` for `query`, in
    /// page order.
    ///
    /// # Errors
    ///
    /// The [`Error`] of the first page that fails.
    pub async fn list_sessions_all(
        &self,
        query: &ListSessionsQuery,
        max_pages: usize,
    ) -> Result<Vec<PricedSessionRow>, Error> {
        let mut pages = self.list_sessions_pages(query);
        let mut rows = Vec::new();
        for _ in 0..max_pages {
            match pages.next_page().await? {
                Some(page) => rows.extend(page.data),
                None => break,
            }
        }
        Ok(rows)
    }
}

/// Where a [`SessionPages`] is.
#[derive(Debug)]
enum PageState {
    /// The first page is next.
    First(ListSessionsQuery),
    /// The page at this link is next.
    Next(String),
    /// No page is left (the last page had no `next`, or a page failed).
    Done,
}

/// The pages of a sessions listing, from [`Client::list_sessions_pages`].
#[derive(Debug)]
#[non_exhaustive]
pub struct SessionPages {
    client: Client,
    state: PageState,
}

impl SessionPages {
    /// The next page: the first page, then the page at each response's `next` link. `Ok(None)`
    /// after the page without a `next`, and after an error.
    ///
    /// # Errors
    ///
    /// The page's [`Error`]; it is returned once, and the pages end.
    ///
    /// Dropping the returned future before it completes (a timeout, say) leaves the pages
    /// where they were: the next call fetches the same page again.
    pub async fn next_page(&mut self) -> Result<Option<SessionsResponse>, Error> {
        let res: Result<SessionsResponse, Error> = match &self.state {
            PageState::First(query) => self.client.list_sessions(query).await,
            PageState::Next(link) => self.client.get_url(link).await,
            PageState::Done => return Ok(None),
        };
        match res {
            Err(e) => {
                self.state = PageState::Done;
                Err(e)
            }
            Ok(page) => {
                self.state = page.next.clone().map_or(PageState::Done, PageState::Next);
                Ok(Some(page))
            }
        }
    }
}
