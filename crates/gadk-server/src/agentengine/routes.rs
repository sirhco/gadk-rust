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
//! The two Agent Engine routes on a strict-slash router.

use std::sync::Arc;

use http::Method;

use super::controller::Controller;
use crate::mux::Handler;
use crate::mux::router::WebRouter;

/// Path of the non-streaming route.
pub(crate) const REASONING_ENGINE_PATH: &str = "/reasoning_engine";
/// Path of the streaming route.
pub(crate) const STREAM_REASONING_ENGINE_PATH: &str = "/stream_reasoning_engine";

/// Builds the router: `POST /reasoning_engine` →
/// `non_stream.query`, `POST /stream_reasoning_engine` → `stream.query` (in that order), on a
/// `WebRouter::new(true)`; an unknown path is 404, another method 405, a non-canonical path the
/// router's redirect.
pub(crate) fn router(non_stream: Arc<Controller>, stream: Arc<Controller>) -> WebRouter {
    let mut router = WebRouter::new(true);
    router.handle(
        &[Method::POST],
        REASONING_ENGINE_PATH,
        query_handler(non_stream),
    );
    router.handle(
        &[Method::POST],
        STREAM_REASONING_ENGINE_PATH,
        query_handler(stream),
    );
    router
}

/// A handler answering with `controller.query`.
fn query_handler(controller: Arc<Controller>) -> Handler {
    crate::mux::handler(move |req| {
        let controller = Arc::clone(&controller);
        async move { controller.query(req).await }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use axum::body::{Body, to_bytes};
    use gadk_platform::GoDuration;
    use http::Method;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::agentengine::Log;
    use crate::mux::{Request, WriteDeadline};

    fn controllers() -> (Arc<Controller>, Arc<Controller>) {
        let c = || Arc::new(Controller::new(GoDuration(0), 4096, vec![], Log::default()).unwrap());
        (c(), c())
    }

    async fn send(
        router: &WebRouter,
        method: Method,
        target: &str,
        body: &str,
    ) -> (u16, String, String) {
        let mut req = Request::new(Body::from(body.to_owned()));
        *req.method_mut() = method;
        *req.uri_mut() = target.parse().unwrap();
        req.extensions_mut().insert(WriteDeadline::new(|_| {}));
        let resp = router.serve(req).await;
        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get(http::header::LOCATION)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, location, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn routes_post_to_each_controller() {
        let (non_stream, stream) = controllers();
        let router = router(non_stream, stream);
        let body = r#"{"class_method":"x"}"#;
        for path in [REASONING_ENGINE_PATH, STREAM_REASONING_ENGINE_PATH] {
            assert_eq!(
                send(&router, Method::POST, path, body).await,
                (
                    500,
                    String::new(),
                    "unrecognized class method: x\n".to_owned()
                )
            );
        }
        assert_eq!(
            send(&router, Method::POST, "/reasoning_engine?x=1", body)
                .await
                .0,
            500
        );
    }

    #[tokio::test]
    async fn unknown_path_wrong_method_and_redirects() {
        let (non_stream, stream) = controllers();
        let router = router(non_stream, stream);
        assert_eq!(
            send(&router, Method::POST, "/nope", "").await,
            (404, String::new(), "404 page not found\n".to_owned())
        );
        assert_eq!(
            send(&router, Method::GET, "/reasoning_engine", "").await.0,
            405
        );
        assert_eq!(
            send(&router, Method::POST, "/reasoning_engine/", "").await,
            (301, "/reasoning_engine".to_owned(), String::new())
        );
        assert_eq!(
            send(&router, Method::POST, "/x/../reasoning_engine", "")
                .await
                .0,
            301
        );
    }
}
