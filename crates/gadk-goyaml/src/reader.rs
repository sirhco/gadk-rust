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
// Bodies are Task 3's; Task 3 removes this line.
#![allow(dead_code, unused_variables, unused_imports, clippy::todo)]

use crate::yamlh::YamlParser;

/// Ensure that the buffer contains at least `length` characters. Return true on success, false
/// on failure (the reader error is set on `parser`).
pub(crate) fn yaml_parser_update_buffer(parser: &mut YamlParser<'_>, length: usize) -> bool {
    // Go: readerc.go yaml_parser_update_buffer
    todo!("Task 3")
}
