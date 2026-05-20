use crate::metrics::TaskResult;
use std::fmt::Write;

#[must_use]
pub fn render_markdown(results: &[TaskResult]) -> String {
    let mut s = String::new();
    writeln!(s, "# Origin bench report").ok();
    writeln!(s).ok();
    writeln!(s, "| contestant | task | in | out | ms | tools | pass |").ok();
    writeln!(s, "|---|---|---:|---:|---:|---:|:---:|").ok();
    for r in results {
        writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} |",
            r.contestant,
            r.task_id,
            r.input_tokens,
            r.output_tokens,
            r.wall_ms,
            r.tool_calls,
            if r.passed { "ok" } else { "fail" },
        )
        .ok();
    }
    s
}

#[must_use]
pub fn render_json(results: &[TaskResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".into())
}
