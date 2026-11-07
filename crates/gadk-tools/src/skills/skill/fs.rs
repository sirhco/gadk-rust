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

//! The file-system abstraction skill sources read through: [`SkillFs`] (the subset of Go's
//! `fs.FS` that `NewFileSystemSource` uses), [`DirFs`] (Go `os.DirFS`) and [`MemFs`] (Go
//! `fstest.MapFS`, also usable for compiled-in skills).

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::task::JoinHandle;

use super::ResourceReader;
use crate::go_fmt::valid_path;

/// One entry of [`SkillFs::read_dir`] (Go `fs.DirEntry`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDirEntry {
    /// The entry's base name.
    pub name: String,
    /// Whether the entry is a directory (Go `DirEntry.IsDir`; a symbolic link is not).
    pub is_dir: bool,
}

/// What [`SkillFs::metadata`] reports (Go `fs.FileInfo`, reduced to what skills use).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillMetadata {
    /// Whether the path is a directory.
    pub is_dir: bool,
}

/// A read-only, slash-separated file system (Go `fs.FS` as `NewFileSystemSource` uses it).
///
/// Paths follow Go's `fs.ValidPath`: unrooted, `/`-separated, no `.`, `..` or empty elements,
/// with `"."` naming the root. A missing path is `io::ErrorKind::NotFound`.
#[async_trait]
pub trait SkillFs: Send + Sync {
    /// The entries of the directory `path`, sorted by name (Go `fs.ReadDir`).
    async fn read_dir(&self, path: &str) -> io::Result<Vec<SkillDirEntry>>;

    /// Opens the file `path` for reading (Go `fs.FS.Open`). Opening a directory may succeed
    /// with a stream whose reads fail, as in Go.
    async fn open(&self, path: &str) -> io::Result<ResourceReader>;

    /// Describes `path` (Go `fs.Stat`).
    async fn metadata(&self, path: &str) -> io::Result<SkillMetadata>;
}

/// Go's `*fs.PathError` text: `"<op> <path>: <err>"`.
fn path_error(kind: io::ErrorKind, op: &str, path: &str, err: &str) -> io::Error {
    io::Error::new(kind, format!("{op} {path}: {err}"))
}

/// A [`SkillFs`] over a directory of the host file system (Go `os.DirFS`).
///
/// Every path is checked with Go's `fs.ValidPath` rules first, so absolute paths, `..`
/// elements and other invalid names fail with `io::ErrorKind::InvalidInput` (on Windows,
/// paths containing `\` or `:` too). As with `os.DirFS`, symbolic links inside the root are
/// followed. The blocking file-system calls run on Tokio's blocking pool, so the methods must
/// be called inside a Tokio runtime; outside one they return an error.
#[derive(Debug, Clone)]
pub struct DirFs {
    root: PathBuf,
}

impl DirFs {
    /// A file system rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Go `os.DirFS.join`: the host path for a valid `fs` path.
    fn host_path(&self, op: &str, path: &str) -> io::Result<PathBuf> {
        let invalid_on_windows = cfg!(windows) && path.contains(['\\', ':']);
        if !valid_path(path) || invalid_on_windows {
            return Err(path_error(
                io::ErrorKind::InvalidInput,
                op,
                path,
                "invalid argument",
            ));
        }
        if path == "." {
            return Ok(self.root.clone());
        }
        Ok(path
            .split('/')
            .fold(self.root.clone(), |acc, elem| acc.join(elem)))
    }
}

/// Runs `f` on Tokio's blocking pool.
async fn blocking<T, F>(f: F) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    let handle = tokio::runtime::Handle::try_current().map_err(io::Error::other)?;
    handle.spawn_blocking(f).await.map_err(io::Error::other)?
}

