//! P11.11 — parquet reader with pushdown predicates on `kind` and `error_kind`.
//!
//! Walks every `.parquet` file under `args.dir`, decodes each `RecordBatch`,
//! and emits matching rows up to `args.limit`. Filtering happens per-row
//! after column decode; row-group statistics pushdown is a future
//! optimization once we start writing min/max stats from the ring side.

use std::fs::File;
use std::path::PathBuf;

use arrow::array::{Array, StringArray, UInt64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct QueryArgs {
    pub dir: PathBuf,
    pub kind: Option<String>,
    pub error_kind: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct QueryRow {
    pub ts_ns: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub kind: String,
    pub provider: String,
    pub tool: String,
    pub dur_us: u64,
    pub error_kind: String,
    pub attrs_json: String,
}

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parquet: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("arrow: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
}

/// Stream every `.parquet` file under `args.dir`, filter rows that match
/// `(kind, error_kind)`, return up to `limit`.
///
/// # Errors
/// Returns [`QueryError`] on I/O or parquet decode failure.
///
/// # Panics
/// Panics if a parquet file is structurally inconsistent with
/// [`crate::schema::span_schema`] (i.e. column order or types changed
/// out-of-band). The ring writer is the single source of truth for that
/// schema; out-of-band files are a bug, not a recoverable error.
pub fn run(args: &QueryArgs) -> Result<Vec<QueryRow>, QueryError> {
    let mut out = Vec::with_capacity(args.limit.min(1024));
    let mut entries: Vec<PathBuf> = Vec::new();
    // A missing dir is treated as "no traces yet" — friendlier for a
    // freshly-installed daemon that hasn't written its first parquet file.
    let read = match std::fs::read_dir(&args.dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(QueryError::Io(e)),
    };
    for entry in read {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) == Some("parquet") {
            entries.push(entry.path());
        }
    }
    // Sort chronologically — file names embed an ISO date + ms timestamp so
    // lexicographic sort matches creation order.
    entries.sort();

    for path in entries {
        let file = File::open(&path)?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
        let reader = builder.build()?;
        for batch in reader {
            let batch = batch?;
            let ts_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .expect("ts_ns is UInt64");
            let span_col = batch
                .column(1)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .expect("span_id is UInt64");
            let parent_col = batch
                .column(2)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .expect("parent_id is UInt64");
            let kind_col = batch
                .column(3)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("kind is Utf8");
            let provider_col = batch
                .column(4)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("provider is Utf8");
            let tool_col = batch
                .column(5)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("tool is Utf8");
            let dur_col = batch
                .column(6)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .expect("dur_us is UInt64");
            let error_col = batch
                .column(7)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("error_kind is Utf8");
            let attrs_col = batch
                .column(8)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("attrs_json is Utf8");

            for i in 0..batch.num_rows() {
                if let Some(want) = &args.kind {
                    if kind_col.value(i) != want {
                        continue;
                    }
                }
                if let Some(want) = &args.error_kind {
                    if error_col.value(i) != want {
                        continue;
                    }
                }
                out.push(QueryRow {
                    ts_ns: ts_col.value(i),
                    span_id: span_col.value(i),
                    parent_id: parent_col.value(i),
                    kind: kind_col.value(i).into(),
                    provider: provider_col.value(i).into(),
                    tool: tool_col.value(i).into(),
                    dur_us: dur_col.value(i),
                    error_kind: error_col.value(i).into(),
                    attrs_json: attrs_col.value(i).into(),
                });
                if out.len() >= args.limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}
