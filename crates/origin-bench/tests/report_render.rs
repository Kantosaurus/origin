use origin_bench::metrics::TaskResult;
use origin_bench::report::render_markdown;

#[test]
fn markdown_renders_one_row_per_contestant() {
    let results = vec![
        TaskResult {
            contestant: "origin".into(),
            task_id: "01-read-and-summarize".into(),
            input_tokens: 1000,
            output_tokens: 200,
            wall_ms: 1500,
            tool_calls: 1,
            passed: true,
        },
        TaskResult {
            contestant: "claude-code".into(),
            task_id: "01-read-and-summarize".into(),
            input_tokens: 1200,
            output_tokens: 220,
            wall_ms: 1700,
            tool_calls: 1,
            passed: true,
        },
    ];
    let md = render_markdown(&results);
    assert!(md.contains("| contestant | task |"));
    assert!(md.contains("origin"));
    assert!(md.contains("claude-code"));
}
