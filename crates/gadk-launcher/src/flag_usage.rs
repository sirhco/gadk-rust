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

//! Go `internal/cli/util.FormatFlagUsage` (`flagset_helpers.go`), the only `internal/cli/util`
//! helper the launcher uses.

use gadk_platform::go_flag::FlagSet;

/// Go `util.FormatFlagUsage`: the set's `PrintDefaults` text (Go swaps the output to a builder
/// around `PrintDefaults`; `print_defaults` returns the text and writes nothing).
pub(crate) fn format_flag_usage(fs: &FlagSet) -> String {
    fs.print_defaults()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn format_flag_usage_is_print_defaults_and_writes_nothing() {
        let out = Arc::new(Mutex::new(Vec::<u8>::new()));
        let mut fs = FlagSet::new("console");
        fs.set_output(out.clone());
        fs.duration(
            "shutdown-timeout",
            gadk_platform::GoDuration::from_secs(2),
            "Console shutdown timeout (i.e. '10s', '2m' - see time.ParseDuration for details) - for waiting for active requests to finish during shutdown",
        );
        fs.string("streaming_mode", "", "defines streaming mode (none|sse)");
        assert_eq!(
            format_flag_usage(&fs),
            "  -shutdown-timeout duration\n    \tConsole shutdown timeout (i.e. '10s', '2m' - see time.ParseDuration for details) - for waiting for active requests to finish during shutdown (default 2s)\n  -streaming_mode string\n    \tdefines streaming mode (none|sse)\n"
        );
        assert!(out.lock().unwrap().is_empty());
    }
}