#[async_trait]
impl SkillFs for DirFs {
    async fn read_dir(&self, path: &str) -> io::Result<Vec<SkillDirEntry>> {
        let dir = self.host_path("readdir", path)?;
        blocking(move || {
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                entries.push(SkillDirEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_dir: entry.file_type()?.is_dir(),
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        })
        .await
    }

    async fn open(&self, path: &str) -> io::Result<ResourceReader> {
        let file_path = self.host_path("open", path)?;
        let file = blocking(move || std::fs::File::open(&file_path)).await?;
        Ok(Box::pin(BlockingFileReader::new(file)))
    }

    async fn metadata(&self, path: &str) -> io::Result<SkillMetadata> {
        let file_path = self.host_path("stat", path)?;
        let meta = blocking(move || std::fs::metadata(&file_path)).await?;
        Ok(SkillMetadata {
            is_dir: meta.is_dir(),
        })
    }
}

/// The chunk size of one blocking read.
const READ_CHUNK: usize = 64 * 1024;

/// An [`AsyncRead`] over a `std::fs::File` that performs each read on Tokio's blocking pool,
/// one chunk at a time (what `tokio::fs::File` does).
struct BlockingFileReader {
    state: ReaderState,
}

enum ReaderState {
    /// The file plus the bytes of the last chunk not yet handed out.
    Idle {
        file: std::fs::File,
        chunk: Vec<u8>,
        pos: usize,
    },
    /// A blocking read is in flight.
    Busy(JoinHandle<(std::fs::File, Vec<u8>, io::Result<usize>)>),
    /// The blocking task failed; the file is gone.
    Failed,
    /// Transient state while switching.
    Empty,
}

impl BlockingFileReader {
    fn new(file: std::fs::File) -> Self {
        Self {
            state: ReaderState::Idle {
                file,
                chunk: Vec::new(),
                pos: 0,
            },
        }
    }
}

impl AsyncRead for BlockingFileReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            match std::mem::replace(&mut self.state, ReaderState::Empty) {
                ReaderState::Idle { file, chunk, pos } if pos < chunk.len() => {
                    let n = buf.remaining().min(chunk.len() - pos);
                    buf.put_slice(&chunk[pos..pos + n]);
                    self.state = ReaderState::Idle {
                        file,
                        chunk,
                        pos: pos + n,
                    };
                    return Poll::Ready(Ok(()));
                }
                ReaderState::Idle {
                    mut file,
                    mut chunk,
                    ..
                } => {
                    if buf.remaining() == 0 {
                        self.state = ReaderState::Idle {
                            file,
                            chunk,
                            pos: 0,
                        };
                        return Poll::Ready(Ok(()));
                    }
                    let handle = match tokio::runtime::Handle::try_current() {
                        Ok(handle) => handle,
                        Err(e) => {
                            self.state = ReaderState::Idle {
                                file,
                                chunk,
                                pos: 0,
                            };
                            return Poll::Ready(Err(io::Error::other(e)));
                        }
                    };
                    self.state = ReaderState::Busy(handle.spawn_blocking(move || {
                        chunk.resize(READ_CHUNK, 0);
                        let res = io::Read::read(&mut file, &mut chunk);
                        (file, chunk, res)
                    }));
                }
                ReaderState::Busy(mut task) => {
                    let joined = match Pin::new(&mut task).poll(cx) {
                        Poll::Pending => {
                            self.state = ReaderState::Busy(task);
                            return Poll::Pending;
                        }
                        Poll::Ready(joined) => joined,
                    };
                    match joined {
                        Err(e) => {
                            self.state = ReaderState::Failed;
                            return Poll::Ready(Err(io::Error::other(e)));
                        }
                        Ok((file, mut chunk, res)) => match res {
                            Err(e) => {
                                chunk.clear();
                                self.state = ReaderState::Idle {
                                    file,
                                    chunk,
                                    pos: 0,
                                };
                                return Poll::Ready(Err(e));
                            }
                            Ok(0) => {
                                chunk.clear();
                                self.state = ReaderState::Idle {
                                    file,
                                    chunk,
                                    pos: 0,
                                };
                                return Poll::Ready(Ok(()));
                            }
                            Ok(n) => {
                                chunk.truncate(n);
                                self.state = ReaderState::Idle {
                                    file,
                                    chunk,
                                    pos: 0,
                                };
                            }
                        },
                    }
                }
                ReaderState::Failed | ReaderState::Empty => {
                    self.state = ReaderState::Failed;
                    return Poll::Ready(Err(io::Error::other("file reader failed")));
                }
            }
        }
    }
}

/// A reader over a directory: every read fails, as reading a directory opened through Go's
/// `fstest.MapFS` (or `os.DirFS`) does.
struct DirectoryReader {
    path: String,
}

impl AsyncRead for DirectoryReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(path_error(
            io::ErrorKind::IsADirectory,
            "read",
            &self.path,
            "is a directory",
        )))
    }
}

/// An in-memory [`SkillFs`] built from `(path, bytes)` pairs (Go `fstest.MapFS`).
///
/// Directories are implied by the file paths: `a/b/SKILL.md` creates `a` and `a/b`. Paths
/// that are not Go-`fs.ValidPath` (or are `"."`) are ignored, because Go's `MapFS` can never
/// open them. Opening or reading through an invalid path reports `NotFound`, as `MapFS` does.
#[derive(Debug, Clone, Default)]
pub struct MemFs {
    files: BTreeMap<String, Arc<[u8]>>,
    dirs: BTreeSet<String>,
}

