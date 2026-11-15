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

//! Port of ADK-Go `internal/plugininternal/plugin_manager_context_test.go` (R10, #1777). Go's
//! `ToContext` / `FromContext` are the context builder's `plugin_manager` and
//! [`InvocationContext::plugin_manager`]; `WithInheritedPlugins` / `TakeInheritedPlugins` are
//! [`InvocationContext::with_inherited_plugins`] / [`InvocationContext::take_inherited_plugins`].

use std::sync::Arc;
use std::time::Duration;

use crate::context::InvocationContext;
use crate::plugin::{Plugin, PluginConfig};
use crate::plugin_manager::PluginManager;

fn manager(plugins: Vec<Plugin>) -> Arc<PluginManager> {
    Arc::new(PluginManager::new(plugins, Duration::ZERO).expect("NewPluginManager()"))
}

fn test_plugin() -> Plugin {
    Plugin::new(PluginConfig {
        name: "test-plugin".into(),
        ..Default::default()
    })
}

// parity: adapted: Go's nil *PluginManager receiver is `None` at the call site (`Option::is_some_and`)
#[test]
fn test_plugin_manager_has_plugins() {
    let with_plugin = manager(vec![test_plugin()]);
    let empty = manager(vec![]);
    let cases: [(&str, Option<Arc<PluginManager>>, bool); 3] = [
        ("nil manager", None, false),
        ("empty manager", Some(empty), false),
        ("manager with a plugin", Some(with_plugin), true),
    ];
    for (name, pm, want) in cases {
        let got = pm.as_ref().is_some_and(|m| m.has_plugins());
        assert_eq!(got, want, "{name}: HasPlugins()");
    }
}

#[test]
fn test_from_context() {
    let pm = manager(vec![]);
    let ctx = InvocationContext::builder()
        .plugin_manager(pm.clone())
        .build_invocation();
    let got = ctx.plugin_manager().expect("FromContext() = nil");
    assert!(
        Arc::ptr_eq(&got, &pm),
        "FromContext() did not return the manager stored by ToContext"
    );
    assert!(
        InvocationContext::builder()
            .build_invocation()
            .plugin_manager()
            .is_none(),
        "FromContext() on an empty context = non-nil, want nil"
    );
}

#[test]
fn test_take_inherited_plugins() {
    let pm = manager(vec![test_plugin()]);
    let ctx = InvocationContext::builder()
        .plugin_manager(pm.clone())
        .build_invocation();

    let (_, got) = ctx.take_inherited_plugins();
    assert!(
        got.is_none(),
        "TakeInheritedPlugins() without a mark = non-nil, want nil"
    );
    let (cleared, got) = ctx.with_inherited_plugins().take_inherited_plugins();
    assert!(
        got.is_some_and(|g| Arc::ptr_eq(&g, &pm)),
        "TakeInheritedPlugins() did not return the marked manager"
    );
    let (_, again) = cleared.take_inherited_plugins();
    assert!(
        again.is_none(),
        "TakeInheritedPlugins() on a cleared context = non-nil, want nil"
    );
}
