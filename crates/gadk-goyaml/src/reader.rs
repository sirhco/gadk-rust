// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
// Portions derived from libyaml, Copyright (c) 2006-2010 Kirill Simonov.
//
// Permission is hereby granted, free of charge, to any person obtaining a copy of
// this software and associated documentation files (the "Software"), to deal in
// the Software without restriction, including without limitation the rights to
// use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
// of the Software, and to permit persons to whom the Software is furnished to do
// so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! Port of yaml.v3 v3.0.1 `readerc.go`: input decoding (UTF-8 / UTF-16 BOM detection), raw
//! and working buffer refill, reader errors.
//!
//! Go's buffers are slices with a fixed capacity (`raw_buffer` 512 bytes, `buffer` 1536 bytes)
//! whose length moves; here they are `Vec`s whose length follows Go's slice length, and Go's
//! `cap(...)` is the `INPUT_RAW_BUFFER_SIZE` / `INPUT_BUFFER_SIZE` constant. Indexing past the
//! length panics in both languages.

use crate::yamlh::{
    INPUT_BUFFER_SIZE, INPUT_RAW_BUFFER_SIZE, YamlEncoding, YamlErrorType, YamlParser,
};

/// Set the reader error and return false.
fn yaml_parser_set_reader_error(
    parser: &mut YamlParser<'_>,
    problem: &str,
    offset: usize,
    value: isize,
) -> bool {
    // Go: readerc.go yaml_parser_set_reader_error
    parser.error = YamlErrorType::Reader;
    parser.problem = problem.to_string();
    parser.problem_offset = offset;
    parser.problem_value = value;
    false
}

// Byte order marks.
const BOM_UTF8: &[u8] = b"\xef\xbb\xbf";
const BOM_UTF16LE: &[u8] = b"\xff\xfe";
const BOM_UTF16BE: &[u8] = b"\xfe\xff";

/// Determine the input stream encoding by checking the BOM symbol. If no BOM is found, the
/// UTF-8 encoding is assumed. Return true on success, false on failure.
fn yaml_parser_determine_encoding(parser: &mut YamlParser<'_>) -> bool {
    // Go: readerc.go yaml_parser_determine_encoding
    // Ensure that we had enough bytes in the raw buffer.
    while !parser.eof && parser.raw_buffer.len() - parser.raw_buffer_pos < 3 {
        if !yaml_parser_update_raw_buffer(parser) {
            return false;
        }
    }

    // Determine the encoding.
    let buf = &parser.raw_buffer;
    let pos = parser.raw_buffer_pos;
    let avail = buf.len() - pos;
    if avail >= 2 && buf[pos] == BOM_UTF16LE[0] && buf[pos + 1] == BOM_UTF16LE[1] {
        parser.encoding = YamlEncoding::Utf16le;
        parser.raw_buffer_pos += 2;
        parser.offset += 2;
    } else if avail >= 2 && buf[pos] == BOM_UTF16BE[0] && buf[pos + 1] == BOM_UTF16BE[1] {
        parser.encoding = YamlEncoding::Utf16be;
        parser.raw_buffer_pos += 2;
        parser.offset += 2;
    } else if avail >= 3
        && buf[pos] == BOM_UTF8[0]
        && buf[pos + 1] == BOM_UTF8[1]
        && buf[pos + 2] == BOM_UTF8[2]
    {
        parser.encoding = YamlEncoding::Utf8;
        parser.raw_buffer_pos += 3;
        parser.offset += 3;
    } else {
        parser.encoding = YamlEncoding::Utf8;
    }
    true
}

