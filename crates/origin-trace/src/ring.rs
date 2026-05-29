// SPDX-License-Identifier: Apache-2.0
//! Per-day parquet ring writer with 64 MiB rotation.

#![allow(clippy::needless_pass_by_value)]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{StringBuilder, UInt64Builder};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use thiserror::Error;

use crate::schema::{span_schema, SpanRow};

#[derive(Debug, Error)]
pub enum RingError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parquet: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("arrow: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

const BATCH_ROWS: usize = 4096;

pub struct Ring {
    dir: PathBuf,
    cap_bytes: usize,
    // In-memory builders flushed to parquet every `BATCH_ROWS` rows or on
    // explicit `flush()` / `Drop`.
    ts_ns: UInt64Builder,
    span_id: UInt64Builder,
    parent_id: UInt64Builder,
    kind: StringBuilder,
    provider: StringBuilder,
    tool: StringBuilder,
    dur_us: UInt64Builder,
    error_kind: StringBuilder,
    attrs_json: StringBuilder,
    rows_in_buf: usize,
    bytes_in_file: usize,
    current: Option<ArrowWriter<File>>,
    current_path: PathBuf,
    rotate_seq: u64,
}

impl Ring {
    /// Open (or create) the ring under `dir`. New files are created lazily.
    ///
    /// # Errors
    /// Returns [`RingError::Io`] if `dir` cannot be created.
    pub fn open<P: AsRef<Path>>(dir: P, cap_bytes: usize) -> Result<Self, RingError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            cap_bytes,
            ts_ns: UInt64Builder::with_capacity(BATCH_ROWS),
            span_id: UInt64Builder::with_capacity(BATCH_ROWS),
            parent_id: UInt64Builder::with_capacity(BATCH_ROWS),
            kind: StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 8),
            provider: StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 8),
            tool: StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 8),
            dur_us: UInt64Builder::with_capacity(BATCH_ROWS),
            error_kind: StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 8),
            attrs_json: StringBuilder::with_capacity(BATCH_ROWS, BATCH_ROWS * 64),
            rows_in_buf: 0,
            bytes_in_file: 0,
            current: None,
            current_path: PathBuf::new(),
            rotate_seq: 0,
        })
    }

    /// Append one row.
    ///
    /// # Errors
    /// Returns [`RingError`] on parquet/arrow failure.
    pub fn append(&mut self, row: SpanRow) -> Result<(), RingError> {
        self.ts_ns.append_value(row.ts_ns);
        self.span_id.append_value(row.span_id);
        self.parent_id.append_value(row.parent_id);
        self.kind.append_value(row.kind);
        self.provider.append_value(row.provider);
        self.tool.append_value(row.tool);
        self.dur_us.append_value(row.dur_us);
        self.error_kind.append_value(row.error_kind);
        self.attrs_json.append_value(&row.attrs_json);
        self.rows_in_buf += 1;
        if self.rows_in_buf >= BATCH_ROWS {
            self.flush_batch()?;
        }
        Ok(())
    }

    /// Drain in-memory builders into the current parquet file and rotate if
    /// the file is past `cap_bytes`.
    ///
    /// # Errors
    /// Returns [`RingError`] on parquet/arrow failure.
    pub fn flush(&mut self) -> Result<(), RingError> {
        if self.rows_in_buf > 0 {
            self.flush_batch()?;
        }
        if let Some(w) = self.current.as_mut() {
            w.flush()?;
        }
        Ok(())
    }

    fn flush_batch(&mut self) -> Result<(), RingError> {
        let batch = RecordBatch::try_new(
            span_schema(),
            vec![
                Arc::new(self.ts_ns.finish()),
                Arc::new(self.span_id.finish()),
                Arc::new(self.parent_id.finish()),
                Arc::new(self.kind.finish()),
                Arc::new(self.provider.finish()),
                Arc::new(self.tool.finish()),
                Arc::new(self.dur_us.finish()),
                Arc::new(self.error_kind.finish()),
                Arc::new(self.attrs_json.finish()),
            ],
        )?;

        let approx = approx_batch_bytes(&batch);
        if self.current.is_none() || self.bytes_in_file + approx > self.cap_bytes {
            self.rotate()?;
        }
        let writer = self.current.as_mut().expect("rotate sets writer");
        writer.write(&batch)?;
        self.bytes_in_file += approx;
        self.rows_in_buf = 0;
        Ok(())
    }

    fn rotate(&mut self) -> Result<(), RingError> {
        if let Some(w) = self.current.take() {
            w.close()?;
        }
        let today = chrono::Utc::now().format("%Y-%m-%d");
        let ts_ms = chrono::Utc::now().timestamp_millis();
        // Include a monotonic counter so back-to-back rotations within the
        // same millisecond produce distinct file names.
        let seq = self.rotate_seq;
        self.rotate_seq = self.rotate_seq.wrapping_add(1);
        let path = self.dir.join(format!("trace-{today}-{ts_ms}-{seq}.parquet"));
        let file = File::create(&path)?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let writer = ArrowWriter::try_new(file, span_schema(), Some(props))?;
        self.current = Some(writer);
        self.current_path = path;
        self.bytes_in_file = 0;
        Ok(())
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        let _ = self.flush();
        if let Some(w) = self.current.take() {
            let _ = w.close();
        }
    }
}

fn approx_batch_bytes(batch: &RecordBatch) -> usize {
    // Snappy-compressed parquet typically lands ~25-40% of the raw arrow size
    // for our string-heavy schema. Use the raw size as a conservative cap
    // proxy — the actual file may be smaller, which is fine.
    batch
        .columns()
        .iter()
        .map(|c| c.get_array_memory_size())
        .sum::<usize>()
}
