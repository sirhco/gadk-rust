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

//! The httprr trace file format and the HTTP/1.x wire encoding of its entries.

use std::path::Path;

use bytes::Bytes;
use http::header::{CONTENT_LENGTH, HOST, TRANSFER_ENCODING};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Version};

use crate::HttprrError;

/// First line of every trace file.
pub(crate) const TRACE_HEADER: &str = "httprr trace v1";

/// One trace entry: `(request wire, response wire)`.
pub(crate) type Entry = (Vec<u8>, Vec<u8>);

/// Parses a whole trace file into entries, in file order. Port of Go `httprr.open`.
pub(crate) fn parse_trace(path: &Path, data: &[u8]) -> Result<Vec<Entry>, HttprrError> {
    let corrupt = || HttprrError::Corrupt(path.to_path_buf());
    let Some((line, mut data)) = cut_line(data) else {
        return Err(HttprrError::NotATrace(path.to_path_buf()));
    };
    if line != TRACE_HEADER.as_bytes() {
        return Err(HttprrError::NotATrace(path.to_path_buf()));
    }
    let mut out = Vec::new();
    while !data.is_empty() {
        let (line, rest) = cut_line(data).ok_or_else(corrupt)?;
        let line = std::str::from_utf8(line).map_err(|_| corrupt())?;
        let (f1, f2) = line.split_once(' ').ok_or_else(corrupt)?;
        let n1: usize = f1.parse().map_err(|_| corrupt())?;
        let n2: usize = f2.parse().map_err(|_| corrupt())?;
        if n1 > rest.len() || n2 > rest.len() - n1 {
            return Err(corrupt());
        }
        out.push((rest[..n1].to_vec(), rest[n1..n1 + n2].to_vec()));
        data = &rest[n1 + n2..];
    }
    Ok(out)
}

/// Splits at the first `\n`: `(line without the \n, rest)`.
fn cut_line(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let i = data.iter().position(|&b| b == b'\n')?;
    Some((&data[..i], &data[i + 1..]))
}

fn wire(msg: impl Into<String>) -> HttprrError {
    HttprrError::Wire(msg.into())
}

/// The start line, header fields and remaining bytes of an HTTP message.
struct Head<'a> {
    start: String,
    headers: Vec<(String, Vec<u8>)>,
    rest: &'a [u8],
}

/// Parses the start line and headers (CRLF or LF line endings) up to the blank line.
fn parse_head(data: &[u8]) -> Result<Head<'_>, HttprrError> {
    let trim = |l: &[u8]| l.strip_suffix(b"\r").unwrap_or(l).to_vec();
    let (start, mut data) = cut_line(data).ok_or_else(|| wire("missing start line"))?;
    let start = String::from_utf8(trim(start)).map_err(|_| wire("non-UTF-8 start line"))?;
    let mut headers = Vec::new();
    loop {
        let (line, rest) = cut_line(data).ok_or_else(|| wire("unexpected end of headers"))?;
        data = rest;
        let line = trim(line);
        if line.is_empty() {
            break;
        }
        let colon = line.iter().position(|&b| b == b':').ok_or_else(|| {
            wire(format!(
                "malformed header line {:?}",
                String::from_utf8_lossy(&line)
            ))
        })?;
        let name =
            String::from_utf8(line[..colon].to_vec()).map_err(|_| wire("non-UTF-8 header name"))?;
        let value = line[colon + 1..].trim_ascii().to_vec();
        headers.push((name, value));
    }
    Ok(Head {
        start,
        headers,
        rest: data,
    })
}

fn header<'h>(headers: &'h [(String, Vec<u8>)], name: &str) -> Option<&'h [u8]> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_slice())
}

/// Truncates `body` to the `Content-Length` header, if present.
fn content_length_body<'a>(
    headers: &[(String, Vec<u8>)],
    body: &'a [u8],
) -> Result<&'a [u8], HttprrError> {
    let Some(cl) = header(headers, "content-length") else {
        return Ok(body);
    };
    let n: usize = std::str::from_utf8(cl)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| wire("invalid Content-Length"))?;
    body.get(..n)
        .ok_or_else(|| wire("body shorter than Content-Length"))
}

/// A request entry from a trace.
#[derive(Debug)]
pub(crate) struct ParsedRequest {
    pub(crate) method: String,
    pub(crate) url: String,
    pub(crate) body: Vec<u8>,
}

/// Parses a request entry: `METHOD URL HTTP/x.y`, headers, blank line, body.
pub(crate) fn parse_request(data: &[u8]) -> Result<ParsedRequest, HttprrError> {
    let head = parse_head(data)?;
    let mut parts = head.start.split(' ');
    let (Some(method), Some(url), Some(proto), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(wire(format!("malformed request line {:?}", head.start)));
    };
    if !proto.starts_with("HTTP/") || method.is_empty() || url.is_empty() {
        return Err(wire(format!("malformed request line {:?}", head.start)));
    }
    let body = content_length_body(&head.headers, head.rest)?.to_vec();
    Ok(ParsedRequest {
        method: method.to_string(),
        url: url.to_string(),
        body,
    })
}