/// Update the raw buffer.
fn yaml_parser_update_raw_buffer(parser: &mut YamlParser<'_>) -> bool {
    // Go: readerc.go yaml_parser_update_raw_buffer
    // Return if the raw buffer is full.
    if parser.raw_buffer_pos == 0 && parser.raw_buffer.len() == INPUT_RAW_BUFFER_SIZE {
        return true;
    }

    // Return on EOF.
    if parser.eof {
        return true;
    }

    // Move the remaining bytes in the raw buffer to the beginning.
    if parser.raw_buffer_pos > 0 && parser.raw_buffer_pos < parser.raw_buffer.len() {
        let pos = parser.raw_buffer_pos;
        parser.raw_buffer.copy_within(pos.., 0);
    }
    let len = parser.raw_buffer.len() - parser.raw_buffer_pos;
    parser.raw_buffer.truncate(len);
    parser.raw_buffer_pos = 0;

    // Call the read handler to fill the buffer. The handler gets the free tail of the raw
    // buffer (Go: `raw_buffer[len:cap]`); the buffer is lent out around the call.
    let Some(read_handler) = parser.read_handler else {
        panic!("read handler must be set");
    };
    let mut raw_buffer = std::mem::take(&mut parser.raw_buffer);
    raw_buffer.resize(INPUT_RAW_BUFFER_SIZE, 0);
    let result = read_handler(parser, &mut raw_buffer[len..]);
    let size_read = *result.as_ref().unwrap_or(&0);
    raw_buffer.truncate(len + size_read);
    parser.raw_buffer = raw_buffer;
    match result {
        // Go: err == io.EOF (a handler returning 0 bytes is at the end of its input).
        Ok(0) => parser.eof = true,
        Ok(_) => {}
        Err(err) => {
            let offset = parser.offset;
            return yaml_parser_set_reader_error(
                parser,
                &format!("input error: {err}"),
                offset,
                -1,
            );
        }
    }
    true
}

