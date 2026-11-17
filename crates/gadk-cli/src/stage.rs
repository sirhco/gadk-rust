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

//! The build context a deploy ships: the two-stage Dockerfile's text, the staged source
//! tree (copy and archive) and the temp-dir helper (a `<prefix>YYYYMMDD_HHMMSS__*`
//! directory).

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, Utc};
use flate2::Compression;
use flate2::write::GzEncoder;
use tar::{Builder, Header, HeaderMode};

/// The runtime image (glibc, Debian 12 like the `rust:*-bookworm` builder).
pub const RUNTIME_IMAGE: &str = "gcr.io/distroless/cc-debian12";
/// The builder's `cargo build --target-dir`.
pub const TARGET_DIR: &str = "/gadk-target";

/// The builder stage of the generated Dockerfile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuilderStage {
    /// `rust:<ver>-bookworm` or `rust:bookworm` ([`crate::target::builder_image`]).
    pub image: String,
    /// The workspace root relative to the staged root (`""` for Cloud Run): `WORKDIR /app/<workdir>`.
    pub workdir: String,
    /// `cargo build -p <package>`.
    pub package: String,
    /// `cargo build --bin <bin>`; the runtime stage's `/app/<bin>`.
    pub bin: String,
}

/// The whole Dockerfile, starting with a leading newline: the builder stage, the
/// distroless runtime stage, `EXPOSE <server_port>`, a comment and
/// `CMD ["/app/<bin>", "web", "-host", "0.0.0.0", "-port", "<server_port>"` + `cmd_tail` + `]`
/// (`cmd_tail` is a segment list, each starting `, `).
// The build happens remotely in a `rust:<ver>-bookworm` stage
// (`cargo build --release --locked --target-dir /gadk-target -p <pkg> --bin <bin>`, the
// whole staged root copied to `/app/`) and the image runs on `distroless/cc-debian12`. The
// runtime stage's `EXPOSE`, comment and `CMD` match ADK's generated Dockerfile.
pub fn dockerfile(builder: &BuilderStage, server_port: i64, cmd_tail: &str) -> String {
    let BuilderStage {
        image,
        workdir,
        package,
        bin,
    } = builder;
    format!(
        "
FROM {image} AS builder
WORKDIR /app/{workdir}
COPY . /app/
RUN cargo build --release --locked --target-dir {TARGET_DIR} -p {package} --bin {bin}

FROM {RUNTIME_IMAGE}

COPY --from=builder {TARGET_DIR}/release/{bin}  /app/{bin}
EXPOSE {server_port}
# Command to run the executable when the container starts
CMD [\"/app/{bin}\", \"web\", \"-host\", \"0.0.0.0\", \"-port\", \"{server_port}\"{cmd_tail}]"
    )
}

/// What a [`SourceTree`] entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// One staged path, relative to the tree's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: EntryKind,
}

/// The files of a source root in sorted path order, without `.git`, `target` (at any depth)
/// and the excluded paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceTree {
    pub root: PathBuf,
    pub entries: Vec<Entry>,
}

/// Names never staged, at any depth: VCS data (`.git`) and Cargo build output.
// `.git` and `target` are excluded at any depth. Cloud Run sources are staged too: no
// prebuilt binary is uploaded.
const EXCLUDED_NAMES: [&str; 2] = [".git", "target"];

impl SourceTree {
    /// Walks `root`, skipping `.git` and `target` at any depth and every absolute path in
    /// `exclude` (the temp dir when nested; the root `Dockerfile` for an archive). Entries are
    /// sorted by path (component-wise); symbolic links are recorded as links and not followed
    /// (as `tar` and `gcloud`'s upload do); other special files are skipped.
    pub fn walk(root: &Path, exclude: &[PathBuf]) -> io::Result<SourceTree> {
        let mut entries = Vec::new();
        walk_dir(root, Path::new(""), exclude, &mut entries)?;
        Ok(SourceTree {
            root: root.to_path_buf(),
            entries,
        })
    }

    /// Copies the entries under `dir`: directories created, files copied with their
    /// permissions, symbolic links recreated with the same target.
    pub fn copy_into(&self, dir: &Path) -> io::Result<()> {
        for entry in &self.entries {
            let src = self.root.join(&entry.path);
            let dst = dir.join(&entry.path);
            match entry.kind {
                EntryKind::Dir => fs::create_dir_all(&dst)?,
                EntryKind::File => {
                    fs::copy(&src, &dst)?;
                }
                EntryKind::Symlink => std::os::unix::fs::symlink(fs::read_link(&src)?, &dst)?,
            }
        }
        Ok(())
    }

