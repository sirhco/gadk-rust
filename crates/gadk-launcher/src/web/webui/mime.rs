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

//! Go's built-in MIME table (`mime.builtinTypes`) and `http.DetectContentType` for the web UI
//! (Task 10).
//!
//! `http.ServeContent` types a file by `mime.TypeByExtension(filepath.Ext(name))` and, when that is
//! empty, by sniffing its first 512 bytes. Go's table is the built-in one augmented by the
//! system's MIME database on Unix (`/etc/mime.types`, `/etc/apache2/mime.types`, FreeDesktop
//! `globs2`); gadk uses the built-in table alone, so every machine answers alike (ledger 6). For
//! the bundle's extensions (`.css`, `.html`, `.js`, `.json`, `.svg`) both give the same types.

/// Go `mime.builtinTypesLower` (Go 1.27 `mime/type.go`), sorted by extension.
const BUILTIN_TYPES: &[(&str, &str)] = &[
    (".ai", "application/postscript"),
    (".apk", "application/vnd.android.package-archive"),
    (".apng", "image/apng"),
    (".avif", "image/avif"),
    (".bin", "application/octet-stream"),
    (".bmp", "image/bmp"),
    (".com", "application/octet-stream"),
    (".css", "text/css; charset=utf-8"),
    (".csv", "text/csv; charset=utf-8"),
    (".doc", "application/msword"),
    (
        ".docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (".ehtml", "text/html; charset=utf-8"),
    (".eml", "message/rfc822"),
    (".eps", "application/postscript"),
    (".exe", "application/octet-stream"),
    (".flac", "audio/flac"),
    (".gif", "image/gif"),
    (".gz", "application/gzip"),
    (".htm", "text/html; charset=utf-8"),
    (".html", "text/html; charset=utf-8"),
    (".ico", "image/vnd.microsoft.icon"),
    (".ics", "text/calendar; charset=utf-8"),
    (".jfif", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".jpg", "image/jpeg"),
    (".js", "text/javascript; charset=utf-8"),
    (".json", "application/json"),
    (".m4a", "audio/mp4"),
    (".mjs", "text/javascript; charset=utf-8"),
    (".mp3", "audio/mpeg"),
    (".mp4", "video/mp4"),
    (".oga", "audio/ogg"),
    (".ogg", "audio/ogg"),
    (".ogv", "video/ogg"),
    (".opus", "audio/ogg"),
    (".pdf", "application/pdf"),
    (".pjp", "image/jpeg"),
    (".pjpeg", "image/jpeg"),
    (".png", "image/png"),
    (".ppt", "application/vnd.ms-powerpoint"),
    (
        ".pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    (".ps", "application/postscript"),
    (".rdf", "application/rdf+xml"),
    (".rtf", "application/rtf"),
    (".shtml", "text/html; charset=utf-8"),
    (".svg", "image/svg+xml"),
    (".text", "text/plain; charset=utf-8"),
    (".tif", "image/tiff"),
    (".tiff", "image/tiff"),
    (".txt", "text/plain; charset=utf-8"),
    (".vtt", "text/vtt; charset=utf-8"),
    (".wasm", "application/wasm"),
    (".wav", "audio/wav"),
    (".weba", "audio/webm"),
    (".webm", "video/webm"),
    (".webp", "image/webp"),
    (".xbl", "text/xml; charset=utf-8"),
    (".xbm", "image/x-xbitmap"),
    (".xht", "application/xhtml+xml"),
    (".xhtml", "application/xhtml+xml"),
    (".xls", "application/vnd.ms-excel"),
    (
        ".xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (".xml", "text/xml; charset=utf-8"),
    (".xsl", "text/xml; charset=utf-8"),
    (".zip", "application/zip"),
];

/// Go `mime.TypeByExtension(ext)` over the built-in table: `ext` (with its leading dot) looked up
/// as given, then lower-cased; `""` when unknown.
pub(super) fn type_by_extension(ext: &str) -> &'static str {
    let find = |e: &str| {
        BUILTIN_TYPES
            .binary_search_by(|(k, _)| (*k).cmp(e))
            .map_or("", |i| BUILTIN_TYPES[i].1)
    };
    match find(ext) {
        "" => find(&ext.to_lowercase()),
        found => found,
    }
}

/// Go `internal.SniffLen`: the sniffer reads at most this many bytes.
pub(super) const SNIFF_LEN: usize = 512;

/// Go `http.DetectContentType` (`net/http/internal/sniff.go`): the WHATWG MIME sniffing
/// algorithm over the first [`SNIFF_LEN`] bytes, `application/octet-stream` when nothing matches.
pub(super) fn detect_content_type(data: &[u8]) -> &'static str {
    let data = &data[..data.len().min(SNIFF_LEN)];
    let first_non_ws = data.iter().position(|&b| !is_ws(b)).unwrap_or(data.len());
    SNIFF_SIGNATURES
        .iter()
        .find_map(|sig| sig.matches(data, first_non_ws))
        .unwrap_or("application/octet-stream")
}

/// Go `isWS`: the whitespace bytes of the MIME sniffing spec.
fn is_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

/// Go `isTT`: a tag-terminating byte.
fn is_tt(b: u8) -> bool {
    matches!(b, b' ' | b'>')
}

/// Go's `sniffSig` implementations.
enum Sig {
    /// Go `htmlSig`.
    Html(&'static [u8]),
    /// Go `maskedSig`.
    Masked {
        mask: &'static [u8],
        pat: &'static [u8],
        skip_ws: bool,
        ct: &'static str,
    },
    /// Go `exactSig`.
    Exact(&'static [u8], &'static str),
    /// Go `mp4Sig`.
    Mp4,
    /// Go `textSig`.
    Text,
}

const fn masked(mask: &'static [u8], pat: &'static [u8], ct: &'static str) -> Sig {
    Sig::Masked {
        mask,
        pat,
        skip_ws: false,
        ct,
    }
}

/// Go `sniffSignatures`, in order.
const SNIFF_SIGNATURES: &[Sig] = &[
    Sig::Html(b"<!DOCTYPE HTML"),
    Sig::Html(b"<HTML"),
    Sig::Html(b"<HEAD"),
    Sig::Html(b"<SCRIPT"),
    Sig::Html(b"<IFRAME"),
    Sig::Html(b"<H1"),
    Sig::Html(b"<DIV"),
    Sig::Html(b"<FONT"),
    Sig::Html(b"<TABLE"),
    Sig::Html(b"<A"),
    Sig::Html(b"<STYLE"),
    Sig::Html(b"<TITLE"),
    Sig::Html(b"<B"),
    Sig::Html(b"<BODY"),
    Sig::Html(b"<BR"),
    Sig::Html(b"<P"),
    Sig::Html(b"<!--"),
    Sig::Masked {
        mask: b"\xFF\xFF\xFF\xFF\xFF",
        pat: b"<?xml",
        skip_ws: true,
        ct: "text/xml; charset=utf-8",
    },
    Sig::Exact(b"%PDF-", "application/pdf"),
    Sig::Exact(b"%!PS-Adobe-", "application/postscript"),
    // UTF BOMs.
    masked(
        b"\xFF\xFF\x00\x00",
        b"\xFE\xFF\x00\x00",
        "text/plain; charset=utf-16be",
    ),
    masked(
        b"\xFF\xFF\x00\x00",
        b"\xFF\xFE\x00\x00",
        "text/plain; charset=utf-16le",
    ),
    masked(
        b"\xFF\xFF\xFF\x00",
        b"\xEF\xBB\xBF\x00",
        "text/plain; charset=utf-8",
    ),
    // Image types.
    Sig::Exact(b"\x00\x00\x01\x00", "image/x-icon"),
    Sig::Exact(b"\x00\x00\x02\x00", "image/x-icon"),
    Sig::Exact(b"BM", "image/bmp"),
    Sig::Exact(b"GIF87a", "image/gif"),
    Sig::Exact(b"GIF89a", "image/gif"),
    masked(
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF\xFF\xFF",
        b"RIFF\x00\x00\x00\x00WEBPVP",
        "image/webp",
    ),
    Sig::Exact(b"\x89PNG\x0D\x0A\x1A\x0A", "image/png"),
    Sig::Exact(b"\xFF\xD8\xFF", "image/jpeg"),
    // Audio and video types.
    masked(
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        b"FORM\x00\x00\x00\x00AIFF",
        "audio/aiff",
    ),
    masked(b"\xFF\xFF\xFF", b"ID3", "audio/mpeg"),
    masked(b"\xFF\xFF\xFF\xFF\xFF", b"OggS\x00", "application/ogg"),
    masked(
        b"\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF",
        b"MThd\x00\x00\x00\x06",
        "audio/midi",
    ),
    masked(
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        b"RIFF\x00\x00\x00\x00AVI ",
        "video/avi",
    ),
    masked(
        b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF",
        b"RIFF\x00\x00\x00\x00WAVE",
        "audio/wave",
    ),
    Sig::Mp4,
    Sig::Exact(b"\x1A\x45\xDF\xA3", "video/webm"),
    // Font types: 34 NUL bytes (masked out) followed by "LP".
    masked(
        b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xFF\xFF",
        b"\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00LP",
        "application/vnd.ms-fontobject",
    ),
    Sig::Exact(b"\x00\x01\x00\x00", "font/ttf"),
    Sig::Exact(b"OTTO", "font/otf"),
    Sig::Exact(b"ttcf", "font/collection"),
    Sig::Exact(b"wOFF", "font/woff"),
    Sig::Exact(b"wOF2", "font/woff2"),
    // Archive types.
    Sig::Exact(b"\x1F\x8B\x08", "application/x-gzip"),
    Sig::Exact(b"PK\x03\x04", "application/zip"),
    Sig::Exact(b"Rar!\x1A\x07\x00", "application/x-rar-compressed"),
    Sig::Exact(b"Rar!\x1A\x07\x01\x00", "application/x-rar-compressed"),
    Sig::Exact(b"\x00\x61\x73\x6D", "application/wasm"),
    // Should be last.
    Sig::Text,
];

impl Sig {
    /// Go `sniffSig.match`: the type, or `None` when the signature does not match.
    fn matches(&self, data: &[u8], first_non_ws: usize) -> Option<&'static str> {
        match self {
            Sig::Exact(sig, ct) => data.starts_with(sig).then_some(*ct),
            Sig::Masked {
                mask,
                pat,
                skip_ws,
                ct,
            } => {
                let data = if *skip_ws {
                    &data[first_non_ws..]
                } else {
                    data
                };
                if pat.len() != mask.len() || data.len() < pat.len() {
                    return None;
                }
                pat.iter()
                    .zip(mask.iter())
                    .zip(data)
                    .all(|((p, m), d)| d & m == *p)
                    .then_some(*ct)
            }
            Sig::Html(h) => {
                let data = &data[first_non_ws..];
                if data.len() < h.len() + 1 {
                    return None;
                }
                for (&b, &d) in h.iter().zip(data) {
                    let d = if b.is_ascii_uppercase() { d & 0xDF } else { d };
                    if b != d {
                        return None;
                    }
                }
                // Next byte must be a tag-terminating byte.
                is_tt(data[h.len()]).then_some("text/html; charset=utf-8")
            }
            Sig::Mp4 => mp4(data),
            Sig::Text => data[first_non_ws..]
                .iter()
                .all(|&b| !matches!(b, 0x00..=0x08 | 0x0B | 0x0E..=0x1A | 0x1C..=0x1F))
                .then_some("text/plain; charset=utf-8"),
        }
    }
}

/// Go `mp4Sig.match` (mimesniff § 6.2.1).
fn mp4(data: &[u8]) -> Option<&'static str> {
    if data.len() < 12 {
        return None;
    }
    let box_size =
        usize::try_from(u32::from_be_bytes([data[0], data[1], data[2], data[3]])).ok()?;
    if data.len() < box_size || box_size % 4 != 0 {
        return None;
    }
    if &data[4..8] != b"ftyp" {
        return None;
    }
    let mut st = 8;
    while st < box_size {
        // Bytes 12..16 are the major brand's version number.
        if st != 12 && &data[st..st + 3] == b"mp4" {
            return Some("video/mp4");
        }
        st += 4;
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn builtin_table_is_sorted_and_go_lookups_hold() {
        assert!(BUILTIN_TYPES.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(BUILTIN_TYPES.len(), 65);
        assert_eq!(type_by_extension(".js"), "text/javascript; charset=utf-8");
        assert_eq!(type_by_extension(".css"), "text/css; charset=utf-8");
        assert_eq!(type_by_extension(".html"), "text/html; charset=utf-8");
        assert_eq!(type_by_extension(".svg"), "image/svg+xml");
        assert_eq!(type_by_extension(".json"), "application/json");
        // Case-insensitive fallback (Go: case-sensitive first, then lower-cased).
        assert_eq!(type_by_extension(".PNG"), "image/png");
        assert_eq!(type_by_extension(".Html"), "text/html; charset=utf-8");
        // Unicode lower-casing: KELVIN SIGN lower-cases to `k`, as in Go's `strings.ToLower`.
        assert_eq!(
            type_by_extension(".ap\u{212A}"),
            "application/vnd.android.package-archive"
        );
        assert_eq!(type_by_extension(".map"), "");
        assert_eq!(type_by_extension(".woff2"), "");
        assert_eq!(type_by_extension(""), "");
        assert_eq!(type_by_extension("js"), "");
    }

    /// Go `net/http/sniff_test.go` `sniffTests`.
    #[test]
    fn detect_content_type_matches_go_sniff_tests() {
        let cases: &[(&str, &[u8], &str)] = &[
            ("Empty", b"", "text/plain; charset=utf-8"),
            ("Binary", &[1, 2, 3], "application/octet-stream"),
            (
                "HTML document #1",
                b"<HtMl><bOdY>blah blah blah</body></html>",
                "text/html; charset=utf-8",
            ),
            (
                "HTML document #2",
                b"<HTML></HTML>",
                "text/html; charset=utf-8",
            ),
            (
                "HTML document #3 (leading whitespace)",
                b"   <!DOCTYPE HTML>...",
                "text/html; charset=utf-8",
            ),
            (
                "HTML document #4 (leading CRLF)",
                b"\r\n<html>...",
                "text/html; charset=utf-8",
            ),
            (
                "Plain text",
                "This is not HTML. It has \u{2603} though.".as_bytes(),
                "text/plain; charset=utf-8",
            ),
            ("XML", b"\n<?xml!", "text/xml; charset=utf-8"),
            ("Windows icon", b"\x00\x00\x01\x00", "image/x-icon"),
            ("Windows cursor", b"\x00\x00\x02\x00", "image/x-icon"),
            ("BMP image", b"BM...", "image/bmp"),
            ("GIF 87a", b"GIF87a", "image/gif"),
            ("GIF 89a", b"GIF89a...", "image/gif"),
            ("WEBP image", b"RIFF\x00\x00\x00\x00WEBPVP", "image/webp"),
            ("PNG image", b"\x89PNG\x0D\x0A\x1A\x0A", "image/png"),
            ("JPEG image", b"\xFF\xD8\xFF", "image/jpeg"),
            ("MIDI audio", b"MThd\x00\x00\x00\x06\x00\x01", "audio/midi"),
            (
                "MP3 audio/MPEG audio",
                b"ID3\x03\x00\x00\x00\x00\x0f",
                "audio/mpeg",
            ),
            (
                "WAV audio #1",
                b"RIFFb\xb8\x00\x00WAVEfmt \x12\x00\x00\x00\x06",
                "audio/wave",
            ),
            (
                "WAV audio #2",
                b"RIFF,\x00\x00\x00WAVEfmt \x12\x00\x00\x00\x06",
                "audio/wave",
            ),
            (
                "AIFF audio #1",
                b"FORM\x00\x00\x00\x00AIFFCOMM\x00\x00\x00\x12\x00\x01\x00\x00\x57\x55\x00\x10\x40\x0d\xf3\x34",
                "audio/aiff",
            ),
            (
                "OGG audio",
                b"OggS\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00\x7e\x46\x00\x00\x00\x00\x00\x00\x1f\xf6\xb4\xfc\x01\x1e\x01\x76\x6f\x72",
                "application/ogg",
            ),
            ("Must not match OGG", b"owow\x00", "application/octet-stream"),
            ("Must not match OGG", b"oooS\x00", "application/octet-stream"),
            ("Must not match OGG", b"oggS\x00", "application/octet-stream"),
            (
                "MP4 video",
                b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom<\x06t\xbfmdat",
                "video/mp4",
            ),
            (
                "AVI video #1",
                b"RIFF,O\n\x00AVI LIST\xc3\x80",
                "video/avi",
            ),
            (
                "AVI video #2",
                b"RIFF,\n\x00\x00AVI LIST\xc3\x80",
                "video/avi",
            ),
            (
                "TTF sample  I",
                b"\x00\x01\x00\x00\x00\x17\x01\x00\x00\x04\x01\x60\x4f",
                "font/ttf",
            ),
            (
                "TTF sample II",
                b"\x00\x01\x00\x00\x00\x0e\x00\x80\x00\x03\x00\x60\x46",
                "font/ttf",
            ),
            (
                "OTTO sample  I",
                b"\x4f\x54\x54\x4f\x00\x0e\x00\x80\x00\x03\x00\x60\x42\x41\x53\x45",
                "font/otf",
            ),
            (
                "woff sample  I",
                b"\x77\x4f\x46\x46\x00\x01\x00\x00\x00\x00\x30\x54\x00\x0d\x00\x00",
                "font/woff",
            ),
            (
                "woff2 sample",
                b"\x77\x4f\x46\x32\x00\x01\x00\x00\x00",
                "font/woff2",
            ),
            ("wasm sample", b"\x00\x61\x73\x6d\x01\x00", "application/wasm"),
            (
                "RAR v1.5-v4.0",
                b"Rar!\x1A\x07\x00",
                "application/x-rar-compressed",
            ),
            (
                "RAR v5+",
                b"Rar!\x1A\x07\x01\x00",
                "application/x-rar-compressed",
            ),
            (
                "Incorrect RAR v1.5-v4.0",
                b"Rar \x1A\x07\x00",
                "application/octet-stream",
            ),
            (
                "Incorrect RAR v5+",
                b"Rar \x1A\x07\x01\x00",
                "application/octet-stream",
            ),
        ];
        for (desc, data, want) in cases {
            assert_eq!(detect_content_type(data), *want, "{desc}");
        }
    }

    #[test]
    fn detect_content_type_reads_at_most_512_bytes() {
        // A control byte past the sniff window does not make the text binary.
        let mut data = vec![b'a'; SNIFF_LEN];
        data.push(0x01);
        assert_eq!(detect_content_type(&data), "text/plain; charset=utf-8");
        data[SNIFF_LEN - 1] = 0x01;
        assert_eq!(detect_content_type(&data), "application/octet-stream");
        // An HTML signature needs its tag-terminating byte.
        assert_eq!(detect_content_type(b"<html"), "text/plain; charset=utf-8");
        assert_eq!(detect_content_type(b"  <!-- x"), "text/html; charset=utf-8");
    }
}