impl MemFs {
    /// A file system holding `files`.
    pub fn new<I, P, B>(files: I) -> Self
    where
        I: IntoIterator<Item = (P, B)>,
        P: Into<String>,
        B: Into<Vec<u8>>,
    {
        let mut fs = MemFs::default();
        fs.dirs.insert(".".to_string());
        for (path, bytes) in files {
            let path = path.into();
            if path == "." || !valid_path(&path) {
                continue;
            }
            let mut end = 0;
            while let Some(i) = path[end..].find('/') {
                end += i;
                fs.dirs.insert(path[..end].to_string());
                end += 1;
            }
            fs.files.insert(path, Arc::from(bytes.into()));
        }
        fs
    }

    /// `path`'s directory listing, merged from the files and the implied directories.
    fn children(&self, dir: &str) -> Vec<SkillDirEntry> {
        let prefix = if dir == "." {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut children: BTreeMap<&str, bool> = BTreeMap::new();
        let files = self.files.keys().map(|p| (p.as_str(), false));
        let dirs = self
            .dirs
            .iter()
            .filter(|d| d.as_str() != ".")
            .map(|d| (d.as_str(), true));
        for (path, is_dir) in files.chain(dirs) {
            let Some(rest) = path.strip_prefix(&prefix) else {
                continue;
            };
            if rest.is_empty() || rest.contains('/') {
                continue;
            }
            let entry = children.entry(rest).or_insert(is_dir);
            *entry |= is_dir;
        }
        children
            .into_iter()
            .map(|(name, is_dir)| SkillDirEntry {
                name: name.to_string(),
                is_dir,
            })
            .collect()
    }
}

#[async_trait]
impl SkillFs for MemFs {
    async fn read_dir(&self, path: &str) -> io::Result<Vec<SkillDirEntry>> {
        if self.dirs.contains(path) {
            return Ok(self.children(path));
        }
        if self.files.contains_key(path) {
            return Err(path_error(
                io::ErrorKind::NotADirectory,
                "readdir",
                path,
                "not a directory",
            ));
        }
        Err(path_error(
            io::ErrorKind::NotFound,
            "open",
            path,
            "file does not exist",
        ))
    }

    async fn open(&self, path: &str) -> io::Result<ResourceReader> {
        if let Some(bytes) = self.files.get(path) {
            return Ok(Box::pin(io::Cursor::new(Arc::clone(bytes))));
        }
        if self.dirs.contains(path) {
            return Ok(Box::pin(DirectoryReader {
                path: path.to_string(),
            }));
        }
        Err(path_error(
            io::ErrorKind::NotFound,
            "open",
            path,
            "file does not exist",
        ))
    }

