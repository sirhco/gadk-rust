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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use gadk_genai::{Blob, FileData, Part};
use gadk_platform::go_to_lower;

use crate::error::{ErrorKind, OpenAiError};

/// Go `fileIDPrefix`: a file URI naming an upload to the provider.
const FILE_ID_PREFIX: &str = "file-";

/// Go `defaultInlineFilename`: the filename of inline data without a display name.
const DEFAULT_INLINE_FILENAME: &str = "inline_data";

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
        if self.source != MediaSource::Data {
            return String::new();
        }
        format!(
            "data:{};base64,{}",
            self.mime_type.trim(),
            STANDARD.encode(&self.data)
        )
    }
}

/// Go `ClassifyMedia`: the part's media, `InlineData` before `FileData`; empty for a part
/// with none. Inline data without a MIME type is `ErrMediaMIMETypeRequired: inline data`,
/// without bytes `ErrMediaDataRequired: inline data of type <mime>`; file data without a URI
/// `ErrMediaURIRequired: file data`, without a MIME type `ErrMediaMIMETypeRequired: file data`.
pub(crate) fn classify_media(part: &Part) -> Result<Vec<Media>, OpenAiError> {
    let mut out = Vec::new();
    if let Some(blob) = &part.inline_data {
        out.push(classify_inline_data(blob)?);
    }
    if let Some(fd) = &part.file_data {
        out.push(classify_file_data(fd)?);
    }
    Ok(out)
}

/// Go `classifyInlineData`.
fn classify_inline_data(blob: &Blob) -> Result<Media, OpenAiError> {
    if blob.mime_type.is_empty() {
        return Err(OpenAiError::new(
            ErrorKind::MediaMimeTypeRequired,
            "inline data",
        ));
    }
    if blob.data.is_empty() {
        return Err(OpenAiError::new(
            ErrorKind::MediaDataRequired,
            format!("inline data of type {}", blob.mime_type),
        ));
    }
    Ok(Media {
        kind: media_kind_of(&blob.mime_type),
        source: MediaSource::Data,
        mime_type: blob.mime_type.clone(),
        filename: if blob.display_name.is_empty() {
            DEFAULT_INLINE_FILENAME.to_string()
        } else {
            blob.display_name.clone()
        },
        data: blob.data.clone(),
        file_id: String::new(),
        url: String::new(),
    })
}

/// Go `classifyFileData`. The URI is never echoed in an error: it may carry a signature.
fn classify_file_data(fd: &FileData) -> Result<Media, OpenAiError> {
    if fd.file_uri.is_empty() {
        return Err(OpenAiError::new(ErrorKind::MediaUriRequired, "file data"));
    }
    if fd.mime_type.is_empty() {
        return Err(OpenAiError::new(
            ErrorKind::MediaMimeTypeRequired,
            "file data",
        ));
    }
    let (source, file_id, url) = if fd.file_uri.starts_with(FILE_ID_PREFIX) {
        (MediaSource::FileId, fd.file_uri.clone(), String::new())
    } else {
        (MediaSource::Url, String::new(), fd.file_uri.clone())
    };
    Ok(Media {
        kind: media_kind_of(&fd.mime_type),
        source,
        mime_type: fd.mime_type.clone(),
        filename: fd.display_name.clone(),
        data: Bytes::new(),
        file_id,
        url,
    })
}

/// Go `mediaKindOf`: the kind by the type's top-level tree, ignoring case, surrounding
/// whitespace and parameters.
fn media_kind_of(mime_type: &str) -> MediaKind {
    let essence = go_to_lower(mime_type.trim());
    if essence.starts_with("image/") {
        MediaKind::Image
    } else if essence.starts_with("audio/") {
        MediaKind::Audio
    } else if essence.starts_with("video/") {
        MediaKind::Video
    } else {
        MediaKind::File
    }
}
