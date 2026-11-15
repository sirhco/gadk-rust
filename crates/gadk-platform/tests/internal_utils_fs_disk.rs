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

//! Port of ADK-Go `internal/utils/fs_disk_test.go` (`SafeSubpath` against a real directory).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use gadk_platform::{SafeSubpathErrorKind, go_filepath_rel, safe_subpath};

/// Go `t.TempDir()`: a fresh directory under the system temp directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("gadk-safesubpath-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    /// Go's `filepath.EvalSymlinks(t.TempDir())` (macOS's /var is a link to /private/var).
    fn resolved(&self) -> PathBuf {
        std::fs::canonicalize(&self.0).unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn kind(base: &Path, rel: &str) -> Option<SafeSubpathErrorKind> {
    safe_subpath(base, rel).err().map(|e| e.kind())
}

/// Go `TestSafeSubpath_OnDisk`: drives `SafeSubpath` against a real directory, so the
/// component walk runs instead of stopping at a base that does not exist.
#[test]
fn test_safe_subpath_on_disk() {
    let tmp = TempDir::new();
    let base = tmp.resolved();
    std::fs::write(base.join("file.txt"), b"x").unwrap();

    // existing file resolves to the file, not the base
    let (got, rel) = safe_subpath(&base, "file.txt").unwrap();
    assert_eq!(got, base.join("file.txt"));
    assert_eq!(rel, "file.txt");

    // A missing or mistyped reference is the caller's read to report, not a containment
    // refusal.
    for (name, rel) in [
        ("missing file", "sbu.yaml"),
        ("missing directory", "nodes/n.yaml"),
        ("below a regular file", "file.txt/nested.yaml"),
    ] {
        let (got, _) = safe_subpath(&base, rel).unwrap_or_else(|e| {
            panic!("{name}: SafeSubpath({base:?}, {rel:?}) = {e}, want success")
        });
        assert_eq!(got, base.join(rel), "{name}");
    }

    // symlinked base is accepted, a link below it is judged by where it lands
    let real_dir = base.join("real");
    std::fs::create_dir(&real_dir).unwrap();
    std::fs::write(real_dir.join("f.txt"), b"").unwrap();
    let alias = base.join("alias");
    symlink(&real_dir, &alias).unwrap();
    assert_eq!(kind(&alias, "f.txt"), None, "the base is trusted");
    // a link that stays inside base keeps loading
    assert_eq!(
        kind(&base, "alias/f.txt"),
        None,
        "the link stays inside base"
    );

    // a link that leaves base is refused, also with a missing tail below it
    let outside_tmp = TempDir::new();
    let outside = outside_tmp.resolved();
    std::fs::write(outside.join("secret.txt"), b"").unwrap();
    symlink(&outside, base.join("out")).unwrap();
    for rel in [
        "out/secret.txt",
        "out/missing.txt",
        "out/missing/deeper.txt",
    ] {
        assert_eq!(
            kind(&base, rel),
            Some(SafeSubpathErrorKind::SymlinkInRelativePath),
            "{rel}"
        );
    }
    // a relative link destination with ".." is applied where the link sits, as the operating
    // system applies it: one climbs to base/f.txt and stays inside, the other climbs out to
    // the outside directory and is refused
    symlink(Path::new("..").join("f.txt"), real_dir.join("up.txt")).unwrap();
    assert_eq!(
        kind(&base, "real/up.txt"),
        None,
        "../f.txt from real/ is base/f.txt"
    );
    let rel_out = go_filepath_rel(
        &real_dir.to_string_lossy(),
        &outside.join("secret.txt").to_string_lossy(),
    )
    .unwrap();
    symlink(&rel_out, real_dir.join("climb.txt")).unwrap();
    assert_eq!(
        kind(&base, "real/climb.txt"),
        Some(SafeSubpathErrorKind::SymlinkInRelativePath),
        "the link climbs out through {rel_out:?}"
    );

    // a dangling link is judged by where it points: outside is refused, inside is returned
    // so the caller's read reports the missing file
    symlink(outside.join("nope"), base.join("dangling_out")).unwrap();
    assert_eq!(
        kind(&base, "dangling_out"),
        Some(SafeSubpathErrorKind::SymlinkInRelativePath)
    );
    symlink("real/nope", base.join("dangling_in")).unwrap();
    assert_eq!(
        kind(&base, "dangling_in"),
        None,
        "the link points inside base"
    );
    symlink(outside.join("secret.txt"), real_dir.join("sneak.txt")).unwrap();
    assert_eq!(
        kind(&base, "real/sneak.txt"),
        Some(SafeSubpathErrorKind::SymlinkInRelativePath)
    );

    // ConfigMap-style layout keeps loading (Kubernetes mounts: key -> ..data/key,
    // ..data -> ..<timestamp>/)
    let cm_tmp = TempDir::new();
    let cm = cm_tmp.0.clone();
    let ts = cm.join("..2026_10_22");
    std::fs::create_dir(&ts).unwrap();
    std::fs::write(ts.join("node.yaml"), b"").unwrap();
    symlink("..2026_10_22", cm.join("..data")).unwrap();
    symlink(Path::new("..data").join("node.yaml"), cm.join("node.yaml")).unwrap();
    assert_eq!(
        kind(&cm, "node.yaml"),
        None,
        "a ConfigMap-style link is accepted"
    );
}

/// Go `TestSafeSubpath_FilesystemRoot`: a base that is a filesystem root, where the "base
/// plus separator" prefix is itself root-shaped.
#[test]
fn test_safe_subpath_filesystem_root() {
    let root = Path::new("/");
    let (got, rel) = safe_subpath(root, "nodes/n.yaml").unwrap();
    assert_eq!(got, PathBuf::from("/nodes/n.yaml"));
    assert_eq!(rel, "nodes/n.yaml");
    assert_eq!(
        kind(root, "../x"),
        Some(SafeSubpathErrorKind::NotValidRelativePath)
    );
}

// parity: skipped TestSafeSubpath_RootedLinkTargetWithoutDrive: Windows-only (a rooted link target without a drive exists only on Windows; Go skips it elsewhere)

/// Go `TestSafeSubpath_DotDotAfterLinkInTarget`: a link whose target has a ".." after
/// another link (`s/../y.yaml`) applies the ".." to what `s` points at.
#[test]
fn test_safe_subpath_dot_dot_after_link_in_target() {
    let tmp = TempDir::new();
    let root = tmp.resolved();
    let base = root.join("base");
    let outside = root.join("outside").join("p");
    for d in [
        base.clone(),
        base.join("sub1").join("sub2"),
        base.join("deep"),
        outside.join("q"),
    ] {
        std::fs::create_dir_all(&d).unwrap();
    }
    for f in [
        outside.join("y.yaml"),
        base.join("in.yaml"),
        root.join("x.yaml"),
    ] {
        std::fs::write(&f, b"").unwrap();
    }
    // s -> a directory outside base; x.yaml -> "s/../y.yaml" opens outside/p/y.yaml.
    symlink(outside.join("q"), base.join("s")).unwrap();
    symlink("s/../y.yaml", base.join("x.yaml")).unwrap();
    // deep/up -> "..", which stays inside; chain.yaml -> "deep/up/../x.yaml" opens
    // root/x.yaml, one level above base, through links that each stay inside.
    symlink("..", base.join("deep").join("up")).unwrap();
    symlink("deep/up/../x.yaml", base.join("chain.yaml")).unwrap();
    // d -> sub1/sub2 (inside); back.yaml -> "d/../../in.yaml" opens base/in.yaml.
    symlink("sub1/sub2", base.join("d")).unwrap();
    symlink("d/../../in.yaml", base.join("back.yaml")).unwrap();
    // escape -> outside; gap.yaml -> "missing/../escape/y.yaml". Nothing is at "missing", but
    // the ".." climbs back into base, and escape must still be followed.
    symlink(&outside, base.join("escape")).unwrap();
    symlink("missing/../escape/y.yaml", base.join("gap.yaml")).unwrap();
    // stub.yaml -> "missing/../in.yaml" opens base/in.yaml by the same rule.
    symlink("missing/../in.yaml", base.join("stub.yaml")).unwrap();

    for rel in ["x.yaml", "chain.yaml", "gap.yaml"] {
        assert_eq!(
            kind(&base, rel),
            Some(SafeSubpathErrorKind::SymlinkInRelativePath),
            "{rel}: the link opens a file outside base"
        );
    }
    for rel in ["back.yaml", "stub.yaml"] {
        let (got, _) = safe_subpath(&base, rel)
            .unwrap_or_else(|e| panic!("{rel}: {e}, want success: the link opens base/in.yaml"));
        assert_eq!(got, base.join("in.yaml"), "{rel}");
    }
}