    async fn metadata(&self, path: &str) -> io::Result<SkillMetadata> {
        if self.files.contains_key(path) {
            return Ok(SkillMetadata { is_dir: false });
        }
        if self.dirs.contains(path) {
            return Ok(SkillMetadata { is_dir: true });
        }
        Err(path_error(
            io::ErrorKind::NotFound,
            "stat",
            path,
            "file does not exist",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::skills::skill::{FileSystemSource, SkillErrorKind, Source};

    /// A fresh directory under the system temp dir, removed on drop (no `tempfile` crate in
    /// the workspace).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "gadk-tools-dirfs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, rel: &str, data: &[u8]) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn read_all(mut reader: ResourceReader) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        reader.read_to_end(&mut data).await?;
        Ok(data)
    }

    fn entry(name: &str, is_dir: bool) -> SkillDirEntry {
        SkillDirEntry {
            name: name.into(),
            is_dir,
        }
    }

    #[tokio::test]
    async fn dir_fs_reads_lists_and_stats() {
        let tmp = TempDir::new();
        tmp.write(
            "skill-b/SKILL.md",
            b"---\nname: skill-b\ndescription: B\n---\nbody",
        );
        tmp.write("skill-a/references/doc.md", b"doc");
        // Larger than one blocking-read chunk, to exercise the chunked reader.
        let big: Vec<u8> = (0..(3 * READ_CHUNK + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        tmp.write("skill-a/assets/big.bin", &big);
        tmp.write("top.txt", b"top");
        let fs = DirFs::new(&tmp.0);

        assert_eq!(
            fs.read_dir(".").await.unwrap(),
            [
                entry("skill-a", true),
                entry("skill-b", true),
                entry("top.txt", false)
            ]
        );
        assert_eq!(
            fs.read_dir("skill-a").await.unwrap(),
            [entry("assets", true), entry("references", true)]
        );
        assert_eq!(
            fs.metadata(".").await.unwrap(),
            SkillMetadata { is_dir: true }
        );
        assert_eq!(
            fs.metadata("top.txt").await.unwrap(),
            SkillMetadata { is_dir: false }
        );
        assert_eq!(
            read_all(fs.open("skill-a/references/doc.md").await.unwrap())
                .await
                .unwrap(),
            b"doc"
        );
        assert_eq!(
            read_all(fs.open("skill-a/assets/big.bin").await.unwrap())
                .await
                .unwrap(),
            big
        );
        for missing in ["nope", "skill-a/nope.md"] {
            assert_eq!(
                fs.open(missing).await.err().unwrap().kind(),
                io::ErrorKind::NotFound
            );
            assert_eq!(
                fs.metadata(missing).await.unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
            assert_eq!(
                fs.read_dir(missing).await.unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
        }
    }

    /// Go `fs.ValidPath`: absolute paths, `..`, `.` and empty elements are rejected before
    /// the host file system is touched, so nothing outside the root is reachable.
    #[tokio::test]
    async fn dir_fs_rejects_invalid_paths() {
        let outer = TempDir::new();
        outer.write("secret.txt", b"secret");
        outer.write("root/skill/SKILL.md", b"x");
        let fs = DirFs::new(outer.0.join("root"));
        let absolute = outer.0.join("secret.txt").to_string_lossy().into_owned();
        for path in [
            "..",
            "../secret.txt",
            "skill/../../secret.txt",
            "skill/./SKILL.md",
            "skill/",
            "",
            "/",
            absolute.as_str(),
        ] {
            assert_eq!(
                fs.open(path).await.err().unwrap().kind(),
                io::ErrorKind::InvalidInput,
                "open {path:?}"
            );
            assert_eq!(
                fs.metadata(path).await.unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "stat {path:?}"
            );
            assert_eq!(
                fs.read_dir(path).await.unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "readdir {path:?}"
            );
        }
        assert_eq!(
            fs.open("../secret.txt").await.err().unwrap().to_string(),
            "open ../secret.txt: invalid argument"
        );
    }

    #[tokio::test]
    async fn file_system_source_over_dir_fs() {
        let tmp = TempDir::new();
        tmp.write(
            "weather/SKILL.md",
            b"---\nname: weather\ndescription: Weather.\n---\nUse it.",
        );
        tmp.write("weather/references/weather_us.json", b"{}");
        tmp.write("notes/readme.txt", b"not a skill");
        let source = FileSystemSource::new(Arc::new(DirFs::new(&tmp.0)));

        let fms = source.list_frontmatters().await.unwrap();
        assert_eq!(fms.len(), 1);
        assert_eq!(fms[0].name, "weather");
        assert_eq!(
            source.load_instructions("weather").await.unwrap(),
            "Use it."
        );
        assert_eq!(
            source.list_resources("weather", ".").await.unwrap(),
            ["references/weather_us.json"]
        );
        let reader = source
            .load_resource("weather", "references/weather_us.json")
            .await
            .unwrap();
        assert_eq!(read_all(reader).await.unwrap(), b"{}");
        let err = source
            .load_resource("weather", "../../etc/passwd")
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind(), SkillErrorKind::InvalidResourcePath);
        // A skill name that escapes the root reaches DirFs as an invalid path: not a
        // `SkillNotFound`, and never a read outside the root.
        let err = source.load_frontmatter("../weather").await.unwrap_err();
        assert_eq!(err.kind(), SkillErrorKind::Other);
        assert_eq!(
            err.to_string(),
            "open \"../weather/SKILL.md\": open ../weather/SKILL.md: invalid argument"
        );
    }

    #[tokio::test]
    async fn mem_fs_implies_directories_like_map_fs() {
        let fs = MemFs::new([
            ("b/x/y.txt", "y"),
            ("a.txt", "a"),
            ("b/c.txt", "c"),
            ("../bad", "z"),
        ]);
        assert_eq!(
            fs.read_dir(".").await.unwrap(),
            [entry("a.txt", false), entry("b", true)]
        );
        assert_eq!(
            fs.read_dir("b").await.unwrap(),
            [entry("c.txt", false), entry("x", true)]
        );
        assert_eq!(
            fs.metadata("b/x").await.unwrap(),
            SkillMetadata { is_dir: true }
        );
        assert_eq!(
            read_all(fs.open("b/c.txt").await.unwrap()).await.unwrap(),
            b"c"
        );
        // Opening a directory succeeds; reading it fails, as with Go's MapFS.
        let err = read_all(fs.open("b").await.unwrap()).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::IsADirectory);
        assert_eq!(
            fs.open("../bad").await.err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            fs.read_dir("a.txt").await.unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
    }
}
