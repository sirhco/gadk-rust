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

//! Port of ADK-Go `internal/configurable/configurable_utils_test.go`.
//!
//! Go's containment tests assert `errors.Is` on `internal/utils`' two sentinels; gadk's
//! [`ConfigError`] has no sentinels, so a rejection is read off the sentinel's Go text
//! ([`NOT_VALID_RELATIVE_PATH`], [`SYMLINK_IN_RELATIVE_PATH`]). Go's unexported
//! `resolveConfigReference(configDir, ref)` has no gadk counterpart yet: those tests drive the
//! same check through [`Registry::resolve_agent_reference`] (whose first argument is, as in
//! Go 0990f9e, the referencing config's directory) and read the outcome off its error.

use std::path::{Path, PathBuf};

use crate::tests::{CfgErr, fake_key_env, temp_dir, test_registry, write};
use crate::{ConfigError, GoMap, GoValue, Registry};

/// Go `utils.ErrNotValidRelativePath`'s text.
const NOT_VALID_RELATIVE_PATH: &str = "path is not a relative path inside the base directory";
/// Go `utils.ErrSymlinkInRelativePath`'s text.
const SYMLINK_IN_RELATIVE_PATH: &str = "path leads outside the base directory through a link";
/// gadk's refusals before 8a (Go bb1fd51's texts): a check that a reference is NOT refused
/// also rules these out, so it cannot pass on a refusal worded the old way.
const OLD_REFUSALS: [&str; 3] = [
    "path traversal detected",
    "absolute paths are not allowed",
    "agent reference path cannot be empty",
];

/// Go `isContainmentRejection`: the containment check refusing a reference, as opposed to a
/// later failure to load the config that it names.
fn is_containment_rejection(err: &str) -> bool {
    err.contains(NOT_VALID_RELATIVE_PATH) || err.contains(SYMLINK_IN_RELATIVE_PATH)
}

/// Any refusal of the reference: Go's ([`is_containment_rejection`]) or gadk's old wording.
fn is_any_refusal(err: &str) -> bool {
    is_containment_rejection(err) || OLD_REFUSALS.iter().any(|t| err.contains(t))
}

/// The error text of a result (empty on success).
fn err_text<T>(result: Result<T, ConfigError>) -> String {
    result.err().map(|e| e.to_string()).unwrap_or_default()
}

/// Go `newAgentDir`: an agent directory with a sibling config and a file outside the
/// directory. Returns the temp dir guard, the base directory, the agent directory and the
/// parent agent config path. It creates no link; a test that needs the escaping link calls
/// [`link_outside`].
fn new_agent_dir() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let (guard, base) = temp_dir();
    let agent_dir = base.join("agents").join("root");
    let agent_path = write(&agent_dir, "root_agent.yaml", "agent_class: LlmAgent\n");
    write(&agent_dir, "sub_agent.yaml", "agent_class: LlmAgent\n");
    write(&base, "outside.yaml", "agent_class: LlmAgent\n");
    (guard, base, agent_dir, agent_path)
}

/// Go `linkOutside`: `agentDir/link.yaml`, a symlink to the `outside.yaml` that
/// [`new_agent_dir`] wrote next to the agent directory.
fn link_outside(base: &Path, agent_dir: &Path) {
    std::os::unix::fs::symlink(base.join("outside.yaml"), agent_dir.join("link.yaml")).unwrap();
}

