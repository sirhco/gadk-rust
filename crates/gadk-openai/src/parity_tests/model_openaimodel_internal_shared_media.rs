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

//! Port of ADK-Go `model/openaimodel/internal/shared/media_test.go`.

use bytes::Bytes;
use gadk_genai::{Blob, FileData, Part};
use pretty_assertions::assert_eq;

use crate::error::ErrorKind;
use crate::shared::media::{Media, MediaKind, MediaSource, classify_media};

fn inline(mime_type: &str, data: &'static [u8]) -> Part {
    Part {
        inline_data: Some(Blob {
            mime_type: mime_type.to_string(),
            data: Bytes::from_static(data),
            ..Blob::default()
        }),
        ..Part::default()
    }
}

fn file(mime_type: &str, file_uri: &str) -> FileData {
    FileData {
        mime_type: mime_type.to_string(),
        file_uri: file_uri.to_string(),
        ..FileData::default()
    }
}

/// A [`Media`] with only its kind, source and type set (Go's zero fields elsewhere).
fn media(kind: MediaKind, source: MediaSource, mime_type: &str) -> Media {
    Media {
        kind,
        source,
        mime_type: mime_type.to_string(),
        filename: String::new(),
        data: Bytes::new(),
        file_id: String::new(),
        url: String::new(),
    }
}

/// Inline media as `ClassifyMedia` reports it.
fn data_media(kind: MediaKind, mime_type: &str, filename: &str, data: &'static [u8]) -> Media {
    Media {
        filename: filename.to_string(),
        data: Bytes::from_static(data),
        ..media(kind, MediaSource::Data, mime_type)
    }
}

// parity: adapted: `classify_media` takes a `&Part`, so Go's "nil part" case classifies an empty part, which Go answers the same way (no media, no error)
#[test]
fn test_classify_media() {
    let tests: Vec<(&str, Part, Vec<Media>)> = vec![
        ("nil part", Part::default(), vec![]),
        (
            "part without media",
            Part {
                text: "just words".to_string(),
                ..Part::default()
            },
            vec![],
        ),
        (
            "inline image",
            inline("image/png", &[0x89, b'P', b'N', b'G']),
            vec![data_media(
                MediaKind::Image,
                "image/png",
                "inline_data",
                &[0x89, b'P', b'N', b'G'],
            )],
        ),
        (
            "inline pdf keeps its display name",
            Part {
                inline_data: Some(Blob {
                    mime_type: "application/pdf".to_string(),
                    display_name: "invoice.pdf".to_string(),
                    data: Bytes::from_static(b"%PDF-1.7"),
                }),
                ..Part::default()
            },
            vec![data_media(
                MediaKind::File,
                "application/pdf",
                "invoice.pdf",
                b"%PDF-1.7",
            )],
        ),
        (
            "inline audio is labeled, not rejected here",
            inline("audio/mpeg", b"ID3"),
            vec![data_media(
                MediaKind::Audio,
                "audio/mpeg",
                "inline_data",
                b"ID3",
            )],
        ),
        (
            "inline video is labeled, not rejected here",
            inline("video/mp4", b"ftyp"),
            vec![data_media(
                MediaKind::Video,
                "video/mp4",
                "inline_data",
                b"ftyp",
            )],
        ),
        // The point of splitting kind from source: an uploaded image is an image whose bytes
        // are a file id, and the field an endpoint reaches for follows the source.
        (
            "image by file id is a file id, not a url",
            Part {
                file_data: Some(file("image/jpeg", "file-abc123")),
                ..Part::default()
            },
            vec![Media {
                file_id: "file-abc123".to_string(),
                ..media(MediaKind::Image, MediaSource::FileId, "image/jpeg")
            }],
        ),
        (
            "image by url",
            Part {
                file_data: Some(file("image/webp", "https://example.test/cat.webp")),
                ..Part::default()
            },
            vec![Media {
                url: "https://example.test/cat.webp".to_string(),
                ..media(MediaKind::Image, MediaSource::Url, "image/webp")
            }],
        ),
        (
            "pdf by url",
            Part {
                file_data: Some(FileData {
                    display_name: "spec.pdf".to_string(),
                    ..file("application/pdf", "https://example.test/spec.pdf")
                }),
                ..Part::default()
            },
            vec![Media {
                filename: "spec.pdf".to_string(),
                url: "https://example.test/spec.pdf".to_string(),
                ..media(MediaKind::File, MediaSource::Url, "application/pdf")
            }],
        ),
        // Case, surrounding whitespace and trailing parameters do not change what the media
        // is; the declared type is kept verbatim, as it goes on the wire.
        (
            "case, padding and parameters do not change the kind",
            inline("  IMAGE/PNG; charset=binary ", &[0x89]),
            vec![data_media(
                MediaKind::Image,
                "  IMAGE/PNG; charset=binary ",
                "inline_data",
                &[0x89],
            )],
        ),
        // A part can hold both, and the order has to be the one the request will carry.
        (
            "inline data comes before file data",
            Part {
                file_data: Some(file("application/pdf", "file-xyz")),
                ..inline("image/png", &[0x89])
            },
            vec![
                data_media(MediaKind::Image, "image/png", "inline_data", &[0x89]),
                Media {
                    file_id: "file-xyz".to_string(),
                    ..media(MediaKind::File, MediaSource::FileId, "application/pdf")
                },
            ],
        ),
    ];

    for (name, part, want) in tests {
        let got = classify_media(&part).unwrap_or_else(|err| panic!("{name}: err = {err}"));
        assert_eq!(got, want, "{name}");
    }
}

