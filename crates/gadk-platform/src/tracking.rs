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

//! The ADK tracking header value.

/// The `user-agent` / `x-goog-api-client` value gadk sends with model and MCP requests:
/// `gadk-rust/<crate version> rust/<rustc version>`. `GADK_RUSTC_VERSION` is set by this crate's
/// `build.rs`; every workspace crate shares one version, so the value is the same whichever crate
/// asks.
pub fn tracking_header() -> String {
    format!(
        "gadk-rust/{} rust/{}",
        env!("CARGO_PKG_VERSION"),
        env!("GADK_RUSTC_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port of Go `TestTrackingHeader` (internal/version/version_test.go) for the gadk shape.
    #[test]
    fn tracking_header_has_the_gadk_shape() {
        let got = tracking_header();
        let prefix = format!("gadk-rust/{} rust/", env!("CARGO_PKG_VERSION"));
        let Some(rustc) = got.strip_prefix(&prefix) else {
            panic!("{got:?} does not start with {prefix:?}");
        };
        assert!(
            rustc.starts_with(|c: char| c.is_ascii_digit()) && !rustc.contains(' '),
            "rustc version {rustc:?} in {got:?} is not one version token"
        );
    }
}