// parity: adapted: Go's package-global registry is a `Registry` value with Go init()'s agent classes (`Registry::with_agent_classes`; the built-in tools are not needed here); Go's sentinels are matched by their text
#[ignore = "8a bug: ResolveAgentReference takes the referencing config's file path (Go 0990f9e: its directory) and refuses with \"absolute paths are not allowed\"/\"path traversal detected\" instead of the utils.SafeSubpath sentinels (Go #1299)"]
#[tokio::test]
async fn test_resolve_agent_reference_rejects_escaping_config_path() {
    let (_guard, base, agent_dir, _) = new_agent_dir();
    link_outside(&base, &agent_dir);
    let registry = Registry::with_agent_classes();

    let tests = [
        ("absolute path", "/etc/passwd", NOT_VALID_RELATIVE_PATH),
        (
            "parent traversal",
            "../../outside.yaml",
            NOT_VALID_RELATIVE_PATH,
        ),
        (
            "symlink escaping the agent directory",
            "link.yaml",
            SYMLINK_IN_RELATIVE_PATH,
        ),
    ];

    for (name, ref_path, want_err) in tests {
        // The reference must be rejected by the containment check itself, not merely fail
        // later while the config is being loaded.
        let err = registry
            .resolve_agent_reference(&agent_dir, ref_path)
            .await
            .cfg_err()
            .to_string();
        assert!(
            err.contains(want_err),
            "{name}: ResolveAgentReference(_, {agent_dir:?}, {ref_path:?}) = {err}, want {want_err:?}"
        );
    }
}

// parity: adapted: Go's package-global registry is a `Registry` value with Go init()'s agent classes; the reference must also reach the loader (no "config file not found": sub_agent.yaml exists)
#[ignore = "8a bug: ResolveAgentReference resolves the reference against the parent of its first argument (Go 0990f9e: the config directory itself), so agentDir/sub_agent.yaml is looked up one directory up (Go #1299)"]
#[tokio::test]
async fn test_resolve_agent_reference_allows_paths_inside_agent_dir() {
    let (_guard, _base, agent_dir, _) = new_agent_dir();
    let registry = Registry::with_agent_classes();

    let err = err_text(
        registry
            .resolve_agent_reference(&agent_dir, "sub_agent.yaml")
            .await,
    );
    assert!(
        !is_any_refusal(&err) && !err.contains("config file not found"),
        "ResolveAgentReference(_, {agent_dir:?}, \"sub_agent.yaml\") = {err}, want no containment rejection"
    );
}

#[tokio::test]
async fn test_resolve_tool_reference_mcp_toolset_non_string_args() {
    fn new_args(server_args: Vec<GoValue>, tool_filter: Vec<GoValue>) -> GoMap {
        let server_params = GoValue::Map(vec![
            ("command".to_owned(), GoValue::String("echo".into())),
            ("args".to_owned(), GoValue::Seq(server_args)),
        ]);
        let stdio = GoValue::Map(vec![("server_params".to_owned(), server_params)]);
        [
            ("stdio_connection_params".to_owned(), stdio),
            ("tool_filter".to_owned(), GoValue::Seq(tool_filter)),
        ]
        .into_iter()
        .collect()
    }
    let s = |v: &str| GoValue::String(v.into());

    let tests = [
        (
            "non-string in server args",
            new_args(vec![s("a"), GoValue::Int(1), GoValue::Int(2)], vec![s("a")]),
            "server_params.args[1]",
        ),
        (
            "non-string in tool filter",
            new_args(vec![s("a")], vec![GoValue::Bool(true)]),
            "tool_filter[0]",
        ),
        (
            "all strings is valid",
            new_args(vec![s("a"), s("b")], vec![s("t1")]),
            "",
        ),
    ];

    let registry = Registry::new();
    for (name, args, want_err) in tests {
        // A non-string element previously triggered a panic; the call must return normally
        // (with or without an error), never crash.
        let result = registry
            .resolve_tool_reference(&Default::default(), "McpToolset", Some(&args))
            .await;
        if want_err.is_empty() {
            match result {
                Ok((_, toolset)) => assert!(
                    toolset.is_some(),
                    "{name}: ResolveToolReference(McpToolset) returned a nil toolset, want non-nil"
                ),
                Err(err) => {
                    panic!("{name}: ResolveToolReference(McpToolset) = {err}, want no error")
                }
            }
            continue;
        }
        let err = result.cfg_err().to_string();
        assert!(
            err.contains(want_err),
            "{name}: ResolveToolReference(McpToolset) = {err}, want error containing {want_err:?}"
        );
    }
}

