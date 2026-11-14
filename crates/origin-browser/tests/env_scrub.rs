// SPDX-License-Identifier: Apache-2.0
//! A6: the browser sidecars get the allow-listed environment, not the
//! daemon's. One test fn in its own binary because it mutates the process
//! environment.
#![allow(clippy::unwrap_used)]

use origin_browser::agent_browser::AgentBrowserClient;
use origin_browser::cloak::CloakClient;
use origin_browser::protocol::Verb;

mod common;

/// A sidecar stand-in that answers each verb with `$ANTHROPIC_API_KEY` (or
/// `<unset>`) in `snapshot`.
const ECHO_KEY: &str = "require('readline').createInterface({input:process.stdin}).on('line',()=>{\
    const v=process.env.ANTHROPIC_API_KEY;\
    console.log(JSON.stringify({ok:true,snapshot:v===undefined?'<unset>':v}));})";

fn verb() -> Verb {
    Verb::Open {
        url: "u".into(),
        session: "s".into(),
    }
}

#[tokio::test]
async fn browser_sidecars_get_scrubbed_env() {
    if !common::node_available() {
        eprintln!("skipping browser_sidecars_get_scrubbed_env: `node` unavailable");
        return;
    }
    std::env::set_var("ANTHROPIC_API_KEY", "sk-parent-secret");
    std::env::remove_var("ORIGIN_ENV_SCRUB");
    std::env::remove_var("ORIGIN_ENV_PASSTHROUGH");

    let mut agent = AgentBrowserClient::spawn_with_command("node", &["-e", ECHO_KEY])
        .await
        .unwrap();
    let seen = agent.send(&verb()).await.unwrap().snapshot;
    assert_eq!(seen.as_deref(), Some("<unset>"), "agent-browser: secret leaked");

    let mut cloak = CloakClient::spawn_with_command("node", &["-e", ECHO_KEY])
        .await
        .unwrap();
    let seen = cloak.send(&verb()).await.unwrap().snapshot;
    assert_eq!(seen.as_deref(), Some("<unset>"), "cloak: secret leaked");
}
