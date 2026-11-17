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

//! [`safe_subpath`]: the containment check for a relative path read from an untrusted config,
//! with its two error kinds ([`ERR_NOT_VALID_RELATIVE_PATH`],
//! [`ERR_SYMLINK_IN_RELATIVE_PATH`]) and [`is_link_like`].
//!
// Unix path semantics only, as in the other path helpers (`go_path_clean`, `go_filepath_rel`,
// `go_filepath_is_local`); the Windows rules (forbidden characters, trailing dot/space,
// drives, UNC prefixes, reserved device names, junctions) are not implemented.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::go_err::go_errno_text;
use crate::go_fmt::go_quote;
use crate::go_path::{
    go_filepath_is_local, go_filepath_rel, go_path_clean, go_path_dir, go_path_join,
};

/// Text of the error for a reference that, read as a string, is not a relative path below the
/// base directory.
pub const ERR_NOT_VALID_RELATIVE_PATH: &str =
    "path is not a relative path inside the base directory";

/// Text of the error for a link on the way that leads outside the base directory. A link that
/// stays inside is not an error.
pub const ERR_SYMLINK_IN_RELATIVE_PATH: &str =
    "path leads outside the base directory through a link";

/// Which kind of failure a [`SafeSubpathError`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SafeSubpathErrorKind {
    /// The reference is not a relative path below the base ([`ERR_NOT_VALID_RELATIVE_PATH`]).
    NotValidRelativePath,
    /// A link on the way leads outside the base ([`ERR_SYMLINK_IN_RELATIVE_PATH`]).
    SymlinkInRelativePath,
    /// Neither sentinel: a base that is not absolute, or a filesystem error while following
    /// links (`cannot resolve links: lstat …`, `too many links in …`).
    Other,
}

/// An error of [`safe_subpath`]: its [`kind`](Self::kind) and full error text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeSubpathError {
    kind: SafeSubpathErrorKind,
    message: String,
}

impl SafeSubpathError {
    fn not_valid(detail: &str) -> Self {
        Self {
            kind: SafeSubpathErrorKind::NotValidRelativePath,
            message: format!("{ERR_NOT_VALID_RELATIVE_PATH}: {detail}"),
        }
    }

    fn symlink(detail: &str) -> Self {
        Self {
            kind: SafeSubpathErrorKind::SymlinkInRelativePath,
            message: format!("{ERR_SYMLINK_IN_RELATIVE_PATH}: {detail}"),
        }
    }

    fn other(message: String) -> Self {
        Self {
            kind: SafeSubpathErrorKind::Other,
            message,
        }
    }

    /// Which kind of failure this error reports.
    pub fn kind(&self) -> SafeSubpathErrorKind {
        self.kind
    }

    /// True for [`SafeSubpathErrorKind::NotValidRelativePath`].
    pub fn is_not_valid_relative_path(&self) -> bool {
        self.kind == SafeSubpathErrorKind::NotValidRelativePath
    }

    /// True for [`SafeSubpathErrorKind::SymlinkInRelativePath`].
    pub fn is_symlink_in_relative_path(&self) -> bool {
        self.kind == SafeSubpathErrorKind::SymlinkInRelativePath
    }
}

impl fmt::Display for SafeSubpathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SafeSubpathError {}

/// Resolves `rel` below `base` and checks that the result stays inside it.
/// stays inside it.
///
/// `base` must be absolute and is trusted; `rel` is not. On success the result is the
/// absolute path the check followed `rel` to, every link on the way replaced by its target,
/// and the base-relative spelling of `rel`. A caller that opens the returned path opens the
/// file the check judged.
///
/// [`SafeSubpathErrorKind::NotValidRelativePath`] iff `rel` is not a local path below `base`
/// (absolute, empty, escaping through `..`, effectively empty, or carrying a NUL byte).
/// [`SafeSubpathErrorKind::SymlinkInRelativePath`] iff following the links on the way lands
/// outside `base`, itself followed the same way. A link that lands inside is accepted (which
/// keeps Kubernetes ConfigMap and Secret mounts working); a link whose target does not exist
/// is judged by where it points. A component that does not exist, or that sits below a
/// non-directory, is not an error: the path is returned and the caller's read reports it.
pub fn safe_subpath(base: &Path, rel: &str) -> Result<(PathBuf, String), SafeSubpathError> {
    // Validation order: base, empty, NUL, locality, containment, then links.
    let base = base.to_string_lossy();
    if !base.starts_with('/') {
        return Err(SafeSubpathError::other(
            "base path is not absolute".to_owned(),
        ));
    }
    let abs_base = go_path_clean(&base);

    if rel.is_empty() {
        return Err(SafeSubpathError::not_valid("rel path is empty"));
    }
    // NUL char is not allowed both on linux and windows
    if rel.contains('\0') {
        return Err(SafeSubpathError::not_valid("rel path contains null byte"));
    }
    // IsLocal settles absolute paths and any ".." that escapes.
    if !go_filepath_is_local(rel) {
        return Err(SafeSubpathError::not_valid("rel path is not local"));
    }

    // check if rel stays below absBase. Rel + IsLocal, not a string prefix: the prefix
    // absBase+separator is wrong for a filesystem root ("//").
    let res = go_path_join(&[abs_base.as_str(), rel]);
    let r = go_filepath_rel(&abs_base, &res)
        .map_err(|e| SafeSubpathError::not_valid(&format!("filepath.Rel failed: {e}")))?;
    if r == "." {
        // rel is effectively empty
        return Err(SafeSubpathError::not_valid("rel is effectively empty"));
    }
    if !go_filepath_is_local(&r) {
        return Err(SafeSubpathError::not_valid("not a subpath"));
    }

    let real = resolve_inside(&abs_base, &res)?;
    Ok((PathBuf::from(real), r))
}