// parity: adapted: Go's resolveConfigReference(agentDir, "self.yaml") succeeding is ResolveAgentReference reaching the loader (the linked LlmAgent config has no model, so it fails on that, not as a refusal or a missing file)
#[ignore = "8a bug: ResolveAgentReference resolves against the parent of the config directory and refuses ../outside.yaml with \"path traversal detected\" instead of utils.ErrNotValidRelativePath (Go #1299)"]
#[tokio::test]
async fn test_resolve_agent_reference_symlinked_parent_dir() {
    let (_guard, base) = temp_dir();
    let real_dir = base.join("real");
    write(&real_dir, "root_agent.yaml", "agent_class: LlmAgent\n");
    let alias = base.join("alias");
    std::os::unix::fs::symlink(&real_dir, &alias).unwrap();
    let registry = Registry::with_agent_classes();

    let agent_dir = alias;
    // A missing reference is reported by the read as not found, not as a containment refusal.
    let err = err_text(
        registry
            .resolve_agent_reference(&agent_dir, "missing.yaml")
            .await,
    );
    assert!(
        !err.is_empty() && !is_any_refusal(&err) && err.contains("config file not found"),
        "ResolveAgentReference(_, {agent_dir:?}, \"missing.yaml\") = {err}, want a config file not found error that is not a containment rejection"
    );

    // A link below the alias that names its target by the real path stays inside: this is
    // only true when the base is resolved as well, otherwise the two sides are rooted
    // differently and the reference is refused.
    std::os::unix::fs::symlink(real_dir.join("root_agent.yaml"), real_dir.join("self.yaml"))
        .unwrap();
    let err = err_text(
        registry
            .resolve_agent_reference(&agent_dir, "self.yaml")
            .await,
    );
    assert!(
        !is_any_refusal(&err) && !err.contains("config file not found"),
        "resolveConfigReference({agent_dir:?}, self.yaml) = {err}, want success: the link lands inside the real directory"
    );

    // The symlinked parent must not weaken the check itself. The outside file exists, so an
    // accepted reference would load it rather than fail as not found, and the refusal must
    // come from the lexical check, which runs before any link is looked at.
    write(&base, "outside.yaml", "agent_class: LlmAgent\n");
    let err = err_text(
        registry
            .resolve_agent_reference(&agent_dir, "../outside.yaml")
            .await,
    );
    assert!(
        err.contains(NOT_VALID_RELATIVE_PATH),
        "ResolveAgentReference through a symlinked parent with an escaping reference = {err}, want {NOT_VALID_RELATIVE_PATH:?}"
    );
}

// parity: adapted: the Unix branch only (filepath.VolumeName is always empty there, so every reference is accepted); resolveConfigReference is driven through ResolveAgentReference, whose not-found read Go also accepts
#[tokio::test]
async fn test_resolve_config_reference_volume_qualified_refs() {
    let (_guard, _base, agent_dir, _) = new_agent_dir();
    let registry = Registry::with_agent_classes();

    for ref_path in [
        r"C:node.yaml",
        r"D:\escaped.yaml",
        r"\\host\share\escaped.yaml",
    ] {
        // Volume-qualified on this platform means the reference escapes and must be refused;
        // otherwise it is a legal file name and must be accepted.
        let err = err_text(registry.resolve_agent_reference(&agent_dir, ref_path).await);
        assert!(
            !is_any_refusal(&err),
            "resolveConfigReference({agent_dir:?}, {ref_path:?}) rejected ({err}), want rejected = false"
        );
    }
}

