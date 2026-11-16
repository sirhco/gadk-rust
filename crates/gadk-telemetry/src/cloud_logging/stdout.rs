// Copyright 2026 Chris Olson
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

//! The structured-JSON stdout log exporter: one line per record with Cloud Logging's special
//! keys, for platforms whose logging agent collects stdout (Cloud Run, GKE, Cloud Functions).

use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::{LogBatch, LogExporter};
use parking_lot::Mutex;

use super::entry::stdout_line;
use super::{CloudLogTarget, WARN_TARGET};

/// Writes each record as one JSON line.
pub(crate) struct StdoutJsonExporter {
    out: Mutex<Box<dyn Write + Send>>,
    target: CloudLogTarget,
    failing: AtomicBool,
}

impl fmt::Debug for StdoutJsonExporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StdoutJsonExporter")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl StdoutJsonExporter {
    pub(crate) fn new(out: Box<dyn Write + Send>, target: CloudLogTarget) -> Self {
        Self {
            out: Mutex::new(out),
            target,
            failing: AtomicBool::new(false),
        }
    }

    /// An exporter writing to the process's stdout.
    pub(crate) fn stdout(target: CloudLogTarget) -> Self {
        Self::new(Box::new(std::io::stdout()), target)
    }

    /// Renders the batch into one buffer and writes it under the lock. The first failing write
    /// returns `Err` and warns on [`WARN_TARGET`]; later failures return `Ok` until a write
    /// succeeds again.
    pub(crate) fn write_batch(&self, batch: &LogBatch<'_>, now: SystemTime) -> OTelSdkResult {
        let mut buf = Vec::new();
        for (record, _) in batch.iter() {
            if let Ok(line) = serde_json::to_vec(&stdout_line(record, &self.target, now)) {
                buf.extend_from_slice(&line);
                buf.push(b'\n');
            }
        }
        if buf.is_empty() {
            return Ok(());
        }
        let result = {
            let mut out = self.out.lock();
            out.write_all(&buf).and_then(|()| out.flush())
        };
        match result {
            Ok(()) => {
                self.failing.store(false, Ordering::SeqCst);
                Ok(())
            }
            Err(e) if !self.failing.swap(true, Ordering::SeqCst) => {
                let err = OTelSdkError::InternalFailure(format!("cloud logging stdout: {e}"));
                // The SDK drops export results off the `force_flush` path, so the first failure
                // is also a `tracing` warning (the I/O error only, never record content).
                tracing::warn!(target: WARN_TARGET, "{err}");
                Err(err)
            }
            Err(_) => Ok(()),
        }
    }
}

impl LogExporter for StdoutJsonExporter {
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        self.write_batch(&batch, SystemTime::now())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::SystemTime;

    use opentelemetry::logs::{AnyValue, LogRecord as _};
    use opentelemetry_sdk::logs::LogBatch;
    use parking_lot::Mutex;
    use pretty_assertions::assert_eq;
    use serde_json::{Value, json};

    use super::*;
    use crate::cloud_logging::CloudLogTarget;
    use crate::cloud_logging::test_records::{Warnings, record, refs};

    /// A writer into a shared buffer that fails while `fail` is set.
    #[derive(Clone, Default)]
    struct Buf {
        bytes: Arc<Mutex<Vec<u8>>>,
        fail: Arc<AtomicBool>,
    }

    impl Write for Buf {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(io::Error::other("disk full"));
            }
            self.bytes.lock().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Buf {
        fn lines(&self) -> Vec<Value> {
            String::from_utf8(self.bytes.lock().clone())
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    fn exporter(buf: &Buf) -> StdoutJsonExporter {
        StdoutJsonExporter::new(Box::new(buf.clone()), CloudLogTarget::new("p", "", None))
    }

    #[tokio::test]
    async fn one_json_object_per_record_per_line() {
        let buf = Buf::default();
        let records = vec![
            record(|r| r.set_body(AnyValue::String("one".into()))),
            record(|r| r.set_body(AnyValue::String("two".into()))),
        ];
        exporter(&buf)
            .export(LogBatch::new(&refs(&records)))
            .await
            .unwrap();
        let lines = buf.lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["body"], json!("one"));
        assert_eq!(lines[1]["body"], json!("two"));
        assert_eq!(lines[0]["severity"], json!("DEFAULT"));
        assert!(lines[0]["timestamp"].is_string());
    }

    #[test]
    fn concurrent_exports_never_interleave() {
        let buf = Buf::default();
        let exp = Arc::new(exporter(&buf));
        let records: Vec<_> = (0..50)
            .map(|i| {
                record(|r| {
                    r.set_body(AnyValue::String(
                        format!("{i}-{}", "v".repeat(5_000)).into(),
                    ))
                })
            })
            .collect();
        let records = Arc::new(records);
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let exp = Arc::clone(&exp);
                let records = Arc::clone(&records);
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        exp.write_batch(&LogBatch::new(&refs(&records)), SystemTime::now())
                            .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(buf.lines().len(), 4 * 10 * 50);
    }

    #[tokio::test]
    async fn a_failing_writer_errs_once_until_a_write_succeeds() {
        let buf = Buf::default();
        let exp = exporter(&buf);
        let records = vec![record(|_| {})];
        buf.fail.store(true, Ordering::SeqCst);
        let first = exp.export(LogBatch::new(&refs(&records))).await;
        assert!(first.unwrap_err().to_string().contains("disk full"));
        assert!(exp.export(LogBatch::new(&refs(&records))).await.is_ok());
        buf.fail.store(false, Ordering::SeqCst);
        exp.export(LogBatch::new(&refs(&records))).await.unwrap();
        assert_eq!(buf.lines().len(), 1);
        buf.fail.store(true, Ordering::SeqCst);
        assert!(exp.export(LogBatch::new(&refs(&records))).await.is_err());
    }

    #[tokio::test]
    async fn a_failing_writer_warns_once_until_a_write_succeeds() {
        let warnings = Warnings::default();
        let _guard = warnings.capture();
        let buf = Buf::default();
        let exp = exporter(&buf);
        let records = vec![record(|r| r.set_body(AnyValue::String("secret".into())))];
        buf.fail.store(true, Ordering::SeqCst);
        let _ = exp.export(LogBatch::new(&refs(&records))).await;
        let first = warnings.take();
        assert_eq!(first.len(), 1, "{first:?}");
        assert!(first[0].contains("disk full"), "{first:?}");
        assert!(!first[0].contains("secret"), "{first:?}");
        let _ = exp.export(LogBatch::new(&refs(&records))).await;
        assert!(warnings.take().is_empty());
        buf.fail.store(false, Ordering::SeqCst);
        exp.export(LogBatch::new(&refs(&records))).await.unwrap();
        assert!(warnings.take().is_empty());
        buf.fail.store(true, Ordering::SeqCst);
        let _ = exp.export(LogBatch::new(&refs(&records))).await;
        assert_eq!(warnings.take().len(), 1);
    }
}
