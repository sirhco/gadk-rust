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

//! Parity port of Go internal/llminternal/live_injection_test.go.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::llm::state::LiveModeToolInjection;

#[test]
fn test_live_mode_tool_injection_retries_after_panic_and_latches_after_success() {
    let injection = LiveModeToolInjection::default();
    let calls = Cell::new(0usize);
    for _ in 0..2 {
        let got = catch_unwind(AssertUnwindSafe(|| {
            injection.do_once(|| {
                calls.set(calls.get() + 1);
                panic!("injection failed");
            })
        }));
        let payload = got.expect_err("Do did not propagate the injection's panic");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"injection failed"),
            "Do did not propagate the injection's panic"
        );
    }
    assert_eq!(
        calls.get(),
        2,
        "panicking injection ran {} times, want 2",
        calls.get()
    );

    injection.do_once(|| calls.set(calls.get() + 1));
    assert_eq!(calls.get(), 3, "successful retry did not run");
    injection.do_once(|| calls.set(calls.get() + 1));
    assert_eq!(calls.get(), 3, "successful injection ran again");
}