// parity: adapted: resolveConfigReference returning the real path is ResolveAgentReference reading the linked file (a JoinNode config is no agent class, so the loader fails on that, not as a refusal or a missing file)
#[ignore = "8a bug: ResolveAgentReference resolves the reference against the parent of the config directory, so the inside link is not found (Go #1299)"]
#[tokio::test]
async fn test_resolve_config_reference_accepts_links_that_stay_inside() {
    const NODE_YAML: &str = "name: join_node\nagent_class: JoinNode\n";

    fn alias_layout(dir: &Path) -> &'static str {
        let target = write(&dir.join("nodes"), "join.yaml", NODE_YAML);
        std::os::unix::fs::symlink(target, dir.join("alias.yaml")).unwrap();
        "alias.yaml"
    }
    fn projected_volume_layout(dir: &Path) -> &'static str {
        let data = dir.join("..2026_09_01_10_00_00.123456");
        write(&data, "join.yaml", NODE_YAML);
        std::os::unix::fs::symlink(&data, dir.join("..data")).unwrap();
        std::os::unix::fs::symlink(Path::new("..data").join("join.yaml"), dir.join("join.yaml"))
            .unwrap();
        "join.yaml"
    }

    type Layout = fn(&Path) -> &'static str;
    let tests: [(&str, Layout); 2] = [
        ("alias to a sibling in the same directory", alias_layout),
        (
            "kubernetes projected volume layout",
            projected_volume_layout,
        ),
    ];
    for (name, layout) in tests {
        let (_guard, dir) = temp_dir();
        let ref_path = layout(&dir);
        let registry = Registry::with_agent_classes();

        let err = err_text(registry.resolve_agent_reference(&dir, ref_path).await);
        assert!(
            !is_any_refusal(&err) && err.contains("invalid agent class 'JoinNode'"),
            "{name}: resolveConfigReference({dir:?}, {ref_path:?}) = {err}, want success: a link that stays inside the directory keeps loading"
        );
    }
}

// parity: adapted: resolveConfigReference's keys are read off the registry's agent cache, keyed by the path ResolveAgentReference loaded
#[ignore = "8a bug: ResolveAgentReference resolves against the parent of the config directory and keys the agent cache by the unresolved (symlinked) spelling, not the real path (Go #1299)"]
#[tokio::test]
async fn test_resolve_config_reference_aliased_directory_share_key() {
    let (_guard, base) = temp_dir();
    let real_dir = base.join("real");
    // SequentialAgent, not LlmAgent: an LlmAgent needs a model.
    write(
        &real_dir,
        "sub_agent.yaml",
        "name: sub_agent\nagent_class: SequentialAgent\n",
    );
    let alias_dir = base.join("alias");
    std::os::unix::fs::symlink(&real_dir, &alias_dir).unwrap();
    let registry = Registry::with_agent_classes();

    let real_key = real_dir.join("sub_agent.yaml");
    if let Err(err) = registry
        .resolve_agent_reference(&base, "real/sub_agent.yaml")
        .await
    {
        panic!("ResolveAgentReference through the real directory failed: {err}");
    }
    if let Err(err) = registry
        .resolve_agent_reference(&base, "alias/sub_agent.yaml")
        .await
    {
        panic!("ResolveAgentReference through an inside link failed: {err}");
    }
    let g = registry.inner.read();
    assert!(
        g.agents.contains_key(&real_key),
        "agentRegistry has no entry keyed by the real path {real_key:?}"
    );
    let alias_key = alias_dir.join("sub_agent.yaml");
    assert!(
        !g.agents.contains_key(&alias_key),
        "agentRegistry holds an entry keyed by the symlinked spelling {alias_key:?}"
    );
}

// parity: adapted: resolveConfigReference is driven through ResolveAgentReference (no refusal; the read reports the missing file)
#[tokio::test]
async fn test_resolve_config_reference_below_non_directory() {
    let (_guard, _base, agent_dir, _) = new_agent_dir();
    let registry = Registry::with_agent_classes();

    // sub_agent.yaml is a regular file, so sub_agent.yaml/nested.yaml cannot exist.
    let ref_path = "sub_agent.yaml/nested.yaml";
    let err = err_text(registry.resolve_agent_reference(&agent_dir, ref_path).await);
    assert!(
        !is_any_refusal(&err),
        "resolveConfigReference({agent_dir:?}, {ref_path:?}) = {err}, want no error: the reference cannot exist, which is the caller's read to report"
    );
}

// parity: adapted: resolveConfigReference is driven through ResolveAgentReference
#[ignore = "8a bug: an empty reference is refused with \"agent reference path cannot be empty\", not utils.ErrNotValidRelativePath's \"…: rel path is empty\" (Go #1299)"]
#[tokio::test]
async fn test_resolve_config_reference_empty_ref() {
    let (_guard, _base, agent_dir, _) = new_agent_dir();
    let registry = Registry::with_agent_classes();

    let err = err_text(registry.resolve_agent_reference(&agent_dir, "").await);
    assert!(
        err.contains(NOT_VALID_RELATIVE_PATH),
        "resolveConfigReference({agent_dir:?}, \"\") = {err}, want {NOT_VALID_RELATIVE_PATH:?}"
    );
    assert!(
        err.contains("empty"),
        "resolveConfigReference({agent_dir:?}, \"\") = {err:?}, want the message to name the reference as empty"
    );
}