    /// Writes a gzip tar of the entries in order at `path`, then each `extra` `(name, bytes)`
    /// file at the archive root (the generated `Dockerfile` last).
    // The archive is written in-process: sorted entries without a `./` prefix, deterministic
    // headers (fixed mtime, uid/gid 0, mode 0644/0755), the extra files appended last.
    pub fn write_tar_gz(&self, path: &Path, extra: &[(&str, &[u8])]) -> io::Result<()> {
        let mut tar = Builder::new(GzEncoder::new(File::create(path)?, Compression::default()));
        tar.mode(HeaderMode::Deterministic);
        tar.follow_symlinks(false);
        for entry in &self.entries {
            tar.append_path_with_name(self.root.join(&entry.path), &entry.path)?;
        }
        for (name, data) in extra {
            let mut header = Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(tar::DETERMINISTIC_TIMESTAMP);
            header.set_cksum();
            tar.append_data(&mut header, name, *data)?;
        }
        let mut file = tar.into_inner()?.finish()?;
        file.flush()
    }
}

/// Appends the sorted entries of `root/rel` (recursively) to `out`.
fn walk_dir(root: &Path, rel: &Path, exclude: &[PathBuf], out: &mut Vec<Entry>) -> io::Result<()> {
    let mut children: Vec<(PathBuf, fs::FileType)> = fs::read_dir(root.join(rel))?
        .map(|e| e.and_then(|e| Ok((rel.join(e.file_name()), e.file_type()?))))
        .collect::<io::Result<_>>()?;
    children.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, file_type) in children {
        let name = path.file_name().unwrap_or(OsStr::new(""));
        if EXCLUDED_NAMES.iter().any(|n| name == *n) || exclude.contains(&root.join(&path)) {
            continue;
        }
        if file_type.is_symlink() {
            out.push(Entry {
                path,
                kind: EntryKind::Symlink,
            });
        } else if file_type.is_dir() {
            out.push(Entry {
                path: path.clone(),
                kind: EntryKind::Dir,
            });
            walk_dir(root, &path, exclude, out)?;
        } else if file_type.is_file() {
            out.push(Entry {
                path,
                kind: EntryKind::File,
            });
        }
    }
    Ok(())
}

