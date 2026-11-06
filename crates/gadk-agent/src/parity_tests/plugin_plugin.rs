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

use std::sync::Arc;

use futures::FutureExt;

use crate::error::AgentError;
use crate::plugin::{Plugin, PluginConfig};

// parity: adapted: Go's Plugin.Close() method becomes the close() accessor (always Some; Plugin::new installs a no-op) whose future is awaited
#[tokio::test]
async fn test_new() {
    // Successfully maps all fields; default close is Ok
    let p = Plugin::new(PluginConfig {
        on_user_message_callback: Some(Arc::new(|_ctx, _msg| async { Ok(None) }.boxed())),
        ..Default::default()
    });
    assert!(p.on_user_message_callback().is_some());
    assert!((p.close().unwrap())().await.is_ok());
    // Handles a missing close function gracefully
    let p = Plugin::new(PluginConfig::default());
    assert!((p.close().unwrap())().await.is_ok());
    // Executes the provided close function
    let p = Plugin::new(PluginConfig {
        close: Some(Arc::new(|| {
            async { Err(AgentError::other("close error")) }.boxed()
        })),
        ..Default::default()
    });
    assert_eq!(
        (p.close().unwrap())().await.unwrap_err().to_string(),
        "close error"
    );
}
