#![no_main]
use libfuzzer_sys::fuzz_target;
use origin_daemon::tool_use_parser::ToolUseParser;

fuzz_target!(|data: &[u8]| {
    let mut p = ToolUseParser::new();
    p.begin_tool_use("X");
    let _ = p.feed(data);
});