/// Follows the links in `target` and reports where it really lands;
/// [`ERR_SYMLINK_IN_RELATIVE_PATH`] when that is not below `base`, itself followed the same
/// way so a base reached through a link is trusted.
fn resolve_inside(base: &str, target: &str) -> Result<String, SafeSubpathError> {
    // Resolve both paths through their links, then compare them.
    let real_base = follow_links(base)
        .map_err(|e| SafeSubpathError::other(format!("cannot resolve links in base: {e}")))?;
    let real_target = follow_links(target)
        .map_err(|e| SafeSubpathError::other(format!("cannot resolve links: {e}")))?;
    match go_filepath_rel(&real_base, &real_target) {
        Ok(rel) if rel == "." || go_filepath_is_local(&rel) => Ok(real_target),
        _ => Err(SafeSubpathError::symlink(&format!(
            "it resolves to {}",
            go_quote(&real_target)
        ))),
    }
}

/// The number of links followed on one path before giving up, so a link loop is an error
/// rather than a hang.
const MAX_LINK_FOLLOWS: usize = 255;

/// Walks the absolute path `p` one component at a time and replaces every
/// link by its target, splicing the target in uncleaned so a `..` in it applies to whatever
/// the walk has reached (in `s/../y.yaml`, what `s` points at). A link whose target does not
/// exist is still followed by name; a component that does not exist, or sits below a
/// non-directory, is walked past as a plain name (a later `..` can climb back into a
/// directory that exists, and the components after it are still examined).
fn follow_links(p: &str) -> Result<String, String> {
    // Components are consumed left to right; a link target is spliced in front of the rest.
    let mut cur = "/".to_owned();
    let mut rest: Vec<String> = p.trim_matches('/').split('/').map(str::to_owned).collect();
    let mut follows = 0usize;
    let mut i = 0usize;
    while i < rest.len() {
        let c = rest[i].clone();
        match c.as_str() {
            "" | "." => {
                i += 1;
                continue;
            }
            ".." => {
                cur = go_path_dir(&cur);
                i += 1;
                continue;
            }
            _ => {}
        }
        let next = go_path_join(&[cur.as_str(), c.as_str()]);
        let meta = match std::fs::symlink_metadata(&next) {
            Ok(meta) => meta,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                // Not a link, so nothing to follow; keep walking by name.
                cur = next;
                i += 1;
                continue;
            }
            Err(e) => return Err(format!("lstat {next}: {}", go_errno_text(&e))),
        };
        if !is_link_like(&meta.file_type()) {
            cur = next;
            i += 1;
            continue;
        }
        let dest = match std::fs::read_link(&next) {
            Ok(dest) => dest.to_string_lossy().into_owned(),
            Err(_) => {
                // A reparse point that names no target redirects the read nowhere, so it is
                // an ordinary entry here.
                cur = next;
                i += 1;
                continue;
            }
        };
        follows += 1;
        if follows > MAX_LINK_FOLLOWS {
            return Err(format!("too many links in {}", go_quote(p)));
        }
        // Continue the walk from the target's components, then whatever followed the link.
        // The target is not cleaned: a ".." in it is applied by the walk, after the component
        // before it has been followed. A relative target is read from the directory the link
        // sits in, which is cur.
        if dest.starts_with('/') {
            cur = "/".to_owned();
        }
        let mut spliced: Vec<String> = dest
            .trim_matches('/')
            .split('/')
            .map(str::to_owned)
            .collect();
        spliced.extend(rest.drain(i + 1..));
        rest = spliced;
        i = 0;
    }
    Ok(go_path_clean(&cur))
}

/// Whether a directory entry may redirect a read somewhere other than where its own name sits
/// in the tree. On Unix that is a symbolic link.
pub fn is_link_like(file_type: &std::fs::FileType) -> bool {
    // Only symbolic links redirect a read on Unix.
    file_type.is_symlink()
}