/// A new directory under `base` named `<prefix>YYYYMMDD_HHMMSS__<random>` with
/// `now` in local time (`chrono::Local`); returns the created directory (mode 0700, a
/// random decimal suffix, retried on a name clash up to 10000 attempts).
pub fn make_temp(base: &Path, prefix: &str, now: DateTime<Utc>) -> io::Result<PathBuf> {
    let stamp = now.with_timezone(&Local).format("%Y%m%d_%H%M%S__");
    let random = RandomState::new();
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    for attempt in 0u64..10_000 {
        // The decimal form of a random uint32.
        let suffix = random.hash_one(attempt) as u32;
        let dir = base.join(format!("{prefix}{stamp}{suffix}"));
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "mkdirtemp {}: file exists",
            base.join(format!("{prefix}{stamp}*")).display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use chrono::{DateTime, Local, Utc};
    use flate2::read::GzDecoder;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::testutil::default_now;

    #[test]
    fn dockerfile_golden_cloud_run_shape() {
        let builder = BuilderStage {
            image: "rust:1.99.1-bookworm".into(),
            workdir: String::new(),
            package: "my-agent".into(),
            bin: "agent".into(),
        };
        let tail = r#", "api", "-webui_address", "127.0.0.1:8081", "a2a", "--a2a_agent_url", "http://127.0.0.1:8081", "webui", "--api_server_address", "http://127.0.0.1:8081/api""#;

        let got = dockerfile(&builder, 8080, tail);

        assert_eq!(
            got,
            r#"
FROM rust:1.99.1-bookworm AS builder
WORKDIR /app/
COPY . /app/
RUN cargo build --release --locked --target-dir /gadk-target -p my-agent --bin agent

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /gadk-target/release/agent  /app/agent
EXPOSE 8080
# Command to run the executable when the container starts
CMD ["/app/agent", "web", "-host", "0.0.0.0", "-port", "8080", "api", "-webui_address", "127.0.0.1:8081", "a2a", "--a2a_agent_url", "http://127.0.0.1:8081", "webui", "--api_server_address", "http://127.0.0.1:8081/api"]"#
        );
    }

    #[test]
    fn dockerfile_golden_agent_engine_shape() {
        let builder = BuilderStage {
            image: "rust:bookworm".into(),
            workdir: "ws".into(),
            package: "agent".into(),
            bin: "agent".into(),
        };

        let got = dockerfile(&builder, 9090, r#", "agentengine""#);

        assert_eq!(
            got,
            r#"
FROM rust:bookworm AS builder
WORKDIR /app/ws
COPY . /app/
RUN cargo build --release --locked --target-dir /gadk-target -p agent --bin agent

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /gadk-target/release/agent  /app/agent
EXPOSE 9090
# Command to run the executable when the container starts
CMD ["/app/agent", "web", "-host", "0.0.0.0", "-port", "9090", "agentengine"]"#
        );
    }

    #[test]
    fn dockerfile_without_tail_closes_the_cmd_array() {
        let got = dockerfile(&BuilderStage::default(), 8080, "");

        assert!(
            got.ends_with(r#"CMD ["/app/", "web", "-host", "0.0.0.0", "-port", "8080"]"#),
            "{got}"
        );
    }

    /// A source root:
    /// ```text
    /// Cargo.lock, Cargo.toml, Dockerfile, README -> Cargo.toml (symlink), a.txt,
    /// a/b.rs, .git/HEAD, target/debug/x, crates/c/{Cargo.toml, Dockerfile, .git (file),
    /// target/y, src/main.rs (0755)}, tmp/cloudrun_x/staged, empty/
    /// ```
    fn source_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for d in [
            ".git",
            "target/debug",
            "a",
            "crates/c/src",
            "crates/c/target",
            "tmp/cloudrun_x",
            "empty",
        ] {
            fs::create_dir_all(r.join(d)).unwrap();
        }
        for (f, body) in [
            ("Cargo.toml", "[workspace]\n"),
            ("Cargo.lock", "version = 4\n"),
            ("Dockerfile", "FROM user\n"),
            ("a.txt", "a\n"),
            ("a/b.rs", "b\n"),
            (".git/HEAD", "ref\n"),
            ("target/debug/x", "x\n"),
            ("crates/c/Cargo.toml", "[package]\n"),
            ("crates/c/Dockerfile", "FROM nested\n"),
            ("crates/c/.git", "gitdir: x\n"),
            ("crates/c/target/y", "y\n"),
            ("crates/c/src/main.rs", "fn main() {}\n"),
            ("tmp/cloudrun_x/staged", "s\n"),
        ] {
            fs::write(r.join(f), body).unwrap();
        }
        fs::set_permissions(
            r.join("crates/c/src/main.rs"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        symlink("Cargo.toml", r.join("README")).unwrap();
        dir
    }

    fn entry(path: &str, kind: EntryKind) -> Entry {
        Entry {
            path: PathBuf::from(path),
            kind,
        }
    }

    #[test]
    fn walk_sorts_and_excludes() {
        let dir = source_root();
        let r = dir.path();

        let tree = SourceTree::walk(r, &[r.join("tmp/cloudrun_x"), r.join("Dockerfile")]).unwrap();

        assert_eq!(tree.root, r);
        assert_eq!(
            tree.entries,
            vec![
                entry("Cargo.lock", EntryKind::File),
                entry("Cargo.toml", EntryKind::File),
                entry("README", EntryKind::Symlink),
                entry("a", EntryKind::Dir),
                entry("a/b.rs", EntryKind::File),
                entry("a.txt", EntryKind::File),
                entry("crates", EntryKind::Dir),
                entry("crates/c", EntryKind::Dir),
                entry("crates/c/Cargo.toml", EntryKind::File),
                entry("crates/c/Dockerfile", EntryKind::File),
                entry("crates/c/src", EntryKind::Dir),
                entry("crates/c/src/main.rs", EntryKind::File),
                entry("empty", EntryKind::Dir),
                entry("tmp", EntryKind::Dir),
            ]
        );
    }

    #[test]
    fn walk_without_excludes_keeps_the_root_dockerfile() {
        let dir = source_root();
        let r = dir.path();

        let tree = SourceTree::walk(r, &[]).unwrap();
        let paths: Vec<&Path> = tree.entries.iter().map(|e| e.path.as_path()).collect();

        assert!(paths.contains(&Path::new("Dockerfile")));
        assert!(paths.contains(&Path::new("tmp/cloudrun_x/staged")));
        assert!(
            !paths
                .iter()
                .any(|p| p.starts_with(".git") || p.starts_with("target"))
        );
    }

    #[test]
    fn walk_of_a_missing_root_is_an_error() {
        let dir = tempfile::tempdir().unwrap();

        assert!(SourceTree::walk(&dir.path().join("missing"), &[]).is_err());
    }

    #[test]
    fn copy_into_reproduces_the_tree() {
        let dir = source_root();
        let r = dir.path();
        let out = tempfile::tempdir().unwrap();
        let tree = SourceTree::walk(r, &[r.join("tmp/cloudrun_x")]).unwrap();

        tree.copy_into(out.path()).unwrap();

        let copied = SourceTree::walk(out.path(), &[]).unwrap();
        assert_eq!(copied.entries, tree.entries);
        assert_eq!(
            fs::read_to_string(out.path().join("crates/c/src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
        assert_eq!(
            fs::metadata(out.path().join("crates/c/src/main.rs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_link(out.path().join("README")).unwrap(),
            PathBuf::from("Cargo.toml")
        );
        assert_eq!(
            fs::read_to_string(out.path().join("Dockerfile")).unwrap(),
            "FROM user\n"
        );
        assert!(out.path().join("empty").is_dir());
    }

    /// `(name, entry type, mode, mtime, contents)` of every archive member, in order.
    fn read_archive(path: &Path) -> Vec<(String, tar::EntryType, u32, u64, String)> {
        let mut archive = tar::Archive::new(GzDecoder::new(fs::File::open(path).unwrap()));
        archive
            .entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let name = e.path().unwrap().to_string_lossy().into_owned();
                let header = e.header().clone();
                let mut body = String::new();
                e.read_to_string(&mut body).unwrap();
                (
                    name,
                    header.entry_type(),
                    header.mode().unwrap(),
                    header.mtime().unwrap(),
                    body,
                )
            })
            .collect()
    }

    #[test]
    fn write_tar_gz_sorted_entries_then_extra_files() {
        let dir = source_root();
        let r = dir.path();
        let out = tempfile::tempdir().unwrap();
        let archive = out.path().join("archive.tgz");
        let tree = SourceTree::walk(r, &[r.join("tmp/cloudrun_x"), r.join("Dockerfile")]).unwrap();

        tree.write_tar_gz(&archive, &[("Dockerfile", b"\nFROM generated")])
            .unwrap();

        let members = read_archive(&archive);
        let names: Vec<&str> = members.iter().map(|m| m.0.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Cargo.lock",
                "Cargo.toml",
                "README",
                "a",
                "a/b.rs",
                "a.txt",
                "crates",
                "crates/c",
                "crates/c/Cargo.toml",
                "crates/c/Dockerfile",
                "crates/c/src",
                "crates/c/src/main.rs",
                "empty",
                "tmp",
                "Dockerfile",
            ]
        );
        let by_name = |n: &str| members.iter().find(|m| m.0 == n).unwrap().clone();
        let readme = by_name("README");
        assert_eq!(readme.1, tar::EntryType::Symlink);
        let main = by_name("crates/c/src/main.rs");
        assert_eq!((main.2, main.3), (0o755, tar::DETERMINISTIC_TIMESTAMP));
        assert_eq!(main.4, "fn main() {}\n");
        assert_eq!(by_name("Cargo.toml").2, 0o644);
        assert_eq!(by_name("a").1, tar::EntryType::Directory);
        let dockerfile = members.last().unwrap();
        assert_eq!(
            dockerfile,
            &(
                "Dockerfile".to_string(),
                tar::EntryType::Regular,
                0o644,
                tar::DETERMINISTIC_TIMESTAMP,
                "\nFROM generated".to_string()
            )
        );
    }

    #[test]
    fn make_temp_uses_go_pattern_in_local_time() {
        let base = tempfile::tempdir().unwrap();
        let now: DateTime<Utc> = default_now();

        let a = make_temp(base.path(), "cloudrun_", now).unwrap();
        let b = make_temp(base.path(), "cloudrun_", now).unwrap();

        let prefix = format!(
            "cloudrun_{}",
            now.with_timezone(&Local).format("%Y%m%d_%H%M%S__")
        );
        for dir in [&a, &b] {
            assert_eq!(dir.parent(), Some(base.path()));
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let suffix = name
                .strip_prefix(&prefix)
                .unwrap_or_else(|| panic!("{name}"));
            assert!(
                !suffix.is_empty() && suffix.bytes().all(|c| c.is_ascii_digit()),
                "{name}"
            );
            let meta = fs::metadata(dir).unwrap();
            assert!(meta.is_dir());
            assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        }
        assert_ne!(a, b);
    }

    #[test]
    fn make_temp_in_a_missing_base_is_an_error() {
        let base = tempfile::tempdir().unwrap();

        let err =
            make_temp(&base.path().join("missing"), "agentEngine_", default_now()).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
