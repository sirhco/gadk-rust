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

//! Port of ADK-Go `plugin/agentanalytics/gcs_offloader.go`: `uploads` (the upload
//! orchestration of a batch, `impl BatchProcessor`) and [`gcs`] (`gcsOffloader`, the Cloud
//! Storage [`GcsContentOffloader`](crate::config::GcsContentOffloader)). This module holds what
//! both use: the constants of `gcs_offloader.go` and its error types (`offloaderPanicError`,
//! `ownedUploadError`, `errInvalidGCSURI`, `errUploadCooldown`, `errBatchUploadTimeout`).
//! `pendingUpload`, `objectRef`, `uploadFailedText`, `textUploadContentType`,
//! `gcsUploadTimeout` and `defaultMaxPendingUploadBytes` are 7b-1's (`crate::formatter`,
//! `crate::config`).

pub mod gcs;
pub(crate) mod uploads;

use std::fmt;

use crate::context::ContextError;
use crate::error::Error;

/// Go `mediaOffloadedText`: the text of an uploaded binary part.
pub(crate) const MEDIA_OFFLOADED_TEXT: &str = "[MEDIA OFFLOADED]";

/// Go `textPreviewRunes`.
pub(crate) const TEXT_PREVIEW_RUNES: usize = 200;

/// Go `textPreviewSuffix`.
pub(crate) const TEXT_PREVIEW_SUFFIX: &str = "... [OFFLOADED]";

/// Go `missingTraceID`.
pub(crate) const MISSING_TRACE_ID: &str = "no_trace";

/// Go `missingSpanID`.
pub(crate) const MISSING_SPAN_ID: &str = "no_span";

/// Go `missingEventID`.
pub(crate) const MISSING_EVENT_ID: &str = "no_event";

/// Go `fallbackExtension`.
pub(crate) const FALLBACK_EXTENSION: &str = ".bin";

/// Go `mimeExtensions`, sorted by type.
pub(crate) const MIME_EXTENSIONS: [(&str, &str); 11] = [
    ("application/json", ".json"),
    ("application/pdf", ".pdf"),
    ("audio/mpeg", ".mp3"),
    ("audio/wav", ".wav"),
    ("image/gif", ".gif"),
    ("image/jpeg", ".jpg"),
    ("image/png", ".png"),
    ("image/webp", ".webp"),
    ("text/plain", ".txt"),
    ("video/mp4", ".mp4"),
    ("video/webm", ".webm"),
];

/// Go `googleapi.DefaultUploadChunkSize` (16 MiB): payloads below it are uploaded in one
/// multipart request, others as a resumable upload in chunks of this size.
pub(crate) const DEFAULT_UPLOAD_CHUNK_SIZE: usize = 16 * 1024 * 1024;

/// Go's upload sentinel errors (`errors.New` / `fmt.Errorf` values compared with `errors.Is`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadSentinel {
    /// Go `errInvalidGCSURI`.
    InvalidGcsUri,
    /// Go `errUploadCooldown`.
    UploadCooldown,
    /// Go `errBatchUploadTimeout`: wraps `context.DeadlineExceeded`.
    BatchUploadTimeout,
}

impl fmt::Display for UploadSentinel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UploadSentinel::InvalidGcsUri => "offloader returned a URI that is not a gs:// URI",
            UploadSentinel::UploadCooldown => "upload skipped during timeout cooldown",
            UploadSentinel::BatchUploadTimeout => "batch upload timeout: context deadline exceeded",
        })
    }
}

impl std::error::Error for UploadSentinel {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UploadSentinel::BatchUploadTimeout => Some(&ContextError::DeadlineExceeded),
            _ => None,
        }
    }
}

impl UploadSentinel {
    /// The sentinel as an [`Error`].
    pub(crate) fn error(self) -> Error {
        Error::Other(Box::new(self))
    }

    /// Go `errors.Is(err, <sentinel>)`.
    pub(crate) fn is_in(self, err: &Error) -> bool {
        err.find(&mut |e| {
            matches!(e, Error::Other(b) if b.downcast_ref::<UploadSentinel>() == Some(&self))
        })
        .is_some()
    }
}

/// Go `offloaderPanicError`: a panic of an offloader, naming only its value's type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OffloaderPanicError {
    /// Go `valueType` (`%T` of the panic value).
    pub(crate) value_type: String,
}

impl fmt::Display for OffloaderPanicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "offloader panicked ({})", self.value_type)
    }
}

impl std::error::Error for OffloaderPanicError {}