// parity: adapted: the Unix branch (runtime.GOOS != "windows"); resolveConfigReference is driven through ResolveAgentReference, so Go's check that an accepted path stays inside the directory (withinDir) has no returned path to inspect
#[ignore = "8a bug: an absolute reference is refused with \"absolute paths are not allowed\", not utils.ErrNotValidRelativePath (Go #1299)"]
#[tokio::test]
async fn test_resolve_config_reference_os_specific_refs() {
    let (_guard, base) = temp_dir();
    let agent_dir = base.join("agents").join("root");
    let agent_path = write(&agent_dir, "root_agent.yaml", "agent_class: LlmAgent\n");
    let registry = Registry::with_agent_classes();

    let windows = false;
    let tests = [
        ("reserved name NUL", "NUL", windows),
        ("reserved name COM1", "COM1", windows),
        ("reserved name LPT1", "LPT1", windows),
        (
            "reserved name PhysicalDrive0",
            r"\\.\PhysicalDrive0",
            windows,
        ),
        ("reserved name direct c:", r"\\.\C:", windows),
        (r"reserved name direct c:\", r"\\.\C:\", windows),
        ("reserved name Charger0", r"\\.\Charger0", windows),
        ("reserved name CON", "CON", windows),
        ("reserved name CONIN$", "CONIN$", windows),
        ("reserved name CONOUT$", "CONOUT$", windows),
        // Backslash as a separator (Windows) climbs out; as a literal character (Unix) it is
        // one file name that stays put.
        ("backslash traversal", r"..\..\outside.yaml", windows),
        // Accepted on both, and contained on both.
        ("backslash subdir", r"sub\child.yaml", false),
        // Drive-relative: a volume name on Windows, a legal name on Unix.
        ("drive-relative ref", "C:node.yaml", windows),
        // Rooted on every platform.
        ("posix absolute", "/etc/passwd", true),
    ];

    for (name, ref_path, want_rejected) in tests {
        let err = err_text(registry.resolve_agent_reference(&agent_dir, ref_path).await);
        let got_rejected = is_any_refusal(&err);
        assert_eq!(
            got_rejected, want_rejected,
            "{name}: resolveConfigReference({agent_dir:?}, {ref_path:?}) rejected = {got_rejected} ({err}), want rejected = {want_rejected}"
        );
        if got_rejected {
            // A rejection here is the lexical containment check refusing the reference,
            // never a later failure to load what it names.
            assert!(
                err.contains(NOT_VALID_RELATIVE_PATH),
                "{name}: resolveConfigReference({agent_path:?}, {ref_path:?}) = {err}, want {NOT_VALID_RELATIVE_PATH:?}"
            );
        }
    }
}

// parity: adapted: Go's t.Setenv("GOOGLE_API_KEY") is the registry's environment (fake_key_env); Go init()'s registries are Registry::new()
#[ignore = "8a bug: an escaping sub_agents/AgentTool config_path is refused with \"path traversal detected\", not a utils.SafeSubpath sentinel wrapped as `config reference \"<ref>\": …` (Go #1299)"]
#[tokio::test]
async fn test_from_config_refuses_escaping_agent_references() {
    let (_guard, base, agent_dir, _) = new_agent_dir();
    write(
        &agent_dir,
        "sub.yaml",
        "name: sub\nagent_class: SequentialAgent\n",
    );

    let tests = [
        (
            "sub_agents inside",
            "name: root\nagent_class: SequentialAgent\nsub_agents:\n  - config_path: sub.yaml\n",
            "sub.yaml",
            false,
        ),
        (
            "sub_agents escaping",
            "name: root\nagent_class: SequentialAgent\nsub_agents:\n  - config_path: ../../outside.yaml\n",
            "../../outside.yaml",
            true,
        ),
        (
            "AgentTool escaping",
            "name: root\nagent_class: LlmAgent\nmodel: gemini-2.5-flash\ntools:\n  - name: AgentTool\n    args:\n      agent:\n        config_path: ../../outside.yaml\n",
            "../../outside.yaml",
            true,
        ),
    ];

    for (name, config, ref_path, want_err) in tests {
        let registry = Registry::new();
        registry.set_env(fake_key_env());
        let path = write(
            &agent_dir,
            &format!("{}.yaml", name.replace(' ', "_")),
            config,
        );
        let result = registry.from_config(&path).await;
        if !want_err {
            if let Err(err) = result {
                panic!("{name}: FromConfig({path:?}) = {err}, want success");
            }
            continue;
        }
        let err = err_text(result);
        assert!(
            is_containment_rejection(&err),
            "{name}: FromConfig({path:?}) = {err}, want a containment rejection of {ref_path:?} (outside file: {:?})",
            base.join("outside.yaml")
        );
        assert!(
            err.contains(&format!("{ref_path:?}")),
            "{name}: FromConfig({path:?}) = {err:?}, want the error to name the reference {ref_path:?}"
        );
    }
}

// parity: adapted: Go init()'s registries are a `Registry` value with Go init()'s agent classes (test_registry)
#[ignore = "8a bug: a sub_agents link escaping through \"s/../y.yaml\" is refused with \"path traversal detected\", not utils.ErrSymlinkInRelativePath wrapped as `config reference \"x.yaml\": …` (Go #1299)"]
#[tokio::test]
async fn test_from_config_dot_dot_after_link_in_target() {
    use std::os::unix::fs::symlink;

    let (_guard, root) = temp_dir();
    let agent_dir = root.join("agent");
    let outside = root.join("outside").join("p");
    let registry = test_registry();

    // s -> a directory outside agentDir; x.yaml -> "s/../y.yaml", which the loader resolves
    // to outside/p/y.yaml.
    write(
        &outside,
        "y.yaml",
        "agent_class: SequentialAgent\nname: outside_y\n",
    );
    write(&outside.join("q"), ".keep", "");
    write(&agent_dir, ".keep", "");
    symlink(outside.join("q"), agent_dir.join("s")).unwrap();
    symlink("s/../y.yaml", agent_dir.join("x.yaml")).unwrap();
    write(
        &agent_dir,
        "escape.yaml",
        "agent_class: SequentialAgent\nname: root\nsub_agents:\n  - config_path: x.yaml\n",
    );
    let err = err_text(registry.from_config(&agent_dir.join("escape.yaml")).await);
    assert!(
        err.contains(SYMLINK_IN_RELATIVE_PATH),
        "FromConfig(escape.yaml) = {err}, want {SYMLINK_IN_RELATIVE_PATH:?}: x.yaml opens {:?}",
        outside.join("y.yaml")
    );
    assert!(
        err.is_empty() || err.contains(r#"config reference "x.yaml""#),
        "FromConfig(escape.yaml) = {err:?}, want the error to name the reference"
    );

    // d -> sub1/sub2 (inside); back.yaml -> "d/../../in.yaml", which the loader resolves to
    // agentDir/in.yaml.
    write(
        &agent_dir,
        "in.yaml",
        "agent_class: SequentialAgent\nname: inside\n",
    );
    write(&agent_dir.join("sub1").join("sub2"), ".keep", "");
    symlink("sub1/sub2", agent_dir.join("d")).unwrap();
    symlink("d/../../in.yaml", agent_dir.join("back.yaml")).unwrap();
    write(
        &agent_dir,
        "inside.yaml",
        "agent_class: SequentialAgent\nname: root2\nsub_agents:\n  - config_path: back.yaml\n",
    );
    if let Err(err) = registry.from_config(&agent_dir.join("inside.yaml")).await {
        panic!(
            "FromConfig(inside.yaml) = {err}, want success: back.yaml opens {:?}",
            agent_dir.join("in.yaml")
        );
    }
}