/// Parses a response entry: `HTTP/x.y CODE REASON`, headers, blank line, body.
///
/// The body is de-chunked if `Transfer-Encoding: chunked`, truncated to
/// `Content-Length` if present, and otherwise the remainder of the entry.
pub(crate) fn parse_response(data: &[u8]) -> Result<http::Response<Bytes>, HttprrError> {
    let head = parse_head(data)?;
    let mut parts = head.start.splitn(3, ' ');
    let (Some(proto), Some(code)) = (parts.next(), parts.next()) else {
        return Err(wire(format!("malformed status line {:?}", head.start)));
    };
    let version = match proto {
        "HTTP/0.9" => Version::HTTP_09,
        "HTTP/1.0" => Version::HTTP_10,
        "HTTP/1.1" => Version::HTTP_11,
        "HTTP/2" | "HTTP/2.0" => Version::HTTP_2,
        "HTTP/3" | "HTTP/3.0" => Version::HTTP_3,
        _ => return Err(wire(format!("malformed HTTP version {proto:?}"))),
    };
    let status = code
        .parse::<u16>()
        .ok()
        .and_then(|c| StatusCode::from_u16(c).ok())
        .ok_or_else(|| wire(format!("malformed status code {code:?}")))?;

    let chunked = header(&head.headers, "transfer-encoding").is_some_and(|v| {
        String::from_utf8_lossy(v)
            .to_ascii_lowercase()
            .contains("chunked")
    });
    let body = if chunked {
        dechunk(head.rest)?
    } else {
        content_length_body(&head.headers, head.rest)?.to_vec()
    };

    let mut headers = HeaderMap::new();
    for (name, value) in &head.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| wire(format!("header name {name:?}: {e}")))?;
        if chunked && name == TRANSFER_ENCODING {
            continue;
        }
        let value =
            HeaderValue::from_bytes(value).map_err(|e| wire(format!("header {name}: {e}")))?;
        headers.append(name, value);
    }

    let mut resp = http::Response::new(Bytes::from(body));
    *resp.status_mut() = status;
    *resp.version_mut() = version;
    *resp.headers_mut() = headers;
    Ok(resp)
}

/// Decodes a chunked transfer-encoded body (trailers are ignored).
fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, HttprrError> {
    let mut out = Vec::new();
    loop {
        let (line, rest) = cut_line(data).ok_or_else(|| wire("truncated chunk size"))?;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let size = std::str::from_utf8(line)
            .ok()
            .map(|s| s.split(';').next().unwrap_or("").trim())
            .and_then(|s| usize::from_str_radix(s, 16).ok())
            .ok_or_else(|| wire("malformed chunk size"))?;
        if size == 0 {
            return Ok(out);
        }
        let chunk = rest.get(..size).ok_or_else(|| wire("truncated chunk"))?;
        out.extend_from_slice(chunk);
        let rest = &rest[size..];
        data = rest
            .strip_prefix(b"\r\n")
            .or_else(|| rest.strip_prefix(b"\n"))
            .ok_or_else(|| wire("missing chunk terminator"))?;
    }
}

/// Go's `textproto.CanonicalMIMEHeaderKey` for ASCII names: `x-goog-api-key` →
/// `X-Goog-Api-Key`.
fn canonical_header_name(name: &str) -> String {
    let mut upper = true;
    name.chars()
        .map(|c| {
            let out = if upper {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            };
            upper = c == '-';
            out
        })
        .collect()
}

/// Appends header lines, sorted by name, skipping framing headers.
fn write_headers(out: &mut Vec<u8>, headers: &HeaderMap) {
    let mut lines: Vec<(String, &[u8])> = headers
        .iter()
        .filter(|(n, _)| **n != HOST && **n != CONTENT_LENGTH && **n != TRANSFER_ENCODING)
        .map(|(n, v)| (canonical_header_name(n.as_str()), v.as_bytes()))
        .collect();
    lines.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, value) in lines {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
}

/// Encodes a request as a trace entry.
pub(crate) fn write_request(req: &http::Request<Bytes>) -> Result<Vec<u8>, HttprrError> {
    let uri = req.uri();
    let host = uri
        .authority()
        .ok_or_else(|| wire(format!("request URL {uri} has no host")))?;
    let mut out = format!("{} {} HTTP/1.1\r\nHost: {}\r\n", req.method(), uri, host).into_bytes();
    let body = req.body();
    if !body.is_empty() {
        out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    }
    write_headers(&mut out, req.headers());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    Ok(out)
}

/// Encodes a response as a trace entry.
pub(crate) fn write_response(status: StatusCode, headers: &HeaderMap, body: &[u8]) -> Vec<u8> {
    let reason = status
        .canonical_reason()
        .map(str::to_string)
        .unwrap_or_else(|| format!("status code {}", status.as_u16()));
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n",
        status.as_u16(),
        reason,
        body.len()
    )
    .into_bytes();
    write_headers(&mut out, headers);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_header_names() {
        assert_eq!(canonical_header_name("x-goog-api-key"), "X-Goog-Api-Key");
        assert_eq!(canonical_header_name("content-type"), "Content-Type");
    }

    #[test]
    fn request_round_trip() {
        let req = http::Request::post("https://h.example/p?alt=sse")
            .header("content-type", "application/json")
            .body(Bytes::from_static(b"{}"))
            .unwrap_or_default();
        let w = write_request(&req).unwrap_or_default();
        assert_eq!(
            String::from_utf8_lossy(&w),
            "POST https://h.example/p?alt=sse HTTP/1.1\r\nHost: h.example\r\nContent-Length: 2\r\nContent-Type: application/json\r\n\r\n{}"
        );
        let p = parse_request(&w).ok();
        assert_eq!(p.as_ref().map(|p| p.method.as_str()), Some("POST"));
        assert_eq!(p.as_ref().map(|p| p.body.as_slice()), Some(&b"{}"[..]));
    }

    #[test]
    fn response_round_trip() {
        let mut h = HeaderMap::new();
        h.insert("content-type", HeaderValue::from_static("text/plain"));
        let w = write_response(StatusCode::NOT_MODIFIED, &h, b"hi");
        let r = parse_response(&w).ok();
        assert_eq!(
            r.as_ref().map(|r| r.status()),
            Some(StatusCode::NOT_MODIFIED)
        );
        assert_eq!(r.as_ref().map(|r| &r.body()[..]), Some(&b"hi"[..]));
    }
}