/// Go `ownedUploadError`: an error of the built-in Cloud Storage offloader, whose text is safe
/// to log once scrubbed. `Display` and `source` are the wrapped error's.
#[derive(Debug)]
pub(crate) struct OwnedUploadError(pub(crate) Error);

impl fmt::Display for OwnedUploadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for OwnedUploadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

impl OwnedUploadError {
    /// `err` wrapped as Go's `ownedUploadError{err: err}`.
    pub(crate) fn wrap(err: Error) -> Error {
        Error::Other(Box::new(OwnedUploadError(err)))
    }

    /// Go `errors.As(err, &owned)`: the first `OwnedUploadError` in `err`'s tree.
    pub(crate) fn find(err: &Error) -> Option<&OwnedUploadError> {
        match err.find(&mut |e| matches!(e, Error::Other(b) if b.is::<OwnedUploadError>())) {
            Some(Error::Other(b)) => b.downcast_ref::<OwnedUploadError>(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;

    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::config::{DEFAULT_MAX_PENDING_UPLOAD_BYTES, GCS_UPLOAD_TIMEOUT};
    use crate::formatter::{TEXT_UPLOAD_CONTENT_TYPE, UPLOAD_FAILED_TEXT};

    #[test]
    fn constants_match_go() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/helpers/constants.json");
        let go: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(go["mediaOffloadedText"], MEDIA_OFFLOADED_TEXT);
        assert_eq!(go["uploadFailedText"], UPLOAD_FAILED_TEXT);
        assert_eq!(go["textPreviewRunes"], TEXT_PREVIEW_RUNES);
        assert_eq!(go["textPreviewSuffix"], TEXT_PREVIEW_SUFFIX);
        assert_eq!(go["textUploadContentType"], TEXT_UPLOAD_CONTENT_TYPE);
        assert_eq!(go["missingTraceID"], MISSING_TRACE_ID);
        assert_eq!(go["missingSpanID"], MISSING_SPAN_ID);
        assert_eq!(go["missingEventID"], MISSING_EVENT_ID);
        assert_eq!(go["fallbackExtension"], FALLBACK_EXTENSION);
        assert_eq!(
            go["defaultMaxPendingUploadBytes"],
            DEFAULT_MAX_PENDING_UPLOAD_BYTES
        );
        assert_eq!(
            go["gcsUploadTimeout_ns"],
            u64::try_from(GCS_UPLOAD_TIMEOUT.as_nanos()).unwrap()
        );
        assert_eq!(
            go["googleapiDefaultUploadChunkSize"],
            DEFAULT_UPLOAD_CHUNK_SIZE
        );
        assert_eq!(go["mimeExtensions"], json!(MIME_EXTENSIONS));
        assert_eq!(
            go["errInvalidGCSURI"],
            UploadSentinel::InvalidGcsUri.to_string()
        );
        assert_eq!(
            go["errUploadCooldown"],
            UploadSentinel::UploadCooldown.to_string()
        );
        assert_eq!(
            go["errBatchUploadTimeout"],
            UploadSentinel::BatchUploadTimeout.to_string()
        );
    }

    #[test]
    fn sentinels_match_through_wrapping_and_the_timeout_is_a_deadline() {
        let timeout = UploadSentinel::BatchUploadTimeout.error();
        assert!(UploadSentinel::BatchUploadTimeout.is_in(&timeout));
        assert!(!UploadSentinel::UploadCooldown.is_in(&timeout));
        assert!(timeout.is_context(ContextError::DeadlineExceeded));
        assert!(!timeout.is_context(ContextError::Canceled));
        let wrapped = Error::wrap("upload", UploadSentinel::InvalidGcsUri.error());
        assert!(UploadSentinel::InvalidGcsUri.is_in(&wrapped));
        assert!(!wrapped.is_context(ContextError::DeadlineExceeded));
        assert_eq!(
            wrapped.to_string(),
            "upload: offloader returned a URI that is not a gs:// URI"
        );
    }

    #[test]
    fn owned_and_panic_errors_keep_go_texts() {
        let owned = OwnedUploadError::wrap(Error::wrap(
            "failed to write the Cloud Storage object",
            Error::msg("boom"),
        ));
        assert_eq!(
            owned.to_string(),
            "failed to write the Cloud Storage object: boom"
        );
        assert!(OwnedUploadError::find(&owned).is_some());
        assert!(OwnedUploadError::find(&Error::wrap("x", owned)).is_some());
        assert!(OwnedUploadError::find(&Error::msg("boom")).is_none());
        let panic = OffloaderPanicError {
            value_type: "string".into(),
        };
        assert_eq!(panic.to_string(), "offloader panicked (string)");
    }
}
