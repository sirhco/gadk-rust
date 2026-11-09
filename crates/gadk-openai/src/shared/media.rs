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

//! Media classification both APIs share (port of ADK-Go `internal/shared/media.go`).

#![allow(dead_code, unused_variables, clippy::todo)] // D1: Task 3 fills the bodies.

use bytes::Bytes;
use gadk_genai::Part;

use crate::error::OpenAiError;

/// Go `MediaKind`: what a part's media is, as its MIME type reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MediaKind {
    /// `"image"`: an `image/` type.
    Image,
    /// `"audio"`: an `audio/` type.
    Audio,
    /// `"video"`: a `video/` type.
    Video,
    /// `"file"`: any other type.
    File,
}

impl MediaKind {
    /// Go's string value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::File => "file",
        }
    }
}

/// Go `MediaSource`: where the bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MediaSource {
    /// `"data"`: raw bytes in the request.
    Data,
    /// `"file_id"`: a file already uploaded to the provider (a URI starting `file-`).
    FileId,
    /// `"url"`: a location the provider fetches.
    Url,
}

impl MediaSource {
    /// Go's string value (error texts print it with `%s`).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::FileId => "file_id",
            Self::Url => "url",
        }
    }
}

/// Go `Media`: a part's `InlineData` or `FileData` reduced to plain values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Media {
    /// What the MIME type says the media is.
    pub(crate) kind: MediaKind,
    /// Where the bytes are.
    pub(crate) source: MediaSource,
    /// The type as the part declared it, parameters and all.
    pub(crate) mime_type: String,
    /// The part's display name; inline data without one is `inline_data`.
    pub(crate) filename: String,
    /// The raw bytes ([`MediaSource::Data`] only).
    pub(crate) data: Bytes,
    /// The provider's file id ([`MediaSource::FileId`] only).
    pub(crate) file_id: String,
    /// The location to fetch ([`MediaSource::Url`] only).
    pub(crate) url: String,
}

impl Media {
    /// Go `Media.DataURL`: `data:<trimmed mime type>;base64,<standard base64>` for inline
    /// media, `""` otherwise.
    pub(crate) fn data_url(&self) -> String {
        todo!("Task 3")
    }
}

/// Go `ClassifyMedia`: the part's media, `InlineData` before `FileData`; empty for a part
/// with none. Inline data without a MIME type is `ErrMediaMIMETypeRequired: inline data`,
/// without bytes `ErrMediaDataRequired: inline data of type <mime>`; file data without a URI
/// `ErrMediaURIRequired: file data`, without a MIME type `ErrMediaMIMETypeRequired: file data`.
pub(crate) fn classify_media(part: &Part) -> Result<Vec<Media>, OpenAiError> {
    todo!("Task 3")
}
