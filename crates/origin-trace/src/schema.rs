// SPDX-License-Identifier: Apache-2.0
//! Arrow schema for a single span row.

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};

#[derive(Debug, Clone)]
pub struct SpanRow {
    pub ts_ns: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub kind: &'static str,
    pub provider: &'static str,
    pub tool: &'static str,
    pub dur_us: u64,
    pub error_kind: &'static str,
    pub attrs_json: String,
}

#[must_use]
pub fn span_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ts_ns", DataType::UInt64, false),
        Field::new("span_id", DataType::UInt64, false),
        Field::new("parent_id", DataType::UInt64, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("provider", DataType::Utf8, false),
        Field::new("tool", DataType::Utf8, false),
        Field::new("dur_us", DataType::UInt64, false),
        Field::new("error_kind", DataType::Utf8, false),
        Field::new("attrs_json", DataType::Utf8, false),
    ]))
}