/// Ensure that the buffer contains at least `length` characters. Return true on success, false
/// on failure (the reader error is set on `parser`).
///
/// The length is supposed to be significantly less that the buffer size.
pub(crate) fn yaml_parser_update_buffer(parser: &mut YamlParser<'_>, length: usize) -> bool {
    // Go: readerc.go yaml_parser_update_buffer
    if parser.read_handler.is_none() {
        panic!("read handler must be set");
    }

    // [Go] This function was changed to guarantee the requested length size at EOF.
    // The fact we need to do this is pretty awful, but the description above implies
    // for that to be the case, and there are tests

    // If the EOF flag is set and the raw buffer is empty, do nothing.
    //
    // [Go] ACTUALLY! Read the documentation of this function above. This is just broken. To
    // return true, we need to have the given length in the buffer. Not doing that means every
    // single check that calls this function to make sure the buffer has a given length is Go)
    // panicking; or C) accessing invalid memory. (Go keeps the check with an empty body.)

    // Return if the buffer contains enough characters.
    if parser.unread >= length {
        return true;
    }

    // Determine the input encoding if it is not known yet.
    if parser.encoding == YamlEncoding::Any && !yaml_parser_determine_encoding(parser) {
        return false;
    }

    // Move the unread characters to the beginning of the buffer.
    let mut buffer_len = parser.buffer.len();
    if parser.buffer_pos > 0 && parser.buffer_pos < buffer_len {
        let pos = parser.buffer_pos;
        parser.buffer.copy_within(pos.., 0);
        buffer_len -= parser.buffer_pos;
        parser.buffer_pos = 0;
    } else if parser.buffer_pos == buffer_len {
        buffer_len = 0;
        parser.buffer_pos = 0;
    }

    // Open the whole buffer for writing, and cut it before returning.
    parser.buffer.resize(INPUT_BUFFER_SIZE, 0);

    // Fill the buffer until it has enough characters.
    let mut first = true;
    while parser.unread < length {
        // Fill the raw buffer if necessary.
        if (!first || parser.raw_buffer_pos == parser.raw_buffer.len())
            && !yaml_parser_update_raw_buffer(parser)
        {
            parser.buffer.truncate(buffer_len);
            return false;
        }
        first = false;

        // Decode the raw buffer.
        'inner: while parser.raw_buffer_pos != parser.raw_buffer.len() {
            let mut value: u32;
            let width: usize;

            let raw_unread = parser.raw_buffer.len() - parser.raw_buffer_pos;

            // Decode the next character.
            match parser.encoding {
                YamlEncoding::Utf8 => {
                    // Decode a UTF-8 character.  Check RFC 3629
                    // (http://www.ietf.org/rfc/rfc3629.txt) for more details.
                    //
                    // The following table (taken from the RFC) is used for
                    // decoding.
                    //
                    //    Char. number range |        UTF-8 octet sequence
                    //      (hexadecimal)    |              (binary)
                    //   --------------------+------------------------------------
                    //   0000 0000-0000 007F | 0xxxxxxx
                    //   0000 0080-0000 07FF | 110xxxxx 10xxxxxx
                    //   0000 0800-0000 FFFF | 1110xxxx 10xxxxxx 10xxxxxx
                    //   0001 0000-0010 FFFF | 11110xxx 10xxxxxx 10xxxxxx 10xxxxxx
                    //
                    // Additionally, the characters in the range 0xD800-0xDFFF
                    // are prohibited as they are reserved for use with UTF-16
                    // surrogate pairs.

                    // Determine the length of the UTF-8 sequence.
                    let mut octet = parser.raw_buffer[parser.raw_buffer_pos];
                    width = if octet & 0x80 == 0x00 {
                        1
                    } else if octet & 0xE0 == 0xC0 {
                        2
                    } else if octet & 0xF0 == 0xE0 {
                        3
                    } else if octet & 0xF8 == 0xF0 {
                        4
                    } else {
                        // The leading octet is invalid.
                        let offset = parser.offset;
                        return yaml_parser_set_reader_error(
                            parser,
                            "invalid leading UTF-8 octet",
                            offset,
                            isize::from(octet),
                        );
                    };

                    // Check if the raw buffer contains an incomplete character.
                    if width > raw_unread {
                        if parser.eof {
                            let offset = parser.offset;
                            return yaml_parser_set_reader_error(
                                parser,
                                "incomplete UTF-8 octet sequence",
                                offset,
                                -1,
                            );
                        }
                        break 'inner;
                    }

                    // Decode the leading octet.
                    value = if octet & 0x80 == 0x00 {
                        u32::from(octet & 0x7F)
                    } else if octet & 0xE0 == 0xC0 {
                        u32::from(octet & 0x1F)
                    } else if octet & 0xF0 == 0xE0 {
                        u32::from(octet & 0x0F)
                    } else if octet & 0xF8 == 0xF0 {
                        u32::from(octet & 0x07)
                    } else {
                        0
                    };

                    // Check and decode the trailing octets.
                    for k in 1..width {
                        octet = parser.raw_buffer[parser.raw_buffer_pos + k];

                        // Check if the octet is valid.
                        if (octet & 0xC0) != 0x80 {
                            let offset = parser.offset + k;
                            return yaml_parser_set_reader_error(
                                parser,
                                "invalid trailing UTF-8 octet",
                                offset,
                                isize::from(octet),
                            );
                        }

                        // Decode the octet.
                        value = (value << 6) + u32::from(octet & 0x3F);
                    }

                    // Check the length of the sequence against the value.
                    let length_ok = width == 1
                        || (width == 2 && value >= 0x80)
                        || (width == 3 && value >= 0x800)
                        || (width == 4 && value >= 0x10000);
                    if !length_ok {
                        let offset = parser.offset;
                        return yaml_parser_set_reader_error(
                            parser,
                            "invalid length of a UTF-8 sequence",
                            offset,
                            -1,
                        );
                    }

                    // Check the range of the value.
                    if (0xD800..=0xDFFF).contains(&value) || value > 0x10FFFF {
                        let offset = parser.offset;
                        return yaml_parser_set_reader_error(
                            parser,
                            "invalid Unicode character",
                            offset,
                            value as isize,
                        );
                    }
                }

                YamlEncoding::Utf16le | YamlEncoding::Utf16be => {
                    let (low, high) = if parser.encoding == YamlEncoding::Utf16le {
                        (0, 1)
                    } else {
                        (1, 0)
                    };

                    // The UTF-16 encoding is not as simple as one might
                    // naively think.  Check RFC 2781
                    // (http://www.ietf.org/rfc/rfc2781.txt).
                    //
                    // Normally, two subsequent bytes describe a Unicode
                    // character.  However a special technique (called a
                    // surrogate pair) is used for specifying character
                    // values larger than 0xFFFF.
                    //
                    // A surrogate pair consists of two pseudo-characters:
                    //      high surrogate area (0xD800-0xDBFF)
                    //      low surrogate area (0xDC00-0xDFFF)
                    //
                    // The following formulas are used for decoding
                    // and encoding characters using surrogate pairs:
                    //
                    //  U  = U' + 0x10000   (0x01 00 00 <= U <= 0x10 FF FF)
                    //  U' = yyyyyyyyyyxxxxxxxxxx   (0 <= U' <= 0x0F FF FF)
                    //  W1 = 110110yyyyyyyyyy
                    //  W2 = 110111xxxxxxxxxx
                    //
                    // where U is the character value, W1 is the high surrogate
                    // area, W2 is the low surrogate area.

                    // Check for incomplete UTF-16 character.
                    if raw_unread < 2 {
                        if parser.eof {
                            let offset = parser.offset;
                            return yaml_parser_set_reader_error(
                                parser,
                                "incomplete UTF-16 character",
                                offset,
                                -1,
                            );
                        }
                        break 'inner;
                    }

                    // Get the character.
                    value = u32::from(parser.raw_buffer[parser.raw_buffer_pos + low])
                        + (u32::from(parser.raw_buffer[parser.raw_buffer_pos + high]) << 8);

                    // Check for unexpected low surrogate area.
                    if value & 0xFC00 == 0xDC00 {
                        let offset = parser.offset;
                        return yaml_parser_set_reader_error(
                            parser,
                            "unexpected low surrogate area",
                            offset,
                            value as isize,
                        );
                    }

                    // Check for a high surrogate area.
                    if value & 0xFC00 == 0xD800 {
                        width = 4;

                        // Check for incomplete surrogate pair.
                        if raw_unread < 4 {
                            if parser.eof {
                                let offset = parser.offset;
                                return yaml_parser_set_reader_error(
                                    parser,
                                    "incomplete UTF-16 surrogate pair",
                                    offset,
                                    -1,
                                );
                            }
                            break 'inner;
                        }

                        // Get the next character.
                        let value2 = u32::from(parser.raw_buffer[parser.raw_buffer_pos + low + 2])
                            + (u32::from(parser.raw_buffer[parser.raw_buffer_pos + high + 2]) << 8);

                        // Check for a low surrogate area.
                        if value2 & 0xFC00 != 0xDC00 {
                            let offset = parser.offset + 2;
                            return yaml_parser_set_reader_error(
                                parser,
                                "expected low surrogate area",
                                offset,
                                value2 as isize,
                            );
                        }

                        // Generate the value of the surrogate pair.
                        value = 0x10000 + ((value & 0x3FF) << 10) + (value2 & 0x3FF);
                    } else {
                        width = 2;
                    }
                }

                YamlEncoding::Any => panic!("impossible"),
            }

            // Check if the character is in the allowed range:
            //      #x9 | #xA | #xD | [#x20-#x7E]               (8 bit)
            //      | #x85 | [#xA0-#xD7FF] | [#xE000-#xFFFD]    (16 bit)
            //      | [#x10000-#x10FFFF]                        (32 bit)
            let allowed = value == 0x09
                || value == 0x0A
                || value == 0x0D
                || (0x20..=0x7E).contains(&value)
                || value == 0x85
                || (0xA0..=0xD7FF).contains(&value)
                || (0xE000..=0xFFFD).contains(&value)
                || (0x10000..=0x10FFFF).contains(&value);
            if !allowed {
                let offset = parser.offset;
                return yaml_parser_set_reader_error(
                    parser,
                    "control characters are not allowed",
                    offset,
                    value as isize,
                );
            }

            // Move the raw pointers.
            parser.raw_buffer_pos += width;
            parser.offset += width;

            // Finally put the character into the buffer.
            if value <= 0x7F {
                // 0000 0000-0000 007F . 0xxxxxxx
                parser.buffer[buffer_len] = value as u8;
                buffer_len += 1;
            } else if value <= 0x7FF {
                // 0000 0080-0000 07FF . 110xxxxx 10xxxxxx
                parser.buffer[buffer_len] = (0xC0 + (value >> 6)) as u8;
                parser.buffer[buffer_len + 1] = (0x80 + (value & 0x3F)) as u8;
                buffer_len += 2;
            } else if value <= 0xFFFF {
                // 0000 0800-0000 FFFF . 1110xxxx 10xxxxxx 10xxxxxx
                parser.buffer[buffer_len] = (0xE0 + (value >> 12)) as u8;
                parser.buffer[buffer_len + 1] = (0x80 + ((value >> 6) & 0x3F)) as u8;
                parser.buffer[buffer_len + 2] = (0x80 + (value & 0x3F)) as u8;
                buffer_len += 3;
            } else {
                // 0001 0000-0010 FFFF . 11110xxx 10xxxxxx 10xxxxxx 10xxxxxx
                parser.buffer[buffer_len] = (0xF0 + (value >> 18)) as u8;
                parser.buffer[buffer_len + 1] = (0x80 + ((value >> 12) & 0x3F)) as u8;
                parser.buffer[buffer_len + 2] = (0x80 + ((value >> 6) & 0x3F)) as u8;
                parser.buffer[buffer_len + 3] = (0x80 + (value & 0x3F)) as u8;
                buffer_len += 4;
            }

            parser.unread += 1;
        }

        // On EOF, put NUL into the buffer and return.
        if parser.eof {
            parser.buffer[buffer_len] = 0;
            buffer_len += 1;
            parser.unread += 1;
            break;
        }
    }
    // [Go] Read the documentation of this function above. To return true,
    // we need to have the given length in the buffer. Not doing that means
    // every single check that calls this function to make sure the buffer
    // has a given length is Go) panicking; or C) accessing invalid memory.
    // This happens here due to the EOF above breaking early.
    while buffer_len < length {
        parser.buffer[buffer_len] = 0;
        buffer_len += 1;
    }
    parser.buffer.truncate(buffer_len);
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::yaml_parser_update_buffer;
    use crate::api::{
        yaml_parser_initialize, yaml_parser_set_input_reader, yaml_parser_set_input_string,
    };
    use crate::yamlh::{INPUT_RAW_BUFFER_SIZE, YamlEncoding, YamlErrorType, YamlParser};

    /// A reader handing out one byte per `read` (every multi-byte character is split across
    /// raw buffer refills), then EOF or (with `fail`) an error. Shared with the scanner tests.
    struct OneByteReader {
        input: Vec<u8>,
        pos: usize,
        fail: bool,
    }

    impl std::io::Read for OneByteReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.input.len() {
                if self.fail {
                    return Err(std::io::Error::other("boom"));
                }
                return Ok(0);
            }
            buf[0] = self.input[self.pos];
            self.pos += 1;
            Ok(1)
        }
    }

    /// A parser over string input (shared with the scanner tests).
    pub(crate) fn string_parser(input: &[u8]) -> YamlParser<'_> {
        let mut parser = YamlParser::default();
        assert!(yaml_parser_initialize(&mut parser));
        yaml_parser_set_input_string(&mut parser, input);
        parser
    }

    /// A parser over a [`OneByteReader`] (shared with the scanner tests).
    pub(crate) fn reader_parser(input: &[u8], fail: bool) -> YamlParser<'static> {
        let mut parser = YamlParser::default();
        assert!(yaml_parser_initialize(&mut parser));
        let reader = OneByteReader {
            input: input.to_vec(),
            pos: 0,
            fail,
        };
        yaml_parser_set_input_reader(&mut parser, Box::new(reader));
        parser
    }

    fn utf16(text: &str, little_endian: bool) -> Vec<u8> {
        let mut out = if little_endian {
            b"\xff\xfe".to_vec()
        } else {
            b"\xfe\xff".to_vec()
        };
        for unit in text.encode_utf16() {
            if little_endian {
                out.extend_from_slice(&unit.to_le_bytes());
            } else {
                out.extend_from_slice(&unit.to_be_bytes());
            }
        }
        out
    }

    /// The reader error after asking for more characters than `input` holds:
    /// `(problem, offset, value)`.
    fn reader_error(input: &[u8]) -> (String, usize, isize) {
        let mut parser = string_parser(input);
        assert!(!yaml_parser_update_buffer(&mut parser, 64));
        assert_eq!(parser.error, YamlErrorType::Reader);
        (
            parser.problem.clone(),
            parser.problem_offset,
            parser.problem_value,
        )
    }

    #[test]
    fn utf8_without_bom_is_copied_and_padded_with_nul_at_eof() {
        // Encoding detection reads until 3 raw bytes are available, so a 2-byte input already
        // reaches EOF there and gets its NUL on the first fill.
        let mut parser = string_parser(b"ab");
        assert!(yaml_parser_update_buffer(&mut parser, 1));
        assert!(parser.eof);
        assert_eq!(parser.buffer, b"ab\0");
        assert_eq!(parser.unread, 3);

        let mut parser = string_parser(b"abc");
        assert!(yaml_parser_update_buffer(&mut parser, 1));
        assert_eq!(parser.encoding, YamlEncoding::Utf8);
        // The string handler has not reported EOF yet: no NUL.
        assert!(!parser.eof);
        assert_eq!(parser.buffer, b"abc");
        assert_eq!(parser.unread, 3);
        // Asking for more reaches EOF: one NUL counted as a character, then zero padding up
        // to the requested length (Go's "[Go] ... guarantee the requested length" loop).
        assert!(yaml_parser_update_buffer(&mut parser, 5));
        assert!(parser.eof);
        assert_eq!(parser.buffer, b"abc\0\0");
        assert_eq!(parser.unread, 4);
        assert_eq!(parser.offset, 3);
    }

    #[test]
    fn utf8_bom_is_skipped() {
        let mut parser = string_parser(b"\xef\xbb\xbfa");
        assert!(yaml_parser_update_buffer(&mut parser, 2));
        assert_eq!(parser.encoding, YamlEncoding::Utf8);
        assert_eq!(parser.buffer, b"a\0");
        assert_eq!(parser.offset, 4);
    }

    #[test]
    fn utf16_inputs_are_decoded_to_utf8() {
        let text = "aé𝄞\n";
        for (little_endian, encoding) in [
            (true, YamlEncoding::Utf16le),
            (false, YamlEncoding::Utf16be),
        ] {
            let input = utf16(text, little_endian);
            let mut parser = string_parser(&input);
            assert!(yaml_parser_update_buffer(&mut parser, 5));
            assert_eq!(parser.encoding, encoding);
            let mut want = text.as_bytes().to_vec();
            want.push(0);
            assert_eq!(parser.buffer, want);
            // Four characters plus the EOF NUL.
            assert_eq!(parser.unread, 5);
            // BOM 2 + 'a' 2 + 'é' 2 + surrogate pair 4 + '\n' 2.
            assert_eq!(parser.offset, 12);
        }
    }

    #[test]
    fn one_byte_reads_reassemble_split_characters() {
        let text = "aé𝄞あ\n";
        for input in [
            text.as_bytes().to_vec(),
            utf16(text, true),
            utf16(text, false),
        ] {
            let mut parser = reader_parser(&input, false);
            assert!(yaml_parser_update_buffer(&mut parser, 6));
            let mut want = text.as_bytes().to_vec();
            want.push(0);
            assert_eq!(parser.buffer, want);
            assert_eq!(parser.unread, 6);
        }
    }

    #[test]
    fn buffer_refill_moves_unread_characters_to_the_front() {
        // More than one raw buffer of input: consume part, then refill.
        let input = "x".repeat(INPUT_RAW_BUFFER_SIZE + 100);
        let mut parser = string_parser(input.as_bytes());
        assert!(yaml_parser_update_buffer(&mut parser, 1));
        assert_eq!(parser.unread, INPUT_RAW_BUFFER_SIZE);
        assert!(parser.raw_buffer.len() <= INPUT_RAW_BUFFER_SIZE);
        // Pretend the scanner consumed 500 characters.
        parser.buffer_pos += 500;
        parser.unread -= 500;
        assert!(yaml_parser_update_buffer(&mut parser, 20));
        assert_eq!(parser.buffer_pos, 0);
        assert_eq!(parser.unread, 12 + 100);
        assert_eq!(parser.buffer.len(), 112);
        assert!(parser.buffer.iter().all(|&b| b == b'x'));
        assert!(yaml_parser_update_buffer(&mut parser, 113));
        assert_eq!(parser.buffer.len(), 113);
        assert_eq!(parser.buffer[112], 0);
    }

    #[test]
    fn reader_errors() {
        let cases: &[(&[u8], &str, usize, isize)] = &[
            (b"a: \xff\n", "invalid leading UTF-8 octet", 3, 255),
            (b"a: \xc3\x28\n", "invalid trailing UTF-8 octet", 4, 40),
            (b"a: \xe2\x82", "incomplete UTF-8 octet sequence", 3, -1),
            (
                b"a: \xc0\x80\n",
                "invalid length of a UTF-8 sequence",
                3,
                -1,
            ),
            (b"a: \xed\xa0\x80\n", "invalid Unicode character", 3, 55296),
            (
                b"a: \xf4\x90\x80\x80\n",
                "invalid Unicode character",
                3,
                1114112,
            ),
            (b"a: \x01\n", "control characters are not allowed", 3, 1),
            (b"a: \x00b\n", "control characters are not allowed", 3, 0),
            (b"\xff\xfea\x00b\x00a", "incomplete UTF-16 character", 6, -1),
            (
                b"\xff\xfe\x00\xdc",
                "unexpected low surrogate area",
                2,
                56320,
            ),
            (
                b"\xff\xfe\x00\xd8a\x00",
                "expected low surrogate area",
                4,
                97,
            ),
            (
                b"\xff\xfe\x00\xd8\x00",
                "incomplete UTF-16 surrogate pair",
                2,
                -1,
            ),
            (
                b"\xfe\xff\x00\x01",
                "control characters are not allowed",
                2,
                1,
            ),
        ];
        for (input, problem, offset, value) in cases {
            assert_eq!(
                reader_error(input),
                (problem.to_string(), *offset, *value),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn read_handler_failure_is_an_input_error() {
        // Encoding detection reads "abc"; the next fill reads "d", the one after fails.
        let mut parser = reader_parser(b"abcd", true);
        assert!(yaml_parser_update_buffer(&mut parser, 2));
        assert_eq!(parser.unread, 3);
        assert!(!yaml_parser_update_buffer(&mut parser, 5));
        assert_eq!(parser.error, YamlErrorType::Reader);
        assert_eq!(parser.problem, "input error: boom");
        assert_eq!(parser.problem_offset, 4);
        assert_eq!(parser.problem_value, -1);
    }

    #[test]
    #[should_panic(expected = "read handler must be set")]
    fn missing_read_handler_panics() {
        let mut parser = YamlParser::default();
        yaml_parser_update_buffer(&mut parser, 1);
    }
}
