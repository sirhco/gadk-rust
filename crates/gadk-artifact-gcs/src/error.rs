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

//! [`GcsError`] (the storage client's errors) and the Go `%w` wrapping the service builds on top
//! of it.
#![allow(dead_code)]

use std::error::Error as StdError;

use gadk_artifact::ArtifactError;
use gadk_model::rest::RestError;

/// A boxed error cause.
pub(crate) type BoxError = Box<dyn StdError + Send + Sync>;

/// An error from the storage client (Go's `storage` and `googleapi` errors). It is reachable
/// from an [`ArtifactError::Backend`] through the `source()` chain; [`is_object_not_exist`]
/// finds [`GcsError::ObjectNotExist`] there.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum GcsError {
    /// The object does not exist (Go `storage.ErrObjectNotExist`; HTTP 404).
    #[error("storage: object doesn't exist")]
    ObjectNotExist,
    /// A precondition failed (HTTP 412): the object a does-not-exist upload targets exists.
    /// `message` is the JSON error body's `error.message`, `body` the whole response body.
    #[error("{}", googleapi_text(412, message, body))]
    PreconditionFailed {
        /// The JSON error body's `error.message` (empty when there is none).
        message: String,
        /// The response body.
        body: String,
    },
    /// Another HTTP error status (Go `googleapi.Error`).
    #[error("{}", googleapi_text(*status, message, body))]
    Http {
        /// The HTTP status code.
        status: u16,
        /// The JSON error body's `error.message` (empty when there is none).
        message: String,
        /// The response body.
        body: String,
    },
    /// The request got no usable response (connection error, credentials, dropped body).
    #[error("{0}")]
    Transport(#[source] RestError),
    /// A successful response's body could not be decoded.
    #[error("{0}")]
    Decode(String),
    /// Any other error (the test fake's injected errors).
    #[error("{0}")]
    Other(#[source] BoxError),
}

/// Go `googleapi.Error.Error()` for an error without details: `googleapi: Error {status}:
/// {message}`, or `googleapi: got HTTP response code {status} with body: {body}` when there is
/// no message.
fn googleapi_text(status: u16, message: &str, body: &str) -> String {
    if message.is_empty() {
        format!("googleapi: got HTTP response code {status} with body: {body}")
    } else {
        format!("googleapi: Error {status}: {message}")
    }
}

impl GcsError {
    /// Go `errors.Is(err, storage.ErrObjectNotExist)`: this is [`GcsError::ObjectNotExist`] or
    /// wraps one.
    pub fn is_object_not_exist(&self) -> bool {
        chain_any(self, is_object_not_exist_error)
    }
}

fn is_object_not_exist_error(e: &(dyn StdError + 'static)) -> bool {
    matches!(e.downcast_ref::<GcsError>(), Some(GcsError::ObjectNotExist))
}

/// Whether `err` or any error in its `source()` chain satisfies `pred` (Go `errors.Is` /
/// `errors.As`).
pub(crate) fn chain_any(
    err: &(dyn StdError + 'static),
    pred: impl Fn(&(dyn StdError + 'static)) -> bool,
) -> bool {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if pred(e) {
            return true;
        }
        cur = e.source();
    }
    false
}

/// Go `fmt.Errorf("{prefix}: %w", source)`: displays `{prefix}: {source}` and keeps `source`
/// as its `source()`.
#[derive(Debug)]
pub(crate) struct GoWrapped {
    prefix: String,
    source: BoxError,
}

impl GoWrapped {
    pub(crate) fn new(prefix: impl Into<String>, source: impl Into<BoxError>) -> Self {
        Self {
            prefix: prefix.into(),
            source: source.into(),
        }
    }
}

impl std::fmt::Display for GoWrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.prefix, self.source)
    }
}

impl StdError for GoWrapped {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

/// `ArtifactError::Backend` holding Go's `fmt.Errorf("{prefix}: %w", source)`.
pub(crate) fn backend(prefix: impl Into<String>, source: impl Into<BoxError>) -> ArtifactError {
    ArtifactError::Backend(Box::new(GoWrapped::new(prefix, source)))
}

/// Go `ErrVersionConflict`: `save` could not claim a new version within its retry budget.
#[derive(Debug, thiserror::Error)]
#[error("artifact version conflict")]
pub(crate) struct VersionConflict;

/// Go `errors.Is(err, ErrVersionConflict)`: `save` gave up because concurrent writers kept
/// taking the version it picked; the save can be retried.
pub fn is_version_conflict(err: &ArtifactError) -> bool {
    match err {
        ArtifactError::Backend(b) => chain_any(b.as_ref(), |e| e.is::<VersionConflict>()),
        _ => false,
    }
}

/// Go `errors.Is(err, storage.ErrObjectNotExist)` for a service error: the storage client said
/// the object does not exist (deleting a missing version, for example). Distinct from
/// [`ArtifactError::NotFound`], which `load`, `versions` and `get_artifact_version` return.
pub fn is_object_not_exist(err: &ArtifactError) -> bool {
    match err {
        ArtifactError::Backend(b) => chain_any(b.as_ref(), is_object_not_exist_error),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn gcs_error_display_is_gos_text() {
        assert_eq!(
            GcsError::ObjectNotExist.to_string(),
            "storage: object doesn't exist"
        );
        assert_eq!(
            GcsError::PreconditionFailed {
                message: "conditionNotMet".into(),
                body: "{}".into(),
            }
            .to_string(),
            "googleapi: Error 412: conditionNotMet"
        );
        assert_eq!(
            GcsError::PreconditionFailed {
                message: String::new(),
                body: String::new(),
            }
            .to_string(),
            "googleapi: got HTTP response code 412 with body: "
        );
        assert_eq!(
            GcsError::Http {
                status: 500,
                message: "boom".into(),
                body: String::new(),
            }
            .to_string(),
            "googleapi: Error 500: boom"
        );
        assert_eq!(
            GcsError::Http {
                status: 502,
                message: String::new(),
                body: "bad gateway".into(),
            }
            .to_string(),
            "googleapi: got HTTP response code 502 with body: bad gateway"
        );
        assert_eq!(
            GcsError::Transport(RestError::DeadlineExceeded).to_string(),
            "context deadline exceeded"
        );
        assert_eq!(GcsError::Decode("bad json".into()).to_string(), "bad json");
        assert_eq!(GcsError::Other("injected".into()).to_string(), "injected");
    }

    #[test]
    fn is_object_not_exist_follows_the_chain() {
        assert!(GcsError::ObjectNotExist.is_object_not_exist());
        let wrapped = GcsError::Other(Box::new(GoWrapped::new("delete", GcsError::ObjectNotExist)));
        assert!(wrapped.is_object_not_exist());
        assert!(!GcsError::Other("other".into()).is_object_not_exist());

        let err = backend("failed to delete artifact", wrapped);
        assert_eq!(
            err.to_string(),
            "failed to delete artifact: delete: storage: object doesn't exist"
        );
        assert!(is_object_not_exist(&err));
        assert!(!is_object_not_exist(&ArtifactError::NotFound));
        assert!(!is_object_not_exist(&backend(
            "x",
            GcsError::Http {
                status: 404,
                message: "m".into(),
                body: String::new(),
            }
        )));
    }

    #[test]
    fn version_conflict_is_found_through_the_wrapping() {
        let err = backend(
            "failed to save artifact \"f\" after 16 attempts",
            VersionConflict,
        );
        assert_eq!(
            err.to_string(),
            "failed to save artifact \"f\" after 16 attempts: artifact version conflict"
        );
        assert!(is_version_conflict(&err));
        assert!(!is_version_conflict(&ArtifactError::NotFound));
        assert!(!is_version_conflict(&backend(
            "x",
            GcsError::ObjectNotExist
        )));
    }

    #[test]
    fn errors_are_send_sync_static() {
        fn assert_bounds<T: StdError + Send + Sync + 'static>() {}
        assert_bounds::<GcsError>();
        assert_bounds::<GoWrapped>();
    }
}
