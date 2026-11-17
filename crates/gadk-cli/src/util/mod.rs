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

//! The text checks and the console helpers the `gadk` commands print with. The flag-usage
//! helper lives in `gadk-launcher` as `flag_usage` (the launcher's user).

mod oscmd;
mod text;

pub use oscmd::{
    BLUE, CYAN, GRAY, GREEN, MAGENTA, Printer, RED, RESET, ReprintableStream, WHITE, YELLOW,
    log_command, log_start_stop, new_reprintable_stream, strip_extension,
};
pub use text::{center_string, validate_dockerfile_safe, validate_shell_arg_safe};