#[test]
fn test_classify_media_errors() {
    // (name, part, wantErr, wantNamed, wantAbsent)
    let tests: Vec<(&str, Part, ErrorKind, &str, &str)> = vec![
        (
            "inline data without a mime type",
            inline("", &[0x89]),
            ErrorKind::MediaMimeTypeRequired,
            "inline data",
            "",
        ),
        (
            "file data without a mime type does not echo the uri",
            Part {
                file_data: Some(file("", "https://example.test/mystery?X-Signature=secret")),
                ..Part::default()
            },
            ErrorKind::MediaMimeTypeRequired,
            "file data",
            "example.test",
        ),
        (
            "inline data with no bytes names the type",
            inline("image/png", &[]),
            ErrorKind::MediaDataRequired,
            "image/png",
            "",
        ),
        (
            "file data without a uri",
            Part {
                file_data: Some(file("image/png", "")),
                ..Part::default()
            },
            ErrorKind::MediaUriRequired,
            "file data",
            "",
        ),
        // The first field in request order decides the error, so a bad blob is not masked by
        // a good file reference behind it.
        (
            "a bad blob is reported even with sound file data behind it",
            Part {
                file_data: Some(file("image/png", "file-ok")),
                ..inline("", &[0x89])
            },
            ErrorKind::MediaMimeTypeRequired,
            "inline data",
            "",
        ),
    ];

    for (name, part, want_err, want_named, want_absent) in tests {
        // An error carries no partial list: Go's `got` is nil whenever `err` is set.
        let err = match classify_media(&part) {
            Ok(got) => panic!("{name}: ClassifyMedia() = {got:?}, want error {want_err:?}"),
            Err(err) => err,
        };
        assert!(err.is(want_err), "{name}: err = {err}, want {want_err:?}");
        assert!(
            err.to_string().contains(want_named),
            "{name}: err = {err:?}, want it to name {want_named:?}"
        );
        if !want_absent.is_empty() {
            assert!(
                !err.to_string().contains(want_absent),
                "{name}: err = {err}, want it free of {want_absent:?}"
            );
        }
    }
}

#[test]
fn test_media_data_url() {
    let inline = Media {
        data: Bytes::from_static(&[0x89, b'P', b'N', b'G']),
        ..media(MediaKind::File, MediaSource::Data, "image/png")
    };
    assert_eq!(inline.data_url(), "data:image/png;base64,iVBORw==");

    // Media held elsewhere has no bytes to render, and must not be handed a data URL of the
    // empty string, which a server would read as empty media.
    for m in [
        Media {
            file_id: "file-abc".to_string(),
            ..media(MediaKind::File, MediaSource::FileId, "image/png")
        },
        Media {
            url: "https://example.test/a.png".to_string(),
            ..media(MediaKind::File, MediaSource::Url, "image/png")
        },
    ] {
        assert_eq!(
            m.data_url(),
            "",
            "DataURL() for source {}",
            m.source.as_str()
        );
    }
}

/// A data URL trims the declared type (a space inside the URL stops it being one) but keeps
/// its case and parameters.
#[test]
fn test_data_url_trims_the_declared_mime_type() {
    let tests = [
        ("leading space", " image/png", "data:image/png;base64,AQ=="),
        ("trailing space", "image/png ", "data:image/png;base64,AQ=="),
        (
            "surrounding whitespace",
            "\timage/png\n",
            "data:image/png;base64,AQ==",
        ),
        // Case and parameters are the part's to declare.
        (
            "case is left alone",
            "IMAGE/PNG",
            "data:IMAGE/PNG;base64,AQ==",
        ),
        (
            "parameters are kept",
            "image/png;charset=utf-8",
            "data:image/png;charset=utf-8;base64,AQ==",
        ),
    ];
    for (name, mime, want) in tests {
        let media = classify_media(&inline(mime, &[1]))
            .unwrap_or_else(|err| panic!("{name}: ClassifyMedia() err = {err}"));
        assert_eq!(media[0].data_url(), want, "{name}");
    }
}
