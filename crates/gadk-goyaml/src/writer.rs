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

//! Port of yaml.v3 v3.0.1 `writerc.go`: flushing the emitter buffer to the output.

use crate::yamlh::{YamlEmitter, YamlErrorType};

/// Set the writer error and return false.
pub(crate) fn yaml_emitter_set_writer_error(emitter: &mut YamlEmitter<'_>, problem: &str) -> bool {
    // Go: writerc.go yaml_emitter_set_writer_error
    emitter.error = YamlErrorType::Writer;
    emitter.problem = problem.to_owned();
    false
}

/// Flush the output buffer through the write handler (`write error: …` on failure).
pub(crate) fn yaml_emitter_flush(emitter: &mut YamlEmitter<'_>) -> bool {
    // Go: writerc.go yaml_emitter_flush
    let Some(write_handler) = emitter.write_handler else {
        panic!("write handler not set");
    };

    // Check if the buffer is empty.
    if emitter.buffer_pos == 0 {
        return true;
    }

    // The handler gets the buffer as a separate argument: lend it out around the call.
    let buffer = std::mem::take(&mut emitter.buffer);
    let result = write_handler(emitter, &buffer[..emitter.buffer_pos]);
    emitter.buffer = buffer;
    if let Err(err) = result {
        return yaml_emitter_set_writer_error(emitter, &format!("write error: {err}"));
    }
    emitter.buffer_pos = 0;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{
        yaml_emitter_initialize, yaml_emitter_set_output_string, yaml_emitter_set_output_writer,
    };
    use std::io::Write;

    fn fill(emitter: &mut YamlEmitter<'_>, data: &[u8]) {
        emitter.buffer[..data.len()].copy_from_slice(data);
        emitter.buffer_pos = data.len();
    }

    #[test]
    fn flush_empty_buffer_writes_nothing() {
        let mut e = YamlEmitter::default();
        yaml_emitter_initialize(&mut e);
        yaml_emitter_set_output_string(&mut e);
        assert!(yaml_emitter_flush(&mut e));
        assert!(e.output_buffer.is_empty());
        assert_eq!(e.error, YamlErrorType::No);
    }

    #[test]
    fn flush_appends_buffer_to_string_output_and_resets_pos() {
        let mut e = YamlEmitter::default();
        yaml_emitter_initialize(&mut e);
        yaml_emitter_set_output_string(&mut e);
        fill(&mut e, b"a: 1\n");
        assert!(yaml_emitter_flush(&mut e));
        fill(&mut e, b"b: 2\n");
        assert!(yaml_emitter_flush(&mut e));
        assert_eq!(e.output_buffer, b"a: 1\nb: 2\n");
        assert_eq!(e.buffer_pos, 0);
        // The lent-out buffer is back, with Go's fixed length.
        assert_eq!(e.buffer.len(), crate::yamlh::OUTPUT_BUFFER_SIZE);
    }

    #[test]
    fn flush_writes_to_writer_output() {
        let mut out = Vec::new();
        {
            let mut e = YamlEmitter::default();
            yaml_emitter_initialize(&mut e);
            yaml_emitter_set_output_writer(&mut e, Box::new(&mut out));
            fill(&mut e, b"x\n");
            assert!(yaml_emitter_flush(&mut e));
        }
        assert_eq!(out, b"x\n");
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("boom"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn flush_error_sets_writer_error_and_keeps_buffer() {
        let mut e = YamlEmitter::default();
        yaml_emitter_initialize(&mut e);
        yaml_emitter_set_output_writer(&mut e, Box::new(FailingWriter));
        fill(&mut e, b"x\n");
        assert!(!yaml_emitter_flush(&mut e));
        assert_eq!(e.error, YamlErrorType::Writer);
        // Go: "write error: " + err.Error(); errors.New("boom") gives the same text.
        assert_eq!(e.problem, "write error: boom");
        // As in Go, buffer_pos is not reset on failure.
        assert_eq!(e.buffer_pos, 2);
    }

    #[test]
    fn set_writer_error_returns_false() {
        let mut e = YamlEmitter::default();
        assert!(!yaml_emitter_set_writer_error(&mut e, "write error: x"));
        assert_eq!(e.error, YamlErrorType::Writer);
        assert_eq!(e.problem, "write error: x");
    }

    #[test]
    #[should_panic(expected = "write handler not set")]
    fn flush_without_handler_panics() {
        let mut e = YamlEmitter::default();
        yaml_emitter_initialize(&mut e);
        yaml_emitter_flush(&mut e);
    }
}
