// SPDX-License-Identifier: Apache-2.0
use origin_mcp::{McpClient, StdioTransport};
use std::sync::Arc;

/// Probe whether the mock-server interpreter is actually runnable on this
/// runner. CI images differ in shell/PowerShell availability and quoting, so
/// we skip cleanly (rather than hard-fail) when the interpreter can't be
/// invoked at all.
fn interpreter_available(prog: &str) -> bool {
    let probe = if cfg!(windows) {
        std::process::Command::new(prog)
            .args(["-NoProfile", "-Command", "exit 0"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    } else {
        std::process::Command::new(prog)
            .args(["-c", "exit 0"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    };
    matches!(probe, Ok(s) if s.success())
}

// A mock MCP server that responds to `initialize` and `tools/list` with
// canned JSON-RPC responses. Implemented as a one-liner shell script so we
// don't need a Rust mock binary in tree.
fn mock_server_cmd() -> (String, Vec<String>) {
    if cfg!(windows) {
        // PowerShell loop: read JSON-RPC line, respond with canned tools/list.
        (
            "powershell.exe".into(),
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "while($line=[Console]::In.ReadLine()){if($line -match '\"method\":\"tools/list\"'){[Console]::Out.WriteLine('{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"ping\",\"description\":\"d\",\"input_schema\":{}}]}}')} elseif($line -match '\"method\":\"initialize\"'){[Console]::Out.WriteLine('{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}')}[Console]::Out.Flush()}".into(),
            ],
        )
    } else {
        (
            "/bin/sh".into(),
            vec![
                "-c".into(),
                r#"while IFS= read -r line; do
                    case "$line" in
                      *'"method":"tools/list"'*) printf '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"ping","description":"d","input_schema":{}}]}}\n' ;;
                      *'"method":"initialize"'*)  printf '{"jsonrpc":"2.0","id":1,"result":{}}\n' ;;
                    esac
                done"#.into(),
            ],
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_and_list_tools_against_mock() {
    let (prog, args) = mock_server_cmd();
    // Skip cleanly when the mock-server interpreter isn't available on this
    // runner, so the suite never hard-fails on an interpreter-less/mismatched
    // CI image.
    if !interpreter_available(&prog) {
        eprintln!("skipping handshake_and_list_tools_against_mock: `{prog}` unavailable");
        return;
    }
    let transport: Arc<dyn origin_mcp::Transport> =
        Arc::new(StdioTransport::spawn(&prog, &args).expect("spawn"));
    let client = McpClient::new(transport);
    client.initialize().await.expect("initialize");
    let tools = client.list_tools().await.expect("list");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "ping");
}
